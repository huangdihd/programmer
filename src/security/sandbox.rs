// Copyright (C) 2026 huangdihd
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

use skarn_sandbox::{NetPolicy, Policy};
use std::ffi::OsString;
use std::path::PathBuf;

pub(crate) const POLICY_ENV: &str = "PROGRAMMER_SANDBOX_POLICY";

pub(crate) struct SandboxInvocation {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub policy_json: String,
    pub cwd: PathBuf,
    pub environment: Vec<(OsString, OsString)>,
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
        .read_write(security.workspace())
        .allow_read_system(config.allow_system_read)
        .net(if config.network {
            NetPolicy::AllowAll
        } else {
            NetPolicy::DenyAll
        })
        .fail_closed(config.fail_closed);

    if config.allow_temp_write {
        builder = builder.read_write(std::env::temp_dir());
    }
    for path in &config.readable_paths {
        builder = builder.read(security.resolve_path(path)?);
    }
    for path in &config.denied_read_paths {
        let path = security.resolve_path(path)?;
        if !security.workspace().starts_with(&path) {
            builder = builder.deny_read(path);
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
        environment: inherited_environment(&config.inherit_environment)?,
    }))
}

fn inherited_environment(patterns: &[String]) -> Result<Vec<(OsString, OsString)>, String> {
    let matchers = compile_environment_patterns(patterns)?;
    Ok(std::env::vars_os()
        .filter(|(name, _)| {
            name.to_str()
                .is_some_and(|name| matchers.iter().any(|matcher| matcher.is_match(name)))
        })
        .collect())
}

fn compile_environment_patterns(patterns: &[String]) -> Result<Vec<globset::GlobMatcher>, String> {
    patterns
        .iter()
        .map(|pattern| {
            globset::GlobBuilder::new(pattern)
                .case_insensitive(cfg!(windows))
                .build()
                .map(|glob| glob.compile_matcher())
                .map_err(|error| {
                    format!("invalid sandbox environment pattern '{pattern}': {error}")
                })
        })
        .collect()
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
    let SandboxInvocation {
        args,
        policy_json,
        cwd,
        environment,
        ..
    } = invocation;
    command.env_clear();
    command.envs(environment);
    command.args(args);
    command.env(POLICY_ENV, policy_json);
    command.current_dir(cwd);
}

pub(crate) fn configure_pty_command(
    command: &mut portable_pty::CommandBuilder,
    invocation: SandboxInvocation,
) {
    let SandboxInvocation {
        args,
        policy_json,
        cwd,
        environment,
        ..
    } = invocation;
    command.env_clear();
    for (name, value) in environment {
        command.env(name, value);
    }
    command.args(args);
    command.env(POLICY_ENV, policy_json);
    command.cwd(cwd);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

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
        let denied = root.join("secret");
        let readable = root.join("toolchain");
        config.sandbox.denied_read_paths = vec![denied.clone()];
        config.sandbox.readable_paths = vec![readable.clone()];
        let security = super::super::SecurityManager::new(config, root.clone()).unwrap();
        let denied = security.resolve_path(denied).unwrap();
        let readable = security.resolve_path(readable).unwrap();

        let wrapped = invocation(&security, "echo test", None)
            .unwrap()
            .expect("sandbox invocation");

        assert_eq!(wrapped.args[0], "--sandbox-worker");
        assert_eq!(wrapped.args[1], "--");
        assert_eq!(wrapped.program, sandbox_host_executable().unwrap());
        let policy: Policy = serde_json::from_str(&wrapped.policy_json).unwrap();
        assert!(policy.fs_deny_read.contains(&denied));
        assert!(policy.fs_read.contains(&readable));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sandbox_environment_uses_configured_globs() {
        let patterns = vec!["PATH".to_string(), "LC_*".to_string()];
        let matchers = compile_environment_patterns(&patterns).unwrap();
        let matches = |name| matchers.iter().any(|matcher| matcher.is_match(name));

        assert!(matches("PATH"));
        assert!(matches("LC_ALL"));
        assert!(!matches("OPENAI_API_KEY"));
        assert!(!matches("SSH_AUTH_SOCK"));
        assert!(!matches(POLICY_ENV));
    }

    #[test]
    fn invalid_environment_glob_rejects_the_invocation() {
        let error = compile_environment_patterns(&["[".to_string()]).unwrap_err();
        assert!(error.contains("invalid sandbox environment pattern"));
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
            environment: Vec::new(),
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
