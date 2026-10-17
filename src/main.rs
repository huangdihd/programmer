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
use std::io::{self, Write};
use std::path::Path;

mod agents;
mod app;
mod cancel;
mod checkpoint;
mod classifier;
mod cli;
mod clipboard;
mod commands;
mod compaction;
mod config;
mod consts;
mod conversation;
mod diagnostics;
mod execution;
mod headless;
mod headless_interaction;
mod headless_security;
mod headless_session;
mod mcp;
mod memory;
mod peers;
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

/// Build the `(client, model, top_logprobs)` the MCP server's `auto` mode uses
/// to classify tool calls: the configured classifier model, else the default
/// model. Returns `None` when no provider resolves (auto mode then refuses
/// dangerous tools).
async fn build_mcp_classifier(
    config: &ProgrammerConfig,
) -> Option<(
    async_openai::Client<async_openai::config::OpenAIConfig>,
    String,
    u8,
)> {
    let pm = crate::providers::ProviderManager::new(config).await;
    let model = config
        .classifier_model
        .clone()
        .unwrap_or_else(|| pm.default_model());
    pm.resolve(&model)
        .map(|(client, name)| (client.clone(), name, config.classifier_top_logprobs))
}

/// Resolved session data ready for the application.
struct SessionBootstrap {
    seed: app::session::SessionSeed,
    mgr: Option<SessionManager>,
    messages: Vec<String>,
    _lock: Option<crate::session::SessionLock>,
}

fn resolve_session(resume: Option<Option<String>>) -> Option<SessionBootstrap> {
    let session_mgr = SessionManager::new();
    let mut startup_messages: Vec<String> = Vec::new();

    let mut session_uuid = match (resume, &session_mgr) {
        (Some(Some(uuid)), Some(mgr)) => match mgr.load(&uuid) {
            Ok(Some(_)) => uuid,
            Err(error) => {
                eprintln!("Cannot resume session {uuid}: {error}. Original file was not changed.");
                return None;
            }
            Ok(None) => {
                startup_messages.push(format!("Session {uuid} not found, creating a new session."));
                let session = mgr.create();
                session.uuid
            }
        },
        (Some(None), Some(mgr)) => match mgr.list_all() {
            Ok(sessions) => {
                let was_empty = sessions.is_empty();
                match session::pick_session(&sessions, mgr) {
                    Some(uuid) => match mgr.load(&uuid) {
                        Ok(Some(_)) => uuid,
                        Err(error) => {
                            eprintln!(
                                "Cannot resume session {uuid}: {error}. Original file was not changed."
                            );
                            return None;
                        }
                        Ok(None) => {
                            startup_messages.push(format!(
                                "Session {uuid} not found on disk, starting a new session."
                            ));
                            let session = mgr.create();
                            session.uuid
                        }
                    },
                    None => {
                        if was_empty {
                            startup_messages.push(
                                "No existing sessions found, creating a new one.".to_string(),
                            );
                        }
                        let session = mgr.create();
                        session.uuid
                    }
                }
            }
            Err(e) => {
                startup_messages.push(format!(
                    "Failed to list sessions: {e}, creating new session."
                ));
                if let Some(mgr) = session_mgr.as_ref() {
                    let session = mgr.create();
                    session.uuid
                } else {
                    String::new()
                }
            }
        },
        _ => {
            if let Some(mgr) = &session_mgr {
                let session = mgr.create();
                session.uuid
            } else {
                startup_messages.push("Session persistence unavailable.".to_string());
                String::new()
            }
        }
    };

    let session_lock = if let Some(mgr) = &session_mgr {
        match mgr.try_lock(&session_uuid) {
            Ok(lock) => Some(lock),
            Err(crate::session::SessionLockError::InUse { pid }) => {
                if !prompt_session_fork(pid) {
                    return None;
                }
                let source = match mgr.load(&session_uuid) {
                    Ok(Some(source)) => source,
                    Ok(None) => {
                        eprintln!("Session {session_uuid} was not found for forking.");
                        return None;
                    }
                    Err(error) => {
                        eprintln!("Cannot load session for forking: {error}");
                        return None;
                    }
                };
                let forked = match mgr.fork(&source) {
                    Ok(session) => session,
                    Err(error) => {
                        eprintln!("Failed to fork session: {error}");
                        return None;
                    }
                };
                session_uuid = forked.uuid;
                startup_messages.push(format!(
                    "Forked the in-use conversation into session {session_uuid}."
                ));
                match mgr.try_lock(&session_uuid) {
                    Ok(lock) => Some(lock),
                    Err(error) => {
                        eprintln!("Failed to lock forked session: {error:?}");
                        return None;
                    }
                }
            }
            Err(crate::session::SessionLockError::Io(error)) => {
                eprintln!("Failed to lock session: {error}");
                return None;
            }
        }
    } else {
        None
    };

    // An offline peer inquiry may have saved an exchange between the picker
    // read and lock acquisition. Restore the authoritative locked snapshot.
    let seed = if let Some(manager) = session_mgr.as_ref().filter(|_| session_lock.is_some()) {
        match app::session::SessionSeed::load(manager, session_uuid) {
            Ok(seed) => seed,
            Err(error) => {
                eprintln!("Cannot restore locked session: {error}. Original file was not changed.");
                return None;
            }
        }
    } else {
        app::session::SessionSeed::Fresh { uuid: session_uuid }
    };

    Some(SessionBootstrap {
        seed,
        mgr: session_mgr,
        messages: startup_messages,
        _lock: session_lock,
    })
}

