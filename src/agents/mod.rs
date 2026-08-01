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

//! In-process sub-agent lifecycle and runtime snapshots.
//!
//! A child gets its own [`Conversation`] and [`TurnRunner`], while sharing the
//! parent's provider clients, local security handle, MCP registry, and project
//! directory. Child registries deliberately omit the `agent` provider, making
//! the first implementation one level deep without prompt-only enforcement.

use crate::cancel::CancellationToken;
use crate::classifier::WorkMode;
use crate::conversation::Conversation;
use crate::runner::{
    AgentSurface, LlmPolicy, ReviewDecision, RunnerEvent, RunnerPolicy, TurnRunner,
};
use crate::ui::event::{AppEvent, Event, ReplyTx};
use async_openai::Client;
use async_openai::config::OpenAIConfig;
use async_openai::types::responses::{
    FunctionToolCall, InputContent, InputMessage, InputRole, MessageItem as ApiMessageItem,
    OutputStatus,
};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{Notify, mpsc};

pub(crate) const MAX_CONCURRENT_AGENTS: usize = 3;
const DEFAULT_MAX_STEPS: usize = 50;
static NEXT_FILE_SCOPE: AtomicU64 = AtomicU64::new(1);
static NEXT_MANAGER_GENERATION: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl AgentStatus {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub(crate) fn is_terminal(self) -> bool {
        self != Self::Running
    }
}

#[derive(Debug, Clone)]
pub(crate) struct AgentSnapshot {
    pub(crate) id: u64,
    pub(crate) name: String,
    pub(crate) prompt: String,
    pub(crate) status: AgentStatus,
    pub(crate) elapsed: Duration,
    pub(crate) result: Option<String>,
}

struct AgentEntry {
    id: u64,
    name: String,
    prompt: String,
    status: AgentStatus,
    started: Instant,
    finished: Option<Instant>,
    result: Option<String>,
    conversation: Arc<Mutex<Conversation>>,
    cancel: CancellationToken,
    changed: Arc<Notify>,
    notify_parent: Arc<AtomicBool>,
}

impl AgentEntry {
    fn snapshot(&self) -> AgentSnapshot {
        AgentSnapshot {
            id: self.id,
            name: self.name.clone(),
            prompt: self.prompt.clone(),
            status: self.status,
            elapsed: self.finished.unwrap_or_else(Instant::now) - self.started,
            result: self.result.clone(),
        }
    }
}

#[derive(Default)]
struct AgentState {
    next_id: u64,
    entries: BTreeMap<u64, AgentEntry>,
}

#[derive(Clone)]
pub(crate) struct AgentManager {
    state: Arc<Mutex<AgentState>>,
    generation: u64,
}

impl Default for AgentManager {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(AgentState::default())),
            generation: NEXT_MANAGER_GENERATION.fetch_add(1, Ordering::Relaxed),
        }
    }
}

struct AgentStart {
    id: u64,
    file_scope: u64,
    conversation: Arc<Mutex<Conversation>>,
    cancel: CancellationToken,
}

impl AgentManager {
    pub(crate) fn spawn(
        &self,
        prompt: String,
        name: Option<String>,
        runtime: AgentRuntime,
        events: mpsc::UnboundedSender<Event>,
    ) -> Result<u64, String> {
        let start = self.reserve(prompt.clone(), name)?;
        let id = start.id;
        let manager = self.clone();
        tokio::spawn(async move {
            seed_conversation(&start.conversation, prompt);
            let surface = SubagentSurface {
                id,
                generation: manager.generation,
                tx: events.clone(),
                skill_prompt: runtime.skill_prompt.clone(),
                approval_label: runtime.approval_label.clone(),
                cancel: start.cancel.clone(),
            };
            let result = runtime
                .build_runner(start.file_scope)
                .run_turn(&start.conversation, &start.cancel, &surface)
                .await;
            runtime
                .security
                .snapshot()
                .clear_file_scope(start.file_scope);
            manager.finish(id, result);
            let _ = events.send(Event::App(AppEvent::AgentStateChanged {
                generation: manager.generation,
                id,
            }));
        });
        Ok(id)
    }

