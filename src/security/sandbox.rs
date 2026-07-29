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
    let mut worker_args = Vec::with_capacity(args.len() + 2);
    worker_args.push("--".to_string());
    worker_args.push(program.to_string());
    worker_args.extend(args.iter().cloned());
    Ok(Some(SandboxInvocation {
        program: worker_executable()?,
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

fn worker_executable() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("PROGRAMMER_SANDBOX_WORKER") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        return Err(format!(
            "PROGRAMMER_SANDBOX_WORKER does not point to a file: {}",
            path.display()
        ));
    }

    let current = std::env::current_exe()
        .map_err(|error| format!("could not locate the programmer executable: {error}"))?;
    let filename = if cfg!(windows) {
        "programmer-sandbox-worker.exe"
    } else {
        "programmer-sandbox-worker"
    };
    let parent = current
        .parent()
        .ok_or_else(|| "programmer executable has no parent directory".to_string())?;
    let direct = parent.join(filename);
    if direct.is_file() {
        return Ok(direct);
    }
    if parent.file_name().is_some_and(|name| name == "deps")
        && let Some(debug_dir) = parent.parent()
    {
        let candidate = debug_dir.join(filename);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(format!(
        "sandbox worker was not found next to {}; rebuild with `cargo build --bins`",
        current.display()
    ))
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
}
