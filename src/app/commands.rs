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
use crate::classifier::{PlanPhase, WorkMode};
use crate::commands::Command;
use crate::ui::components::conversation_panel::conversation_panel::ConversationPanel;
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

/// Build an optional plan-mode system prompt snippet.
fn plan_system_prompt(app: &App<'_>) -> Option<&'static str> {
    if app.work_mode != WorkMode::Plan {
        return None;
    }
    match app.plan_phase {
        PlanPhase::Planning => Some(PLAN_PLANNING_PROMPT),
        PlanPhase::Reviewing => None,
    }
}

// ---------------------------------------------------------------------------
// Message sending
// ---------------------------------------------------------------------------

/// Collect input, push to history, and start a user request.
pub(crate) async fn send_message(app: &mut App<'_>) {
    let typed = app.input_panel.expanded_content();
    if typed.is_empty() {
        return;
    }
    let pasted_images = app.input_panel.take_images();
    // History keeps the compact `@path` form; the model receives a path-only
    // reference for regular files or a stored image for image paths. The
    // conversation filters stored images out of API requests while vision is
    // off, without deleting them from session history.
    app.input_panel.push_history(typed.clone());
    app.input_panel.clear();

    // Expand before deciding whether to start or queue the request. Queued
    // messages must retain the same path annotations and image attachments as
    // messages that start immediately.
    let diagnostics = app.diagnostics_state.lock().unwrap().baseline.clone();
    let expanded = crate::commands::expand_references(&typed, diagnostics.as_deref()).await;
    for notice in expanded.notices {
        app.conversation_panel.add_warning_string(notice);
    }
    let mut images = pasted_images;
    images.extend(expanded.images);
    let omitted = crate::commands::limit_image_attachments(&mut images);
    if omitted > 0 {
        app.conversation_panel.add_warning_string(format!(
            "omitted {omitted} image(s): attachment count or total size exceeds the per-message limit"
        ));
    }
    start_request_with_images(app, expanded.text, images).await;
}

pub(crate) async fn start_request_with_images(
    app: &mut App<'_>,
    text: String,
    images: Vec<async_openai::types::responses::InputImageContent>,
) {
    start_request_as_with_images(app, text, InputRole::User, images).await;
}

/// Start a turn from a message with the given role. `User` is a normal user
/// message; `Developer` carries a hidden instruction (like `/init`).
pub(crate) async fn start_request_as(app: &mut App<'_>, text: String, role: InputRole) {
    start_request_as_with_images(app, text, role, Vec::new()).await;
}

async fn start_request_as_with_images(
    app: &mut App<'_>,
    text: String,
    role: InputRole,
    images: Vec<async_openai::types::responses::InputImageContent>,
) {
    // active_id is the lifecycle authority. UI phases are presentation state
    // and can briefly lag the runner; they must never decide whether two turns
    // are allowed to overlap.
    if app.cancel.active_id.is_some() {
        queue_pending_request(
            &mut app.conversation_panel,
            &mut app.pending_images,
            text,
            images,
        );
        return;
    }

    start_ready_request(app, vec![(text, role, images)]).await;
}

/// Start one turn containing any completed task/sub-agent updates and an
/// optional queued user follow-up. Runtime updates use the developer role so
/// they cannot impersonate the user.
pub(crate) async fn start_runtime_update_request(
    app: &mut App<'_>,
    task_events: Vec<crate::tasks::TaskLifecycleEvent>,
    agent_events: Vec<crate::agents::AgentSnapshot>,
    pending_user: Option<(String, Vec<InputImageContent>)>,
) {
    if task_events.is_empty() && agent_events.is_empty() {
        if let Some((text, images)) = pending_user {
            start_request_with_images(app, text, images).await;
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
    if let Some((text, images)) = pending_user {
        inputs.push((text, InputRole::User, images));
    }
    start_ready_request(app, inputs).await;
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

async fn start_ready_request(
    app: &mut App<'_>,
    inputs: Vec<(String, InputRole, Vec<InputImageContent>)>,
) {
    debug_assert!(app.cancel.active_id.is_none());
    for (text, role, images) in inputs {
        let content = ordered_message_content(text, images);
        app.conversation_panel
            .add_input_message(ApiMessageItem::Input(InputMessage {
                content,
                role,
                status: Some(OutputStatus::Completed),
            }));
    }
    app.conversation_panel.reset_accumulated_usage();
    diagnostics::maybe_seed_diagnostics_baseline(app);
    session::save_session(app);
    // Fresh turn: start from an un-cancelled root token so a prior turn's Esc
    // doesn't carry over to this one. Bump the operation id synchronously
    // before spawning so the UI can tag all turn events and filter stale ones.
    app.cancel.active = crate::cancel::CancellationToken::new();
    app.cancel.next_id = app.cancel.next_id.wrapping_add(1);
    let operation_id = app.cancel.next_id;
    app.cancel.active_id = Some(operation_id);

    let Some(runner) = app.build_runner() else {
        app.cancel.active_id = None;
        app.conversation_panel
            .add_error_string(format!("unknown provider/model: {}", app.current_model));
        return;
    };
    let surface = TuiSurface {
        tx: app.events.sender.clone(),
        skill_prompt: app.skill_registry.catalog_prompt(),
        plan_prompt: plan_system_prompt(app),
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
    panel: &mut ConversationPanel,
    pending_images: &mut Vec<InputImageContent>,
    text: String,
    mut images: Vec<InputImageContent>,
) {
    let is_at_bottom = panel.is_at_bottom();
    match panel.pending_message.as_mut() {
        Some(pending) => {
            pending.push('\n');
            pending.push_str(&text);
        }
        None => panel.pending_message = Some(text),
    }
    pending_images.append(&mut images);
    if is_at_bottom {
        panel.scroll_to_bottom();
    }
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
    use crate::ui::components::terminal_panel::TerminalPane;

    let command = input.strip_prefix('!').unwrap_or(input).trim().to_string();
    if command.is_empty() {
        app.input_panel.clear();
        return;
    }
    app.input_panel.push_history(input.to_string());
    app.input_panel.clear();
    app.input_panel.completion = None;

    // Spawn at the size the terminal panel will render at, so the first frame
    // doesn't have to resize the fresh PTY. A resize racing the child's
    // startup leaves a SIGWINCH pending from the fork/exec window, which the
    // kernel then delivers at the worst moment (e.g. inside Python 3.14's
    // REPL `tcsetattr`, which dies on EINTR).
    let (rows, cols) = crossterm::terminal::size()
        .map(|(w, h)| (h.saturating_sub(2).max(1), w.max(1)))
        .unwrap_or((24, 80));
    let security = app.security.snapshot();
    match crate::tasks::spawn_bang_secure(&command, None, Some(&command), rows, cols, &security) {
        Ok(id) => {
            // The record in the conversation; the transcript follows when the
            // command exits and the agent picks it up.
            app.conversation_panel.add_info_string(format!(
                "🖥 !{command} — running in the interactive terminal; \
                 the agent will respond when it exits"
            ));
            session::mark_dirty(app);
            let mut pane = TerminalPane::new(id, command);
            // Grab input immediately — the user typed `!` to interact.
            pane.grabbed = true;
            app.terminal_pane = Some(pane);
        }
        Err(e) => app.conversation_panel.add_error_string(e),
    }
}

/// `/compact [provider/model]`: ask the model for a continuation summary of the
/// conversation so far, then (in [`super::events`]'s `CompactFinished` handler)
/// install it as a context boundary — the model afterwards sees the summary
/// instead of the summarized history, while the UI keeps everything visible.
/// An argument picks a different model for the summarization request only; the
/// chat model is unchanged.
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

pub(crate) fn start_compact(app: &mut App<'_>, model_arg: &str) {
    use crate::ui::components::conversation_panel::conversation_panel::ActivePhase;
    use crate::ui::event::Event;
    use async_openai::types::responses::{
        InputItem, InputParam, Item, OutputItem, OutputMessageContent,
    };

    if app.cancel.active_id.is_some() {
        app.conversation_panel
            .add_warning_string("cannot compact while a turn is in flight");
        return;
    }
    if !app.conversation_panel.has_compactable_history() {
        app.conversation_panel
            .add_info_string("nothing to compact yet".to_string());
        return;
    }
    let target_model = if model_arg.is_empty() {
        app.current_model.clone()
    } else {
        model_arg.to_string()
    };
    let (client, model_name) = match app.provider_manager.resolve(&target_model) {
        Some((c, m)) => (c.clone(), m),
        None => {
            app.conversation_panel
                .add_error_string(format!("unknown provider/model: {target_model}"));
            return;
        }
    };
    if !model_arg.is_empty() {
        app.conversation_panel
            .add_info_string(format!("compacting with {target_model}"));
    }

    // The full current context plus the summarization instruction. No tools:
    // the model must answer with the summary text, not act.
    let mut input_items = match app.conversation_panel.get_input_param(
        &target_model,
        None,
        None,
        None,
        app.vision_enabled,
    ) {
        InputParam::Items(items) => items,
        InputParam::Text(text) => vec![InputItem::from(Item::Message(ApiMessageItem::Input(
            InputMessage {
                content: vec![InputContent::InputText(InputTextContent { text })],
                role: InputRole::User,
                status: Some(OutputStatus::Completed),
            },
        )))],
    };
    input_items.push(InputItem::from(Item::Message(ApiMessageItem::Input(
        InputMessage {
            content: vec![InputContent::InputText(InputTextContent {
                text: crate::prompts::COMPACT_PROMPT.to_string(),
            })],
            role: InputRole::User,
            status: Some(OutputStatus::Completed),
        },
    ))));

    app.conversation_panel.phase = ActivePhase::Compacting;
    app.cancel.active = crate::cancel::CancellationToken::new();
    app.cancel.next_id = app.cancel.next_id.wrapping_add(1);
    let operation_id = app.cancel.next_id;
    app.cancel.active_id = Some(operation_id);
    let cancel_token = app.cancel.active.child();
    let thinking_level = app.thinking_level;
    let sender = app.events.sender.clone();
    tokio::spawn(async move {
        let request = build_compact_request(input_items, model_name, thinking_level);
        // Race the model request against cancellation so Esc doesn't leave
        // the UI stuck in Cancelling.
        let result = match cancel_token
            .wait_or(client.responses().create(request))
            .await
        {
            Some(Ok(response)) => {
                let text = response
                    .output
                    .iter()
                    .filter_map(|item| match item {
                        OutputItem::Message(msg) => {
                            Some(msg.content.iter().filter_map(|c| match c {
                                OutputMessageContent::OutputText(t) => Some(t.text.as_str()),
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
            Some(Err(e)) => Err(e.to_string()),
            None => Err("cancelled".to_string()),
        };
        // Always send CompactFinished — even when cancelled — so
        // handle_compact_finished can clear active_id and reset the phase.
        let _ = sender.send(Event::App(crate::ui::event::AppEvent::CompactFinished(
            operation_id,
            result,
            cancel_token,
        )));
    });
}

/// Open the full-screen task panel. Interactive tasks can grab input; pipe
/// tasks use the same viewer in a read-only mode.
pub(super) fn open_terminal(app: &mut App<'_>, arg: &str) {
    use crate::ui::components::terminal_panel::TerminalPane;

    // Accept an id as the first token (completion may append the task name).
    let first = arg.split_whitespace().next().unwrap_or("");
    if first.eq_ignore_ascii_case("clear") {
        let cleared = crate::tasks::clear_finished();
        if let Some(sidebar) = app.sidebar.as_mut() {
            sidebar.retain_existing_tasks();
        }
        app.conversation_panel
            .add_info_string(format!("Cleared {cleared} finished task(s)."));
        session::mark_dirty(app);
        return;
    }
    let id = if first.is_empty() {
        // Auto-select the sole running task.
        let running: Vec<u64> = crate::tasks::snapshot_all()
            .iter()
            .filter(|t| t.status == crate::tasks::TaskStatus::Running)
            .map(|t| t.id)
            .collect();
        match running.as_slice() {
            [only] => *only,
            [] => {
                app.conversation_panel
                    .add_warning_string("no running task — create one with the task tool");
                return;
            }
            _ => {
                app.conversation_panel
                    .add_warning_string("multiple running tasks — specify one with /terminal <id>");
                return;
            }
        }
    } else {
        match first.parse::<u64>() {
            Ok(id) => id,
            Err(_) => {
                app.conversation_panel
                    .add_warning_string(format!("/terminal: '{first}' is not a task id"));
                return;
            }
        }
    };

    let Some(snapshot) = crate::tasks::snapshot(id) else {
        app.conversation_panel
            .add_warning_string(format!("task {id} was not found"));
        return;
    };
    app.terminal_pane = Some(TerminalPane::new(id, snapshot.name));
}

// ---------------------------------------------------------------------------
// Slash-command dispatch
// ---------------------------------------------------------------------------

/// Parse and execute a slash command. If the command is unknown, fall back
/// to sending it to the AI model.
pub(crate) async fn execute_command(app: &mut App<'_>, input: &str) {
    app.input_panel.completion = None;
    let Some(command) = Command::parse(input) else {
        // Unknown slash-command; send it to the AI as a normal message.
        app.events.send(AppEvent::Start);
        return;
    };

    let outcome = match command {
        command @ (Command::Quit
        | Command::Clear
        | Command::New
        | Command::Session
        | Command::Usage
        | Command::Todo
        | Command::Terminal(_)
        | Command::Help) => command_handlers::session::execute(app, command),
        command @ (Command::Model(_)
        | Command::Vision(_)
        | Command::Mode(_)
        | Command::Classifier(_)
        | Command::Thinking(_)
        | Command::Permission(_)) => command_handlers::settings::execute(app, command),
        command @ (Command::Providers(_) | Command::Skill(_) | Command::Mcp(_)) => {
            command_handlers::integrations::execute(app, command)
        }
        command @ (Command::Init | Command::Compact(_) | Command::Plan(_)) => {
            command_handlers::workflow::execute(app, command).await
        }
    };

    if outcome.save_session {
        session::save_session(app);
    }
    if outcome.record_history {
        app.input_panel.push_history(input.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ConversationPanel, build_compact_request, execute_command, format_agent_updates,
        format_task_updates, ordered_message_content, queue_pending_request,
    };
    use async_openai::types::responses::{ImageDetail, InputContent, InputImageContent};
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
        SecurityPanel,
        TodoPanel,
        Quit,
    }

    async fn command_test_app() -> crate::app::App<'static> {
        let mut config = crate::config::programmer_config::ProgrammerConfig::default();
        config.providers.clear();
        crate::app::App::new(
            config,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            "test-session".to_string(),
            None,
            Vec::new(),
            false,
            "test-project".to_string(),
        )
        .await
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
            ("usage", "/usage", ExpectedCommandEffect::AppendedMessage),
            ("mode", "/mode auto", ExpectedCommandEffect::AppendedMessage),
            (
                "classifier",
                "/classifier",
                ExpectedCommandEffect::AppendedMessage,
            ),
            ("init", "/init", ExpectedCommandEffect::AppendedMessage),
            ("todo", "/todo", ExpectedCommandEffect::TodoPanel),
            ("skill", "/skill manage", ExpectedCommandEffect::SkillsPanel),
            ("mcp", "/mcp manage", ExpectedCommandEffect::McpPanel),
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
            ("vision", "/vision", ExpectedCommandEffect::AppendedMessage),
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
            let before = app.conversation_panel.items_snapshot().len();
            execute_command(&mut app, input).await;

            match expected {
                ExpectedCommandEffect::AppendedMessage => assert!(
                    app.conversation_panel.items_snapshot().len() > before,
                    "/{name} produced no visible message"
                ),
                ExpectedCommandEffect::ResetWithInfo => assert!(matches!(
                    app.conversation_panel.items_snapshot().last(),
                    Some(crate::response::message_item::MessageItem::Info(_))
                )),
                ExpectedCommandEffect::ClearedConversation => {
                    assert!(app.conversation_panel.items_snapshot().is_empty())
                }
                ExpectedCommandEffect::ProviderPanel => assert!(app.provider_panel.is_some()),
                ExpectedCommandEffect::SkillsPanel => assert!(app.skills_panel.is_some()),
                ExpectedCommandEffect::McpPanel => assert!(app.mcp_panel.is_some()),
                ExpectedCommandEffect::SecurityPanel => assert!(app.security_panel.is_some()),
                ExpectedCommandEffect::TodoPanel => assert!(app.todo_panel.is_some()),
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
    async fn queued_file_reference_uses_the_real_expansion_path() {
        let expanded = crate::commands::expand_references("inspect @Cargo.toml", None).await;
        let mut panel = ConversationPanel::new();
        let mut pending_images = Vec::new();

        queue_pending_request(
            &mut panel,
            &mut pending_images,
            expanded.text,
            expanded.images,
        );

        let pending = panel.pending_message.as_deref().expect("queued text");
        assert!(pending.contains("inspect @Cargo.toml"));
        assert!(pending.contains("Referenced local file path (content not included): Cargo.toml"));
        assert!(pending_images.is_empty());
    }

    #[test]
    fn queue_coalesces_text_and_preserves_all_images() {
        let image = || InputImageContent {
            detail: ImageDetail::Auto,
            file_id: None,
            image_url: Some("data:image/png;base64,AAAA".to_string()),
        };
        let mut panel = ConversationPanel::new();
        let mut pending_images = Vec::new();

        queue_pending_request(
            &mut panel,
            &mut pending_images,
            "first".to_string(),
            vec![image()],
        );
        queue_pending_request(
            &mut panel,
            &mut pending_images,
            "second".to_string(),
            vec![image()],
        );

        assert_eq!(panel.pending_message.as_deref(), Some("first\nsecond"));
        assert_eq!(pending_images.len(), 2);
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
        }]);

        assert!(prompt.contains("<sub_agent_updates>"));
        assert!(prompt.contains("Sub-agent 3 (review) completed"));
        assert!(prompt.contains("review the parser"));
        assert!(prompt.contains("sub-agent result truncated"));
        assert!(prompt.len() < 12_500);
    }
}