    fn reserve(&self, prompt: String, name: Option<String>) -> Result<AgentStart, String> {
        let mut state = self.state.lock().unwrap();
        let running = state
            .entries
            .values()
            .filter(|entry| entry.status == AgentStatus::Running)
            .count();
        if running >= MAX_CONCURRENT_AGENTS {
            return Err(format!(
                "error: at most {MAX_CONCURRENT_AGENTS} sub-agents may run concurrently"
            ));
        }

        state.next_id = state.next_id.wrapping_add(1).max(1);
        let id = state.next_id;
        let conversation = Arc::new(Mutex::new(Conversation::new()));
        let cancel = CancellationToken::new();
        state.entries.insert(
            id,
            AgentEntry {
                id,
                name: name.unwrap_or_else(|| default_name(&prompt)),
                prompt,
                status: AgentStatus::Running,
                started: Instant::now(),
                finished: None,
                result: None,
                conversation: conversation.clone(),
                cancel: cancel.clone(),
                changed: Arc::new(Notify::new()),
                notify_parent: Arc::new(AtomicBool::new(true)),
            },
        );
        Ok(AgentStart {
            id,
            file_scope: NEXT_FILE_SCOPE.fetch_add(1, Ordering::Relaxed),
            conversation,
            cancel,
        })
    }

    fn finish(
        &self,
        id: u64,
        result: Result<crate::runner::TurnResult, crate::runner::RunnerError>,
    ) {
        let mut state = self.state.lock().unwrap();
        let Some(entry) = state.entries.get_mut(&id) else {
            return;
        };
        entry.finished = Some(Instant::now());
        match result {
            Ok(result) => {
                entry.status = AgentStatus::Completed;
                entry.result = Some(result.final_text);
            }
            Err(crate::runner::RunnerError::Cancelled) => {
                entry.status = AgentStatus::Cancelled;
                entry.result = Some("cancelled".to_string());
            }
            Err(error) => {
                entry.status = AgentStatus::Failed;
                entry.result = Some(error.to_string());
            }
        }
        entry.changed.notify_waiters();
    }

