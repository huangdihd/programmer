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

pub(crate) mod commands;
pub(crate) mod diagnostics;
pub(crate) mod events;
pub(crate) mod helpers;
pub(crate) mod session;
pub(crate) mod stream;
pub(crate) mod tools;

use crate::cancel::CancellationToken;
use crate::classifier::WorkMode;
use crate::config::programmer_config::ProgrammerConfig;
use crate::providers::ProviderManager;
use crate::response::message_item::MessageItem;
use crate::session::SessionManager;
use crate::ui::components::conversation_panel::conversation_panel::ConversationPanel;
use crate::ui::components::footer::footer::Footer;
use crate::ui::components::input_panel::input_panel::InputPanel;
use crate::ui::components::provider_panel::ProviderPanel;
use crate::ui::components::sidebar::Sidebar;
use crate::ui::components::skills_panel::SkillsPanel;
use crate::ui::components::mcp_panel::McpPanel;
use crate::ui::components::question_panel::QuestionPanel;
use crate::ui::components::todo_panel::TodoPanel;
use crate::ui::event::{Event, EventHandler};
use async_openai::error::OpenAIError;
use async_openai::types::responses::{
    FunctionToolCall, ResponseStreamEvent,
};
use crossterm::event::KeyEvent;
use ratatui::DefaultTerminal;
use ratatui::layout::Rect;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// Pending tool-call approvals in Manual mode and the state of the in-progress
/// approval UI.
#[derive(Default)]
pub(crate) struct ApprovalState {
    /// Pending tool-call approvals: (call, reason).
    pub(crate) queue: Vec<(FunctionToolCall, String)>,
    /// Calls the user has approved so far (waiting for all to be decided).
    pub(crate) approved: Vec<FunctionToolCall>,
    /// Which option is highlighted in the approval UI
    /// (0=approve, 1=deny, 2=approve all, 3=deny all).
    pub(crate) selected: usize,
}

/// Diagnostics baseline and the edit-turn counter behind PROGRAMMER.md reminders.
pub(crate) struct DiagnosticsState {
    /// The last full diagnostics snapshot, used to diff after each edit so the
    /// model is told which problems it introduced vs. resolved.
    pub(crate) baseline: Option<Vec<crate::diagnostics::Diagnostic>>,
    /// Count of turns that edited files, driving the periodic reminder to keep
    /// `PROGRAMMER.md` up to date.
    pub(crate) mutating_turns: usize,
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
    /// True while the stream task is backing off between connection retries.
    pub(crate) stream_retrying: Arc<AtomicBool>,
}

/// Session identity, persistence handle, and the deferred-save dirty flag.
pub(crate) struct SessionState {
    /// Session UUID.
    pub(crate) uuid: String,
    /// Session manager for persistence.
    pub(crate) mgr: Option<SessionManager>,
    /// Set when session state changed and needs persisting. The actual disk
    /// write is deferred to the next idle tick (see [`session::flush_if_dirty`])
    /// so a burst of changes within a turn collapses into a single save at turn
    /// end instead of writing after every event.
    pub(crate) dirty: bool,
}

