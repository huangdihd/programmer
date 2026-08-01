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
use clap::{CommandFactory, Parser};
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

/// Parsed command-line arguments.
#[derive(Debug, Parser)]
#[command(
    name = "programmer",
    version,
    about = "A coding agent written in Rust",
    group(
        clap::ArgGroup::new("headless")
            .args(["print", "mcp_server", "mcp_http"])
            .multiple(false)
    )
)]
struct Args {
    /// Resume a saved session; without a UUID, open the session picker.
    #[arg(
        long,
        value_name = "UUID",
        num_args = 0..=1,
        default_missing_value = "",
        conflicts_with = "session",
        conflicts_with = "headless"
    )]
    resume: Option<String>,

    /// Open the session management panel on startup.
    #[arg(long, conflicts_with = "headless")]
    session: bool,

    /// Open the provider management panel on startup.
    #[arg(long, conflicts_with = "headless")]
    providers: bool,

    /// Serve local tools over stdio JSON-RPC without a TUI (Auto/YOLO only).
    #[arg(long, visible_alias = "serve-mcp")]
    mcp_server: bool,

    /// Run the HTTP MCP server with an approval console (default 127.0.0.1:8765).
    #[arg(
        long,
        value_name = "ADDR",
        num_args = 0..=1,
        default_missing_value = "127.0.0.1:8765"
    )]
    mcp_http: Option<std::net::SocketAddr>,

    /// Tool-gating mode for headless runs and the HTTP MCP console.
    #[arg(
        long,
        visible_alias = "mcp-mode",
        value_enum,
        default_value_t = crate::classifier::WorkMode::Auto
    )]
    work_mode: crate::classifier::WorkMode,

    /// Run one headless turn with Auto/YOLO, print the answer, and exit.
    #[arg(short = 'p', long, value_name = "TEXT")]
    print: Option<String>,
}

impl Args {
    fn validate(self) -> Result<Self, clap::Error> {
        let non_interactive_flag = if self.print.is_some() {
            Some("-p/--print")
        } else if self.mcp_server {
            Some("--mcp-server")
        } else {
            None
        };

        if let Some(flag) = non_interactive_flag
            && !non_interactive_mode_ok(self.work_mode)
        {
            let mut command = Self::command();
            return Err(clap::Error::raw(
                clap::error::ErrorKind::InvalidValue,
                format!("{flag} is non-interactive and supports only --work-mode auto or yolo"),
            )
            .format(&mut command));
        }

        Ok(self)
    }
}

/// The headless `--mcp-server` (stdio, launched by a client with no terminal)
/// only accepts non-interactive gating: `auto` (LLM classifier decides) or
/// `yolo` (run everything). `manual` needs an approval surface and `plan` just
/// refuses every mutation — both belong to the `--mcp-http` console instead.
fn non_interactive_mode_ok(mode: crate::classifier::WorkMode) -> bool {
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
    pm.resolve(&model)
        .map(|(client, name)| (client.clone(), name))
}

/// `-p/--print`: run a single headless turn and print the model's answer.
///
/// No TUI and no session persistence. Only the non-interactive gating modes
/// apply — `auto` (LLM classifier) or `yolo` — mirroring `--mcp-server`; a
/// print run has no way to answer `ask_user` or a Manual approval prompt.
async fn run_print_mode(
    prompt: String,
    mode: crate::classifier::WorkMode,
) -> color_eyre::Result<()> {
    use crate::runner::{RunnerPolicy, TurnRunner};
    use crate::tools::provider::{LocalToolProvider, ToolRegistry};
    use async_openai::types::responses::{
        InputContent, InputMessage, InputRole, MessageItem as ApiMessageItem, OutputStatus,
    };

    let (config, _) = load_config()?;
    let security = std::sync::Arc::new(
        crate::security::SecurityManager::for_current_dir(config.security.clone())
            .map_err(|error| color_eyre::eyre::eyre!(error))?,
    );
    crate::security::install_active(security.clone());
    let security = std::sync::Arc::new(crate::security::SecurityHandle::new(security));
    let pm = crate::providers::ProviderManager::new(&config).await;
    let chat_model = pm.default_model();
    let Some((chat_client, chat_name)) = pm.resolve(&chat_model).map(|(c, n)| (c.clone(), n))
    else {
        eprintln!("no usable provider/model configured; run `programmer --providers` to add one");
        std::process::exit(1);
    };

    // Local built-ins only (no MCP in print mode), behind the registry.
    // Interactive tools stay advertised, but the headless surface has no
    // interactive channel, so the runner pre-denies them with a clear reason.
    let tools = std::sync::Arc::new(ToolRegistry::new(vec![std::sync::Arc::new(
        LocalToolProvider::new(
            std::sync::Arc::new(std::sync::Mutex::new(Default::default())),
            security,
        ),
    )]));

    let policy = match mode {
        crate::classifier::WorkMode::Yolo => RunnerPolicy::Yolo,
        // Auto (and anything else that passed the gate): LLM classifier, using
        // the configured classifier model or the chat model.
        _ => {
            let clf_model = config
                .classifier_model
                .clone()
                .unwrap_or_else(|| pm.default_classifier_model());
            let Some((clf_client, clf_name)) = pm.resolve(&clf_model).map(|(c, n)| (c.clone(), n))
            else {
                eprintln!("classifier model '{clf_model}' not found");
                std::process::exit(1);
            };
            // Print mode advertises local built-ins only (no MCP), so there are
            // no per-server MCP policies to carry here.
            RunnerPolicy::Llm(Box::new(crate::runner::LlmPolicy {
                client: clf_client,
                model_name: clf_name,
                no_logprobs: std::sync::Arc::new(std::sync::Mutex::new(
                    std::collections::HashSet::new(),
                )),
            }))
        }
    };

    let runner = TurnRunner {
        client: chat_client,
        model_name: chat_name,
        model_str: chat_model,
        tools,
        policy,
        coauthor: config.git_coauthor.clone(),
        vision_enabled: false,
        thinking_level: crate::thinking::ThinkingLevel::Auto,
        // Print mode stays lean: no turn hooks (post-edit diagnostics, reminders).
        hooks: Vec::new(),
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

    match runner
        .run_turn(&conversation, &cancel, &crate::runner::HeadlessSurface)
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
    let args = Args::try_parse()
        .and_then(Args::validate)
        .unwrap_or_else(|error| error.exit());
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async_main(args))
}

