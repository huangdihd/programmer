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

/// Current model-list discovery state for one configured provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProviderModelState {
    Refreshing,
    Ready { model_count: usize },
    Failed,
}

/// Sidebar-facing model-list status for one provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderModelStatus {
    pub(crate) name: String,
    pub(crate) state: ProviderModelState,
}

impl ProviderModelStatus {
    pub(crate) fn from_config(config: &ProgrammerConfig) -> Vec<Self> {
        let mut statuses = config
            .providers
            .iter()
            .map(|(name, provider)| Self {
                name: name.clone(),
                state: match &provider.models {
                    Some(models) => ProviderModelState::Ready {
                        model_count: models.len(),
                    },
                    None => ProviderModelState::Refreshing,
                },
            })
            .collect::<Vec<_>>();
        statuses.sort_by(|left, right| left.name.cmp(&right.name));
        statuses
    }
}

/// Counts only results owned by the refresh that was actually applied.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ModelRefreshSummary {
    pub model_count: usize,
    pub provider_count: usize,
    pub error_count: usize,
}

/// Manages multiple OpenAI-compatible providers, each with its own API key,
/// base URL, and model list (auto-discovered or manually configured).
#[derive(Clone)]
pub struct ProviderManager {
    clients: HashMap<String, Client<OpenAIConfig>>,
    /// Resolved models per provider.
    models: HashMap<String, Vec<String>>,
    configs: HashMap<String, ProviderConfig>,
    model_statuses: Vec<ProviderModelStatus>,
    next_refresh_generation: u64,
    refresh_owners: HashMap<String, u64>,
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
    /// Build a manager from local configuration without performing network I/O.
    pub fn from_config(config: &ProgrammerConfig) -> Self {
        let mut clients: HashMap<String, Client<OpenAIConfig>> = HashMap::new();
        let mut models: HashMap<String, Vec<String>> = HashMap::new();

        for (name, provider_config) in &config.providers {
            let openai_config = OpenAIConfig::default()
                .with_api_base(&provider_config.base_url)
                .with_api_key(&provider_config.api_key);
            clients.insert(name.clone(), Client::with_config(openai_config));
            models.insert(
                name.clone(),
                provider_config.models.clone().unwrap_or_default(),
            );
        }

        ProviderManager {
            clients,
            models,
            configs: config.providers.clone(),
            model_statuses: ProviderModelStatus::from_config(config),
            next_refresh_generation: 0,
            refresh_owners: HashMap::new(),
            default_provider: config.default_provider.clone(),
            startup_errors: Vec::new(),
        }
    }

    /// Build a new manager from the application config.
    ///
    /// For each provider whose `models` field is `None`, we call the
    /// `/models` endpoint to auto-discover available models at startup.
    /// Wrapped in a global timeout so startup never hangs on network.
    pub async fn new(config: &ProgrammerConfig) -> Self {
        let mut manager = Self::from_config(config);
        let (models, startup_errors) =
            Self::discover_models(&config.providers, &manager.clients).await;
        let requested_providers = config
            .providers
            .iter()
            .filter(|(_, provider)| provider.models.is_none())
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        let generation = manager.begin_model_refresh(&requested_providers);
        manager.finish_model_refresh(&requested_providers, generation, models, startup_errors);
        manager
    }

