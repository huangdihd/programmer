//! Shared turn assembly. Entry points own connection lifetimes and declare
//! capabilities; this module installs the same common providers for every turn.
use super::{LlmPolicy, RunnerPolicy, TurnRunner};
use crate::classifier::WorkMode;
use crate::providers::ProviderManager;
use crate::tools::memory::MemoryModel;
use crate::tools::provider::{
    LocalToolProvider, McpToolProvider, SkillToolProvider, ToolProvider, ToolRegistry,
};
use async_openai::{Client, config::OpenAIConfig};
use std::collections::HashSet;
use std::sync::{Arc, Mutex, atomic::AtomicBool};

#[derive(Clone)]
pub(crate) enum PolicySpec {
    Yolo,
    Sync(WorkMode),
    Llm(Box<LlmPolicy>),
}

impl PolicySpec {
    pub(crate) fn resolve(
        mode: WorkMode,
        providers: &ProviderManager,
        classifier_model: &str,
        top_logprobs: u8,
        no_logprobs: Arc<Mutex<HashSet<String>>>,
    ) -> Option<Self> {
        match mode {
            WorkMode::Yolo => Some(Self::Yolo),
            WorkMode::Manual | WorkMode::Plan => Some(Self::Sync(mode)),
            WorkMode::Auto => {
                let (client, model_name) = providers.resolve(classifier_model)?;
                Some(Self::Llm(Box::new(LlmPolicy {
                    client: client.clone(),
                    model_name,
                    top_logprobs,
                    no_logprobs,
                })))
            }
        }
    }

    pub(crate) fn build(&self) -> RunnerPolicy {
        match self {
            Self::Yolo => RunnerPolicy::Yolo,
            Self::Sync(mode) => RunnerPolicy::Sync(mode.classifier()),
            Self::Llm(policy) => RunnerPolicy::Llm(policy.clone()),
        }
    }
}

pub(crate) fn resolve_memory_model(
    providers: &ProviderManager,
    enabled: bool,
    target: Option<&str>,
    current_model: &str,
) -> Option<MemoryModel> {
    if !enabled {
        return None;
    }
    providers
        .resolve(target.unwrap_or(current_model))
        .map(|(client, model)| MemoryModel {
            client: client.clone(),
            model,
        })
}

/// Local access scope, checkpoints and history remain explicit at the caller.
/// Extra providers grant capabilities (delegation/peers); children pass none.
pub(crate) struct AgentSpec {
    pub client: Client<OpenAIConfig>,
    pub model_name: String,
    pub model_str: String,
    pub local: LocalToolProvider,
    pub skills: crate::skills::SkillRegistry,
    pub mcp: Option<Arc<crate::mcp::McpManager>>,
    pub extra_providers: Vec<Arc<dyn ToolProvider>>,
    pub policy: PolicySpec,
    pub soul: Option<String>,
    pub coauthor: Option<String>,
    pub vision_enabled: bool,
    pub thinking_level: crate::thinking::ThinkingLevel,
    pub memory_model: Option<MemoryModel>,
    pub hooks: Vec<Arc<dyn super::hooks::TurnHook>>,
    pub stream_retrying: Arc<AtomicBool>,
    pub max_steps: Option<usize>,
}

