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

//! Application core: struct definition, lifecycle, and method dispatch to
//! focused submodules.

mod command_handlers;
pub(crate) mod commands;
pub(crate) mod diagnostics;
pub(crate) mod events;
pub(crate) mod helpers;
pub(crate) mod session;
pub(crate) mod surface;

use crate::cancel::CancellationToken;
use crate::classifier::WorkMode;
use crate::config::programmer_config::ProgrammerConfig;
use crate::mcp::McpServerStatus;
use crate::providers::{ProviderManager, ProviderModelStatus};
use crate::response::message_item::MessageItem;
use crate::session::{AutoCompactOverride, ModelOverride, SessionManager};
use crate::ui::components::conversation_panel::conversation_panel::ConversationPanel;
use crate::ui::components::diagnostics_panel::DiagnosticsPanel;
use crate::ui::components::footer::footer::Footer;
use crate::ui::components::input_panel::input_panel::InputPanel;
use crate::ui::components::mcp_panel::McpPanel;
use crate::ui::components::provider_panel::ProviderPanel;
use crate::ui::components::question_panel::QuestionPanel;
use crate::ui::components::rewind_panel::RewindPanel;
use crate::ui::components::security_panel::SecurityPanel;
use crate::ui::components::sidebar::Sidebar;
use crate::ui::components::skills_panel::SkillsPanel;
use crate::ui::components::todo_panel::TodoPanel;
use crate::ui::event::{Event, EventHandler};
use async_openai::types::responses::FunctionToolCall;
use crossterm::event::KeyEvent;
use ratatui::DefaultTerminal;
use ratatui::layout::Rect;
use std::collections::{HashSet, VecDeque};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Keep input bursts from starving terminal redraws. Four events per frame
/// sustains 120 events/second at the normal 30 FPS tick rate while giving
/// trackpad scrolling and streamed output a frame boundary every few events.
const MAX_EVENTS_PER_FRAME: usize = 4;

/// A pending tool-call review request from the runner. Manual mode now gets
/// per-call reviews (no batch), driven by the runner's `review()` callback.
pub(crate) struct PendingReview {
    pub(crate) call: FunctionToolCall,
    pub(crate) reason: String,
    /// 1-based position and batch total (e.g. (2, 5)).
    pub(crate) position: (usize, usize),
    /// The oneshot back to the runner.
    pub(crate) reply: crate::ui::event::ReplyTx,
    /// Which approval option is highlighted (0=Approve, 1=Deny).
    pub(crate) selected: usize,
    /// Main-turn operation id, or zero for an independently running sub-agent.
    pub(crate) operation_id: u64,
    /// Child id for a sub-agent review; absent for the main turn.
    pub(crate) agent_id: Option<u64>,
    pub(crate) agent_generation: Option<u64>,
}

/// UI-only diagnostics bookkeeping. The mutable state (baseline + edit-turn
/// counter) lives in [`App::diagnostics_state`], shared with the runner, so the
/// runner's post-edit feedback loop sees the same baseline the sidebar renders.
pub(crate) struct DiagnosticsState {
    /// Whether the project's diagnostics profile declares an LSP checker.
    pub(crate) lsp_configured: bool,
}

/// Cancellation-related tokens for the current request lifecycle.
pub(crate) struct CancelState {
    /// The current turn's root cancel token. Every phase (stream,
    /// classification, tool execution, diagnostics) runs against a child
    /// derived from it, so cancelling this one token stops whichever phase is
    /// in flight — including the post-stream pipeline whose own stream token is
    /// already gone by the time it runs.
    pub(crate) active: CancellationToken,
    /// Monotonically increasing counter — every turn (including retries and
    /// `/init`) bumps this by 1 so operation ids are never reused within a
    /// single process lifetime. It wraps naturally on overflow, but two turns
    /// always get distinct ids in practice.
    pub(crate) next_id: u64,
    /// The current turn's operation id, or `None` when idle. Set synchronously
    /// before the turn spawns and cleared when [`AppEvent::TurnFinished`]
    /// arrives, so the UI never races between "start" and "what is my id?" and
    /// stale events from an earlier turn are always dropped.
    pub(crate) active_id: Option<u64>,
    /// Conversation length immediately before the active turn was appended.
    /// Automatic compaction may only summarize items before this boundary.
    pub(crate) turn_conversation_cutoff: Option<usize>,
    /// True while the stream task is backing off between connection retries.
    pub(crate) stream_retrying: Arc<AtomicBool>,
    /// Becomes true after the current request produces any model output. It
    /// stays true after the live response has been committed.
    pub(crate) response_started: bool,
    /// Original editable draft for a user request, retained until completion
    /// so an early cancellation can put it back in the input.
    pub(crate) active_user_request: Option<ActiveUserRequest>,
}

