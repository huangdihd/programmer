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
use crate::ui::components::conversation_panel::conversation_panel::{
    ActivePhase, ConversationPanel,
};
use crate::ui::components::question_panel::QuestionPanel;
use crate::ui::event::{AppEvent, Event};
use async_openai::types::responses::InputImageContent;
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
            let _ = crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture);
        }
        crossterm::event::Event::Key(key_event) if key_event.kind == KeyEventKind::Press => {
            handle_key_events(app, key_event).await?
        }
        crossterm::event::Event::Paste(data) => keys::handle_paste(app, data),
        crossterm::event::Event::Mouse(_) if app.provider_panel.is_some() => {}
        // The task viewer owns the whole screen. Interactive tasks can forward
        // mouse input to their PTY; read-only tasks use the wheel to scroll.
        crossterm::event::Event::Mouse(mouse) if app.terminal_pane.is_some() => {
            keys::handle_terminal_mouse(app, mouse);
        }
        crossterm::event::Event::Mouse(mouse) => mouse::handle_mouse(app, mouse),
        _ => {}
    }
    Ok(())
}

/// Returns true when an operation id from an event matches the current active
/// turn. Non-turn events (Cancel, Start, …) always pass through.
fn is_current_turn(app: &App<'_>, op_id: u64) -> bool {
    is_current_turn_id(app.cancel.active_id, op_id)
}

/// Non-terminal events are accepted only while their operation is both current
/// and live. A cancelled operation remains current until its finish event
/// arrives, but its late phase/prompt/chunk events must not resurrect UI state.
fn is_live_turn(app: &App<'_>, op_id: u64) -> bool {
    is_live_turn_id(
        app.cancel.active_id,
        app.cancel.active.is_cancelled(),
        op_id,
    )
}

/// Core check: does `event_op_id` belong to the turn identified by `active_id`?
/// `event_op_id == 0` means "untagged" and always passes (pre-operation-id or
/// non-turn events). Exposed so tests exercise the same logic [`is_current_turn`]
/// calls.
fn is_current_turn_id(active_id: Option<u64>, event_op_id: u64) -> bool {
    event_op_id == 0 || active_id == Some(event_op_id)
}

fn is_live_turn_id(active_id: Option<u64>, cancelled: bool, event_op_id: u64) -> bool {
    is_current_turn_id(active_id, event_op_id) && (event_op_id == 0 || !cancelled)
}

