// Copyright (C) 2026 huangdihd
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

use skarn_sandbox::{NetPolicy, Policy};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

pub(crate) const POLICY_ENV: &str = "PROGRAMMER_SANDBOX_POLICY";

const SAFE_ENVIRONMENT_VARIABLES: &[&str] = &[
    "ANDROID_HOME",
    "ANDROID_SDK_ROOT",
    "AR",
    "CC",
    "CARGO_HOME",
    "CLICOLOR",
    "CLICOLOR_FORCE",
    "CMAKE_PREFIX_PATH",
    "COLORTERM",
    "CPATH",
    "CXX",
    "DEVELOPER_DIR",
    "GOPATH",
    "GOROOT",
    "HOME",
    "JAVA_HOME",
    "LANG",
    "LANGUAGE",
    "LD",
    "LIBRARY_PATH",
    "LOGNAME",
    "MACOSX_DEPLOYMENT_TARGET",
    "NO_COLOR",
    "PATH",
    "PKG_CONFIG_PATH",
    "RUST_BACKTRACE",
    "RUST_LOG",
    "RUSTUP_HOME",
    "SDKROOT",
    "SHELL",
    "TEMP",
    "TERM",
    "TERMINFO",
    "TERMINFO_DIRS",
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
    "TMP",
    "TMPDIR",
    "TZ",
    "USER",
];

pub(crate) struct SandboxInvocation {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub policy_json: String,
    pub cwd: PathBuf,
}

pub(super) fn invocation(
    security: &super::SecurityManager,
    command: &str,
    dir: Option<&str>,
) -> Result<Option<SandboxInvocation>, String> {
    let (shell, flag) = crate::tools::shell();
    program_invocation(
        security,
        shell,
        &[flag.to_string(), command.to_string()],
        dir,
    )
}

pub(super) fn program_invocation(
    security: &super::SecurityManager,
    program: &str,
    args: &[String],
    dir: Option<&str>,
) -> Result<Option<SandboxInvocation>, String> {
    let config = security.sandbox_config();
    if !config.enabled {
        return Ok(None);
    }

    let cwd = match dir {
        Some(dir) => security.authorize_path(super::policy::AccessKind::Write, dir)?,
        None => security.workspace().to_path_buf(),
    };
    if !cwd.is_dir() {
        return Err(format!(
            "sandbox working directory is not a directory: {}",
            cwd.display()
        ));
    }

    let mut builder = Policy::builder()
        .workspace(security.workspace())
        .read_write(std::env::temp_dir())
        .net(if config.network {
            NetPolicy::AllowAll
        } else {
            NetPolicy::DenyAll
        })
        .fail_closed(true);

    for path in common_development_paths() {
        if path.exists() {
            builder = builder.read(path);
        }
    }
    for path in sensitive_read_paths(security.workspace()) {
        builder = builder.deny_read(path);
    }
    for path in &config.writable_paths {
        builder = builder.read_write(security.resolve_path(path)?);
    }

    let policy_json = serde_json::to_string(&builder.build())
        .map_err(|error| format!("could not serialize sandbox policy: {error}"))?;
    let mut worker_args = Vec::with_capacity(args.len() + 3);
    worker_args.push("--sandbox-worker".to_string());
    worker_args.push("--".to_string());
    worker_args.push(program.to_string());
    worker_args.extend(args.iter().cloned());
    Ok(Some(SandboxInvocation {
        program: sandbox_host_executable()?,
        args: worker_args,
        policy_json,
        cwd,
    }))
}

fn common_development_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(home) = dirs::home_dir() {
        paths.extend([
            home.join(".cargo").join("bin"),
            home.join(".cargo").join("git"),
            home.join(".cargo").join("registry"),
            home.join(".rustup"),
            home.join(".local").join("share"),
        ]);
    }
    paths.extend(
        ["/opt/homebrew", "/nix/store"]
            .into_iter()
            .map(PathBuf::from),
    );
    paths
}

fn sensitive_read_paths(workspace: &Path) -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let mut paths = [
        ".bash_history",
        ".claude",
        ".codex",
        ".config/opencode",
        ".fish_history",
        ".local/share/fish/fish_history",
        ".zsh_history",
        "Library/Application Support/1Password",
        "Library/Application Support/Arc/User Data",
        "Library/Application Support/Bitwarden",
        "Library/Application Support/BraveSoftware",
        "Library/Application Support/Firefox",
        "Library/Application Support/Google/Chrome",
        "Library/Application Support/Microsoft Edge",
        "Library/Application Support/com.apple.TCC",
        "Library/Application Support/programmer",
        "Library/Cookies",
        "Library/Keychains",
        "Library/Mail",
        "Library/Messages",
        "Library/Safari",
    ]
    .into_iter()
    .map(|path| home.join(path))
    .filter(|path| path.exists() && !workspace.starts_with(path))
    .collect::<Vec<_>>();
    for base in [dirs::config_dir(), dirs::data_dir()].into_iter().flatten() {
        paths.push(base.join("programmer"));
    }
    paths.retain(|path| path.exists() && !workspace.starts_with(path));
    paths.sort();
    paths.dedup();
    paths
}

fn should_inherit_environment(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    #[cfg(not(windows))]
    {
        name.starts_with("LC_") || SAFE_ENVIRONMENT_VARIABLES.contains(&name)
    }
    #[cfg(windows)]
    {
        let name = name.to_ascii_uppercase();
        name.starts_with("LC_") || SAFE_ENVIRONMENT_VARIABLES.contains(&name.as_str())
    }
}

