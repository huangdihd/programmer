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

use std::collections::HashMap;

use super::App;
use super::PendingReview;
use super::{commands, diagnostics, session};
use crate::cancel::{CancellationToken, OperationId};
use crate::classifier::WorkMode;
use crate::commands::CompletionEngine;
use crate::response::message_item::MessageItem;
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
    super::peers::sync_session(app);
    let was_ready = queue_ready(app);
    let suggestion_owner = match &event {
        Event::App(AppEvent::TurnFinished(id, Ok(_))) if app.cancel.is_current(*id) => Some(*id),
        _ => None,
    };
    let peer_tick = matches!(&event, Event::Tick);
    match event {
        Event::Tick => app.tick(),
        Event::Redraw => {}
        Event::SelectionScroll => app.conversation_panel.selection_auto_scroll_tick(),
        Event::Crossterm(event) => handle_crossterm(app, event).await?,
        Event::App(app_event) => handle_app_event(app, app_event).await,
    }
    let peers_ready = super::peers::poll(app, peer_tick).await;
    // One handoff boundary for every event, including closing a panel or
    // clearing a draft. Ready is scheduling eligibility, not a painted label.
    if app.running {
        dispatch_ready_work(app, !was_ready).await;
        if peers_ready {
            super::peers::start_ready_work(app).await;
        }
        if let Some(owner) = suggestion_owner
            && app.cancel.active_id.is_none()
            && app.conversation_panel.pending_message.is_none()
        {
            commands::maybe_start_input_suggestion(app, owner);
        }
    }
    if peer_tick {
        super::activity::tick(app);
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
            // External programs can alter mouse reporting. Restore whichever
            // mode the user selected when the terminal regains focus.
            let _ = crate::terminal::set_mouse_capture(!app.native_selection_mode);
        }
        crossterm::event::Event::Key(key_event) if key_event.kind == KeyEventKind::Press => {
            handle_key_events(app, key_event).await?
        }
        crossterm::event::Event::Paste(data) => keys::handle_paste(app, data),
        crossterm::event::Event::Mouse(_)
            if app.provider_panel.is_some()
                || app.skills_panel.is_some()
                || app.mcp_panel.is_some()
                || app.diagnostics_panel.is_some()
                || app.security_panel.is_some()
                || app.rewind_panel.is_some() => {}
        crossterm::event::Event::Mouse(mouse) if app.activity_panel.is_some() => {
            super::activity::handle_mouse(app, mouse);
        }
        // The task viewer owns the whole screen. Interactive tasks can forward
        // mouse input to their PTY; read-only tasks use the wheel to scroll.
        crossterm::event::Event::Mouse(mouse) if app.terminal_pane.is_some() => {
            keys::handle_terminal_mouse(app, mouse);
        }
        crossterm::event::Event::Mouse(mouse) if app.agent_panel.is_some() => {
            if let Some(panel) = app.agent_panel.as_mut() {
                panel.handle_mouse(mouse);
            }
        }
        crossterm::event::Event::Mouse(mouse) => mouse::handle_mouse(app, mouse),
        _ => {}
    }
    Ok(())
}

/// Non-terminal events are accepted only while their operation is both current
/// and live. A cancelled operation remains current until its finish event
/// arrives, but its late phase/prompt/chunk events must not resurrect UI state.
fn is_live_turn(app: &App<'_>, op_id: OperationId) -> bool {
    app.cancel.is_live(op_id)
}

/// Core check: does `event_op_id` belong to the turn identified by `active_id`?
/// `event_op_id == 0` means "untagged" and always passes (pre-operation-id or
/// non-turn events). Tests exercise the same identity checks as the lifecycle.
#[cfg(test)]
fn is_current_turn_id(active_id: Option<OperationId>, event_op_id: OperationId) -> bool {
    event_op_id.is_current(active_id)
}

