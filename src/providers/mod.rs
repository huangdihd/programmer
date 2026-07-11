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

/// Manages multiple OpenAI-compatible providers, each with its own API key,
/// base URL, and model list (auto-discovered or manually configured).
pub struct ProviderManager {
    clients: HashMap<String, Client<OpenAIConfig>>,
    /// Resolved models per provider.
    models: HashMap<String, Vec<String>>,
    configs: HashMap<String, ProviderConfig>,
    default_provider: String,
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
    pub async fn new(config: &ProgrammerConfig) -> Self {
        let mut clients = HashMap::new();
        let mut models: HashMap<String, Vec<String>> = HashMap::new();

        for (name, provider_config) in &config.providers {
            let openai_config = OpenAIConfig::default()
                .with_api_base(&provider_config.base_url)
                .with_api_key(&provider_config.api_key);
            let client = Client::with_config(openai_config);

            let model_list = match &provider_config.models {
                Some(manual) => manual.clone(),
                None => Self::fetch_models(&client, name).await,
            };

            models.insert(name.clone(), model_list);
            clients.insert(name.clone(), client);
        }

        ProviderManager {
            clients,
            models,
            configs: config.providers.clone(),
            default_provider: config.default_provider.clone(),
        }
    }

    /// Try to fetch the model list from the provider's `/models` endpoint.
    /// On failure, returns an empty list (the provider still works, just
    /// without completion support).
    async fn fetch_models(client: &Client<OpenAIConfig>, name: &str) -> Vec<String> {
        match client.models().list().await {
            Ok(response) => response.data.into_iter().map(|m| m.id).collect(),
            Err(e) => {
                eprintln!(
                    "warning: failed to fetch models for provider '{name}': {e}\n\
                     (provider will work but /model completion won't list its models)"
                );
                Vec::new()
            }
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
    pub fn default_model(&self) -> String {
        let config = self.configs.get(&self.default_provider);
        let model = config
            .and_then(|c| c.default_model.as_deref())
            .or_else(|| {
                self.models
                    .get(&self.default_provider)
                    .and_then(|m| m.first().map(|s| s.as_str()))
            })
            .unwrap_or("");
        if model.is_empty() {
            String::new()
        } else {
            format!("{}/{}", self.default_provider, model)
        }
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
}
