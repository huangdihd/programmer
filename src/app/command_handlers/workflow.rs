// Copyright (C) 2026 huangdihd
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

use super::CommandOutcome;
use crate::app::{App, commands, session};
use crate::classifier::{PlanPhase, WorkMode};
use crate::commands::Command;
use crate::ui::event::AppEvent;
use async_openai::types::responses::InputRole;

pub(in crate::app) async fn execute(app: &mut App<'_>, command: Command) -> CommandOutcome {
    app.input_panel.clear();
    match command {
        Command::Init => init(app),
        Command::Compact(arg) => compact(app, &arg),
        Command::Plan(arg) => plan(app, &arg).await,
        _ => unreachable!("workflow handler received a command from another domain"),
    }
}

fn init(app: &mut App<'_>) -> CommandOutcome {
    let Some(prompt) = app
        .skill_registry
        .prompt(crate::skills::INITIALIZE_PROJECT_SKILL)
    else {
        app.conversation_panel
            .add_error_string("built-in initialize-project skill is unavailable");
        return CommandOutcome::handled(false);
    };

    if app.cancel.active_id.is_some() {
        commands::queue_pending_request(
            &mut app.conversation_panel,
            &mut app.pending_images,
            prompt,
            Vec::new(),
        );
        return CommandOutcome::without_history(false);
    }

    app.conversation_panel
        .add_info_string("Scanning project and setting up diagnostics…");
    // Send synchronously so another request cannot claim the operation id first.
    app.events.send(AppEvent::StartInit(prompt));
    CommandOutcome::handled(false)
}

fn compact(app: &mut App<'_>, arg: &str) -> CommandOutcome {
    // Completion may append a display name; only the first token is the model.
    let model = arg.split_whitespace().next().unwrap_or("");
    commands::start_compact(app, model);
    CommandOutcome::handled(false)
}

async fn plan(app: &mut App<'_>, arg: &str) -> CommandOutcome {
    match arg.trim().to_lowercase().as_str() {
        "approve" | "ok" | "go"
            if app.work_mode == WorkMode::Plan && app.plan_phase == PlanPhase::Reviewing =>
        {
            app.work_mode = WorkMode::Auto;
            app.plan_phase = PlanPhase::default();
            app.conversation_panel
                .add_info_string("Plan approved — executing with Auto mode.");
            session::save_session(app);
            let instruction =
                "The plan was approved by the user. Execute it now using the identified steps.";
            commands::start_request_as(app, instruction.to_string(), InputRole::Developer).await;
        }
        "approve" | "ok" | "go" => {
            app.conversation_panel
                .add_info_string("No plan pending approval. Use /mode plan to enter Plan mode.");
        }
        "cancel" | "abort" => {
            app.work_mode = WorkMode::Auto;
            app.plan_phase = PlanPhase::default();
            app.conversation_panel
                .add_info_string("Plan cancelled — returned to Auto mode.");
            session::persist_config(app);
        }
        _ => app.conversation_panel.add_info_string(
            "usage: /plan approve — approve current plan and execute\n\
             \u{20}      /plan cancel — cancel plan and return to Auto",
        ),
    }
    CommandOutcome::handled(true)
}