    /// Fetch auto-discovered model lists for every provider without a manual
    /// `models` list. Both models and errors are keyed by provider, including
    /// global timeout errors, so stale refresh results can be discarded together.
    ///
    /// This runs the same network fetches as [`Self::new`] but without
    /// rebuilding clients, so it can be executed in a background task and the
    /// result applied to an existing manager.
    pub async fn discover_models(
        providers: &HashMap<String, ProviderConfig>,
        clients: &HashMap<String, Client<OpenAIConfig>>,
    ) -> (HashMap<String, Vec<String>>, HashMap<String, String>) {
        let mut models: HashMap<String, Vec<String>> = HashMap::new();
        let mut startup_errors = HashMap::new();

        // Fetch models concurrently, but with a hard cap so startup is
        // never blocked indefinitely (some DNS / TCP stacks on Windows
        // can bypass tokio::time::timeout).
        const STARTUP_TIMEOUT: Duration = Duration::from_secs(8);
        let fetches =
            providers.iter().filter_map(|(name, pc)| {
                if pc.models.is_some() {
                    return None; // manual list already present
                }
                let Some(client) = clients.get(name).cloned() else {
                    startup_errors.insert(name.clone(), format!(
                    "cannot fetch models for provider '{name}': provider client is unavailable"
                ));
                    return None;
                };
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
                            let msg = model_fetch_timeout_message(&name);
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
                        Ok((name, Err(message))) => {
                            startup_errors.insert(name, message);
                        }
                        Err(_) => {} // task panicked; nothing to report
                    }
                }
            }
            Err(_) => {
                for (name, provider) in providers {
                    if provider.models.is_none() {
                        startup_errors.entry(name.clone()).or_insert_with(|| {
                            "model discovery timed out — providers work, \
                             but /model completion may be incomplete; \
                             use /providers refresh to retry"
                                .to_string()
                        });
                    }
                }
            }
        }

        (models, startup_errors)
    }

    pub(crate) fn model_statuses(&self) -> &[ProviderModelStatus] {
        &self.model_statuses
    }

    pub(crate) fn begin_model_refresh(&mut self, requested_providers: &[String]) -> u64 {
        self.next_refresh_generation = self
            .next_refresh_generation
            .checked_add(1)
            .expect("provider refresh generation exhausted");
        let generation = self.next_refresh_generation;
        for status in &mut self.model_statuses {
            if requested_providers.contains(&status.name) {
                self.refresh_owners.insert(status.name.clone(), generation);
                status.state = ProviderModelState::Refreshing;
            }
        }
        generation
    }

    /// Only the latest request for each provider may replace its catalog or state.
    /// Failed refreshes preserve the last successfully discovered catalog.
    /// Errors and notification counts include only accepted providers; an entirely
    /// stale completion leaves existing errors untouched and returns `None`.
    pub(crate) fn finish_model_refresh(
        &mut self,
        requested_providers: &[String],
        generation: u64,
        mut models: HashMap<String, Vec<String>>,
        mut startup_errors: HashMap<String, String>,
    ) -> Option<ModelRefreshSummary> {
        let mut summary = ModelRefreshSummary::default();
        let mut accepted = false;
        let mut accepted_errors = Vec::new();
        for status in &mut self.model_statuses {
            if !requested_providers.contains(&status.name)
                || self.refresh_owners.get(&status.name) != Some(&generation)
            {
                continue;
            }
            self.refresh_owners.remove(&status.name);
            accepted = true;
            if let Some(error) = startup_errors.remove(&status.name) {
                summary.error_count += 1;
                accepted_errors.push(error);
            }
            let Some(models) = models.remove(&status.name) else {
                status.state = ProviderModelState::Failed;
                continue;
            };
            summary.model_count += models.len();
            summary.provider_count += 1;
            status.state = ProviderModelState::Ready {
                model_count: models.len(),
            };
            self.models.insert(status.name.clone(), models);
        }
        if accepted {
            // A global timeout retains the startup UI's single-message output.
            accepted_errors.sort();
            accepted_errors.dedup();
            self.startup_errors = accepted_errors;
        }
        accepted.then_some(summary)
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

    pub fn provider_names(&self) -> Vec<&str> {
        self.configs.keys().map(|s| s.as_str()).collect()
    }

    pub fn models_for(&self, provider: &str) -> Vec<&str> {
        self.models
            .get(provider)
            .map(|v| v.iter().map(|s| s.as_str()).collect())
            .unwrap_or_default()
    }

    /// Access the provider clients so model discovery can be re-run in the
    /// background without rebuilding the manager.
    pub(crate) fn clients(&self) -> &HashMap<String, Client<OpenAIConfig>> {
        &self.clients
    }

    /// Create a stub instance for tests — no clients, no models.
    #[cfg(test)]
    pub fn stub(models: HashMap<String, Vec<String>>) -> Self {
        ProviderManager {
            clients: HashMap::new(),
            models,
            configs: HashMap::new(),
            model_statuses: Vec::new(),
            next_refresh_generation: 0,
            refresh_owners: HashMap::new(),
            default_provider: String::new(),
            startup_errors: Vec::new(),
        }
    }
}

