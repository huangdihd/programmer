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
use ::config::Config;
use ::config::Environment;
use ::config::File;
use app::App;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    supports_keyboard_enhancement,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::io;
use std::path::Path;

pub mod app;
pub mod classifier;
pub mod clipboard;
pub mod commands;
pub mod config;
pub mod diagnostics;
pub mod mcp;
pub mod providers;
pub mod response;
pub mod session;
pub mod skills;
pub mod todos;
pub mod tools;
mod ui;

/// Parsed command-line arguments.
struct Args {
    /// `--resume` with an optional UUID.
    resume: Option<Option<String>>,
    /// `--help`: print usage and exit.
    help: bool,
    /// `--session`: open the session management panel (same picker as bare
    /// `--resume`).
    session: bool,
    /// `--providers`: start with the provider management panel open.
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

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    let args = parse_args();

    if args.help {
        println!("{HELP_TEXT}");
        return Ok(());
    }

    // `--session` opens the same management panel as bare `--resume`.
    let resume = if args.session && args.resume.is_none() {
        Some(None)
    } else {
        args.resume
    };

    // ---- resolve which session to use ----
    let session_mgr = SessionManager::new();
    let mut startup_messages: Vec<String> = Vec::new();
    let (session_uuid, saved_items, saved_history, saved_todos) = match (resume, &session_mgr) {
        // --resume <uuid>
        (Some(Some(uuid)), Some(mgr)) => {
            match mgr.load(&uuid) {
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
            }
        }
        // --resume (no UUID) → TUI interactive picker
        (Some(None), Some(mgr)) => {
            match mgr.list_all() {
                Ok(sessions) => {
                    let was_empty = sessions.is_empty();
                    match session::pick_session(&sessions, mgr) {
                        Some(uuid) => {
                            match mgr.load(&uuid) {
                                Some(session) => {
                                    let history = session.history.clone();
                                    let todos = session.todos.clone();
                                    let items = SessionManager::into_items(session);
                                    (uuid, items, history, todos)
                                }
                                None => {
                                    // Listed but file missing — create new.
                                    startup_messages.push(format!(
                                        "Session {uuid} not found on disk, starting a new session."
                                    ));
                                    let session = mgr.create();
                                    (session.uuid, Vec::new(), Vec::new(), Vec::new())
                                }
                            }
                        }
                        None => {
                            // User chose "new" or no sessions exist.
                            if was_empty {
                                startup_messages.push(
                                    "No saved sessions found. Starting a new session.".into(),
                                );
                            }
                            let session = mgr.create();
                            (session.uuid, Vec::new(), Vec::new(), Vec::new())
                        }
                    }
                }
                Err(e) => {
                    startup_messages.push(format!("Warning: {e}"));
                    let session = mgr.create();
                    (session.uuid, Vec::new(), Vec::new(), Vec::new())
                }
            }
        }
        // No --resume flag and no session manager → fresh start
        _ => {
            let session = session_mgr.as_ref().map(|m| m.create());
            match session {
                Some(s) => (s.uuid, Vec::new(), Vec::new(), Vec::new()),
                None => (String::new(), Vec::new(), Vec::new(), Vec::new()),
            }
        }
    };

    // ---- configure ----
    let config_dir = dirs::config_dir().unwrap();
    let programmer_dir = config_dir.join("programmer");
    if !Path::new(programmer_dir.as_path()).exists() {
        std::fs::create_dir(&programmer_dir)?;
    }
    let config_path = programmer_dir.join("config.toml");
    color_eyre::install()?;
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    let keyboard_enhanced = supports_keyboard_enhancement().unwrap_or(false);
    if keyboard_enhanced {
        execute!(
            stdout,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
    }

    let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    let programmer_config = Config::builder()
        .add_source(File::with_name(config_path.as_path().to_str().unwrap()).required(false))
        .add_source(Environment::with_prefix("Programmer"))
        .build()
        .unwrap_or_default();

    let mut programmer_config: ProgrammerConfig = programmer_config.try_deserialize()?;

    if programmer_config.migrate_if_needed() {
        std::fs::write(&config_path, toml::to_string(&programmer_config)?)?;
    }

    if !Path::new(config_path.as_path()).exists() {
        std::fs::write(config_path, toml::to_string(&programmer_config)?)?;
    }

    let has_session_mgr = session_mgr.is_some();
    let (result, final_uuid) = App::new(
        programmer_config,
        saved_items,
        saved_history,
        saved_todos,
        session_uuid,
        session_mgr,
        startup_messages,
        args.providers,
    )
    .await
    .run(terminal)
    .await;

    // Pop while still on the alternate screen.
    if keyboard_enhanced {
        let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
    }
    ratatui::restore();
    execute!(
        io::stdout(),
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    disable_raw_mode()?;

    // Print resume hint so the user can continue this session later.
    if has_session_mgr && !final_uuid.is_empty() {
        println!("Session saved. Resume with: programmer --resume {final_uuid}");
    }

    result
}
