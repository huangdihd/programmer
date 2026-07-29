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

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

pub(crate) const DEFAULT_SECURITY_PROFILE: &str = "default";

pub(crate) fn validate_security_profile_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("profile name is required".to_string());
    }
    if name
        .chars()
        .any(|character| !(character.is_ascii_alphanumeric() || "_-.".contains(character)))
    {
        return Err("use only letters, numbers, '.', '-' or '_'".to_string());
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ProgrammerConfig {
    /// The provider to use when none is specified in the model string.
    pub default_provider: String,
    /// All configured providers, keyed by name.
    pub providers: HashMap<String, ProviderConfig>,
    /// Model used by the Auto-mode LLM tool-call classifier, as a
    /// `provider/model` string. When absent, the current chat model is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classifier_model: Option<String>,
    /// YOLO mode (run every tool call unchecked) is gated behind this flag so
    /// it can't be reached by the normal Ctrl+T cycle or a bare `/mode yolo`.
    #[serde(default)]
    pub allow_yolo: bool,
    /// Mandatory filesystem and process isolation. Unlike work-mode approval,
    /// these restrictions still apply in YOLO mode. This field only accepts
    /// legacy single-policy configs; current configs serialize profiles below.
    #[serde(default, skip_serializing)]
    pub security: crate::security::SecurityConfig,
    /// Named security policies. The active profile is copied into `security`
    /// at load time so runtime consumers keep one immutable policy snapshot.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub security_profiles: BTreeMap<String, crate::security::SecurityConfig>,
    /// Name of the security profile currently enforced by local tools.
    #[serde(default = "default_security_profile_name")]
    pub active_security_profile: String,
    /// Co-author identity (`Name <email>`) the agent adds as a
    /// `Co-Authored-By:` trailer to git commit messages it writes. For the
    /// co-author to show a GitHub avatar, the email must belong to a GitHub
    /// account — e.g. that account's `<id>+<username>@users.noreply.github.com`
    /// no-reply address. Set to omit/null to disable the trailer.
    #[serde(
        default = "default_git_coauthor",
        skip_serializing_if = "Option::is_none"
    )]
    pub git_coauthor: Option<String>,
    /// Configured MCP (Model Context Protocol) servers. Each entry is spawned
    /// as a child process at startup; its tools are bridged into the tool list
    /// as `mcp__<server>__<tool>`. Empty by default (no servers, no overhead).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) mcp_servers: Vec<crate::mcp::types::McpServerConfig>,
    // Legacy fields for backward compatibility with v0.1.x configs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProviderConfig {
    pub base_url: String,
    pub api_key: String,
    /// Optional explicit model list. When absent, models are auto-discovered
    /// from the provider's `/models` endpoint at startup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models: Option<Vec<String>>,
    /// Default model for this provider. When absent, the first model from the
    /// list (auto-discovered or manual) is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    /// Model used for Auto-mode tool-call classification for this provider.
    /// When absent, falls back to [`default_model`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classifier_model: Option<String>,
}

/// Default co-author trailer. It's a placeholder — replace the email with one
/// tied to a GitHub account to get an avatar (see [`ProgrammerConfig::git_coauthor`]).
fn default_git_coauthor() -> Option<String> {
    Some("programmer <noreply@programmer.local>".to_string())
}

fn default_security_profile_name() -> String {
    DEFAULT_SECURITY_PROFILE.to_string()
}

impl Default for ProgrammerConfig {
    fn default() -> Self {
        let mut providers = HashMap::new();
        providers.insert(
            "openai".to_string(),
            ProviderConfig {
                base_url: "https://api.openai.com/v1".to_string(),
                api_key: "sk-...".to_string(),
                models: None,
                default_model: None,
                classifier_model: None,
            },
        );
        let security = crate::security::SecurityConfig::default();
        ProgrammerConfig {
            default_provider: "openai".to_string(),
            providers,
            classifier_model: None,
            allow_yolo: false,
            security,
            // Kept empty until normalization so deserialization can distinguish
            // a legacy `[security]` table from current named profiles.
            security_profiles: BTreeMap::new(),
            active_security_profile: default_security_profile_name(),
            git_coauthor: default_git_coauthor(),
            mcp_servers: Vec::new(),
            model: None,
            base_url: None,
            api_key: None,
        }
    }
}

impl ProgrammerConfig {
    /// Migrate a v0.1.x config (which only has `model`, `base_url`, `api_key`)
    /// by promoting the legacy fields into a single "openai" provider entry.
    /// Returns `true` if migration happened, so the caller can persist the new
    /// config format back to disk.
    pub fn migrate_if_needed(&mut self) -> bool {
        let mut changed = self.migrate_provider_config();
        changed |= self.normalize_security_profiles();
        changed
    }

    fn migrate_provider_config(&mut self) -> bool {
        if !self.providers.is_empty() {
            return false;
        }
        let Some(base_url) = self
            .base_url
            .as_ref()
            .filter(|value| value.as_str() != "Type your base_url here")
            .cloned()
        else {
            return false;
        };
        let Some(api_key) = self
            .api_key
            .as_ref()
            .filter(|value| value.as_str() != "Type your api_key here")
            .cloned()
        else {
            return false;
        };
        let model = self.model.clone().unwrap_or_else(|| "gpt-4o".to_string());

        self.providers.insert(
            "openai".to_string(),
            ProviderConfig {
                base_url,
                api_key,
                models: Some(vec![model]),
                default_model: None,
                classifier_model: None,
            },
        );
        self.default_provider = "openai".to_string();
        self.model = None;
        self.base_url = None;
        self.api_key = None;
        true
    }

