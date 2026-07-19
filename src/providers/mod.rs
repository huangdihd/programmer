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

use crate::config::programmer_config::{ProgrammerConfig, ProviderConfig};
use async_openai::{Client, config::OpenAIConfig};
use std::collections::HashMap;
use std::time::Duration;

/// How long to wait for a provider's `/models` endpoint before giving up, so
/// startup never hangs when there is no network.
const MODEL_FETCH_TIMEOUT: Duration = Duration::from_secs(5);

/// Manages multiple OpenAI-compatible providers, each with its own API key,
/// base URL, and model list (auto-discovered or manually configured).
pub struct ProviderManager {
    clients: HashMap<String, Client<OpenAIConfig>>,
    /// Resolved models per provider.
    models: HashMap<String, Vec<String>>,
    configs: HashMap<String, ProviderConfig>,
    default_provider: String,
    /// Errors from startup model discovery, surfaced in the UI after launch.
    pub startup_errors: Vec<String>,
}

impl std::fmt::Debug for ProviderManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderManager")
            .field("providers", &self.configs.keys().collect::<Vec<_>>())
            .field("default_provider", &self.default_provider)
            .finish()
    }
}

impl ProviderManager {
    /// Build a new manager from the application config.
    ///
    /// For each provider whose `models` field is `None`, we call the
    /// `/models` endpoint to auto-discover available models at startup.
    /// Wrapped in a global timeout so startup never hangs on network.
    pub async fn new(config: &ProgrammerConfig) -> Self {
        let mut clients: HashMap<String, Client<OpenAIConfig>> = HashMap::new();
        let mut models: HashMap<String, Vec<String>> = HashMap::new();
        let mut startup_errors = Vec::new();

        // Build clients synchronously — always instant.
        for (name, provider_config) in &config.providers {
            let openai_config = OpenAIConfig::default()
                .with_api_base(&provider_config.base_url)
                .with_api_key(&provider_config.api_key);
            clients.insert(name.clone(), Client::with_config(openai_config));
            if let Some(manual) = &provider_config.models {
                models.insert(name.clone(), manual.clone());
            } else {
                models.insert(name.clone(), Vec::new());
            }
        }

        // Fetch models concurrently, but with a hard cap so startup is
        // never blocked indefinitely (some DNS / TCP stacks on Windows
        // can bypass tokio::time::timeout).
        const STARTUP_TIMEOUT: Duration = Duration::from_secs(8);
        let fetches = config.providers.iter().filter_map(|(name, pc)| {
            if pc.models.is_some() {
                return None; // already populated above
            }
            let client = clients.get(name).unwrap().clone();
            let name = name.clone();
            Some(tokio::spawn(async move {
                match tokio::time::timeout(MODEL_FETCH_TIMEOUT, client.models().list()).await {
                    Ok(Ok(resp)) => {
                        let list = resp.data.into_iter().map(|m| m.id).collect();
                        (name, Ok(list))
                    }
                    Ok(Err(e)) => {
                        let msg = format!(
                            "failed to fetch models for provider '{name}': {e} \
                             (provider still works, but /model completion won't list its models)"
                        );
                        (name, Err(msg))
                    }
                    Err(_) => {
                        let msg = format!(
                            "timed out fetching models for provider '{name}' after \
                             {}s — check your network \
                             (provider still works, but /model completion won't list its models)",
                            MODEL_FETCH_TIMEOUT.as_secs()
                        );
                        (name, Err(msg))
                    }
                }
            }))
        });

        match tokio::time::timeout(STARTUP_TIMEOUT, futures::future::join_all(fetches)).await {
            Ok(results) => {
                for result in results {
                    match result {
                        Ok((name, Ok(list))) => {
                            models.insert(name, list);
                        }
                        Ok((_, Err(msg))) => startup_errors.push(msg),
                        Err(_) => {} // task panicked; nothing to report
                    }
                }
            }
            Err(_) => {
                startup_errors.push(
                    "model discovery timed out — providers work, \
                     but /model completion may be incomplete"
                        .to_string(),
                );
            }
        }

        ProviderManager {
            clients,
            models,
            configs: config.providers.clone(),
            default_provider: config.default_provider.clone(),
            startup_errors,
        }
    }

