// Copyright (C) 2026 huangdihd
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

use super::CommandOutcome;
use crate::app::App;
use crate::commands::Command;
use crate::ui::components::diagnostics_panel::DiagnosticsPanel;
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
        Command::Diagnostics(arg) => diagnostics(app, &arg),
        _ => unreachable!("integrations handler received a command from another domain"),
    }
    CommandOutcome::handled(true)
}

fn diagnostics(app: &mut App<'_>, arg: &str) {
    match arg.trim() {
        "manage" => match DiagnosticsPanel::load() {
            Ok(panel) => app.diagnostics_panel = Some(panel),
            Err(error) => app
                .conversation_panel
                .add_error_string(format!("could not open diagnostics profile: {error}")),
        },
        "update" | "refresh" => crate::app::diagnostics::start_update(app, true),
        _ => app.conversation_panel.add_info_string(
            "usage: /diagnostics manage — edit .programmer/diagnostics.toml\n\
             \u{20}      /diagnostics update — re-run checkers and refresh current findings",
        ),
    }
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
            app.events.send(AppEvent::RefreshProviderModels {
                name: None,
                notify: true,
            });
        }
        arg if arg.starts_with("refresh ") => {
            let provider_name = arg["refresh ".len()..].trim().to_string();
            if provider_name.is_empty() {
                app.conversation_panel
                    .add_info_string("usage: /providers refresh [provider]");
                return;
            }
            app.events.send(AppEvent::RefreshProviderModels {
                name: Some(provider_name),
                notify: true,
            });
        }
        _ => app.conversation_panel.add_info_string(
            "usage: /providers show — list configured providers\n\
             \u{20}      /providers manage — open the management panel\n\
             \u{20}      /providers refresh [provider] — refetch auto-discovered model lists",
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
                    let connected = app
                        .mcp_manager
                        .as_deref()
                        .is_some_and(|manager| manager.has_server(&server.name));
                    format_mcp_server(server, connected)
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

fn format_mcp_server(server: &crate::mcp::types::McpServerConfig, connected: bool) -> String {
    let status = if connected {
        "connected"
    } else {
        "disconnected"
    };
    let transport = match &server.url {
        Some(_) => "HTTP".to_string(),
        None => format!("stdio: {}", server.command),
    };
    format!("  {} [{status}] ({transport})", server.name)
}

#[cfg(test)]
mod tests {
    use super::format_mcp_server;
    use crate::mcp::types::McpServerConfig;
    use std::collections::HashMap;

    fn server(command: &str, url: Option<&str>) -> McpServerConfig {
        McpServerConfig {
            name: "example".to_string(),
            command: command.to_string(),
            args: Vec::new(),
            env: HashMap::new(),
            url: url.map(str::to_string),
        }
    }

    #[test]
    fn mcp_status_formats_stdio_servers() {
        assert_eq!(
            format_mcp_server(&server("example-mcp", None), true),
            "  example [connected] (stdio: example-mcp)"
        );
    }

    #[test]
    fn mcp_status_formats_http_servers_without_an_empty_command() {
        assert_eq!(
            format_mcp_server(&server("", Some("https://example.com/mcp")), false),
            "  example [disconnected] (HTTP)"
        );
    }
}
