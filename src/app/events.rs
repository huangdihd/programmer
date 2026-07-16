// Copyright (C) 2026 huangdihd
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Event handling: keyboard, mouse, paste, tick, and completions.

use super::App;
use super::{commands, diagnostics, helpers, session, stream, tools};
use crate::classifier::WorkMode;
use crate::commands::CompletionEngine;
use crate::ui::components::conversation_panel::conversation_panel::{ActivePhase, SelectionEnd};
use crate::ui::components::provider_panel::PanelAction;
use crate::ui::components::question_panel::QuestionPanel;
use crate::ui::components::sidebar::ClickTarget;
use crate::ui::event::{AppEvent, Event};
use async_openai::types::responses::{FunctionCallOutput, FunctionCallOutputItemParam, FunctionToolCall};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

// ---------------------------------------------------------------------------
// Main event handler
// ---------------------------------------------------------------------------

pub(crate) async fn handle_event(app: &mut App<'_>, event: Event) -> color_eyre::Result<()> {
    match event {
        Event::Tick => app.tick(),
        Event::Crossterm(event) => match event {
            crossterm::event::Event::FocusGained => {
                // Restore mouse capture explicitly on focus regain.
                let _ = crossterm::execute!(
                    std::io::stdout(),
                    crossterm::event::EnableMouseCapture
                );
            }
            crossterm::event::Event::Key(key_event)
                if key_event.kind == KeyEventKind::Press =>
            {
                handle_key_events(app, key_event).await?
            }
            crossterm::event::Event::Paste(data) => handle_paste(app, data),
            crossterm::event::Event::Mouse(_) if app.provider_panel.is_some() => {}
            crossterm::event::Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::ScrollDown => {
                    if app.sidebar_area.map_or(false, |a| mouse.column >= a.x) {
                        if let Some(ref mut s) = app.sidebar { s.scroll_down(); }
                    } else {
                        app.conversation_panel.scroll_down();
                    }
                }
                MouseEventKind::ScrollUp => {
                    if app.sidebar_area.map_or(false, |a| mouse.column >= a.x) {
                        if let Some(ref mut s) = app.sidebar { s.scroll_up(); }
                    } else {
                        app.conversation_panel.scroll_up();
                    }
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    // If click is in the sidebar, track it and don't start selection.
                    app.sidebar_click_active = app.sidebar.is_some()
                        && app.sidebar_area.as_ref().map_or(false, |area| {
                            mouse.column >= area.x
                                && mouse.column < area.x + area.width
                                && mouse.row >= area.y
                                && mouse.row < area.y + area.height
                        });
                    if app.sidebar_click_active {
                        return Ok(());
                    }
                    app.conversation_panel
                        .selection_begin(mouse.column, mouse.row)
                }
                MouseEventKind::Drag(MouseButton::Left) => {
                    if app.sidebar_click_active {
                        return Ok(());
                    }
                    app.conversation_panel
                        .selection_drag(mouse.column, mouse.row)
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    if app.sidebar_click_active {
                        app.sidebar_click_active = false;
                        // Only act if the release is still in the sidebar.
                        if let Some(ref sidebar) = app.sidebar {
                            if let Some(ref area) = app.sidebar_area {
                                if mouse.column > area.x
                                    && mouse.column < area.x + area.width
                                    && mouse.row >= area.y
                                    && mouse.row < area.y + area.height
                                {
                                    let line_idx = (mouse.row - area.y) as usize;
                                    if line_idx < sidebar.click_map.len() {
                                        let target =
                                            sidebar.click_map[line_idx].clone();
                                        handle_sidebar_click(app, &target);
                                    }
                                }
                            }
                        }
                        return Ok(());
                    }
                    match app
                        .conversation_panel
                        .selection_end(mouse.column, mouse.row)
                    {
                        SelectionEnd::Click => app
                            .conversation_panel
                            .handle_click(mouse.column, mouse.row),
                        SelectionEnd::Copied(text) => {
                            if !crate::clipboard::copy(&text) {
                                app.conversation_panel
                                    .add_error_string("failed to copy selection to clipboard");
                                session::save_session(app);
                            }
                        }
                        SelectionEnd::Ignored => {}
                    }
                }
                _ => {}
            },
            _ => {}
        },
        Event::App(app_event) => match app_event {
            AppEvent::Cancel => {
                if let Some(partial) = &app.conversation_panel.receiving_response {
                    partial.cancelled.store(true, Ordering::Relaxed);
                }
                // Also stop the post-stream pipeline (classifying / running
                // tools) — its token outlives `receiving_response`.
                if let Some(token) = app.active_cancel_token.take() {
                    token.store(true, Ordering::Relaxed);
                }
                app.conversation_panel.abort_receiving();
                app.conversation_panel.phase = ActivePhase::None;
                app.conversation_panel.flush_usage();
                app.conversation_panel
                    .add_info_string("Request cancelled by user.".to_string());
                session::save_session(app);
                if let Some(pending_request) = app.conversation_panel.pending_message.take() {
                    commands::start_request(app, pending_request).await;
                }
            }
            AppEvent::ChunkReceived(chunk) => stream::handle_chunk_events(app, chunk).await,
            AppEvent::OpenAIErrorReceived(error) => {
                stream::handle_error_events(app, error).await;
                session::save_session(app);
            }
            AppEvent::ResponseFinished(partial_response) => {
                if partial_response.cancelled.load(Ordering::Relaxed) {
                    return Ok(());
                }
                let cancel_token = partial_response.cancelled.clone();
                let calls = helpers::function_calls(&partial_response);
                if calls.is_empty() {
                    // Plan mode: if in Planning phase and model stopped without
                    // making tool calls, it has finished presenting the plan.
                    if app.work_mode == WorkMode::Plan
                        && app.plan_phase == crate::classifier::PlanPhase::Planning
                    {
                        app.plan_phase = crate::classifier::PlanPhase::Reviewing;
                        app.conversation_panel.phase = ActivePhase::None;
                        app.conversation_panel.flush_usage();
                        session::save_session(app);
                        return Ok(());
                    }
                    app.conversation_panel.phase = ActivePhase::None;
                    app.conversation_panel.flush_usage();
                    if let Some(pending_request) =
                        app.conversation_panel.pending_message.take()
                    {
                        commands::start_request(app, pending_request).await
                    }
                    session::save_session(app);
                } else {
                    tools::run_tool_calls(app, calls, cancel_token);
                }
            }
            AppEvent::ClassificationCompleted {
                allowed,
                denied,
                ask_queue,
                cancel_token,
            } => {
                if cancel_token.load(Ordering::Relaxed) {
                    return Ok(());
                }
                if !ask_queue.is_empty() {
                    for output in &denied {
                        app.conversation_panel.add_tool_output(output.clone());
                    }
                    app.approval_queue = ask_queue;
                    app.conversation_panel.phase = ActivePhase::None;
                    return Ok(());
                }
                tools::process_classification_results(app, allowed, denied, cancel_token);
            }
            AppEvent::ToolCallsCompleted(outputs, cancel_token) => {
                if cancel_token.load(Ordering::Relaxed) {
                    return Ok(());
                }
                app.conversation_panel.phase = ActivePhase::None;
                app.lsp_configured = helpers::lsp_checker_configured();
                let edited_files = tools::batch_edited_files(app, &outputs);
                for output in outputs {
                    app.conversation_panel.add_tool_output(output);
                }
                app.todo_list = crate::todos::TodoList::load();
                session::save_session(app);
                // Restore mouse capture — external commands disable it.
                let _ = crossterm::execute!(
                    std::io::stdout(),
                    crossterm::event::EnableMouseCapture
                );
                if !diagnostics::continue_with_diagnostics(
                    app,
                    edited_files,
                    cancel_token.clone(),
                ) {
                    stream::spawn_stream(app);
                }
            }
            AppEvent::DiagnosticsCompleted {
                snapshot,
                reminder_due,
                seed,
                cancel_token,
            } => {
                if cancel_token.load(Ordering::Relaxed) {
                    return Ok(());
                }
                if seed {
                    if app.diagnostics_baseline.is_none() {
                        app.diagnostics_baseline = Some(snapshot.diagnostics);
                    }
                    return Ok(());
                }
                diagnostics::apply_diagnostics(app, snapshot, reminder_due);
                session::save_session(app);
                // Restore mouse capture — diagnostic runners may reset console
                // modes on Windows.
                let _ = crossterm::execute!(
                    std::io::stdout(),
                    crossterm::event::EnableMouseCapture
                );
                stream::spawn_stream(app);
            }
            AppEvent::Start => {
                diagnostics::maybe_seed_diagnostics_baseline(app);
                commands::send_message(app).await;
            }
            AppEvent::StartInit => {
                app.conversation_panel.add_meta(
                    "\u{25B8} Initializing project\u{2026}",
                    helpers::init_prompt(),
                );
                app.conversation_panel.reset_accumulated_usage();
                diagnostics::maybe_seed_diagnostics_baseline(app);
                session::save_session(app);
                stream::spawn_stream(app);
            }
            AppEvent::Quit => app.quit(),
            AppEvent::ProvidersChanged => {
                app.provider_manager = crate::providers::ProviderManager::new(&app.config).await;
                for msg in &app.provider_manager.startup_errors {
                    app.conversation_panel.add_error_string(msg.clone());
                }
                if app
                    .provider_manager
                    .resolve(&app.current_model)
                    .is_none()
                {
                    app.current_model = app.provider_manager.default_model();
                    app.conversation_panel.add_info_string(format!(
                        "current model reset to: {}",
                        app.current_model
                    ));
                }
            }
            AppEvent::McpChanged => {
                if app.config.mcp_servers.is_empty() {
                    app.mcp_manager = None;
                    app.conversation_panel
                        .add_info_string("MCP servers cleared.".to_string());
                } else {
                    let mcp = crate::mcp::McpManager::from_config(
                        &app.config.mcp_servers,
                        ".",
                    )
                    .await;
                    for err in &mcp.startup_errors {
                        app.conversation_panel.add_error_string(err.clone());
                    }
                    app.conversation_panel.add_info_string(format!(
                        "MCP reloaded: {} server(s), {} tool(s) available",
                        mcp.server_count(),
                        mcp.all_tools().len(),
                    ));
                    app.mcp_manager = Some(std::sync::Arc::new(mcp));
                }
            }
            AppEvent::QuestionPrompt {
                question,
                answer_tx,
            } => {
                app.question_panel = Some(QuestionPanel::new(question, answer_tx));
            }
        },
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tool-call approval (Manual / AllowEdits modes)
// ---------------------------------------------------------------------------

/// Handle approval keys when tool calls are queued.
pub(crate) fn handle_approval_key(
    app: &mut App<'_>,
    key_event: KeyEvent,
) -> color_eyre::Result<()> {
    match key_event.code {
        KeyCode::Up | KeyCode::Char('k') => {
            app.approval_selected = app.approval_selected.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
            if app.approval_selected + 1 < 4 {
                app.approval_selected += 1;
            }
        }
        KeyCode::Enter => match app.approval_selected {
            0 => {
                if let Some((call, _)) = app.approval_queue.drain(..1).next() {
                    app.approved_calls.push(call);
                }
                app.approval_selected = 0;
                check_approval_done(app);
            }
            1 => {
                let denied = app.approval_queue.drain(..1).next();
                if let Some((call, reason)) = denied {
                    deny_single_call(app, call, reason);
                }
                app.approval_selected = 0;
                check_approval_done(app);
            }
            2 => {
                let approved: Vec<FunctionToolCall> =
                    app.approval_queue.drain(..).map(|(c, _)| c).collect();
                app.approved_calls.extend(approved);
                app.approval_selected = 0;
                check_approval_done(app);
            }
            3 => {
                let all = std::mem::take(&mut app.approval_queue);
                for (call, reason) in all {
                    deny_single_call(app, call, reason);
                }
                app.approval_selected = 0;
                check_approval_done(app);
            }
            _ => {}
        },
        _ => {}
    }
    Ok(())
}

/// Handle keyboard input in the plan review bar (Plan mode Reviewing phase).
async fn handle_plan_review_key(
    app: &mut App<'_>,
    key_event: KeyEvent,
) -> color_eyre::Result<()> {
    let option_count = if app.config.allow_yolo { 4 } else { 3 };
    match key_event.code {
        KeyCode::Up | KeyCode::Char('k') => {
            app.plan_review_selected = app.plan_review_selected.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
            if app.plan_review_selected + 1 < option_count {
                app.plan_review_selected += 1;
            }
        }
        KeyCode::Esc => {
            // Cancel review — go back to Planning for revision.
            app.plan_phase = crate::classifier::PlanPhase::Planning;
            app.plan_review_selected = 0;
            app.conversation_panel
                .add_info_string("Plan review cancelled — you can revise the plan.");
            session::save_session(app);
        }
        KeyCode::Enter => {
            match app.plan_review_selected {
                0 => {
                    // Execute with Manual — exit Plan mode into Manual.
                    app.work_mode = WorkMode::Manual;
                    app.plan_phase = crate::classifier::PlanPhase::default();
                    app.plan_execution_mode = None;
                    app.plan_review_selected = 0;
                    app.conversation_panel
                        .add_info_string("Plan approved — executing with Manual mode.");
                    let hidden = "The plan was approved by the user. Execute it now using the identified steps. Ask for approval before running commands or destructive edits.";
                    session::save_session(app);
                    super::commands::start_request_as(
                        app,
                        hidden.to_string(),
                        async_openai::types::responses::InputRole::Developer,
                    )
                    .await;
                }
                1 => {
                    // Execute with Auto — exit Plan mode into Auto.
                    app.work_mode = WorkMode::Auto;
                    app.plan_phase = crate::classifier::PlanPhase::default();
                    app.plan_execution_mode = None;
                    app.plan_review_selected = 0;
                    app.conversation_panel
                        .add_info_string("Plan approved — executing with Auto mode.");
                    let hidden = "The plan was approved by the user. Execute it now using the identified steps.";
                    session::save_session(app);
                    super::commands::start_request_as(
                        app,
                        hidden.to_string(),
                        async_openai::types::responses::InputRole::Developer,
                    )
                    .await;
                }
                2 => {
                    if app.config.allow_yolo {
                        // Execute with YOLO — exit Plan mode into YOLO.
                        app.work_mode = WorkMode::Yolo;
                        app.plan_phase = crate::classifier::PlanPhase::default();
                        app.plan_execution_mode = None;
                        app.plan_review_selected = 0;
                        app.conversation_panel
                            .add_info_string("Plan approved — executing with YOLO mode.");
                        let hidden = "The plan was approved by the user. Execute it now using the identified steps. You have full autonomy.";
                        session::save_session(app);
                        super::commands::start_request_as(
                            app,
                            hidden.to_string(),
                            async_openai::types::responses::InputRole::Developer,
                        )
                        .await;
                    } else {
                        // Propose changes (3rd option when YOLO is off)
                        app.plan_phase = crate::classifier::PlanPhase::Planning;
                        app.plan_review_selected = 0;
                        app.conversation_panel
                            .add_info_string("Enter your feedback in the input panel.");
                        session::save_session(app);
                    }
                }
                3 => {
                    // Propose changes (4th option when YOLO is on)
                    app.plan_phase = crate::classifier::PlanPhase::Planning;
                    app.plan_review_selected = 0;
                    app.conversation_panel
                        .add_info_string("Enter your feedback in the input panel.");
                    session::save_session(app);
                }
                _ => {}
            }
        }
        _ => {
            // Any other key: pass through to input panel for feedback text
            app.input_panel.input(key_event);
        }
    }
    Ok(())
}

fn deny_single_call(app: &mut App<'_>, call: FunctionToolCall, reason: String) {
    app.conversation_panel.add_info_string(format!(
        "🛡 Denied: {} ({})",
        call.name, reason
    ));
    let output = crate::tools::ToolOutput {
        param: FunctionCallOutputItemParam {
            call_id: call.call_id,
            output: FunctionCallOutput::Text(format!(
                "error: tool call denied by user — {} ({})",
                call.name, reason
            )),
            id: None,
            status: None,
        },
        failed: true,
        approval_label: Some(format!("{} denied in Manual mode by user", WorkMode::Manual.icon())),
    };
    app.conversation_panel.add_tool_output(output);
}

/// If the approval queue is empty, run the approved calls and continue.
fn check_approval_done(app: &mut App<'_>) {
    if !app.approval_queue.is_empty() {
        return;
    }
    let calls = std::mem::take(&mut app.approved_calls);
    if calls.is_empty() {
        stream::spawn_stream(app);
        return;
    }

    let sender = app.events.sender.clone();
    let cancel_token = Arc::new(AtomicBool::new(false));
    let mcp = app.mcp_manager.clone();
    tokio::spawn(async move {
        let mut outputs = Vec::new();
        for call in &calls {
            let mut out =
                crate::tools::run_tool_call(call, &sender, mcp.as_deref()).await;
            out.approval_label = Some(format!("{} approved in Manual mode by user", WorkMode::Manual.icon()));
            outputs.push(out);
        }
        let _ = sender.send(Event::App(AppEvent::ToolCallsCompleted(
            outputs,
            cancel_token,
        )));
    });
}

// ---------------------------------------------------------------------------
// Paste handling
// ---------------------------------------------------------------------------

/// Handles text pasted into the terminal (bracketed paste).
pub(crate) fn handle_paste(app: &mut App<'_>, data: String) {
    let data = data.replace("\r\n", "\n").replace('\r', "\n");
    if let Some(panel) = app.question_panel.as_mut() {
        panel.handle_paste(&data);
        return;
    }
    if let Some(panel) = app.provider_panel.as_mut() {
        panel.handle_paste(&data);
        return;
    }
    if let Some(panel) = app.mcp_panel.as_mut() {
        panel.handle_paste(&data);
        return;
    }
    if !data.contains('\n') && data.chars().count() <= 200 {
        app.input_panel.insert_str(&data);
    } else {
        app.input_panel.add_paste(data);
    }
    update_completions(app);
}

// ---------------------------------------------------------------------------
// Keyboard handler
// ---------------------------------------------------------------------------

/// Enter combined with any of these modifiers inserts a newline instead of sending.
pub(crate) fn is_newline_modifier(modifiers: KeyModifiers) -> bool {
    modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT)
}