#[cfg(test)]
fn is_live_turn_id(
    active_id: Option<OperationId>,
    cancelled: bool,
    event_op_id: OperationId,
) -> bool {
    event_op_id.is_live(active_id, cancelled)
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
                if app.conversation_panel.response_started() {
                    app.cancel.response_started = true;
                }
            }
        }
        AppEvent::ResponseCommitted(op_id) => {
            if !is_live_turn(app, op_id) {
                return;
            }
            app.cancel.activity = Some("response committed; finishing response".to_string());
            app.conversation_panel.commit_live();
            app.cancel.response_started = true;
            app.sync_todos_from_store();
        }
        AppEvent::KeepRetryAttempt(op_id) => {
            if !is_live_turn(app, op_id) {
                return;
            }
            app.cancel.activity = Some("retry backoff".to_string());
            app.conversation_panel.abort_receiving();
            app.conversation_panel.phase = ActivePhase::None;
        }
        AppEvent::RunnerActivity(op_id, description) => {
            if is_live_turn(app, op_id) {
                app.cancel.activity = Some(description);
            }
        }
        AppEvent::RunnerPhase(op_id, p) => {
            if !is_live_turn(app, op_id) {
                return;
            }
            app.cancel.activity = Some(p.label().to_string());
            use crate::runner::RunnerPhase;
            app.conversation_panel.phase = match p {
                RunnerPhase::Streaming => {
                    app.conversation_panel.begin_live_response();
                    app.conversation_panel.receiving_response =
                        Some(PartialResponse::new(app.cancel.active.child()));
                    ActivePhase::None // "Thinking" — derived from receiving_response
                }
                RunnerPhase::Classifying => ActivePhase::Classifying,
                RunnerPhase::Associating => ActivePhase::Associating,
                RunnerPhase::RunningTools => ActivePhase::ToolRunning,
                RunnerPhase::Checking => ActivePhase::Checking,
            };
        }
        AppEvent::WaitingSubagents(op_id, waiting) => {
            if is_live_turn(app, op_id) {
                app.waiting_for_subagents = waiting;
            }
        }
        AppEvent::UsageSafePoint(op_id, input_tokens, resume) => {
            if !is_live_turn(app, op_id) {
                return;
            }
            if app
                .mandatory_compact_tokens()
                .is_some_and(|limit| input_tokens >= limit)
            {
                app.auto_compact.mandatory_waiting = true;
                app.auto_compact.mandatory_resume = Some(resume);
                if !commands::maybe_start_auto_compact(app, input_tokens) {
                    fail_mandatory_compaction(
                        app,
                        "mandatory context compaction could not start at the provider boundary"
                            .to_string(),
                    );
                }
            } else {
                let _ = resume.send(());
            }
        }
        AppEvent::ReviewRequest {
            call,
            reason,
            position,
            reply,
            operation_id,
            agent_id,
            agent_generation,
        } => {
            if !is_live_turn(app, operation_id) {
                // Drop the sender so the runner's review() gets a closed-channel
                // denial instead of hanging.
                return;
            }
            if let (Some(generation), Some(id)) = (agent_generation, agent_id)
                && (generation != app.agents.generation()
                    || app
                        .agents
                        .snapshot(id)
                        .is_none_or(|agent| agent.status.is_terminal()))
            {
                return;
            }
            let review = PendingReview {
                call,
                reason,
                position,
                reply,
                selected: 0,
                operation_id,
                agent_id,
                agent_generation,
            };
            if app.pending_review.is_none() {
                app.pending_review = Some(review);
            } else {
                app.review_queue.push_back(review);
            }
            app.conversation_panel.phase = ActivePhase::None;
        }
        AppEvent::TurnFinished(op_id, result) => {
            if !app.cancel.finish(op_id) {
                return;
            }
            // Clear the active operation so stale events from this (or any
            // earlier) turn are dropped and Esc won't try to cancel a
            // turn that has already ended.
            app.waiting_for_subagents = false;
            // A prompt may have been installed just before cancellation won the
            // race. Turn completion is the final defensive cleanup boundary.
            discard_reviews_for_operation(app, op_id);
            if app.peers.consent.is_none() {
                app.question_panel = None;
            }
            app.peers.runner_prompts.retain(|(id, _)| *id != op_id);
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
                Err(
                    e @ (crate::runner::RunnerError::EmptyResponse
                    | crate::runner::RunnerError::StepLimit { .. }),
                ) => {
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
            // External commands may alter mouse capture. Restore the user's
            // current TUI/native-selection choice.
            let _ = crate::terminal::set_mouse_capture(!app.native_selection_mode);
        }
        AppEvent::Start => {
            diagnostics::maybe_seed_diagnostics_baseline(app);
            commands::send_message(app).await;
        }
        AppEvent::StartInit(prompt) => handle_start_init(app, prompt),
        AppEvent::TaskStateChanged(event) => handle_task_state_changed(app, event),
        AppEvent::AgentStateChanged { generation, id } => {
            handle_agent_state_changed(app, generation, id)
        }
        AppEvent::AgentPhase {
            generation,
            id,
            phase,
        } => handle_agent_phase(app, generation, id, phase),
        AppEvent::Notice(op_id, text) => {
            // A notice from a superseded turn would be noise, so it is held to
            // the same liveness rule as phase updates.
            if is_live_turn(app, op_id) {
                app.conversation_panel.add_info_string(text);
            }
        }
        AppEvent::FlushTaskNotifications(token) => {
            flush_task_notifications(app, token).await;
        }
        AppEvent::FlushAgentNotifications(token) => {
            flush_agent_notifications(app, token).await;
        }
        AppEvent::DreamPreviewFinished(operation_id, result) => {
            if !app.cancel.is_current(operation_id) {
                return;
            }
            let cancelled = app.cancel.active.is_cancelled();
            app.cancel.finish(operation_id);
            app.conversation_panel.phase = ActivePhase::None;
            if cancelled {
                app.conversation_panel
                    .add_info_string("Dream preview cancelled; memories were not applied.");
            } else {
                match result {
                    Ok(message) => app.conversation_panel.add_info_string(message),
                    Err(error) => app
                        .conversation_panel
                        .add_warning_string(format!("Dream preview failed: {error}")),
                }
            }
            session::mark_dirty(app);
        }
        AppEvent::CompactFinished(op_id, cutoff, result, cancel_token) => {
            if !app.cancel.finish(op_id) {
                return;
            }
            handle_compact_finished(app, cutoff, result, cancel_token);
        }
        AppEvent::AutoCompactFinished {
            job_id,
            history_epoch,
            cutoff,
            result,
        } => {
            if app.auto_compact.active_id != Some(job_id) {
                return;
            }
            app.auto_compact.active_id = None;
            if app.auto_compact.history_epoch != history_epoch {
                return;
            }
            let compaction = match result {
                Ok(compaction) => compaction,
                Err(error) => {
                    fail_mandatory_compaction(
                        app,
                        format!("automatic context compaction failed: {error}"),
                    );
                    return;
                }
            };
            let Some((input_tokens, output_tokens)) = reducing_compaction_usage(&compaction) else {
                fail_mandatory_compaction(
                    app,
                    "automatic context compaction rejected: provider did not prove the summary was smaller"
                        .to_string(),
                );
                return;
            };
            let turns = app.conversation_panel.compaction_turn_count(cutoff);
            let details = compaction_details(turns, &compaction);
            if !app
                .conversation_panel
                .apply_compaction_at(cutoff, compaction.summary)
            {
                fail_mandatory_compaction(
                    app,
                    "automatic context compaction rejected: history changed before installation"
                        .to_string(),
                );
                return;
            }
            if !app.auto_compact.mandatory_waiting {
                app.auto_compact.last_completed_item_count =
                    Some(app.conversation_panel.items_snapshot().len());
            }
            if let Some(stable_end) = app.cancel.turn_conversation_cutoff.as_mut()
                && cutoff <= *stable_end
            {
                *stable_end = stable_end.saturating_add(1);
            }
            if let Some(store) = &app.checkpoint_store
                && let Err(error) = store.lock().unwrap().record_conversation_insertion(cutoff)
            {
                app.conversation_panel.add_warning_string(format!(
                    "could not update rewind checkpoints after compaction: {error}"
                ));
            }
            // Announce the compaction only where the announcement is still
            // true. A pass that finishes inside a live turn — the mandatory
            // safe-point pass — resumes that turn, so the compaction is already
            // in effect for the turn's very next request: a "next turn" banner
            // would be stale the moment it was drawn, and nothing would consume
            // it until the user sent another message, leaving it above the input
            // box long after the compacted context had been used. Only a pass
            // that lands before its turn starts is genuinely about the next
            // turn, so only that case raises the banner. Anything still pending
            // is dropped, so a banner can never outlive the context it
            // described.
            if app.cancel.active_id.is_some() {
                app.input_panel.clear_next_turn_compaction();
            } else {
                app.input_panel.show_next_turn_compaction(details);
            }
            session::mark_dirty(app);
            session::flush_if_dirty(app);

            let previous_tokens = app.auto_compact.last_input_tokens.unwrap_or(input_tokens);
            let estimated_tokens =
                estimated_tokens_after_compaction(previous_tokens, input_tokens, output_tokens);
            app.auto_compact.last_input_tokens = Some(estimated_tokens);
            app.auto_compact.retry_blocked = false;

            if app.auto_compact.mandatory_waiting
                && app
                    .mandatory_compact_tokens()
                    .is_some_and(|limit| estimated_tokens >= limit)
            {
                app.auto_compact.last_cutoff = None;
                if commands::maybe_start_auto_compact(app, estimated_tokens) {
                    return;
                }
                fail_mandatory_compaction(
                    app,
                    "mandatory context compaction made progress but could not reduce the context below the hard limit"
                        .to_string(),
                );
                return;
            }
            if app.auto_compact.mandatory_waiting {
                finish_mandatory_compaction(app).await;
            }
        }
        AppEvent::SessionTitleGenerated {
            session_uuid,
            generation_id,
            result,
        } => {
            if app.session.uuid != session_uuid || app.session.title_generation_id != generation_id
            {
                return;
            }
            match result {
                Ok(title) => {
                    app.session.title = title;
                    session::mark_dirty(app);
                }
                Err(error) => app
                    .conversation_panel
                    .add_warning_string(format!("session title generation failed: {error}")),
            }
        }
        AppEvent::InputSuggestionGenerated {
            session_uuid,
            operation_id,
            result,
        } => {
            if app.session.uuid != session_uuid
                || app.active_suggestion_operation_id != Some(operation_id)
            {
                return;
            }
            app.active_suggestion_operation_id = None;
            app.input_suggestion_cancel = None;
            if let Ok(suggestion) = result
                && app.cancel.active_id.is_none()
                && app.input_panel.get_content().is_empty()
            {
                app.input_panel.set_suggestion(suggestion);
                session::mark_dirty(app);
                session::flush_if_dirty(app);
            }
        }
        AppEvent::Quit => handle_quit_request(app),
        AppEvent::ProvidersChanged => reload_provider_manager(app),
        AppEvent::RefreshProviderModels { name, notify } => {
            handle_provider_models_refresh(app, name, notify)
        }
        AppEvent::ProviderModelsRefreshed {
            requested_providers,
            models,
            startup_errors,
            notify,
        } => handle_provider_models_refreshed(
            app,
            requested_providers,
            models,
            startup_errors,
            notify,
        ),
        AppEvent::McpChanged => handle_mcp_changed(app),
        AppEvent::McpServerConnectionUpdated {
            generation,
            server_name,
            state,
        } => handle_mcp_server_connection_updated(app, generation, &server_name, state),
        AppEvent::McpReloaded {
            generation,
            manager,
        } => handle_mcp_reloaded(app, generation, *manager),
        AppEvent::DiagnosticsUpdated {
            generation,
            snapshot,
        } => {
            if !app
                .diagnostics_state
                .lock()
                .unwrap()
                .publish(generation, snapshot.as_ref())
            {
                return;
            }
            app.diag.lsp_configured = crate::app::helpers::lsp_checker_configured();
            match snapshot {
                None => {
                    app.conversation_panel.add_info_string(
                        "No diagnostics profile configured. Use /diagnostics manage to add one.",
                    );
                }
                Some(snapshot) => {
                    let rendered = snapshot.render();
                    if snapshot.errors.is_empty() {
                        app.conversation_panel
                            .add_info_string(format!("Diagnostics updated.\n{rendered}"));
                    } else {
                        app.conversation_panel.add_warning_string(format!(
                            "Diagnostics updated with checker errors.\n{rendered}"
                        ));
                    }
                }
            }
        }
        AppEvent::QuestionPrompt {
            question,
            answer_tx,
            operation_id,
        } => {
            if !is_live_turn(app, operation_id) {
                return;
            }
            let panel = QuestionPanel::new(question, answer_tx);
            if app.peers.consent.is_some() {
                app.peers.runner_prompts.push_back((operation_id, panel));
            } else {
                app.question_panel = Some(panel);
            }
        }
        AppEvent::UpdateAvailable(tag) => {
            app.conversation_panel.add_info_string(format!(
                "A newer version of programmer is available: {tag} — run `programmer upgrade` to update."
            ));
        }
    }
}

const QUIT_CONFIRM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
pub(crate) const QUIT_CONFIRM_WARNING: &str = "Press Ctrl+C again within 2 seconds to exit.";

pub(crate) fn is_quit_confirmation_warning(item: &MessageItem) -> bool {
    matches!(item, MessageItem::Warning(text) if text == QUIT_CONFIRM_WARNING)
}

pub(crate) fn remove_quit_confirmation_warning(panel: &mut ConversationPanel) {
    panel.remove_warning_string(QUIT_CONFIRM_WARNING);
}

fn handle_quit_request(app: &mut App<'_>) {
    let now = std::time::Instant::now();
    if quit_is_confirmed(app.quit_requested_at, now) {
        remove_quit_confirmation_warning(&mut app.conversation_panel);
        app.quit();
        return;
    }

    remove_quit_confirmation_warning(&mut app.conversation_panel);
    app.quit_requested_at = Some(now);
    app.conversation_panel
        .add_warning_string(QUIT_CONFIRM_WARNING);
}

fn quit_is_confirmed(previous: Option<std::time::Instant>, now: std::time::Instant) -> bool {
    previous.is_some_and(|pressed_at| now.duration_since(pressed_at) <= QUIT_CONFIRM_TIMEOUT)
}

fn quit_confirmation_expired(
    previous: Option<std::time::Instant>,
    now: std::time::Instant,
) -> bool {
    previous.is_some_and(|pressed_at| now.duration_since(pressed_at) > QUIT_CONFIRM_TIMEOUT)
}

fn expire_quit_confirmation(app: &mut App<'_>, now: std::time::Instant) {
    if quit_confirmation_expired(app.quit_requested_at, now) {
        app.quit_requested_at = None;
        remove_quit_confirmation_warning(&mut app.conversation_panel);
    }
}

