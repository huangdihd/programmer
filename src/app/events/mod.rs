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
use crate::cancel::CancellationToken;
use crate::classifier::WorkMode;
use crate::commands::CompletionEngine;
use crate::response::partial_response::PartialResponse;
use crate::ui::components::conversation_panel::conversation_panel::ActivePhase;
use crate::ui::components::question_panel::QuestionPanel;
use crate::ui::event::{AppEvent, Event};
use async_openai::error::OpenAIError;
use async_openai::types::responses::FunctionToolCall;
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

/// Dispatch an [`AppEvent`] to its handler. Each variant's logic lives in a
/// dedicated `handle_*` function below; this only routes.
async fn handle_app_event(app: &mut App<'_>, app_event: AppEvent) {
    match app_event {
        AppEvent::Cancel => handle_cancel(app).await,
        AppEvent::ChunkReceived(chunk) => stream::handle_chunk_events(app, chunk).await,
        AppEvent::OpenAIErrorReceived(error) => handle_openai_error(app, error).await,
        AppEvent::ResponseFinished(partial_response) => {
            handle_response_finished(app, partial_response).await
        }
        AppEvent::ClassificationCompleted {
            allowed,
            denied,
            ask_queue,
            cancel_token,
        } => handle_classification_completed(app, allowed, denied, ask_queue, cancel_token),
        AppEvent::ToolCallsCompleted(outputs, cancel_token) => {
            handle_tool_calls_completed(app, outputs, cancel_token)
        }
        AppEvent::DiagnosticsCompleted {
            snapshot,
            reminder_due,
            seed,
            cancel_token,
        } => handle_diagnostics_completed(app, snapshot, reminder_due, seed, cancel_token),
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

/// Cancel: stop the in-flight stream and post-stream pipeline, then start any
/// queued follow-up request.
async fn handle_cancel(app: &mut App<'_>) {
    // Cancel the turn's root token; every phase (the in-flight stream and the
    // post-stream classify/tool pipeline) runs against a child of it, so this
    // one call stops whichever is currently running.
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

/// A streaming error ended the turn: surface it and mark the session dirty.
async fn handle_openai_error(app: &mut App<'_>, error: OpenAIError) {
    stream::handle_error_events(app, error).await;
    session::mark_dirty(app);
}

/// A response finished: run any tool calls it requested, otherwise close out
/// the turn (handling plan mode and any queued follow-up request).
async fn handle_response_finished(app: &mut App<'_>, partial_response: PartialResponse) {
    if partial_response.cancelled.is_cancelled() {
        return;
    }
    let cancel_token = partial_response.cancelled.clone();
    let calls = helpers::function_calls(&partial_response);
    if calls.is_empty() {
        // Plan mode: if in Planning phase and model stopped without making
        // tool calls, it has finished presenting the plan.
        if app.work_mode == WorkMode::Plan
            && app.plan_phase == crate::classifier::PlanPhase::Planning
        {
            app.plan_phase = crate::classifier::PlanPhase::Reviewing;
            app.conversation_panel.phase = ActivePhase::None;
            app.conversation_panel.flush_usage();
            session::mark_dirty(app);
            return;
        }
        app.conversation_panel.phase = ActivePhase::None;
        app.conversation_panel.flush_usage();
        if let Some(pending_request) = app.conversation_panel.pending_message.take() {
            commands::start_request(app, pending_request).await
        }
        session::mark_dirty(app);
    } else {
        tools::run_tool_calls(app, calls, cancel_token);
    }
}

/// Classification finished: surface any denials that need approval, otherwise
/// run the allowed calls.
fn handle_classification_completed(
    app: &mut App<'_>,
    allowed: Vec<FunctionToolCall>,
    denied: Vec<crate::tools::ToolOutput>,
    ask_queue: Vec<(FunctionToolCall, String)>,
    cancel_token: CancellationToken,
) {
    if cancel_token.is_cancelled() {
        return;
    }
    if !ask_queue.is_empty() {
        for output in &denied {
            app.conversation_panel.add_tool_output(output.clone());
        }
        app.approval.queue = ask_queue;
        app.conversation_panel.phase = ActivePhase::None;
        return;
    }
    tools::process_classification_results(app, allowed, denied, cancel_token);
}

/// All tool calls ran: record their outputs, then either run diagnostics or
/// resume the stream.
fn handle_tool_calls_completed(
    app: &mut App<'_>,
    outputs: Vec<crate::tools::ToolOutput>,
    cancel_token: CancellationToken,
) {
    if cancel_token.is_cancelled() {
        return;
    }
    app.conversation_panel.phase = ActivePhase::None;
    app.diag.lsp_configured = helpers::lsp_checker_configured();
    let edited_files = tools::batch_edited_files(app, &outputs);
    for output in outputs {
        app.conversation_panel.add_tool_output(output);
    }
    app.todo_list = crate::todos::TodoList::load();
    session::mark_dirty(app);
    // Restore mouse capture — external commands disable it.
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::event::EnableMouseCapture
    );
    if !diagnostics::continue_with_diagnostics(app, edited_files, cancel_token.clone()) {
        stream::spawn_stream(app);
    }
}

/// Diagnostics finished: seed the baseline, or apply the diff and resume the
/// stream.
fn handle_diagnostics_completed(
    app: &mut App<'_>,
    snapshot: crate::diagnostics::Snapshot,
    reminder_due: bool,
    seed: bool,
    cancel_token: CancellationToken,
) {
    if cancel_token.is_cancelled() {
        return;
    }
    if seed {
        if app.diag.baseline.is_none() {
            app.diag.baseline = Some(snapshot.diagnostics);
        }
        return;
    }
    diagnostics::apply_diagnostics(app, snapshot, reminder_due);
    session::mark_dirty(app);
    // Restore mouse capture — diagnostic runners may reset console modes on
    // Windows.
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::event::EnableMouseCapture
    );
    stream::spawn_stream(app);
}

/// `/init`: seed the init prompt and kick off the first stream.
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
    stream::spawn_stream(app);
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
