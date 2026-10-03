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

pub(crate) mod activity;
mod command_handlers;
pub(crate) mod commands;
pub(crate) mod diagnostics;
pub(crate) mod events;
pub(crate) mod helpers;
mod lifecycle;
pub(crate) mod peers;
mod scheduling;
pub(crate) mod session;
pub(crate) mod surface;

use crate::cancel::{CancellationToken, OperationId};
use crate::classifier::WorkMode;
use crate::config::programmer_config::ProgrammerConfig;
use crate::providers::ProviderManager;
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
use std::collections::{HashSet, VecDeque};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Bound how many already-queued events are folded into state in one pass.
/// Bound how many non-streaming events are folded into state in one pass.
const MAX_EVENTS_PER_FRAME: usize = 4;
/// Streaming chunks are cheap to merge and must not consume the interaction
/// event budget, otherwise a fast provider can delay keyboard and mouse input.
const MAX_CHUNKS_PER_BATCH: usize = 256;

fn event_requests_immediate_redraw(event: &Event) -> bool {
    !matches!(
        event,
        Event::App(crate::ui::event::AppEvent::ChunkReceived(_, _))
            | Event::Crossterm(crossterm::event::Event::Mouse(
                crossterm::event::MouseEvent {
                    kind: crossterm::event::MouseEventKind::Moved,
                    ..
                }
            ))
    )
}

fn is_left_drag(event: &Event) -> bool {
    matches!(
        event,
        Event::Crossterm(crossterm::event::Event::Mouse(
            crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
                ..
            }
        ))
    )
}

fn scroll_direction(event: &Event) -> Option<crossterm::event::MouseEventKind> {
    match event {
        Event::Crossterm(crossterm::event::Event::Mouse(mouse)) => match mouse.kind {
            crossterm::event::MouseEventKind::ScrollUp
            | crossterm::event::MouseEventKind::ScrollDown => Some(mouse.kind),
            _ => None,
        },
        _ => None,
    }
}

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
    pub(crate) operation_id: OperationId,
    /// Child id for a sub-agent review; absent for the main turn.
    pub(crate) agent_id: Option<u64>,
    pub(crate) agent_generation: Option<u64>,
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
    /// `/init`) bumps this by 1 so operation ids are never reused within an
    /// App lifetime. Exhaustion fails explicitly rather than reusing an id.
    pub(crate) next_id: u64,
    /// The current turn's operation id, or `None` when idle. Set synchronously
    /// before the turn spawns and cleared when `AppEvent::TurnFinished`
    /// arrives, so the UI never races between "start" and "what is my id?" and
    /// stale events from an earlier turn are always dropped.
    pub(crate) active_id: Option<OperationId>,
    /// Last observed work; frozen when cancellation is requested until terminal acknowledgement.
    pub(crate) activity: Option<String>,
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

/// Stopping is retryable, but never admits work again, even after a failed save.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionLifecycle {
    Running,
    Stopping,
}

/// Session identity, execution settings, shared resources, and persistence state.
pub(crate) struct SessionState {
    pub(crate) lifecycle: SessionLifecycle,
    pub(crate) conversation: Arc<Mutex<crate::conversation::Conversation>>,
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
    pub(crate) persistence: session::PersistenceState,
    /// Model-generated title persisted with the session.
    pub(crate) title: String,
    /// Prevent duplicate title requests while the first one is in flight.
    pub(crate) title_generation_started: bool,
    /// Monotonic id used to ignore stale title-generation results.
    pub(crate) title_generation_id: u64,
    pub(crate) classifier_model_override: ModelOverride,
    pub(crate) compact_model_override: ModelOverride,
    pub(crate) auto_compact_override: AutoCompactOverride,
    pub(crate) compact_keep_recent_turns_override: Option<usize>,
    pub(crate) tasks: crate::tasks::TaskManager,
    /// Currently active model in `provider/model` format.
    pub current_model: String,
    /// Whether supported `@image` references are sent as multimodal inputs.
    pub vision_enabled: bool,
    /// Reasoning effort for main chat and compaction requests.
    pub(crate) thinking_level: crate::thinking::ThinkingLevel,
    /// Per-application in-process sub-agent registry.
    pub(crate) agents: crate::agents::AgentManager,
    /// Shared per-session todo state used by the model's `todo` tool.
    pub(crate) todo_store: Arc<Mutex<crate::todos::TodoList>>,
    /// Current safety/work mode.
    pub work_mode: WorkMode,
    /// Shared diagnostics state the runner also reads/writes for post-edit
    /// feedback (baseline + edit-turn counter). The TUI holds this so it
    /// persists across per-turn engines.
    pub(crate) diagnostics_state: Arc<std::sync::Mutex<crate::runner::DiagnosticsState>>,
    pub(crate) checkpoint_store: Option<Arc<Mutex<crate::checkpoint::CheckpointStore>>>,
}