fn take_pending_request(
    panel: &mut ConversationPanel,
    pending_images: &mut Vec<InputImageContent>,
) -> Option<super::scheduling::UserRequest> {
    panel
        .pending_message
        .take()
        .map(|text| super::scheduling::UserRequest {
            text,
            images: std::mem::take(pending_images),
        })
}

fn queue_ready(app: &App<'_>) -> bool {
    use super::scheduling::{StartDecision, StartupState, WorkSource};
    app.running && StartupState::from_app(app).decide(WorkSource::Queued) == StartDecision::Start
}

async fn dispatch_ready_work(app: &mut App<'_>, became_ready: bool) {
    if !queue_ready(app) {
        return;
    }
    app.task_notifications.discard_consumed();
    app.agent_notifications.discard_consumed(&app.agents);
    let now = std::time::Instant::now();
    let notifications_due = [
        app.task_notifications.ready_at,
        app.agent_notifications.ready_at,
    ]
    .into_iter()
    .flatten()
    .any(|deadline| now >= deadline);
    let has_notifications =
        !app.task_notifications.pending.is_empty() || !app.agent_notifications.pending.is_empty();
    if app.conversation_panel.pending_message.is_none()
        && !(has_notifications && (became_ready || notifications_due))
    {
        return;
    }
    // Runtime notifications share the same hard-limit gate as user input.
    if app.mandatory_compact_tokens().is_some_and(|limit| {
        app.auto_compact
            .last_input_tokens
            .is_some_and(|tokens| tokens >= limit)
    }) {
        app.auto_compact.mandatory_waiting = true;
        let tokens = app.auto_compact.last_input_tokens.expect("checked usage");
        if !commands::maybe_start_auto_compact(app, tokens) {
            fail_mandatory_compaction(app, "mandatory context compaction could not start".into());
        }
        return;
    }
    start_queued_work(app).await;
}

async fn start_queued_work(app: &mut App<'_>) {
    use super::scheduling::{StartDecision, StartupState, WorkSource};

    if StartupState::from_app(app).decide(WorkSource::Queued) != StartDecision::Start {
        return;
    }
    let pending_user = take_pending_request(&mut app.conversation_panel, &mut app.pending_images);
    app.task_notifications.discard_consumed();
    app.agent_notifications.discard_consumed(&app.agents);
    if app.task_notifications.pending.is_empty() && app.agent_notifications.pending.is_empty() {
        if let Some(request) = pending_user {
            commands::start_request_with_images(app, request.text, request.images).await;
        }
        return;
    }

    let events: Vec<_> = app.task_notifications.pending.drain(..).collect();
    app.task_notifications.ready_at = None;
    app.task_notifications.flush_requested = false;
    for event in &events {
        app.conversation_panel.add_info_string(format!(
            "Task #{} {} {} — notifying agent.",
            event.task_id,
            event.name,
            event.new_status.label()
        ));
    }
    let agent_ids: Vec<_> = app.agent_notifications.pending.drain(..).collect();
    app.agent_notifications.ready_at = None;
    app.agent_notifications.flush_requested = false;
    let agents: Vec<_> = agent_ids
        .into_iter()
        .filter_map(|id| {
            let snapshot = app.agents.snapshot(id)?;
            app.agents.consume_notification(id);
            app.conversation_panel.add_info_string(format!(
                "Sub-agent #{} {} {} — notifying parent agent.",
                snapshot.id,
                snapshot.name,
                snapshot.status.label()
            ));
            Some(snapshot)
        })
        .collect();
    session::mark_dirty(app);
    commands::start_runtime_update_request(app, events, agents, pending_user).await;
}

fn handle_task_state_changed(app: &mut App<'_>, event: crate::tasks::TaskLifecycleEvent) {
    if event.generation != app.tasks.current_generation() {
        return;
    }
    app.task_notifications.push(event);
}

/// Record a running sub-agent's turn phase so its sidebar row can show it.
/// Phases from a previous manager generation belong to a replaced agent
/// registry and are dropped.
fn handle_agent_phase(
    app: &mut App<'_>,
    generation: u64,
    id: u64,
    phase: crate::runner::RunnerPhase,
) {
    if generation == app.agents.generation() {
        app.agents.set_phase(id, phase);
    }
}

fn handle_agent_state_changed(app: &mut App<'_>, generation: u64, id: u64) {
    if generation != app.agents.generation() {
        return;
    }
    discard_reviews_for_agent(app, generation, id);
    if app.agents.should_notify_parent(id) {
        app.agent_notifications.push(id);
    }
}

async fn flush_task_notifications(app: &mut App<'_>, token: u64) {
    if token != app.task_notifications.flush_token {
        return;
    }
    app.task_notifications.flush_requested = false;
}

async fn flush_agent_notifications(app: &mut App<'_>, token: u64) {
    if token != app.agent_notifications.flush_token {
        return;
    }
    app.agent_notifications.flush_requested = false;
}

pub(crate) fn has_blocking_surface(app: &App<'_>) -> bool {
    app.pending_review.is_some()
        || app.question_panel.is_some()
        || app.provider_panel.is_some()
        || app.skills_panel.is_some()
        || app.mcp_panel.is_some()
        || app.diagnostics_panel.is_some()
        || app.security_panel.is_some()
        || app.todo_panel.is_some()
        || app.rewind_panel.is_some()
        || app.terminal_pane.is_some()
        || app.agent_panel.is_some()
        || app.activity_panel.is_some()
        || (app.work_mode == WorkMode::Plan
            && app.plan_phase == crate::classifier::PlanPhase::Reviewing)
}

fn discard_reviews_for_operation(app: &mut App<'_>, operation_id: OperationId) {
    if app
        .pending_review
        .as_ref()
        .is_some_and(|review| review.operation_id == operation_id)
    {
        app.pending_review = None;
    }
    app.review_queue
        .retain(|review| review.operation_id != operation_id);
    if app.pending_review.is_none() {
        app.pending_review = app.review_queue.pop_front();
    }
}

fn discard_reviews_for_agent(app: &mut App<'_>, generation: u64, agent_id: u64) {
    if app.pending_review.as_ref().is_some_and(|review| {
        review.agent_generation == Some(generation) && review.agent_id == Some(agent_id)
    }) {
        app.pending_review = None;
    }
    app.review_queue.retain(|review| {
        review.agent_generation != Some(generation) || review.agent_id != Some(agent_id)
    });
    if app.pending_review.is_none() {
        app.pending_review = app.review_queue.pop_front();
    }
}

// ---------------------------------------------------------------------------
// Per-variant AppEvent handlers
// ---------------------------------------------------------------------------

/// Cancel: stop the in-flight runner turn. Transitions the UI to Cancelling
/// and does NOT go idle or start a queued request — the matching
/// `TurnFinished` event will handle that once the runner actually stops.
async fn handle_cancel(app: &mut App<'_>) {
    app.peers.runner_prompts.clear();
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
    // Restore only into an empty composer with no submitted successor. Otherwise
    // keep the cancelled input in history and let queued work advance normally.
    let restore_draft = !app.cancel.response_started
        && app.conversation_panel.pending_message.is_none()
        && app.input_panel.get_content().is_empty();
    if app.cancel.activity.is_none()
        || app.cancel.activity.as_deref() == Some("streaming")
        || app.question_panel.is_some()
        || app.pending_review.is_some()
        || app.waiting_for_subagents
        || app.auto_compact.mandatory_waiting
    {
        app.cancel.activity = Some(
            app.resolve_status()
                .emoji_label()
                .split_once(' ')
                .map_or("runner", |(_, label)| label)
                .to_string(),
        );
    }
    app.cancel.cancel_current();
    if restore_draft && let Some(request) = app.cancel.active_user_request.take() {
        app.conversation_panel.truncate(request.conversation_cutoff);
        app.input_panel
            .remove_last_history_if(&request.history_text);
        app.input_panel.restore_draft(request.draft);
        if let (Some(checkpoint_id), Some(store)) =
            (app.current_checkpoint_id, app.checkpoint_store.as_ref())
        {
            let _ = store.lock().unwrap().truncate_after(checkpoint_id, None);
        }
        app.current_checkpoint_id = None;
        session::mark_dirty(app);
    }
    app.conversation_panel.abort_receiving();
    app.conversation_panel.phase = ActivePhase::Cancelling;
    app.conversation_panel.flush_usage();
    app.conversation_panel.add_info_string(
        "Cancellation requested; waiting for the active operation to finish.".to_string(),
    );
    // Release any blocking UI prompts so the runner's review() / ask_user
    // futures unblock and can reach the next cancel check-point.
    if let Some(operation_id) = app.cancel.active_id {
        discard_reviews_for_operation(app, operation_id);
    }
    if app.peers.consent.is_none() {
        app.question_panel = None;
    }
    session::mark_dirty(app);
    // Do NOT start queued requests here — wait for TurnFinished.
}