    /// Resolve a `provider/model` string into a client reference and the bare
    /// model name to pass to the API.
    ///
    /// If no `/` is present, the `default_provider` is assumed.
    /// Returns `None` when the provider does not exist.
    pub fn resolve(&self, model: &str) -> Option<(&Client<OpenAIConfig>, String)> {
        let (provider, model_name) = if let Some((p, m)) = model.split_once('/') {
            (p, m.to_string())
        } else {
            (self.default_provider.as_str(), model.to_string())
        };
        self.clients.get(provider).map(|c| (c, model_name))
    }

    /// The model string to use on startup: `default_provider/<default_model>`.
    ///
    /// Resolution order for the model portion:
    /// 1. provider's `default_model` config field
    /// 2. first model from the provider's model list
    /// 3. `"default"` — model discovery failed (e.g. no network); most
    ///    providers accept it or the user can /model to something real
    pub fn default_model(&self) -> String {
        let config = self.configs.get(&self.default_provider);
        let model = config
            .and_then(|c| c.default_model.as_deref())
            .or_else(|| {
                self.models
                    .get(&self.default_provider)
                    .and_then(|m| m.first().map(|s| s.as_str()))
            })
            .unwrap_or("default");
        format!("{}/{}", self.default_provider, model)
    }

    /// The model string to use for Auto-mode tool-call classification when no
    /// global `classifier_model` is configured.
    ///
    /// Resolution: provider's `classifier_model` → `default_model` → first in
    /// list → `"default"`.
    pub fn default_classifier_model(&self) -> String {
        let config = self.configs.get(&self.default_provider);
        let model = config
            .and_then(|c| c.classifier_model.as_deref())
            .or_else(|| config.and_then(|c| c.default_model.as_deref()))
            .or_else(|| {
                self.models
                    .get(&self.default_provider)
                    .and_then(|m| m.first().map(|s| s.as_str()))
            })
            .unwrap_or("default");
        format!("{}/{}", self.default_provider, model)
    }

    pub fn provider_names(&self) -> Vec<&str> {
        self.configs.keys().map(|s| s.as_str()).collect()
    }

    pub fn models_for(&self, provider: &str) -> Vec<&str> {
        self.models
            .get(provider)
            .map(|v| v.iter().map(|s| s.as_str()).collect())
            .unwrap_or_default()
    }

    /// Create a stub instance for tests — no clients, no models.
    #[cfg(test)]
    pub fn stub(models: HashMap<String, Vec<String>>) -> Self {
        ProviderManager {
            clients: HashMap::new(),
            models,
            configs: HashMap::new(),
            default_provider: String::new(),
            startup_errors: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Startup must never hang on model discovery: an unreachable provider
    /// gets cut off by the timeout, reports an error, and falls back to
    /// `<provider>/default`.
    #[tokio::test]
    async fn unreachable_provider_times_out_and_falls_back() {
        let mut providers = HashMap::new();
        providers.insert(
            "offline".to_string(),
            ProviderConfig {
                // TEST-NET-1 black hole: connections neither succeed nor refuse.
                base_url: "http://192.0.2.1:9".to_string(),
                api_key: "unused".to_string(),
                models: None,
                default_model: None,
                classifier_model: None,
            },
        );
        let config = ProgrammerConfig {
            default_provider: "offline".to_string(),
            providers,
            classifier_model: None,
            allow_yolo: false,
            git_coauthor: None,
            mcp_servers: Vec::new(),
            model: None,
            base_url: None,
            api_key: None,
        };

        let start = std::time::Instant::now();
        let manager = ProviderManager::new(&config).await;
        assert!(
            start.elapsed() < MODEL_FETCH_TIMEOUT + Duration::from_secs(3),
            "startup took {:?}, model fetch is not being cut off",
            start.elapsed()
        );
        assert_eq!(manager.startup_errors.len(), 1);
        assert!(manager.models_for("offline").is_empty());
        assert_eq!(manager.default_model(), "offline/default");
    }
}