impl AgentSpec {
    pub(crate) fn build(self) -> TurnRunner {
        let mut providers: Vec<Arc<dyn ToolProvider>> = vec![
            Arc::new(self.local.with_memory_model(self.memory_model.clone())),
            Arc::new(SkillToolProvider::new(self.skills)),
        ];
        if let Some(manager) = self.mcp {
            providers.push(Arc::new(McpToolProvider::new(manager)));
        }
        providers.extend(self.extra_providers);
        TurnRunner {
            client: self.client,
            model_name: self.model_name,
            model_str: self.model_str,
            tools: Arc::new(ToolRegistry::new(providers)),
            policy: self.policy.build(),
            soul: self.soul,
            coauthor: self.coauthor,
            vision_enabled: self.vision_enabled,
            thinking_level: self.thinking_level,
            memory_model: self.memory_model,
            hooks: self.hooks,
            stream_retrying: self.stream_retrying,
            stream_retry_limit: crate::consts::MAX_STREAM_RETRIES,
            max_steps: self.max_steps,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(memory_enabled: bool) -> AgentSpec {
        let configuration = crate::config::programmer_config::ProgrammerConfig::default();
        let providers = ProviderManager::from_config(&configuration);
        let security = crate::security::SecurityManager::standalone().unwrap();
        AgentSpec {
            client: Client::with_config(OpenAIConfig::default()),
            model_name: "test".into(),
            model_str: "openai/test".into(),
            local: LocalToolProvider::new(
                Default::default(),
                Arc::new(crate::security::SecurityHandle::new(Arc::new(security))),
            )
            .with_memory_enabled(memory_enabled),
            skills: Default::default(),
            mcp: None,
            extra_providers: Vec::new(),
            policy: PolicySpec::Yolo,
            soul: None,
            coauthor: None,
            vision_enabled: false,
            thinking_level: crate::thinking::ThinkingLevel::Auto,
            memory_model: resolve_memory_model(&providers, memory_enabled, None, "openai/test"),
            hooks: Vec::new(),
            stream_retrying: Arc::new(AtomicBool::new(false)),
            max_steps: Some(7),
        }
    }

    #[test]
    fn common_tools_preserve_memory_gate_and_do_not_grant_delegation() {
        for enabled in [false, true] {
            let runner = spec(enabled).build();
            let names: Vec<_> = runner
                .tools
                .tools()
                .into_iter()
                .filter_map(|tool| match tool {
                    async_openai::types::responses::Tool::Function(function) => Some(function.name),
                    _ => None,
                })
                .collect();
            assert!(names.iter().any(|name| name == "read_file"));
            assert_eq!(names.iter().any(|name| name == "memory"), enabled);
            assert!(
                !names
                    .iter()
                    .any(|name| name == "agent" || name == "peer_session")
            );
            assert_eq!(runner.memory_model.is_some(), enabled);
            assert_eq!(runner.max_steps, Some(7));
            assert!(runner.hooks.is_empty());
        }
    }

    #[tokio::test]
    async fn assembly_includes_configured_mcp_tools() {
        let (url, server) = crate::mcp::tests::spawn_mock_http_server().await;
        let configuration = crate::mcp::types::McpServerConfig {
            name: "assembly".into(),
            command: String::new(),
            args: Vec::new(),
            env: Default::default(),
            url: Some(url),
        };
        let manager =
            crate::mcp::McpManager::from_config_with_updates(&[configuration], ".", |_, _| {})
                .await;
        server.abort();
        assert!(manager.startup_errors.is_empty());
        let mut specification = spec(false);
        specification.mcp = Some(Arc::new(manager));
        let runner = specification.build();
        assert!(runner.tools.tools().into_iter().any(|tool| matches!(
            tool, async_openai::types::responses::Tool::Function(function)
                if function.name == "mcp__assembly__echo"
        )));
    }

    #[test]
    fn only_auto_requires_a_resolvable_classifier() {
        let providers = ProviderManager::from_config(&Default::default());
        for mode in [
            WorkMode::Manual,
            WorkMode::Plan,
            WorkMode::Yolo,
            WorkMode::Auto,
        ] {
            let policy =
                PolicySpec::resolve(mode, &providers, "missing/model", 5, Default::default());
            assert_eq!(policy.is_some(), mode != WorkMode::Auto);
        }
    }

    #[test]
    fn memory_target_overrides_current_model_and_disabled_memory_never_resolves() {
        let providers = ProviderManager::from_config(&Default::default());
        assert_eq!(
            resolve_memory_model(&providers, true, Some("openai/memory"), "openai/chat")
                .unwrap()
                .model,
            "memory"
        );
        assert_eq!(
            resolve_memory_model(&providers, true, None, "openai/child")
                .unwrap()
                .model,
            "child"
        );
        assert!(
            resolve_memory_model(&providers, false, Some("openai/memory"), "openai/chat").is_none()
        );
        assert!(
            resolve_memory_model(&providers, true, Some("missing/model"), "openai/chat").is_none()
        );
    }
}
