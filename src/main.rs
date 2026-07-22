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

mod app;
mod cancel;
mod classifier;
mod clipboard;
mod commands;
mod config;
mod consts;
mod conversation;
mod diagnostics;
mod engine;
mod mcp;
mod prompts;
mod providers;
mod response;
mod session;
mod skills;
mod tasks;
mod terminal;
mod todos;
mod tools;
mod ui;

/// Parsed command-line arguments.
struct Args {
    resume: Option<Option<String>>,
    help: bool,
    session: bool,
    providers: bool,
    mcp_server: bool,
    /// `--mcp-http [addr]`: run the HTTP MCP server + console. Carries the bind
    /// address (default 127.0.0.1:8765).
    mcp_http: Option<String>,
    /// Work mode for MCP tool gating (default Auto).
    mcp_mode: crate::classifier::WorkMode,
    /// `-p/--print <prompt>`: run one headless turn, print the final answer, and
    /// exit. No TUI, no session persistence.
    print: Option<String>,
}

const HELP_TEXT: &str = "\
programmer — a coding agent in your terminal

Usage: programmer [OPTIONS]

Options:
  --resume [uuid]   Resume a saved session; without a uuid, opens the
                    session management panel to pick one
  --session         Open the session management panel
  --providers       Open the provider management panel on startup
  --mcp-server      Run as an MCP server on stdio, exposing programmer's local
                    tools to any MCP client. Headless (no terminal), so it only
                    accepts --mcp-mode auto (default) or yolo
  --mcp-http [addr] Run an HTTP MCP server (default 127.0.0.1:8765) with a
                    ratatui approval console; the operator approves manual-mode
                    calls and switches mode (Ctrl+T) live
  --mcp-mode <mode> Tool-gating mode for the MCP server: auto (default; LLM
                    confirms dangerous tools), yolo (run everything). --mcp-http
                    also accepts manual (console approval) and plan (read-only)
  -p, --print <text> Run one headless turn on <text>, print the answer, and exit.
                    No TUI or session. Honors --mcp-mode auto (default) or yolo
  -h, --help        Show this help and exit";

fn parse_args() -> Args {
    let args: Vec<String> = std::env::args().collect();
    let mut parsed = Args {
        resume: None,
        help: false,
        session: false,
        providers: false,
        mcp_server: false,
        mcp_http: None,
        mcp_mode: crate::classifier::WorkMode::Auto,
        print: None,
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
            "--mcp-server" | "--serve-mcp" => parsed.mcp_server = true,
            "--mcp-http" => {
                if i + 1 < args.len() && !args[i + 1].starts_with('-') {
                    parsed.mcp_http = Some(args[i + 1].clone());
                    i += 1;
                } else {
                    parsed.mcp_http = Some(String::new());
                }
            }
            "--mcp-mode" => {
                if let Some(m) = args.get(i + 1) {
                    parsed.mcp_mode = parse_work_mode(m);
                    i += 1;
                }
            }
            "-p" | "--print" => {
                if let Some(prompt) = args.get(i + 1) {
                    parsed.print = Some(prompt.clone());
                    i += 1;
                }
            }
            "-h" | "--help" => parsed.help = true,
            _ => {}
        }
        i += 1;
    }
    parsed
}

fn parse_work_mode(s: &str) -> crate::classifier::WorkMode {
    use crate::classifier::WorkMode;
    match s.to_ascii_lowercase().as_str() {
        "manual" => WorkMode::Manual,
        "yolo" => WorkMode::Yolo,
        "plan" => WorkMode::Plan,
        _ => WorkMode::Auto,
    }
}

/// The headless `--mcp-server` (stdio, launched by a client with no terminal)
/// only accepts non-interactive gating: `auto` (LLM classifier decides) or
/// `yolo` (run everything). `manual` needs an approval surface and `plan` just
/// refuses every mutation — both belong to the `--mcp-http` console instead.
fn mcp_server_mode_ok(mode: crate::classifier::WorkMode) -> bool {
    use crate::classifier::WorkMode;
    matches!(mode, WorkMode::Auto | WorkMode::Yolo)
}

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
    pm.resolve(&model).map(|(client, name)| (client.clone(), name))
}