/// `/init`: seed the init prompt and start the first runner turn.
fn handle_start_init(app: &mut App<'_>, prompt: String) {
    // StartInit is normally queued synchronously by `/init`. Keep this guard so
    // duplicate or externally injected events can never replace a live turn's
    // cancellation token and operation id.
    if app.cancel.active_id.is_some() {
        app.conversation_panel
            .add_warning_string("cannot initialize while a turn is in flight");
        return;
    }
    let conversation_cutoff = app.conversation_panel.items_snapshot().len();
    app.conversation_panel
        .add_meta("\u{25B8} Initializing project\u{2026}", prompt);
    app.conversation_panel.reset_accumulated_usage();
    diagnostics::maybe_seed_diagnostics_baseline(app);
    session::mark_dirty(app);
    // Fresh turn: start from an un-cancelled root token.
    let operation_id = app.cancel.begin(Some(conversation_cutoff));

    // Spawn the init turn through the same runner path.
    let Some(runner) = app.build_runner() else {
        app.cancel.clear();
        app.conversation_panel
            .add_error_string(format!("unknown provider/model: {}", app.current_model));
        return;
    };
    let surface = super::surface::TuiSurface {
        tx: app.events.sender.clone(),
        skill_prompt: app.skill_registry.catalog_prompt(),
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

fn reducing_compaction_usage(
    compaction: &crate::ui::event::CompactionResult,
) -> Option<(u32, u32)> {
    compaction
        .input_tokens
        .zip(compaction.output_tokens)
        .filter(|(input, output)| output < input)
}

fn estimated_tokens_after_compaction(
    previous_tokens: u32,
    compact_input_tokens: u32,
    compact_output_tokens: u32,
) -> u32 {
    previous_tokens
        .saturating_sub(compact_input_tokens)
        .saturating_add(compact_output_tokens)
}

fn fail_mandatory_compaction(app: &mut App<'_>, message: String) {
    app.conversation_panel.add_warning_string(message);
    if !app.auto_compact.mandatory_waiting {
        return;
    }
    app.auto_compact.mandatory_waiting = false;
    app.auto_compact.retry_blocked = true;
    app.conversation_panel.add_warning_string(
        "Context compaction is blocked; queued input has not been sent. Retry with /compact or edit and resend the queued input with Up then Enter."
            .to_string(),
    );
    // Never let a blocked in-flight runner continue above the hard limit. A
    // queued user request remains queued for a later retry.
    if app.auto_compact.mandatory_resume.is_some() {
        app.cancel.active.cancel();
    }
    if let Some(resume) = app.auto_compact.mandatory_resume.take() {
        let _ = resume.send(());
    }
}

async fn finish_mandatory_compaction(app: &mut App<'_>) {
    app.auto_compact.mandatory_waiting = false;
    if let Some(resume) = app.auto_compact.mandatory_resume.take() {
        let _ = resume.send(());
    }
    app.auto_compact.retry_blocked = false;
}

fn compaction_details(turns: usize, compaction: &crate::ui::event::CompactionResult) -> String {
    let turn_label = if turns == 1 { "turn" } else { "turns" };
    match (compaction.input_tokens, compaction.output_tokens) {
        (Some(input), Some(output)) => {
            format!("{turns} {turn_label}, {input}→{output} tokens")
        }
        _ => format!("{turns} {turn_label}"),
    }
}

/// `/compact` finished: install the summary as the new context boundary, or
/// surface the error after lifecycle ownership is released. Always resets the
/// phase so cancelled compaction cannot leave the UI stuck in Cancelling.
fn handle_compact_finished(
    app: &mut App<'_>,
    cutoff: usize,
    result: Result<crate::ui::event::CompactionResult, String>,
    cancel_token: CancellationToken,
) {
    app.conversation_panel.phase = ActivePhase::None;
    if cancel_token.is_cancelled() {
        return;
    }
    match result {
        Ok(compaction) => {
            let Some((_input_tokens, output_tokens)) = reducing_compaction_usage(&compaction)
            else {
                app.conversation_panel.add_warning_string(
                    "context compaction rejected: provider did not prove the summary was smaller"
                        .to_string(),
                );
                return;
            };
            app.auto_compact.last_input_tokens = Some(output_tokens);
            app.auto_compact.retry_blocked = false;
            if app
                .conversation_panel
                .apply_compaction_at(cutoff, compaction.summary)
                && let Some(store) = &app.checkpoint_store
                && let Err(error) = store.lock().unwrap().record_conversation_insertion(cutoff)
            {
                app.conversation_panel.add_warning_string(format!(
                    "could not update rewind checkpoints after compaction: {error}"
                ));
            }
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
fn reload_provider_manager(app: &mut App<'_>) {
    app.provider_manager = crate::providers::ProviderManager::from_config(&app.config);
    app.provider_model_statuses = crate::providers::ProviderModelStatus::from_config(&app.config);
    if app.provider_manager.resolve(&app.current_model).is_none() {
        app.current_model = app.provider_manager.default_model();
        app.conversation_panel
            .add_info_string(format!("current model reset to: {}", app.current_model));
    }
    if app
        .config
        .providers
        .values()
        .any(|provider| provider.models.is_none())
    {
        handle_provider_models_refresh(app, None, false);
    }
}

/// `/providers refresh`: kick off background model discovery so the event
/// loop stays responsive while the network fetches run.
fn handle_provider_models_refresh(app: &mut App<'_>, name: Option<String>, notify: bool) {
    // If a specific provider was requested, validate it exists.
    if let Some(ref provider_name) = name
        && !app.config.providers.contains_key(provider_name)
    {
        app.conversation_panel
            .add_error_string(format!("unknown provider: {provider_name}"));
        return;
    }

    let providers = if let Some(ref provider_name) = name {
        let mut filtered = HashMap::new();
        if let Some(config) = app.config.providers.get(provider_name) {
            filtered.insert(provider_name.clone(), config.clone());
        }
        filtered
    } else {
        app.config.providers.clone()
    };
    let requested_providers = providers
        .iter()
        .filter(|(_, provider)| provider.models.is_none())
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    for status in &mut app.provider_model_statuses {
        if requested_providers.contains(&status.name) {
            status.state = crate::providers::ProviderModelState::Refreshing;
        }
    }

    let clients = app.provider_manager.clients().clone();
    let tx = app.events.sender.clone();
    tokio::spawn(async move {
        let (models, startup_errors) =
            crate::providers::ProviderManager::discover_models(&providers, &clients).await;
        let _ = tx.send(Event::App(AppEvent::ProviderModelsRefreshed {
            requested_providers,
            models,
            startup_errors,
            notify,
        }));
    });
}

/// Background model discovery finished: apply the fresh lists and report.
fn handle_provider_models_refreshed(
    app: &mut App<'_>,
    requested_providers: Vec<String>,
    models: std::collections::HashMap<String, Vec<String>>,
    startup_errors: Vec<String>,
    notify: bool,
) {
    let model_count = models.values().map(|v| v.len()).sum::<usize>();
    let provider_count = models.len();
    let error_count = startup_errors.len();
    for status in &mut app.provider_model_statuses {
        if requested_providers.contains(&status.name) {
            status.state = models.get(&status.name).map_or(
                crate::providers::ProviderModelState::Failed,
                |models| crate::providers::ProviderModelState::Ready {
                    model_count: models.len(),
                },
            );
        }
    }
    app.provider_manager
        .apply_model_refresh(models, startup_errors);
    if !notify {
        return;
    }
    if error_count == 0 {
        app.conversation_panel.add_info_string(format!(
            "Provider models refreshed: {model_count} model(s) across {provider_count} provider(s)."
        ));
    } else {
        let error_label = if error_count == 1 { "error" } else { "errors" };
        app.conversation_panel.add_warning_string(format!(
            "Provider model refresh incomplete: loaded {model_count} model(s) from \
             {provider_count} provider(s); {error_count} {error_label}. \
             Providers remain usable — retry with /providers refresh [provider]."
        ));
    }
}

/// MCP config changed: start a background reload (or clear the manager).
fn handle_mcp_changed(app: &mut App<'_>) {
    app.mcp_reload_generation = app.mcp_reload_generation.wrapping_add(1);
    let generation = app.mcp_reload_generation;
    app.mcp_manager = None;
    app.mcp_server_statuses = app
        .config
        .mcp_servers
        .iter()
        .map(|server| crate::mcp::McpServerStatus::connecting(server.name.clone()))
        .collect();

    if app.config.mcp_servers.is_empty() {
        app.conversation_panel
            .add_info_string("MCP servers cleared.".to_string());
        return;
    }

    let configs = app.config.mcp_servers.clone();
    let tx = app.events.sender.clone();
    tokio::spawn(async move {
        let manager = crate::mcp::McpManager::from_config_with_updates(
            &configs,
            ".",
            |server_name, state| {
                let _ = tx.send(Event::App(AppEvent::McpServerConnectionUpdated {
                    generation,
                    server_name,
                    state,
                }));
            },
        )
        .await;
        let _ = tx.send(Event::App(AppEvent::McpReloaded {
            generation,
            manager: Box::new(manager),
        }));
    });
}

fn handle_mcp_server_connection_updated(
    app: &mut App<'_>,
    generation: u64,
    server_name: &str,
    state: crate::mcp::McpConnectionState,
) {
    if generation != app.mcp_reload_generation {
        return;
    }
    let Some(server) = app
        .mcp_server_statuses
        .iter_mut()
        .find(|server| server.name == server_name)
    else {
        return;
    };
    server.state = state;
}

/// Apply a completed MCP reload if it still matches the latest config.
fn handle_mcp_reloaded(app: &mut App<'_>, generation: u64, manager: crate::mcp::McpManager) {
    if generation != app.mcp_reload_generation {
        return;
    }
    for error in &manager.startup_errors {
        app.conversation_panel.add_error_string(error.clone());
    }
    app.mcp_manager = Some(std::sync::Arc::new(manager));
}

// ---------------------------------------------------------------------------
// Tick & completions
// ---------------------------------------------------------------------------

/// Handles the tick event of the terminal.
///
/// Ticks fire at 10 FPS while a busy status is active; we use them to refresh
/// the elapsed timer, flush a dirty session once the current turn has gone
/// idle, debounce saves to turn boundaries, and watch interactive tasks for
/// exit (auto-closing the terminal panel, handing `!` results to the agent).
pub(crate) fn tick(app: &mut App<'_>) {
    expire_quit_confirmation(app, std::time::Instant::now());
    session::flush_if_dirty(app);
    poll_finished_terminals(app);
    if app.cancel.active_id.is_none()
        && !app.task_notifications.pending.is_empty()
        && !app.task_notifications.flush_requested
        && !has_blocking_surface(app)
        && app
            .task_notifications
            .ready_at
            .is_some_and(|ready| std::time::Instant::now() >= ready)
    {
        app.task_notifications.flush_requested = true;
        app.events.send(AppEvent::FlushTaskNotifications(
            app.task_notifications.flush_token,
        ));
    }
    app.agent_notifications.discard_consumed(&app.agents);
    if app.cancel.active_id.is_none()
        && !app.agent_notifications.pending.is_empty()
        && !app.agent_notifications.flush_requested
        && !has_blocking_surface(app)
        && app
            .agent_notifications
            .ready_at
            .is_some_and(|ready| std::time::Instant::now() >= ready)
    {
        app.agent_notifications.flush_requested = true;
        app.events.send(AppEvent::FlushAgentNotifications(
            app.agent_notifications.flush_token,
        ));
    }
}

/// Consecutive ticks a task must be seen finished before acting on it. At
/// 3 ticks at 10 FPS gives ~300 ms — enough for the PTY reader thread to
/// flush the tail of the output after the child exits.
const TASK_EXIT_GRACE_TICKS: u8 = 3;

/// Watch interactive tasks for exit and close their terminal panel after the
/// reader has had a brief chance to flush the final screen.
fn poll_finished_terminals(app: &mut App<'_>) {
    use crate::tasks::TaskStatus;

    let is_running = |id: u64| {
        app.tasks
            .snapshot(id)
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
                let status = app
                    .tasks
                    .snapshot(pane.task_id)
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

/// Recompute tab-completion candidates from the current input text.
pub(crate) fn update_completions(app: &mut App<'_>) {
    let content = app.input_panel.get_content();
    app.input_panel.completion = if content.starts_with('/') {
        CompletionEngine::complete(
            &app.tasks,
            &content,
            &app.provider_manager,
            &app.skill_registry,
        )
    } else if content.starts_with('!') {
        // Shell-style completion for `!command` lines.
        CompletionEngine::complete_bang(&content)
    } else {
        // Non-slash input may still carry a trailing diagnostic or file reference.
        let diagnostics = app
            .diagnostics_state
            .lock()
            .unwrap()
            .baseline
            .clone()
            .unwrap_or_default();
        CompletionEngine::complete_reference(&content, &diagnostics)
    };
    if let Some(ref mut c) = app.input_panel.completion {
        c.visible = true;
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ActivePhase, ConversationPanel, QUIT_CONFIRM_TIMEOUT, QUIT_CONFIRM_WARNING,
        estimated_tokens_after_compaction, handle_app_event, handle_cancel, handle_event,
        is_current_turn_id, is_live_turn_id, quit_confirmation_expired, quit_is_confirmed,
        reducing_compaction_usage, remove_quit_confirmation_warning, take_pending_request,
    };
    use crate::cancel::OperationId;
    use crate::response::message_item::MessageItem;
    use crate::ui::event::{AppEvent, Event};
    use std::time::{Duration, Instant};

    #[tokio::test]
    async fn dream_preview_yields_to_ui_and_cancellation_releases_only_its_owner() {
        use crate::app::commands::start_dream_preview;
        use crate::ui::components::conversation_panel::conversation_panel::ActivePhase;
        let mut app = headless_test_app("dream-preview-lifecycle").await;
        let (started, ready) = tokio::sync::oneshot::channel();
        start_dream_preview(&mut app, |cancellation| async move {
            let _ = started.send(());
            cancellation.wait().await;
            Err("cancelled test preview".into())
        });
        let operation = app.cancel.active_id.unwrap();
        tokio::time::timeout(Duration::from_secs(2), ready)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(app.conversation_panel.phase, ActivePhase::Associating);
        start_dream_preview(&mut app, |_| async {
            panic!("must not replace active preview")
        });
        assert_eq!(app.cancel.active_id, Some(operation));
        // The event loop remains usable while the fake provider never completes.
        handle_app_event(
            &mut app,
            AppEvent::DreamPreviewFinished(OperationId(operation.0 + 1), Ok("stale".into())),
        )
        .await;
        assert_eq!(app.cancel.active_id, Some(operation));
        handle_cancel(&mut app).await;
        assert_eq!(app.conversation_panel.phase, ActivePhase::Cancelling);
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let event = app.events.next().await.unwrap();
                if let Event::App(AppEvent::DreamPreviewFinished(id, result)) = event {
                    assert_eq!(id, operation);
                    handle_app_event(&mut app, AppEvent::DreamPreviewFinished(id, result)).await;
                    break;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(app.cancel.active_id, None);
        assert_eq!(app.conversation_panel.phase, ActivePhase::None);
        for result in [Ok("preview ready".into()), Err("provider failed".into())] {
            let successor = app.cancel.begin(None);
            app.conversation_panel.phase = ActivePhase::Associating;
            handle_app_event(
                &mut app,
                AppEvent::DreamPreviewFinished(operation, Ok("late".into())),
            )
            .await;
            assert_eq!(app.cancel.active_id, Some(successor));
            handle_app_event(&mut app, AppEvent::DreamPreviewFinished(successor, result)).await;
            assert_eq!(app.cancel.active_id, None);
            assert_eq!(app.conversation_panel.phase, ActivePhase::None);
        }
    }

    #[test]
    fn compaction_usage_requires_proven_reduction() {
        use crate::ui::event::CompactionResult;

        let result = |input_tokens, output_tokens| CompactionResult {
            summary: "summary".to_string(),
            input_tokens,
            output_tokens,
        };
        assert_eq!(
            reducing_compaction_usage(&result(Some(100), Some(40))),
            Some((100, 40))
        );
        assert_eq!(
            reducing_compaction_usage(&result(Some(100), Some(100))),
            None
        );
        assert_eq!(
            reducing_compaction_usage(&result(Some(100), Some(101))),
            None
        );
        assert_eq!(reducing_compaction_usage(&result(None, Some(40))), None);
        assert_eq!(reducing_compaction_usage(&result(Some(100), None)), None);
    }

    #[test]
    fn mandatory_compaction_estimate_tracks_replaced_tokens() {
        assert_eq!(
            estimated_tokens_after_compaction(180_000, 80_000, 20_000),
            120_000
        );
        assert_eq!(
            estimated_tokens_after_compaction(180_000, 20_000, 10_000),
            170_000
        );
        assert_eq!(
            estimated_tokens_after_compaction(10_000, 20_000, 3_000),
            3_000
        );
    }

    #[test]
    fn quit_requires_a_second_request_within_timeout() {
        let first_press = Instant::now();

        assert!(!quit_is_confirmed(None, first_press));
        assert!(quit_is_confirmed(
            Some(first_press),
            first_press + QUIT_CONFIRM_TIMEOUT
        ));
        assert!(!quit_is_confirmed(
            Some(first_press),
            first_press + QUIT_CONFIRM_TIMEOUT + Duration::from_millis(1)
        ));
    }

    #[test]
    fn quit_confirmation_expires_after_timeout() {
        let first_press = Instant::now();

        assert!(!quit_confirmation_expired(None, first_press));
        assert!(!quit_confirmation_expired(
            Some(first_press),
            first_press + QUIT_CONFIRM_TIMEOUT
        ));
        assert!(quit_confirmation_expired(
            Some(first_press),
            first_press + QUIT_CONFIRM_TIMEOUT + Duration::from_millis(1)
        ));
    }

    #[test]
    fn quit_confirmation_warning_can_be_removed_without_touching_other_warnings() {
        let mut panel = ConversationPanel::new();
        panel.add_warning_string(QUIT_CONFIRM_WARNING);
        panel.add_warning_string("keep this warning");

        remove_quit_confirmation_warning(&mut panel);

        let items = panel.items_snapshot();
        assert!(!items.iter().any(
            |item| matches!(item, MessageItem::Warning(text) if text == QUIT_CONFIRM_WARNING)
        ));
        assert!(
            items.iter().any(
                |item| matches!(item, MessageItem::Warning(text) if text == "keep this warning")
            )
        );
    }

    #[test]
    fn is_current_turn_allows_untagged_zero_events() {
        assert!(is_current_turn_id(Some(OperationId(1)), OperationId(0)));
    }

    #[test]
    fn is_current_turn_passes_when_ids_match() {
        assert!(is_current_turn_id(Some(OperationId(5)), OperationId(5)));
    }

    #[test]
    fn is_current_turn_filters_stale_events() {
        assert!(!is_current_turn_id(Some(OperationId(3)), OperationId(7)));
    }

    #[test]
    fn is_current_turn_always_passes_zero_op_id() {
        assert!(is_current_turn_id(Some(OperationId(42)), OperationId(0)));
        assert!(is_current_turn_id(None, OperationId(0)));
        assert!(is_current_turn_id(Some(OperationId(99)), OperationId(0)));
    }

    #[test]
    fn is_current_turn_filters_when_no_active_turn() {
        assert!(!is_current_turn_id(None, OperationId(5)));
        assert!(!is_current_turn_id(None, OperationId(1)));
    }

    #[test]
    fn is_current_turn_filters_lower_id() {
        // A stale event from an older, lower-numbered turn.
        assert!(!is_current_turn_id(Some(OperationId(5)), OperationId(3)));
    }

    #[test]
    fn cancelled_turn_rejects_late_non_terminal_events() {
        assert!(is_live_turn_id(Some(OperationId(7)), false, OperationId(7)));
        assert!(
            !is_live_turn_id(Some(OperationId(7)), true, OperationId(7)),
            "late phase and prompt events must not revive cancelled UI state"
        );
        assert!(!is_live_turn_id(
            Some(OperationId(8)),
            false,
            OperationId(7)
        ));
        assert!(
            is_live_turn_id(None, true, OperationId(0)),
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

        let request = take_pending_request(&mut panel, &mut images).expect("pending request");
        assert_eq!(request.text, "queued during compact");
        assert_eq!(request.images.len(), 1);
        assert!(images.is_empty());
        assert!(take_pending_request(&mut panel, &mut images).is_none());
    }

    /// An `App` with no providers: enough for tests that drive UI state
    /// transitions without any network or session persistence.
    async fn headless_test_app(session: &str) -> crate::app::App<'static> {
        let mut config = crate::config::programmer_config::ProgrammerConfig::default();
        config.providers.clear();
        crate::app::App::new(
            config,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            crate::tasks::TaskManager::default(),
            session.to_string(),
            None,
            Vec::new(),
            false,
            "test".to_string(),
        )
        .await
    }

    #[tokio::test]
    async fn graph_mouse_and_escape_stay_inside_the_read_only_modal() {
        use crate::ui::components::activity_panel::{ActivityEntry, ActivityMode, ActivityPanel};
        use crossterm::event::{
            Event as TerminalEvent, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind,
        };
        use ratatui::{buffer::Buffer, layout::Rect};

        let mut app = headless_test_app("graph-routing").await;
        let entries = ["first", "second"].map(|id| ActivityEntry {
            id: id.into(),
            title: id.into(),
            summary: String::new(),
            details: String::new(),
            from: Some(id.into()),
            to: Some("graph-routing".into()),
            rollback_allowed: false,
        });
        let mut panel = ActivityPanel::new(
            "Graph".into(),
            ActivityMode::SessionGraph,
            "graph-routing".into(),
            entries.to_vec(),
        );
        let area = Rect::new(0, 0, 120, 30);
        panel.render(area, &mut Buffer::empty(area));
        app.activity_panel = Some(panel);
        app.cancel.active_id = Some(crate::cancel::OperationId(42));
        let original_input = app.input_panel.get_content();
        super::handle_crossterm(
            &mut app,
            TerminalEvent::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 2,
                row: 2,
                modifiers: KeyModifiers::NONE,
            }),
        )
        .await
        .unwrap();
        assert_eq!(
            app.activity_panel
                .as_ref()
                .unwrap()
                .selected_entry()
                .unwrap()
                .id,
            "second"
        );
        for code in [KeyCode::Enter, KeyCode::Enter, KeyCode::Esc, KeyCode::Esc] {
            super::handle_crossterm(
                &mut app,
                TerminalEvent::Key(KeyEvent::new(code, KeyModifiers::NONE)),
            )
            .await
            .unwrap();
            assert!(app.activity_panel.is_some());
            assert!(super::has_blocking_surface(&app));
        }
        super::handle_crossterm(&mut app, TerminalEvent::Paste("blocked".into()))
            .await
            .unwrap();
        assert_eq!(app.input_panel.get_content(), original_input);
        super::handle_crossterm(
            &mut app,
            TerminalEvent::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        )
        .await
        .unwrap();
        assert!(app.activity_panel.is_none());
        assert_eq!(app.cancel.active_id, Some(crate::cancel::OperationId(42)));
        assert!(!app.cancel.active.is_cancelled());
    }

    /// A proven-smaller compaction result whose usage numbers are enough to
    /// pass every validity check the event handler applies.
    fn compaction_result() -> crate::ui::event::CompactionResult {
        crate::ui::event::CompactionResult {
            summary: "earlier turns summarized".to_string(),
            input_tokens: Some(200_000),
            output_tokens: Some(3_000),
        }
    }

    async fn finish_auto_compaction(app: &mut crate::app::App<'_>, cutoff: usize) {
        handle_event(
            app,
            Event::App(AppEvent::AutoCompactFinished {
                job_id: 1,
                history_epoch: 0,
                cutoff,
                result: Ok(compaction_result()),
            }),
        )
        .await
        .expect("dispatching the compaction result must not fail");
    }

    #[tokio::test]
    async fn a_compaction_before_a_queued_turn_announces_itself_above_the_input() {
        use async_openai::types::responses::{InputContent, InputMessage, InputRole, OutputStatus};

        let mut app = headless_test_app("compaction-banner-queued").await;
        for turn in 0..2 {
            app.conversation_panel.add_input_message(
                async_openai::types::responses::MessageItem::Input(InputMessage {
                    content: vec![InputContent::InputText(format!("turn {turn}").into())],
                    role: InputRole::User,
                    status: Some(OutputStatus::Completed),
                }),
            );
            // Separate the two requests, so they are not grouped into one turn.
            app.conversation_panel
                .add_info_string(format!("reply {turn}"));
        }
        app.auto_compact.active_id = Some(1);
        app.auto_compact.mandatory_waiting = true;
        // No turn is live: the queued request has not started yet, so the next
        // model turn really is the first to use the compacted context.
        app.cancel.active_id = None;

        finish_auto_compaction(&mut app, 4).await;

        assert_eq!(
            app.input_panel.next_turn_compaction.as_deref(),
            Some("2 turns, 200000→3000 tokens"),
            "the next turn must be told what it is about to run on"
        );
    }

    #[tokio::test]
    async fn failed_mandatory_compaction_keeps_input_and_allows_explicit_retry() {
        let mut app = headless_test_app("compaction-failure-retry").await;
        app.auto_compact.active_id = Some(1);
        app.auto_compact.last_cutoff = Some(42);
        app.auto_compact.last_input_tokens = Some(u32::MAX);
        app.auto_compact.mandatory_waiting = true;
        app.conversation_panel.pending_message = Some("111".to_string());
        handle_event(
            &mut app,
            Event::App(AppEvent::AutoCompactFinished {
                job_id: 1,
                history_epoch: 0,
                cutoff: 42,
                result: Err("aborted".to_string()),
            }),
        )
        .await
        .unwrap();
        assert_eq!(
            app.conversation_panel.pending_message.as_deref(),
            Some("111")
        );
        assert!(!app.auto_compact.mandatory_waiting);
        assert!(app.auto_compact.active_id.is_none());
        assert_eq!(app.auto_compact.last_cutoff, Some(42));
        assert!(app.auto_compact.retry_blocked);
        for _ in 0..3 {
            handle_event(&mut app, Event::Redraw).await.unwrap();
            assert_eq!(app.auto_compact.last_cutoff, Some(42));
            assert_eq!(
                app.conversation_panel.pending_message.as_deref(),
                Some("111")
            );
        }
        // No historical prefix exists in this fixture, so no API call starts,
        // but the explicit request must clear the failed-prefix dedup marker.
        crate::app::commands::start_request_with_images(&mut app, "retry".into(), Vec::new()).await;
        assert_eq!(app.auto_compact.last_cutoff, None);
        assert!(app.conversation_panel.pending_message.is_some());
        assert!(
            app.auto_compact.retry_blocked,
            "synchronous startup failure also blocks automatic retry"
        );
        let retained = app.conversation_panel.pending_message.clone();
        for _ in 0..3 {
            handle_event(&mut app, Event::Redraw).await.unwrap();
            assert_eq!(app.conversation_panel.pending_message, retained);
        }
    }

    #[tokio::test]
    async fn a_compaction_inside_a_live_turn_leaves_no_banner_behind() {
        let mut app = headless_test_app("compaction-banner-live").await;
        app.conversation_panel
            .add_info_string("older history".to_string());
        app.auto_compact.active_id = Some(1);
        app.auto_compact.mandatory_waiting = true;
        // The mandatory safe-point pass suspends a live turn and resumes it, so
        // the compaction is in effect for that turn's next request.
        app.cancel.active_id = Some(OperationId(7));
        app.input_panel
            .show_next_turn_compaction("left over from an earlier pass".to_string());

        finish_auto_compaction(&mut app, 0).await;

        assert!(
            app.input_panel.next_turn_compaction.is_none(),
            "a compaction the live turn already uses must not promise anything about the next turn"
        );
        // The context change itself is still recorded in the conversation.
        assert!(
            app.conversation_panel
                .items_snapshot()
                .iter()
                .any(|item| matches!(item, MessageItem::Compacted { .. }))
        );
    }

    #[tokio::test]
    async fn automatic_compaction_thresholds_distinguish_soft_and_hard_limits() {
        use async_openai::types::responses::{InputContent, InputMessage, InputRole, OutputStatus};

        let mut providers = std::collections::HashMap::new();
        providers.insert(
            "offline".to_string(),
            crate::config::programmer_config::ProviderConfig {
                base_url: "http://192.0.2.1:9".to_string(),
                api_key: "unused".to_string(),
                models: None,
                default_model: None,
            },
        );
        let config = crate::config::programmer_config::ProgrammerConfig {
            providers,
            default_provider: "offline".to_string(),
            auto_compact_tokens: 100_000,
            mandatory_compact_tokens: 150_000,
            auto_compact_cooldown_turns: 0,
            ..Default::default()
        };
        let mut app = crate::app::App::new(
            config,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            crate::tasks::TaskManager::default(),
            "automatic-compaction-thresholds".to_string(),
            None,
            Vec::new(),
            false,
            "test".to_string(),
        )
        .await;
        for turn in 0..6 {
            app.conversation_panel.add_input_message(
                async_openai::types::responses::MessageItem::Input(InputMessage {
                    content: vec![InputContent::InputText(format!("turn {turn}").into())],
                    role: InputRole::User,
                    status: Some(OutputStatus::Completed),
                }),
            );
            app.conversation_panel
                .add_info_string(format!("reply {turn}"));
        }
        app.cancel.active_id = Some(OperationId(9));
        app.cancel.turn_conversation_cutoff = Some(app.conversation_panel.items_snapshot().len());

        let (resume, _rx) = tokio::sync::oneshot::channel();
        handle_event(
            &mut app,
            Event::App(AppEvent::UsageSafePoint(OperationId(9), 110_000, resume)),
        )
        .await
        .unwrap();
        assert_eq!(
            app.auto_compact.active_id, None,
            "the soft threshold is not checked from this safe-point path"
        );

        // The hard threshold is checked at the same safe point and starts the
        // mandatory background compaction.
        let (resume, _rx) = tokio::sync::oneshot::channel();
        handle_event(
            &mut app,
            Event::App(AppEvent::UsageSafePoint(OperationId(9), 160_000, resume)),
        )
        .await
        .unwrap();
        assert!(
            app.auto_compact.active_id.is_some(),
            "the hard threshold must start automatic compaction"
        );
    }

    async fn cancellation_test_app() -> crate::app::App<'static> {
        let mut app = headless_test_app("cancel-queue-test").await;
        crate::app::peers::sync_session(&mut app);
        app.input_panel.set_content("original request");
        let draft = app.input_panel.draft_snapshot();
        app.input_panel.clear();
        app.cancel.active_id = Some(OperationId(7));
        app.cancel.next_id = 7;
        app.cancel.active_user_request = Some(crate::app::ActiveUserRequest {
            draft,
            conversation_cutoff: app.conversation_panel.items_snapshot().len(),
            history_text: "original request".into(),
        });
        app
    }

    async fn cancelled_terminal(app: &mut crate::app::App<'_>, id: u64) {
        handle_event(
            app,
            Event::App(AppEvent::TurnFinished(
                OperationId(id),
                Err(crate::runner::RunnerError::Cancelled),
            )),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn cancelling_keeps_last_activity_until_matching_terminal() {
        for description in [
            "tool batch: command, read_file",
            "hook diagnostics (after tools)",
            "context usage safe point / compaction",
        ] {
            let mut app = cancellation_test_app().await;
            handle_app_event(
                &mut app,
                AppEvent::RunnerActivity(OperationId(7), description.to_string()),
            )
            .await;
            handle_cancel(&mut app).await;
            let activity = app.cancel.activity.clone();
            assert_eq!(activity.as_deref(), Some(description));
            handle_app_event(
                &mut app,
                AppEvent::RunnerPhase(OperationId(7), crate::runner::RunnerPhase::Streaming),
            )
            .await;
            handle_app_event(
                &mut app,
                AppEvent::RunnerActivity(OperationId(7), "late hook".into()),
            )
            .await;
            handle_app_event(
                &mut app,
                AppEvent::RunnerActivity(OperationId(6), "stale tool".into()),
            )
            .await;
            assert_eq!(app.cancel.activity, activity);
            assert_eq!(app.conversation_panel.phase, ActivePhase::Cancelling);
            assert_eq!(
                app.resolve_status(),
                crate::ui::components::status_bar::status_bar::StatusState::Cancelling
            );
            cancelled_terminal(&mut app, 6).await;
            assert_eq!(app.cancel.activity, activity);
            assert_eq!(app.cancel.active_id, Some(OperationId(7)));
            cancelled_terminal(&mut app, 7).await;
            assert!(app.cancel.activity.is_none());
            assert!(app.cancel.active_id.is_none());
        }
    }

    #[tokio::test]
    async fn cancelling_after_committed_response_reports_finishing_not_connecting() {
        let mut app = cancellation_test_app().await;
        handle_app_event(&mut app, AppEvent::ResponseCommitted(OperationId(7))).await;
        handle_cancel(&mut app).await;
        assert_eq!(
            app.cancel.activity.as_deref(),
            Some("response committed; finishing response")
        );
        assert_eq!(app.cancel.active_id, Some(OperationId(7)));
    }

    #[tokio::test]
    async fn event_settlement_dispatches_ready_queue_without_a_completion_callback() {
        for blocker in ["draft", "approval", "shutdown"] {
            let mut app = headless_test_app("central-queue").await;
            app.conversation_panel.pending_message = Some("queued request".into());
            if blocker == "draft" {
                app.input_panel.set_content("unfinished draft");
            } else if blocker == "approval" {
                let (reply, _) = tokio::sync::oneshot::channel();
                app.pending_review = Some(crate::app::PendingReview {
                    call: serde_json::from_value(serde_json::json!({"type":"function_call", "name":"command", "arguments":"{}", "call_id":"approval"})).unwrap(),
                    reason: "approval".into(), position: (1, 1), reply: crate::ui::event::ReplyTx(reply), selected: 0,
                    operation_id: OperationId::UNTAGGED, agent_id: None, agent_generation: None,
                });
            } else {
                app.running = false;
            }
            handle_event(&mut app, Event::Redraw).await.unwrap();
            assert!(app.conversation_panel.pending_message.is_some());
            assert_eq!(app.cancel.next_id, 0);
            app.input_panel.clear();
            app.pending_review = None;
            if blocker == "shutdown" {
                continue;
            }
            handle_event(&mut app, Event::Redraw).await.unwrap();
            assert!(app.conversation_panel.pending_message.is_none());
            assert_eq!(app.cancel.next_id, 1);
            handle_event(&mut app, Event::Redraw).await.unwrap();
            assert_eq!(app.cancel.next_id, 1);
        }
    }

    #[tokio::test]
    async fn central_dispatch_respects_notification_deadline_and_stale_flushes() {
        let mut app = headless_test_app("central-notifications").await;
        app.task_notifications
            .push(crate::tasks::TaskLifecycleEvent {
                sequence: 1,
                generation: app.tasks.current_generation(),
                task_id: 1,
                origin: crate::tasks::TaskOrigin::TaskTool,
                old_status: crate::tasks::TaskStatus::Running,
                new_status: crate::tasks::TaskStatus::Completed,
                name: "finished task".into(),
                command: "true".into(),
                exit_code: Some(0),
                elapsed: Duration::ZERO,
                stdout_tail: String::new(),
                stderr_tail: String::new(),
                transcript_tail: String::new(),
                notify_agent: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            });
        app.task_notifications.ready_at = Some(Instant::now() + Duration::from_secs(60));
        handle_event(&mut app, Event::App(AppEvent::FlushTaskNotifications(0)))
            .await
            .unwrap();
        handle_event(&mut app, Event::Redraw).await.unwrap();
        assert_eq!(app.task_notifications.pending.len(), 1);
        assert_eq!(app.cancel.next_id, 0);
        app.task_notifications.ready_at = Some(Instant::now());
        handle_event(&mut app, Event::Redraw).await.unwrap();
        assert!(app.task_notifications.pending.is_empty());
        assert_eq!(app.cancel.next_id, 1);
        handle_event(&mut app, Event::Redraw).await.unwrap();
        assert_eq!(app.cancel.next_id, 1);
    }

    #[tokio::test]
    async fn cancelled_safe_point_does_not_strand_queued_input_after_compaction() {
        use async_openai::types::responses::{InputContent, InputMessage, InputRole, OutputStatus};
        let mut app = cancellation_test_app().await;
        app.config.mandatory_compact_tokens = 100_000;
        for turn in 0..2 {
            app.conversation_panel.add_input_message(
                async_openai::types::responses::MessageItem::Input(InputMessage {
                    content: vec![InputContent::InputText(format!("turn {turn}").into())],
                    role: InputRole::User,
                    status: Some(OutputStatus::Completed),
                }),
            );
            app.conversation_panel
                .add_info_string(format!("reply {turn}"));
        }
        app.auto_compact.active_id = Some(1);
        app.auto_compact.last_input_tokens = Some(200_000);
        app.auto_compact.mandatory_waiting = true;
        let (resume, receiver) = tokio::sync::oneshot::channel();
        app.auto_compact.mandatory_resume = Some(resume);
        app.conversation_panel.pending_message = Some("queued successor".into());
        handle_cancel(&mut app).await;
        drop(receiver); // The cancelled runner no longer owns the safe-point wait.
        cancelled_terminal(&mut app, 7).await;
        assert!(app.cancel.active_id.is_none());
        assert!(app.conversation_panel.pending_message.is_some());
        finish_auto_compaction(&mut app, 4).await;
        assert!(
            app.conversation_panel.pending_message.is_none(),
            "completed compaction must hand off the queued successor, not a dead runner"
        );
        assert_eq!(app.cancel.next_id, 8);
        assert!(app.auto_compact.mandatory_resume.is_none());
    }

    #[tokio::test]
    async fn compaction_before_cancelled_terminal_preserves_the_owner_until_handoff() {
        let mut app = cancellation_test_app().await;
        app.conversation_panel.pending_message = Some("queued successor".into());
        app.auto_compact.mandatory_waiting = true;
        let (resume, receiver) = tokio::sync::oneshot::channel();
        app.auto_compact.mandatory_resume = Some(resume);
        handle_cancel(&mut app).await;
        super::finish_mandatory_compaction(&mut app).await;
        receiver.await.unwrap();
        assert_eq!(app.cancel.active_id, Some(OperationId(7)));
        assert_eq!(app.cancel.next_id, 7);
        assert!(app.conversation_panel.pending_message.is_some());
        cancelled_terminal(&mut app, 7).await;
        assert_eq!(app.cancel.next_id, 8);
        assert!(app.conversation_panel.pending_message.is_none());
    }

    #[tokio::test]
    async fn cancel_queue_waits_for_matching_terminal_then_dispatches_once() {
        let mut app = cancellation_test_app().await;
        app.conversation_panel.pending_message = Some("queued successor".into());
        handle_event(&mut app, Event::App(AppEvent::Cancel))
            .await
            .unwrap();
        assert!(app.cancel.active.is_cancelled());
        assert_eq!(app.cancel.active_id, Some(OperationId(7)));
        assert!(app.input_panel.get_content().is_empty());
        handle_event(&mut app, Event::Redraw).await.unwrap();
        cancelled_terminal(&mut app, 6).await;
        assert_eq!(app.cancel.next_id, 7);
        assert_eq!(
            app.conversation_panel.pending_message.as_deref(),
            Some("queued successor")
        );

        cancelled_terminal(&mut app, 7).await;
        // No providers: the real dispatch consumes the queue and allocates a new
        // operation, then reports missing configuration instead of using a network.
        assert_eq!(app.cancel.next_id, 8);
        assert!(!app.cancel.active.is_cancelled());
        assert!(app.conversation_panel.pending_message.is_none());
        assert!(app.conversation_panel.items_snapshot().iter().any(|item| {
            matches!(item, MessageItem::Input(input)
                if serde_json::to_string(input).unwrap().contains("queued successor"))
        }));
        // Keep a real successor owner: stale terminal events cannot release it.
        let successor = app.cancel.begin(None);
        app.conversation_panel.pending_message = Some("later".into());
        cancelled_terminal(&mut app, 7).await;
        assert_eq!(app.cancel.active_id, Some(successor));
        assert_eq!(
            app.conversation_panel.pending_message.as_deref(),
            Some("later")
        );
    }

    #[tokio::test]
    async fn cancel_queue_dispatches_notifications_once_after_terminal() {
        let mut app = cancellation_test_app().await;
        app.cancel.response_started = true;
        app.task_notifications
            .push(crate::tasks::TaskLifecycleEvent {
                sequence: 1,
                generation: app.tasks.current_generation(),
                task_id: 1,
                origin: crate::tasks::TaskOrigin::TaskTool,
                old_status: crate::tasks::TaskStatus::Running,
                new_status: crate::tasks::TaskStatus::Completed,
                name: "completed task".into(),
                command: "true".into(),
                exit_code: Some(0),
                elapsed: Duration::ZERO,
                stdout_tail: String::new(),
                stderr_tail: String::new(),
                transcript_tail: String::new(),
                notify_agent: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            });
        handle_cancel(&mut app).await;
        let token = app.task_notifications.flush_token;
        super::flush_task_notifications(&mut app, token).await;
        assert_eq!(app.cancel.next_id, 7);
        assert_eq!(app.task_notifications.pending.len(), 1);
        cancelled_terminal(&mut app, 7).await;
        assert_eq!(app.cancel.next_id, 8);
        assert!(app.task_notifications.pending.is_empty());
        super::flush_task_notifications(&mut app, token).await;
        handle_event(&mut app, Event::Redraw).await.unwrap();
        assert_eq!(app.cancel.next_id, 8);
    }

    #[tokio::test]
    async fn cancel_queue_preserves_unsent_draft_and_waits_for_it() {
        let mut app = cancellation_test_app().await;
        app.conversation_panel.pending_message = Some("queued successor".into());
        app.input_panel.set_content("actual unsent draft");
        handle_cancel(&mut app).await;
        cancelled_terminal(&mut app, 7).await;
        assert_eq!(app.input_panel.get_content(), "actual unsent draft");
        assert_eq!(app.cancel.next_id, 7);
        assert!(app.conversation_panel.pending_message.is_some());
        app.input_panel.clear();
        handle_event(&mut app, Event::Redraw).await.unwrap();
        assert_eq!(app.cancel.next_id, 8);
    }

    #[tokio::test]
    async fn cancel_without_queue_restores_draft_without_restarting() {
        for draft in ["", "actual unsent draft"] {
            let mut app = cancellation_test_app().await;
            app.input_panel.set_content(draft);
            handle_cancel(&mut app).await;
            cancelled_terminal(&mut app, 7).await;
            handle_event(&mut app, Event::Redraw).await.unwrap();
            assert_eq!(app.cancel.next_id, 7);
            assert!(app.cancel.active_id.is_none());
            assert_eq!(
                app.input_panel.get_content(),
                if draft.is_empty() {
                    "original request"
                } else {
                    draft
                }
            );
        }
        let mut app = cancellation_test_app().await;
        app.cancel.response_started = true;
        handle_cancel(&mut app).await;
        cancelled_terminal(&mut app, 7).await;
        assert!(app.input_panel.get_content().is_empty());
        assert_eq!(app.cancel.next_id, 7);
    }

    #[tokio::test]
    async fn cancel_queue_does_not_bypass_independent_approval_or_peer_consent() {
        for peer_consent in [false, true] {
            let mut app = cancellation_test_app().await;
            app.conversation_panel.pending_message = Some("queued successor".into());
            if peer_consent {
                let (_, rx) = tokio::sync::oneshot::channel();
                app.peers.consent = Some((
                    crate::peers::PeerEnvelope::new(
                        uuid::Uuid::new_v4().to_string(),
                        uuid::Uuid::new_v4().to_string(),
                        crate::peers::PeerKind::Delegation,
                        "delegated task".into(),
                        None,
                    )
                    .unwrap(),
                    rx,
                ));
            } else {
                let (reply, _) = tokio::sync::oneshot::channel();
                app.pending_review = Some(crate::app::PendingReview {
                    call: serde_json::from_value(serde_json::json!({
                        "type": "function_call", "name": "command", "arguments": "{}", "call_id": "review"
                    })).unwrap(),
                    reason: "independent child approval".into(), position: (1, 1),
                    reply: crate::ui::event::ReplyTx(reply),
                    selected: 0, operation_id: OperationId::UNTAGGED, agent_id: None, agent_generation: None,
                });
            }
            handle_cancel(&mut app).await;
            cancelled_terminal(&mut app, 7).await;
            assert_eq!(app.cancel.next_id, 7);
            assert!(app.conversation_panel.pending_message.is_some());
            app.pending_review = None;
            app.peers.consent = None;
            handle_event(&mut app, Event::Redraw).await.unwrap();
            assert_eq!(app.cancel.next_id, 8);
        }
    }
    #[tokio::test]
    async fn cancelling_before_model_output_restores_the_original_draft() {
        use async_openai::types::responses::{
            ImageDetail, InputContent, InputImageContent, InputMessage, InputRole, OutputStatus,
        };

        let mut config = crate::config::programmer_config::ProgrammerConfig::default();
        config.providers.clear();
        let mut app = crate::app::App::new(
            config,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            crate::tasks::TaskManager::default(),
            "cancel-draft-test".to_string(),
            None,
            Vec::new(),
            false,
            "test".to_string(),
        )
        .await;
        app.input_panel.add_paste("first\nsecond".to_string());
        assert!(app.input_panel.add_image(
            InputImageContent {
                detail: ImageDetail::Auto,
                file_id: None,
                image_url: Some("data:image/png;base64,AAAA".to_string()),
            },
            4,
            4,
        ));
        let draft = app.input_panel.draft_snapshot();
        let history_text = app.input_panel.expanded_content();
        app.input_panel.push_history(history_text.clone());
        app.input_panel.clear();
        let cutoff = app.conversation_panel.items_snapshot().len();
        app.conversation_panel.add_input_message(
            async_openai::types::responses::MessageItem::Input(InputMessage {
                content: vec![InputContent::InputText("sent".into())],
                role: InputRole::User,
                status: Some(OutputStatus::Completed),
            }),
        );
        app.cancel.active_id = Some(OperationId(1));
        app.cancel.response_started = false;
        app.cancel.active_user_request = Some(crate::app::ActiveUserRequest {
            draft,
            conversation_cutoff: cutoff,
            history_text,
        });

        handle_cancel(&mut app).await;

        assert!(app.input_panel.get_content().contains("[Pasted text #1"));
        assert_eq!(app.input_panel.take_images().len(), 1);
        assert!(app.input_panel.history.is_empty());
        assert!(
            !app.conversation_panel
                .items_snapshot()
                .iter()
                .any(|item| { matches!(item, MessageItem::Input(_)) })
        );
    }
}
