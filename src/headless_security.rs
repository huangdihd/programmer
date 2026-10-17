// Copyright (C) 2026 huangdihd
// SPDX-License-Identifier: GPL-3.0-or-later

//! Invocation-local security overrides; never persists configuration.
use crate::cli::SecurityArgs;
use crate::config::programmer_config::ProgrammerConfig;
use crate::security::policy::{PermissionEffect, PermissionRule};
use crate::security::{AccessKind, SandboxMode, SecurityConfig, SecurityManager};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Invalid invocation settings, distinguishable from execution failures.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct UsageError(pub String);

/// Resolve invocation policy without installing it or starting a watcher.
/// Path normalization reads the filesystem; missing descendants are supported.
pub(crate) fn resolve_security(
    config: &ProgrammerConfig,
    args: &SecurityArgs,
    cwd: &Path,
) -> Result<SecurityConfig, UsageError> {
    let mut security = match args.security_profile.as_deref() {
        Some(name) => config.security_profiles.get(name).cloned().ok_or_else(|| {
            UsageError(format!(
                "unknown security profile '{name}'; available profiles: {}",
                {
                    let mut names: Vec<_> = config.security_profiles.keys().cloned().collect();
                    names.sort();
                    names.join(", ")
                }
            ))
        })?,
        None => config.security.clone(),
    };
    if let Some(mode) = &args.sandbox {
        SandboxMode::parse(mode)
            .ok_or_else(|| UsageError(format!("unknown sandbox mode '{mode}'")))?
            .apply(&mut security.sandbox);
    }
    if args.no_protect_file_changes {
        security.protect_file_changes = false;
    }
    if !args.allow_read.is_empty() || !args.allow_write.is_empty() || !args.deny_read.is_empty() {
        // A disabled resolver shares SecurityManager's canonicalization and tilde
        // expansion without compiling user rules or starting a filesystem watcher.
        let resolver = SecurityManager::new(
            SecurityConfig {
                enabled: false,
                protect_file_changes: false,
                ..Default::default()
            },
            cwd.to_path_buf(),
        )
        .map_err(UsageError)?;
        append_paths(
            &mut security.sandbox.readable_paths,
            &args.allow_read,
            &resolver,
        )?;
        let writes = append_paths(
            &mut security.sandbox.writable_paths,
            &args.allow_write,
            &resolver,
        )?;
        let denies = append_paths(
            &mut security.sandbox.denied_read_paths,
            &args.deny_read,
            &resolver,
        )?;
        append_rules(
            &mut security,
            writes,
            AccessKind::Write,
            PermissionEffect::Allow,
        );
        append_rules(
            &mut security,
            denies,
            AccessKind::Read,
            PermissionEffect::Deny,
        );
    }
    for pattern in &args.deny_env {
        globset::Glob::new(pattern).map_err(|error| {
            UsageError(format!(
                "invalid environment name pattern '{pattern}': {error}"
            ))
        })?;
        if !security.sandbox.denied_environment.contains(pattern) {
            security.sandbox.denied_environment.push(pattern.clone());
        }
    }
    Ok(security)
}

fn append_paths(
    target: &mut Vec<PathBuf>,
    values: &[PathBuf],
    resolver: &SecurityManager,
) -> Result<Vec<PathBuf>, UsageError> {
    let mut paths = Vec::new();
    for value in values {
        let path = resolver.resolve_path(value).map_err(UsageError)?;
        if !target.contains(&path) {
            target.push(path.clone());
        }
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    Ok(paths)
}

fn append_rules(
    config: &mut SecurityConfig,
    paths: Vec<PathBuf>,
    operation: AccessKind,
    effect: PermissionEffect,
) {
    for path in paths {
        // SecurityManager matches slash-normalized paths. Escape CLI paths so
        // metacharacters never broaden a literal directory grant or denial.
        let literal = path.to_string_lossy().replace('\\', "/");
        let escaped = globset::escape(&literal);
        for pattern in [
            escaped.clone(),
            format!("{}/**", escaped.trim_end_matches('/')),
        ] {
            let rule = PermissionRule {
                operation,
                effect,
                pattern,
            };
            if !config.rules.contains(&rule) {
                config.rules.push(rule);
            }
        }
    }
}

/// Summary for result/event metadata; excludes paths and environment patterns.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct SecurityMetadata {
    pub profile: String,
    pub filesystem_enabled: bool,
    pub overrides: bool,
    pub sandbox: &'static str,
}