#[derive(Debug, Default)]
pub(crate) struct AutoCompactState {
    pub(crate) next_id: u64,
    pub(crate) active_id: Option<u64>,
    pub(crate) history_epoch: u64,
    pub(crate) last_cutoff: Option<usize>,
    pub(crate) last_input_tokens: Option<u32>,
    /// Conversation length immediately after the latest successful automatic
    /// compaction, used to count subsequent user turns for cooldown.
    pub(crate) last_completed_item_count: Option<usize>,
    /// A user/runtime request is queued behind a mandatory compaction.
    pub(crate) mandatory_waiting: bool,
    /// Failed mandatory compaction requires explicit retry, never an idle-event loop.
    pub(crate) retry_blocked: bool,
    pub(crate) mandatory_resume: Option<tokio::sync::oneshot::Sender<()>>,
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

/// Presentation state grouped without changing component or conversation ownership.
pub struct UiState<'a> {
    /// Time of the first Ctrl+C press while waiting for exit confirmation.
    pub(crate) quit_requested_at: Option<std::time::Instant>,
    /// Whether the terminal emulator owns mouse drags for native text
    /// selection instead of the TUI receiving mouse events.
    pub(crate) native_selection_mode: bool,
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
    pub(crate) runner_prompts: VecDeque<(OperationId, QuestionPanel)>,
    /// Todo-list panel shown with `/todo`.
    pub todo_panel: Option<TodoPanel>,
    /// Full-screen checkpoint selector opened by `/rewind`.
    pub rewind_panel: Option<RewindPanel>,
    /// Full-screen interactive terminal panel, when open (`/terminal`).
    pub terminal_pane: Option<crate::ui::components::terminal_panel::TerminalPane>,
    /// Full-screen read-only child conversation, opened from the Agents sidebar.
    pub(crate) agent_panel: Option<crate::ui::components::agent_panel::AgentPanel>,
    pub(crate) activity_page: Option<activity::ActivityPage>,
    /// Right-hand sidebar panel (toggled with Ctrl+B).
    pub sidebar: Option<Sidebar>,
    /// The sidebar's screen area from the last render, used to route mouse
    /// scroll events to the correct panel.
    /// Tracks whether the current mouse-drag started in the sidebar area.
    pub(crate) sidebar_click_active: bool,
    /// UI snapshot of the current session's todo list.
    pub todo_list: crate::todos::TodoList,
    /// Operation whose completed turn is currently generating an input hint.
    pub(crate) active_suggestion_operation_id: Option<OperationId>,
    /// Cancels obsolete hint requests when another user turn starts.
    pub(crate) input_suggestion_cancel: Option<crate::cancel::CancellationToken>,
    /// Which option is highlighted in the plan review bar.
    pub(crate) plan_review_selected: usize,
}

/// Runtime state of the foreground agent loop.
pub(crate) struct LoopState {
    pub(crate) background_handles: Vec<tokio::task::JoinHandle<()>>,
    pub(crate) runner: Option<Arc<crate::runner::TurnRunner>>,
    pub(crate) peer_inbox: peers::PeerInbox,
    pub(crate) runner_handles: Vec<tokio::task::JoinHandle<()>>,
    pub(crate) pending_request: Option<scheduling::UserRequest>,
    pub(crate) phase: crate::execution::ActivePhase,
    /// Local authorization is intentionally never restored from disk.
    pub(crate) peer_delegations: std::collections::HashMap<String, peers::DelegationState>,
    pub(crate) peer_consent: Option<(
        crate::peers::PeerEnvelope,
        tokio::sync::oneshot::Receiver<String>,
    )>,
    /// Terminal task events waiting to be delivered to the agent.
    pub(crate) task_notifications: TaskNotificationState,
    /// Completed sub-agents waiting to be delivered to the parent agent.
    pub(crate) agent_notifications: AgentNotificationState,
    /// Loaded agent skills, with activation state.
    pub(crate) skill_registry: crate::skills::SkillRegistry,
    /// Manual-mode pending tool-call review (per-call, no batch). `None` when
    /// no review is in progress.
    pub(crate) pending_review: Option<PendingReview>,
    /// Concurrent sub-agent reviews waiting for the single approval surface.
    pub(crate) review_queue: VecDeque<PendingReview>,
    /// Classifier models discovered not to support logprobs, so Auto mode skips
    /// the single-token fast path and goes straight to the merged reasoned call.
    pub(crate) classifier_no_logprobs: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    /// Cancellation tokens for the current request lifecycle.
    pub(crate) cancel: CancelState,
    /// Session settings, resources, and persistence owned by this loop.
    pub(crate) session: SessionState,
    /// Background automatic compaction bookkeeping. This is deliberately
    /// independent from the foreground turn phase and cancellation token.
    pub(crate) auto_compact: AutoCompactState,
    pub(crate) waiting_for_subagents: bool,
    pub(crate) current_checkpoint_id: Option<u64>,
    /// Plan mode sub-phase. Only meaningful when `session.work_mode == WorkMode::Plan`.
    pub(crate) plan_phase: crate::classifier::PlanPhase,
}

/// Application.
pub struct App<'a> {
    /// Presentation state and UI components owned by this application.
    pub ui: UiState<'a>,
    /// Is the application running?
    pub running: bool,
    /// Multi-provider manager (replaces the single OpenAI client).
    pub provider_manager: ProviderManager,
    /// Event handler.
    pub events: EventHandler,
    /// Application configuration.
    pub config: ProgrammerConfig,
    /// Stable owner of MCP reload state and replaceable connection snapshots.
    pub(crate) mcp_runtime: crate::mcp::runtime::McpRuntime,
    /// Live security policy shared by the UI and every local tool provider.
    pub(crate) security: Arc<crate::security::SecurityHandle>,
    /// Live Dream inputs (settings + resolved memory model) read by the worker.
    pub(crate) dream_runtime: Arc<std::sync::Mutex<crate::memory::dream::DreamRuntime>>,
    /// Background consolidation worker, stopped when the application exits.
    pub(crate) dream_worker: Option<crate::memory::dream::DreamWorker>,
    /// Raised by the worker while a consolidation pass is running, so the title
    /// bar can show a Dream indicator. Purely presentational: nothing else reads
    /// it, and it stays false whenever memory or Dream is off.
    pub(crate) dream_active: Arc<std::sync::atomic::AtomicBool>,
    /// Project directory name for the terminal title.
    pub(crate) project_name: String,

    pub(crate) agent_loop: LoopState,
    pub(crate) task_event_generation: u64,
}