    pub(crate) fn snapshot(&self, id: u64) -> Option<AgentSnapshot> {
        self.state
            .lock()
            .unwrap()
            .entries
            .get(&id)
            .map(AgentEntry::snapshot)
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn snapshot_all(&self) -> Vec<AgentSnapshot> {
        self.state
            .lock()
            .unwrap()
            .entries
            .values()
            .map(AgentEntry::snapshot)
            .collect()
    }

    pub(crate) fn conversation(&self, id: u64) -> Option<Arc<Mutex<Conversation>>> {
        self.state
            .lock()
            .unwrap()
            .entries
            .get(&id)
            .map(|entry| entry.conversation.clone())
    }

    pub(crate) async fn wait(&self, id: u64, timeout: Duration) -> Result<AgentSnapshot, String> {
        let changed = {
            let state = self.state.lock().unwrap();
            let entry = state
                .entries
                .get(&id)
                .ok_or_else(|| format!("error: no sub-agent with id {id}"))?;
            entry.changed.clone()
        };

        let notified = changed.notified();
        tokio::pin!(notified);
        let snapshot = self
            .snapshot(id)
            .ok_or_else(|| format!("error: no sub-agent with id {id}"))?;
        if snapshot.status == AgentStatus::Running {
            let _ = tokio::time::timeout(timeout, &mut notified).await;
        }
        let snapshot = self
            .snapshot(id)
            .ok_or_else(|| format!("error: no sub-agent with id {id}"))?;
        if snapshot.status.is_terminal() {
            self.consume_notification(id);
        }
        Ok(snapshot)
    }

    pub(crate) fn cancel(&self, id: u64) -> Result<(), String> {
        let state = self.state.lock().unwrap();
        let entry = state
            .entries
            .get(&id)
            .ok_or_else(|| format!("error: no sub-agent with id {id}"))?;
        if entry.status.is_terminal() {
            return Err(format!(
                "error: sub-agent {id} is already {}",
                entry.status.label()
            ));
        }
        entry.notify_parent.store(false, Ordering::Release);
        entry.cancel.cancel();
        Ok(())
    }

    pub(crate) fn cancel_all(&self) {
        let state = self.state.lock().unwrap();
        for entry in state.entries.values() {
            if entry.status == AgentStatus::Running {
                entry.cancel.cancel();
            }
        }
    }

    pub(crate) fn should_notify_parent(&self, id: u64) -> bool {
        self.state
            .lock()
            .unwrap()
            .entries
            .get(&id)
            .is_some_and(|entry| entry.notify_parent.load(Ordering::Acquire))
    }

    pub(crate) fn consume_notification(&self, id: u64) {
        if let Some(entry) = self.state.lock().unwrap().entries.get(&id) {
            entry.notify_parent.store(false, Ordering::Release);
        }
    }
}

#[derive(Clone)]
pub(crate) enum AgentPolicyFactory {
    Yolo,
    Sync(WorkMode),
    Llm(Box<LlmPolicy>),
}

impl AgentPolicyFactory {
    fn build(&self) -> RunnerPolicy {
        match self {
            Self::Yolo => RunnerPolicy::Yolo,
            Self::Sync(mode) => RunnerPolicy::Sync(mode.classifier()),
            Self::Llm(policy) => RunnerPolicy::Llm(policy.clone()),
        }
    }
}

#[derive(Clone)]
pub(crate) struct AgentRuntime {
    pub(crate) provider_manager: Arc<crate::providers::ProviderManager>,
    pub(crate) client: Client<OpenAIConfig>,
    pub(crate) model_name: String,
    pub(crate) model_str: String,
    pub(crate) todos: Arc<Mutex<crate::todos::TodoList>>,
    pub(crate) security: Arc<crate::security::SecurityHandle>,
    pub(crate) mcp_manager: Option<Arc<crate::mcp::McpManager>>,
    pub(crate) policy: AgentPolicyFactory,
    pub(crate) coauthor: Option<String>,
    pub(crate) vision_enabled: bool,
    pub(crate) thinking_level: crate::thinking::ThinkingLevel,
    pub(crate) skill_prompt: Option<String>,
    pub(crate) approval_label: String,
}

impl AgentRuntime {
    pub(crate) fn with_overrides(
        &self,
        model: Option<&str>,
        thinking: Option<&str>,
    ) -> Result<Self, String> {
        let mut runtime = self.clone();
        if let Some(model) = model {
            let (client, model_name) = self
                .provider_manager
                .resolve(model)
                .ok_or_else(|| format!("error: unknown provider/model: {model}"))?;
            runtime.client = client.clone();
            runtime.model_name = model_name;
            runtime.model_str = model.to_string();
        }
        if let Some(thinking) = thinking {
            runtime.thinking_level =
                crate::thinking::ThinkingLevel::parse(thinking).ok_or_else(|| {
                    format!(
                        "error: invalid thinking level '{thinking}' — expected {}",
                        crate::thinking::ThinkingLevel::VALUES
                    )
                })?;
        }
        Ok(runtime)
    }

