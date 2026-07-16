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

//! Event dispatch: terminal events route to [`keys`] and [`mouse`];
//! application events ([`AppEvent`]) drive the request/tool pipeline here.

mod keys;
mod mouse;

pub(crate) use keys::handle_key_events;

use super::App;
use super::{commands, diagnostics, helpers, session, stream, tools};
use crate::classifier::WorkMode;
use crate::commands::CompletionEngine;
use crate::ui::components::conversation_panel::conversation_panel::ActivePhase;
use crate::ui::components::question_panel::QuestionPanel;
use crate::ui::event::{AppEvent, Event};
use crossterm::event::KeyEventKind;
use std::sync::atomic::Ordering;

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
            crossterm::event::Event::Paste(data) => keys::handle_paste(app, data),
            crossterm::event::Event::Mouse(_) if app.provider_panel.is_some() => {}
            crossterm::event::Event::Mouse(mouse) => mouse::handle_mouse(app, mouse),
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