fn model_fetch_timeout_message(name: &str) -> String {
    format!(
        "timed out fetching models for provider '{name}' after {}s — \
         use /providers refresh to retry \
         (provider still works, but /model completion won't list its models)",
        MODEL_FETCH_TIMEOUT.as_secs()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_fetch_timeout_points_to_refresh_command() {
        let message = model_fetch_timeout_message("llmhub");

        assert!(message.contains("provider 'llmhub'"));
        assert!(message.contains("/providers refresh"));
        assert!(!message.contains("check your network"));
    }

    #[tokio::test]
    async fn model_discovery_reports_a_missing_client() {
        let mut providers = HashMap::new();
        providers.insert(
            "missing".to_string(),
            ProviderConfig {
                base_url: "https://example.invalid".to_string(),
                api_key: String::new(),
                models: None,
                default_model: None,
            },
        );

        let (models, errors) = ProviderManager::discover_models(&providers, &HashMap::new()).await;

        assert!(models.is_empty());
        assert!(
            errors
                .values()
                .any(|error| error.contains("client is unavailable"))
        );
    }

    #[test]
    fn from_config_builds_clients_without_model_discovery() {
        let mut config = ProgrammerConfig::default();
        config.providers.get_mut("openai").unwrap().default_model =
            Some("configured-model".to_string());

        let start = std::time::Instant::now();
        let manager = ProviderManager::from_config(&config);

        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(manager.startup_errors.is_empty());
        assert!(manager.models_for("openai").is_empty());
        assert_eq!(manager.default_model(), "openai/configured-model");
        assert!(manager.resolve("openai/any-model").is_some());
    }

    #[test]
    fn model_refresh_status_and_catalog_share_one_owner() {
        let mut configuration = ProgrammerConfig::default();
        configuration.providers.clear();
        for (name, models) in [("automatic", None), ("manual", Some(vec!["fixed".into()]))] {
            configuration.providers.insert(
                name.into(),
                ProviderConfig {
                    base_url: "https://example.invalid".into(),
                    api_key: String::new(),
                    models,
                    default_model: None,
                },
            );
        }
        let mut manager = ProviderManager::from_config(&configuration);
        let requested = vec!["automatic".into()];
        assert_eq!(
            manager.model_statuses()[0].state,
            ProviderModelState::Refreshing
        );
        let generation = manager.begin_model_refresh(&requested);
        manager.finish_model_refresh(
            &requested,
            generation,
            HashMap::from([("automatic".into(), vec!["discovered".into()])]),
            HashMap::new(),
        );
        assert_eq!(manager.models_for("automatic"), vec!["discovered"]);
        assert_eq!(
            manager.model_statuses()[0].state,
            ProviderModelState::Ready { model_count: 1 }
        );
        let generation = manager.begin_model_refresh(&requested);
        assert_eq!(
            manager.model_statuses()[0].state,
            ProviderModelState::Refreshing
        );
        manager.finish_model_refresh(
            &requested,
            generation,
            HashMap::new(),
            HashMap::from([("automatic".into(), "unavailable".into())]),
        );
        assert_eq!(
            manager.model_statuses()[0].state,
            ProviderModelState::Failed
        );
        assert_eq!(manager.models_for("automatic"), vec!["discovered"]);
        assert_eq!(
            manager.model_statuses()[1].state,
            ProviderModelState::Ready { model_count: 1 }
        );
        assert_eq!(manager.models_for("manual"), vec!["fixed"]);
    }

    #[test]
    fn overlapping_refresh_older_success_cannot_overwrite_newer_catalog_or_state() {
        assert_overlapping_refresh_completion(false);
    }

    #[test]
    fn overlapping_refresh_older_failure_cannot_overwrite_newer_state_or_catalog() {
        assert_overlapping_refresh_completion(true);
    }

    fn assert_overlapping_refresh_completion(older_failed: bool) {
        let mut configuration = ProgrammerConfig::default();
        configuration.providers.retain(|name, _| name == "openai");
        configuration.providers.get_mut("openai").unwrap().models = None;
        let mut manager = ProviderManager::from_config(&configuration);
        let requested = vec!["openai".to_string()];

        // Begin A, then B; deliver B before A without network timing dependencies.
        let older_generation = manager.begin_model_refresh(&requested);
        let newer_generation = manager.begin_model_refresh(&requested);
        manager.finish_model_refresh(
            &requested,
            newer_generation,
            HashMap::from([("openai".into(), vec!["new-one".into(), "new-two".into()])]),
            HashMap::new(),
        );
        assert_eq!(manager.models_for("openai"), vec!["new-one", "new-two"]);
        assert_eq!(
            manager.model_statuses()[0].state,
            ProviderModelState::Ready { model_count: 2 }
        );
        assert!(manager.startup_errors.is_empty());

        let (older_models, older_errors) = if older_failed {
            (
                HashMap::new(),
                HashMap::from([("openai".into(), "older refresh failed".into())]),
            )
        } else {
            (
                HashMap::from([("openai".into(), vec!["old".into()])]),
                HashMap::new(),
            )
        };
        assert!(
            manager
                .finish_model_refresh(&requested, older_generation, older_models, older_errors,)
                .is_none()
        );
        assert_eq!(manager.models_for("openai"), vec!["new-one", "new-two"]);
        assert_eq!(
            manager.model_statuses()[0].state,
            ProviderModelState::Ready { model_count: 2 }
        );
        assert!(manager.startup_errors.is_empty());
    }

    #[test]
    fn overlapping_refreshes_keep_different_provider_owners_independent() {
        let mut configuration = ProgrammerConfig::default();
        let mut provider = configuration.providers["openai"].clone();
        provider.models = None;
        configuration.providers = HashMap::from([
            ("first".into(), provider.clone()),
            ("second".into(), provider),
        ]);
        let mut manager = ProviderManager::from_config(&configuration);
        let both = vec!["first".into(), "second".into()];
        let first = vec!["first".into()];
        let older_generation = manager.begin_model_refresh(&both);
        let newer_generation = manager.begin_model_refresh(&first);

        // The older batch can finish second, but must not finish first's new owner.
        assert!(
            manager
                .finish_model_refresh(
                    &both,
                    older_generation,
                    HashMap::from([
                        ("first".into(), vec!["stale".into()]),
                        ("second".into(), vec!["independent".into()]),
                    ]),
                    HashMap::new(),
                )
                .is_some()
        );
        assert!(manager.models_for("first").is_empty());
        assert_eq!(
            manager.model_statuses()[0].state,
            ProviderModelState::Refreshing
        );
        assert_eq!(manager.models_for("second"), vec!["independent"]);
        assert_eq!(
            manager.model_statuses()[1].state,
            ProviderModelState::Ready { model_count: 1 }
        );
        assert!(
            manager
                .finish_model_refresh(
                    &first,
                    newer_generation,
                    HashMap::from([("first".into(), vec!["current".into()])]),
                    HashMap::new(),
                )
                .is_some()
        );
        assert_eq!(manager.models_for("first"), vec!["current"]);
        assert_eq!(
            manager.model_statuses()[0].state,
            ProviderModelState::Ready { model_count: 1 }
        );
        assert_eq!(manager.models_for("second"), vec!["independent"]);
        assert!(
            manager
                .finish_model_refresh(&first, newer_generation, HashMap::new(), HashMap::new())
                .is_none()
        );
        assert_eq!(
            manager.model_statuses()[0].state,
            ProviderModelState::Ready { model_count: 1 }
        );
    }

    #[test]
    fn partially_stale_refresh_counts_and_errors_include_only_accepted_providers() {
        for stale_failed in [false, true] {
            let mut configuration = ProgrammerConfig::default();
            let mut provider = configuration.providers["openai"].clone();
            provider.models = None;
            configuration.providers = HashMap::from([
                ("first".into(), provider.clone()),
                ("second".into(), provider),
            ]);
            let mut manager = ProviderManager::from_config(&configuration);
            let both = vec!["first".into(), "second".into()];
            let first = vec!["first".into()];
            let older_generation = manager.begin_model_refresh(&both);
            let newer_generation = manager.begin_model_refresh(&first);
            manager.finish_model_refresh(
                &first,
                newer_generation,
                HashMap::from([("first".into(), vec!["current".into()])]),
                HashMap::new(),
            );
            let (models, errors, expected) = if stale_failed {
                (
                    HashMap::from([("second".into(), vec!["accepted".into()])]),
                    HashMap::from([("first".into(), "stale failure".into())]),
                    ModelRefreshSummary {
                        model_count: 1,
                        provider_count: 1,
                        error_count: 0,
                    },
                )
            } else {
                (
                    HashMap::from([("first".into(), vec!["stale-one".into(), "stale-two".into()])]),
                    HashMap::from([("second".into(), "accepted failure".into())]),
                    ModelRefreshSummary {
                        model_count: 0,
                        provider_count: 0,
                        error_count: 1,
                    },
                )
            };
            let summary = manager.finish_model_refresh(&both, older_generation, models, errors);
            assert_eq!(summary, Some(expected));
            assert_eq!(manager.models_for("first"), vec!["current"]);
            if stale_failed {
                assert!(manager.startup_errors.is_empty());
                assert_eq!(manager.models_for("second"), vec!["accepted"]);
            } else {
                assert_eq!(manager.startup_errors, vec!["accepted failure"]);
                assert_eq!(
                    manager.model_statuses()[1].state,
                    ProviderModelState::Failed
                );
            }
        }
    }

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
            },
        );
        let config = ProgrammerConfig {
            soul: None,
            theme: Default::default(),
            default_provider: "offline".to_string(),
            providers,
            classifier_model: None,
            classifier_top_logprobs: crate::consts::DEFAULT_CLASSIFIER_TOP_LOGPROBS,
            compact_model: None,
            memory_model: None,
            title_model: None,
            suggestion_model: None,
            auto_compact_tokens: 100_000,
            mandatory_compact_tokens: 150_000,
            compact_keep_recent_turns: 2,
            auto_compact_cooldown_turns: 5,
            memory: Default::default(),
            allow_yolo: false,
            security: Default::default(),
            security_profiles: Default::default(),
            active_security_profile: crate::config::programmer_config::DEFAULT_SECURITY_PROFILE
                .to_string(),
            git_coauthor: None,
            vision_enabled: true,
            auto_update_check: true,
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
