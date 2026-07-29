// Copyright (C) 2026 huangdihd
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

use globset::{Glob, GlobMatcher};
use notify::{RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AccessKind {
    Read,
    Write,
    Execute,
    Network,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PermissionEffect {
    Allow,
    Deny,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct PermissionRule {
    pub operation: AccessKind,
    pub pattern: String,
    pub effect: PermissionEffect,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct SandboxConfig {
    /// Apply an OS sandbox to commands and background tasks.
    pub enabled: bool,
    /// Permit network access from sandboxed processes.
    pub network: bool,
    /// Additional paths that sandboxed processes may modify.
    pub writable_paths: Vec<PathBuf>,
}

#[allow(clippy::derivable_impls)]
impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            // Unit tests exercise process plumbing directly. Production starts
            // sandboxed unless the user explicitly disables it.
            enabled: cfg!(all(not(test), unix)),
            network: false,
            writable_paths: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct SecurityConfig {
    pub enabled: bool,
    /// Existing files must be read before they can be overwritten and must not
    /// have changed since that read.
    pub protect_file_changes: bool,
    /// Direct file reads are allowed outside the project unless a rule denies
    /// them. Writes remain project-scoped by default.
    pub allow_read_outside_workspace: bool,
    pub rules: Vec<PermissionRule>,
    pub sandbox: SandboxConfig,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            protect_file_changes: true,
            allow_read_outside_workspace: true,
            rules: Vec::new(),
            sandbox: SandboxConfig::default(),
        }
    }
}

#[derive(Debug)]
struct CompiledRule {
    operation: AccessKind,
    effect: PermissionEffect,
    matcher: GlobMatcher,
}

pub(crate) struct SecurityManager {
    workspace: PathBuf,
    config: SecurityConfig,
    rules: Vec<CompiledRule>,
    snapshots: Mutex<HashMap<PathBuf, blake3::Hash>>,
    dirty: Arc<Mutex<HashSet<PathBuf>>>,
    _watcher: Option<notify::RecommendedWatcher>,
}

impl SecurityManager {
    pub(crate) fn new(config: SecurityConfig, workspace: PathBuf) -> Result<Self, String> {
        let workspace = normalize_existing(&workspace)?;
        let rules = config
            .rules
            .iter()
            .map(|rule| {
                let expanded = expand_pattern(&rule.pattern, &workspace);
                let matcher = Glob::new(&expanded)
                    .map_err(|error| {
                        format!("invalid security rule pattern '{}': {error}", rule.pattern)
                    })?
                    .compile_matcher();
                Ok(CompiledRule {
                    operation: rule.operation,
                    effect: rule.effect,
                    matcher,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let dirty = Arc::new(Mutex::new(HashSet::new()));
        let watcher = if config.enabled && config.protect_file_changes {
            let dirty_for_events = dirty.clone();
            let mut watcher =
                notify::recommended_watcher(move |event: Result<notify::Event, notify::Error>| {
                    if let Ok(event) = event
                        && let Ok(mut dirty) = dirty_for_events.lock()
                    {
                        dirty.extend(event.paths);
                    }
                })
                .map_err(|error| format!("could not start project file monitor: {error}"))?;
            watcher
                .watch(&workspace, RecursiveMode::Recursive)
                .map_err(|error| {
                    format!("could not monitor project directory for changes: {error}")
                })?;
            Some(watcher)
        } else {
            None
        };
        Ok(Self {
            workspace,
            config,
            rules,
            snapshots: Mutex::new(HashMap::new()),
            dirty,
            _watcher: watcher,
        })
    }

    pub(crate) fn for_current_dir(config: SecurityConfig) -> Result<Self, String> {
        let workspace = std::env::current_dir()
            .map_err(|error| format!("could not determine project directory: {error}"))?;
        Self::new(config, workspace)
    }

    pub(crate) fn standalone() -> Result<Self, String> {
        let config = SecurityConfig {
            enabled: false,
            protect_file_changes: false,
            ..SecurityConfig::default()
        };
        Self::for_current_dir(config)
    }

    pub(crate) fn resolve_path(&self, path: impl AsRef<Path>) -> Result<PathBuf, String> {
        let expanded;
        let path = path.as_ref();
        let path = if path == Path::new("~") {
            expanded = dirs::home_dir().unwrap_or_else(|| path.to_path_buf());
            &expanded
        } else if let Ok(rest) = path.strip_prefix("~") {
            expanded = dirs::home_dir()
                .map(|home| home.join(rest))
                .unwrap_or_else(|| path.to_path_buf());
            &expanded
        } else {
            path
        };
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.workspace.join(path)
        };
        normalize_allow_missing(&absolute)
    }

    pub(crate) fn authorize_path(
        &self,
        operation: AccessKind,
        path: impl AsRef<Path>,
    ) -> Result<PathBuf, String> {
        let path = self.resolve_path(path)?;
        if !self.config.enabled {
            return Ok(path);
        }

        let candidate = slash_path(&path);
        if self.rules.iter().any(|rule| {
            rule.operation == operation
                && rule.effect == PermissionEffect::Deny
                && rule.matcher.is_match(&candidate)
        }) {
            return Err(format!(
                "security policy denied {operation:?} access to {}",
                path.display()
            ));
        }
        if self.rules.iter().any(|rule| {
            rule.operation == operation
                && rule.effect == PermissionEffect::Allow
                && rule.matcher.is_match(&candidate)
        }) {
            return Ok(path);
        }

        let in_workspace = path.starts_with(&self.workspace);
        let allowed = match operation {
            AccessKind::Read => in_workspace || self.config.allow_read_outside_workspace,
            AccessKind::Write | AccessKind::Execute => in_workspace,
            AccessKind::Network => false,
        };
        if allowed {
            Ok(path)
        } else {
            Err(format!(
                "security policy denied {operation:?} access outside the project: {}",
                path.display()
            ))
        }
    }

    pub(crate) fn authorize_tool_call(&self, name: &str, arguments: &str) -> Result<(), String> {
        let operation = match name {
            crate::tools::read_file::NAME | crate::tools::read_image::NAME => AccessKind::Read,
            crate::tools::write_file::NAME | crate::tools::edit_file::NAME => AccessKind::Write,
            _ => return Ok(()),
        };
        let arguments: serde_json::Value = serde_json::from_str(arguments)
            .map_err(|error| format!("invalid tool arguments for security check: {error}"))?;
        let path = arguments
            .get("path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "missing path for security check".to_string())?;
        self.authorize_path(operation, path).map(|_| ())
    }

    pub(crate) fn record_read(&self, path: &Path, contents: &[u8]) {
        if !self.config.enabled || !self.config.protect_file_changes {
            return;
        }
        if let Ok(mut snapshots) = self.snapshots.lock() {
            snapshots.insert(path.to_path_buf(), blake3::hash(contents));
        }
        if let Ok(mut dirty) = self.dirty.lock() {
            dirty.remove(path);
        }
    }

    pub(crate) fn validate_write(
        &self,
        path: &Path,
        contents: Option<&[u8]>,
    ) -> Result<(), String> {
        if !self.config.enabled || !self.config.protect_file_changes || contents.is_none() {
            return Ok(());
        }
        let expected = self
            .snapshots
            .lock()
            .map_err(|_| "file snapshot store is unavailable".to_string())?
            .get(path)
            .copied()
            .ok_or_else(|| {
                format!(
                    "refusing to overwrite {} because it has not been read in this session",
                    path.display()
                )
            })?;
        let actual = blake3::hash(contents.expect("checked above"));
        if actual != expected {
            return Err(format!(
                "refusing to overwrite {} because it changed after the last read; read it again before editing",
                path.display()
            ));
        }
        Ok(())
    }

    pub(crate) fn sandbox_invocation(
        &self,
        command: &str,
        dir: Option<&str>,
    ) -> Result<Option<crate::security::SandboxInvocation>, String> {
        crate::security::sandbox::invocation(self, command, dir)
    }

    pub(crate) fn sandbox_program_invocation(
        &self,
        program: &str,
        args: &[String],
        dir: Option<&str>,
    ) -> Result<Option<crate::security::SandboxInvocation>, String> {
        crate::security::sandbox::program_invocation(self, program, args, dir)
    }

    pub(super) fn sandbox_config(&self) -> &SandboxConfig {
        &self.config.sandbox
    }

    pub(super) fn workspace(&self) -> &Path {
        &self.workspace
    }

    pub(crate) fn status_text(&self) -> String {
        let report = skarn_sandbox::backend_report();
        let sandbox = &self.config.sandbox;
        format!(
            "Security:\n  sandbox: {}\n  backend: {} ({:?})\n  network: {}\n  workspace: {}\n  extra writable paths: {}\n  file conflict protection: {}\n  permission rules: {}",
            if sandbox.enabled {
                "enabled"
            } else {
                "disabled"
            },
            report.backend,
            report.status,
            if sandbox.network { "allowed" } else { "denied" },
            self.workspace.display(),
            sandbox.writable_paths.len(),
            if self.config.protect_file_changes {
                "enabled"
            } else {
                "disabled"
            },
            self.config.rules.len(),
        )
    }
}

fn expand_pattern(pattern: &str, workspace: &Path) -> String {
    let workspace = slash_path(workspace);
    if pattern == "workspace" {
        workspace
    } else if let Some(rest) = pattern.strip_prefix("workspace/") {
        format!("{workspace}/{rest}")
    } else {
        pattern.to_string()
    }
}

fn slash_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn normalize_existing(path: &Path) -> Result<PathBuf, String> {
    path.canonicalize()
        .map_err(|error| format!("could not resolve {}: {error}", path.display()))
}

fn normalize_allow_missing(path: &Path) -> Result<PathBuf, String> {
    if path.exists() {
        return normalize_existing(path);
    }

    let mut missing = Vec::new();
    let mut existing = path;
    while !existing.exists() {
        let name = existing.file_name().ok_or_else(|| {
            format!(
                "could not resolve path without an existing parent: {}",
                path.display()
            )
        })?;
        missing.push(name.to_os_string());
        existing = existing.parent().ok_or_else(|| {
            format!(
                "could not resolve path without an existing parent: {}",
                path.display()
            )
        })?;
    }
    let mut normalized = normalize_existing(existing)?;
    for part in missing.iter().rev() {
        if Path::new(part)
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::RootDir))
        {
            return Err(format!("invalid path component in {}", path.display()));
        }
        normalized.push(part);
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manager(config: SecurityConfig, workspace: &Path) -> SecurityManager {
        SecurityManager::new(config, workspace.to_path_buf()).unwrap()
    }

    #[test]
    fn default_policy_allows_workspace_writes_and_external_reads() {
        let root =
            std::env::temp_dir().join(format!("programmer-security-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let security = manager(SecurityConfig::default(), &root);

        assert!(
            security
                .authorize_path(AccessKind::Write, root.join("new.txt"))
                .is_ok()
        );
        assert!(
            security
                .authorize_path(AccessKind::Read, std::env::temp_dir())
                .is_ok()
        );
        assert!(
            security
                .authorize_path(AccessKind::Write, std::env::temp_dir())
                .is_err()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn explicit_deny_wins_over_allow() {
        let root =
            std::env::temp_dir().join(format!("programmer-security-{}", uuid::Uuid::new_v4()));
        let private = root.join("private");
        std::fs::create_dir_all(&private).unwrap();
        let config = SecurityConfig {
            rules: vec![
                PermissionRule {
                    operation: AccessKind::Write,
                    pattern: "workspace/**".into(),
                    effect: PermissionEffect::Allow,
                },
                PermissionRule {
                    operation: AccessKind::Write,
                    pattern: "workspace/private/**".into(),
                    effect: PermissionEffect::Deny,
                },
            ],
            ..SecurityConfig::default()
        };
        let security = manager(config, &root);

        assert!(
            security
                .authorize_path(AccessKind::Write, root.join("public.txt"))
                .is_ok()
        );
        assert!(
            security
                .authorize_path(AccessKind::Write, private.join("secret.txt"))
                .is_err()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_paths_are_normalized_against_existing_parent() {
        let root =
            std::env::temp_dir().join(format!("programmer-security-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let security = manager(SecurityConfig::default(), &root);
        let resolved = security
            .resolve_path(root.join("missing").join("file.txt"))
            .unwrap();
        assert_eq!(
            resolved,
            root.canonicalize()
                .unwrap()
                .join("missing")
                .join("file.txt")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn write_requires_a_current_read_snapshot() {
        let root =
            std::env::temp_dir().join(format!("programmer-security-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("file.txt");
        std::fs::write(&path, b"one").unwrap();
        let security = manager(SecurityConfig::default(), &root);

        assert!(security.validate_write(&path, Some(b"one")).is_err());
        security.record_read(&path, b"one");
        assert!(security.validate_write(&path, Some(b"one")).is_ok());
        assert!(security.validate_write(&path, Some(b"two")).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
