// Copyright (C) 2026 huangdihd
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

use super::CommandOutcome;
use crate::app::{App, session};
use crate::classifier::WorkMode;
use crate::commands::{Command, PERMISSION_BOOLEAN_SETTINGS, PERMISSION_COLLECTION_KINDS};
use crate::security::SandboxMode;
use crate::security::SecurityConfig;
use crate::thinking::ThinkingLevel;
use std::path::PathBuf;
use std::sync::Arc;

pub(in crate::app) fn execute(app: &mut App<'_>, command: Command) -> CommandOutcome {
    app.input_panel.clear();
    match command {
        Command::Model(name) => model(app, name),
        Command::Vision(arg) => vision(app, &arg),
        Command::Mode(arg) => mode(app, &arg),
        Command::Classifier(arg) => classifier(app, &arg),
        Command::Thinking(arg) => thinking(app, &arg),
        Command::Permission(arg) => permission(app, &arg),
        _ => unreachable!("settings handler received a command from another domain"),
    }
}

fn permission(app: &mut App<'_>, arg: &str) -> CommandOutcome {
    let previous = app.config.security.clone();
    match update_security_config(&mut app.config.security, arg) {
        Ok(SecurityUpdate::Show) => {
            app.conversation_panel
                .add_info_string(app.security.status_text());
        }
        Ok(SecurityUpdate::Changed(message)) => {
            match crate::security::SecurityManager::for_current_dir(app.config.security.clone()) {
                Ok(security) => {
                    let security = Arc::new(security);
                    app.security.replace(security);
                    session::persist_config(app);
                    app.conversation_panel.add_info_string(message);
                }
                Err(error) => {
                    app.config.security = previous;
                    app.conversation_panel
                        .add_error_string(format!("invalid security configuration: {error}"));
                }
            }
        }
        Err(error) => app.conversation_panel.add_error_string(error),
    }
    CommandOutcome::handled(true)
}

#[derive(Debug)]
enum SecurityUpdate {
    Show,
    Changed(String),
}

fn update_security_config(
    config: &mut SecurityConfig,
    argument: &str,
) -> Result<SecurityUpdate, String> {
    let parts = argument.split_whitespace().collect::<Vec<_>>();
    match parts.as_slice() {
        [] | ["show"] | ["status"] => Ok(SecurityUpdate::Show),
        ["reset"] => {
            *config = SecurityConfig::default();
            Ok(SecurityUpdate::Changed(
                "security settings reset to defaults".to_string(),
            ))
        }
        ["mode", value] => {
            let mode = SandboxMode::parse(value).ok_or_else(|| {
                format!(
                    "unknown sandbox mode '{value}' — use {}",
                    SandboxMode::VALUES.join(", ")
                )
            })?;
            mode.apply(&mut config.sandbox);
            Ok(SecurityUpdate::Changed(format!(
                "sandbox mode set to {}",
                mode.label()
            )))
        }
        [setting, value] if PERMISSION_BOOLEAN_SETTINGS.contains(setting) => {
            let enabled = parse_toggle(value)?;
            set_boolean_setting(config, setting, enabled);
            Ok(SecurityUpdate::Changed(format!(
                "security setting {setting} set to {}",
                if enabled { "on" } else { "off" }
            )))
        }
        [action @ ("add" | "remove"), kind, value] => {
            update_collection(config, action, kind, value)
        }
        _ => Err(permission_usage()),
    }
}

fn parse_toggle(value: &str) -> Result<bool, String> {
    match value.to_ascii_lowercase().as_str() {
        "on" | "true" | "enable" | "enabled" => Ok(true),
        "off" | "false" | "disable" | "disabled" => Ok(false),
        _ => Err(format!("expected on or off, got '{value}'")),
    }
}