/// `-p/--print`: run a single headless turn and print the model's answer.
///
/// No TUI and no session persistence. Only the non-interactive gating modes
/// apply — `auto` (LLM classifier) or `yolo` — mirroring `--mcp-server`; a
/// print run has no way to answer `ask_user` or a Manual approval prompt.
async fn run_print_mode(prompt: String, mode: crate::classifier::WorkMode) -> color_eyre::Result<()> {
    use crate::engine::{Engine, EnginePolicy};
    use async_openai::types::responses::{
        InputContent, InputMessage, InputRole, MessageItem as ApiMessageItem, OutputStatus, Tool,
    };

    if !mcp_server_mode_ok(mode) {
        eprintln!(
            "-p/--print is non-interactive and supports only --mcp-mode auto (default) or yolo"
        );
        std::process::exit(2);
    }

    let (config, _) = load_config()?;
    let pm = crate::providers::ProviderManager::new(&config).await;
    let chat_model = pm.default_model();
    let Some((chat_client, chat_name)) = pm.resolve(&chat_model).map(|(c, n)| (c.clone(), n)) else {
        eprintln!("no usable provider/model configured; run `programmer --providers` to add one");
        std::process::exit(1);
    };

    // Full local tool set minus ask_user (unanswerable headlessly).
    let tools: Vec<Tool> = crate::tools::tools(None)
        .into_iter()
        .filter(|t| !matches!(t, Tool::Function(f) if f.name == crate::tools::ask_user::NAME))
        .collect();

    let policy = match mode {
        crate::classifier::WorkMode::Yolo => EnginePolicy::Yolo,
        // Auto (and anything else that passed the gate): LLM classifier, using
        // the configured classifier model or the chat model.
        _ => {
            let clf_model = config
                .classifier_model
                .clone()
                .unwrap_or_else(|| pm.default_classifier_model());
            let Some((clf_client, clf_name)) =
                pm.resolve(&clf_model).map(|(c, n)| (c.clone(), n))
            else {
                eprintln!("classifier model '{clf_model}' not found");
                std::process::exit(1);
            };
            let mcp_policies = config
                .mcp_servers
                .iter()
                .map(|s| (s.name.clone(), s.auto_approve))
                .collect();
            EnginePolicy::Llm(Box::new(crate::engine::LlmPolicy {
                client: clf_client,
                model_name: clf_name,
                no_logprobs: std::sync::Arc::new(std::sync::Mutex::new(
                    std::collections::HashSet::new(),
                )),
                mcp_policies,
            }))
        }
    };

    let engine = Engine {
        client: chat_client,
        model_name: chat_name,
        model_str: chat_model,
        tools,
        policy,
        mcp: None,
        coauthor: config.git_coauthor.clone(),
        // Print mode stays lean: no post-edit diagnostics feedback for now.
        diagnostics: crate::engine::DiagnosticsFeedback::default(),
        stream_retrying: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };

    let mut conversation = crate::conversation::Conversation::new();
    conversation.add_input_message(ApiMessageItem::Input(InputMessage {
        content: vec![InputContent::InputText(prompt.into())],
        role: InputRole::User,
        status: Some(OutputStatus::Completed),
    }));
    // Shared behind a Mutex to match `run_turn`'s signature (the TUI shares the
    // conversation with its render thread; print mode has only this one owner).
    let conversation = std::sync::Mutex::new(conversation);

    // Ctrl-C cancels the in-flight turn.
    let cancel = crate::cancel::CancellationToken::new();
    let cancel_signal = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            cancel_signal.cancel();
        }
    });

    match engine
        .run_turn(&conversation, &cancel, &crate::engine::HeadlessSurface)
        .await
    {
        Ok(result) => {
            println!("{}", result.final_text);
            Ok(())
        }
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
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
                tasks::restore(&session.tasks);
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

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    #[cfg(windows)]
    crate::tasks::harden_dll_search();
    let args = parse_args();

    if args.help {
        println!("{HELP_TEXT}");
        return Ok(());
    }

    // Print mode: one headless turn to stdout, no TUI.
    if let Some(prompt) = args.print {
        return run_print_mode(prompt, args.mcp_mode).await;
    }

    // MCP server mode: no TUI, stdout is reserved for the JSON-RPC protocol.
    // Launched by an MCP client as a subprocess, so there is no terminal — only
    // the non-interactive gating modes make sense here (see `mcp_server_mode_ok`).
    if args.mcp_server {
        if !mcp_server_mode_ok(args.mcp_mode) {
            eprintln!(
                "--mcp-server is headless (stdio, no terminal) and supports only \
                 --mcp-mode auto or yolo; use --mcp-http for a console (manual/plan)"
            );
            std::process::exit(2);
        }
        let classifier = build_mcp_classifier().await;
        mcp::server::McpServer::new(args.mcp_mode, classifier)
            .run()
            .await?;
        return Ok(());
    }

    // HTTP MCP server + ratatui approval console.
    if let Some(addr_arg) = args.mcp_http {
        let addr: std::net::SocketAddr = if addr_arg.trim().is_empty() {
            ([127, 0, 0, 1], 8765).into()
        } else {
            match addr_arg.parse() {
                Ok(a) => a,
                Err(e) => {
                    eprintln!("invalid --mcp-http address '{addr_arg}': {e}");
                    std::process::exit(1);
                }
            }
        };
        let classifier = build_mcp_classifier().await;
        let allow_yolo = load_config().map(|(c, _)| c.allow_yolo).unwrap_or(false);
        mcp::http_server::serve(args.mcp_mode, classifier, addr, allow_yolo).await?;
        return Ok(());
    }

    let resume = if args.session && args.resume.is_none() {
        Some(None)
    } else {
        args.resume
    };

    let bootstrap = resolve_session(resume);
    let (programmer_config, _config_path) = load_config()?;

    // Derive a project name from the current directory for the terminal title.
    let project_name = std::env::current_dir()
        .ok()
        .and_then(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
        })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classifier::WorkMode;

    #[test]
    fn mcp_server_accepts_only_auto_and_yolo() {
        assert!(mcp_server_mode_ok(WorkMode::Auto));
        assert!(mcp_server_mode_ok(WorkMode::Yolo));
        assert!(!mcp_server_mode_ok(WorkMode::Manual));
        assert!(!mcp_server_mode_ok(WorkMode::Plan));
    }

    #[test]
    fn mcp_mode_defaults_to_auto() {
        assert!(matches!(parse_work_mode("something-unknown"), WorkMode::Auto));
    }
}

