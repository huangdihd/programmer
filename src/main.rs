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

pub mod app;
pub mod classifier;
pub mod clipboard;
pub mod commands;
pub mod config;
pub mod diagnostics;
pub mod mcp;
pub mod prompts;
pub mod providers;
pub mod response;
pub mod session;
pub mod skills;
pub mod tasks;
pub mod terminal;
pub mod todos;
pub mod tools;
mod ui;

/// Parsed command-line arguments.
struct Args {
    resume: Option<Option<String>>,
    help: bool,
    session: bool,
    providers: bool,
}

const HELP_TEXT: &str = "\
programmer — a coding agent in your terminal

Usage: programmer [OPTIONS]

Options:
  --resume [uuid]   Resume a saved session; without a uuid, opens the
                    session management panel to pick one
  --session         Open the session management panel
  --providers       Open the provider management panel on startup
  -h, --help        Show this help and exit";

fn parse_args() -> Args {
    let args: Vec<String> = std::env::args().collect();
    let mut parsed = Args {
        resume: None,
        help: false,
        session: false,
        providers: false,
    };
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--resume" => {
                if i + 1 < args.len() && !args[i + 1].starts_with('-') {
                    parsed.resume = Some(Some(args[i + 1].clone()));
                    i += 1;
                } else {
                    parsed.resume = Some(None);
                }
            }
            "--session" => parsed.session = true,
            "--providers" => parsed.providers = true,
            "-h" | "--help" => parsed.help = true,
            _ => {}
        }
        i += 1;
    }
    parsed
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

fn resolve_session(
    resume: Option<Option<String>>,
) -> SessionBootstrap {
    let session_mgr = SessionManager::new();
    let mut startup_messages: Vec<String> = Vec::new();

    let (session_uuid, saved_items, saved_history, saved_todos) = match (resume, &session_mgr) {
        (Some(Some(uuid)), Some(mgr)) => match mgr.load(&uuid) {
            Some(session) => {
                let history = session.history.clone();
                let todos = session.todos.clone();
                let items = SessionManager::into_items(session);
                (uuid, items, history, todos)
            }
            None => {
                startup_messages
                    .push(format!("Session {uuid} not found, creating a new session."));
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
                            startup_messages
                                .push("No existing sessions found, creating a new one."
                                    .to_string());
                        }
                        let session = mgr.create();
                        (session.uuid, Vec::new(), Vec::new(), Vec::new())
                    }
                }
            }
            Err(e) => {
                startup_messages
                    .push(format!("Failed to list sessions: {e}, creating new session."));
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
                startup_messages
                    .push("Session persistence unavailable.".to_string());
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

    if programmer_config.migrate_if_needed() {
        std::fs::write(&config_path, toml::to_string(&programmer_config)?)?;
    }
    if !config_path.exists() {
        std::fs::write(&config_path, toml::to_string(&programmer_config)?)?;
    }

    Ok((programmer_config, config_path))
}

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    let args = parse_args();

    if args.help {
        println!("{HELP_TEXT}");
        return Ok(());
    }

    let resume = if args.session && args.resume.is_none() {
        Some(None)
    } else {
        args.resume
    };

    let bootstrap = resolve_session(resume);
    let has_session_mgr = bootstrap.mgr.is_some();
    let (programmer_config, _config_path) = load_config()?;

    // ---- run the TUI ----
    let final_uuid;
    let result;
    {
        let (_guard, terminal) = terminal::TerminalGuard::enter()?;
        (result, final_uuid) = App::new(
            programmer_config,
            bootstrap.items,
            bootstrap.history,
            bootstrap.todos,
            bootstrap.uuid,
            bootstrap.mgr,
            bootstrap.messages,
            args.providers,
        )
        .await
        .run(terminal)
        .await;
        // Guard drops here → terminal restored.
    }

    if has_session_mgr && !final_uuid.is_empty() {
        println!("Session saved. Resume with: programmer --resume {final_uuid}");
    }

    result
}