fn set_boolean_setting(config: &mut SecurityConfig, setting: &str, enabled: bool) {
    match setting {
        "filesystem" => config.enabled = enabled,
        "sandbox" => config.sandbox.enabled = enabled,
        "network" => config.sandbox.network = enabled,
        "system-read" => config.sandbox.allow_system_read = enabled,
        "temp-write" => config.sandbox.allow_temp_write = enabled,
        "fail-closed" => config.sandbox.fail_closed = enabled,
        "file-protection" => config.protect_file_changes = enabled,
        "outside-read" => config.allow_read_outside_workspace = enabled,
        _ => unreachable!("setting was checked before dispatch"),
    }
}

fn update_collection(
    config: &mut SecurityConfig,
    action: &str,
    kind: &str,
    value: &str,
) -> Result<SecurityUpdate, String> {
    if !PERMISSION_COLLECTION_KINDS.contains(&kind) {
        return Err(format!("unknown permission collection '{kind}'"));
    }
    if kind == "env" {
        globset::Glob::new(value)
            .map_err(|error| format!("invalid environment name pattern '{value}': {error}"))?;
        return update_value(
            &mut config.sandbox.inherit_environment,
            action,
            value.to_string(),
            kind,
        );
    }

    let path = PathBuf::from(value);
    let values = match kind {
        "read" => &mut config.sandbox.readable_paths,
        "write" => &mut config.sandbox.writable_paths,
        "deny-read" => &mut config.sandbox.denied_read_paths,
        _ => unreachable!("collection kind was checked before dispatch"),
    };
    update_value(values, action, path, kind)
}

fn update_value<T: PartialEq>(
    values: &mut Vec<T>,
    action: &str,
    value: T,
    kind: &str,
) -> Result<SecurityUpdate, String> {
    match action {
        "add" => {
            if !values.contains(&value) {
                values.push(value);
            }
        }
        "remove" => {
            let Some(index) = values.iter().position(|entry| entry == &value) else {
                return Err(format!("no matching {kind} entry is configured"));
            };
            values.remove(index);
        }
        _ => unreachable!("action was checked before dispatch"),
    }
    let verb = if action == "add" { "added" } else { "removed" };
    Ok(SecurityUpdate::Changed(format!(
        "security {kind} entry {verb}"
    )))
}

fn permission_usage() -> String {
    "usage: /permission show | /permission mode <restricted|network|off> | \
     /permission reset | \
     /permission <filesystem|sandbox|network|system-read|temp-write|fail-closed|\
     file-protection|outside-read> <on|off> | \
     /permission <add|remove> <read|write|deny-read|env> <value>"
        .to_string()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_updates_boolean_settings() {
        let mut config = SecurityConfig::default();

        update_security_config(&mut config, "network on").expect("enable network");
        update_security_config(&mut config, "file-protection off")
            .expect("disable file protection");

        assert!(config.sandbox.network);
        assert!(!config.protect_file_changes);
    }

    #[test]
    fn permission_mode_controls_sandbox_and_network_together() {
        let mut config = SecurityConfig::default();

        update_security_config(&mut config, "mode network").expect("network mode");
        assert!(config.sandbox.enabled);
        assert!(config.sandbox.network);

        update_security_config(&mut config, "mode off").expect("off mode");
        assert!(!config.sandbox.enabled);

        update_security_config(&mut config, "mode restricted").expect("restricted mode");
        assert!(config.sandbox.enabled);
        assert!(!config.sandbox.network);
    }

    #[test]
    fn permission_updates_collection_settings_without_duplicates() {
        let mut config = SecurityConfig::default();
        config.sandbox.readable_paths.clear();

        update_security_config(&mut config, "add read ../shared").expect("add path");
        update_security_config(&mut config, "add read ../shared").expect("repeat add");
        assert_eq!(
            config.sandbox.readable_paths,
            vec![PathBuf::from("../shared")]
        );

        update_security_config(&mut config, "remove read ../shared").expect("remove path");
        assert!(config.sandbox.readable_paths.is_empty());
    }

    #[test]
    fn permission_rejects_invalid_values_without_mutating_config() {
        let mut config = SecurityConfig::default();
        let original = config.sandbox.network;

        let error =
            update_security_config(&mut config, "network maybe").expect_err("invalid toggle");

        assert!(error.contains("expected on or off"));
        assert_eq!(config.sandbox.network, original);
    }
}
