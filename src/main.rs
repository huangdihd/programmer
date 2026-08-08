// Copyright (C) 2026 huangdihd
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

use crate::config::programmer_config::ProgrammerConfig;
use crate::session::SessionManager;
use ::config::{Config, Environment, File};
use app::App;
use std::path::Path;

mod agents;
mod app;
mod cancel;
mod classifier;
mod cli;
mod clipboard;
mod commands;
mod config;
mod consts;
mod conversation;
mod diagnostics;
mod headless;
mod mcp;
mod prompts;
mod providers;
mod response;
mod runner;
mod security;
mod session;
mod skills;
mod tasks;
mod terminal;
mod thinking;
mod todos;
mod tools;
mod ui;
mod upgrade;

/// Build the `(client, model)` the MCP server's `auto` mode uses to classify
/// tool calls: the configured classifier model, else the default model. Returns
/// `None` when no provider resolves (auto mode then refuses dangerous tools).
async fn build_mcp_classifier() -> Option<(
    async_openai::Client<async_openai::config::OpenAIConfig>,
    String,
)> {
    let (config, _) = load_config().ok()?;
    let pm = crate::providers::ProviderManager::new(&config).await;
    let model = config
        .classifier_model
        .clone()
        .unwrap_or_else(|| pm.default_classifier_model());
    pm.resolve(&model)
        .map(|(client, name)| (client.clone(), name))
}

/// Resolved session data ready for the application.
struct SessionBootstrap {
    uuid: String,
    items: Vec<crate::response::message_item::MessageItem>,
    history: Vec<String>,
    todos: Vec<crate::todos::Todo>,
    mgr: Option<SessionManager>,
    messages: Vec<String>,
}

fn resolve_session(resume: Option<Option<String>>) -> SessionBootstrap {
    let session_mgr = SessionManager::new();
    let mut startup_messages: Vec<String> = Vec::new();

    let (session_uuid, saved_items, saved_history, saved_todos) = match (resume, &session_mgr) {
        (Some(Some(uuid)), Some(mgr)) => match mgr.load(&uuid) {
            Some(session) => {
                let history = session.history.clone();
                let todos = session.todos.clone();
                tasks::restore(&session.tasks);
                let items = SessionManager::into_items(session);
                (uuid, items, history, todos)
            }
            None => {
                startup_messages.push(format!("Session {uuid} not found, creating a new session."));
                let session = mgr.create();
                (session.uuid, Vec::new(), Vec::new(), Vec::new())
            }
        },
        (Some(None), Some(mgr)) => match mgr.list_all() {
            Ok(sessions) => {
                let was_empty = sessions.is_empty();
                match session::pick_session(&sessions, mgr) {
                    Some(uuid) => match mgr.load(&uuid) {
                        Some(session) => {
                            let history = session.history.clone();
                            let todos = session.todos.clone();
                            tasks::restore(&session.tasks);
                            let items = SessionManager::into_items(session);
                            (uuid, items, history, todos)
                        }
                        None => {
                            startup_messages.push(format!(
                                "Session {uuid} not found on disk, starting a new session."
                            ));
                            let session = mgr.create();
                            (session.uuid, Vec::new(), Vec::new(), Vec::new())
                        }
                    },
                    None => {
                        if was_empty {
                            startup_messages.push(
                                "No existing sessions found, creating a new one.".to_string(),
                            );
                        }
                        let session = mgr.create();
                        (session.uuid, Vec::new(), Vec::new(), Vec::new())
                    }
                }
            }
            Err(e) => {
                startup_messages.push(format!(
                    "Failed to list sessions: {e}, creating new session."
                ));
                if let Some(mgr) = session_mgr.as_ref() {
                    let session = mgr.create();
                    (session.uuid, Vec::new(), Vec::new(), Vec::new())
                } else {
                    (String::new(), Vec::new(), Vec::new(), Vec::new())
                }
            }
        },
        _ => {
            if let Some(mgr) = &session_mgr {
                let session = mgr.create();
                (session.uuid, Vec::new(), Vec::new(), Vec::new())
            } else {
                startup_messages.push("Session persistence unavailable.".to_string());
                (String::new(), Vec::new(), Vec::new(), Vec::new())
            }
        }
    };

    SessionBootstrap {
        uuid: session_uuid,
        items: saved_items,
        history: saved_history,
        todos: saved_todos,
        mgr: session_mgr,
        messages: startup_messages,
    }
}