pub(crate) fn security_metadata(
    config: &ProgrammerConfig,
    args: &SecurityArgs,
    resolved: &SecurityConfig,
) -> SecurityMetadata {
    SecurityMetadata {
        profile: args
            .security_profile
            .clone()
            .unwrap_or_else(|| config.active_security_profile.clone()),
        filesystem_enabled: resolved.enabled,
        overrides: args.has_overrides(),
        sandbox: SandboxMode::from_config(&resolved.sandbox).label(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> ProgrammerConfig {
        let mut config = ProgrammerConfig::default();
        config.normalize_security_profiles();
        config
    }
    #[test]
    fn defaults_preserve_policy() {
        let config = config();
        assert_eq!(
            resolve_security(&config, &SecurityArgs::default(), Path::new("/project")).unwrap(),
            config.security
        );
    }
    #[test]
    fn profile_precedes_overrides_without_mutation() {
        let mut config = config();
        let mut profile = config.security.clone();
        profile.protect_file_changes = false;
        config
            .security_profiles
            .insert("other".into(), profile.clone());
        let args = SecurityArgs {
            security_profile: Some("other".into()),
            sandbox: Some("restricted".into()),
            ..Default::default()
        };
        let resolved = resolve_security(&config, &args, Path::new("/project")).unwrap();
        assert!(!resolved.protect_file_changes);
        assert!(resolved.sandbox.enabled);
        assert!(!resolved.sandbox.network);
        assert_eq!(config.security_profiles["other"], profile);
        assert_ne!(config.active_security_profile, "other");
        let metadata = security_metadata(&config, &args, &resolved);
        assert_eq!(metadata.profile, "other");
        assert_eq!(metadata.sandbox, "restricted");
    }
    #[test]
    fn unknown_profile_is_downcastable() {
        let args = SecurityArgs {
            security_profile: Some("missing".into()),
            ..Default::default()
        };
        let error: color_eyre::Report = resolve_security(&config(), &args, Path::new("/project"))
            .unwrap_err()
            .into();
        assert!(error.downcast_ref::<UsageError>().is_some());
        for name in config().security_profiles.keys() {
            assert!(error.to_string().contains(name));
        }
    }
    #[test]
    fn additive_paths_and_environment_are_deduplicated() {
        let mut config = config();
        config
            .security
            .sandbox
            .readable_paths
            .push("/existing".into());
        config
            .security
            .sandbox
            .denied_environment
            .push("OLD_*".into());
        let args = SecurityArgs {
            allow_read: vec!["relative".into(), "/absolute".into(), "relative".into()],
            allow_write: vec!["output".into()],
            deny_read: vec!["private".into()],
            deny_env: vec!["NEW_*".into(), "OLD_*".into()],
            ..Default::default()
        };
        let result = resolve_security(&config, &args, Path::new("/")).unwrap();
        // Canonical roots differ per platform (`/` vs. a verbatim `\\?\D:\`).
        let root = Path::new("/").canonicalize().unwrap();
        assert_eq!(
            result.sandbox.readable_paths,
            [
                PathBuf::from("/existing"),
                root.join("relative"),
                root.join("absolute")
            ]
        );
        assert_eq!(result.sandbox.writable_paths, [root.join("output")]);
        assert_eq!(result.sandbox.denied_read_paths, [root.join("private")]);
        assert_eq!(result.sandbox.denied_environment, ["OLD_*", "NEW_*"]);
        assert_eq!(result.rules.len(), config.security.rules.len() + 4);
    }
    #[test]
    fn filesystem_and_sandbox_are_independent() {
        for mode in ["restricted", "network", "off"] {
            let args = SecurityArgs {
                sandbox: Some(mode.into()),
                ..Default::default()
            };
            let config = config();
            let resolved = resolve_security(&config, &args, Path::new("/project")).unwrap();
            assert!(resolved.enabled);
            assert_eq!(SandboxMode::from_config(&resolved.sandbox).label(), mode);
            let args = SecurityArgs {
                no_protect_file_changes: true,
                ..args
            };
            let result = resolve_security(&config, &args, Path::new("/")).unwrap();
            assert!(result.enabled);
            assert!(!result.protect_file_changes);
            assert_eq!(result.sandbox, resolved.sandbox);
        }
    }
    #[test]
    fn invalid_glob_and_mode_are_rejected() {
        for args in [
            SecurityArgs {
                deny_env: vec!["[".into()],
                ..Default::default()
            },
            SecurityArgs {
                sandbox: Some("invalid".into()),
                ..Default::default()
            },
        ] {
            assert!(resolve_security(&config(), &args, Path::new("/project")).is_err());
        }
    }
    #[test]
    fn literal_canonical_rules_authorize_only_requested_paths() {
        let root =
            std::env::temp_dir().join(format!("programmer-overrides-{}", uuid::Uuid::new_v4()));
        let workspace = root.join("workspace");
        // `*` and `?` are not valid in Windows file names.
        let literal = root.join(if cfg!(windows) {
            "data[1]{x}"
        } else {
            "data[1]*{x}?"
        });
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&literal).unwrap();
        let args = SecurityArgs {
            allow_write: vec![literal.clone()],
            deny_read: vec![literal.clone()],
            no_protect_file_changes: true,
            ..Default::default()
        };
        let resolved = resolve_security(&config(), &args, &workspace).unwrap();
        let manager = SecurityManager::new(resolved, workspace.clone()).unwrap();
        for path in [literal.clone(), literal.join("nested/file")] {
            assert!(manager.authorize_path(AccessKind::Write, &path).is_ok());
            assert!(manager.authorize_path(AccessKind::Read, &path).is_err());
        }
        for path in [
            root.join("data1ZZxQ"),
            root.join("data1x"),
            root.join("unrelated/file"),
        ] {
            assert!(manager.authorize_path(AccessKind::Write, &path).is_err());
            assert!(manager.authorize_path(AccessKind::Read, &path).is_ok());
        }
        #[cfg(unix)]
        {
            let alias = root.join("alias");
            std::os::unix::fs::symlink(&literal, &alias).unwrap();
            let args = SecurityArgs {
                allow_write: vec![alias.clone()],
                deny_read: vec![alias],
                ..args
            };
            let resolved = resolve_security(&config(), &args, &workspace).unwrap();
            let manager = SecurityManager::new(resolved, workspace.clone()).unwrap();
            assert!(
                manager
                    .authorize_path(AccessKind::Write, literal.join("new/file"))
                    .is_ok()
            );
            assert!(
                manager
                    .authorize_path(AccessKind::Read, literal.join("new/file"))
                    .is_err()
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn tilde_and_metadata_follow_resolved_policy() {
        let mut config = config();
        config.security.enabled = false;
        let args = SecurityArgs {
            allow_read: vec!["~".into()],
            no_protect_file_changes: true,
            ..Default::default()
        };
        let result = resolve_security(&config, &args, &std::env::current_dir().unwrap()).unwrap();
        if let Some(home) = dirs::home_dir() {
            assert!(
                result
                    .sandbox
                    .readable_paths
                    .contains(&home.canonicalize().unwrap())
            );
        }
        assert!(!result.enabled);
        let metadata = security_metadata(&config, &args, &result);
        assert!(metadata.overrides);
        assert!(!metadata.filesystem_enabled);
        assert!(!security_metadata(&config, &SecurityArgs::default(), &config.security).overrides);
    }
}
