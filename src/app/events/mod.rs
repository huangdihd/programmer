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
use super::PendingReview;
use super::{commands, diagnostics, helpers, session};
use crate::cancel::CancellationToken;
use crate::classifier::WorkMode;
use crate::commands::CompletionEngine;
use crate::response::partial_response::PartialResponse;
use crate::ui::components::conversation_panel::conversation_panel::ActivePhase;
use crate::ui::components::question_panel::QuestionPanel;
use crate::ui::event::{AppEvent, Event};
use crossterm::event::KeyEventKind;

// ---------------------------------------------------------------------------
// Main event handler
// ---------------------------------------------------------------------------

pub(crate) async fn handle_event(app: &mut App<'_>, event: Event) -> color_eyre::Result<()> {
    match event {
        Event::Tick => app.tick(),
        Event::Crossterm(event) => handle_crossterm(app, event).await?,
        Event::App(app_event) => handle_app_event(app, app_event).await,
    }
    Ok(())
}

/// Route a terminal event to the focus, keyboard, paste, and mouse handlers.
async fn handle_crossterm(
    app: &mut App<'_>,
    event: crossterm::event::Event,
) -> color_eyre::Result<()> {
    match event {
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
        // The interactive terminal owns the whole screen: forward the mouse to
        // its PTY when grabbed (and the program wants it), otherwise swallow it
        // so it doesn't scroll the hidden conversation beneath.
        crossterm::event::Event::Mouse(mouse) if app.terminal_pane.is_some() => {
            keys::handle_terminal_mouse(app, mouse);
        }
        crossterm::event::Event::Mouse(mouse) => mouse::handle_mouse(app, mouse),
        _ => {}
    }
    Ok(())
}

/// Dispatch an [`AppEvent`] to its handler.
async fn handle_app_event(app: &mut App<'_>, app_event: AppEvent) {
    match app_event {
        AppEvent::Cancel => handle_cancel(app).await,
        AppEvent::ChunkReceived(chunk) => {
            if app.conversation_panel.receiving_response.is_some() {
                app.conversation_panel.handle_response_stream_event(*chunk);
            }
        }
        AppEvent::ResponseCommitted => {
            app.conversation_panel.commit_live();
            // Reload in case the model ran a `todo` tool this iteration.
            app.todo_list = crate::todos::TodoList::load();
        }
        AppEvent::RunnerPhase(p) => {
            use crate::runner::RunnerPhase;
            app.conversation_panel.phase = match p {
                RunnerPhase::Streaming => {
                    app.conversation_panel.receiving_response =
                        Some(PartialResponse::new(app.cancel.active.child()));
                    ActivePhase::None // "Thinking" — derived from receiving_response
                }
                RunnerPhase::Classifying => ActivePhase::Classifying,
                RunnerPhase::RunningTools => ActivePhase::ToolRunning,
                RunnerPhase::Checking => ActivePhase::Checking,
            };
        }
        AppEvent::ReviewRequest { call, reason, position, reply } => {
            app.pending_review = Some(PendingReview {
                call,
                reason,
                position,
                reply,
                selected: 0,
            });
            app.conversation_panel.phase = ActivePhase::None;
        }
        AppEvent::TurnFinished(result) => {
            app.conversation_panel.abort_receiving();
            app.conversation_panel.phase = ActivePhase::None;
            app.conversation_panel.flush_usage();
            let was_ok = result.is_ok();
            match result {
                Err(crate::runner::RunnerError::Stream(e)) => {
                    app.conversation_panel.add_error(e);
                }
                Err(crate::runner::RunnerError::Api { message, .. }) => {
                    app.conversation_panel.add_error_string(message);
                }
                Err(crate::runner::RunnerError::Cancelled) => {
                    // Handled by handle_cancel.
                }
                Err(e @ crate::runner::RunnerError::EmptyResponse) => {
                    app.conversation_panel.add_error_string(e.to_string());
                }
                Ok(_) => {}
            }
            // Plan mode: if in Planning phase and turn finished successfully,
            // the model finished presenting the plan.
            if app.work_mode == WorkMode::Plan
                && app.plan_phase == crate::classifier::PlanPhase::Planning
                && was_ok
            {
                app.plan_phase = crate::classifier::PlanPhase::Reviewing;
            }
            app.todo_list = crate::todos::TodoList::load();
            session::mark_dirty(app);
            // Restore mouse capture — external commands may disable it.
            let _ = crossterm::execute!(
                std::io::stdout(),
                crossterm::event::EnableMouseCapture
            );
            // Start any queued follow-up request.
            if let Some(pending_request) = app.conversation_panel.pending_message.take() {
                commands::start_request(app, pending_request).await;
            }
        }
        AppEvent::Start => {
            diagnostics::maybe_seed_diagnostics_baseline(app);
            commands::send_message(app).await;
        }
        AppEvent::StartInit => handle_start_init(app),
        AppEvent::BangFinished(task_id) => handle_bang_finished(app, task_id).await,
        AppEvent::CompactFinished(result, cancel_token) => {
            handle_compact_finished(app, result, cancel_token)
        }
        AppEvent::Quit => app.quit(),
        AppEvent::ProvidersChanged => handle_providers_changed(app).await,
        AppEvent::McpChanged => handle_mcp_changed(app).await,
        AppEvent::QuestionPrompt {
            question,
            answer_tx,
        } => {
            app.question_panel = Some(QuestionPanel::new(question, answer_tx));
        }
    }
}