fn prompt_session_fork(pid: Option<u32>) -> bool {
    let owner = pid.map_or_else(
        || "another Programmer process".to_string(),
        |pid| format!("Programmer process {pid}"),
    );
    eprintln!("{owner} is currently using this conversation.");
    loop {
        eprint!("[f] Fork this conversation  [q] Exit this Programmer: ");
        let _ = io::stderr().flush();
        let mut answer = String::new();
        if io::stdin().read_line(&mut answer).is_err() {
            return false;
        }
        match answer.trim().to_ascii_lowercase().as_str() {
            "f" | "fork" => return true,
            "q" | "quit" | "exit" => return false,
            _ => eprintln!("Please enter 'f' to fork or 'q' to exit."),
        }
    }
}

/// Explicit files are required and read-only: no migration or first-run writes.
/// Relative paths are resolved against the current working directory.
pub(crate) fn load_config_from(
    path: Option<&Path>,
) -> color_eyre::Result<(ProgrammerConfig, std::path::PathBuf)> {
    let Some(path) = path else {
        return load_config();
    };
    let mut config: ProgrammerConfig = Config::builder()
        .add_source(File::from(path).required(true))
        .add_source(Environment::with_prefix("Programmer"))
        .build()?
        .try_deserialize()?;
    // Migrate in memory only: the active security profile must still be
    // copied into `security`, but an explicit file is never rewritten.
    config.migrate_if_needed();
    Ok((config, path.to_path_buf()))
}

