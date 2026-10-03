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
    app.ui.input_panel.clear();
    match command {
        Command::Init => init(app),
        Command::Compact(arg) => compact(app, &arg),
        Command::Plan(arg) => plan(app, &arg).await,
        _ => unreachable!("workflow handler received a command from another domain"),
    }
}

fn init(app: &mut App<'_>) -> CommandOutcome {
    let Some(prompt) = app
        .agent_loop
        .skill_registry
        .prompt(crate::skills::INITIALIZE_PROJECT_SKILL)
    else {
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_error_string("built-in initialize-project skill is unavailable");
        app.ui.conversation_panel.scroll_to_bottom();
        return CommandOutcome::handled(false);
    };

    if app.agent_loop.cancel.active_id.is_some() {
        commands::queue_pending_request(
            &mut app.agent_loop.pending_request,
            crate::app::scheduling::UserRequest {
                text: prompt,
                images: Vec::new(),
            },
        );
        return CommandOutcome::without_history(false);
    }

    app.agent_loop
        .session
        .conversation
        .lock()
        .unwrap()
        .add_info_string("Scanning project and setting up diagnostics…");

    app.ui.conversation_panel.scroll_to_bottom();
    // Send synchronously so another request cannot claim the operation id first.
    app.events.send(AppEvent::StartInit(prompt));
    CommandOutcome::handled(false)
}

fn compact(app: &mut App<'_>, arg: &str) -> CommandOutcome {
    let parts = arg.split_whitespace().collect::<Vec<_>>();
    match parts.as_slice() {
        [] => commands::start_compact(app),
        ["show"] => {
            let model_source = match &app.agent_loop.session.compact_model_override {
                crate::session::ModelOverride::Inherit => "global",
                crate::session::ModelOverride::Current => "session: current chat model",
                crate::session::ModelOverride::Model(_) => "session",
            };
            let tokens = app
                .effective_auto_compact_tokens()
                .map_or_else(|| "off".to_string(), |value| value.to_string());
            let tokens_source = match app.agent_loop.session.auto_compact_override {
                crate::session::AutoCompactOverride::Inherit => "global",
                _ => "session",
            };
            let keep_source = if app
                .agent_loop
                .session
                .compact_keep_recent_turns_override
                .is_some()
            {
                "session"
            } else {
                "global"
            };
            let reported = app.agent_loop.auto_compact.last_input_tokens.map_or_else(
                || "unavailable (provider has not reported usage)".to_string(),
                |tokens| tokens.to_string(),
            );
            let status = if app.agent_loop.auto_compact.active_id.is_some() {
                "running in background"
            } else {
                "idle"
            };
            app.agent_loop
                .session
                .conversation
                .lock()
                .unwrap()
                .add_info_string(format!(
                    "compact model: {} ({model_source})\n\
             auto compact input tokens: {tokens} ({tokens_source})\n\
             recent turns kept: {} ({keep_source})\n\
             last reported input tokens: {reported}\n\
             auto compact status: {status}",
                    app.effective_compact_model(),
                    app.effective_compact_keep_recent_turns()
                ));
            app.ui.conversation_panel.scroll_to_bottom();
        }
        ["set", "model", "default"] => {
            app.agent_loop.session.compact_model_override = crate::session::ModelOverride::Inherit;
            app.agent_loop
                .session
                .conversation
                .lock()
                .unwrap()
                .add_info_string("compact model now inherits the global setting");
            app.ui.conversation_panel.scroll_to_bottom();
            session::mark_dirty(app);
        }
        ["set", "model", "current"] => {
            app.agent_loop.session.compact_model_override = crate::session::ModelOverride::Current;
            app.agent_loop
                .session
                .conversation
                .lock()
                .unwrap()
                .add_info_string("compact model set to the current chat model for this session");
            app.ui.conversation_panel.scroll_to_bottom();
            session::mark_dirty(app);
        }
        ["set", "model", model] if app.provider_manager.resolve(model).is_some() => {
            app.agent_loop.session.compact_model_override =
                crate::session::ModelOverride::Model((*model).to_string());
            app.agent_loop
                .session
                .conversation
                .lock()
                .unwrap()
                .add_info_string(format!("compact model set to {model} for this session"));
            app.ui.conversation_panel.scroll_to_bottom();
            session::mark_dirty(app);
        }
        ["set", "model", model] => {
            app.agent_loop
                .session
                .conversation
                .lock()
                .unwrap()
                .add_error_string(format!("unknown provider/model: {model}"));
            app.ui.conversation_panel.scroll_to_bottom();
        }
        ["set", "tokens", "default"] => {
            app.agent_loop.session.auto_compact_override =
                crate::session::AutoCompactOverride::Inherit;
            app.agent_loop
                .session
                .conversation
                .lock()
                .unwrap()
                .add_info_string("auto compact threshold now inherits the global setting");
            app.ui.conversation_panel.scroll_to_bottom();
            session::mark_dirty(app);
        }
        ["set", "tokens", "off"] => {
            app.agent_loop.session.auto_compact_override =
                crate::session::AutoCompactOverride::Disabled;
            app.agent_loop
                .session
                .conversation
                .lock()
                .unwrap()
                .add_info_string("automatic context compaction disabled for this session");
            app.ui.conversation_panel.scroll_to_bottom();
            session::mark_dirty(app);
        }
        ["set", "tokens", value] => match value.parse::<u32>() {
            Ok(tokens) if tokens > 0 => {
                app.agent_loop.session.auto_compact_override =
                    crate::session::AutoCompactOverride::Tokens(tokens);
                app.agent_loop.session.conversation.lock().unwrap().add_info_string(format!(
                "automatic context compaction threshold set to {tokens} input tokens for this session"
                ));
                app.ui.conversation_panel.scroll_to_bottom();
                session::mark_dirty(app);
            }
            _ => {
                app.agent_loop
                    .session
                    .conversation
                    .lock()
                    .unwrap()
                    .add_error_string("compact token threshold must be a positive integer");
                app.ui.conversation_panel.scroll_to_bottom();
            }
        },
        ["set", "keep", "default"] => {
            app.agent_loop.session.compact_keep_recent_turns_override = None;
            app.agent_loop
                .session
                .conversation
                .lock()
                .unwrap()
                .add_info_string("recent-turn retention now inherits the global setting");
            app.ui.conversation_panel.scroll_to_bottom();
            session::mark_dirty(app);
        }
        ["set", "keep", value] => match value.parse::<usize>() {
            Ok(keep) => {
                app.agent_loop.session.compact_keep_recent_turns_override = Some(keep);
                app.agent_loop
                    .session
                    .conversation
                    .lock()
                    .unwrap()
                    .add_info_string(format!(
                        "compaction will keep {keep} recent complete turn(s) for this session"
                    ));
                app.ui.conversation_panel.scroll_to_bottom();
                session::mark_dirty(app);
            }
            Err(_) => {
                app.agent_loop
                    .session
                    .conversation
                    .lock()
                    .unwrap()
                    .add_error_string("compact keep value must be a non-negative integer");
                app.ui.conversation_panel.scroll_to_bottom();
            }
        },
        _ => {
            app.agent_loop.session.conversation.lock().unwrap().add_error_string(
            "usage: /compact [show | set model <provider/model|current|default> | set tokens <positive integer|off|default> | set keep <non-negative integer|default>]",
        );
            app.ui.conversation_panel.scroll_to_bottom();
        }
    }
    CommandOutcome::handled(false)
}

