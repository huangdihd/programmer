// Copyright (C) 2026 huangdihd
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

use super::CommandOutcome;
use crate::app::{App, commands, diagnostics, session};
use crate::commands::Command;
use crate::ui::components::todo_panel::TodoPanel;

pub(in crate::app) async fn execute(app: &mut App<'_>, command: Command) -> CommandOutcome {
    app.ui.input_panel.clear();
    match command {
        Command::Quit => {
            app.quit();
            CommandOutcome::handled(false)
        }
        Command::Clear => clear(app).await,
        Command::New => new(app).await,
        Command::Session(arguments) => match arguments.trim() {
            "" => show_session(app),
            "graph" => {
                crate::app::activity::open_graph(app);
                CommandOutcome::handled(false)
            }
            _ => {
                app.agent_loop
                    .session
                    .conversation
                    .lock()
                    .unwrap()
                    .add_warning_string("usage: /session [graph]");
                app.ui.conversation_panel.scroll_to_bottom();
                CommandOutcome::handled(false)
            }
        },
        Command::Title(title) => {
            commands::set_or_regenerate_session_title(app, &title);
            CommandOutcome::handled(true)
        }
        Command::Usage => usage(app),
        Command::Rewind => rewind(app),
        Command::Todo => {
            app.sync_todos_from_store();
            app.ui.todo_panel = Some(TodoPanel::new(app.ui.todo_list.clone()));
            CommandOutcome::handled(false)
        }
        Command::Terminal(arg) => {
            commands::open_terminal(app, &arg);
            CommandOutcome::handled(false)
        }
        Command::Help => help(app),
        _ => unreachable!("session handler received a command from another domain"),
    }
}

fn rewind(app: &mut App<'_>) -> CommandOutcome {
    if app.agent_loop.cancel.active_id.is_some() {
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_warning_string("cannot rewind while a turn is in flight");
        app.ui.conversation_panel.scroll_to_bottom();
        return CommandOutcome::handled(false);
    }
    let Some(store) = &app.agent_loop.session.checkpoint_store else {
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_warning_string("rewind checkpoints are unavailable");
        app.ui.conversation_panel.scroll_to_bottom();
        return CommandOutcome::handled(false);
    };
    let panel =
        crate::ui::components::rewind_panel::RewindPanel::new(store.lock().unwrap().checkpoints());
    if panel.is_empty() {
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_info_string("no user prompt checkpoints to rewind to");
        app.ui.conversation_panel.scroll_to_bottom();
    } else {
        app.ui.rewind_panel = Some(panel);
    }
    CommandOutcome::handled(false)
}

fn show_session(app: &mut App<'_>) -> CommandOutcome {
    let item_count = app
        .agent_loop
        .session
        .conversation
        .lock()
        .unwrap()
        .items
        .len();
    let message = match &app.agent_loop.session.mgr {
        Some(manager) => {
            let path = manager.session_path(&app.agent_loop.session.uuid);
            format_session_info(
                item_count,
                &app.agent_loop.session.uuid,
                &app.agent_loop.session.title,
                Some((&path, path.exists())),
            )
        }
        None => format_session_info(
            item_count,
            &app.agent_loop.session.uuid,
            &app.agent_loop.session.title,
            None,
        ),
    };
    app.agent_loop
        .session
        .conversation
        .lock()
        .unwrap()
        .add_info_string(message);
    app.ui.conversation_panel.scroll_to_bottom();
    CommandOutcome::handled(true)
}

fn format_session_info(
    item_count: usize,
    uuid: &str,
    title: &str,
    session_file: Option<(&std::path::Path, bool)>,
) -> String {
    let title_line = if title.is_empty() {
        String::new()
    } else {
        format!("\n  title: {title}")
    };
    match session_file {
        Some((path, exists)) => {
            let status = if exists {
                "saved on disk"
            } else {
                "not yet saved"
            };
            format!(
                "Session: {item_count} messages, {status}{title_line}\n  uuid: {uuid}\n  path: {}",
                path.display()
            )
        }
        None => format!(
            "Session: {item_count} messages (no session manager){title_line}\n  uuid: {uuid}"
        ),
    }
}

fn usage(app: &mut App<'_>) -> CommandOutcome {
    let summary = app
        .agent_loop
        .session
        .conversation
        .lock()
        .unwrap()
        .usage_summary();
    app.agent_loop
        .session
        .conversation
        .lock()
        .unwrap()
        .add_info_string(format_usage(summary));
    app.ui.conversation_panel.scroll_to_bottom();
    CommandOutcome::handled(true)
}