pub(crate) struct ActiveUserRequest {
    pub(crate) draft: crate::ui::components::input_panel::input_panel::InputDraft,
    pub(crate) conversation_cutoff: usize,
    pub(crate) history_text: String,
}

/// Session identity, persistence handle, and the deferred-save dirty flag.
pub(crate) struct SessionState {
    /// Session UUID.
    pub(crate) uuid: String,
    /// Whether the session was actually saved at least once during this run
    /// (i.e. there was user input worth persisting).
    pub(crate) did_save: bool,
    /// Session manager for persistence.
    pub(crate) mgr: Option<SessionManager>,
    /// Set when session state changed and needs persisting. The actual disk
    /// write is deferred to the next idle tick (see [`session::flush_if_dirty`])
    /// so a burst of changes within a turn collapses into a single save at turn
    /// end instead of writing after every event.
    pub(crate) dirty: bool,
    pub(crate) classifier_model_override: ModelOverride,
    pub(crate) classifier_top_logprobs_override: Option<u8>,
    pub(crate) compact_model_override: ModelOverride,
    pub(crate) auto_compact_override: AutoCompactOverride,
    pub(crate) compact_keep_recent_turns_override: Option<usize>,
}

#[derive(Debug, Default)]
pub(crate) struct AutoCompactState {
    pub(crate) next_id: u64,
    pub(crate) active_id: Option<u64>,
    pub(crate) history_epoch: u64,
    pub(crate) last_cutoff: Option<usize>,
    pub(crate) last_input_tokens: Option<u32>,
}

pub(crate) struct TaskNotificationState {
    pub(crate) pending: VecDeque<crate::tasks::TaskLifecycleEvent>,
    seen: HashSet<(u64, u64)>,
    pub(crate) ready_at: Option<Instant>,
    pub(crate) flush_token: u64,
    pub(crate) flush_requested: bool,
}

pub(crate) struct AgentNotificationState {
    pub(crate) pending: VecDeque<u64>,
    seen: HashSet<u64>,
    pub(crate) ready_at: Option<Instant>,
    pub(crate) flush_token: u64,
    pub(crate) flush_requested: bool,
}

impl AgentNotificationState {
    fn new() -> Self {
        Self {
            pending: VecDeque::new(),
            seen: HashSet::new(),
            ready_at: None,
            flush_token: 0,
            flush_requested: false,
        }
    }

    pub(crate) fn push(&mut self, id: u64) {
        if self.seen.insert(id) {
            self.pending.push_back(id);
            self.ready_at = Some(Instant::now() + std::time::Duration::from_millis(200));
            self.flush_token = self.flush_token.wrapping_add(1);
            self.flush_requested = false;
        }
    }

    pub(crate) fn discard_consumed(&mut self, manager: &crate::agents::AgentManager) {
        self.pending.retain(|id| manager.should_notify_parent(*id));
        if self.pending.is_empty() {
            self.ready_at = None;
            self.flush_requested = false;
        }
    }
}