async fn async_main(args: Args) -> color_eyre::Result<()> {
    // Print mode: one headless turn to stdout, no TUI.
    if let Some(prompt) = args.print {
        return run_print_mode(prompt, args.work_mode).await;
    }

    // MCP server mode: no TUI, stdout is reserved for the JSON-RPC protocol.
    // Launched by an MCP client as a subprocess, so there is no terminal — only
    // the non-interactive gating modes make sense here (validated by `Args`).
    if args.mcp_server {
        let classifier = build_mcp_classifier().await;
        let (config, _) = load_config()?;
        let security = crate::security::SecurityManager::for_current_dir(config.security)
            .map_err(|error| color_eyre::eyre::eyre!(error))?;
        let security = std::sync::Arc::new(security);
        crate::security::install_active(security.clone());
        mcp::server::McpServer::with_security(args.work_mode, classifier, security)
            .run()
            .await?;
        return Ok(());
    }

    // HTTP MCP server + ratatui approval console.
    if let Some(addr) = args.mcp_http {
        let classifier = build_mcp_classifier().await;
        let config = load_config().map(|(config, _)| config).unwrap_or_default();
        let allow_yolo = config.allow_yolo;
        let security = crate::security::SecurityManager::for_current_dir(config.security)
            .map_err(|error| color_eyre::eyre::eyre!(error))?;
        let security = std::sync::Arc::new(security);
        crate::security::install_active(security.clone());
        mcp::http_server::serve(args.work_mode, classifier, addr, allow_yolo, security).await?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classifier::WorkMode;

    #[test]
    fn non_interactive_runs_accept_only_auto_and_yolo() {
        assert!(non_interactive_mode_ok(WorkMode::Auto));
        assert!(non_interactive_mode_ok(WorkMode::Yolo));
        assert!(!non_interactive_mode_ok(WorkMode::Manual));
        assert!(!non_interactive_mode_ok(WorkMode::Plan));
    }

    #[test]
    fn cli_defaults_work_mode_to_auto() {
        let args = Args::try_parse_from(["programmer"])
            .and_then(Args::validate)
            .expect("default arguments should parse");
        assert_eq!(args.work_mode, WorkMode::Auto);
    }

    #[test]
    fn cli_accepts_legacy_mcp_mode_alias() {
        let args = Args::try_parse_from(["programmer", "--mcp-server", "--mcp-mode", "yolo"])
            .and_then(Args::validate)
            .expect("legacy MCP command should remain valid");
        assert!(args.mcp_server);
        assert_eq!(args.work_mode, WorkMode::Yolo);
    }

    #[test]
    fn cli_rejects_unknown_arguments_and_modes() {
        assert!(Args::try_parse_from(["programmer", "--unknown"]).is_err());
        assert!(
            Args::try_parse_from(["programmer", "--mcp-server", "--work-mode", "bananas"]).is_err()
        );
    }

    #[test]
    fn cli_rejects_conflicting_launch_modes() {
        assert!(Args::try_parse_from(["programmer", "--mcp-server", "--mcp-http"]).is_err());
        assert!(Args::try_parse_from(["programmer", "--print", "hi", "--session"]).is_err());
    }

    #[test]
    fn cli_validates_mode_against_the_selected_surface() {
        assert!(
            Args::try_parse_from(["programmer", "--mcp-server", "--work-mode", "manual",])
                .and_then(Args::validate)
                .is_err()
        );
        assert!(
            Args::try_parse_from(["programmer", "--mcp-http", "--work-mode", "manual",])
                .and_then(Args::validate)
                .is_ok()
        );
    }

    #[test]
    fn cli_supplies_optional_argument_defaults() {
        let resume = Args::try_parse_from(["programmer", "--resume"])
            .and_then(Args::validate)
            .expect("resume without a UUID should open the picker");
        assert_eq!(resume.resume.as_deref(), Some(""));

        let http = Args::try_parse_from(["programmer", "--mcp-http"])
            .and_then(Args::validate)
            .expect("HTTP MCP should use the default bind address");
        assert_eq!(http.mcp_http, Some(([127, 0, 0, 1], 8765).into()));
    }
}