/// Application.
pub struct App<'a> {
    /// Is the application running?
    pub running: bool,
    /// Multi-provider manager (replaces the single OpenAI client).
    pub provider_manager: ProviderManager,
    /// Currently active model in `provider/model` format.
    pub current_model: String,
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
    /// Modal question panel shown when the model calls `ask_user`.
    pub question_panel: Option<QuestionPanel>,
    /// Todo-list panel shown with `/todo`.
    pub todo_panel: Option<TodoPanel>,
    /// Full-screen interactive terminal panel, when open (`/terminal`).
    pub terminal_pane: Option<crate::ui::components::terminal_panel::TerminalPane>,
    /// Right-hand sidebar panel (toggled with Ctrl+B).
    pub sidebar: Option<Sidebar>,
    /// The sidebar's screen area from the last render, used to route mouse
    /// scroll events to the correct panel.
    pub sidebar_area: Option<Rect>,
    /// In-memory todo list synced with the global todos file and the session.
    pub todo_list: crate::todos::TodoList,
    /// Loaded agent skills, with activation state.
    pub(crate) skill_registry: crate::skills::SkillRegistry,
    /// MCP server manager (None if no servers configured).
    pub(crate) mcp_manager: Option<Arc<crate::mcp::McpManager>>,
    /// Current safety/work mode.
    pub work_mode: WorkMode,
    /// Manual-mode tool-call approval queue and UI state.
    pub(crate) approval: ApprovalState,
    /// Classifier models discovered not to support logprobs, so Auto mode skips
    /// the single-token fast path and goes straight to the merged reasoned call.
    pub(crate) classifier_no_logprobs: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    /// Diagnostics baseline and edit-turn bookkeeping.
    pub(crate) diag: DiagnosticsState,
    /// Tracks whether the current mouse-drag started in the sidebar area.
    pub(crate) sidebar_click_active: bool,
    /// Cancellation tokens for the current request lifecycle.
    pub(crate) cancel: CancelState,
    /// Session identity, persistence handle, and deferred-save flag.
    pub(crate) session: SessionState,
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
    ) -> Self {
        let provider_manager = ProviderManager::new(&config).await;
        let mut current_model = provider_manager.default_model();
        let mut work_mode = WorkMode::default();

        let mut saved_activated_skills: Vec<String> = Vec::new();
        if let Some(mgr) = &session_mgr
            && let Some(saved) = mgr.load(&session_uuid) {
                if let Some(wm) = saved.work_mode {
                    work_mode = wm;
                }
                if let Some(model) = saved.current_model
                    && provider_manager.resolve(&model).is_some() {
                        current_model = model;
                    }
                if saved.classifier_model.is_some() {
                    config.classifier_model = saved.classifier_model;
                }
                saved_activated_skills = saved.activated_skills;
            }
        let mut conversation_panel = ConversationPanel::new();
        conversation_panel.restore_items(saved_items);
        for msg in startup_messages {
            conversation_panel.add_info_string(msg);
        }
        for msg in &provider_manager.startup_errors {
            conversation_panel.add_error_string(msg.clone());
        }
        if config.providers.is_empty() {
            conversation_panel.add_warning_string(
                "no providers configured — press / then type 'providers manage' to add one, \
                 or restart with the --providers flag",
            );
        }
        let mut input_panel = InputPanel::new();
        input_panel.history = saved_history;
        let mut app = Self {
            running: true,
            provider_manager,
            current_model,
            events: EventHandler::new(),
            config,
            input_panel,
            conversation_panel,
            footer: Footer::new(),
            provider_panel: open_provider_panel.then(ProviderPanel::new),
            skills_panel: None,
            mcp_panel: None,
            question_panel: None,
            todo_panel: None,
            terminal_pane: None,
            sidebar: Some(Sidebar::new()),
            sidebar_area: None,
            todo_list: {
                let list = crate::todos::TodoList {
                    todos: saved_todos,
                };
                let _ = list.save_to_file();
                list
            },
            work_mode,
            approval: ApprovalState::default(),
            classifier_no_logprobs: Arc::new(std::sync::Mutex::new(
                std::collections::HashSet::new(),
            )),
            diag: DiagnosticsState {
                baseline: None,
                mutating_turns: 0,
                lsp_configured: helpers::lsp_checker_configured(),
            },
            sidebar_click_active: false,
            cancel: CancelState {
                active: CancellationToken::new(),
                stream_retrying: Arc::new(AtomicBool::new(false)),
            },
            session: SessionState {
                uuid: session_uuid,
                mgr: session_mgr,
                dirty: false,
            },
            skill_registry: crate::skills::SkillRegistry::load(),
            mcp_manager: None,
            plan_phase: crate::classifier::PlanPhase::default(),
            plan_review_selected: 0,
        };

        if !saved_activated_skills.is_empty() {
            app.skill_registry
                .set_activated(&saved_activated_skills);
        }

        if !app.config.mcp_servers.is_empty() {
            let mcp = crate::mcp::McpManager::from_config(&app.config.mcp_servers, ".").await;
            for err in &mcp.startup_errors {
                app.conversation_panel.add_error_string(err.clone());
            }
            if mcp.is_connected() {
                app.conversation_panel.add_info_string(format!(
                    "MCP: connected {} server(s), {} tool(s) available",
                    mcp.server_count(),
                    mcp.all_tools().len(),
                ));
            }
            app.mcp_manager = Some(Arc::new(mcp));
        }

        app
    }

    /// Run the application's main loop. Returns the final session UUID.
    pub(crate) async fn run(
        mut self,
        mut terminal: DefaultTerminal,
    ) -> (color_eyre::Result<()>, String) {
        // Kick off diagnostics baseline seeding on startup.
        crate::app::diagnostics::maybe_seed_diagnostics_baseline(&mut self);

        let result = async {
            while self.running {
                terminal.draw(|frame| frame.render_widget(&mut self, frame.area()))?;

                let event = self.events.next().await?;
                self.handle_event(event).await?;
                while self.running {
                    match self.events.try_next() {
                        Some(event) => self.handle_event(event).await?,
                        None => break,
                    }
                }
            }
            Ok(())
        }
        .await;
        crate::diagnostics::shutdown_lsp().await;
        let uuid = self.session.uuid.clone();
        (result, uuid)
    }

    // ---------------------------------------------------------------
    // Delegating methods — implementation lives in submodules
    // ---------------------------------------------------------------

    async fn handle_event(&mut self, event: Event) -> color_eyre::Result<()> {
        events::handle_event(self, event).await
    }

    pub async fn handle_chunk_events(&mut self, response_stream_event: ResponseStreamEvent) {
        stream::handle_chunk_events(self, response_stream_event).await
    }

    pub async fn handle_error_events(&mut self, error: OpenAIError) {
        stream::handle_error_events(self, error).await
    }

    pub async fn handle_key_events(&mut self, key_event: KeyEvent) -> color_eyre::Result<()> {
        events::handle_key_events(self, key_event).await
    }

    pub fn tick(&mut self) {
        events::tick(self)
    }

    pub fn quit(&mut self) {
        session::save_session(self);
        self.running = false;
    }
}
