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

//! Keyboard and paste handling: modal panels first (approval, question,
//! plan review, todo/provider/skills/MCP panels), then global shortcuts,
//! then the input panel.

use super::super::{commands, session, App};
use super::update_completions;
use crate::classifier::WorkMode;
use crate::ui::components::provider_panel::PanelAction;
use crate::ui::event::AppEvent;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Handles the key events and updates the state of [`App`].
pub(crate) async fn handle_key_events(
    app: &mut App<'_>,
    key_event: KeyEvent,
) -> color_eyre::Result<()> {
    // ---- interactive terminal panel (fully modal; grabs input) ----
    if app.terminal_pane.is_some() {
        handle_terminal_key(app, key_event);
        return Ok(());
    }
    // ---- tool-call approval (Manual mode) ----
    if app.pending_review.is_some() {
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
    if app.sidebar.as_ref().is_some_and(|s| s.has_focus) {
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
        .is_some_and(|c| c.visible)
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
            } else if text.starts_with('!') {
                commands::run_bang_command(app, &text);
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

/// Handle a key while the interactive terminal panel is open. `Ctrl+O` toggles
/// input grab. While grabbed, keys are translated to terminal bytes and written
/// to the task's PTY; while released, `Esc`/`q` close the panel.
fn handle_terminal_key(app: &mut App<'_>, key_event: KeyEvent) {
    use crate::ui::components::terminal_panel::key_event_to_bytes;

    let Some(pane) = app.terminal_pane.as_mut() else {
        return;
    };

    // Ctrl+O is the escape hatch — never forwarded.
    if key_event.code == KeyCode::Char('o')
        && key_event.modifiers.contains(KeyModifiers::CONTROL)
    {
        pane.grabbed = !pane.grabbed;
        return;
    }

    if pane.grabbed {
        // Cursor keys need the child's DECCKM mode to pick CSI vs SS3.
        let app_cursor = crate::tasks::with_screen(pane.task_id, |s| s.application_cursor())
            .unwrap_or(false);
        if let Some(bytes) = key_event_to_bytes(key_event, app_cursor) {
            let _ = crate::tasks::write_bytes(pane.task_id, &bytes);
            // Typing snaps the view back to live output.
            crate::tasks::scroll_screen(pane.task_id, i32::MIN);
        }
        return;
    }

    // Released: the panel owns its keys.
    match key_event.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.terminal_pane = None;
        }
        _ => {}
    }
}

/// Forward a mouse event to the terminal panel's PTY when input is grabbed and
/// the program has enabled mouse reporting. Swallowed otherwise.
pub(crate) fn handle_terminal_mouse(app: &mut App<'_>, mouse: crossterm::event::MouseEvent) {
    use crate::ui::components::terminal_panel::mouse_event_to_bytes;
    use crossterm::event::MouseEventKind;

    let Some(pane) = app.terminal_pane.as_ref() else {
        return;
    };
    let mode = crate::tasks::with_screen(pane.task_id, |s| s.mouse_protocol_mode())
        .unwrap_or(vt100::MouseProtocolMode::None);

    // The wheel scrolls the local scrollback unless a grabbed program is
    // consuming the mouse itself (then the wheel is forwarded below).
    let program_wants_mouse = pane.grabbed && mode != vt100::MouseProtocolMode::None;
    match mouse.kind {
        MouseEventKind::ScrollUp if !program_wants_mouse => {
            crate::tasks::scroll_screen(pane.task_id, 3);
            return;
        }
        MouseEventKind::ScrollDown if !program_wants_mouse => {
            crate::tasks::scroll_screen(pane.task_id, -3);
            return;
        }
        _ => {}
    }

    // Everything else is only forwarded while grabbed.
    if !pane.grabbed {
        return;
    }
    let Some(grid) = pane.grid else {
        return;
    };
    if let Some(bytes) = mouse_event_to_bytes(mouse, grid, mode) {
        let _ = crate::tasks::write_bytes(pane.task_id, &bytes);
    }
}

/// Enter combined with any of these modifiers inserts a newline instead of sending.
pub(crate) fn is_newline_modifier(modifiers: KeyModifiers) -> bool {
    modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT)
}