impl std::fmt::Debug for App<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("running", &self.running)
            .field("provider_manager", &self.provider_manager)
            .field("current_model", &self.agent_loop.session.current_model)
            .field("config", &self.config)
            .field("input_panel", &self.ui.input_panel)
            .field("conversation_panel", &self.ui.conversation_panel)
            .field("footer", &self.ui.footer)
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
    pub(crate) async fn new(
        mut config: ProgrammerConfig,
        seed: session::SessionSeed,
        session_mgr: Option<SessionManager>,
        startup_messages: Vec<String>,
        open_provider_panel: bool,
        project_name: String,
    ) -> Self {
        config.normalize_security_profiles();
        let provider_manager = ProviderManager::from_config(&config);
        let mut current_model = provider_manager.default_model();
        let mut work_mode = WorkMode::default();
        let mut vision_enabled = config.vision_enabled;
        let mut thinking_level = crate::thinking::ThinkingLevel::default();
        let mut classifier_model_override = ModelOverride::Inherit;
        let mut compact_model_override = ModelOverride::Inherit;
        let mut auto_compact_override = AutoCompactOverride::Inherit;
        let mut compact_keep_recent_turns_override = None;
        let mut session_title = String::new();
        let mut saved_input_suggestion = None;
        let mut saved_last_request_input_tokens = None;

        let mut saved_activated_skills: Option<Vec<String>> = None;
        let mut saved_file_snapshots: Vec<crate::security::policy::PersistedFileSnapshot> =
            Vec::new();
        let persistence = session::PersistenceState::default();
        let tasks = crate::tasks::TaskManager::default();
        let mut saved_items = Vec::new();
        let mut saved_history = Vec::new();
        let mut saved_todos = Vec::new();
        let mut saved_agents = Vec::new();
        let (session_uuid, restored_session) = match seed {
            session::SessionSeed::Fresh { uuid } => (uuid, None),
            session::SessionSeed::Restored(snapshot) => (snapshot.uuid.clone(), Some(snapshot)),
        };
        if let Some(saved) = restored_session {
            tasks.restore(&saved.tasks);
            saved_items = saved
                .items
                .into_iter()
                .map(crate::response::message_item::MessageItem::from)
                .collect();
            saved_history = saved.history;
            saved_todos = saved.todos;
            saved_agents = saved.agents;
            if let Some(wm) = saved.work_mode {
                work_mode = wm;
            }
            if let Some(model) = saved.current_model
                && provider_manager.resolve(&model).is_some()
            {
                current_model = model;
            }
            session_title = saved.title;
            saved_input_suggestion = saved.input_suggestion;
            saved_last_request_input_tokens = saved.last_request_input_tokens;
            saved_file_snapshots = saved.file_snapshots;
            vision_enabled = saved.vision_enabled;
            thinking_level = saved.thinking_level;
            classifier_model_override = saved.classifier_model_override;
            compact_model_override = saved.compact_model_override;
            auto_compact_override = saved.auto_compact_override;
            compact_keep_recent_turns_override = saved.compact_keep_recent_turns_override;
            if saved.skill_selection_saved || !saved.activated_skills.is_empty() {
                saved_activated_skills = Some(saved.activated_skills);
            }
        }
        let conversation = Arc::new(Mutex::new(crate::conversation::Conversation::new()));
        let mut conversation_panel = ConversationPanel::from_shared(conversation.clone());
        conversation_panel.tasks = tasks.clone();
        conversation.lock().unwrap().restore_items(saved_items);
        conversation_panel.history_restored();
        conversation.lock().unwrap().last_request_input_tokens = saved_last_request_input_tokens;
        events::remove_quit_confirmation_warning(&conversation, &mut conversation_panel);
        for msg in startup_messages {
            conversation.lock().unwrap().add_info_string(msg);
            conversation_panel.scroll_to_bottom();
        }
        if config.providers.is_empty() {
            conversation.lock().unwrap().add_warning_string(
                "no providers configured — press / then type 'providers manage' to add one, \
                 or restart with the --providers flag",
            );
        }
        let mut input_panel = InputPanel::new();
        input_panel.history = saved_history;
        if let Some(suggestion) = saved_input_suggestion {
            input_panel.set_suggestion(suggestion);
        }
        let todo_list = crate::todos::TodoList { todos: saved_todos };
        let todo_store = Arc::new(Mutex::new(todo_list.clone()));
        let security_manager = Arc::new(
            crate::security::SecurityManager::for_current_dir(config.security.clone())
                .expect("security configuration should be validated before starting the app"),
        );
        crate::security::install_active(security_manager.clone());
        security_manager.restore_snapshots(&saved_file_snapshots);
        let security = Arc::new(crate::security::SecurityHandle::new(security_manager));
        let mut mcp_runtime = crate::mcp::runtime::McpRuntime::default();
        mcp_runtime.begin_reload(&config.mcp_servers);
        let agents = crate::agents::AgentManager::default();
        agents.restore(&saved_agents);
        let checkpoint_store = crate::checkpoint::CheckpointStore::for_session(&session_uuid)
            .map(|store| Arc::new(Mutex::new(store)));
        let mut app = Self {
            ui: UiState {
                quit_requested_at: None,
                native_selection_mode: false,
                input_panel,
                conversation_panel,
                footer: Footer::new(),
                provider_panel: open_provider_panel.then(ProviderPanel::new),
                skills_panel: None,
                mcp_panel: None,
                diagnostics_panel: None,
                security_panel: None,
                question_panel: None,
                runner_prompts: VecDeque::new(),
                todo_panel: None,
                rewind_panel: None,
                terminal_pane: None,
                agent_panel: None,
                activity_page: None,
                sidebar: Some(Sidebar::new()),
                sidebar_click_active: false,
                todo_list,
                active_suggestion_operation_id: None,
                input_suggestion_cancel: None,
                plan_review_selected: 0,
            },
            running: true,
            provider_manager,
            events: EventHandler::new(),
            config,
            security,
            dream_runtime: Arc::new(std::sync::Mutex::new(
                crate::memory::dream::DreamRuntime::default(),
            )),
            dream_worker: None,
            task_event_generation: 0,
            dream_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            mcp_runtime,

            agent_loop: LoopState {
                background_handles: Vec::new(),
                runner: None,
                peer_inbox: peers::PeerInbox::default(),
                runner_handles: Vec::new(),
                pending_request: None,
                phase: crate::execution::ActivePhase::None,
                peer_delegations: std::collections::HashMap::new(),
                peer_consent: None,
                task_notifications: TaskNotificationState::new(),
                agent_notifications: AgentNotificationState::new(),
                pending_review: None,
                review_queue: VecDeque::new(),
                classifier_no_logprobs: Arc::new(std::sync::Mutex::new(
                    std::collections::HashSet::new(),
                )),
                cancel: CancelState {
                    active: CancellationToken::new(),
                    next_id: 0,
                    active_id: None,
                    activity: None,
                    turn_conversation_cutoff: None,
                    stream_retrying: Arc::new(AtomicBool::new(false)),
                    response_started: false,
                    active_user_request: None,
                },
                session: SessionState {
                    lifecycle: SessionLifecycle::Running,
                    conversation,
                    uuid: session_uuid,
                    mgr: session_mgr,
                    dirty: false,
                    persistence,
                    did_save: false,
                    title_generation_started: !session_title.is_empty(),
                    title_generation_id: 0,
                    title: session_title,
                    classifier_model_override,
                    compact_model_override,
                    auto_compact_override,
                    compact_keep_recent_turns_override,
                    current_model,
                    vision_enabled,
                    thinking_level,
                    tasks,
                    agents,
                    todo_store,
                    work_mode,
                    diagnostics_state: Arc::new(std::sync::Mutex::new({
                        let mut state = crate::runner::DiagnosticsState::default();
                        state.lsp_configured = helpers::lsp_checker_configured();
                        state
                    })),
                    checkpoint_store,
                },
                auto_compact: AutoCompactState::default(),
                waiting_for_subagents: false,
                current_checkpoint_id: None,
                skill_registry: crate::skills::SkillRegistry::load(),
                plan_phase: crate::classifier::PlanPhase::default(),
            },
            project_name,
        };

        if let Some(saved_activated_skills) = saved_activated_skills {
            app.agent_loop
                .skill_registry
                .set_activated(&saved_activated_skills);
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

        app.connect_task_events();

        app.start_auto_dream();

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
        self.ui.todo_list = self.agent_loop.session.todo_store.lock().unwrap().clone();
        if let Some(panel) = &mut self.ui.todo_panel {
            panel.replace_snapshot(self.ui.todo_list.clone());
        }
    }

    pub(crate) fn effective_classifier_model(&self) -> String {
        self.effective_model_override(
            &self.agent_loop.session.classifier_model_override,
            self.config.classifier_model.as_deref(),
        )
    }

    pub(crate) fn effective_compact_model(&self) -> String {
        self.effective_model_override(
            &self.agent_loop.session.compact_model_override,
            self.config.compact_model.as_deref(),
        )
    }

    pub(crate) fn effective_title_model(&self) -> String {
        self.config
            .title_model
            .clone()
            .unwrap_or_else(|| self.agent_loop.session.current_model.clone())
    }

    pub(crate) fn effective_suggestion_model(&self) -> String {
        self.config
            .suggestion_model
            .clone()
            .unwrap_or_else(|| self.agent_loop.session.current_model.clone())
    }

    pub(crate) fn mandatory_compact_tokens(&self) -> Option<u32> {
        (self.config.mandatory_compact_tokens > 0).then_some(self.config.mandatory_compact_tokens)
    }

    pub(crate) fn effective_auto_compact_tokens(&self) -> Option<u32> {
        match self.agent_loop.session.auto_compact_override {
            AutoCompactOverride::Inherit => {
                (self.config.auto_compact_tokens > 0).then_some(self.config.auto_compact_tokens)
            }
            AutoCompactOverride::Disabled => None,
            AutoCompactOverride::Tokens(tokens) => Some(tokens),
        }
    }

    pub(crate) fn effective_compact_keep_recent_turns(&self) -> usize {
        self.agent_loop
            .session
            .compact_keep_recent_turns_override
            .unwrap_or(self.config.compact_keep_recent_turns)
    }

    pub(crate) fn checkpoint_recorder(&self) -> Option<crate::checkpoint::CheckpointRecorder> {
        Some(crate::checkpoint::CheckpointRecorder {
            store: self.agent_loop.session.checkpoint_store.as_ref()?.clone(),
            checkpoint_id: self.agent_loop.current_checkpoint_id?,
        })
    }

    fn effective_model_override(
        &self,
        model_override: &ModelOverride,
        global: Option<&str>,
    ) -> String {
        match model_override {
            ModelOverride::Inherit => global
                .unwrap_or(&self.agent_loop.session.current_model)
                .to_string(),
            ModelOverride::Current => self.agent_loop.session.current_model.clone(),
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

        let (client, model_name) = self
            .provider_manager
            .resolve(&self.agent_loop.session.current_model)?;
        let model_str = self.agent_loop.session.current_model.clone();
        // Keep the Dream worker's view of the memory model current: this runs at
        // the start of every turn, which is exactly when `/model`, provider
        // changes, and `/memory off` have already been applied to `self.config`.
        self.refresh_dream_runtime();
        let memory_model = self.effective_memory_model();
        // Unify every tool source behind the registry: the local built-ins are
        // one provider, all connected MCP servers another.
        let mut base_providers: Vec<Arc<dyn ToolProvider>> = vec![
            Arc::new(
                LocalToolProvider::new(
                    self.agent_loop.session.todo_store.clone(),
                    self.security.clone(),
                )
                .with_diagnostics_state(self.agent_loop.session.diagnostics_state.clone())
                .with_tasks(self.agent_loop.session.tasks.clone())
                .with_checkpoint(self.checkpoint_recorder())
                .with_memory_enabled(self.config.memory.enabled)
                .with_memory_model(memory_model.clone())
                .with_conversation_history(Some(self.agent_loop.session.conversation.clone())),
            ),
            Arc::new(SkillToolProvider::new(
                self.agent_loop.skill_registry.clone(),
            )),
        ];
        if let Some(mcp) = self.mcp_runtime.connections() {
            base_providers.push(Arc::new(McpToolProvider::new(mcp.clone())));
        }
        let (policy, child_policy) = match self.agent_loop.session.work_mode {
            WorkMode::Yolo => (RunnerPolicy::Yolo, crate::agents::AgentPolicyFactory::Yolo),
            WorkMode::Manual | WorkMode::Plan => (
                RunnerPolicy::Sync(self.agent_loop.session.work_mode.classifier()),
                crate::agents::AgentPolicyFactory::Sync(self.agent_loop.session.work_mode),
            ),
            WorkMode::Auto => {
                let model_str = self.effective_classifier_model();
                let (c_client, c_model_name) = self.provider_manager.resolve(&model_str)?;
                let top_logprobs = self.config.classifier_top_logprobs;
                (
                    RunnerPolicy::Llm(Box::new(LlmPolicy {
                        client: c_client.clone(),
                        model_name: c_model_name.clone(),
                        top_logprobs,
                        no_logprobs: self.agent_loop.classifier_no_logprobs.clone(),
                    })),
                    crate::agents::AgentPolicyFactory::Llm(Box::new(LlmPolicy {
                        client: c_client.clone(),
                        model_name: c_model_name,
                        top_logprobs,
                        no_logprobs: self.agent_loop.classifier_no_logprobs.clone(),
                    })),
                )
            }
        };

        let child_runtime = crate::agents::AgentRuntime {
            tasks: self.agent_loop.session.tasks.clone(),
            events: self.events.sender.clone(),
            provider_manager: Arc::new(self.provider_manager.clone()),
            client: client.clone(),
            model_name: model_name.clone(),
            model_str: model_str.clone(),
            todos: self.agent_loop.session.todo_store.clone(),
            security: self.security.clone(),
            mcp_manager: self.mcp_runtime.connections().cloned(),
            policy: child_policy,
            soul: self.config.soul.clone(),
            coauthor: self.config.git_coauthor.clone(),
            vision_enabled: self.agent_loop.session.vision_enabled,
            thinking_level: self.agent_loop.session.thinking_level,
            memory_config: self.config.memory.clone(),
            memory_model: self.config.memory_model.clone(),
            skill_registry: self.agent_loop.skill_registry.clone(),
            skill_prompt: self.agent_loop.skill_registry.catalog_prompt(),
            approval_label: format!(
                "{} approved by {} mode (sub-agent)",
                self.agent_loop.session.work_mode.icon(),
                self.agent_loop.session.work_mode.label()
            ),
            checkpoint: self.checkpoint_recorder(),
            conversation_history: Some(self.agent_loop.session.conversation.clone()),
        };
        base_providers.push(Arc::new(crate::tools::provider::AgentToolProvider::new(
            self.agent_loop.session.agents.clone(),
            child_runtime,
        )));
        base_providers.push(Arc::new(crate::peers::PeerSessionProvider::new(
            self.agent_loop.session.uuid.clone(),
            self.provider_manager.clone(),
        )));
        let tools = Arc::new(ToolRegistry::new(base_providers));

        Some(TurnRunner {
            client: client.clone(),
            model_name,
            model_str,
            tools,
            policy,
            soul: self.config.soul.clone(),
            coauthor: self.config.git_coauthor.clone(),
            vision_enabled: self.agent_loop.session.vision_enabled,
            thinking_level: self.agent_loop.session.thinking_level,
            memory_model,
            hooks: crate::runner::hooks::standard_hooks(
                self.agent_loop.session.diagnostics_state.clone(),
            ),
            stream_retrying: self.agent_loop.cancel.stream_retrying.clone(),
            stream_retry_limit: crate::consts::MAX_STREAM_RETRIES,
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
        // Receive durable peer requests even before the first user turn.
        self.events.schedule_status_tick();

        let result = async {
            // Paint startup state immediately. After this, raw stream chunks only
            // mutate the in-flight response; Tick presents their accumulated state
            // at the configured frame rate. User input and lifecycle events still
            // request an immediate frame.
            terminal.draw(|frame| frame.render_widget(&mut self, frame.area()))?;
            while self.running {
                let mut event = Some(self.events.next().await?);
                let mut redraw = false;
                let mut regular_events = 0;
                let mut chunks = 0;
                loop {
                    let Some(current) = event.take() else {
                        break;
                    };
                    let chunk_received = matches!(
                        &current,
                        Event::App(crate::ui::event::AppEvent::ChunkReceived(_, _))
                    );
                    let current_scroll_direction = scroll_direction(&current);
                    let current_is_left_drag = is_left_drag(&current);
                    redraw |= matches!(&current, Event::Tick | Event::Redraw)
                        || event_requests_immediate_redraw(&current);
                    self.handle_event(current).await?;
                    if chunk_received {
                        chunks += 1;
                    } else {
                        regular_events += 1;
                    }
                    // Status timing remains at 10 FPS. Edge-drag selection has
                    // a separate, smoother timer so it can advance one row per
                    // step without making background housekeeping run faster.
                    // Housekeeping includes peer inbox delivery while idle.
                    self.events.schedule_status_tick();
                    if self.ui.conversation_panel.selection_auto_scroll_active() {
                        self.events.schedule_selection_scroll();
                    }
                    if chunk_received {
                        self.events.schedule_redraw();
                    }
                    // Drag reports can arrive much faster than a full terminal
                    // frame can be drawn. Apply only the latest contiguous point
                    // so stale coordinates cannot build up behind rendering.
                    if current_is_left_drag {
                        let mut latest_drag = None;
                        while let Some(next) = self.events.try_next() {
                            if !is_left_drag(&next) {
                                event = Some(next);
                                break;
                            }
                            latest_drag = Some(next);
                        }
                        if let Some(latest_drag) = latest_drag {
                            self.handle_event(latest_drag).await?;
                            if self.ui.conversation_panel.selection_auto_scroll_active() {
                                self.events.schedule_selection_scroll();
                            }
                        }
                    }
                    // Mouse wheel events are often delivered in a burst. Consume
                    // only the contiguous events with the same direction, so a
                    // reverse scroll or any keyboard/input event remains next.
                    if let Some(direction) = current_scroll_direction {
                        while let Some(next) = self.events.try_next() {
                            if scroll_direction(&next) != Some(direction) {
                                event = Some(next);
                                break;
                            }
                            redraw |= event_requests_immediate_redraw(&next);
                            self.handle_event(next).await?;
                        }
                    }
                    if !self.running || redraw {
                        break;
                    }
                    if regular_events >= MAX_EVENTS_PER_FRAME || chunks >= MAX_CHUNKS_PER_BATCH {
                        break;
                    }
                    if event.is_none() {
                        event = self.events.try_next();
                    }
                }
                if self.running && redraw {
                    terminal.draw(|frame| frame.render_widget(&mut self, frame.area()))?;
                }
            }
            Ok(())
        }
        .await;
        let shutdown = self.close_session().await.and_then(|()| {
            session::persist_session(&mut self).map(|saved| {
                if saved {
                    self.queue_current_session_for_dream();
                }
            })
        });
        crate::memory::dream::shutdown(&mut self.dream_worker);
        let result = match (result, shutdown) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) => Err(error),
            (Ok(()), Err(error)) => Err(color_eyre::eyre::eyre!(error)),
            (Err(error), Err(shutdown)) => {
                Err(error.wrap_err(format!("session shutdown: {shutdown}")))
            }
        };
        crate::diagnostics::shutdown_lsp().await;
        let uuid = if result.is_ok() && self.agent_loop.session.did_save {
            Some(self.agent_loop.session.uuid.clone())
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

    /// Refresh the worker's view of the memory model and Dream settings. Called
    /// before each turn so `/model`, provider edits, and `/memory off` are picked
    /// up without restarting the application.
    fn refresh_dream_runtime(&self) {
        let runtime = crate::memory::dream::DreamRuntime {
            config: crate::memory::dream::DreamConfig::from(&self.config.memory),
            model: self
                .effective_memory_model()
                .map(|model| crate::memory::dream::DreamModel {
                    client: model.client,
                    model: model.model,
                }),
        };
        if let Ok(mut slot) = self.dream_runtime.lock() {
            *slot = runtime;
        }
    }

    /// The dedicated memory model, or the current chat model when none is
    /// configured. Shared by recall, association, and Dream so all three agree
    /// on which model memory work uses.
    pub(crate) fn effective_memory_model(&self) -> Option<crate::tools::memory::MemoryModel> {
        if !self.config.memory.enabled {
            return None;
        }
        let target = self
            .config
            .memory_model
            .as_deref()
            .unwrap_or(&self.agent_loop.session.current_model);
        self.provider_manager
            .resolve(target)
            .map(|(client, model)| crate::tools::memory::MemoryModel {
                client: client.clone(),
                model,
            })
    }

    fn start_auto_dream(&mut self) {
        let Ok(manager) = crate::memory::MemoryManager::for_current_dir() else {
            return;
        };
        self.refresh_dream_runtime();
        self.dream_worker = Some(crate::memory::dream::DreamWorker::start(
            manager,
            self.dream_runtime.clone(),
            self.dream_active.clone(),
        ));
    }

    /// Queue the current session's user/assistant prose for background
    /// consolidation. Called when a session ends (quit or `/new`), while its
    /// transcript is still in the conversation.
    pub(crate) fn queue_current_session_for_dream(&self) {
        if !self.config.memory.enabled || !self.config.memory.dream_enabled {
            return;
        }
        let transcript = dream_transcript(
            &self
                .agent_loop
                .session
                .conversation
                .lock()
                .unwrap()
                .items
                .clone(),
        );
        if transcript.trim().is_empty() {
            return;
        }
        if let Ok(manager) = crate::memory::MemoryManager::for_current_dir() {
            let _ = manager.enqueue_dream(&self.agent_loop.session.uuid.to_string(), &transcript);
        }
    }

    pub(crate) fn connect_task_events(&mut self) {
        self.ui.conversation_panel.tasks = self.agent_loop.session.tasks.clone();
        self.task_event_generation += 1;
        let generation = self.task_event_generation;
        let (task_event_tx, mut task_event_rx) = tokio::sync::mpsc::unbounded_channel();
        self.agent_loop
            .session
            .tasks
            .install_event_sink(task_event_tx);
        let app_event_tx = self.events.sender.clone();
        self.agent_loop
            .background_handles
            .push(tokio::spawn(async move {
                while let Some(mut event) = task_event_rx.recv().await {
                    event.generation = generation;
                    if app_event_tx
                        .send(Event::App(crate::ui::event::AppEvent::TaskStateChanged(
                            event,
                        )))
                        .is_err()
                    {
                        break;
                    }
                }
            }));
    }

    /// Stop producers before saving or replacing their session-owned state.
    /// Approval receivers must be released before awaiting workers: the event
    /// loop cannot service prompts while this barrier is running.
    pub(crate) fn session_accepts_work(&self) -> bool {
        self.agent_loop.session.lifecycle == SessionLifecycle::Running
    }

    pub(crate) fn require_running_session(&mut self) -> bool {
        if self.session_accepts_work() {
            return true;
        }
        self.agent_loop.session.conversation.lock().unwrap().add_warning_string(
        "Session is stopping; use /new to retry closing it, or quit. No new work can start.",
        );
        self.ui.conversation_panel.scroll_to_bottom();
        false
    }

    pub(crate) async fn close_session(&mut self) -> Result<(), String> {
        // Seal admission before the first await. Failed barriers/saves stay sealed.
        self.agent_loop.session.lifecycle = SessionLifecycle::Stopping;
        self.agent_loop.cancel.cancel_current();
        commands::invalidate_auto_compaction(self);
        self.agent_loop.pending_review = None;
        self.agent_loop.review_queue.clear();
        self.ui.question_panel = None;
        self.ui.runner_prompts.clear();
        self.agent_loop.peer_consent = None;
        self.agent_loop.peer_delegations.clear();
        self.agent_loop.peer_inbox.close().await;
        if let Some(cancel) = self.ui.input_suggestion_cancel.take() {
            cancel.cancel();
        }
        // These jobs only forward events or produce model-derived metadata.
        // They have no tool side effects and must not outlive their session.
        for handle in &self.agent_loop.background_handles {
            handle.abort();
        }
        for handle in self.agent_loop.background_handles.drain(..) {
            let _ = handle.await;
        }
        let timeout = std::time::Duration::from_secs(10);
        let (agents, tasks) = tokio::join!(
            tokio::time::timeout(timeout, self.agent_loop.session.agents.shutdown()),
            self.agent_loop.session.tasks.shutdown(timeout),
        );
        let mut errors = Vec::new();
        match agents {
            Ok(Ok(())) => {}
            Ok(Err(error)) => errors.push(error),
            Err(_) => errors.push("sub-agent shutdown timed out; workers are still pending".into()),
        }
        if let Err(error) = tasks {
            errors.push(error);
        }
        // Keep timed-out handles owned so a later close can retry. Aborting a
        // runner here would disguise detached tool work as a completed barrier.
        while let Some(handle) = self.agent_loop.runner_handles.last_mut() {
            match tokio::time::timeout(timeout, handle).await {
                Ok(result) => {
                    self.agent_loop.runner_handles.pop();
                    if let Err(error) = result {
                        errors.push(format!("runner join failed: {error}"));
                    }
                }
                Err(_) => {
                    errors.push("runner shutdown timed out; session was not replaced".into());
                    break;
                }
            }
        }
        if !errors.is_empty() {
            return Err(errors.join("; "));
        }
        if let Some(operation) = self.agent_loop.cancel.active_id {
            self.agent_loop.cancel.finish(operation);
        }
        self.agent_loop.runner = None;
        self.agent_loop.waiting_for_subagents = false;
        self.ui.conversation_panel.abort_receiving();
        self.agent_loop.phase =
            crate::ui::components::conversation_panel::conversation_panel::ActivePhase::None;
        self.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .flush_usage();
        Ok(())
    }

    pub fn quit(&mut self) {
        // Both explicit quit and event-loop errors share the awaited run epilogue.
        self.running = false;
    }
}

pub(crate) fn dream_transcript(items: &[crate::response::message_item::MessageItem]) -> String {
    use async_openai::types::responses::{
        InputContent, InputItem, InputRole, Item, MessageItem as ApiMessageItem, OutputItem,
        OutputMessageContent,
    };

    let mut lines = Vec::new();
    for item in items {
        match item {
            crate::response::message_item::MessageItem::Input(InputItem::Item(Item::Message(
                ApiMessageItem::Input(message),
            ))) if message.role == InputRole::User => {
                for content in &message.content {
                    if let InputContent::InputText(text) = content {
                        lines.push(format!("User: {}", text.text));
                    }
                }
            }
            crate::response::message_item::MessageItem::Output(OutputItem::Message(message)) => {
                for content in &message.content {
                    if let OutputMessageContent::OutputText(text) = content {
                        lines.push(format!("Assistant: {}", text.text));
                    }
                }
            }
            _ => {}
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::{
        TaskNotificationState, dream_transcript, event_requests_immediate_redraw, is_left_drag,
    };
    use crate::ui::event::{AppEvent, Event};
    use crossterm::event::{
        Event as CrosstermEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    #[test]
    fn dream_transcript_keeps_only_user_and_assistant_prose() {
        use async_openai::types::responses::AssistantRole;
        use async_openai::types::responses::{
            FunctionCallOutput, FunctionCallOutputItemParam, InputContent, InputItem, InputMessage,
            InputRole, InputTextContent, Item, MessageItem as ApiMessageItem, OutputItem,
            OutputMessage, OutputMessageContent, OutputStatus, OutputTextContent,
        };
        let user = |text: &str| {
            crate::response::message_item::MessageItem::Input(InputItem::Item(Item::Message(
                ApiMessageItem::Input(InputMessage {
                    role: InputRole::User,
                    content: vec![InputContent::InputText(InputTextContent {
                        text: text.to_string(),
                    })],
                    ..Default::default()
                }),
            )))
        };
        let developer = |text: &str| {
            crate::response::message_item::MessageItem::Input(InputItem::Item(Item::Message(
                ApiMessageItem::Input(InputMessage {
                    role: InputRole::Developer,
                    content: vec![InputContent::InputText(InputTextContent {
                        text: text.to_string(),
                    })],
                    ..Default::default()
                }),
            )))
        };
        let assistant = |text: &str| {
            crate::response::message_item::MessageItem::Output(OutputItem::Message(OutputMessage {
                content: vec![OutputMessageContent::OutputText(OutputTextContent {
                    text: text.to_string(),
                    annotations: Vec::new(),
                    logprobs: None,
                })],
                id: "msg_1".to_string(),
                role: AssistantRole::Assistant,
                phase: None,
                status: OutputStatus::Completed,
            }))
        };

        let items = vec![
            user("add a dream worker"),
            assistant("done"),
            // Tool traffic and Programmer's own injected memory block are noisy
            // and must never be consolidated into long-term memory.
            developer("<!-- programmer-associated-memory: ... -->"),
            crate::response::message_item::MessageItem::ToolOutput {
                output: FunctionCallOutputItemParam {
                    call_id: "call_1".to_string(),
                    id: None,
                    status: None,
                    output: FunctionCallOutput::Text("exit code 1".to_string()),
                },
                failed: true,
                approval_label: None,
            },
            crate::response::message_item::MessageItem::Info("startup".to_string()),
            user("now run the tests"),
        ];

        assert_eq!(
            dream_transcript(&items),
            "User: add a dream worker\nAssistant: done\nUser: now run the tests"
        );
        assert_eq!(dream_transcript(&[]), "");
    }

    #[test]
    fn redraw_policy_keeps_ticks_and_interaction_immediate_but_throttles_chunks() {
        assert!(event_requests_immediate_redraw(&Event::Tick));
        assert!(event_requests_immediate_redraw(&Event::App(AppEvent::Quit)));
        assert!(!event_requests_immediate_redraw(&mouse_event(
            MouseEventKind::Moved
        )));
    }

    #[test]
    fn left_drag_events_can_be_coalesced() {
        assert!(is_left_drag(&mouse_event(MouseEventKind::Drag(
            MouseButton::Left
        ))));
        assert!(!is_left_drag(&mouse_event(MouseEventKind::Drag(
            MouseButton::Right
        ))));
        assert!(!is_left_drag(&mouse_event(MouseEventKind::Up(
            MouseButton::Left
        ))));
    }

    fn mouse_event(kind: MouseEventKind) -> Event {
        Event::Crossterm(CrosstermEvent::Mouse(MouseEvent {
            kind,
            column: 1,
            row: 1,
            modifiers: KeyModifiers::NONE,
        }))
    }

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
                crate::app::session::SessionSeed::Fresh {
                    uuid: "startup-test".to_string(),
                },
                None,
                Vec::new(),
                false,
                "test-project".to_string(),
            ),
        )
        .await
        .expect("App::new must not wait for an MCP process or handshake");

        assert!(app.mcp_runtime.connections().is_none());
        assert_eq!(app.mcp_runtime.statuses().len(), 1);
        assert_eq!(app.mcp_runtime.statuses()[0].name, "slow");
        assert!(matches!(
            app.mcp_runtime.statuses()[0].state,
            crate::mcp::McpConnectionState::Connecting
        ));
    }
}
