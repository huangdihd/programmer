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
use std::collections::HashMap;

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
    /// Co-author identity (`Name <email>`) the agent adds as a
    /// `Co-Authored-By:` trailer to git commit messages it writes. For the
    /// co-author to show a GitHub avatar, the email must belong to a GitHub
    /// account — e.g. that account's `<id>+<username>@users.noreply.github.com`
    /// no-reply address. Set to omit/null to disable the trailer.
    #[serde(default = "default_git_coauthor", skip_serializing_if = "Option::is_none")]
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
}

/// Default co-author trailer. It's a placeholder — replace the email with one
/// tied to a GitHub account to get an avatar (see [`ProgrammerConfig::git_coauthor`]).
fn default_git_coauthor() -> Option<String> {
    Some("programmer <noreply@programmer.local>".to_string())
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
            },
        );
        ProgrammerConfig {
            default_provider: "openai".to_string(),
            providers,
            classifier_model: None,
            allow_yolo: false,
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
        if !self.providers.is_empty() {
            return false;
        }

        let base_url = match &self.base_url {
            Some(u) if u != "Type your base_url here" => u.clone(),
            _ => return false,
        };
        let api_key = match &self.api_key {
            Some(k) if k != "Type your api_key here" => k.clone(),
            _ => return false,
        };
        let model = self.model.clone().unwrap_or_else(|| "gpt-4o".to_string());

        self.providers.insert(
            "openai".to_string(),
            ProviderConfig {
                base_url,
                api_key,
                models: Some(vec![model]),
                default_model: None,
            },
        );
        self.default_provider = "openai".to_string();
        // Clear legacy fields so they aren't serialized back.
        self.model = None;
        self.base_url = None;
        self.api_key = None;
        true
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
            args: vec!["-y".into(), "@modelcontextprotocol/server-filesystem".into()],
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
}
