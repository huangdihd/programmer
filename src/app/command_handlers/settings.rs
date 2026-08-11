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
use crate::config::programmer_config::validate_security_profile_name;
use crate::security::SandboxMode;
use crate::security::SecurityConfig;
use crate::thinking::ThinkingLevel;
use crate::ui::components::security_panel::SecurityPanel;
use std::path::PathBuf;

pub(in crate::app) fn execute(app: &mut App<'_>, command: Command) -> CommandOutcome {
    app.input_panel.clear();
    match command {
        Command::Model(name) => model(app, name),
        Command::Vision(arg) => vision(app, &arg),
        Command::Select(arg) => select(app, &arg),
        Command::Mode(arg) => mode(app, &arg),
        Command::Classifier(arg) => classifier(app, &arg),
        Command::Thinking(arg) => thinking(app, &arg),
        Command::Permission(arg) => permission(app, &arg),
        _ => unreachable!("settings handler received a command from another domain"),
    }
}

fn select(app: &mut App<'_>, arg: &str) -> CommandOutcome {
    let enabled = match selection_mode_target(app.native_selection_mode, arg) {
        Ok(enabled) => enabled,
        Err(error) => {
            app.conversation_panel.add_error_string(error);
            return CommandOutcome::handled(false);
        }
    };
    if let Err(error) = crate::terminal::set_mouse_capture(!enabled) {
        app.conversation_panel
            .add_error_string(format!("could not change terminal selection mode: {error}"));
        return CommandOutcome::handled(false);
    }

    app.native_selection_mode = enabled;
    let message = if enabled {
        "Selection mode enabled. Drag to select text and use your terminal's copy shortcut; mouse scrolling and clicks are paused. Run /select off to restore them."
    } else {
        "Selection mode disabled. Mouse scrolling and clicks are restored."
    };
    app.conversation_panel.add_info_string(message);
    CommandOutcome::handled(false)
}

fn selection_mode_target(current: bool, argument: &str) -> Result<bool, String> {
    match argument.trim().to_ascii_lowercase().as_str() {
        "" => Ok(!current),
        "on" => Ok(true),
        "off" => Ok(false),
        other => Err(format!(
            "unknown selection setting '{other}' — use /select, /select on, or /select off"
        )),
    }
}

fn permission(app: &mut App<'_>, arg: &str) -> CommandOutcome {
    let parts = arg.split_whitespace().collect::<Vec<_>>();
    if matches!(parts.as_slice(), ["manage"]) {
        app.security_panel = Some(SecurityPanel::new());
        return CommandOutcome::handled(false);
    }
    if matches!(parts.first(), Some(&"profile") | Some(&"profiles")) {
        return permission_profile(app, &parts[1..]);
    }

    let previous_profile = app.config.active_security_profile.clone();
    let previous_security = app.config.security.clone();
    let mut updated = app.config.security.clone();
    match update_security_config(&mut updated, arg) {
        Ok(SecurityUpdate::Show) => {
            app.conversation_panel.add_info_string(format!(
                "Active security profile: {}\n{}",
                app.config.active_security_profile,
                app.security.status_text()
            ));
        }
        Ok(SecurityUpdate::Changed(message)) => {
            app.config
                .update_active_security(|config| *config = updated);
            match app.install_active_security() {
                Ok(()) => {
                    session::persist_config(app);
                    app.conversation_panel.add_info_string(format!(
                        "{message} in profile '{}'",
                        app.config.active_security_profile
                    ));
                }
                Err(error) => {
                    restore_active_security(&mut app.config, previous_profile, previous_security);
                    app.conversation_panel
                        .add_error_string(format!("invalid security configuration: {error}"));
                }
            }
        }
        Err(error) => app.conversation_panel.add_error_string(error),
    }
    CommandOutcome::handled(true)
}

fn permission_profile(app: &mut App<'_>, args: &[&str]) -> CommandOutcome {
    let previous_profile = app.config.active_security_profile.clone();
    let previous_security = app.config.security.clone();
    let result = update_security_profiles(&mut app.config, args);
    match result {
        Ok(ProfileUpdate::Show(message)) => app.conversation_panel.add_info_string(message),
        Ok(ProfileUpdate::Saved(message)) => {
            session::persist_config(app);
            app.conversation_panel.add_info_string(message);
        }
        Ok(ProfileUpdate::Apply(message)) => match app.install_active_security() {
            Ok(()) => {
                session::persist_config(app);
                app.conversation_panel.add_info_string(message);
            }
            Err(error) => {
                restore_active_security(&mut app.config, previous_profile, previous_security);
                app.conversation_panel
                    .add_error_string(format!("invalid security configuration: {error}"));
            }
        },
        Err(error) => {
            app.conversation_panel.add_error_string(error);
        }
    }
    CommandOutcome::handled(true)
}