// ---------------------------------------------------------------------------
// Per-variant AppEvent handlers
// ---------------------------------------------------------------------------

/// Cancel: stop the in-flight runner turn, then start any queued follow-up.
async fn handle_cancel(app: &mut App<'_>) {
    // Cancel the turn's root token; the runner's spawned task checks this token
    // between every iteration and stops.
    app.cancel.active.cancel();
    app.conversation_panel.abort_receiving();
    app.conversation_panel.phase = ActivePhase::None;
    app.conversation_panel.flush_usage();
    app.conversation_panel
        .add_info_string("Request cancelled by user.".to_string());
    session::mark_dirty(app);
    if let Some(pending_request) = app.conversation_panel.pending_message.take() {
        commands::start_request(app, pending_request).await;
    }
}

/// `/init`: seed the init prompt and start the first runner turn.
fn handle_start_init(app: &mut App<'_>) {
    app.conversation_panel.add_meta(
        "\u{25B8} Initializing project\u{2026}",
        helpers::init_prompt(),
    );
    app.conversation_panel.reset_accumulated_usage();
    diagnostics::maybe_seed_diagnostics_baseline(app);
    session::mark_dirty(app);
    // Fresh turn: start from an un-cancelled root token.
    app.cancel.active = CancellationToken::new();
    // Kick off via the runner: re-use start_request_as with a developer role so
    // the hidden init prompt runs through the runner just like `/init` did before.
    let tokio_handle = tokio::runtime::Handle::current();
    tokio_handle.spawn(async move {
        // Can't call start_request_as from sync context; queue a synthetic Start
        // event instead.
    });
    // Spawn the init turn through the same runner path.
    let Some(runner) = app.build_runner() else {
        app.conversation_panel
            .add_error_string(format!("unknown provider/model: {}", app.current_model));
        return;
    };
    let surface = super::surface::TuiSurface {
        tx: app.events.sender.clone(),
        skill_prompt: app.skill_registry.combined_prompt(),
        plan_prompt: None,
        approval_label: format!(
            "{} approved by {} mode",
            app.work_mode.icon(),
            app.work_mode.label()
        ),
    };
    let shared = app.conversation_panel.shared_conversation();
    let cancel = app.cancel.active.clone();
    let tx = app.events.sender.clone();
    tokio::spawn(async move {
        let result = runner.run_turn(&shared, &cancel, &surface).await;
        let _ = tx.send(Event::App(AppEvent::TurnFinished(result)));
    });
}

/// `/compact` finished: install the summary as the new context boundary, or
/// surface the error. Results from a cancelled run are dropped.
fn handle_compact_finished(
    app: &mut App<'_>,
    result: Result<String, String>,
    cancel_token: CancellationToken,
) {
    if cancel_token.is_cancelled() {
        return;
    }
    app.conversation_panel.phase = ActivePhase::None;
    match result {
        Ok(summary) => {
            app.conversation_panel.apply_compaction(summary);
            app.conversation_panel.add_info_string(
                "Context compacted — older history is summarized for the model \
                 (click the divider to read the summary) but stays visible here."
                    .to_string(),
            );
        }
        Err(e) => {
            app.conversation_panel
                .add_error_string(format!("compaction failed: {e}"));
        }
    }
    session::mark_dirty(app);
}

/// Providers changed: rebuild the manager and reset the model if it vanished.
async fn handle_providers_changed(app: &mut App<'_>) {
    app.provider_manager = crate::providers::ProviderManager::new(&app.config).await;
    for msg in &app.provider_manager.startup_errors {
        app.conversation_panel.add_error_string(msg.clone());
    }
    if app.provider_manager.resolve(&app.current_model).is_none() {
        app.current_model = app.provider_manager.default_model();
        app.conversation_panel.add_info_string(format!(
            "current model reset to: {}",
            app.current_model
        ));
    }
}

