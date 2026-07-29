// Copyright (C) 2026 huangdihd
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

use super::CommandOutcome;
use crate::app::{App, session};
use crate::classifier::WorkMode;
use crate::commands::Command;
use crate::thinking::ThinkingLevel;

pub(in crate::app) fn execute(app: &mut App<'_>, command: Command) -> CommandOutcome {
    app.input_panel.clear();
    match command {
        Command::Model(name) => model(app, name),
        Command::Vision(arg) => vision(app, &arg),
        Command::Mode(arg) => mode(app, &arg),
        Command::Classifier(arg) => classifier(app, &arg),
        Command::Thinking(arg) => thinking(app, &arg),
        _ => unreachable!("settings handler received a command from another domain"),
    }
}

fn model(app: &mut App<'_>, model: String) -> CommandOutcome {
    let model = model.trim().to_string();
    if model.is_empty() {
        app.conversation_panel
            .add_info_string("usage: /model <provider/model> — e.g. /model openai/gpt-4o");
        return CommandOutcome::without_history(true);
    }

    match app.provider_manager.resolve(&model) {
        Some(_) => {
            app.current_model = model;
            app.conversation_panel
                .add_info_string(format!("switched to model: {}", app.current_model));
        }
        None => {
            app.conversation_panel.add_error_string(format!(
                "unknown provider/model: {model} — use /providers to list available",
            ));
        }
    }
    CommandOutcome::handled(true)
}

fn vision(app: &mut App<'_>, arg: &str) -> CommandOutcome {
    match arg.trim().to_ascii_lowercase().as_str() {
        "on" => {
            app.vision_enabled = true;
            app.conversation_panel.add_info_string(
                "Vision enabled for this session. Reference images with @path.".to_string(),
            );
        }
        "off" => {
            let count = app.conversation_panel.image_count();
            app.vision_enabled = false;
            let suffix = if count == 0 {
                String::new()
            } else {
                format!(
                    " {count} stored image(s) will be omitted from future requests until vision is enabled again."
                )
            };
            app.conversation_panel
                .add_info_string(format!("Vision disabled for this session.{suffix}"));
        }
        "" => {
            let state = if app.vision_enabled { "on" } else { "off" };
            app.conversation_panel.add_info_string(format!(
                "Vision is {state} for this session. Usage: /vision <on|off>"
            ));
        }
        other => {
            app.conversation_panel.add_error_string(format!(
                "unknown vision setting '{other}' — use /vision on or /vision off"
            ));
        }
    }
    CommandOutcome::handled(true)
}

fn mode(app: &mut App<'_>, arg: &str) -> CommandOutcome {
    let previous = app.work_mode;
    match arg.trim().to_lowercase().as_str() {
        "manual" => app.work_mode = WorkMode::Manual,
        "auto" => app.work_mode = WorkMode::Auto,
        "plan" => app.work_mode = WorkMode::Plan,
        "yolo" if app.config.allow_yolo => app.work_mode = WorkMode::Yolo,
        "yolo" => {
            app.conversation_panel.add_error_string(
                "YOLO mode runs every tool call unchecked and is \
                 disabled by default — set `allow_yolo = true` in \
                 config to enable it"
                    .to_string(),
            );
            return CommandOutcome::without_history(false);
        }
        "" => app.work_mode = app.work_mode.next(app.config.allow_yolo),
        other => {
            app.conversation_panel.add_error_string(format!(
                "unknown mode '{other}' — use manual, auto, plan, or yolo"
            ));
            return CommandOutcome::without_history(false);
        }
    }
    let message = if app.work_mode == previous {
        format!("work mode unchanged: {}", app.work_mode.label())
    } else {
        format!("work mode set to: {}", app.work_mode.label())
    };
    app.conversation_panel.add_info_string(message);
    CommandOutcome::handled(true)
}

fn classifier(app: &mut App<'_>, arg: &str) -> CommandOutcome {
    match arg.trim() {
        "" => {
            let current = app
                .config
                .classifier_model
                .clone()
                .unwrap_or_else(|| format!("{} (chat model)", app.current_model));
            app.conversation_panel.add_info_string(format!(
                "classifier model: {current}\n\
                 usage: /classifier <provider/model> to set, \
                 /classifier clear to reset to the chat model"
            ));
        }
        "clear" | "default" | "reset" => {
            app.config.classifier_model = None;
            app.conversation_panel
                .add_info_string("classifier model reset — Auto mode now uses the chat model");
            session::persist_config(app);
        }
        model => match app.provider_manager.resolve(model) {
            Some(_) => {
                app.config.classifier_model = Some(model.to_string());
                app.conversation_panel
                    .add_info_string(format!("classifier model set to: {model}"));
                session::persist_config(app);
            }
            None => {
                app.conversation_panel.add_error_string(format!(
                    "unknown provider/model: {model} — use /providers to list available"
                ));
            }
        },
    }
    CommandOutcome::handled(true)
}

fn thinking(app: &mut App<'_>, arg: &str) -> CommandOutcome {
    let arg = arg.trim();
    if arg.is_empty() {
        app.conversation_panel.add_info_string(format!(
            "thinking level: {}\nusage: /thinking <{}>",
            app.thinking_level.label(),
            ThinkingLevel::VALUES
        ));
    } else if let Some(level) = ThinkingLevel::parse(arg) {
        app.thinking_level = level;
        let detail = if level == ThinkingLevel::Auto {
            "provider/model default; reasoning.effort is omitted"
        } else {
            "sent explicitly as reasoning.effort"
        };
        app.conversation_panel.add_info_string(format!(
            "thinking level set to: {} ({detail})",
            level.label()
        ));
    } else {
        app.conversation_panel.add_error_string(format!(
            "unknown thinking level '{arg}' — use {}",
            ThinkingLevel::VALUES
        ));
    }
    CommandOutcome::handled(true)
}