async fn plan(app: &mut App<'_>, arg: &str) -> CommandOutcome {
    match arg.trim().to_lowercase().as_str() {
        "approve" | "ok" | "go"
            if app.agent_loop.session.work_mode == WorkMode::Plan
                && app.agent_loop.plan_phase == PlanPhase::Reviewing =>
        {
            app.agent_loop.session.work_mode = WorkMode::Auto;
            app.agent_loop.plan_phase = PlanPhase::default();
            app.agent_loop
                .session
                .conversation
                .lock()
                .unwrap()
                .add_info_string("Plan approved — executing with Auto mode.");
            app.ui.conversation_panel.scroll_to_bottom();
            session::save_session(app);
            let instruction =
                "The plan was approved by the user. Execute it now using the identified steps.";
            commands::start_request_as(app, instruction.to_string(), InputRole::Developer).await;
        }
        "approve" | "ok" | "go" => {
            app.agent_loop
                .session
                .conversation
                .lock()
                .unwrap()
                .add_info_string("No plan pending approval. Use /mode plan to enter Plan mode.");
            app.ui.conversation_panel.scroll_to_bottom();
        }
        "cancel" | "abort" => {
            app.agent_loop.session.work_mode = WorkMode::Auto;
            app.agent_loop.plan_phase = PlanPhase::default();
            app.agent_loop
                .session
                .conversation
                .lock()
                .unwrap()
                .add_info_string("Plan cancelled — returned to Auto mode.");
            app.ui.conversation_panel.scroll_to_bottom();
            session::persist_config(app);
        }
        _ => {
            app.agent_loop
                .session
                .conversation
                .lock()
                .unwrap()
                .add_info_string(
                    "usage: /plan approve — approve current plan and execute\n\
             \u{20}      /plan cancel — cancel plan and return to Auto",
                );
            app.ui.conversation_panel.scroll_to_bottom();
        }
    }
    CommandOutcome::handled(true)
}