fn restore_active_security(
    config: &mut crate::config::programmer_config::ProgrammerConfig,
    profile: String,
    security: SecurityConfig,
) {
    config.active_security_profile = profile.clone();
    config.security_profiles.insert(profile, security.clone());
    config.security = security;
}

#[derive(Debug, PartialEq, Eq)]
enum ProfileUpdate {
    Show(String),
    Saved(String),
    Apply(String),
}

fn update_security_profiles(
    config: &mut crate::config::programmer_config::ProgrammerConfig,
    args: &[&str],
) -> Result<ProfileUpdate, String> {
    match args {
        [] | ["list"] | ["show"] => {
            let lines = config
                .security_profiles
                .iter()
                .map(|(name, profile)| {
                    format!(
                        "  {} {name} ({})",
                        if config.active_security_profile == *name {
                            "●"
                        } else {
                            " "
                        },
                        SandboxMode::from_config(&profile.sandbox).label()
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            Ok(ProfileUpdate::Show(format!(
                "Security profiles:\n{lines}\nUse /permission manage to edit profiles."
            )))
        }
        ["use", name] | ["switch", name] => {
            config.activate_security_profile(name)?;
            Ok(ProfileUpdate::Apply(format!(
                "active security profile set to '{name}'"
            )))
        }
        ["create", name] | ["clone", name] => create_security_profile(config, name, None),
        ["create", name, source] | ["clone", name, source] => {
            create_security_profile(config, name, Some(source))
        }
        ["delete", name] | ["remove", name] => {
            if !config.security_profiles.contains_key(*name) {
                return Err(format!("unknown security profile '{name}'"));
            }
            if config.active_security_profile == *name {
                return Err("activate another profile before deleting the active one".to_string());
            }
            if config.security_profiles.len() == 1 {
                return Err("the last security profile cannot be deleted".to_string());
            }
            config.security_profiles.remove(*name);
            Ok(ProfileUpdate::Saved(format!(
                "security profile '{name}' deleted"
            )))
        }
        ["rename", old, new] => {
            validate_security_profile_name(new)?;
            if old == new {
                if config.security_profiles.contains_key(*old) {
                    return Ok(ProfileUpdate::Saved(format!(
                        "security profile remains '{old}'"
                    )));
                }
                return Err(format!("unknown security profile '{old}'"));
            }
            if config.security_profiles.contains_key(*new) {
                return Err(format!("security profile '{new}' already exists"));
            }
            let profile = config
                .security_profiles
                .remove(*old)
                .ok_or_else(|| format!("unknown security profile '{old}'"))?;
            config.security_profiles.insert((*new).to_string(), profile);
            if config.active_security_profile == *old {
                config.active_security_profile = (*new).to_string();
            }
            Ok(ProfileUpdate::Saved(format!(
                "security profile '{old}' renamed to '{new}'"
            )))
        }
        _ => Err(permission_usage()),
    }
}

fn create_security_profile(
    config: &mut crate::config::programmer_config::ProgrammerConfig,
    name: &str,
    source: Option<&str>,
) -> Result<ProfileUpdate, String> {
    validate_security_profile_name(name)?;
    if config.security_profiles.contains_key(name) {
        return Err(format!("security profile '{name}' already exists"));
    }
    let source = source.unwrap_or(&config.active_security_profile);
    let profile = config
        .security_profiles
        .get(source)
        .cloned()
        .ok_or_else(|| format!("unknown security profile '{source}'"))?;
    config.security_profiles.insert(name.to_string(), profile);
    Ok(ProfileUpdate::Saved(format!(
        "security profile '{name}' cloned from '{source}'"
    )))
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
    if kind == "deny-env" {
        globset::Glob::new(value)
            .map_err(|error| format!("invalid environment name pattern '{value}': {error}"))?;
        return update_value(
            &mut config.sandbox.denied_environment,
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
    "usage: /permission show | /permission manage | \
     /permission profile <list|use|create|delete|rename> ... | \
     /permission mode <restricted|network|off> | \
     /permission reset | \
     /permission <filesystem|sandbox|network|system-read|temp-write|fail-closed|\
     file-protection|outside-read> <on|off> | \
     /permission <add|remove> <read|write|deny-read|deny-env> <value>"
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
    let parts = arg.split_whitespace().collect::<Vec<_>>();
    match parts.as_slice() {
        [] => {
            let current = app
                .config
                .classifier_model
                .clone()
                .unwrap_or_else(|| format!("{} (chat model)", app.current_model));
            app.conversation_panel.add_info_string(format!(
                "classifier model: {current}\n\
                 classifier top logprobs: {}\n\
                 usage: /classifier <provider/model> to set the model, \
                 /classifier logprobs <0-20> to set the fast-probe alternatives, \
                 /classifier clear to reset to the chat model",
                app.config.classifier_top_logprobs
            ));
        }
        ["clear" | "default" | "reset"] => {
            app.config.classifier_model = None;
            app.conversation_panel
                .add_info_string("classifier model reset — Auto mode now uses the chat model");
            session::persist_config(app);
        }
        ["logprobs" | "top-logprobs" | "top_logprobs", value] => {
            match parse_classifier_top_logprobs(value) {
                Ok(top_logprobs) => {
                    app.config.classifier_top_logprobs = top_logprobs;
                    app.conversation_panel
                        .add_info_string(format!("classifier top logprobs set to: {top_logprobs}"));
                    session::persist_config(app);
                }
                Err(error) => app.conversation_panel.add_error_string(error),
            }
        }
        ["logprobs" | "top-logprobs" | "top_logprobs"] => app
            .conversation_panel
            .add_error_string("usage: /classifier logprobs <0-20>".to_string()),
        [model] => match app.provider_manager.resolve(model) {
            Some(_) => {
                app.config.classifier_model = Some((*model).to_string());
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
        _ => app.conversation_panel.add_error_string(
            "usage: /classifier [provider/model | clear | logprobs <0-20>]".to_string(),
        ),
    }
    CommandOutcome::handled(true)
}

fn parse_classifier_top_logprobs(value: &str) -> Result<u8, String> {
    let value = value
        .parse::<u8>()
        .map_err(|_| format!("invalid top logprobs '{value}': expected an integer from 0 to 20"))?;
    if value > crate::consts::MAX_CLASSIFIER_TOP_LOGPROBS {
        return Err(format!(
            "invalid top logprobs '{value}': expected an integer from 0 to {}",
            crate::consts::MAX_CLASSIFIER_TOP_LOGPROBS
        ));
    }
    Ok(value)
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
    fn classifier_top_logprobs_parser_enforces_api_range() {
        assert_eq!(parse_classifier_top_logprobs("0"), Ok(0));
        assert_eq!(parse_classifier_top_logprobs("5"), Ok(5));
        assert_eq!(parse_classifier_top_logprobs("20"), Ok(20));
        assert!(parse_classifier_top_logprobs("21").is_err());
        assert!(parse_classifier_top_logprobs("five").is_err());
    }

    #[test]
    fn selection_mode_toggles_and_accepts_explicit_states() {
        assert_eq!(selection_mode_target(false, ""), Ok(true));
        assert_eq!(selection_mode_target(true, ""), Ok(false));
        assert_eq!(selection_mode_target(false, "ON"), Ok(true));
        assert_eq!(selection_mode_target(true, "off"), Ok(false));
        assert!(selection_mode_target(false, "maybe").is_err());
    }

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
    fn profile_commands_clone_and_switch_policies() {
        let mut config = crate::config::programmer_config::ProgrammerConfig::default();
        config.normalize_security_profiles();
        config.update_active_security(|profile| profile.sandbox.network = true);

        assert!(matches!(
            update_security_profiles(&mut config, &["create", "online"]),
            Ok(ProfileUpdate::Saved(_))
        ));
        assert!(config.security_profiles["online"].sandbox.network);
        assert!(matches!(
            update_security_profiles(&mut config, &["use", "online"]),
            Ok(ProfileUpdate::Apply(_))
        ));
        assert_eq!(config.active_security_profile, "online");
        assert_eq!(config.security, config.security_profiles["online"]);
    }

    #[test]
    fn profile_commands_protect_active_profile_from_deletion() {
        let mut config = crate::config::programmer_config::ProgrammerConfig::default();
        config.normalize_security_profiles();
        config
            .security_profiles
            .insert("other".to_string(), SecurityConfig::default());

        let error = update_security_profiles(&mut config, &["delete", "default"])
            .expect_err("active profile deletion must fail");

        assert!(error.contains("activate another profile"));
        assert!(config.security_profiles.contains_key("default"));
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

        update_security_config(&mut config, "add deny-env SECRET_*")
            .expect("add environment blacklist pattern");
        assert_eq!(config.sandbox.denied_environment, ["SECRET_*"]);
        update_security_config(&mut config, "remove deny-env SECRET_*")
            .expect("remove environment blacklist pattern");
        assert!(config.sandbox.denied_environment.is_empty());
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