impl TaskNotificationState {
    fn new() -> Self {
        Self {
            pending: VecDeque::new(),
            seen: HashSet::new(),
            ready_at: None,
            flush_token: 0,
            flush_requested: false,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.pending.clear();
        self.seen.clear();
        self.ready_at = None;
        self.flush_token = self.flush_token.wrapping_add(1);
        self.flush_requested = false;
    }

    pub(crate) fn push(&mut self, event: crate::tasks::TaskLifecycleEvent) -> bool {
        if !self.seen.insert((event.generation, event.sequence)) {
            return false;
        }
        self.pending.push_back(event);
        self.ready_at = Some(Instant::now() + std::time::Duration::from_millis(200));
        self.flush_token = self.flush_token.wrapping_add(1);
        self.flush_requested = false;
        true
    }

    pub(crate) fn discard_consumed(&mut self) {
        self.pending.retain(|event| event.should_notify_agent());
        if self.pending.is_empty() {
            self.ready_at = None;
            self.flush_requested = false;
        }
    }
}

/// Application.
pub struct App<'a> {
    /// Is the application running?
    pub running: bool,
    /// Time of the first Ctrl+C press while waiting for exit confirmation.
    pub(crate) quit_requested_at: Option<std::time::Instant>,
    /// Multi-provider manager (replaces the single OpenAI client).
    pub provider_manager: ProviderManager,
    /// Per-provider model-list state rendered in the right sidebar.
    pub(crate) provider_model_statuses: Vec<ProviderModelStatus>,
    /// Currently active model in `provider/model` format.
    pub current_model: String,
    /// Whether supported `@image` references are sent as multimodal inputs.
    pub vision_enabled: bool,
    /// Whether the terminal emulator owns mouse drags for native text
    /// selection instead of the TUI receiving mouse events.
    pub(crate) native_selection_mode: bool,
    /// Reasoning effort for main chat and compaction requests.
    pub(crate) thinking_level: crate::thinking::ThinkingLevel,
    /// Images belonging to the queued follow-up message while a turn is busy.
    pub(crate) pending_images: Vec<async_openai::types::responses::InputImageContent>,
    /// Event handler.
    pub events: EventHandler,
    /// Application configuration.
    pub config: ProgrammerConfig,
    pub input_panel: InputPanel<'a>,
    pub conversation_panel: ConversationPanel,
    pub footer: Footer,
    /// Full-screen provider management panel, when open.
    pub provider_panel: Option<ProviderPanel>,
    /// Full-screen skills management panel, when open.
    pub skills_panel: Option<SkillsPanel>,
    /// Full-screen MCP server management panel, when open.
    pub mcp_panel: Option<McpPanel>,
    /// Full-screen project diagnostics management panel, when open.
    pub diagnostics_panel: Option<DiagnosticsPanel>,
    /// Full-screen security profile management panel, when open.
    pub security_panel: Option<SecurityPanel>,
    /// Modal question panel shown when the model calls `ask_user`.
    pub question_panel: Option<QuestionPanel>,
    /// Todo-list panel shown with `/todo`.
    pub todo_panel: Option<TodoPanel>,
    /// Full-screen checkpoint selector opened by `/rewind`.
    pub rewind_panel: Option<RewindPanel>,
    /// Full-screen interactive terminal panel, when open (`/terminal`).
    pub terminal_pane: Option<crate::ui::components::terminal_panel::TerminalPane>,
    /// Full-screen read-only child conversation, opened from the Agents sidebar.
    pub(crate) agent_panel: Option<crate::ui::components::agent_panel::AgentPanel>,
    /// Terminal task events waiting to be delivered to the agent.
    pub(crate) task_notifications: TaskNotificationState,
    /// Completed sub-agents waiting to be delivered to the parent agent.
    pub(crate) agent_notifications: AgentNotificationState,
    /// Per-application in-process sub-agent registry.
    pub(crate) agents: crate::agents::AgentManager,
    /// Right-hand sidebar panel (toggled with Ctrl+B).
    pub sidebar: Option<Sidebar>,
    /// The sidebar's screen area from the last render, used to route mouse
    /// scroll events to the correct panel.
    pub sidebar_area: Option<Rect>,
    /// UI snapshot of the current session's todo list.
    pub todo_list: crate::todos::TodoList,
    /// Shared per-session todo state used by the model's `todo` tool.
    pub(crate) todo_store: Arc<Mutex<crate::todos::TodoList>>,
    /// Loaded agent skills, with activation state.
    pub(crate) skill_registry: crate::skills::SkillRegistry,
    /// MCP server manager (None if no servers configured).
    pub(crate) mcp_manager: Option<Arc<crate::mcp::McpManager>>,
    /// Per-server MCP connection state shown while the manager is loading.
    pub(crate) mcp_server_statuses: Vec<McpServerStatus>,
    /// Identifies the latest background MCP reload so stale results are ignored.
    pub(crate) mcp_reload_generation: u64,
    /// Current safety/work mode.
    pub work_mode: WorkMode,
    /// Live security policy shared by the UI and every local tool provider.
    pub(crate) security: Arc<crate::security::SecurityHandle>,
    /// Manual-mode pending tool-call review (per-call, no batch). `None` when
    /// no review is in progress.
    pub(crate) pending_review: Option<PendingReview>,
    /// Concurrent sub-agent reviews waiting for the single approval surface.
    pub(crate) review_queue: VecDeque<PendingReview>,
    /// Classifier models discovered not to support logprobs, so Auto mode skips
    /// the single-token fast path and goes straight to the merged reasoned call.
    pub(crate) classifier_no_logprobs: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    /// Diagnostics baseline and edit-turn bookkeeping.
    pub(crate) diag: DiagnosticsState,
    /// Shared diagnostics state the runner also reads/writes for post-edit
    /// feedback (baseline + edit-turn counter). The TUI holds this so it
    /// persists across per-turn engines.
    pub(crate) diagnostics_state: Arc<std::sync::Mutex<crate::runner::DiagnosticsState>>,
    /// Rejects stale results when diagnostics are manually refreshed twice.
    pub(crate) diagnostics_update_generation: u64,
    /// Tracks whether the current mouse-drag started in the sidebar area.
    pub(crate) sidebar_click_active: bool,
    /// Cancellation tokens for the current request lifecycle.
    pub(crate) cancel: CancelState,
    /// Session identity, persistence handle, and deferred-save flag.
    pub(crate) session: SessionState,
    /// Background automatic compaction bookkeeping. This is deliberately
    /// independent from the foreground turn phase and cancellation token.
    pub(crate) auto_compact: AutoCompactState,
    pub(crate) checkpoint_store: Option<Arc<Mutex<crate::checkpoint::CheckpointStore>>>,
    pub(crate) current_checkpoint_id: Option<u64>,
    /// Project directory name for the terminal title.
    pub(crate) project_name: String,
    /// Plan mode sub-phase. Only meaningful when `work_mode == WorkMode::Plan`.
    pub(crate) plan_phase: crate::classifier::PlanPhase,
    /// Which option is highlighted in the plan review bar.
    pub(crate) plan_review_selected: usize,
}