/// MCP config changed: reload the servers (or clear them).
async fn handle_mcp_changed(app: &mut App<'_>) {
    if app.config.mcp_servers.is_empty() {
        app.mcp_manager = None;
        app.conversation_panel
            .add_info_string("MCP servers cleared.".to_string());
    } else {
        let mcp = crate::mcp::McpManager::from_config(&app.config.mcp_servers, ".").await;
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

// ---------------------------------------------------------------------------
// Tick & completions
// ---------------------------------------------------------------------------

/// Handles the tick event of the terminal.
///
/// Ticks fire at [`crate::consts::TICK_FPS`] to drive animation redraws; we
/// piggy-back on them to flush a dirty session once the current turn has gone
/// idle, debouncing saves to turn boundaries, and to watch interactive tasks
/// for exit (auto-closing the terminal panel, handing `!` results to the
/// agent).
pub(crate) fn tick(app: &mut App<'_>) {
    session::flush_if_dirty(app);
    poll_finished_terminals(app);
}

/// Consecutive ticks a task must be seen finished before acting on it. At
/// [`crate::consts::TICK_FPS`] (30) this is ~100 ms — enough for the PTY
/// reader thread to flush the tail of the output after the child exits.
const TASK_EXIT_GRACE_TICKS: u8 = 3;

/// Watch interactive tasks for exit: close the terminal panel (returning focus
/// to the input) and fire [`AppEvent::BangFinished`] for watched `!` commands.
fn poll_finished_terminals(app: &mut App<'_>) {
    use crate::tasks::TaskStatus;

    let is_running = |id: u64| {
        crate::tasks::snapshot(id)
            .map(|s| s.status == TaskStatus::Running)
            .unwrap_or(false)
    };

    // The open panel: auto-close once its task is gone.
    if let Some(pane) = app.terminal_pane.as_mut() {
        if is_running(pane.task_id) {
            pane.finished_ticks = 0;
        } else {
            pane.finished_ticks += 1;
            if pane.finished_ticks >= TASK_EXIT_GRACE_TICKS {
                let pane = app.terminal_pane.take().unwrap();
                // `!` tasks get their record via BangFinished below; announce
                // the close only for plain `/terminal` panes.
                if !app.bang_watch.iter().any(|(id, _)| *id == pane.task_id) {
                    let status = crate::tasks::snapshot(pane.task_id)
                        .map(|s| s.status.label())
                        .unwrap_or("gone");
                    app.conversation_panel.add_info_string(format!(
                        "🖥 terminal [{}] {} — {status}",
                        pane.task_id, pane.name
                    ));
                }
            }
        }
    }

    // Watched `!` commands: exited tasks go to the agent (even if the user
    // closed the panel early and the command finished in the background).
    let mut i = 0;
    while i < app.bang_watch.len() {
        let (id, ref mut ticks) = app.bang_watch[i];
        if is_running(id) {
            *ticks = 0;
            i += 1;
        } else {
            *ticks += 1;
            if *ticks >= TASK_EXIT_GRACE_TICKS {
                app.bang_watch.remove(i);
                app.events.send(AppEvent::BangFinished(id));
            } else {
                i += 1;
            }
        }
    }
}

/// Maximum transcript characters replayed to the model for a `!command`.
const MAX_BANG_TRANSCRIPT: usize = 10_000;

/// A watched `!command` exited: compose its transcript into a user message and
/// start a turn so the agent responds to the outcome.
async fn handle_bang_finished(app: &mut App<'_>, task_id: u64) {
    use crate::tasks::TaskStatus;

    let Some(snap) = crate::tasks::snapshot(task_id) else {
        return;
    };
    let status = match (snap.status, snap.exit_code) {
        (TaskStatus::Killed, _) => "killed".to_string(),
        (_, Some(code)) => format!("exited with code {code}"),
        (s, None) => s.label().to_string(),
    };
    let transcript = crate::tasks::transcript(task_id).unwrap_or_default();
    let transcript = transcript.trim();
    let body = if transcript.is_empty() {
        "(no output)"
    } else {
        // Keep the tail — the interesting part — under the cap.
        let mut start = transcript.len().saturating_sub(MAX_BANG_TRANSCRIPT);
        while !transcript.is_char_boundary(start) {
            start += 1;
        }
        &transcript[start..]
    };
    let text = format!(
        "!{}\n[interactive terminal session {status}]\n```\n{body}\n```",
        snap.command
    );
    commands::start_request(app, text).await;
}

/// Recompute tab-completion candidates from the current input text.
pub(crate) fn update_completions(app: &mut App<'_>) {
    let content = app.input_panel.get_content();
    app.input_panel.completion = if content.starts_with('/') {
        CompletionEngine::complete(&content, &app.provider_manager, &app.skill_registry)
    } else if content.starts_with('!') {
        // Shell-style completion for `!command` lines.
        CompletionEngine::complete_bang(&content)
    } else {
        // Non-slash input may still carry a trailing `@file` reference.
        CompletionEngine::complete_file_ref(&content)
    };
    if let Some(ref mut c) = app.input_panel.completion {
        c.visible = true;
    }
}