    fn build_runner(&self, file_scope: u64) -> TurnRunner {
        use crate::tools::provider::{
            LocalToolProvider, McpToolProvider, ToolProvider, ToolRegistry,
        };

        let mut providers: Vec<Arc<dyn ToolProvider>> = vec![Arc::new(
            LocalToolProvider::new_scoped(self.todos.clone(), self.security.clone(), file_scope),
        )];
        if let Some(mcp) = &self.mcp_manager {
            providers.push(Arc::new(McpToolProvider::new(mcp.clone())));
        }
        TurnRunner {
            client: self.client.clone(),
            model_name: self.model_name.clone(),
            model_str: self.model_str.clone(),
            tools: Arc::new(ToolRegistry::new(providers)),
            policy: self.policy.build(),
            coauthor: self.coauthor.clone(),
            vision_enabled: self.vision_enabled,
            thinking_level: self.thinking_level,
            hooks: crate::runner::hooks::standard_hooks(Arc::new(Mutex::new(
                crate::runner::DiagnosticsState::default(),
            ))),
            stream_retrying: Arc::new(AtomicBool::new(false)),
            max_steps: Some(DEFAULT_MAX_STEPS),
        }
    }
}

struct SubagentSurface {
    id: u64,
    generation: u64,
    tx: mpsc::UnboundedSender<Event>,
    skill_prompt: Option<String>,
    approval_label: String,
    cancel: CancellationToken,
}

#[async_trait::async_trait]
impl AgentSurface for SubagentSurface {
    fn on_event(&self, _event: RunnerEvent<'_>) {}

    async fn review(
        &self,
        call: &FunctionToolCall,
        reason: &str,
        position: (usize, usize),
    ) -> ReviewDecision {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let _ = self.tx.send(Event::App(AppEvent::ReviewRequest {
            call: call.clone(),
            reason: format!("Sub-agent #{}: {reason}", self.id),
            position,
            reply: ReplyTx(reply_tx),
            operation_id: 0,
            agent_id: Some(self.id),
            agent_generation: Some(self.generation),
        }));
        match self.cancel.wait_or(reply_rx).await {
            Some(Ok(decision)) => decision,
            _ => ReviewDecision::Deny {
                output: crate::runner::classify::classifier_denied_output(call, "cancelled"),
            },
        }
    }

    fn skill_prompt(&self) -> Option<String> {
        self.skill_prompt.clone()
    }

    fn approval_label(&self) -> String {
        self.approval_label.clone()
    }
}

fn seed_conversation(conversation: &Mutex<Conversation>, prompt: String) {
    let mut conversation = conversation.lock().unwrap();
    conversation.add_input_message(input_message(
        "You are an in-process sub-agent. Complete only the delegated task and return a concise, evidence-backed result to the parent agent. You cannot create further sub-agents. Do not ask the user questions; report blockers to the parent.".to_string(),
        InputRole::Developer,
    ));
    conversation.add_input_message(input_message(prompt, InputRole::User));
}

fn input_message(text: String, role: InputRole) -> ApiMessageItem {
    ApiMessageItem::Input(InputMessage {
        content: vec![InputContent::InputText(text.into())],
        role,
        status: Some(OutputStatus::Completed),
    })
}

fn default_name(prompt: &str) -> String {
    let first_line = prompt.lines().next().unwrap_or("sub-agent").trim();
    let mut name: String = first_line.chars().take(48).collect();
    if first_line.chars().count() > 48 {
        name.push('…');
    }
    if name.is_empty() {
        "sub-agent".to_string()
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_enforces_concurrency_and_tracks_terminal_result() {
        let manager = AgentManager::default();
        let first = manager.reserve("one".into(), None).unwrap();
        manager.reserve("two".into(), None).unwrap();
        manager.reserve("three".into(), None).unwrap();
        assert!(manager.reserve("four".into(), None).is_err());

        manager.finish(
            first.id,
            Ok(crate::runner::TurnResult {
                final_text: "done".into(),
                usage: (1, 2),
            }),
        );
        let snapshot = manager.snapshot(first.id).unwrap();
        assert_eq!(snapshot.status, AgentStatus::Completed);
        assert_eq!(snapshot.result.as_deref(), Some("done"));
        assert!(manager.reserve("four".into(), None).is_ok());
    }

    #[tokio::test]
    async fn wait_consumes_parent_notification() {
        let manager = AgentManager::default();
        let start = manager.reserve("inspect".into(), None).unwrap();
        manager.finish(start.id, Err(crate::runner::RunnerError::Cancelled));

        let snapshot = manager.wait(start.id, Duration::ZERO).await.unwrap();
        assert_eq!(snapshot.status, AgentStatus::Cancelled);
        assert!(!manager.should_notify_parent(start.id));
    }

    #[tokio::test]
    async fn timed_out_wait_preserves_completion_notification() {
        let manager = AgentManager::default();
        let start = manager.reserve("inspect".into(), None).unwrap();

        let snapshot = manager.wait(start.id, Duration::ZERO).await.unwrap();

        assert_eq!(snapshot.status, AgentStatus::Running);
        assert!(manager.should_notify_parent(start.id));
    }
}