    /// Ensure an active named profile exists and mirror it into the runtime
    /// `security` field. Returns whether persisted profile metadata changed.
    pub(crate) fn normalize_security_profiles(&mut self) -> bool {
        let mut changed = false;
        if self.security_profiles.is_empty() {
            self.security_profiles
                .insert(DEFAULT_SECURITY_PROFILE.to_string(), self.security.clone());
            changed = true;
        }
        if !self
            .security_profiles
            .contains_key(&self.active_security_profile)
        {
            self.active_security_profile = self
                .security_profiles
                .first_key_value()
                .map(|(name, _)| name.clone())
                .unwrap_or_else(default_security_profile_name);
            changed = true;
        }
        self.security = self
            .security_profiles
            .get(&self.active_security_profile)
            .expect("normalized security profile must exist")
            .clone();
        changed
    }

    /// Update both the active named profile and the live runtime copy.
    pub(crate) fn update_active_security(
        &mut self,
        update: impl FnOnce(&mut crate::security::SecurityConfig),
    ) {
        update(&mut self.security);
        self.security_profiles
            .insert(self.active_security_profile.clone(), self.security.clone());
    }

    pub(crate) fn activate_security_profile(&mut self, name: &str) -> Result<(), String> {
        let Some(profile) = self.security_profiles.get(name).cloned() else {
            return Err(format!("unknown security profile '{name}'"));
        };
        self.active_security_profile = name.to_string();
        self.security = profile;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::types::McpServerConfig;

    #[test]
    fn mcp_servers_round_trip_through_toml() {
        // TOML places array-of-tables after scalar keys; make sure a config
        // carrying MCP servers (with args + env) serializes and parses back.
        let mut config = ProgrammerConfig::default();
        config.mcp_servers.push(McpServerConfig {
            name: "filesystem".into(),
            command: "npx".into(),
            args: vec![
                "-y".into(),
                "@modelcontextprotocol/server-filesystem".into(),
            ],
            env: std::collections::HashMap::from([("API_KEY".to_string(), "secret".to_string())]),
            url: None,
            auto_approve: Default::default(),
        });

        let serialized = toml::to_string(&config).expect("serialize");
        let parsed: ProgrammerConfig = toml::from_str(&serialized).expect("deserialize");

        assert_eq!(parsed.mcp_servers.len(), 1);
        assert_eq!(parsed.mcp_servers[0].name, "filesystem");
        assert_eq!(parsed.mcp_servers[0].command, "npx");
        assert_eq!(parsed.mcp_servers[0].args.len(), 2);
        assert_eq!(parsed.mcp_servers[0].env.get("API_KEY").unwrap(), "secret");
    }

    #[test]
    fn empty_mcp_servers_not_serialized() {
        // With no servers the key is skipped entirely (no empty array noise).
        let config = ProgrammerConfig::default();
        let serialized = toml::to_string(&config).expect("serialize");
        assert!(!serialized.contains("mcp_servers"));
    }

    #[test]
    fn generated_config_exposes_the_complete_sandbox_policy() {
        let mut config = ProgrammerConfig::default();
        config.normalize_security_profiles();
        let serialized = toml::to_string(&config).expect("serialize");

        for field in [
            "allow_system_read",
            "allow_temp_write",
            "fail_closed",
            "readable_paths",
            "writable_paths",
            "denied_read_paths",
            "denied_environment",
        ] {
            assert!(
                serialized.contains(field),
                "generated config omitted sandbox field {field}"
            );
        }
    }

    #[test]
    fn legacy_single_security_policy_migrates_to_default_profile() {
        let source = r#"
[security]
enabled = false
protect_file_changes = false

[security.sandbox]
enabled = true
network = true
"#;
        let mut config: ProgrammerConfig =
            toml::from_str(source).expect("deserialize legacy config");

        assert!(config.security_profiles.is_empty());
        assert!(config.migrate_if_needed());
        assert_eq!(config.active_security_profile, DEFAULT_SECURITY_PROFILE);
        assert_eq!(
            config.security_profiles[DEFAULT_SECURITY_PROFILE],
            config.security
        );
        assert!(!config.security.enabled);
        assert!(config.security.sandbox.network);
    }

    #[test]
    fn named_security_profiles_round_trip_and_restore_active_policy() {
        let mut config = ProgrammerConfig::default();
        config.normalize_security_profiles();
        let mut network = config.security.clone();
        network.sandbox.network = true;
        config
            .security_profiles
            .insert("network".to_string(), network.clone());
        config
            .activate_security_profile("network")
            .expect("activate");

        let serialized = toml::to_string(&config).expect("serialize");
        assert!(!serialized.contains("\n[security]\n"));
        let mut restored: ProgrammerConfig = toml::from_str(&serialized).expect("deserialize");
        restored.normalize_security_profiles();

        assert_eq!(restored.active_security_profile, "network");
        assert_eq!(restored.security, network);
    }
}
