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

pub(in crate::app) fn execute(app: &mut App<'_>, command: Command) -> CommandOutcome {
    app.input_panel.clear();
    match command {
        Command::Quit => {
            app.quit();
            CommandOutcome::handled(false)
        }
        Command::Clear => clear(app),
        Command::New => new(app),
        Command::Session => show_session(app),
        Command::Usage => usage(app),
        Command::Todo => {
            app.sync_todos_from_store();
            app.todo_panel = Some(TodoPanel::new(app.todo_list.clone()));
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

fn show_session(app: &mut App<'_>) -> CommandOutcome {
    let item_count = app.conversation_panel.items_snapshot().len();
    let message = match &app.session.mgr {
        Some(manager) => {
            let path = manager.session_path(&app.session.uuid);
            format_session_info(item_count, &app.session.uuid, Some((&path, path.exists())))
        }
        None => format_session_info(item_count, &app.session.uuid, None),
    };
    app.conversation_panel.add_info_string(message);
    CommandOutcome::handled(true)
}

fn format_session_info(
    item_count: usize,
    uuid: &str,
    session_file: Option<(&std::path::Path, bool)>,
) -> String {
    match session_file {
        Some((path, exists)) => {
            let status = if exists {
                "saved on disk"
            } else {
                "not yet saved"
            };
            format!(
                "Session: {item_count} messages, {status}\n  uuid: {uuid}\n  path: {}",
                path.display()
            )
        }
        None => format!("Session: {item_count} messages (no session manager)\n  uuid: {uuid}"),
    }
}

fn usage(app: &mut App<'_>) -> CommandOutcome {
    let summary = app.conversation_panel.usage_summary();
    app.conversation_panel
        .add_info_string(format_usage(summary));
    CommandOutcome::handled(true)
}

fn format_usage(summary: crate::conversation::UsageSummary) -> String {
    match summary.last_turn {
        Some((last_input, last_output)) => format!(
            "Token usage for this session:\n\
             \u{20} input: {} tokens\n\
             \u{20} output: {} tokens\n\
             \u{20} total: {} tokens\n\
             \u{20} recorded turns: {}\n\
             Last turn: {} input + {} output = {} tokens",
            summary.input_tokens,
            summary.output_tokens,
            summary.total_tokens(),
            summary.turns,
            last_input,
            last_output,
            u64::from(last_input) + u64::from(last_output),
        ),
        None => "No token usage recorded for this session.".to_string(),
    }
}

fn clear(app: &mut App<'_>) -> CommandOutcome {
    app.conversation_panel.clear_messages();
    diagnostics::reset_diagnostics_state(app);
    app.pending_images.clear();
    session::delete_session(app);
    app.todo_list = crate::todos::TodoList::default();
    app.sync_todos_to_store();
    CommandOutcome::handled(true)
}

fn new(app: &mut App<'_>) -> CommandOutcome {
    session::save_session(app);
    app.conversation_panel.clear_messages();
    diagnostics::reset_diagnostics_state(app);
    app.pending_images.clear();
    let killed = crate::tasks::kill_all();
    app.task_notifications.clear();
    if let Some(manager) = &app.session.mgr {
        let new_session = manager.create();
        app.session.uuid = new_session.uuid;
    }
    app.todo_list = crate::todos::TodoList::default();
    app.sync_todos_to_store();
    app.vision_enabled = false;

    let mut message = "Started a new session. Previous session saved.".to_string();
    if killed > 0 {
        message.push_str(&format!(" Killed {killed} background task(s)."));
    }
    app.conversation_panel.add_info_string(message);
    CommandOutcome::handled(true)
}

fn help(app: &mut App<'_>) -> CommandOutcome {
    let mut lines: Vec<String> = Command::descriptions()
        .into_iter()
        .map(|(command, description)| format!("  {command:35} {description}"))
        .collect();
    lines.insert(0, "Available commands:".to_string());
    app.conversation_panel.add_info_string(lines.join("\n"));
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
            Some((Path::new("/tmp/session-uuid.json"), true)),
        );

        assert_eq!(
            message,
            "Session: 7 messages, saved on disk\n  uuid: session-uuid\n  path: /tmp/session-uuid.json"
        );
    }

    #[test]
    fn session_info_handles_unavailable_session_manager() {
        assert_eq!(
            format_session_info(0, "session-uuid", None),
            "Session: 0 messages (no session manager)\n  uuid: session-uuid"
        );
    }

    #[test]
    fn usage_message_reports_session_and_last_turn_totals() {
        let message = format_usage(UsageSummary {
            input_tokens: 13,
            output_tokens: 7,
            turns: 2,
            last_turn: Some((3, 2)),
        });

        assert!(message.contains("input: 13 tokens"));
        assert!(message.contains("output: 7 tokens"));
        assert!(message.contains("total: 20 tokens"));
        assert!(message.contains("recorded turns: 2"));
        assert!(message.contains("Last turn: 3 input + 2 output = 5 tokens"));
    }

    #[test]
    fn usage_message_handles_empty_sessions() {
        assert_eq!(
            format_usage(UsageSummary::default()),
            "No token usage recorded for this session."
        );
    }
}
