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
use std::sync::{Arc, LazyLock, Mutex, RwLock};

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AccessKind {
    Read,
    Write,
    Execute,
    Network,
}

impl AccessKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Execute => "execute",
            Self::Network => "network",
        }
    }
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

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct SandboxConfig {
    /// Apply an OS sandbox to commands and background tasks.
    pub enabled: bool,
    /// Permit network access from sandboxed processes.
    pub network: bool,
    /// Permit reads required for system programs and shared libraries.
    pub allow_system_read: bool,
    /// Permit sandboxed processes to modify the platform temporary directory.
    pub allow_temp_write: bool,
    /// Refuse to run when the platform backend cannot enforce the policy.
    pub fail_closed: bool,
    /// Additional paths that sandboxed processes may read.
    pub readable_paths: Vec<PathBuf>,
    /// Additional paths that sandboxed processes may modify.
    pub writable_paths: Vec<PathBuf>,
    /// Paths that sandboxed processes must not read.
    pub denied_read_paths: Vec<PathBuf>,
    /// Environment variable name globs removed from sandboxed processes.
    pub denied_environment: Vec<String>,
}

/// Coarse process-isolation modes exposed in the UI and permission controls.
/// Other sandbox settings (paths, environment, temporary writes) remain
/// independently configurable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SandboxMode {
    Restricted,
    #[default]
    Network,
    Off,
}

impl SandboxMode {
    pub(crate) const VALUES: &'static [&'static str] = &["restricted", "network", "off"];

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "restricted" | "default" => Some(Self::Restricted),
            "network" => Some(Self::Network),
            "off" | "disabled" => Some(Self::Off),
            _ => None,
        }
    }

    pub(crate) fn from_config(config: &SandboxConfig) -> Self {
        if !config.enabled {
            Self::Off
        } else if config.network {
            Self::Network
        } else {
            Self::Restricted
        }
    }

    pub(crate) fn apply(self, config: &mut SandboxConfig) {
        match self {
            Self::Restricted => {
                config.enabled = true;
                config.network = false;
            }
            Self::Network => {
                config.enabled = true;
                config.network = true;
            }
            Self::Off => config.enabled = false,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Restricted => "restricted",
            Self::Network => "network",
            Self::Off => "off",
        }
    }

    /// Whether changing from `self` to `target` relaxes isolation. Permission
    /// requests may only broaden access; tightening policy belongs in explicit
    /// user configuration rather than an agent-initiated access request.
    pub(crate) fn can_request(self, target: Self) -> bool {
        fn access_level(mode: SandboxMode) -> u8 {
            match mode {
                SandboxMode::Restricted => 0,
                SandboxMode::Network => 1,
                SandboxMode::Off => 2,
            }
        }
        access_level(target) > access_level(self)
    }
}

impl Default for SandboxConfig {
    fn default() -> Self {
        let policy = &*DEFAULT_SANDBOX_POLICY;
        Self {
            enabled: cfg!(all(not(test), unix)),
            network: policy.network,
            allow_system_read: policy.allow_system_read,
            allow_temp_write: policy.allow_temp_write,
            fail_closed: policy.fail_closed,
            readable_paths: policy.readable_paths.clone(),
            writable_paths: policy.writable_paths.clone(),
            denied_read_paths: policy.denied_read_paths.clone(),
            denied_environment: policy.denied_environment.clone(),
        }
    }
}

#[derive(Deserialize)]
struct BundledSandboxPolicy {
    network: bool,
    allow_system_read: bool,
    allow_temp_write: bool,
    fail_closed: bool,
    readable_paths: Vec<PathBuf>,
    writable_paths: Vec<PathBuf>,
    denied_read_paths: Vec<PathBuf>,
    denied_environment: Vec<String>,
}

static DEFAULT_SANDBOX_POLICY: LazyLock<BundledSandboxPolicy> = LazyLock::new(|| {
    toml::from_str(include_str!("default_sandbox.toml"))
        .expect("bundled sandbox defaults must be valid TOML")
});

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SessionGrant {
    operation: AccessKind,
    path: PathBuf,
    recursive: bool,
}