/// Dispatch an [`AppEvent`] to its handler.
async fn handle_app_event(app: &mut App<'_>, app_event: AppEvent) {
    match app_event {
        AppEvent::Cancel => handle_cancel(app).await,
        AppEvent::ChunkReceived(op_id, chunk) => {
            if !is_live_turn(app, op_id) {
                return;
            }
            if app.conversation_panel.receiving_response.is_some() {
                app.conversation_panel.handle_response_stream_event(*chunk);
            }
        }
        AppEvent::ResponseCommitted(op_id) => {
            if !is_live_turn(app, op_id) {
                return;
            }
            app.conversation_panel.commit_live();
            app.sync_todos_from_store();
        }
        AppEvent::RunnerPhase(op_id, p) => {
            if !is_live_turn(app, op_id) {
                return;
            }
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
        AppEvent::ReviewRequest {
            call,
            reason,
            position,
            reply,
            operation_id,
        } => {
            if !is_live_turn(app, operation_id) {
                // Drop the sender so the runner's review() gets a closed-channel
                // denial instead of hanging.
                return;
            }
            app.pending_review = Some(PendingReview {
                call,
                reason,
                position,
                reply,
                selected: 0,
            });
            app.conversation_panel.phase = ActivePhase::None;
        }
        AppEvent::TurnFinished(op_id, result) => {
            if !is_current_turn(app, op_id) {
                return;
            }
            // Clear the active operation so stale events from this (or any
            // earlier) turn are dropped and Esc won't try to cancel a
            // turn that has already ended.
            app.cancel.active_id = None;
            // A prompt may have been installed just before cancellation won the
            // race. Turn completion is the final defensive cleanup boundary.
            app.pending_review = None;
            app.question_panel = None;
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
                    // The Cancelling phase already showed the message; stay
                    // silent here to avoid a duplicate.
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
            app.sync_todos_from_store();
            session::mark_dirty(app);
            // Restore mouse capture — external commands may disable it.
            let _ = crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture);
            // Start any queued follow-up request. handle_cancel deliberately
            // does NOT start it (it waits for us), so we start it for both
            // normal completion and cancellation — exactly once per turn.
            if let Some((pending_request, images)) =
                take_pending_request(&mut app.conversation_panel, &mut app.pending_images)
            {
                commands::start_request_with_images(app, pending_request, images).await;
            }
        }
        AppEvent::Start => {
            diagnostics::maybe_seed_diagnostics_baseline(app);
            commands::send_message(app).await;
        }
        AppEvent::StartInit => handle_start_init(app),
        AppEvent::BangFinished(task_id) => handle_bang_finished(app, task_id).await,
        AppEvent::CompactFinished(op_id, result, cancel_token) => {
            if !is_current_turn(app, op_id) {
                return;
            }
            handle_compact_finished(app, result, cancel_token);
            // handle_compact_finished clears active_id and resets the phase
            // back to idle. If the user queued a message while compacting,
            // start it now — just like TurnFinished does for normal turns.
            if let Some((pending_request, images)) =
                take_pending_request(&mut app.conversation_panel, &mut app.pending_images)
            {
                commands::start_request_with_images(app, pending_request, images).await;
            }
        }
        AppEvent::Quit => app.quit(),
        AppEvent::ProvidersChanged => handle_providers_changed(app).await,
        AppEvent::McpChanged => handle_mcp_changed(app).await,
        AppEvent::QuestionPrompt {
            question,
            answer_tx,
            operation_id,
        } => {
            if !is_live_turn(app, operation_id) {
                return;
            }
            app.question_panel = Some(QuestionPanel::new(question, answer_tx));
        }
    }
}

fn take_pending_request(
    panel: &mut ConversationPanel,
    pending_images: &mut Vec<InputImageContent>,
) -> Option<(String, Vec<InputImageContent>)> {
    panel
        .pending_message
        .take()
        .map(|text| (text, std::mem::take(pending_images)))
}

// ---------------------------------------------------------------------------
// Per-variant AppEvent handlers
// ---------------------------------------------------------------------------

/// Cancel: stop the in-flight runner turn. Transitions the UI to Cancelling
/// and does NOT go idle or start a queued request — the matching
/// `TurnFinished` event will handle that once the runner actually stops.
async fn handle_cancel(app: &mut App<'_>) {
    // No active turn → nothing to cancel.
    if app.cancel.active_id.is_none() {
        return;
    }
    // Already cancelling — the runner hasn't finished yet.
    if app.conversation_panel.phase == ActivePhase::Cancelling {
        return;
    }
    // Cancel the turn's root token; the runner's spawned task checks this token
    // between every iteration and stops.
    app.cancel.active.cancel();
    app.conversation_panel.abort_receiving();
    app.conversation_panel.phase = ActivePhase::Cancelling;
    app.conversation_panel.flush_usage();
    app.conversation_panel
        .add_info_string("Request cancelled by user.".to_string());
    // Release any blocking UI prompts so the runner's review() / ask_user
    // futures unblock and can reach the next cancel check-point.
    app.pending_review = None;
    app.question_panel = None;
    session::mark_dirty(app);
    // Do NOT start queued requests here — wait for TurnFinished.
}