/// Handles text pasted into the terminal (bracketed paste).
pub(crate) fn handle_paste(app: &mut App<'_>, data: String) {
    // While the terminal panel has input grabbed, a paste goes to the PTY.
    if let Some(pane) = app.terminal_pane.as_ref() {
        if pane.grabbed {
            let _ = crate::tasks::write_bytes(pane.task_id, data.as_bytes());
        }
        return;
    }
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
// Tool-call approval (Manual mode)
// ---------------------------------------------------------------------------

/// Handle approval keys when tool calls are queued.
fn handle_approval_key(
    app: &mut App<'_>,
    key_event: KeyEvent,
) -> color_eyre::Result<()> {
    match key_event.code {
        KeyCode::Up | KeyCode::Char('k') => {
            if let Some(ref mut review) = app.pending_review {
                review.selected = review.selected.saturating_sub(1);
            }
        }
        KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
            if let Some(ref mut review) = app.pending_review
                && review.selected + 1 < 2
            {
                review.selected += 1;
            }
        }
        KeyCode::Enter => {
            if let Some(review) = app.pending_review.take() {
                use crate::engine::ReviewDecision;
                use async_openai::types::responses::{FunctionCallOutput, FunctionCallOutputItemParam};
                let decision = match review.selected {
                    0 => ReviewDecision::Approve,
                    _ => ReviewDecision::Deny {
                        output: crate::tools::ToolOutput {
                            param: FunctionCallOutputItemParam {
                                call_id: review.call.call_id.clone(),
                                output: FunctionCallOutput::Text(format!(
                                    "error: tool call denied by user in Manual mode — {}",
                                    review.reason,
                                )),
                                id: None,
                                status: None,
                            },
                            failed: true,
                            approval_label: Some(format!(
                                "{} denied in Manual mode by user",
                                WorkMode::Manual.icon()
                            )),
                        },
                    },
                };
                let _ = review.reply.0.send(decision);
            }
        }
        _ => {}
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Plan review (Plan mode Reviewing phase)
// ---------------------------------------------------------------------------

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
                    approve_plan(
                        app,
                        WorkMode::Manual,
                        "Plan approved — executing with Manual mode.",
                        "The plan was approved by the user. Execute it now using the identified steps. Ask for approval before running commands or destructive edits.",
                    )
                    .await;
                }
                1 => {
                    approve_plan(
                        app,
                        WorkMode::Auto,
                        "Plan approved — executing with Auto mode.",
                        "The plan was approved by the user. Execute it now using the identified steps.",
                    )
                    .await;
                }
                2 => {
                    if app.config.allow_yolo {
                        approve_plan(
                            app,
                            WorkMode::Yolo,
                            "Plan approved — executing with YOLO mode.",
                            "The plan was approved by the user. Execute it now using the identified steps. You have full autonomy.",
                        )
                        .await;
                    } else {
                        propose_plan_changes(app);
                    }
                }
                3 => {
                    propose_plan_changes(app);
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

/// Exit Plan mode into `mode` and kick off execution with a hidden
/// developer-role instruction.
async fn approve_plan(app: &mut App<'_>, mode: WorkMode, info: &str, hidden: &str) {
    app.work_mode = mode;
    app.plan_phase = crate::classifier::PlanPhase::default();
    app.plan_review_selected = 0;
    app.conversation_panel.add_info_string(info);
    session::save_session(app);
    commands::start_request_as(
        app,
        hidden.to_string(),
        async_openai::types::responses::InputRole::Developer,
    )
    .await;
}

/// Return to Planning phase so the user can type feedback on the plan.
fn propose_plan_changes(app: &mut App<'_>) {
    app.plan_phase = crate::classifier::PlanPhase::Planning;
    app.plan_review_selected = 0;
    app.conversation_panel
        .add_info_string("Enter your feedback in the input panel.");
    session::save_session(app);
}