/// Handles the key events and updates the state of [`App`].
pub(crate) async fn handle_key_events(
    app: &mut App<'_>,
    key_event: KeyEvent,
) -> color_eyre::Result<()> {
    // ---- tool-call approval (Manual mode) ----
    if !app.approval_queue.is_empty() {
        return handle_approval_key(app, key_event);
    }
    // ---- question panel ----
    if let Some(panel) = app.question_panel.as_mut() {
        match panel.handle_key(key_event) {
            crate::ui::components::question_panel::AnswerAction::Answer(text) => {
                let question = panel.question_text().to_string();
                panel.answer(text.clone());
                app.question_panel = None;
                app.conversation_panel
                    .add_info_string(format!("❓ {}\n→ {}", question, text));
            }
            crate::ui::components::question_panel::AnswerAction::None => {}
        }
        return Ok(());
    }

    // ---- plan review (Plan mode) ----
    if app.work_mode == WorkMode::Plan
        && app.plan_phase == crate::classifier::PlanPhase::Reviewing
    {
        return handle_plan_review_key(app, key_event).await;
    }

    // ---- todo panel ----
    if let Some(panel) = app.todo_panel.as_mut() {
        match panel.handle_key(key_event) {
            crate::ui::components::todo_panel::PanelAction::Close => {
                app.todo_panel = None;
                app.todo_list = crate::todos::TodoList::load();
            }
            crate::ui::components::todo_panel::PanelAction::None => {}
        }
        return Ok(());
    }

    // ---- sidebar keyboard (when focused) ----
    if app.sidebar.as_ref().map_or(false, |s| s.has_focus) {
        if key_event.code == KeyCode::Esc {
            if let Some(ref mut s) = app.sidebar {
                s.has_focus = false;
            }
            return Ok(());
        }
        if let Some(ref mut s) = app.sidebar {
            s.handle_key(key_event);
        }
        return Ok(());
    }

    // ---- Ctrl+B: toggle sidebar ----
    if key_event.code == KeyCode::Char('b')
        && key_event.modifiers == KeyModifiers::CONTROL
    {
        if app.sidebar.is_some() {
            app.sidebar = None;
        } else {
            app.sidebar = Some(crate::ui::components::sidebar::Sidebar::new());
        }
        return Ok(());
    }

    // ---- Ctrl+T: cycle work mode ----
    if key_event.code == KeyCode::Char('t')
        && key_event.modifiers == KeyModifiers::CONTROL
    {
        app.work_mode = app.work_mode.next(app.config.allow_yolo);
        session::persist_config(app);
        return Ok(());
    }

    // ---- provider management panel (modal) ----
    if let Some(panel) = app.provider_panel.as_mut() {
        if matches!(key_event.code, KeyCode::Char('q' | 'Q' | 'c' | 'C'))
            && key_event.modifiers == KeyModifiers::CONTROL
        {
            app.events.send(AppEvent::Quit);
            return Ok(());
        }
        match panel.handle_key(key_event, &mut app.config, &app.provider_manager) {
            PanelAction::Close => app.provider_panel = None,
            PanelAction::Saved => {
                session::persist_config(app);
                app.events.send(AppEvent::ProvidersChanged);
            }
            PanelAction::None => {}
        }
        return Ok(());
    }

    // ---- skills management panel (modal) ----
    if let Some(panel) = app.skills_panel.as_mut() {
        use crate::ui::components::skills_panel::PanelAction as SkillsAction;
        match panel.handle_key(key_event, &mut app.skill_registry) {
            SkillsAction::Close => app.skills_panel = None,
            SkillsAction::Saved => session::save_session(app),
            SkillsAction::None => {}
        }
        return Ok(());
    }

    // ---- MCP management panel (modal) ----
    if let Some(panel) = app.mcp_panel.as_mut() {
        use crate::ui::components::mcp_panel::PanelAction as McpAction;
        if matches!(key_event.code, KeyCode::Char('q' | 'Q' | 'c' | 'C'))
            && key_event.modifiers == KeyModifiers::CONTROL
        {
            app.events.send(AppEvent::Quit);
            return Ok(());
        }
        match panel.handle_key(key_event, &mut app.config) {
            McpAction::Close => app.mcp_panel = None,
            McpAction::Saved => {
                session::persist_config(app);
                app.events.send(AppEvent::McpChanged);
            }
            McpAction::None => {}
        }
        return Ok(());
    }

    // ---- completion-popup navigation ----
    if app
        .input_panel
        .completion
        .as_ref()
        .map_or(false, |c| c.visible)
    {
        match key_event.code {
            KeyCode::Tab => {
                let content = app.input_panel.get_content();
                if let Some(c) = app.input_panel.completion.as_mut() {
                    if content == c.line(c.selected) {
                        if c.candidates.len() == 1 {
                            app.input_panel.completion = None;
                            return Ok(());
                        }
                        c.selected = (c.selected + 1) % c.candidates.len();
                    }
                    let visible = 10usize;
                    if c.selected < c.scroll_offset {
                        c.scroll_offset = c.selected;
                    } else if c.selected >= c.scroll_offset + visible {
                        c.scroll_offset = c.selected - visible + 1;
                    }
                    let text = c.line(c.selected);
                    app.input_panel.set_content(&text);
                }
                return Ok(());
            }
            KeyCode::Up => {
                if let Some(ref mut c) = app.input_panel.completion {
                    if c.selected > 0 {
                        c.selected -= 1;
                    } else {
                        c.selected = c.candidates.len().saturating_sub(1);
                    }
                    if c.selected < c.scroll_offset {
                        c.scroll_offset = c.selected;
                    }
                    let text = c.line(c.selected);
                    app.input_panel.set_content(&text);
                }
                return Ok(());
            }
            KeyCode::Down => {
                if let Some(ref mut c) = app.input_panel.completion {
                    c.selected = (c.selected + 1) % c.candidates.len();
                    let visible = 10usize;
                    if c.selected >= c.scroll_offset + visible {
                        c.scroll_offset = c.selected - visible + 1;
                    }
                    let text = c.line(c.selected);
                    app.input_panel.set_content(&text);
                }
                return Ok(());
            }
            KeyCode::Esc | KeyCode::Char('q') if key_event.modifiers == KeyModifiers::CONTROL => {
                app.input_panel.completion = None;
                return Ok(());
            }
            _ => {}
        }
    }

    // ---- Esc: cancel current stream ----
    if key_event.code == KeyCode::Esc && app.conversation_panel.is_busy() {
        app.events.send(AppEvent::Cancel);
        return Ok(());
    }

    // ---- Ctrl+C / Ctrl+Q: quit ----
    if key_event.modifiers == KeyModifiers::CONTROL
        && matches!(
            key_event.code,
            KeyCode::Char('c') | KeyCode::Char('C') | KeyCode::Char('q') | KeyCode::Char('Q')
        )
    {
        app.events.send(AppEvent::Quit);
        return Ok(());
    }

    // ---- text input ----
    match key_event.code {
        KeyCode::Char(_c) => {
            app.input_panel.input(key_event);
            update_completions(app);
        }
        KeyCode::Backspace => {
            app.input_panel.input(key_event);
            update_completions(app);
        }
        KeyCode::Delete => {
            app.input_panel.input(key_event);
            update_completions(app);
        }
        KeyCode::Up => {
            if app.input_panel.get_content().is_empty() {
                if let Some(pending) = app.conversation_panel.pending_message.take() {
                    app.input_panel.set_content(&pending);
                } else {
                    app.input_panel.history_up();
                }
            } else if app.input_panel.is_navigating_history() {
                app.input_panel.history_up();
            } else {
                app.input_panel.input(key_event);
            }
        }
        KeyCode::Enter if is_newline_modifier(key_event.modifiers) => {
            app.input_panel.insert_newline();
            update_completions(app);
        }
        KeyCode::Enter => {
            let text = app.input_panel.get_content();
            if text.starts_with('/') {
                commands::execute_command(app, &text).await;
            } else {
                app.events.send(AppEvent::Start);
            }
        }
        _ => {
            app.input_panel.input(key_event);
            update_completions(app);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Sidebar click handling
// ---------------------------------------------------------------------------

fn handle_sidebar_click(app: &mut App<'_>, target: &ClickTarget) {
    match target {
        ClickTarget::Section(key) => {
            if let Some(ref mut s) = app.sidebar {
                s.toggle_section(*key);
            }
        }
        ClickTarget::TodoItem(idx) => {
            let mut sorted: Vec<&crate::todos::Todo> =
                app.todo_list.todos.iter().collect();
            sorted.sort_by_key(|t| {
                crate::ui::components::sidebar::ui::todo_status_order(&t.status)
            });
            if let Some(todo) = sorted.get(*idx) {
                let id = todo.id.clone();
                let _ = app.todo_list.toggle_status(&id);
                let _ = app.todo_list.save_to_file();
            }
        }
        ClickTarget::Diagnostic(_idx) => {
            // Could jump to file location in the future.
        }
        ClickTarget::None => {}
    }
}

// ---------------------------------------------------------------------------
// Tick & completions
// ---------------------------------------------------------------------------

/// Handles the tick event of the terminal.
pub(crate) fn tick(_app: &App<'_>) {}

/// Recompute tab-completion candidates from the current input text.
pub(crate) fn update_completions(app: &mut App<'_>) {
    let content = app.input_panel.get_content();
    if content.starts_with('/') {
        app.input_panel.completion =
            CompletionEngine::complete(&content, &app.provider_manager, &app.skill_registry);
        if let Some(ref mut c) = app.input_panel.completion {
            c.visible = true;
        }
    } else {
        app.input_panel.completion = None;
    }
}