impl std::fmt::Debug for App<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("running", &self.running)
            .field("provider_manager", &self.provider_manager)
            .field("current_model", &self.current_model)
            .field("config", &self.config)
            .field("input_panel", &self.input_panel)
            .field("conversation_panel", &self.conversation_panel)
            .field("footer", &self.footer)
            .finish()
    }
}

impl App<'_> {
    /// Validate and install the active policy after a profile switch or edit.
    pub(crate) fn install_active_security(&mut self) -> Result<(), String> {
        let security =
            crate::security::SecurityManager::for_current_dir(self.config.security.clone())?;
        self.security.replace(Arc::new(security))
    }

    /// Constructs a new instance of [`App`].
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn new(
        mut config: ProgrammerConfig,
        saved_items: Vec<MessageItem>,
        saved_history: Vec<String>,
        saved_todos: Vec<crate::todos::Todo>,
        session_uuid: String,
        session_mgr: Option<SessionManager>,
        startup_messages: Vec<String>,
        open_provider_panel: bool,
        project_name: String,
    ) -> Self {
        config.normalize_security_profiles();
        let provider_manager = ProviderManager::from_config(&config);
        let mut current_model = provider_manager.default_model();
        let mut work_mode = WorkMode::default();
        let mut vision_enabled = false;
        let mut thinking_level = crate::thinking::ThinkingLevel::default();
        let mut classifier_model_override = ModelOverride::Inherit;
        let mut classifier_top_logprobs_override = None;
        let mut compact_model_override = ModelOverride::Inherit;
        let mut auto_compact_override = AutoCompactOverride::Inherit;
        let mut compact_keep_recent_turns_override = None;

        let mut saved_activated_skills: Option<Vec<String>> = None;
        if let Some(mgr) = &session_mgr
            && let Some(saved) = mgr.load(&session_uuid)
        {
            if let Some(wm) = saved.work_mode {
                work_mode = wm;
            }
            if let Some(model) = saved.current_model
                && provider_manager.resolve(&model).is_some()
            {
                current_model = model;
            }
            vision_enabled = saved.vision_enabled;
            thinking_level = saved.thinking_level;
            classifier_model_override = saved.classifier_model_override;
            classifier_top_logprobs_override = saved.classifier_top_logprobs_override;
            compact_model_override = saved.compact_model_override;
            auto_compact_override = saved.auto_compact_override;
            compact_keep_recent_turns_override = saved.compact_keep_recent_turns_override;
            if saved.skill_selection_saved || !saved.activated_skills.is_empty() {
                saved_activated_skills = Some(saved.activated_skills);
            }
        }
        let mut conversation_panel = ConversationPanel::new();
        conversation_panel.restore_items(saved_items);
        events::remove_quit_confirmation_warning(&mut conversation_panel);
        for msg in startup_messages {
            conversation_panel.add_info_string(msg);
        }
        if config.providers.is_empty() {
            conversation_panel.add_warning_string(
                "no providers configured — press / then type 'providers manage' to add one, \
                 or restart with the --providers flag",
            );
        }
        let mut input_panel = InputPanel::new();
        input_panel.history = saved_history;
        let todo_list = crate::todos::TodoList { todos: saved_todos };
        let todo_store = Arc::new(Mutex::new(todo_list.clone()));
        let security_manager = Arc::new(
            crate::security::SecurityManager::for_current_dir(config.security.clone())
                .expect("security configuration should be validated before starting the app"),
        );
        crate::security::install_active(security_manager.clone());
        let security = Arc::new(crate::security::SecurityHandle::new(security_manager));
        let mcp_server_statuses = config
            .mcp_servers
            .iter()
            .map(|server| McpServerStatus::connecting(server.name.clone()))
            .collect();
        let provider_model_statuses = ProviderModelStatus::from_config(&config);
        let checkpoint_store = crate::checkpoint::CheckpointStore::for_session(&session_uuid)
            .map(|store| Arc::new(Mutex::new(store)));
        let mut app = Self {
            running: true,
            quit_requested_at: None,
            provider_manager,
            provider_model_statuses,
            current_model,
            vision_enabled,
            native_selection_mode: false,
            thinking_level,
            pending_images: Vec::new(),
            events: EventHandler::new(),
            config,
            input_panel,
            conversation_panel,
            footer: Footer::new(),
            provider_panel: open_provider_panel.then(ProviderPanel::new),
            skills_panel: None,
            mcp_panel: None,
            diagnostics_panel: None,
            security_panel: None,
            question_panel: None,
            todo_panel: None,
            rewind_panel: None,
            terminal_pane: None,
            agent_panel: None,
            task_notifications: TaskNotificationState::new(),
            agent_notifications: AgentNotificationState::new(),
            agents: crate::agents::AgentManager::default(),
            sidebar: Some(Sidebar::new()),
            sidebar_area: None,
            todo_list,
            todo_store,
            work_mode,
            security,
            pending_review: None,
            review_queue: VecDeque::new(),
            classifier_no_logprobs: Arc::new(std::sync::Mutex::new(
                std::collections::HashSet::new(),
            )),
            diag: DiagnosticsState {
                lsp_configured: helpers::lsp_checker_configured(),
            },
            diagnostics_state: Arc::new(std::sync::Mutex::new(
                crate::runner::DiagnosticsState::default(),
            )),
            diagnostics_update_generation: 0,
            sidebar_click_active: false,
            cancel: CancelState {
                active: CancellationToken::new(),
                next_id: 0,
                active_id: None,
                turn_conversation_cutoff: None,
                stream_retrying: Arc::new(AtomicBool::new(false)),
                response_started: false,
                active_user_request: None,
            },
            session: SessionState {
                uuid: session_uuid,
                mgr: session_mgr,
                dirty: false,
                did_save: false,
                classifier_model_override,
                classifier_top_logprobs_override,
                compact_model_override,
                auto_compact_override,
                compact_keep_recent_turns_override,
            },
            auto_compact: AutoCompactState::default(),
            checkpoint_store,
            current_checkpoint_id: None,
            skill_registry: crate::skills::SkillRegistry::load(),
            mcp_manager: None,
            mcp_server_statuses,
            mcp_reload_generation: 0,
            plan_phase: crate::classifier::PlanPhase::default(),
            plan_review_selected: 0,
            project_name,
        };

        if let Some(saved_activated_skills) = saved_activated_skills {
            app.skill_registry.set_activated(&saved_activated_skills);
        }

        if app
            .config
            .providers
            .values()
            .any(|provider| provider.models.is_none())
        {
            app.events
                .send(crate::ui::event::AppEvent::RefreshProviderModels {
                    name: None,
                    notify: false,
                });
        }
        if !app.config.mcp_servers.is_empty() {
            app.events.send(crate::ui::event::AppEvent::McpChanged);
        }

        let (task_event_tx, mut task_event_rx) = tokio::sync::mpsc::unbounded_channel();
        crate::tasks::install_event_sink(task_event_tx);
        let app_event_tx = app.events.sender.clone();
        tokio::spawn(async move {
            while let Some(event) = task_event_rx.recv().await {
                if app_event_tx
                    .send(Event::App(crate::ui::event::AppEvent::TaskStateChanged(
                        event,
                    )))
                    .is_err()
                {
                    break;
                }
            }
        });

        if app.config.auto_update_check {
            let app_event_tx = app.events.sender.clone();
            tokio::spawn(async move {
                // Quietly ask GitHub for the latest tag; the check never blocks
                // the UI and failures (offline, rate limit) are simply ignored.
                if let Some(tag) = crate::upgrade::check_for_update().await {
                    let _ = app_event_tx
                        .send(Event::App(crate::ui::event::AppEvent::UpdateAvailable(tag)));
                }
            });
        }

        app
    }

    pub(crate) fn sync_todos_from_store(&mut self) {
        if let Ok(list) = self.todo_store.lock() {
            self.todo_list = list.clone();
        }
    }

    pub(crate) fn sync_todos_to_store(&self) {
        if let Ok(mut list) = self.todo_store.lock() {
            *list = self.todo_list.clone();
        }
    }

    pub(crate) fn effective_classifier_model(&self) -> String {
        self.effective_model_override(
            &self.session.classifier_model_override,
            self.config.classifier_model.as_deref(),
        )
    }

    pub(crate) fn effective_classifier_top_logprobs(&self) -> u8 {
        self.session
            .classifier_top_logprobs_override
            .unwrap_or(self.config.classifier_top_logprobs)
    }

    pub(crate) fn effective_compact_model(&self) -> String {
        self.effective_model_override(
            &self.session.compact_model_override,
            self.config.compact_model.as_deref(),
        )
    }

    pub(crate) fn effective_auto_compact_tokens(&self) -> Option<u32> {
        match self.session.auto_compact_override {
            AutoCompactOverride::Inherit => {
                (self.config.auto_compact_tokens > 0).then_some(self.config.auto_compact_tokens)
            }
            AutoCompactOverride::Disabled => None,
            AutoCompactOverride::Tokens(tokens) => Some(tokens),
        }
    }

    pub(crate) fn effective_compact_keep_recent_turns(&self) -> usize {
        self.session
            .compact_keep_recent_turns_override
            .unwrap_or(self.config.compact_keep_recent_turns)
    }

    pub(crate) fn checkpoint_recorder(&self) -> Option<crate::checkpoint::CheckpointRecorder> {
        Some(crate::checkpoint::CheckpointRecorder {
            store: self.checkpoint_store.as_ref()?.clone(),
            checkpoint_id: self.current_checkpoint_id?,
        })
    }

    fn effective_model_override(
        &self,
        model_override: &ModelOverride,
        global: Option<&str>,
    ) -> String {
        match model_override {
            ModelOverride::Inherit => global.unwrap_or(&self.current_model).to_string(),
            ModelOverride::Current => self.current_model.clone(),
            ModelOverride::Model(model) => model.clone(),
        }
    }

    /// Build a fresh [`crate::runner::TurnRunner`] for the current app state.
    /// Called at the start of every turn; the runner is immutable during a turn
    /// and is dropped when the spawned task finishes.
    pub(crate) fn build_runner(&self) -> Option<crate::runner::TurnRunner> {
        use crate::runner::{LlmPolicy, RunnerPolicy, TurnRunner};
        use std::sync::Arc;

        use crate::tools::provider::{
            LocalToolProvider, McpToolProvider, SkillToolProvider, ToolProvider, ToolRegistry,
        };

        let (client, model_name) = self.provider_manager.resolve(&self.current_model)?;
        let model_str = self.current_model.clone();
        // Unify every tool source behind the registry: the local built-ins are
        // one provider, all connected MCP servers another.
        let mut base_providers: Vec<Arc<dyn ToolProvider>> = vec![
            Arc::new(
                LocalToolProvider::new(self.todo_store.clone(), self.security.clone())
                    .with_checkpoint(self.checkpoint_recorder()),
            ),
            Arc::new(SkillToolProvider::new(self.skill_registry.clone())),
        ];
        if let Some(mcp) = &self.mcp_manager {
            base_providers.push(Arc::new(McpToolProvider::new(mcp.clone())));
        }
        let (policy, child_policy) = match self.work_mode {
            WorkMode::Yolo => (RunnerPolicy::Yolo, crate::agents::AgentPolicyFactory::Yolo),
            WorkMode::Manual | WorkMode::Plan => (
                RunnerPolicy::Sync(self.work_mode.classifier()),
                crate::agents::AgentPolicyFactory::Sync(self.work_mode),
            ),
            WorkMode::Auto => {
                let model_str = self.effective_classifier_model();
                let (c_client, c_model_name) = self.provider_manager.resolve(&model_str)?;
                let top_logprobs = self.effective_classifier_top_logprobs();
                (
                    RunnerPolicy::Llm(Box::new(LlmPolicy {
                        client: c_client.clone(),
                        model_name: c_model_name.clone(),
                        top_logprobs,
                        no_logprobs: self.classifier_no_logprobs.clone(),
                    })),
                    crate::agents::AgentPolicyFactory::Llm(Box::new(LlmPolicy {
                        client: c_client.clone(),
                        model_name: c_model_name,
                        top_logprobs,
                        no_logprobs: self.classifier_no_logprobs.clone(),
                    })),
                )
            }
        };

        let child_runtime = crate::agents::AgentRuntime {
            provider_manager: Arc::new(self.provider_manager.clone()),
            client: client.clone(),
            model_name: model_name.clone(),
            model_str: model_str.clone(),
            todos: self.todo_store.clone(),
            security: self.security.clone(),
            mcp_manager: self.mcp_manager.clone(),
            policy: child_policy,
            coauthor: self.config.git_coauthor.clone(),
            vision_enabled: self.vision_enabled,
            thinking_level: self.thinking_level,
            skill_registry: self.skill_registry.clone(),
            skill_prompt: self.skill_registry.catalog_prompt(),
            approval_label: format!(
                "{} approved by {} mode (sub-agent)",
                self.work_mode.icon(),
                self.work_mode.label()
            ),
            checkpoint: self.checkpoint_recorder(),
        };
        base_providers.push(Arc::new(crate::tools::provider::AgentToolProvider::new(
            self.agents.clone(),
            child_runtime,
        )));
        let tools = Arc::new(ToolRegistry::new(base_providers));

        Some(TurnRunner {
            client: client.clone(),
            model_name,
            model_str,
            tools,
            policy,
            coauthor: self.config.git_coauthor.clone(),
            vision_enabled: self.vision_enabled,
            thinking_level: self.thinking_level,
            hooks: crate::runner::hooks::standard_hooks(self.diagnostics_state.clone()),
            stream_retrying: self.cancel.stream_retrying.clone(),
            max_steps: None,
        })
    }

    /// Run the application's main loop. Returns the final session UUID.
    pub(crate) async fn run(
        mut self,
        mut terminal: DefaultTerminal,
    ) -> (color_eyre::Result<()>, Option<String>) {
        // Kick off diagnostics baseline seeding on startup.
        crate::app::diagnostics::maybe_seed_diagnostics_baseline(&mut self);

        let result = async {
            while self.running {
                terminal.draw(|frame| frame.render_widget(&mut self, frame.area()))?;

                let mut event = Some(self.events.next().await?);
                for index in 0..MAX_EVENTS_PER_FRAME {
                    let Some(current) = event.take() else {
                        break;
                    };
                    self.handle_event(current).await?;
                    if !self.running {
                        break;
                    }
                    if index + 1 < MAX_EVENTS_PER_FRAME {
                        event = self.events.try_next();
                    }
                }
            }
            Ok(())
        }
        .await;
        crate::diagnostics::shutdown_lsp().await;
        let uuid = if self.session.did_save {
            Some(self.session.uuid.clone())
        } else {
            None
        };
        (result, uuid)
    }

    // ---------------------------------------------------------------
    // Delegating methods — implementation lives in submodules
    // ---------------------------------------------------------------

    async fn handle_event(&mut self, event: Event) -> color_eyre::Result<()> {
        events::handle_event(self, event).await
    }

    pub async fn handle_key_events(&mut self, key_event: KeyEvent) -> color_eyre::Result<()> {
        events::handle_key_events(self, key_event).await
    }

    pub fn tick(&mut self) {
        events::tick(self)
    }

    pub fn quit(&mut self) {
        self.agents.cancel_all();
        session::save_session(self);
        self.running = false;
    }
}

