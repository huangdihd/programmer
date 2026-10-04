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

//! Message sending and slash-command dispatch.

use super::App;
use super::surface::TuiSurface;
use super::{command_handlers, diagnostics, session};
use crate::cancel::OperationId;
use crate::classifier::{PlanPhase, WorkMode};
use crate::commands::Command;
use crate::ui::event::{AppEvent, Event};
use async_openai::types::responses::MessageItem as ApiMessageItem;
use async_openai::types::responses::{
    InputContent, InputImageContent, InputMessage, InputRole, InputTextContent, OutputStatus,
};
use regex::Regex;
use std::sync::LazyLock;

use crate::prompts::PLAN_PLANNING_PROMPT;

static PASTED_IMAGE_PLACEHOLDER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\[Pasted image #\d+ \d+x\d+\]").expect("valid pasted-image placeholder regex")
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum KeepRetryMode {
    Exponential,
    Fixed(std::time::Duration),
}

impl KeepRetryMode {
    fn delay(self, attempt: u32) -> std::time::Duration {
        match self {
            Self::Exponential => crate::runner::stream::backoff_delay(attempt),
            Self::Fixed(delay) => delay,
        }
    }

    fn label(self) -> String {
        match self {
            Self::Exponential => {
                "exponential backoff (up to 30s between attempts; retries never stop)".to_string()
            }
            Self::Fixed(delay) if delay.subsec_millis() == 0 => {
                format!("a fixed {}s delay", delay.as_secs())
            }
            Self::Fixed(delay) => format!("a fixed {}ms delay", delay.as_millis()),
        }
    }
}

/// Build an optional plan-mode system prompt snippet.
fn plan_system_prompt(app: &App<'_>) -> Option<&'static str> {
    if app.agent_loop.session.work_mode != WorkMode::Plan {
        return None;
    }
    match app.agent_loop.plan_phase {
        PlanPhase::Planning => Some(PLAN_PLANNING_PROMPT),
        PlanPhase::Reviewing => None,
    }
}

// ---------------------------------------------------------------------------
// Message sending
// ---------------------------------------------------------------------------

/// Collect input, push to history, and start a user request.
pub(crate) async fn send_message(app: &mut App<'_>) {
    if !app.require_running_session() {
        return;
    }
    let typed = app.ui.input_panel.expanded_content();
    if typed.is_empty() {
        return;
    }
    let draft = app.ui.input_panel.draft_snapshot();
    let history_text = typed.clone();
    let pasted_images = app.ui.input_panel.take_images();
    // History keeps the compact `@path` form; the model receives a path-only
    // reference for regular files or a stored image for image paths. The
    // conversation filters stored images out of API requests while vision is
    // off, without deleting them from session history.
    app.ui.input_panel.push_history(typed.clone());
    app.ui.input_panel.clear();

    // Expand before deciding whether to start or queue the request. Queued
    // messages must retain the same path annotations and image attachments as
    // messages that start immediately.
    let diagnostics = app
        .agent_loop
        .session
        .diagnostics_state
        .lock()
        .unwrap()
        .baseline
        .clone();
    let expanded = crate::commands::expand_references(&typed, diagnostics.as_deref()).await;
    for notice in expanded.notices {
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_warning_string(notice);
        app.ui.conversation_panel.scroll_to_bottom();
    }
    let mut images = pasted_images;
    images.extend(expanded.images);
    let omitted = crate::commands::limit_image_attachments(&mut images);
    if omitted > 0 {
        app.agent_loop.session.conversation.lock().unwrap().add_warning_string(format!(
        "omitted {omitted} image(s): attachment count or total size exceeds the per-message limit"
        ));
        app.ui.conversation_panel.scroll_to_bottom();
    }
    start_request_as_with_images(
        app,
        expanded.text,
        InputRole::User,
        images,
        Some((draft, history_text)),
    )
    .await;
}

pub(crate) async fn start_request_with_images(
    app: &mut App<'_>,
    text: String,
    images: Vec<async_openai::types::responses::InputImageContent>,
) {
    start_request_as_with_images(app, text, InputRole::User, images, None).await;
}

/// Start a turn from a message with the given role. `User` is a normal user
/// message; `Developer` carries a hidden instruction (like `/init`).
pub(crate) async fn start_request_as(app: &mut App<'_>, text: String, role: InputRole) {
    start_request_as_with_images(app, text, role, Vec::new(), None).await;
}

async fn start_request_as_with_images(
    app: &mut App<'_>,
    text: String,
    role: InputRole,
    images: Vec<async_openai::types::responses::InputImageContent>,
    original_draft: Option<(
        crate::ui::components::input_panel::input_panel::InputDraft,
        String,
    )>,
) {
    if !app.require_running_session() {
        return;
    }
    // active_id is the lifecycle authority. UI phases are presentation state
    // and can briefly lag the runner; they must never decide whether two turns
    // are allowed to overlap.
    if app.agent_loop.cancel.active_id.is_some() {
        queue_pending_request(
            &mut app.agent_loop.pending_request,
            super::scheduling::UserRequest { text, images },
        );
        return;
    }

    // An explicit submission may retry a previously failed mandatory pass.
    app.agent_loop.auto_compact.retry_blocked = false;
    if app.mandatory_compact_tokens().is_some_and(|limit| {
        app.agent_loop
            .auto_compact
            .last_input_tokens
            .is_some_and(|tokens| tokens >= limit)
    }) {
        queue_pending_request(
            &mut app.agent_loop.pending_request,
            super::scheduling::UserRequest { text, images },
        );
        app.agent_loop.auto_compact.mandatory_waiting = true;
        // A new request may retry a failed prefix. Keep the background dedup
        // marker otherwise, so idle polling cannot spin on a provider failure.
        if app.agent_loop.auto_compact.active_id.is_none() {
            app.agent_loop.auto_compact.last_cutoff = None;
        }
        let input_tokens = app
            .agent_loop
            .auto_compact
            .last_input_tokens
            .unwrap_or_default();
        if !maybe_start_auto_compact(app, input_tokens) {
            app.agent_loop.auto_compact.mandatory_waiting = false;
            app.agent_loop.auto_compact.retry_blocked = true;
            app.agent_loop
                .session
                .conversation
                .lock()
                .unwrap()
                .add_error_string(
                    "mandatory context compaction could not start; request remains queued"
                        .to_string(),
                );
            app.ui.conversation_panel.scroll_to_bottom();
        }
        return;
    }

    start_ready_request(app, vec![(text, role, images)], original_draft).await;
}

/// Start one turn containing any completed task/sub-agent updates and an
/// optional queued user follow-up. Runtime updates use the developer role so
/// they cannot impersonate the user.
pub(crate) async fn start_runtime_update_request(
    app: &mut App<'_>,
    task_events: Vec<crate::tasks::TaskLifecycleEvent>,
    agent_events: Vec<crate::agents::AgentSnapshot>,
    pending_user: Option<super::scheduling::UserRequest>,
) {
    if task_events.is_empty() && agent_events.is_empty() {
        if let Some(request) = pending_user {
            start_request_with_images(app, request.text, request.images).await;
        }
        return;
    }

    let mut update = String::new();
    if !task_events.is_empty() {
        update.push_str(&format_task_updates(&task_events));
    }
    if !agent_events.is_empty() {
        if !update.is_empty() {
            update.push_str("\n\n");
        }
        update.push_str(&format_agent_updates(&agent_events));
    }
    let mut inputs = vec![(update, InputRole::Developer, Vec::new())];
    if let Some(request) = pending_user {
        inputs.push((request.text, InputRole::User, request.images));
    }
    start_ready_request(app, inputs, None).await;
}

fn format_agent_updates(agents: &[crate::agents::AgentSnapshot]) -> String {
    use std::fmt::Write;

    const MAX_RESULT_CHARS: usize = 12_000;
    let mut text = String::from(
        "<sub_agent_updates>\n\
         These results were generated by delegated in-process sub-agents.\n",
    );
    for agent in agents {
        let _ = writeln!(
            text,
            "\nSub-agent {id} ({name}) {status} after {secs}s.\n\
             Delegated task: {prompt}",
            id = agent.id,
            name = agent.name,
            status = agent.status.label(),
            secs = agent.elapsed.as_secs(),
            prompt = agent.prompt,
        );
        if let Some(result) = &agent.result {
            let was_truncated = result.chars().count() > MAX_RESULT_CHARS;
            let mut rendered: String = result.chars().take(MAX_RESULT_CHARS).collect();
            if was_truncated {
                rendered.push_str("\n[... sub-agent result truncated ...]");
            }
            let _ = writeln!(text, "Result:\n{rendered}");
        }
    }
    text.push_str("</sub_agent_updates>");
    text
}

pub(in crate::app) fn start_keep_retry(app: &mut App<'_>, mode: KeepRetryMode) {
    if !app.require_running_session() {
        return;
    }
    use std::sync::atomic::Ordering;

    if app.agent_loop.cancel.active_id.is_some() {
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_warning_string("cannot retry while another request is active");
        app.ui.conversation_panel.scroll_to_bottom();
        return;
    }
    if !app
        .agent_loop
        .session
        .conversation
        .lock()
        .unwrap()
        .items
        .clone()
        .iter()
        .any(|item| matches!(item, crate::response::message_item::MessageItem::Input(_)))
    {
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_warning_string("nothing to retry yet — send a message first");
        app.ui.conversation_panel.scroll_to_bottom();
        return;
    }
    let Some(mut runner) = app.build_runner() else {
        {
            app.agent_loop
                .session
                .conversation
                .lock()
                .unwrap()
                .add_error_string(format!(
                    "unknown provider/model: {}",
                    app.agent_loop.session.current_model
                ));
            app.ui.conversation_panel.scroll_to_bottom();
        };
        return;
    };
    // `/keepretry` owns the retry loop so its selected delay is applied after
    // every failed model attempt rather than after the runner's normal batch.
    runner.stream_retry_limit = 0;
    let runner = std::sync::Arc::new(runner);
    app.agent_loop.runner = Some(runner.clone());

    app.ui.input_panel.clear_suggestion();
    app.agent_loop
        .session
        .conversation
        .lock()
        .unwrap()
        .reset_accumulated_usage();
    app.agent_loop
        .session
        .conversation
        .lock()
        .unwrap()
        .add_info_string(format!(
            "Retrying the previous model request with {} until it succeeds. Press Esc to stop.",
            mode.label()
        ));
    app.ui.conversation_panel.scroll_to_bottom();
    let operation_id = app.agent_loop.cancel.begin(Some(
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .items
            .len(),
    ));

    let surface = TuiSurface {
        tx: app.events.sender.clone(),
        skill_prompt: app.agent_loop.skill_registry.catalog_prompt(),
        plan_prompt: plan_system_prompt(app),
        approval_label: format!(
            "{} approved by {} mode",
            app.agent_loop.session.work_mode.icon(),
            app.agent_loop.session.work_mode.label()
        ),
        operation_id,
        cancel: app.agent_loop.cancel.active.clone(),
    };
    let shared = app.agent_loop.session.conversation.clone();
    let cancel = app.agent_loop.cancel.active.clone();
    let retrying = app.agent_loop.cancel.stream_retrying.clone();
    let tx = app.events.sender.clone();
    app.agent_loop.runner_handles.push(tokio::spawn(async move {
        let mut attempt = 1u32;
        let result = loop {
            match runner.run_turn(&shared, &cancel, &surface).await {
                Ok(result) => break Ok(result),
                Err(crate::runner::RunnerError::Cancelled) => {
                    break Err(crate::runner::RunnerError::Cancelled);
                }
                Err(_) => {
                    let _ = tx.send(Event::App(AppEvent::KeepRetryAttempt(operation_id)));
                    retrying.store(true, Ordering::Relaxed);
                    if cancel
                        .wait_or(tokio::time::sleep(mode.delay(attempt)))
                        .await
                        .is_none()
                    {
                        break Err(crate::runner::RunnerError::Cancelled);
                    }
                    retrying.store(false, Ordering::Relaxed);
                    attempt = attempt.saturating_add(1);
                }
            }
        };
        retrying.store(false, Ordering::Relaxed);
        let _ = tx.send(Event::App(AppEvent::TurnFinished(operation_id, result)));
    }));
}

async fn start_ready_request(
    app: &mut App<'_>,
    inputs: Vec<(String, InputRole, Vec<InputImageContent>)>,
    original_draft: Option<(
        crate::ui::components::input_panel::input_panel::InputDraft,
        String,
    )>,
) {
    if !app.session_accepts_work() {
        return;
    }
    debug_assert!(app.agent_loop.cancel.active_id.is_none());
    app.ui.active_suggestion_operation_id = None;
    if let Some(cancel) = app.ui.input_suggestion_cancel.take() {
        cancel.cancel();
    }
    app.ui.input_panel.clear_suggestion();
    let conversation_cutoff = app
        .agent_loop
        .session
        .conversation
        .lock()
        .unwrap()
        .items
        .len();
    let title_source = (app.agent_loop.session.title.is_empty()
        && !app.agent_loop.session.title_generation_started)
        .then(|| inputs.first().map(|(text, _, _)| text.clone()))
        .flatten();
    let user_prompt = inputs
        .iter()
        .find(|(_, role, _)| matches!(role, InputRole::User))
        .map(|(text, _, _)| text.clone());
    app.agent_loop.current_checkpoint_id = None;
    if let (Some(prompt), Some(store)) = (
        user_prompt,
        app.agent_loop.session.checkpoint_store.as_ref(),
    ) {
        let cutoff = app
            .agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .items
            .len();
        match store
            .lock()
            .unwrap()
            .begin(prompt, cutoff, app.ui.todo_list.todos.clone())
        {
            Ok(id) => app.agent_loop.current_checkpoint_id = Some(id),
            Err(error) => {
                app.agent_loop
                    .session
                    .conversation
                    .lock()
                    .unwrap()
                    .add_warning_string(format!("could not create rewind checkpoint: {error}"));
                app.ui.conversation_panel.scroll_to_bottom();
            }
        }
    }
    for (text, role, images) in inputs {
        let content = ordered_message_content(text, images);
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_input_message(ApiMessageItem::Input(InputMessage {
                content,
                role,
                status: Some(OutputStatus::Completed),
            }));
        app.ui.conversation_panel.scroll_to_bottom();
    }
    if let Some(source) = title_source {
        maybe_start_session_title(app, source);
    }
    app.agent_loop
        .session
        .conversation
        .lock()
        .unwrap()
        .reset_accumulated_usage();
    diagnostics::maybe_seed_diagnostics_baseline(app);
    session::save_session(app);
    // Fresh turn: start from an un-cancelled root token so a prior turn's Esc
    // doesn't carry over to this one. Bump the operation id synchronously
    // before spawning so the UI can tag all turn events and filter stale ones.
    let operation_id = app.agent_loop.cancel.begin(Some(conversation_cutoff));
    app.agent_loop.cancel.active_user_request =
        original_draft.map(|(draft, history_text)| super::ActiveUserRequest {
            draft,
            conversation_cutoff,
            history_text,
        });

    let Some(runner) = app.build_runner() else {
        app.agent_loop.cancel.clear();
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_error_string(format!(
                "unknown provider/model: {}",
                app.agent_loop.session.current_model
            ));
        app.ui.conversation_panel.scroll_to_bottom();
        return;
    };
    let runner = std::sync::Arc::new(runner);
    app.agent_loop.runner = Some(runner.clone());
    if let Some(details) = app.ui.input_panel.take_next_turn_compaction() {
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .insert_info_string(
                conversation_cutoff,
                format!(
                    "Automatic context compaction — {details} summarized. The model turn below is \
             the first to use the compacted context; older history remains visible above."
                ),
            );
        app.ui.conversation_panel.scroll_to_bottom();
    }
    let surface = TuiSurface {
        tx: app.events.sender.clone(),
        skill_prompt: app.agent_loop.skill_registry.catalog_prompt(),
        plan_prompt: plan_system_prompt(app),
        approval_label: format!(
            "{} approved by {} mode",
            app.agent_loop.session.work_mode.icon(),
            app.agent_loop.session.work_mode.label()
        ),
        operation_id,
        cancel: app.agent_loop.cancel.active.clone(),
    };
    let shared = app.agent_loop.session.conversation.clone();
    let cancel = app.agent_loop.cancel.active.clone();
    let tx = app.events.sender.clone();
    app.agent_loop.runner_handles.push(tokio::spawn(async move {
        let result = runner.run_turn(&shared, &cancel, &surface).await;
        let _ = tx.send(Event::App(AppEvent::TurnFinished(operation_id, result)));
    }));
}

fn format_task_updates(events: &[crate::tasks::TaskLifecycleEvent]) -> String {
    use crate::tasks::TaskOrigin;
    use std::fmt::Write;

    const MAX_EVENT_OUTPUT: usize = 6_000;
    let mut text = String::from(
        "<background_task_updates>\n\
         These runtime events were generated by the background task system.\n",
    );
    for event in events {
        let origin = match event.origin {
            TaskOrigin::TaskTool => "task tool",
            TaskOrigin::Command => "command",
            TaskOrigin::PromotedCommand => "promoted command",
            TaskOrigin::BangCommand => "user interactive command",
            TaskOrigin::Restored => "restored task",
        };
        let _ = writeln!(
            text,
            "\nTask {id} changed from {old} to {new}.\n\
             Origin: {origin}\nName: {name}\nCommand: {command}\n\
             Exit code: {exit}\nElapsed: {elapsed:.1}s",
            id = event.task_id,
            old = event.old_status.label(),
            new = event.new_status.label(),
            name = event.name,
            command = event.command,
            exit = event
                .exit_code
                .map_or_else(|| "unknown".to_string(), |code| code.to_string()),
            elapsed = event.elapsed.as_secs_f64(),
        );
        if event.origin == TaskOrigin::BangCommand && !event.transcript_tail.is_empty() {
            let _ = writeln!(
                text,
                "Terminal transcript tail:\n{}",
                tail_chars(&event.transcript_tail, MAX_EVENT_OUTPUT)
            );
        } else {
            if !event.stdout_tail.is_empty() {
                let _ = writeln!(
                    text,
                    "Stdout tail:\n{}",
                    tail_chars(&event.stdout_tail, MAX_EVENT_OUTPUT / 2)
                );
            }
            if !event.stderr_tail.is_empty() {
                let _ = writeln!(
                    text,
                    "Stderr tail:\n{}",
                    tail_chars(&event.stderr_tail, MAX_EVENT_OUTPUT / 2)
                );
            }
        }
    }
    text.push_str(
        "</background_task_updates>\n\n\
         Briefly report the meaningful result to the user. If it unblocks an \
         unfinished workflow, continue with the next safe step. Do not merely \
         repeat the metadata, and do not poll tasks that are already finished.",
    );
    text
}

fn tail_chars(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_string();
    }
    format!(
        "[earlier output omitted]\n{}",
        text.chars().skip(count - max_chars).collect::<String>()
    )
}

/// Append a request to the single follow-up queue while preserving attachments.
///
/// Multiple messages are deliberately coalesced with newlines because the UI
/// exposes one pending-message slot.
pub(super) fn queue_pending_request(
    pending_request: &mut Option<super::scheduling::UserRequest>,
    mut request: super::scheduling::UserRequest,
) {
    let Some(pending) = pending_request.as_mut() else {
        *pending_request = Some(request);
        return;
    };
    pending.text.push('\n');
    pending.text.push_str(&request.text);
    pending.images.append(&mut request.images);
}

fn ordered_message_content(text: String, images: Vec<InputImageContent>) -> Vec<InputContent> {
    let mut content = Vec::new();
    let mut images = images.into_iter();
    let mut text_start = 0;

    for placeholder in PASTED_IMAGE_PLACEHOLDER.find_iter(&text) {
        push_input_text(&mut content, &text[text_start..placeholder.start()]);
        if let Some(image) = images.next() {
            content.push(InputContent::InputImage(image));
        }
        text_start = placeholder.end();
    }

    push_input_text(&mut content, &text[text_start..]);
    content.extend(images.map(InputContent::InputImage));
    if content.is_empty() {
        content.push(InputContent::InputText(InputTextContent {
            text: String::new(),
        }));
    }
    content
}

fn push_input_text(content: &mut Vec<InputContent>, text: &str) {
    if !text.is_empty() {
        content.push(InputContent::InputText(InputTextContent {
            text: text.to_string(),
        }));
    }
}

/// Run a `!command` from the input: spawn it as an interactive PTY task and
/// open the terminal panel focused on it, so the user drives it right away.
/// The exit is watched from [`super::events`]'s tick: the panel closes, focus
/// returns to the input, and the transcript goes to the agent for a response.
pub(crate) fn run_bang_command(app: &mut App<'_>, input: &str) {
    if !app.require_running_session() {
        return;
    }
    use crate::ui::components::terminal_panel::TerminalPane;

    let command = input.strip_prefix('!').unwrap_or(input).trim().to_string();
    if command.is_empty() {
        app.ui.input_panel.clear();
        return;
    }
    app.ui.input_panel.push_history(input.to_string());
    app.ui.input_panel.clear();
    app.ui.input_panel.completion = None;

    // Spawn at the size the terminal panel will render at, so the first frame
    // doesn't have to resize the fresh PTY. A resize racing the child's
    // startup leaves a SIGWINCH pending from the fork/exec window, which the
    // kernel then delivers at the worst moment (e.g. inside Python 3.14's
    // REPL `tcsetattr`, which dies on EINTR).
    let (rows, cols) = crossterm::terminal::size()
        .map(|(w, h)| (h.saturating_sub(2).max(1), w.max(1)))
        .unwrap_or((24, 80));
    let security = app.security.snapshot();
    match app.agent_loop.session.tasks.spawn_bang_secure(
        &command,
        None,
        Some(&command),
        rows,
        cols,
        &security,
    ) {
        Ok(id) => {
            // The record in the conversation; the transcript follows when the
            // command exits and the agent picks it up.
            app.agent_loop
                .session
                .conversation
                .lock()
                .unwrap()
                .add_info_string(format!(
                    "🖥 !{command} — running in the interactive terminal; \
             the agent will respond when it exits"
                ));
            app.ui.conversation_panel.scroll_to_bottom();
            session::mark_dirty(app);
            let mut pane = TerminalPane::new(app.agent_loop.session.tasks.clone(), id, command);
            // Grab input immediately — the user typed `!` to interact.
            pane.grabbed = true;
            app.ui.terminal_pane = Some(pane);
        }
        Err(e) => {
            app.agent_loop
                .session
                .conversation
                .lock()
                .unwrap()
                .add_error_string(e);
            app.ui.conversation_panel.scroll_to_bottom();
        }
    }
}

fn maybe_start_session_title(app: &mut App<'_>, first_message: String) -> bool {
    use crate::ui::event::{AppEvent, Event};

    app.agent_loop.session.title_generation_started = true;
    let target_model = app.effective_title_model();
    let Some((client, model_name)) = app.provider_manager.resolve(&target_model) else {
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_warning_string(format!(
                "session title generation skipped: unknown provider/model {target_model}"
            ));
        app.ui.conversation_panel.scroll_to_bottom();
        return false;
    };
    let client = client.clone();
    app.agent_loop.session.title_generation_id =
        app.agent_loop.session.title_generation_id.wrapping_add(1);
    let generation_id = app.agent_loop.session.title_generation_id;
    let session_uuid = app.agent_loop.session.uuid.clone();
    let prompt = format!("{}{first_message}", crate::prompts::SESSION_TITLE_PROMPT);
    let request = async_openai::types::responses::CreateResponse {
        input: async_openai::types::responses::InputParam::Text(prompt),
        model: Some(model_name),
        max_output_tokens: Some(80),
        ..Default::default()
    };
    let sender = app.events.sender.clone();
    app.agent_loop
        .background_handles
        .push(tokio::spawn(async move {
            let result =
                stream_compact_response(&client, request, crate::cancel::CancellationToken::new())
                    .await
                    .and_then(|response| normalize_session_title(&response.summary));
            let _ = sender.send(Event::App(AppEvent::SessionTitleGenerated {
                session_uuid,
                generation_id,
                result,
            }));
        }));
    true
}

pub(in crate::app) fn set_or_regenerate_session_title(app: &mut App<'_>, requested: &str) {
    if !requested.trim().is_empty() {
        let Ok(title) = normalize_session_title(requested) else {
            app.agent_loop
                .session
                .conversation
                .lock()
                .unwrap()
                .add_warning_string("session title cannot be empty");
            app.ui.conversation_panel.scroll_to_bottom();
            return;
        };
        app.agent_loop.session.title_generation_id =
            app.agent_loop.session.title_generation_id.wrapping_add(1);
        app.agent_loop.session.title_generation_started = true;
        app.agent_loop.session.title = title.clone();
        session::mark_dirty(app);
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_info_string(format!("Session title set to: {title}"));
        app.ui.conversation_panel.scroll_to_bottom();
        return;
    }

    let items = app
        .agent_loop
        .session
        .conversation
        .lock()
        .unwrap()
        .items
        .clone();
    let Some(first_message) = super::helpers::first_user_text(&items) else {
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_warning_string("cannot generate a title before the first user message");
        app.ui.conversation_panel.scroll_to_bottom();
        return;
    };
    if maybe_start_session_title(app, first_message) {
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_info_string("Regenerating session title…");
        app.ui.conversation_panel.scroll_to_bottom();
    }
}

fn normalize_session_title(response: &str) -> Result<String, String> {
    let title = response
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default()
        .trim()
        .trim_matches(['"', '\'', '“', '”'])
        .trim();
    if title.is_empty() {
        Err("the model returned an empty title".to_string())
    } else {
        Ok(title.chars().take(80).collect())
    }
}

pub(super) fn maybe_start_input_suggestion(app: &mut App<'_>, operation_id: OperationId) {
    use async_openai::types::responses::{InputItem, InputParam, Item};

    if !app.session_accepts_work()
        || !app.ui.input_panel.get_content().is_empty()
        || app.agent_loop.cancel.active_id.is_some()
    {
        return;
    }
    let target_model = app.effective_suggestion_model();
    let Some((client, model_name)) = app.provider_manager.resolve(&target_model) else {
        return;
    };
    let mut input = match app
        .agent_loop
        .session
        .conversation
        .lock()
        .unwrap()
        .to_input_param_with_vision(&target_model, None, None, None, false)
    {
        InputParam::Items(items) => items,
        InputParam::Text(text) => vec![InputItem::from(Item::Message(ApiMessageItem::Input(
            InputMessage {
                content: vec![InputContent::InputText(InputTextContent { text })],
                role: InputRole::User,
                status: Some(OutputStatus::Completed),
            },
        )))],
    };
    input.push(InputItem::from(Item::Message(ApiMessageItem::Input(
        InputMessage {
            content: vec![InputContent::InputText(InputTextContent {
                text: crate::prompts::INPUT_SUGGESTION_PROMPT.to_string(),
            })],
            role: InputRole::User,
            status: Some(OutputStatus::Completed),
        },
    ))));

    app.ui.active_suggestion_operation_id = Some(operation_id);
    let cancel = crate::cancel::CancellationToken::new();
    app.ui.input_suggestion_cancel = Some(cancel.clone());
    let request = async_openai::types::responses::CreateResponse {
        input: InputParam::Items(input),
        model: Some(model_name),
        max_output_tokens: Some(120),
        ..Default::default()
    };
    let client = client.clone();
    let session_uuid = app.agent_loop.session.uuid.clone();
    let sender = app.events.sender.clone();
    app.agent_loop
        .background_handles
        .push(tokio::spawn(async move {
            let result = stream_compact_response(&client, request, cancel)
                .await
                .and_then(|response| normalize_input_suggestion(&response.summary));
            let _ = sender.send(Event::App(AppEvent::InputSuggestionGenerated {
                session_uuid,
                operation_id,
                result,
            }));
        }));
}

fn normalize_input_suggestion(response: &str) -> Result<String, String> {
    let suggestion = response
        .trim()
        .trim_matches(['"', '\'', '“', '”'])
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if suggestion.is_empty() {
        Err("the model returned an empty input suggestion".to_string())
    } else {
        Ok(suggestion.chars().take(240).collect())
    }
}

/// Build the no-tools request shared by foreground and background compaction.
fn build_compact_request(
    input_items: Vec<async_openai::types::responses::InputItem>,
    model_name: String,
    thinking_level: crate::thinking::ThinkingLevel,
) -> async_openai::types::responses::CreateResponse {
    async_openai::types::responses::CreateResponse {
        input: async_openai::types::responses::InputParam::Items(input_items),
        model: Some(model_name),
        reasoning: thinking_level.reasoning(),
        ..Default::default()
    }
}

fn compact_input_items(
    input: async_openai::types::responses::InputParam,
) -> Vec<async_openai::types::responses::InputItem> {
    use async_openai::types::responses::{InputItem, InputParam, Item};
    let mut items = match input {
        InputParam::Items(items) => items,
        InputParam::Text(text) => vec![InputItem::from(Item::Message(ApiMessageItem::Input(
            InputMessage {
                content: vec![InputContent::InputText(InputTextContent { text })],
                role: InputRole::User,
                status: Some(OutputStatus::Completed),
            },
        )))],
    };
    items.push(InputItem::from(Item::Message(ApiMessageItem::Input(
        InputMessage {
            content: vec![InputContent::InputText(InputTextContent {
                text: crate::prompts::COMPACT_PROMPT.to_string(),
            })],
            role: InputRole::User,
            status: Some(OutputStatus::Completed),
        },
    ))));
    items
}

fn compact_response_text(
    response: async_openai::types::responses::Response,
) -> Result<String, String> {
    use async_openai::types::responses::{OutputItem, OutputMessageContent};
    let text = response
        .output
        .iter()
        .filter_map(|item| match item {
            OutputItem::Message(message) => {
                Some(message.content.iter().filter_map(|content| match content {
                    OutputMessageContent::OutputText(text) => Some(text.text.as_str()),
                    _ => None,
                }))
            }
            _ => None,
        })
        .flatten()
        .collect::<Vec<_>>()
        .join("\n");
    if text.trim().is_empty() {
        Err("the model returned an empty summary".to_string())
    } else {
        Ok(text)
    }
}

/// Stream a compaction response so long summaries keep the provider connection
/// active, then finalize the accumulated events through the same response
/// machinery as a normal turn.
async fn stream_compact_response(
    client: &async_openai::Client<async_openai::config::OpenAIConfig>,
    request: async_openai::types::responses::CreateResponse,
    cancel: crate::cancel::CancellationToken,
) -> Result<crate::ui::event::CompactionResult, String> {
    let mut partial = crate::response::partial_response::PartialResponse::new(cancel.clone());
    let mut stream_error = None;
    let retrying = std::sync::atomic::AtomicBool::new(false);

    crate::runner::stream::stream_with_retries(
        client,
        &request,
        &cancel,
        &retrying,
        crate::consts::MAX_STREAM_RETRIES,
        |event| match event {
            Ok(event) => partial.handle_response_stream_event(event),
            Err(error) => stream_error = Some(error),
        },
    )
    .await;

    if cancel.is_cancelled() {
        return Err("cancelled".to_string());
    }
    if let Some(error) = stream_error {
        return Err(error_chain(&error));
    }

    let usage = partial.usage;
    let summary = partial
        .finalize()
        .map_err(|error| error_chain(&error))
        .and_then(compact_response_text)?;
    Ok(crate::ui::event::CompactionResult {
        summary,
        input_tokens: usage.map(|(input, _, _)| input),
        output_tokens: usage.map(|(_, output, _)| output),
    })
}

/// Include nested transport causes hidden by reqwest's top-level Display text.
fn error_chain(error: &dyn std::error::Error) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

pub(crate) fn start_compact(app: &mut App<'_>) {
    if !app.require_running_session() {
        return;
    }
    use crate::ui::components::conversation_panel::conversation_panel::ActivePhase;
    use crate::ui::event::Event;

    if app.agent_loop.cancel.active_id.is_some() {
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_warning_string("cannot compact while a turn is in flight");
        app.ui.conversation_panel.scroll_to_bottom();
        return;
    }
    invalidate_auto_compaction(app);
    let cutoff = app
        .agent_loop
        .session
        .conversation
        .lock()
        .unwrap()
        .compaction_cutoff(app.effective_compact_keep_recent_turns());
    let Some(cutoff) = cutoff else {
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_info_string("nothing old enough to compact yet".to_string());
        app.ui.conversation_panel.scroll_to_bottom();
        return;
    };
    let target_model = app.effective_compact_model();
    let (client, model_name) = match app.provider_manager.resolve(&target_model) {
        Some((c, m)) => (c.clone(), m),
        None => {
            app.agent_loop
                .session
                .conversation
                .lock()
                .unwrap()
                .add_error_string(format!("unknown provider/model: {target_model}"));
            app.ui.conversation_panel.scroll_to_bottom();
            return;
        }
    };
    app.agent_loop
        .session
        .conversation
        .lock()
        .unwrap()
        .add_info_string(format!("compacting with {target_model}"));
    app.ui.conversation_panel.scroll_to_bottom();

    // The full current context plus the summarization instruction. No tools:
    // the model must answer with the summary text, not act.
    let input_items = compact_input_items(
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .input_param_for_prefix(
                cutoff,
                &target_model,
                app.config.soul.as_deref(),
                app.agent_loop.session.vision_enabled,
            ),
    );

    app.agent_loop.phase = ActivePhase::Compacting;
    let operation_id = app.agent_loop.cancel.begin(None);
    let cancel_token = app.agent_loop.cancel.active.child();
    let thinking_level = app.agent_loop.session.thinking_level;
    let sender = app.events.sender.clone();
    app.agent_loop
        .background_handles
        .push(tokio::spawn(async move {
            let request = build_compact_request(input_items, model_name, thinking_level);
            let result = stream_compact_response(&client, request, cancel_token.clone()).await;
            // Always send CompactFinished — even when cancelled — so
            // handle_compact_finished can clear active_id and reset the phase.
            let _ = sender.send(Event::App(crate::ui::event::AppEvent::CompactFinished(
                operation_id,
                cutoff,
                result,
                cancel_token,
            )));
        }));
}

/// Observe a provider-reported input-token count and, when the session's
/// threshold is crossed, snapshot a complete historical prefix for seamless
/// background compaction. The foreground turn and input remain interactive.
pub(crate) fn maybe_start_auto_compact(app: &mut App<'_>, input_tokens: u32) -> bool {
    if !app.session_accepts_work() {
        return false;
    }
    app.agent_loop.auto_compact.last_input_tokens = Some(input_tokens);
    let threshold = if app.agent_loop.auto_compact.mandatory_waiting {
        app.mandatory_compact_tokens()
    } else {
        app.effective_auto_compact_tokens()
    };
    let Some(threshold) = threshold else {
        return false;
    };
    if !app.agent_loop.auto_compact.mandatory_waiting
        && app.config.auto_compact_cooldown_turns > 0
        && app
            .agent_loop
            .auto_compact
            .last_completed_item_count
            .is_some_and(|start| {
                app.agent_loop
                    .session
                    .conversation
                    .lock()
                    .unwrap()
                    .user_turns_after(start)
                    <= app.config.auto_compact_cooldown_turns
            })
    {
        return false;
    }
    if input_tokens < threshold {
        return false;
    }
    if app.agent_loop.auto_compact.active_id.is_some() {
        return true;
    }
    let snapshot = app
        .agent_loop
        .session
        .conversation
        .lock()
        .unwrap()
        .items
        .clone();
    // The paused runner has recorded every tool output. Only at that safe
    // point may the active turn join the stable prefix without splitting calls
    // from their outputs; background compaction still stops before the turn.
    let stable_end = if app.agent_loop.auto_compact.mandatory_resume.is_some() {
        snapshot.len()
    } else {
        app.agent_loop
            .cancel
            .turn_conversation_cutoff
            .unwrap_or(snapshot.len())
            .min(snapshot.len())
    };
    let keep_recent_turns = if app.agent_loop.auto_compact.mandatory_waiting {
        0
    } else {
        app.effective_compact_keep_recent_turns()
    };
    let cutoff = app
        .agent_loop
        .session
        .conversation
        .lock()
        .unwrap()
        .compaction_cutoff_before(keep_recent_turns, stable_end);
    let Some(cutoff) = cutoff else {
        return false;
    };
    if app.agent_loop.auto_compact.last_cutoff == Some(cutoff) {
        return false;
    }
    let target_model = app.effective_compact_model();
    let Some((client, model_name)) = app.provider_manager.resolve(&target_model) else {
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_warning_string(format!(
                "automatic context compaction skipped: unknown provider/model {target_model}"
            ));
        app.ui.conversation_panel.scroll_to_bottom();
        app.agent_loop.auto_compact.last_cutoff = Some(cutoff);
        return false;
    };
    let client = client.clone();
    let input_items = compact_input_items(
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .input_param_for_prefix(
                cutoff,
                &target_model,
                app.config.soul.as_deref(),
                app.agent_loop.session.vision_enabled,
            ),
    );
    app.agent_loop.auto_compact.next_id = app.agent_loop.auto_compact.next_id.wrapping_add(1);
    let job_id = app.agent_loop.auto_compact.next_id;
    let history_epoch = app.agent_loop.auto_compact.history_epoch;
    app.agent_loop.auto_compact.active_id = Some(job_id);
    let cancellation = crate::cancel::CancellationToken::new();
    app.agent_loop.auto_compact.cancellation = Some(cancellation.clone());
    app.agent_loop.auto_compact.last_cutoff = Some(cutoff);
    let thinking_level = app.agent_loop.session.thinking_level;
    let sender = app.events.sender.clone();
    app.agent_loop
        .background_handles
        .push(tokio::spawn(async move {
            let request = build_compact_request(input_items, model_name, thinking_level);
            // Keep the same job active during recovery: queued input must not be
            // left idle between a transient provider failure and its retry.
            let result =
                stream_compact_response(&client, request.clone(), cancellation.clone()).await;
            let result = match result {
                Err(_) if !cancellation.is_cancelled() => {
                    tokio::select! {
                        biased;
                        _ = cancellation.wait() => return,
                        _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
                    }
                    stream_compact_response(&client, request, cancellation).await
                }
                result => result,
            };
            let _ = sender.send(Event::App(AppEvent::AutoCompactFinished {
                job_id,
                history_epoch,
                cutoff,
                result,
            }));
        }));
    true
}

pub(crate) fn invalidate_auto_compaction(app: &mut App<'_>) {
    if let Some(cancellation) = app.agent_loop.auto_compact.cancellation.take() {
        cancellation.cancel();
    }
    app.agent_loop.auto_compact.history_epoch =
        app.agent_loop.auto_compact.history_epoch.wrapping_add(1);
    app.agent_loop.auto_compact.active_id = None;
    app.agent_loop.auto_compact.last_cutoff = None;
    app.agent_loop.auto_compact.last_completed_item_count = None;
    app.agent_loop.auto_compact.mandatory_waiting = false;
    app.agent_loop.auto_compact.retry_blocked = false;
    if app.agent_loop.auto_compact.mandatory_resume.is_some() {
        app.agent_loop.cancel.active.cancel();
    }
    if let Some(resume) = app.agent_loop.auto_compact.mandatory_resume.take() {
        let _ = resume.send(());
    }
    app.ui.input_panel.clear_next_turn_compaction();
}

/// Open the full-screen task panel. Interactive tasks can grab input; pipe
/// tasks use the same viewer in a read-only mode.
pub(super) fn open_terminal(app: &mut App<'_>, arg: &str) {
    use crate::ui::components::terminal_panel::TerminalPane;

    // Accept an id as the first token (completion may append the task name).
    let first = arg.split_whitespace().next().unwrap_or("");
    if first.eq_ignore_ascii_case("clear") {
        let cleared = app.agent_loop.session.tasks.clear_finished();
        if let Some(sidebar) = app.ui.sidebar.as_mut() {
            sidebar.retain_existing_tasks(&app.agent_loop.session.tasks);
        }
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_info_string(format!("Cleared {cleared} finished task(s)."));
        app.ui.conversation_panel.scroll_to_bottom();
        session::mark_dirty(app);
        return;
    }
    let id = if first.is_empty() {
        // Auto-select the sole running task.
        let running: Vec<u64> = app
            .agent_loop
            .session
            .tasks
            .snapshot_all()
            .iter()
            .filter(|t| t.status == crate::tasks::TaskStatus::Running)
            .map(|t| t.id)
            .collect();
        match running.as_slice() {
            [only] => *only,
            [] => {
                app.agent_loop
                    .session
                    .conversation
                    .lock()
                    .unwrap()
                    .add_warning_string("no running task — create one with the task tool");
                app.ui.conversation_panel.scroll_to_bottom();
                return;
            }
            _ => {
                app.agent_loop
                    .session
                    .conversation
                    .lock()
                    .unwrap()
                    .add_warning_string("multiple running tasks — specify one with /terminal <id>");
                app.ui.conversation_panel.scroll_to_bottom();
                return;
            }
        }
    } else {
        match first.parse::<u64>() {
            Ok(id) => id,
            Err(_) => {
                app.agent_loop
                    .session
                    .conversation
                    .lock()
                    .unwrap()
                    .add_warning_string(format!("/terminal: '{first}' is not a task id"));
                app.ui.conversation_panel.scroll_to_bottom();
                return;
            }
        }
    };

    let Some(snapshot) = app.agent_loop.session.tasks.snapshot(id) else {
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_warning_string(format!("task {id} was not found"));
        app.ui.conversation_panel.scroll_to_bottom();
        return;
    };
    app.ui.terminal_pane = Some(TerminalPane::new(
        app.agent_loop.session.tasks.clone(),
        id,
        snapshot.name,
    ));
}

// ---------------------------------------------------------------------------
// Slash-command dispatch
// ---------------------------------------------------------------------------

async fn memory_command(app: &mut App<'_>, argument: &str) -> command_handlers::CommandOutcome {
    app.ui.input_panel.clear();
    let mut parts = argument.trim().splitn(4, char::is_whitespace);
    let action = parts
        .next()
        .filter(|part| !part.is_empty())
        .unwrap_or("list");

    if matches!(action, "on" | "off") {
        app.config.memory.enabled = action == "on";
        // The worker reads this on its next wake, so turning memory off also
        // stops background consolidation rather than only recall.
        app.refresh_dream_runtime();
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_info_string(format!("Persistent memory {} for this run.", action));
        app.ui.conversation_panel.scroll_to_bottom();
        return command_handlers::CommandOutcome::handled(false);
    }

    if action == "dream" {
        return dream_command(app, parts).await;
    }

    let arguments = match action {
        "list" => serde_json::json!({
            "action": "list",
            "scope": parts.next().filter(|part| !part.is_empty()),
        }),
        "recall" => serde_json::json!({
            "action": "recall",
            "query": parts.collect::<Vec<_>>().join(" "),
        }),
        "remember" => serde_json::json!({
            "action": "remember",
            "scope": parts.next(),
            "kind": parts.next(),
            "content": parts.next(),
        }),
        "update" => serde_json::json!({
            "action": "update",
            "id": parts.next(),
            "content": parts.next(),
        }),
        "forget" => serde_json::json!({
            "action": "forget",
            "id": parts.next(),
        }),
        _ => {
            app.agent_loop.session.conversation.lock().unwrap().add_warning_string(
            "usage: /memory [list [global|project] | recall <query> | remember <global|project> <kind> <content> | update <id> <content> | forget <id> | dream [status|preview|apply|history] [global|project] | on | off]",
            );
            app.ui.conversation_panel.scroll_to_bottom();
            return command_handlers::CommandOutcome::handled(false);
        }
    };

    let is_recall = crate::tools::memory::action_is_recall(&arguments.to_string());
    // One resolver for recall, association, and Dream, so all three agree on
    // which provider/model does memory work.
    let memory_model = is_recall.then(|| app.effective_memory_model()).flatten();
    if is_recall {
        app.agent_loop.phase =
            crate::ui::components::conversation_panel::conversation_panel::ActivePhase::Associating;
    }
    let excluded = app
        .agent_loop
        .session
        .conversation
        .clone()
        .lock()
        .unwrap()
        .context_memory_ids();
    let result = crate::tools::memory::run_excluding(
        &arguments.to_string(),
        memory_model.as_ref(),
        &excluded,
    )
    .await;
    if is_recall {
        app.agent_loop.phase =
            crate::ui::components::conversation_panel::conversation_panel::ActivePhase::None;
    }
    match result {
        Ok(output) => {
            app.agent_loop
                .session
                .conversation
                .lock()
                .unwrap()
                .add_info_string(output);
            app.ui.conversation_panel.scroll_to_bottom();
        }
        Err(error) => {
            app.agent_loop
                .session
                .conversation
                .lock()
                .unwrap()
                .add_warning_string(error);
            app.ui.conversation_panel.scroll_to_bottom();
        }
    }
    command_handlers::CommandOutcome::handled(false)
}

/// `/memory dream [status|preview|apply|history] [global|project]`.
///
/// Kept out of the `memory` tool on purpose: consolidating long-term memory is
/// a user decision, so the agent can neither see nor invoke it.
async fn dream_command(
    app: &mut App<'_>,
    mut parts: impl Iterator<Item = &str>,
) -> command_handlers::CommandOutcome {
    use crate::memory::dream;

    let mode = parts
        .next()
        .filter(|part| !part.is_empty())
        .unwrap_or("status");
    if mode == "recover" {
        let confirmed = parts.next() == Some("confirm") && parts.next().is_none();
        if !confirmed {
            app.agent_loop.session.conversation.lock().unwrap().add_warning_string(
            "Recovery completes the recorded target of an interrupted Dream apply/rollback, including any remaining writes. It does NOT undo it. Run `/memory dream recover confirm` in the originating workspace to proceed.",
            );
            app.ui.conversation_panel.scroll_to_bottom();
            return command_handlers::CommandOutcome::handled(false);
        }
        match crate::memory::MemoryManager::for_current_dir()
            .and_then(|manager| manager.dream_recover())
        {
            Ok(()) => {
                app.agent_loop
                    .session
                    .conversation
                    .lock()
                    .unwrap()
                    .add_info_string("Dream transaction recovered.");
                app.ui.conversation_panel.scroll_to_bottom();
            }
            Err(error) => {
                app.agent_loop
                    .session
                    .conversation
                    .lock()
                    .unwrap()
                    .add_warning_string(format!("Dream recovery failed: {error}"));
                app.ui.conversation_panel.scroll_to_bottom();
            }
        }
        return command_handlers::CommandOutcome::handled(false);
    }
    if mode == "history" {
        let filter = parts.next();
        if !matches!(filter, None | Some("session")) || parts.next().is_some() {
            app.agent_loop
                .session
                .conversation
                .lock()
                .unwrap()
                .add_warning_string("usage: /memory dream history [session]");
            app.ui.conversation_panel.scroll_to_bottom();
            return command_handlers::CommandOutcome::handled(false);
        }
        super::activity::open_dream(app, filter.is_some());
        return command_handlers::CommandOutcome::handled(false);
    }
    let scope = match parts.next().filter(|part| !part.is_empty()) {
        Some("global") => Some(crate::memory::MemoryScope::Global),
        Some("project") => Some(crate::memory::MemoryScope::Project),
        Some(other) => {
            app.agent_loop
                .session
                .conversation
                .lock()
                .unwrap()
                .add_warning_string(format!(
                    "unknown Dream scope '{other}' — use global or project"
                ));
            app.ui.conversation_panel.scroll_to_bottom();
            return command_handlers::CommandOutcome::handled(false);
        }
        None => None,
    };
    let Ok(manager) = crate::memory::MemoryManager::for_current_dir() else {
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_warning_string("error: memory store is unavailable");
        app.ui.conversation_panel.scroll_to_bottom();
        return command_handlers::CommandOutcome::handled(false);
    };

    let config = dream::DreamConfig::from(&app.config.memory);
    let result: Result<String, String> = match mode {
        "status" => manager.dream_status().map(|(state, queued, preview)| {
            let mut line = dream::render_status(&state, queued, preview);
            if !config.enabled {
                line.push_str(" · automatic Dream is disabled");
            }
            line
        }),
        "preview" => {
            if app.agent_loop.cancel.active_id.is_some() {
                app.agent_loop
                    .session
                    .conversation
                    .lock()
                    .unwrap()
                    .add_warning_string(
                        "cannot preview Dream while another operation is in flight",
                    );
                app.ui.conversation_panel.scroll_to_bottom();
                return command_handlers::CommandOutcome::handled(false);
            }
            let Some(model) = app.effective_memory_model() else {
                app.agent_loop
                    .session
                    .conversation
                    .lock()
                    .unwrap()
                    .add_warning_string("error: Dream preview requires a configured memory model");
                app.ui.conversation_panel.scroll_to_bottom();
                return command_handlers::CommandOutcome::handled(false);
            };
            let model = dream::DreamModel {
                client: model.client,
                model: model.model,
            };
            start_dream_preview(app, move |cancellation| async move {
                model
                    .preview(&manager, &config, scope, cancellation)
                    .await
                    .map(dream_report_line)
            });
            return command_handlers::CommandOutcome::handled(false);
        }
        "apply" => dream::apply_saved_preview(&manager, scope).map(dream_report_line),
        other => {
            app.agent_loop
                .session
                .conversation
                .lock()
                .unwrap()
                .add_warning_string(format!(
                    "usage: /memory dream [status|preview|apply|history] [global|project] \
             — '{other}' is not a Dream mode"
                ));
            app.ui.conversation_panel.scroll_to_bottom();
            return command_handlers::CommandOutcome::handled(false);
        }
    };

    match result {
        Ok(line) => {
            app.agent_loop
                .session
                .conversation
                .lock()
                .unwrap()
                .add_info_string(line);
            app.ui.conversation_panel.scroll_to_bottom();
        }
        Err(error) => {
            app.agent_loop
                .session
                .conversation
                .lock()
                .unwrap()
                .add_warning_string(format!("error: {error}"));
            app.ui.conversation_panel.scroll_to_bottom();
        }
    }
    command_handlers::CommandOutcome::handled(false)
}

/// Keep network work outside the UI event handler. The terminal event releases
/// ownership even after cancellation, before any queued successor may start.
pub(crate) fn start_dream_preview<F, Fut>(app: &mut App<'_>, preview: F)
where
    F: FnOnce(crate::cancel::CancellationToken) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<String, String>> + Send + 'static,
{
    if !app.require_running_session() {
        return;
    }
    if app.agent_loop.cancel.active_id.is_some() {
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_warning_string("cannot preview Dream while another operation is in flight");
        app.ui.conversation_panel.scroll_to_bottom();
        return;
    }
    let operation_id = app.agent_loop.cancel.begin(None);
    let cancellation = app.agent_loop.cancel.active.child();
    app.agent_loop.cancel.activity = Some("Dream preview".into());
    app.agent_loop.phase =
        crate::ui::components::conversation_panel::conversation_panel::ActivePhase::Associating;
    app.agent_loop
        .session
        .conversation
        .lock()
        .unwrap()
        .add_info_string("Generating Dream preview · Esc cancels; memories will not be applied.");
    app.ui.conversation_panel.scroll_to_bottom();
    let sender = app.events.sender.clone();
    app.agent_loop
        .background_handles
        .push(tokio::spawn(async move {
            let result = preview(cancellation).await;
            let _ = sender.send(Event::App(AppEvent::DreamPreviewFinished(
                operation_id,
                result,
            )));
        }));
}

fn dream_report_line(report: crate::memory::dream::DreamReport) -> String {
    format!(
        "{} (queued={}, operations={}, applied={})",
        report.message, report.pending, report.operations, report.applied
    )
}

/// Parse and execute a slash command. If the command is unknown, fall back
/// to sending it to the AI model.
pub(crate) async fn execute_command(app: &mut App<'_>, input: &str) {
    app.ui.input_panel.completion = None;
    let Some(command) = Command::parse(input) else {
        // Unknown slash-command; send it to the AI as a normal message.
        app.events.send(AppEvent::Start);
        return;
    };

    if !matches!(command, Command::New | Command::Quit) && !app.require_running_session() {
        return;
    }
    let outcome = match command {
        command @ (Command::Quit
        | Command::Clear
        | Command::New
        | Command::Session(_)
        | Command::Title(_)
        | Command::Usage
        | Command::Rewind
        | Command::Todo
        | Command::Terminal(_)
        | Command::Help) => command_handlers::session::execute(app, command).await,
        command @ (Command::Model(_)
        | Command::Vision(_)
        | Command::Select(_)
        | Command::Theme(_)
        | Command::Mode(_)
        | Command::Classifier(_)
        | Command::Thinking(_)
        | Command::KeepRetry(_)
        | Command::Permission(_)) => command_handlers::settings::execute(app, command),
        command @ (Command::Providers(_)
        | Command::Skill(_)
        | Command::Mcp(_)
        | Command::Diagnostics(_)) => command_handlers::integrations::execute(app, command),
        Command::Memory(arg) => memory_command(app, &arg).await,
        command @ (Command::Init | Command::Compact(_) | Command::Plan(_)) => {
            command_handlers::workflow::execute(app, command).await
        }
    };

    if outcome.save_session {
        session::save_session(app);
    }
    if outcome.record_history {
        app.ui.input_panel.push_history(input.to_string());
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::{
        KeepRetryMode, build_compact_request, execute_command, format_agent_updates,
        format_task_updates, normalize_input_suggestion, normalize_session_title,
        ordered_message_content, queue_pending_request, send_message, start_keep_retry,
        start_request_as, start_runtime_update_request,
    };
    use crate::cancel::OperationId;
    use crate::ui::event::{AppEvent, Event};
    use async_openai::types::responses::{ImageDetail, InputContent, InputImageContent};
    use async_openai::types::responses::{InputMessage, InputRole, MessageItem as ApiMessageItem};
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::{collections::BTreeSet, time::Duration};

    #[derive(Clone, Copy, Debug)]
    enum ExpectedCommandEffect {
        AppendedMessage,
        ResetWithInfo,
        ClearedConversation,
        ProviderPanel,
        SkillsPanel,
        McpPanel,
        DiagnosticsPanel,
        SecurityPanel,
        TodoPanel,
        Quit,
    }

    pub(crate) async fn command_test_app() -> crate::app::App<'static> {
        let mut config = crate::config::programmer_config::ProgrammerConfig::default();
        config.providers.clear();
        let mut app = crate::app::App::new(
            config,
            crate::app::session::SessionSeed::Fresh {
                uuid: uuid::Uuid::new_v4().to_string(),
            },
            None,
            Vec::new(),
            false,
            "test-project".to_string(),
        )
        .await;
        let root = std::env::current_dir()
            .unwrap()
            .join(".programmer")
            .join(format!(
                "command-checkpoints-{}",
                app.agent_loop.session.uuid
            ));
        app.agent_loop.session.checkpoint_store = Some(std::sync::Arc::new(std::sync::Mutex::new(
            crate::checkpoint::CheckpointStore::for_test(root),
        )));
        app
    }

    fn append_compaction_test_turn(conversation: &mut crate::conversation::Conversation) {
        conversation.add_input_message(ApiMessageItem::Input(InputMessage {
            content: vec![InputContent::InputText("audit history".into())],
            role: InputRole::User,
            status: None,
        }));
    }

    fn append_compaction_test_tools(
        conversation: &mut crate::conversation::Conversation,
        index: usize,
    ) {
        use async_openai::types::responses::{
            FunctionCallOutput, FunctionCallOutputItemParam, FunctionToolCall, OutputItem,
        };
        let call_id = format!("history_{index}");
        conversation.add_output(OutputItem::FunctionCall(FunctionToolCall {
            arguments: "{}".into(),
            call_id: call_id.clone(),
            namespace: None,
            name: "command".into(),
            id: None,
            status: None,
        }));
        conversation.add_tool_output(crate::tools::ToolOutput {
            param: FunctionCallOutputItemParam {
                call_id,
                output: FunctionCallOutput::Text("large history output".repeat(1000)),
                id: None,
                status: None,
            },
            failed: false,
            approval_label: None,
        });
    }

    #[tokio::test]
    async fn auto_compact_paused_turn_advances_past_each_completed_tool_batch() {
        let mut app = command_test_app().await;
        app.config.compact_keep_recent_turns = 10;
        app.agent_loop.cancel.turn_conversation_cutoff = Some(0);
        app.agent_loop.auto_compact.mandatory_waiting = true;
        let (resume, _receiver) = tokio::sync::oneshot::channel();
        app.agent_loop.auto_compact.mandatory_resume = Some(resume);
        let conversation = app.agent_loop.session.conversation.clone();
        append_compaction_test_turn(&mut conversation.lock().unwrap());
        let mut previous_cutoff = 0;
        for index in 0..3 {
            let end = {
                let mut conversation = conversation.lock().unwrap();
                append_compaction_test_tools(&mut conversation, index);
                conversation.items.len()
            };
            // No provider is configured: selection is recorded without a network request.
            assert!(!super::maybe_start_auto_compact(&mut app, u32::MAX));
            assert_eq!(app.agent_loop.auto_compact.last_cutoff, Some(end));
            assert!(end > previous_cutoff);
            assert!(
                conversation
                    .lock()
                    .unwrap()
                    .apply_compaction_at(end, "summary".into())
            );
            app.agent_loop.auto_compact.last_cutoff = None;
            assert!(!super::maybe_start_auto_compact(&mut app, u32::MAX));
            assert_eq!(app.agent_loop.auto_compact.last_cutoff, None);
            previous_cutoff = end;
        }
    }

    #[tokio::test]
    async fn auto_compact_background_never_selects_active_turn_tools() {
        let mut app = command_test_app().await;
        app.config.compact_keep_recent_turns = 0;
        app.config.auto_compact_cooldown_turns = 0;
        let conversation = app.agent_loop.session.conversation.clone();
        let stable_end = {
            let mut conversation = conversation.lock().unwrap();
            append_compaction_test_turn(&mut conversation);
            append_compaction_test_tools(&mut conversation, 0);
            let stable_end = conversation.items.len();
            append_compaction_test_turn(&mut conversation);
            append_compaction_test_tools(&mut conversation, 1);
            stable_end
        };
        app.agent_loop.cancel.turn_conversation_cutoff = Some(stable_end);
        assert!(!super::maybe_start_auto_compact(&mut app, u32::MAX));
        assert_eq!(app.agent_loop.auto_compact.last_cutoff, Some(stable_end));
    }

    #[tokio::test]
    async fn clear_closes_background_work_before_resetting_domain_state() {
        let mut app = command_test_app().await;
        let tasks = app.agent_loop.session.tasks.clone();
        let conversation = app.agent_loop.session.conversation.clone();
        let todos = app.agent_loop.session.todo_store.clone();
        let generation = app.task_event_generation;
        let task = tasks.spawn("sleep 30", None, None).unwrap();
        execute_command(&mut app, "/clear").await;
        assert_ne!(
            tasks.snapshot(task).unwrap().status,
            crate::tasks::TaskStatus::Running
        );
        assert!(tasks.spawn("true", None, None).is_err());
        assert!(app.session_accepts_work());
        assert!(app.agent_loop.session.tasks.snapshot_all().is_empty());
        let task = app
            .agent_loop
            .session
            .tasks
            .spawn("true", None, None)
            .unwrap();
        assert!(app.ui.conversation_panel.tasks.snapshot(task).is_some());
        assert!(app.task_event_generation > generation);
        assert!(!std::sync::Arc::ptr_eq(
            &conversation,
            &app.agent_loop.session.conversation
        ));
        assert!(!std::sync::Arc::ptr_eq(
            &todos,
            &app.agent_loop.session.todo_store
        ));
        app.close_session().await.unwrap();
    }

    #[tokio::test]
    async fn closing_session_joins_its_event_forwarder() {
        let mut app = command_test_app().await;
        let forwarders: Vec<_> = app
            .agent_loop
            .background_handles
            .iter()
            .map(tokio::task::JoinHandle::abort_handle)
            .collect();
        assert!(!forwarders.is_empty());
        // Keep the manager alive: dropping App is not enough to close its sink.
        let tasks = app.agent_loop.session.tasks.clone();
        app.close_session().await.unwrap();
        assert!(forwarders.iter().all(tokio::task::AbortHandle::is_finished));
        assert!(app.agent_loop.background_handles.is_empty());
        assert!(tasks.spawn("true", None, None).is_err());
    }

    #[tokio::test]
    async fn new_session_barrier_joins_runner_and_replaces_closed_tasks() {
        let mut app = command_test_app().await;
        let old_uuid = app.agent_loop.session.uuid.clone();
        let old_tasks = app.agent_loop.session.tasks.clone();
        let task = old_tasks.spawn("sleep 30", None, None).unwrap();
        let operation = app.agent_loop.cancel.begin(None);
        let cancel = app.agent_loop.cancel.active.clone();
        let (finished, completion) = tokio::sync::oneshot::channel();
        app.agent_loop.runner_handles.push(tokio::spawn(async move {
            cancel.wait_or(std::future::pending::<()>()).await;
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            finished.send(()).unwrap();
        }));
        execute_command(&mut app, "/new").await;
        completion.await.unwrap();
        assert!(!app.agent_loop.cancel.is_current(operation));
        assert_ne!(app.agent_loop.session.uuid, old_uuid);
        assert!(app.agent_loop.runner_handles.is_empty());
        assert_ne!(
            old_tasks.snapshot(task).unwrap().status,
            crate::tasks::TaskStatus::Running
        );
        assert!(old_tasks.spawn("true", None, None).is_err());
        assert!(app.agent_loop.session.tasks.snapshot_all().is_empty());
        let task = app
            .agent_loop
            .session
            .tasks
            .spawn("true", None, None)
            .unwrap();
        assert!(app.ui.conversation_panel.tasks.snapshot(task).is_some());
        app.agent_loop
            .session
            .tasks
            .spawn("true", None, None)
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Event::App(AppEvent::TaskStateChanged(event)) =
                    app.events.next().await.unwrap()
                    && event.generation == app.task_event_generation
                {
                    break;
                }
            }
        })
        .await
        .unwrap();
        app.close_session().await.unwrap();
    }

    #[tokio::test]
    async fn new_session_persists_terminal_task_state_in_old_session() {
        let mut app = command_test_app().await;
        let directory =
            std::env::temp_dir().join(format!("programmer-close-session-{}", uuid::Uuid::new_v4()));
        app.agent_loop.session.mgr =
            Some(crate::session::SessionManager::for_test(directory.clone()));
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_input_message(ApiMessageItem::Input(InputMessage {
                content: vec![InputContent::InputText("keep task history".into())],
                role: InputRole::User,
                status: None,
            }));
        app.ui.conversation_panel.scroll_to_bottom();
        let uuid = app.agent_loop.session.uuid.clone();
        app.agent_loop
            .session
            .tasks
            .spawn("sleep 30", None, None)
            .unwrap();
        execute_command(&mut app, "/new").await;
        assert_ne!(app.agent_loop.session.uuid, uuid);
        let old = app
            .agent_loop
            .session
            .mgr
            .as_ref()
            .unwrap()
            .load(&uuid)
            .unwrap()
            .unwrap();
        assert_eq!(old.tasks.len(), 1);
        assert_ne!(old.tasks[0].status, "running");
        assert!(!app.agent_loop.session.did_save);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn new_session_runner_join_failure_is_reported_without_replacing_session() {
        let mut app = command_test_app().await;
        let uuid = app.agent_loop.session.uuid.clone();
        app.agent_loop
            .runner_handles
            .push(tokio::spawn(async { panic!("runner failed") }));
        execute_command(&mut app, "/new").await;
        assert_eq!(app.agent_loop.session.uuid, uuid);
        assert!(!app.session_accepts_work());
        assert!(
            app.ui
                .conversation_panel
                .items_snapshot()
                .iter()
                .any(|item| matches!(item, crate::response::message_item::MessageItem::Error(_)))
        );
    }

    #[tokio::test]
    async fn new_session_save_failure_keeps_old_session_without_success_message() {
        let mut app = command_test_app().await;
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_input_message(ApiMessageItem::Input(InputMessage {
                content: vec![InputContent::InputText("keep me".into())],
                role: InputRole::User,
                status: None,
            }));
        app.ui.conversation_panel.scroll_to_bottom();
        let uuid = app.agent_loop.session.uuid.clone();
        execute_command(&mut app, "/new").await;
        assert_eq!(app.agent_loop.session.uuid, uuid);
        assert!(!app.session_accepts_work());
        let items = app.ui.conversation_panel.items_snapshot();
        assert!(
            items
                .iter()
                .any(|item| matches!(item, crate::response::message_item::MessageItem::Error(_)))
        );
        assert!(
            !items.iter().any(|item| matches!(
                item,
                crate::response::message_item::MessageItem::Info(message)
                    if message.contains("Started a new session.")
            )),
            "a failed save must not announce a new session"
        );
    }

    #[tokio::test]
    async fn interrupted_session_close_keeps_admission_sealed_and_runner_for_retry() {
        let mut app = command_test_app().await;
        let uuid = app.agent_loop.session.uuid.clone();
        let tasks = app.agent_loop.session.tasks.clone();
        let operation = app.agent_loop.cancel.begin(None);
        let (release, completion) = tokio::sync::oneshot::channel::<()>();
        let runner = tokio::spawn(async move {
            completion.await.unwrap();
        });
        let runner_id = runner.id();
        app.agent_loop.runner_handles.push(runner);

        // Bound the test's wait, not the production timeout: dropping this
        // close future must leave ownership intact for a subsequent retry.
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(30), app.close_session())
                .await
                .is_err()
        );
        assert!(!app.session_accepts_work());
        assert!(!app.require_running_session());
        assert!(tasks.spawn("true", None, None).is_err());
        assert_eq!(app.agent_loop.session.uuid, uuid);
        assert_eq!(app.agent_loop.runner_handles.len(), 1);
        assert_eq!(app.agent_loop.runner_handles[0].id(), runner_id);
        assert!(!app.agent_loop.runner_handles[0].is_finished());
        assert_eq!(app.agent_loop.cancel.active_id, Some(operation));

        release.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), app.close_session())
            .await
            .expect("released runner should not block the close retry")
            .unwrap();
        assert!(app.agent_loop.runner_handles.is_empty());
        assert_eq!(app.agent_loop.cancel.active_id, None);
        assert_eq!(app.agent_loop.session.uuid, uuid);
        assert!(!app.session_accepts_work());
        assert!(tasks.spawn("true", None, None).is_err());
    }

    #[tokio::test]
    async fn timed_out_session_close_keeps_runner_owned_until_retry() {
        let mut app = command_test_app().await;
        let uuid = app.agent_loop.session.uuid.clone();
        let tasks = app.agent_loop.session.tasks.clone();
        let operation = app.agent_loop.cancel.begin(None);
        let (release, completion) = tokio::sync::oneshot::channel::<()>();
        let runner = tokio::spawn(async move {
            completion.await.unwrap();
        });
        let runner_id = runner.id();
        app.agent_loop.runner_handles.push(runner);

        // Exercise the production ten-second deadline, not cancellation of the
        // close future by a shorter test timeout.
        let started = std::time::Instant::now();
        let error = app.close_session().await.unwrap_err();
        assert_eq!(error, "runner shutdown timed out; session was not replaced");
        assert!(started.elapsed() >= std::time::Duration::from_secs(10));
        assert!(!app.session_accepts_work());
        assert!(tasks.spawn("true", None, None).is_err());
        assert_eq!(app.agent_loop.session.uuid, uuid);
        assert_eq!(app.agent_loop.runner_handles.len(), 1);
        assert_eq!(app.agent_loop.runner_handles[0].id(), runner_id);
        assert!(!app.agent_loop.runner_handles[0].is_finished());
        assert_eq!(app.agent_loop.cancel.active_id, Some(operation));

        release.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), app.close_session())
            .await
            .expect("released runner should not block the close retry")
            .unwrap();
        assert!(app.agent_loop.runner_handles.is_empty());
        assert_eq!(app.agent_loop.cancel.active_id, None);
        assert_eq!(app.agent_loop.session.uuid, uuid);
        assert!(!app.session_accepts_work());
        assert!(tasks.spawn("true", None, None).is_err());
    }

    #[tokio::test]
    async fn stopping_session_rejects_all_startup_paths_and_can_retry_new() {
        let mut app = command_test_app().await;
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_input_message(ApiMessageItem::Input(InputMessage {
                content: vec![InputContent::InputText("preserve source".into())],
                role: InputRole::User,
                status: None,
            }));
        app.ui.conversation_panel.scroll_to_bottom();
        let source = app.agent_loop.session.conversation.clone();
        let todos = app.agent_loop.session.todo_store.clone();
        let operation = app.agent_loop.cancel.begin(None);
        app.agent_loop.pending_request = Some(super::super::scheduling::UserRequest {
            text: "must not run".into(),
            images: Vec::new(),
        });
        execute_command(&mut app, "/new").await;
        assert!(!app.session_accepts_work()); // no persistence manager: save failed
        let next_id = app.agent_loop.cancel.next_id;
        app.ui.input_panel.set_content("draft stays here");
        send_message(&mut app).await;
        assert_eq!(app.ui.input_panel.get_content(), "draft stays here");
        start_request_as(&mut app, "user".into(), InputRole::User).await;
        start_runtime_update_request(
            &mut app,
            Vec::new(),
            Vec::new(),
            Some(super::super::scheduling::UserRequest {
                text: "notification".into(),
                images: Vec::new(),
            }),
        )
        .await;
        start_keep_retry(&mut app, KeepRetryMode::Exponential);
        for event in [
            AppEvent::StartInit("init".into()),
            AppEvent::TurnFinished(operation, Err(crate::runner::RunnerError::Cancelled)),
        ] {
            crate::app::events::handle_event(&mut app, Event::App(event))
                .await
                .unwrap();
        }
        crate::app::peers::sync_session(&mut app).await;
        assert!(!crate::app::peers::poll(&mut app, true).await);
        assert_eq!(app.agent_loop.cancel.next_id, next_id);
        assert!(app.agent_loop.cancel.active_id.is_none());
        assert_eq!(
            app.agent_loop.pending_request.as_ref().unwrap().text,
            "must not run"
        );
        let directory = std::env::current_dir()
            .unwrap()
            .join(".programmer")
            .join(format!("barrier-test-{}", uuid::Uuid::new_v4()));
        app.agent_loop.session.mgr = Some(crate::session::SessionManager::for_test(directory));
        execute_command(&mut app, "/new").await;
        assert!(app.session_accepts_work());
        assert!(!Arc::ptr_eq(&source, &app.agent_loop.session.conversation));
        assert!(!Arc::ptr_eq(&todos, &app.agent_loop.session.todo_store));
        assert!(
            source
                .lock()
                .unwrap()
                .items
                .iter()
                .any(|item| matches!(item, crate::response::message_item::MessageItem::Input(_)))
        );
        app.close_session().await.unwrap();
    }

    #[tokio::test]
    async fn clear_during_active_turn_preserves_history_and_operation() {
        let mut app = command_test_app().await;
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_input_message(ApiMessageItem::Input(InputMessage {
                content: vec![InputContent::InputText("keep active history".into())],
                role: InputRole::User,
                status: None,
            }));
        app.ui.conversation_panel.scroll_to_bottom();
        let before = app.ui.conversation_panel.items_snapshot().len();
        let operation = app.agent_loop.cancel.begin(None);
        execute_command(&mut app, "/clear").await;
        assert_eq!(app.agent_loop.cancel.active_id, Some(operation));
        let items = app.ui.conversation_panel.items_snapshot();
        assert_eq!(items.len(), before + 1);
        assert!(format!("{:?}", items.last()).contains("Cannot clear while a turn is in flight"));
        assert!(app.session_accepts_work());
        app.close_session().await.unwrap();
    }

    #[tokio::test]
    async fn quit_defers_saving_until_barrier_and_escape_keeps_task_admission_open() {
        let mut app = command_test_app().await;
        app.agent_loop.cancel.begin(None);
        crate::app::events::handle_event(&mut app, Event::App(AppEvent::Cancel))
            .await
            .unwrap();
        let tasks = app.agent_loop.session.tasks.clone();
        tasks.spawn("sleep 30", None, None).unwrap();
        app.quit();
        assert!(!app.running);
        assert!(!app.agent_loop.session.did_save);
        app.close_session().await.unwrap();
        assert!(
            tasks
                .snapshot_all()
                .iter()
                .all(|task| task.status != crate::tasks::TaskStatus::Running)
        );
        assert!(tasks.spawn("true", None, None).is_err());
    }

    #[tokio::test]
    async fn consent_model_switch_is_silent_dirty_and_preserves_running_snapshot() {
        use crate::config::programmer_config::ProviderConfig;
        use crate::ui::{components::question_panel::QuestionPanel, event::AnswerTx};
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = command_test_app().await;
        app.config.providers.insert(
            "test".into(),
            ProviderConfig {
                base_url: "http://127.0.0.1:1/v1".into(),
                api_key: String::new(),
                models: Some(vec!["old".into(), "new".into()]),
                default_model: None,
            },
        );
        app.provider_manager = crate::providers::ProviderManager::from_config(&app.config);
        app.agent_loop.session.current_model = "test/old".into();
        let runner = app.build_runner().expect("snapshot");
        app.agent_loop.cancel.active_id = Some(OperationId(42));
        let cancel = app.agent_loop.cancel.active.clone();
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        let mut panel = QuestionPanel::delegation("source", "task", true, AnswerTx(tx));
        panel.set_delegation_models(
            &app.agent_loop.session.current_model,
            vec![crate::commands::CompletionCandidate {
                value: "test/new".into(),
                label: "test/new".into(),
            }],
        );
        app.ui.question_panel = Some(panel);
        let (_consent_tx, consent_rx) = tokio::sync::oneshot::channel();
        app.agent_loop.peer_consent = Some((
            crate::peers::PeerEnvelope::new(
                uuid::Uuid::new_v4().to_string(),
                uuid::Uuid::new_v4().to_string(),
                crate::peers::PeerKind::Delegation,
                "task".into(),
                None,
            )
            .unwrap(),
            consent_rx,
        ));
        // Model selection is routed before tool review and must not answer consent.
        let before = app.ui.conversation_panel.items_snapshot().len();
        for code in [KeyCode::Char('m'), KeyCode::Down, KeyCode::Enter] {
            crate::app::events::handle_key_events(
                &mut app,
                KeyEvent::new(code, KeyModifiers::NONE),
            )
            .await
            .unwrap();
        }
        assert_eq!(app.agent_loop.session.current_model, "test/new");
        assert!(app.agent_loop.session.dirty);
        assert_eq!(app.ui.conversation_panel.items_snapshot().len(), before);
        assert!(rx.try_recv().is_err());
        assert_eq!(runner.model_str, "test/old");
        assert_eq!(runner.model_name, "old");
        assert_eq!(app.build_runner().unwrap().model_str, "test/new");
        assert_eq!(app.agent_loop.cancel.active_id, Some(OperationId(42)));
        assert!(!cancel.is_cancelled());
        // Peer consent owns Escape even while a turn is active.
        for code in [KeyCode::Char('m'), KeyCode::Esc] {
            crate::app::events::handle_key_events(
                &mut app,
                KeyEvent::new(code, KeyModifiers::NONE),
            )
            .await
            .unwrap();
        }
        assert!(app.ui.question_panel.is_some());
        assert_eq!(app.agent_loop.cancel.active_id, Some(OperationId(42)));
        assert!(!cancel.is_cancelled());
        crate::app::events::handle_key_events(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        )
        .await
        .unwrap();
        assert_eq!(rx.await.unwrap(), "No");
        assert_eq!(app.agent_loop.session.current_model, "test/new");
    }

    #[tokio::test]
    async fn every_registered_command_has_an_observable_effect() {
        let cases = [
            ("model", "/model", ExpectedCommandEffect::AppendedMessage),
            ("new", "/new", ExpectedCommandEffect::ResetWithInfo),
            (
                "providers",
                "/providers manage",
                ExpectedCommandEffect::ProviderPanel,
            ),
            (
                "session",
                "/session",
                ExpectedCommandEffect::AppendedMessage,
            ),
            ("title", "/title", ExpectedCommandEffect::AppendedMessage),
            ("usage", "/usage", ExpectedCommandEffect::AppendedMessage),
            ("rewind", "/rewind", ExpectedCommandEffect::AppendedMessage),
            ("mode", "/mode auto", ExpectedCommandEffect::AppendedMessage),
            (
                "classifier",
                "/classifier",
                ExpectedCommandEffect::AppendedMessage,
            ),
            ("init", "/init", ExpectedCommandEffect::AppendedMessage),
            ("todo", "/todo", ExpectedCommandEffect::TodoPanel),
            (
                "memory",
                "/memory off",
                ExpectedCommandEffect::AppendedMessage,
            ),
            ("skill", "/skill manage", ExpectedCommandEffect::SkillsPanel),
            ("mcp", "/mcp manage", ExpectedCommandEffect::McpPanel),
            (
                "diagnostics",
                "/diagnostics manage",
                ExpectedCommandEffect::DiagnosticsPanel,
            ),
            ("plan", "/plan", ExpectedCommandEffect::AppendedMessage),
            (
                "terminal",
                "/terminal invalid",
                ExpectedCommandEffect::AppendedMessage,
            ),
            (
                "compact",
                "/compact",
                ExpectedCommandEffect::AppendedMessage,
            ),
            (
                "thinking",
                "/thinking",
                ExpectedCommandEffect::AppendedMessage,
            ),
            (
                "keepretry",
                "/keepretry",
                ExpectedCommandEffect::AppendedMessage,
            ),
            ("vision", "/vision", ExpectedCommandEffect::AppendedMessage),
            ("theme", "/theme", ExpectedCommandEffect::AppendedMessage),
            (
                "select",
                "/select invalid",
                ExpectedCommandEffect::AppendedMessage,
            ),
            (
                "permission",
                "/permission manage",
                ExpectedCommandEffect::SecurityPanel,
            ),
            (
                "clear",
                "/clear",
                ExpectedCommandEffect::ClearedConversation,
            ),
            ("quit", "/quit", ExpectedCommandEffect::Quit),
            ("help", "/help", ExpectedCommandEffect::AppendedMessage),
        ];

        let covered: BTreeSet<_> = cases.iter().map(|(name, _, _)| *name).collect();
        let registered: BTreeSet<_> = crate::commands::Command::all_commands().collect();
        assert_eq!(covered, registered, "update this contract for new commands");

        for (name, input, expected) in cases {
            let mut app = command_test_app().await;
            let before = app.ui.conversation_panel.items_snapshot().len();
            execute_command(&mut app, input).await;

            match expected {
                ExpectedCommandEffect::AppendedMessage => assert!(
                    app.ui.conversation_panel.items_snapshot().len() > before,
                    "/{name} produced no visible message"
                ),
                ExpectedCommandEffect::ResetWithInfo => assert!(matches!(
                    app.ui.conversation_panel.items_snapshot().last(),
                    Some(crate::response::message_item::MessageItem::Info(_))
                )),
                ExpectedCommandEffect::ClearedConversation => {
                    assert!(app.ui.conversation_panel.items_snapshot().is_empty())
                }
                ExpectedCommandEffect::ProviderPanel => assert!(app.ui.provider_panel.is_some()),
                ExpectedCommandEffect::SkillsPanel => assert!(app.ui.skills_panel.is_some()),
                ExpectedCommandEffect::McpPanel => assert!(app.ui.mcp_panel.is_some()),
                ExpectedCommandEffect::DiagnosticsPanel => {
                    assert!(app.ui.diagnostics_panel.is_some())
                }
                ExpectedCommandEffect::SecurityPanel => assert!(app.ui.security_panel.is_some()),
                ExpectedCommandEffect::TodoPanel => assert!(app.ui.todo_panel.is_some()),
                ExpectedCommandEffect::Quit => assert!(!app.running),
            }
        }
    }

    #[test]
    fn compact_request_uses_the_selected_thinking_level() {
        let request = build_compact_request(
            Vec::new(),
            "test-model".to_string(),
            crate::thinking::ThinkingLevel::Low,
        );
        assert_eq!(
            serde_json::to_value(request.reasoning).unwrap()["effort"],
            "low"
        );

        let auto = build_compact_request(
            Vec::new(),
            "test-model".to_string(),
            crate::thinking::ThinkingLevel::Auto,
        );
        assert!(auto.reasoning.is_none());
    }

    #[tokio::test]
    async fn title_command_sets_a_manual_title() {
        let mut app = command_test_app().await;

        execute_command(&mut app, "/title  Fix queued editing  ").await;

        assert_eq!(app.agent_loop.session.title, "Fix queued editing");
        assert!(app.agent_loop.session.title_generation_started);
        assert_eq!(app.agent_loop.session.title_generation_id, 1);
    }

    #[test]
    fn generated_session_title_is_cleaned_and_bounded() {
        assert_eq!(
            normalize_session_title("  “修复登录重试”  \nextra").unwrap(),
            "修复登录重试"
        );
        assert_eq!(
            normalize_session_title("   \n"),
            Err("the model returned an empty title".to_string())
        );
        assert_eq!(
            normalize_session_title(&"x".repeat(100))
                .unwrap()
                .chars()
                .count(),
            80
        );
    }

    #[test]
    fn generated_input_suggestion_is_cleaned_and_bounded() {
        assert_eq!(
            normalize_input_suggestion("  “继续  修复测试”\n  ").unwrap(),
            "继续 修复测试"
        );
        assert!(normalize_input_suggestion("  \n ").is_err());
        assert_eq!(
            normalize_input_suggestion(&"x".repeat(300))
                .unwrap()
                .chars()
                .count(),
            240
        );
    }

    #[tokio::test]
    async fn queued_file_reference_uses_the_real_expansion_path() {
        let expanded = crate::commands::expand_references("inspect @Cargo.toml", None).await;
        let mut pending_request = None;

        queue_pending_request(
            &mut pending_request,
            super::super::scheduling::UserRequest {
                text: expanded.text,
                images: expanded.images,
            },
        );

        let request = pending_request.expect("queued request");
        let pending = &request.text;
        assert!(pending.contains("inspect @Cargo.toml"));
        assert!(pending.contains("Referenced local file path (content not included): Cargo.toml"));
        assert!(request.images.is_empty());
    }

    #[test]
    fn queue_coalesces_text_and_preserves_all_images() {
        let image = || InputImageContent {
            detail: ImageDetail::Auto,
            file_id: None,
            image_url: Some("data:image/png;base64,AAAA".to_string()),
        };
        let mut pending_request = None;

        queue_pending_request(
            &mut pending_request,
            super::super::scheduling::UserRequest {
                text: "first".to_string(),
                images: vec![image()],
            },
        );
        queue_pending_request(
            &mut pending_request,
            super::super::scheduling::UserRequest {
                text: "second".to_string(),
                images: vec![image()],
            },
        );

        let request = pending_request.expect("coalesced request");
        assert_eq!(request.text, "first\nsecond");
        assert_eq!(request.images, vec![image(), image()]);
    }

    #[test]
    fn pasted_image_stays_before_text_typed_after_it() {
        let image = InputImageContent {
            detail: ImageDetail::Auto,
            file_id: None,
            image_url: Some("data:image/png;base64,AAAA".to_string()),
        };

        let content = ordered_message_content(
            "[Pasted image #1 640x480]Can you see this?".to_string(),
            vec![image],
        );

        assert!(matches!(content[0], InputContent::InputImage(_)));
        assert!(matches!(
            &content[1],
            InputContent::InputText(text) if text.text == "Can you see this?"
        ));
    }

    #[test]
    fn task_update_prompt_is_hidden_runtime_context_with_output() {
        let prompt = format_task_updates(&[crate::tasks::TaskLifecycleEvent {
            sequence: 1,
            generation: 1,
            task_id: 9,
            origin: crate::tasks::TaskOrigin::PromotedCommand,
            old_status: crate::tasks::TaskStatus::Running,
            new_status: crate::tasks::TaskStatus::Failed,
            name: "build".to_string(),
            command: "cargo build".to_string(),
            exit_code: Some(101),
            elapsed: Duration::from_millis(1250),
            stdout_tail: "building".to_string(),
            stderr_tail: "compiler error".to_string(),
            transcript_tail: String::new(),
            notify_agent: Arc::new(AtomicBool::new(true)),
        }]);

        assert!(prompt.contains("<background_task_updates>"));
        assert!(prompt.contains("Task 9 changed from running to failed"));
        assert!(prompt.contains("Origin: promoted command"));
        assert!(prompt.contains("Exit code: 101"));
        assert!(prompt.contains("compiler error"));
        assert!(prompt.contains("continue with the next safe step"));
    }

    #[test]
    fn agent_update_prompt_carries_results_with_a_size_limit() {
        let prompt = format_agent_updates(&[crate::agents::AgentSnapshot {
            id: 3,
            name: "review".to_string(),
            prompt: "review the parser".to_string(),
            status: crate::agents::AgentStatus::Completed,
            elapsed: Duration::from_secs(2),
            result: Some("x".repeat(13_000)),
            phase: None,
        }]);

        assert!(prompt.contains("<sub_agent_updates>"));
        assert!(prompt.contains("Sub-agent 3 (review) completed"));
        assert!(prompt.contains("review the parser"));
        assert!(prompt.contains("sub-agent result truncated"));
        assert!(prompt.len() < 12_500);
    }
}
