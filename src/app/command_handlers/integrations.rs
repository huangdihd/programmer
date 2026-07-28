// Copyright (C) 2026 huangdihd
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

use super::CommandOutcome;
use crate::app::App;
use crate::commands::Command;
use crate::ui::components::mcp_panel::McpPanel;
use crate::ui::components::provider_panel::ProviderPanel;
use crate::ui::components::skills_panel::SkillsPanel;
use crate::ui::event::AppEvent;

pub(in crate::app) fn execute(app: &mut App<'_>, command: Command) -> CommandOutcome {
    app.input_panel.clear();
    match command {
        Command::Providers(arg) => providers(app, &arg),
        Command::Skill(arg) => skill(app, &arg),
        Command::Mcp(arg) => mcp(app, &arg),
        _ => unreachable!("integrations handler received a command from another domain"),
    }
    CommandOutcome::handled(true)
}

fn providers(app: &mut App<'_>, arg: &str) {
    match arg.trim() {
        "show" | "list" => {
            let names = app.provider_manager.provider_names();
            if names.is_empty() {
                app.conversation_panel
                    .add_info_string("no providers configured");
                return;
            }

            let mut lines = vec!["Configured providers:".to_string()];
            for name in names {
                lines.push(format!("  {name}:"));
                let models = app
                    .provider_manager
                    .models_for(name)
                    .iter()
                    .map(|model| format!("    {name}/{model}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                lines.push(models);
            }
            app.conversation_panel.add_info_string(lines.join("\n"));
        }
        "manage" => app.provider_panel = Some(ProviderPanel::new()),
        "refresh" => {
            app.conversation_panel
                .add_info_string("Refreshing provider model lists...");
            app.events.send(AppEvent::RefreshProviderModels);
        }
        _ => app.conversation_panel.add_info_string(
            "usage: /providers show — list configured providers\n\
             \u{20}      /providers manage — open the management panel\n\
             \u{20}      /providers refresh — refetch auto-discovered model lists",
        ),
    }
}

fn skill(app: &mut App<'_>, arg: &str) {
    match arg.trim() {
        "list" | "show" => {
            let names = app.skill_registry.names();
            if names.is_empty() {
                app.conversation_panel
                    .add_info_string("no skills configured");
                return;
            }

            let mut lines: Vec<String> = names
                .iter()
                .map(|name| {
                    let active = if app.skill_registry.is_active(name) {
                        " [active]"
                    } else {
                        ""
                    };
                    format!("  {name}{active}")
                })
                .collect();
            lines.insert(0, "Skills:".to_string());
            app.conversation_panel.add_info_string(lines.join("\n"));
        }
        "off" | "clear" | "none" => {
            app.skill_registry.clear();
            app.conversation_panel.add_info_string("skills deactivated");
        }
        "manage" => app.skills_panel = Some(SkillsPanel::new()),
        "" => {
            let active = app.skill_registry.activated_names().join(", ");
            if active.is_empty() {
                app.conversation_panel.add_info_string(
                    "no skills active — use /skill <name> to activate, \
                     /skill list to see available",
                );
            } else {
                app.conversation_panel
                    .add_info_string(format!("active skills: {active}"));
            }
        }
        name => {
            if app.skill_registry.activate(name) {
                app.conversation_panel
                    .add_info_string(format!("skill activated: {name}"));
            } else {
                app.conversation_panel
                    .add_error_string(format!("unknown skill: {name}"));
            }
        }
    }
}

fn mcp(app: &mut App<'_>, arg: &str) {
    match arg.trim() {
        "show" | "list" => {
            if app.config.mcp_servers.is_empty() {
                app.conversation_panel
                    .add_info_string("no MCP servers configured");
                return;
            }

            let mut lines: Vec<String> = app
                .config
                .mcp_servers
                .iter()
                .map(|server| {
                    let policy = match server.auto_approve {
                        crate::mcp::types::McpPolicy::Trusted => "trusted",
                        crate::mcp::types::McpPolicy::Review => "review",
                    };
                    format!("  {} ({}:{policy})", server.name, server.command)
                })
                .collect();
            lines.insert(0, "MCP servers:".to_string());
            app.conversation_panel.add_info_string(lines.join("\n"));
        }
        "manage" => app.mcp_panel = Some(McpPanel::new()),
        _ => app.conversation_panel.add_info_string(
            "usage: /mcp show — list MCP servers and their status\n\
             \u{20}      /mcp manage — open the management panel",
        ),
    }
}