/// `/init`: seed the init prompt and start the first runner turn.
fn handle_start_init(app: &mut App<'_>) {
    // StartInit is normally queued synchronously by `/init`. Keep this guard so
    // duplicate or externally injected events can never replace a live turn's
    // cancellation token and operation id.
    if app.cancel.active_id.is_some() {
        app.conversation_panel
            .add_warning_string("cannot initialize while a turn is in flight");
        return;
    }
    app.conversation_panel.add_meta(
        "\u{25B8} Initializing project\u{2026}",
        helpers::init_prompt(),
    );
    app.conversation_panel.reset_accumulated_usage();
    diagnostics::maybe_seed_diagnostics_baseline(app);
    session::mark_dirty(app);
    // Fresh turn: start from an un-cancelled root token.
    app.cancel.active = CancellationToken::new();
    app.cancel.next_id = app.cancel.next_id.wrapping_add(1);
    let operation_id = app.cancel.next_id;
    app.cancel.active_id = Some(operation_id);

    // Spawn the init turn through the same runner path.
    let Some(runner) = app.build_runner() else {
        app.cancel.active_id = None;
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
        operation_id,
        cancel: app.cancel.active.clone(),
    };
    let shared = app.conversation_panel.shared_conversation();
    let cancel = app.cancel.active.clone();
    let tx = app.events.sender.clone();
    tokio::spawn(async move {
        let result = runner.run_turn(&shared, &cancel, &surface).await;
        let _ = tx.send(Event::App(AppEvent::TurnFinished(operation_id, result)));
    });
}

/// `/compact` finished: install the summary as the new context boundary, or
/// surface the error. Always clears the active operation id and phase so a
/// cancelled compaction doesn't leave the UI stuck in Cancelling.
fn handle_compact_finished(
    app: &mut App<'_>,
    result: Result<String, String>,
    cancel_token: CancellationToken,
) {
    app.cancel.active_id = None;
    app.conversation_panel.phase = ActivePhase::None;
    if cancel_token.is_cancelled() {
        return;
    }
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
        app.conversation_panel
            .add_info_string(format!("current model reset to: {}", app.current_model));
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

    // Interactive panels auto-close once their task is gone. Read-only panels
    // stay open so the final captured output remains inspectable.
    if let Some(pane) = app
        .terminal_pane
        .as_mut()
        .filter(|pane| pane.accepts_input())
    {
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
                        "\u{1F5A5} terminal [{}] {} — {status}",
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

// Maximum transcript characters replayed to the model for a `!command`.
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

#[cfg(test)]
mod tests {
    use super::{ConversationPanel, is_current_turn_id, is_live_turn_id, take_pending_request};

    #[test]
    fn is_current_turn_allows_untagged_zero_events() {
        assert!(is_current_turn_id(Some(1), 0));
    }

    #[test]
    fn is_current_turn_passes_when_ids_match() {
        assert!(is_current_turn_id(Some(5), 5));
    }

    #[test]
    fn is_current_turn_filters_stale_events() {
        assert!(!is_current_turn_id(Some(3), 7));
    }

    #[test]
    fn is_current_turn_always_passes_zero_op_id() {
        assert!(is_current_turn_id(Some(42), 0));
        assert!(is_current_turn_id(None, 0));
        assert!(is_current_turn_id(Some(99), 0));
    }

    #[test]
    fn is_current_turn_filters_when_no_active_turn() {
        assert!(!is_current_turn_id(None, 5));
        assert!(!is_current_turn_id(None, 1));
    }

    #[test]
    fn is_current_turn_filters_lower_id() {
        // A stale event from an older, lower-numbered turn.
        assert!(!is_current_turn_id(Some(5), 3));
    }

    #[test]
    fn cancelled_turn_rejects_late_non_terminal_events() {
        assert!(is_live_turn_id(Some(7), false, 7));
        assert!(
            !is_live_turn_id(Some(7), true, 7),
            "late phase and prompt events must not revive cancelled UI state"
        );
        assert!(!is_live_turn_id(Some(8), false, 7));
        assert!(
            is_live_turn_id(None, true, 0),
            "untagged non-turn events retain their compatibility behavior"
        );
    }

    #[test]
    fn pending_request_is_drained_with_images_exactly_once() {
        use async_openai::types::responses::{ImageDetail, InputImageContent};

        let mut panel = ConversationPanel::new();
        panel.pending_message = Some("queued during compact".to_string());
        let mut images = vec![InputImageContent {
            detail: ImageDetail::Auto,
            file_id: None,
            image_url: Some("data:image/png;base64,AAAA".to_string()),
        }];

        let (text, taken_images) =
            take_pending_request(&mut panel, &mut images).expect("pending request");
        assert_eq!(text, "queued during compact");
        assert_eq!(taken_images.len(), 1);
        assert!(images.is_empty());
        assert!(take_pending_request(&mut panel, &mut images).is_none());
    }
}