fn cache_percent(cached_tokens: u64, input_tokens: u64) -> u64 {
    cached_tokens
        .saturating_mul(100)
        .checked_div(input_tokens)
        .unwrap_or(0)
}

fn format_usage(summary: crate::conversation::UsageSummary) -> String {
    match summary.last_turn {
        Some(_) => format!(
            "Token usage for this session:\n\
             \u{20} total input: {} tokens\n\
             \u{20} cached input: {} tokens ({}%)\n\
             \u{20} output: {} tokens\n\
             \u{20} total: {} tokens\n\
             \u{20} recorded turns: {}\n\
             Last request input: {}",
            summary.input_tokens,
            summary.cached_input_tokens,
            cache_percent(summary.cached_input_tokens, summary.input_tokens),
            summary.output_tokens,
            summary.total_tokens(),
            summary.turns,
            summary.last_request_input_tokens.map_or_else(
                || "unavailable".to_string(),
                |tokens| format!("{tokens} tokens")
            ),
        ),
        None => "No token usage recorded for this session.".to_string(),
    }
}

async fn clear(app: &mut App<'_>) -> CommandOutcome {
    if app.agent_loop.cancel.active_id.is_some() {
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_warning_string(
                "Cannot clear while a turn is in flight; cancel it and wait for completion first.",
            );
        app.ui.conversation_panel.scroll_to_bottom();
        return CommandOutcome::handled(false);
    }
    if let Err(error) = app.close_session().await {
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_error_string(format!("Cannot clear session: {error}"));
        app.ui.conversation_panel.scroll_to_bottom();
        return CommandOutcome::handled(false);
    }
    app.agent_loop.session.tasks = crate::tasks::TaskManager::default();
    app.connect_task_events();
    app.agent_loop.task_notifications.clear();
    app.agent_loop.session.agents = crate::agents::AgentManager::default();
    app.agent_loop.agent_notifications = crate::app::AgentNotificationState::new();
    app.agent_loop.session.conversation = Default::default();
    app.ui.conversation_panel = crate::ui::components::conversation_panel::conversation_panel::ConversationPanel::from_shared(
        app.agent_loop.session.conversation.clone(),
    );
    app.ui.conversation_panel.tasks = app.agent_loop.session.tasks.clone();
    app.agent_loop.session.todo_store = Default::default();
    diagnostics::reset_diagnostics_state(app);
    app.agent_loop.pending_request = None;
    session::delete_session(app);
    app.sync_todos_from_store();
    app.agent_loop.session.lifecycle = crate::app::SessionLifecycle::Running;
    CommandOutcome::handled(true)
}