fn load_config() -> color_eyre::Result<(ProgrammerConfig, std::path::PathBuf)> {
    let config_path = {
        // Debug-only isolation for CLI integration tests; release config selection
        // deliberately ignores this environment variable.
        #[cfg(debug_assertions)]
        let test_dir = std::env::var_os("PROGRAMMER_TEST_CONFIG_DIR");
        #[cfg(not(debug_assertions))]
        let test_dir: Option<std::ffi::OsString> = None;
        test_dir
            .map(|d| std::path::PathBuf::from(d).join("config.toml"))
            .unwrap_or_else(|| {
                dirs::config_dir()
                    .map(|d| d.join("programmer").join("config.toml"))
                    .unwrap_or_else(|| Path::new("config.toml").to_path_buf())
            })
    };

    let mut programmer_config: ProgrammerConfig = Config::builder()
        .add_source(File::from(config_path.clone()).required(false))
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

fn legacy_outcome(passed: bool) -> headless::HeadlessOutcome {
    if passed {
        headless::HeadlessOutcome::Completed
    } else {
        headless::HeadlessOutcome::Error
    }
}

async fn async_main(mut args: cli::Args) -> color_eyre::Result<()> {
    if let Some(command) = args.command.take() {
        let result = match command {
            cli::Command::Run(run) => headless::run(run).await,
            cli::Command::Init(init) => headless::init(init).await,
            cli::Command::Diagnostics(diagnostics) => headless::diagnostics(diagnostics).await,
            cli::Command::Mcp(mcp) => run_mcp(mcp)
                .await
                .map(|_| headless::HeadlessOutcome::Completed),
            cli::Command::Upgrade(args) => crate::upgrade::upgrade(args).await.map(legacy_outcome),
            cli::Command::Uninstall(args) => {
                crate::upgrade::uninstall(args).await.map(legacy_outcome)
            }
        };
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(error) => {
                eprintln!("{error:#}");
                headless::HeadlessOutcome::from_error(&error)
            }
        };
        if outcome.exit_code() != 0 {
            std::process::exit(outcome.exit_code());
        }
        return Ok(());
    }

    let resume = match args.resume {
        Some(uuid) if uuid.is_empty() => Some(None),
        Some(uuid) => Some(Some(uuid)),
        None if args.session => Some(None),
        None => None,
    };

    let Some(bootstrap) = resolve_session(resume) else {
        return Ok(());
    };
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
            bootstrap.seed,
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
            let (config, _) = load_config_from(args.config.as_deref())?;
            let classifier = build_mcp_classifier(&config).await;
            let security = install_security_with_overrides(&config, &args.security)?;
            mcp::server::McpServer::with_security(args.work_mode, classifier, security)
                .run()
                .await?;
        }
        cli::McpCommand::Http(args) => {
            headless::set_working_directory(args.cwd.as_deref())?;
            let config = if args.config.is_some() {
                load_config_from(args.config.as_deref())?.0
            } else {
                load_config().map(|(config, _)| config).unwrap_or_default()
            };
            let classifier = build_mcp_classifier(&config).await;
            let allow_yolo = config.allow_yolo;
            let security = install_security_with_overrides(&config, &args.security)?;
            mcp::http_server::serve(args.work_mode, classifier, args.addr, allow_yolo, security)
                .await?;
        }
    }
    Ok(())
}

fn install_security_with_overrides(
    config: &ProgrammerConfig,
    args: &cli::SecurityArgs,
) -> color_eyre::Result<std::sync::Arc<crate::security::SecurityManager>> {
    let cwd = std::env::current_dir()?;
    let resolved = headless_security::resolve_security(config, args, &cwd)?;
    if !resolved.enabled && args.has_overrides() {
        eprintln!(
            "warning: security is disabled in the resolved profile; filesystem policy overrides will not be enforced"
        );
    }
    let security = crate::security::SecurityManager::new(resolved, cwd)
        .map_err(|error| color_eyre::eyre::eyre!(error))?;
    let security = std::sync::Arc::new(security);
    crate::security::install_active(security.clone());
    Ok(security)
}

#[cfg(test)]
mod config_loading_tests {
    use super::*;

    #[test]
    fn explicit_config_is_required_and_never_written() {
        let dir = std::env::temp_dir().join(format!(
            "programmer-explicit-config-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let path = dir.join("config.toml");
        assert!(load_config_from(Some(&path)).is_err());
        assert!(
            !dir.exists(),
            "missing explicit config must not create parents"
        );
        std::fs::create_dir_all(&dir).unwrap();
        // Legacy keys would be migrated by the default loader.
        let original = "# keep this comment\nmodel = 'legacy-model'\n";
        std::fs::write(&path, original).unwrap();
        let (_, loaded_path) = load_config_from(Some(&path)).unwrap();
        assert_eq!(loaded_path, path);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        // The active profile is applied in memory without rewriting the file.
        let profiles = "active_security_profile = 'strict'\n\
            [security_profiles.default]\nenabled = true\n\
            [security_profiles.strict]\nenabled = true\nprotect_file_changes = false\n";
        std::fs::write(&path, profiles).unwrap();
        let (config, _) = load_config_from(Some(&path)).unwrap();
        assert_eq!(config.active_security_profile, "strict");
        assert!(!config.security.protect_file_changes);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), profiles);
        std::fs::write(&path, "not valid = [toml").unwrap();
        assert!(load_config_from(Some(&path)).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "not valid = [toml");
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }
}
