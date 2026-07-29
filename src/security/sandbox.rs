// Copyright (C) 2026 huangdihd
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

use skarn_sandbox::{NetPolicy, Policy};
use std::path::PathBuf;

pub(crate) const POLICY_ENV: &str = "PROGRAMMER_SANDBOX_POLICY";

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
    command.args(invocation.args);
    command.env(POLICY_ENV, invocation.policy_json);
    command.current_dir(invocation.cwd);
}

pub(crate) fn configure_pty_command(
    command: &mut portable_pty::CommandBuilder,
    invocation: SandboxInvocation,
) {
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
        std::fs::remove_dir_all(root).unwrap();
    }
}