fn safe_environment() -> impl Iterator<Item = (OsString, OsString)> {
    std::env::vars_os().filter(|(name, _)| should_inherit_environment(name))
}

fn sandbox_host_executable() -> Result<PathBuf, String> {
    let current = std::env::current_exe()
        .map_err(|error| format!("could not locate the programmer executable: {error}"))?;

    #[cfg(test)]
    if current
        .parent()
        .and_then(|parent| parent.file_name())
        .is_some_and(|name| name == "deps")
        && let Some(debug_dir) = current.parent().and_then(|parent| parent.parent())
    {
        let candidate = debug_dir.join(if cfg!(windows) {
            "programmer.exe"
        } else {
            "programmer"
        });
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    Ok(current)
}

pub(crate) fn run_worker_if_requested() {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--sandbox-worker")) {
        return;
    }
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--")) {
        fail_worker("sandbox worker arguments are invalid");
    }
    let Some(program) = args.next() else {
        fail_worker("sandbox worker requires a program");
    };
    let policy_json =
        std::env::var(POLICY_ENV).unwrap_or_else(|_| fail_worker("sandbox policy is missing"));
    let policy: Policy = serde_json::from_str(&policy_json)
        .unwrap_or_else(|error| fail_worker(&format!("invalid sandbox policy: {error}")));
    // The worker runs before the Tokio runtime or any other threads exist.
    // Remove the serialized policy so the target cannot inspect its boundary.
    unsafe {
        std::env::remove_var(POLICY_ENV);
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        policy
            .apply_to_current_process()
            .unwrap_or_else(|error| fail_worker(&format!("could not apply sandbox: {error}")));
        let error = std::process::Command::new(program).args(args).exec();
        fail_worker(&format!("could not execute sandboxed command: {error}"));
    }

    #[cfg(windows)]
    {
        let _ = (program, args, policy);
        fail_worker("the sandbox worker does not yet support Windows process launching");
    }
}

fn fail_worker(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(126);
}

pub(crate) fn configure_tokio_command(
    command: &mut tokio::process::Command,
    invocation: SandboxInvocation,
) {
    command.env_clear();
    command.envs(safe_environment());
    command.args(invocation.args);
    command.env(POLICY_ENV, invocation.policy_json);
    command.current_dir(invocation.cwd);
}

pub(crate) fn configure_pty_command(
    command: &mut portable_pty::CommandBuilder,
    invocation: SandboxInvocation,
) {
    command.env_clear();
    for (name, value) in safe_environment() {
        command.env(name, value);
    }
    command.args(invocation.args);
    command.env(POLICY_ENV, invocation.policy_json);
    command.cwd(invocation.cwd);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_sandbox_does_not_wrap_commands() {
        let root =
            std::env::temp_dir().join(format!("programmer-sandbox-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let security =
            super::super::SecurityManager::new(Default::default(), root.clone()).unwrap();
        assert!(invocation(&security, "echo test", None).unwrap().is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn enabled_sandbox_reexecutes_programmer_in_worker_mode() {
        let root =
            std::env::temp_dir().join(format!("programmer-sandbox-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let mut config = super::super::SecurityConfig::default();
        config.sandbox.enabled = true;
        let security = super::super::SecurityManager::new(config, root.clone()).unwrap();

        let wrapped = invocation(&security, "echo test", None)
            .unwrap()
            .expect("sandbox invocation");

        assert_eq!(wrapped.args[0], "--sandbox-worker");
        assert_eq!(wrapped.args[1], "--");
        assert_eq!(wrapped.program, sandbox_host_executable().unwrap());
        let policy: Policy = serde_json::from_str(&wrapped.policy_json).unwrap();
        for path in sensitive_read_paths(&root) {
            assert!(policy.fs_deny_read.contains(&path));
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sandbox_environment_only_inherits_safe_variables() {
        assert!(should_inherit_environment(OsStr::new("PATH")));
        assert!(should_inherit_environment(OsStr::new("LC_ALL")));
        assert!(should_inherit_environment(OsStr::new("JAVA_HOME")));
        #[cfg(not(windows))]
        assert!(!should_inherit_environment(OsStr::new("java_home")));
        assert!(!should_inherit_environment(OsStr::new("OPENAI_API_KEY")));
        assert!(!should_inherit_environment(OsStr::new("LLMHUB_API_KEY")));
        assert!(!should_inherit_environment(OsStr::new("SSH_AUTH_SOCK")));
        assert!(!should_inherit_environment(OsStr::new(
            "AWS_SECRET_ACCESS_KEY"
        )));
        assert!(!should_inherit_environment(OsStr::new(POLICY_ENV)));
    }

    #[test]
    fn sandbox_process_configuration_clears_parent_environment() {
        let root =
            std::env::temp_dir().join(format!("programmer-sandbox-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let invocation = SandboxInvocation {
            program: PathBuf::from("programmer"),
            args: vec!["--sandbox-worker".to_string()],
            policy_json: "{}".to_string(),
            cwd: root.clone(),
        };
        let mut command = tokio::process::Command::new("programmer");

        configure_tokio_command(&mut command, invocation);

        let environment = command
            .as_std()
            .get_envs()
            .map(|(name, value)| (name.to_owned(), value.map(ToOwned::to_owned)))
            .collect::<std::collections::HashMap<_, _>>();
        assert!(environment.contains_key(OsStr::new(POLICY_ENV)));
        assert!(!environment.contains_key(OsStr::new("OPENAI_API_KEY")));
        assert!(!environment.contains_key(OsStr::new("SSH_AUTH_SOCK")));
        std::fs::remove_dir_all(root).unwrap();
    }
}
