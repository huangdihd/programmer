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
        Command::Session => CommandOutcome::handled(true),
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