async fn new(app: &mut App<'_>) -> CommandOutcome {
    commands::invalidate_auto_compaction(app);
    app.ui.active_suggestion_operation_id = None;
    if let Some(cancel) = app.ui.input_suggestion_cancel.take() {
        cancel.cancel();
    }
    app.ui.input_panel.clear_suggestion();
    if let Err(error) = app.close_session().await {
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_error_string(format!("Cannot start a new session: {error}"));
        app.ui.conversation_panel.scroll_to_bottom();
        return CommandOutcome::handled(false);
    }
    let saved = match session::persist_session(app) {
        Ok(saved) => saved,
        Err(error) => {
            app.agent_loop
                .session
                .conversation
                .lock()
                .unwrap()
                .add_error_string(format!("Cannot start a new session: session save: {error}"));
            app.ui.conversation_panel.scroll_to_bottom();
            return CommandOutcome::handled(false);
        }
    };
    // The session that is ending is queued for consolidation before its
    // conversation is cleared, exactly as it would be on quit.
    if saved {
        app.queue_current_session_for_dream();
    }
    let conversation = std::sync::Arc::new(std::sync::Mutex::new(
        crate::conversation::Conversation::new(),
    ));
    app.agent_loop.session.conversation = conversation.clone();
    app.ui.conversation_panel.conversation = conversation;
    app.agent_loop.session.conversation.lock().unwrap().clear();
    app.ui.conversation_panel.clear_view();
    app.agent_loop.session.todo_store = Default::default();
    diagnostics::reset_diagnostics_state(app);
    app.agent_loop.pending_request = None;
    app.agent_loop.session.tasks = crate::tasks::TaskManager::default();
    app.connect_task_events();
    app.agent_loop.task_notifications.clear();
    app.agent_loop.session.agents = crate::agents::AgentManager::default();
    app.agent_loop.agent_notifications = crate::app::AgentNotificationState::new();
    if let Some(manager) = &app.agent_loop.session.mgr {
        let new_session = manager.create();
        app.agent_loop.session.uuid = new_session.uuid;
    } else {
        app.agent_loop.session.uuid = uuid::Uuid::new_v4().to_string();
    }
    app.agent_loop.session.checkpoint_store =
        crate::checkpoint::CheckpointStore::for_session(&app.agent_loop.session.uuid)
            .map(|store| std::sync::Arc::new(std::sync::Mutex::new(store)));
    app.agent_loop.current_checkpoint_id = None;
    app.agent_loop.session.did_save = false;
    app.agent_loop.session.dirty = false;
    app.agent_loop.session.persistence = session::PersistenceState::default();
    app.agent_loop.session.title.clear();
    app.agent_loop.session.title_generation_started = false;
    app.agent_loop.session.title_generation_id =
        app.agent_loop.session.title_generation_id.wrapping_add(1);
    *app.agent_loop.session.todo_store.lock().unwrap() = crate::todos::TodoList::default();
    app.sync_todos_from_store();
    app.agent_loop.session.vision_enabled = app.config.vision_enabled;
    app.agent_loop.session.classifier_model_override = crate::session::ModelOverride::Inherit;
    app.agent_loop.session.compact_model_override = crate::session::ModelOverride::Inherit;
    app.agent_loop.session.auto_compact_override = crate::session::AutoCompactOverride::Inherit;
    app.agent_loop.session.compact_keep_recent_turns_override = None;

    app.agent_loop.session.lifecycle = crate::app::SessionLifecycle::Running;
    let message = if saved {
        "Started a new session. Previous session saved."
    } else {
        "Started a new session. Previous session was empty; nothing to save."
    }
    .to_string();
    app.agent_loop
        .session
        .conversation
        .lock()
        .unwrap()
        .add_info_string(message);
    app.ui.conversation_panel.scroll_to_bottom();
    CommandOutcome::handled(true)
}

fn help(app: &mut App<'_>) -> CommandOutcome {
    let mut lines: Vec<String> = Command::descriptions()
        .into_iter()
        .map(|(command, description)| format!("  {command:35} {description}"))
        .collect();
    lines.insert(0, "Available commands:".to_string());
    app.agent_loop
        .session
        .conversation
        .lock()
        .unwrap()
        .add_info_string(lines.join("\n"));
    app.ui.conversation_panel.scroll_to_bottom();
    CommandOutcome::handled(true)
}

#[cfg(test)]
mod tests {
    use super::{format_session_info, format_usage};
    use crate::conversation::UsageSummary;
    use std::path::Path;

    #[test]
    fn session_info_reports_identity_path_and_saved_state() {
        let message = format_session_info(
            7,
            "session-uuid",
            "Fix login retries",
            Some((Path::new("/tmp/session-uuid.json"), true)),
        );

        assert_eq!(
            message,
            "Session: 7 messages, saved on disk\n  title: Fix login retries\n  uuid: session-uuid\n  path: /tmp/session-uuid.json"
        );
    }

    #[test]
    fn session_info_handles_unavailable_session_manager() {
        assert_eq!(
            format_session_info(0, "session-uuid", "", None),
            "Session: 0 messages (no session manager)\n  uuid: session-uuid"
        );
    }

    #[test]
    fn usage_message_reports_total_and_last_request_input() {
        let message = format_usage(UsageSummary {
            input_tokens: 13,
            output_tokens: 7,
            cached_input_tokens: 5,
            turns: 2,
            last_turn: Some((3, 2, 2)),
            last_request_input_tokens: Some(11),
        });

        assert!(message.contains("total input: 13 tokens"));
        assert!(message.contains("cached input: 5 tokens (38%)"));
        assert!(message.contains("output: 7 tokens"));
        assert!(message.contains("total: 20 tokens"));
        assert!(message.contains("recorded turns: 2"));
        assert!(message.contains("Last request input: 11 tokens"));
    }

    #[test]
    fn usage_message_handles_empty_sessions() {
        assert_eq!(
            format_usage(UsageSummary::default()),
            "No token usage recorded for this session."
        );
    }
}