#[cfg(test)]
mod tests {
    use super::TaskNotificationState;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    fn event(sequence: u64) -> crate::tasks::TaskLifecycleEvent {
        crate::tasks::TaskLifecycleEvent {
            sequence,
            generation: 3,
            task_id: sequence,
            origin: crate::tasks::TaskOrigin::TaskTool,
            old_status: crate::tasks::TaskStatus::Running,
            new_status: crate::tasks::TaskStatus::Completed,
            name: "test".to_string(),
            command: "true".to_string(),
            exit_code: Some(0),
            elapsed: Duration::ZERO,
            stdout_tail: String::new(),
            stderr_tail: String::new(),
            transcript_tail: String::new(),
            notify_agent: Arc::new(AtomicBool::new(true)),
        }
    }

    #[test]
    fn task_notifications_are_deduplicated_and_clear_resets_delivery() {
        let mut state = TaskNotificationState::new();
        assert!(state.push(event(1)));
        assert!(!state.push(event(1)));
        assert_eq!(state.pending.len(), 1);
        assert!(state.ready_at.is_some());

        state.pending[0]
            .notify_agent
            .store(false, Ordering::Release);
        state.discard_consumed();
        assert!(state.pending.is_empty());
        assert!(state.ready_at.is_none());

        state.clear();
        assert!(state.pending.is_empty());
        assert!(state.ready_at.is_none());
        assert!(state.push(event(1)));
    }

    #[tokio::test]
    async fn app_construction_does_not_wait_for_mcp_handshake() {
        let mut config = crate::config::programmer_config::ProgrammerConfig::default();
        config.providers.clear();
        config.mcp_servers.push(crate::mcp::types::McpServerConfig {
            name: "slow".to_string(),
            command: "server-that-never-responds".to_string(),
            args: Vec::new(),
            env: std::collections::HashMap::new(),
            url: None,
        });

        let app = tokio::time::timeout(
            Duration::from_secs(2),
            super::App::new(
                config,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                "startup-test".to_string(),
                None,
                Vec::new(),
                false,
                "test-project".to_string(),
            ),
        )
        .await
        .expect("App::new must not wait for an MCP process or handshake");

        assert!(app.mcp_manager.is_none());
        assert_eq!(app.mcp_server_statuses.len(), 1);
        assert_eq!(app.mcp_server_statuses[0].name, "slow");
        assert!(matches!(
            app.mcp_server_statuses[0].state,
            crate::mcp::McpConnectionState::Connecting
        ));
    }
}