impl SessionGrant {
    fn sandbox_directory(&self) -> Option<PathBuf> {
        // The sandbox API interprets every path as a subtree. Exact-file grants
        // cannot be represented without risking broader access if the file is
        // later replaced by a directory, so keep them out of child policies.
        if !self.recursive {
            return None;
        }
        // A stored grant is canonical. Re-resolve it before propagation so an
        // ancestor replaced by a symlink cannot redirect the approved pathname
        // to a different subtree.
        let current = self.path.canonicalize().ok()?;
        (current == self.path && current.is_dir()).then_some(current)
    }

    fn sandbox_intersection(&self, other: &Self) -> Option<PathBuf> {
        let path = self.sandbox_directory()?;
        let other_path = other.sandbox_directory()?;
        if path.starts_with(&other_path) {
            return Some(path);
        }
        if other_path.starts_with(&path) {
            return Some(other_path);
        }
        None
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct PersistedFileSnapshot {
    pub(crate) path: PathBuf,
    pub(crate) hash: [u8; 32],
}

pub(crate) struct SecurityManager {
    workspace: PathBuf,
    config: SecurityConfig,
    rules: Vec<CompiledRule>,
    session_grants: RwLock<HashSet<SessionGrant>>,
    snapshots: Mutex<HashMap<(u64, PathBuf), blake3::Hash>>,
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
            session_grants: RwLock::new(HashSet::new()),
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

        if self
            .session_grants
            .read()
            .map_err(|_| "session permission store is unavailable".to_string())?
            .iter()
            .any(|grant| {
                grant.operation == operation
                    && (path == grant.path || grant.recursive && path.starts_with(&grant.path))
            })
        {
            return Ok(path);
        }

        let candidate = slash_path(&path);
        if self.rules.iter().any(|rule| {
            rule.operation == operation
                && rule.effect == PermissionEffect::Deny
                && rule.matcher.is_match(&candidate)
        }) {
            return Err(format!(
                "security policy denied {} access to {}; call request_permission with kind \
                 'filesystem', operation '{}', and this path to ask the user for session access",
                operation.label(),
                path.display(),
                operation.label()
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
                "security policy denied {} access outside the project: {}; call \
                 request_permission with kind 'filesystem', operation '{}', and this path to ask \
                 the user for session access",
                operation.label(),
                path.display(),
                operation.label()
            ))
        }
    }

    #[cfg(test)]
    pub(crate) fn grant_path(
        &self,
        operation: AccessKind,
        path: impl AsRef<Path>,
        recursive: bool,
    ) -> Result<PathBuf, String> {
        let path = self.resolve_path(path)?;
        self.grant_resolved_path(operation, path, recursive)
    }

    pub(crate) fn grant_path_if_unchanged(
        &self,
        operation: AccessKind,
        requested_path: impl AsRef<Path>,
        approved_path: impl AsRef<Path>,
        recursive: bool,
    ) -> Result<PathBuf, String> {
        let path = self.resolve_path(requested_path)?;
        if path != approved_path.as_ref() {
            return Err(format!(
                "filesystem permission target changed while awaiting approval: expected {}, now {}",
                approved_path.as_ref().display(),
                path.display()
            ));
        }
        self.grant_resolved_path(operation, path, recursive)
    }

    fn grant_resolved_path(
        &self,
        operation: AccessKind,
        path: PathBuf,
        recursive: bool,
    ) -> Result<PathBuf, String> {
        if recursive && !path.is_dir() {
            return Err(format!(
                "recursive filesystem grants require an existing directory: {}",
                path.display()
            ));
        }
        if !recursive && path.is_dir() {
            return Err(format!(
                "exact filesystem grants cannot target a directory: {}",
                path.display()
            ));
        }
        self.session_grants
            .write()
            .map_err(|_| "session permission store is unavailable".to_string())?
            .insert(SessionGrant {
                operation,
                path: path.clone(),
                recursive,
            });
        Ok(path)
    }

    pub(crate) fn inherit_session_grants(&self, previous: &Self) -> Result<(), String> {
        let previous = previous
            .session_grants
            .read()
            .map_err(|_| "session permission store is unavailable".to_string())?
            .clone();
        self.session_grants
            .write()
            .map_err(|_| "session permission store is unavailable".to_string())?
            .extend(previous);
        Ok(())
    }

    pub(crate) fn session_grant_count(&self) -> usize {
        self.session_grants
            .read()
            .map(|grants| grants.len())
            .unwrap_or_default()
    }

    pub(crate) fn session_granted_paths(&self, operation: AccessKind) -> Vec<PathBuf> {
        self.session_grants
            .read()
            .map(|grants| {
                grants
                    .iter()
                    .filter(|grant| grant.operation == operation)
                    .filter_map(SessionGrant::sandbox_directory)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The process sandbox only exposes read-only and read-write path rules.
    /// Propagate write access only where the user separately granted read
    /// access, so a write-only grant cannot silently become read permission.
    pub(crate) fn session_read_write_paths(&self) -> Vec<PathBuf> {
        self.session_grants
            .read()
            .map(|grants| {
                let read_grants = grants
                    .iter()
                    .filter(|grant| grant.operation == AccessKind::Read);
                grants
                    .iter()
                    .filter(|grant| grant.operation == AccessKind::Write)
                    .flat_map(|write_grant| {
                        read_grants.clone().filter_map(move |read_grant| {
                            write_grant.sandbox_intersection(read_grant)
                        })
                    })
                    .collect::<HashSet<_>>()
                    .into_iter()
                    .collect()
            })
            .unwrap_or_default()
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

    pub(crate) fn persisted_snapshots(&self) -> Vec<PersistedFileSnapshot> {
        self.snapshots
            .lock()
            .map(|snapshots| {
                snapshots
                    .iter()
                    .filter_map(|((scope, path), hash)| {
                        (*scope == 0).then_some(PersistedFileSnapshot {
                            path: path.clone(),
                            hash: *hash.as_bytes(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn restore_snapshots(&self, snapshots: &[PersistedFileSnapshot]) {
        if let Ok(mut current) = self.snapshots.lock() {
            for snapshot in snapshots {
                current.insert(
                    (0, snapshot.path.clone()),
                    blake3::Hash::from_bytes(snapshot.hash),
                );
            }
        }
    }

    pub(crate) fn record_read(&self, path: &Path, contents: &[u8]) {
        self.record_read_scoped(0, path, contents);
    }

    pub(crate) fn record_read_scoped(&self, scope: u64, path: &Path, contents: &[u8]) {
        if !self.config.enabled || !self.config.protect_file_changes {
            return;
        }
        if let Ok(mut snapshots) = self.snapshots.lock() {
            snapshots.insert((scope, path.to_path_buf()), blake3::hash(contents));
        }
        if let Ok(mut dirty) = self.dirty.lock() {
            dirty.remove(path);
        }
    }

    pub(crate) fn clear_file_scope(&self, scope: u64) {
        if let Ok(mut snapshots) = self.snapshots.lock() {
            snapshots.retain(|(snapshot_scope, _), _| *snapshot_scope != scope);
        }
    }

    #[cfg(test)]
    pub(crate) fn validate_write(
        &self,
        path: &Path,
        contents: Option<&[u8]>,
    ) -> Result<(), String> {
        self.validate_write_scoped(0, path, contents)
    }

    pub(crate) fn validate_write_scoped(
        &self,
        scope: u64,
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
            .get(&(scope, path.to_path_buf()))
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

    pub(crate) fn sandbox_mode(&self) -> SandboxMode {
        SandboxMode::from_config(&self.config.sandbox)
    }

    pub(crate) fn security_config(&self) -> SecurityConfig {
        self.config.clone()
    }

    pub(crate) fn workspace_path(&self) -> PathBuf {
        self.workspace.clone()
    }

    pub(super) fn workspace(&self) -> &Path {
        &self.workspace
    }

    pub(crate) fn status_text(&self) -> String {
        let report = skarn_sandbox::backend_report();
        let sandbox = &self.config.sandbox;
        format!(
            "Security:\n  filesystem policy: {}\n  session filesystem grants: {}\n  sandbox mode: {}\n  sandbox: {}\n  backend: {} ({:?})\n  network: {}\n  system reads: {}\n  temporary writes: {}\n  fail closed: {}\n  workspace: {}\n  reads outside workspace: {}\n  extra readable paths: {}\n  extra writable paths: {}\n  denied read paths: {}\n  denied environment patterns: {}\n  file conflict protection: {}\n  permission rules: {}",
            if self.config.enabled {
                "enabled"
            } else {
                "disabled"
            },
            self.session_grant_count(),
            self.sandbox_mode().label(),
            if sandbox.enabled {
                "enabled"
            } else {
                "disabled"
            },
            report.backend,
            report.status,
            if sandbox.network { "allowed" } else { "denied" },
            if sandbox.allow_system_read {
                "allowed"
            } else {
                "denied"
            },
            if sandbox.allow_temp_write {
                "allowed"
            } else {
                "denied"
            },
            if sandbox.fail_closed {
                "enabled"
            } else {
                "disabled"
            },
            self.workspace.display(),
            if self.config.allow_read_outside_workspace {
                "allowed"
            } else {
                "denied"
            },
            sandbox.readable_paths.len(),
            sandbox.writable_paths.len(),
            sandbox.denied_read_paths.len(),
            sandbox.denied_environment.len(),
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
    fn sandbox_policy_lists_are_configurable() {
        let config: SandboxConfig = toml::from_str(
            r#"
enabled = true
network = true
allow_system_read = false
allow_temp_write = false
fail_closed = false
readable_paths = ["/custom/read"]
writable_paths = ["/custom/write"]
denied_read_paths = ["/custom/secret"]
denied_environment = ["CUSTOM_*"]
"#,
        )
        .unwrap();

        assert!(config.enabled);
        assert!(config.network);
        assert!(!config.allow_system_read);
        assert!(!config.allow_temp_write);
        assert!(!config.fail_closed);
        assert_eq!(config.readable_paths, [PathBuf::from("/custom/read")]);
        assert_eq!(config.writable_paths, [PathBuf::from("/custom/write")]);
        assert_eq!(config.denied_read_paths, [PathBuf::from("/custom/secret")]);
        assert_eq!(config.denied_environment, ["CUSTOM_*"]);
    }

    #[test]
    fn partial_sandbox_config_retains_policy_defaults() {
        let config: SandboxConfig = toml::from_str("enabled = true").unwrap();

        assert!(config.enabled);
        assert!(config.network);
        assert!(config.readable_paths.is_empty());
        assert!(config.writable_paths.is_empty());
        assert!(config.denied_read_paths.is_empty());
        assert!(config.denied_environment.is_empty());
        assert!(config.fail_closed);
    }

    #[test]
    fn default_security_keeps_file_protection_and_process_sandbox_on_in_production() {
        let config = SecurityConfig::default();

        assert!(config.protect_file_changes);
        assert_eq!(config.sandbox.enabled, cfg!(all(not(test), unix)));
        assert!(config.sandbox.network);
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
    fn explicit_session_grant_overrides_matching_deny_rule() {
        let root =
            std::env::temp_dir().join(format!("programmer-security-{}", uuid::Uuid::new_v4()));
        let private = root.join("private");
        std::fs::create_dir_all(&private).unwrap();
        let config = SecurityConfig {
            rules: vec![PermissionRule {
                operation: AccessKind::Write,
                pattern: "workspace/private/**".into(),
                effect: PermissionEffect::Deny,
            }],
            ..SecurityConfig::default()
        };
        let security = manager(config, &root);
        let secret = private.join("secret.txt");

        assert!(security.authorize_path(AccessKind::Write, &secret).is_err());
        security.grant_path(AccessKind::Write, &root, true).unwrap();
        assert!(security.authorize_path(AccessKind::Write, &secret).is_ok());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn session_file_grant_allows_only_the_exact_operation_and_path() {
        let root =
            std::env::temp_dir().join(format!("programmer-security-{}", uuid::Uuid::new_v4()));
        let external =
            std::env::temp_dir().join(format!("programmer-external-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&external).unwrap();
        let denied = external.join("denied.txt");
        let granted = external.join("granted.txt");
        let config = SecurityConfig {
            allow_read_outside_workspace: false,
            ..SecurityConfig::default()
        };
        let security = manager(config, &root);

        let denial = security
            .authorize_path(AccessKind::Write, &granted)
            .unwrap_err();
        assert!(denial.contains("call request_permission"));
        assert!(denial.contains("operation 'write'"));
        security
            .grant_path(AccessKind::Write, &granted, false)
            .unwrap();

        assert!(security.authorize_path(AccessKind::Write, &granted).is_ok());
        assert!(security.authorize_path(AccessKind::Read, &granted).is_err());
        assert!(security.authorize_path(AccessKind::Write, &denied).is_err());
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(external).unwrap();
    }

    #[test]
    fn session_directory_grant_allows_descendants_but_not_prefix_siblings() {
        let root =
            std::env::temp_dir().join(format!("programmer-security-{}", uuid::Uuid::new_v4()));
        let external =
            std::env::temp_dir().join(format!("programmer-external-{}", uuid::Uuid::new_v4()));
        let granted_directory = external.join("granted");
        let sibling = external.join("granted-sibling");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(granted_directory.join("nested")).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        let security = manager(
            SecurityConfig {
                allow_read_outside_workspace: false,
                ..SecurityConfig::default()
            },
            &root,
        );

        security
            .grant_path(AccessKind::Write, &granted_directory, true)
            .unwrap();

        assert!(
            security
                .authorize_path(AccessKind::Write, granted_directory.join("nested/file.txt"))
                .is_ok()
        );
        assert!(
            security
                .authorize_path(AccessKind::Write, sibling.join("file.txt"))
                .is_err()
        );
        assert!(
            security
                .authorize_path(AccessKind::Read, granted_directory.join("nested/file.txt"))
                .is_err()
        );
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(external).unwrap();
    }

    #[test]
    fn sandbox_paths_include_only_recursive_existing_directories() {
        let root =
            std::env::temp_dir().join(format!("programmer-security-{}", uuid::Uuid::new_v4()));
        let directory = root.join("directory");
        let exact_file = root.join("exact.txt");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(&exact_file, b"exact").unwrap();
        let security = manager(SecurityConfig::default(), &root);

        security
            .grant_path(AccessKind::Read, &directory, true)
            .unwrap();
        security
            .grant_path(AccessKind::Read, &exact_file, false)
            .unwrap();
        assert_eq!(
            security.session_granted_paths(AccessKind::Read),
            [directory.canonicalize().unwrap()]
        );

        std::fs::remove_file(&exact_file).unwrap();
        std::fs::create_dir(&exact_file).unwrap();
        assert_eq!(
            security.session_granted_paths(AccessKind::Read),
            [directory.canonicalize().unwrap()]
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn redirected_recursive_grants_do_not_reach_child_policies() {
        let root =
            std::env::temp_dir().join(format!("programmer-security-{}", uuid::Uuid::new_v4()));
        let external =
            std::env::temp_dir().join(format!("programmer-external-{}", uuid::Uuid::new_v4()));
        let parent = external.join("parent");
        let original_parent = external.join("original-parent");
        let granted = parent.join("granted");
        let redirected = external.join("redirected");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&granted).unwrap();
        std::fs::create_dir_all(redirected.join("granted")).unwrap();
        let security = manager(SecurityConfig::default(), &root);
        for operation in [AccessKind::Read, AccessKind::Write] {
            security.grant_path(operation, &granted, true).unwrap();
        }

        std::fs::rename(&parent, &original_parent).unwrap();
        std::os::unix::fs::symlink(&redirected, &parent).unwrap();

        assert!(security.session_granted_paths(AccessKind::Read).is_empty());
        assert!(security.session_read_write_paths().is_empty());
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(external).unwrap();
    }

    #[test]
    fn sandbox_read_write_paths_use_only_recursive_scope_intersections() {
        let root =
            std::env::temp_dir().join(format!("programmer-security-{}", uuid::Uuid::new_v4()));
        let read_parent = root.join("read-parent");
        let shared_child = read_parent.join("shared-child");
        let write_only = root.join("write-only");
        let exact_file = root.join("exact.txt");
        std::fs::create_dir_all(&shared_child).unwrap();
        std::fs::create_dir_all(&write_only).unwrap();
        std::fs::write(&exact_file, b"exact").unwrap();
        let security = manager(SecurityConfig::default(), &root);

        security
            .grant_path(AccessKind::Read, &read_parent, true)
            .unwrap();
        security
            .grant_path(AccessKind::Write, &shared_child, true)
            .unwrap();
        security
            .grant_path(AccessKind::Write, &write_only, true)
            .unwrap();
        for operation in [AccessKind::Read, AccessKind::Write] {
            security.grant_path(operation, &exact_file, false).unwrap();
        }

        assert_eq!(
            security.session_read_write_paths(),
            [shared_child.canonicalize().unwrap()]
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