fn load_config() -> color_eyre::Result<(ProgrammerConfig, std::path::PathBuf)> {
    let config_path = dirs::config_dir()
        .map(|d| d.join("programmer").join("config.toml"))
        .unwrap_or_else(|| Path::new("config.toml").to_path_buf());

    let mut programmer_config: ProgrammerConfig = Config::builder()
        .add_source(File::with_name(config_path.to_str().unwrap()).required(false))
        .add_source(Environment::with_prefix("Programmer"))
        .build()
        .unwrap_or_default()
        .try_deserialize()?;

    if programmer_config.migrate_if_needed() || !config_path.exists() {
        // First run on a fresh machine: the config directory doesn't exist yet
        // and `fs::write` won't create parents.
        if let Some(parent) = config_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&config_path, toml::to_string(&programmer_config)?)?;
    }

    Ok((programmer_config, config_path))
}

fn main() -> color_eyre::Result<()> {
    #[cfg(windows)]
    crate::tasks::harden_dll_search();
    crate::security::sandbox::run_worker_if_requested();
    let args = cli::Args::parse_and_validate();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async_main(args))
}

async fn async_main(mut args: cli::Args) -> color_eyre::Result<()> {
    if let Some(command) = args.command.take() {
        let passed = match command {
            cli::Command::Run(run) => headless::run(run).await?,
            cli::Command::Init(init) => headless::init(init).await?,
            cli::Command::Diagnostics(diagnostics) => headless::diagnostics(diagnostics).await?,
            cli::Command::Mcp(mcp) => {
                run_mcp(mcp).await?;
                true
            }
            cli::Command::Upgrade(upgrade_args) => crate::upgrade::upgrade(upgrade_args).await?,
            cli::Command::Uninstall(uninstall_args) => {
                crate::upgrade::uninstall(uninstall_args).await?
            }
        };
        if !passed {
            std::process::exit(1);
        }
        return Ok(());
    }

    let resume = match args.resume {
        Some(uuid) if uuid.is_empty() => Some(None),
        Some(uuid) => Some(Some(uuid)),
        None if args.session => Some(None),
        None => None,
    };

    let bootstrap = resolve_session(resume);
    let (programmer_config, _config_path) = load_config()?;
    crate::security::SecurityManager::for_current_dir(programmer_config.security.clone())
        .map_err(|error| color_eyre::eyre::eyre!(error))?;

    // Derive a project name from the current directory for the terminal title.
    let project_name = std::env::current_dir()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "programmer".to_string());

    // ---- run the TUI ----
    let final_uuid;
    let result;
    {
        let (_guard, terminal) = terminal::TerminalGuard::enter(&project_name)?;
        (result, final_uuid) = App::new(
            programmer_config,
            bootstrap.items,
            bootstrap.history,
            bootstrap.todos,
            bootstrap.uuid,
            bootstrap.mgr,
            bootstrap.messages,
            args.providers,
            project_name,
        )
        .await
        .run(terminal)
        .await;
        // Guard drops here → terminal restored.
    }

    if let Some(final_uuid) = final_uuid {
        println!("Session saved. Resume with: programmer --resume {final_uuid}");
    }

    result
}

async fn run_mcp(args: cli::McpArgs) -> color_eyre::Result<()> {
    match args.command {
        cli::McpCommand::Stdio(args) => {
            headless::set_working_directory(args.cwd.as_deref())?;
            let classifier = build_mcp_classifier().await;
            let (config, _) = load_config()?;
            let security = install_security_for_current_dir(&config)?;
            mcp::server::McpServer::with_security(args.work_mode, classifier, security)
                .run()
                .await?;
        }
        cli::McpCommand::Http(args) => {
            headless::set_working_directory(args.cwd.as_deref())?;
            let classifier = build_mcp_classifier().await;
            let config = load_config().map(|(config, _)| config).unwrap_or_default();
            let allow_yolo = config.allow_yolo;
            let security = install_security_for_current_dir(&config)?;
            mcp::http_server::serve(args.work_mode, classifier, args.addr, allow_yolo, security)
                .await?;
        }
    }
    Ok(())
}

fn install_security_for_current_dir(
    config: &ProgrammerConfig,
) -> color_eyre::Result<std::sync::Arc<crate::security::SecurityManager>> {
    let security = crate::security::SecurityManager::for_current_dir(config.security.clone())
        .map_err(|error| color_eyre::eyre::eyre!(error))?;
    let security = std::sync::Arc::new(security);
    crate::security::install_active(security.clone());
    Ok(security)
}
