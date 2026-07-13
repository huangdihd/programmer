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

use crate::classifier::WorkMode;
use crate::commands::{Command, CompletionEngine};
use crate::config::programmer_config::ProgrammerConfig;
use crate::providers::ProviderManager;
use crate::response::message_item::MessageItem;
use crate::response::partial_response::PartialResponse;
use crate::session::{SessionManager};
use crate::ui::components::conversation_panel::conversation_panel::{
    ActivePhase, ConversationPanel, SelectionEnd,
};
use crate::ui::components::footer::footer::Footer;
use crate::ui::components::input_panel::input_panel::InputPanel;
use crate::ui::components::provider_panel::{PanelAction, ProviderPanel};
use crate::ui::components::skills_panel::SkillsPanel;
use crate::ui::components::mcp_panel::McpPanel;
use crate::ui::components::question_panel::QuestionPanel;
use crate::ui::components::todo_panel::TodoPanel;
use crate::ui::event::{AppEvent, Event, EventHandler};
use async_openai::error::OpenAIError;
use async_openai::types::responses::{
    CreateResponse, FunctionCallOutput, FunctionCallOutputItemParam, FunctionToolCall,
    InputContent, InputItem, InputMessage, InputRole, InputTextContent, Item,
    MessageItem as ApiMessageItem, OutputItem, OutputStatus,
    ResponseStreamEvent,
};
use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use futures::StreamExt;
use ratatui::DefaultTerminal;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Maximum number of Auto-mode classifier LLM requests in flight at once.
/// Mutating calls within a single turn are classified concurrently up to this
/// bound; the rest queue and start as slots free up.
const MAX_CONCURRENT_CLASSIFICATIONS: usize = 4;

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
    /// In-memory todo list synced with the global todos file and the session.
    pub todo_list: crate::todos::TodoList,
    /// Loaded agent skills, with activation state.
    pub(crate) skill_registry: crate::skills::SkillRegistry,
    /// MCP server manager (None if no servers configured).
    pub(crate) mcp_manager: Option<std::sync::Arc<crate::mcp::McpManager>>,
    /// Current safety/work mode.
    pub work_mode: WorkMode,
    /// Pending tool-call approvals in Manual mode: (call, reason).
    /// Processed one-at-a-time; approved calls are collected and run afterwards.
    pub(crate) approval_queue: Vec<(FunctionToolCall, String)>,
    /// Calls the user has approved so far (waiting for all to be decided).
    pub(crate) approved_calls: Vec<FunctionToolCall>,
    /// Which option is highlighted in the approval UI (0=approve,1=deny,2=approve all,3=deny all).
    pub(crate) approval_selected: usize,
    /// Classifier models discovered not to support logprobs, so Auto mode skips
    /// the single-token fast path and goes straight to the merged reasoned call.
    classifier_no_logprobs: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    /// The last full diagnostics snapshot, used to diff after each edit so the
    /// model is told which problems it introduced vs. resolved. `None` until the
    /// first run of the session (which just establishes the baseline).
    diagnostics_baseline: Option<Vec<crate::diagnostics::Diagnostic>>,
    /// Count of turns that edited files, driving the periodic reminder to keep
    /// `PROGRAMMER.md` up to date.
    mutating_turns: usize,
    /// Whether the project's diagnostics profile declares an LSP checker, so the
    /// footer shows the LSP block from startup (before any server has started).
    pub(crate) lsp_configured: bool,
    /// True while the stream task is backing off between connection retries, so
    /// the status bar can show "Retrying" instead of "Connecting".
    pub(crate) stream_retrying: Arc<AtomicBool>,
    /// Session UUID.
    session_uuid: String,
    /// Session manager for persistence.
    session_mgr: Option<SessionManager>,
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

        // Restore per-session settings (work mode, chat model, classifier model)
        // so resuming a session comes back the way you left it. Unknown models
        // (e.g. a provider that was since removed) fall back to the default.
        let mut saved_activated_skills: Vec<String> = Vec::new();
        if let Some(mgr) = &session_mgr {
            if let Some(saved) = mgr.load(&session_uuid) {
                if let Some(wm) = saved.work_mode {
                    work_mode = wm;
                }
                if let Some(model) = saved.current_model {
                    if provider_manager.resolve(&model).is_some() {
                        current_model = model;
                    }
                }
                if saved.classifier_model.is_some() {
                    config.classifier_model = saved.classifier_model;
                }
                saved_activated_skills = saved.activated_skills;
            }
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
            todo_list: {
                let list = crate::todos::TodoList { todos: saved_todos };
                let _ = list.save_to_file();
                list
            },
            work_mode,
            approval_queue: Vec::new(),
            approved_calls: Vec::new(),
            approval_selected: 0,
            classifier_no_logprobs: Arc::new(std::sync::Mutex::new(
                std::collections::HashSet::new(),
            )),
            diagnostics_baseline: None,
            mutating_turns: 0,
            lsp_configured: lsp_checker_configured(),
            stream_retrying: Arc::new(AtomicBool::new(false)),
            session_uuid,
            skill_registry: crate::skills::SkillRegistry::load(),
            mcp_manager: None, // populated below after async init
            session_mgr,
        };

        // Restore activated skills from the saved session.
        if !saved_activated_skills.is_empty() {
            app.skill_registry
                .set_activated(&saved_activated_skills);
        }

        // Bring up configured MCP servers (spawn, handshake, discover tools).
        // Skipped entirely when none are configured, so the common case pays
        // nothing. Failures are surfaced in the conversation, not fatal.
        if !app.config.mcp_servers.is_empty() {
            let mcp = crate::mcp::McpManager::from_config(&app.config.mcp_servers).await;
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
            app.mcp_manager = Some(std::sync::Arc::new(mcp));
        }

        app
    }

    /// Run the application's main loop. Returns the final session UUID.
    pub(crate) async fn run(mut self, mut terminal: DefaultTerminal) -> (color_eyre::Result<()>, String) {
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
        // Tear down any warm LSP servers started during the session.
        crate::diagnostics::shutdown_lsp().await;
        let uuid = self.session_uuid.clone();
        (result, uuid)
    }

    async fn handle_event(&mut self, event: Event) -> color_eyre::Result<()> {
        match event {
            Event::Tick => self.tick(),
            Event::Crossterm(event) => match event {
                crossterm::event::Event::Key(key_event)
                    if key_event.kind == KeyEventKind::Press =>
                {
                    self.handle_key_events(key_event)?
                }
                crossterm::event::Event::Paste(data) => self.handle_paste(data),
                // The provider panel is modal; ignore mouse interaction with
                // the conversation behind it.
                crossterm::event::Event::Mouse(_) if self.provider_panel.is_some() => {}
                crossterm::event::Event::Mouse(mouse) => match mouse.kind {
                    MouseEventKind::ScrollDown => self.conversation_panel.scroll_down(),
                    MouseEventKind::ScrollUp => self.conversation_panel.scroll_up(),
                    MouseEventKind::Down(MouseButton::Left) => self
                        .conversation_panel
                        .selection_begin(mouse.column, mouse.row),
                    MouseEventKind::Drag(MouseButton::Left) => self
                        .conversation_panel
                        .selection_drag(mouse.column, mouse.row),
                    MouseEventKind::Up(MouseButton::Left) => {
                        match self.conversation_panel.selection_end(mouse.column, mouse.row) {
                            SelectionEnd::Click => self
                                .conversation_panel
                                .handle_click(mouse.column, mouse.row),
                            SelectionEnd::Copied(text) => {
                                if !crate::clipboard::copy(&text) {
                                    self.conversation_panel.add_error_string(
                                        "failed to copy selection to clipboard",
                                    );
                                    self.save_session();
                                }
                            }
                            SelectionEnd::Ignored => {}
                        }
                    }
                    _ => {}
                },
                _ => {}
            },
            Event::App(app_event) => match app_event {
                AppEvent::ChunkReceived(chunk) => self.handle_chunk_events(chunk).await,
                AppEvent::OpenAIErrorReceived(error) => {
                    self.handle_error_events(error).await;
                    self.save_session();
                }
                AppEvent::ResponseFinished(partial_response) => {
                    if partial_response.cancelled.load(Ordering::Relaxed) {
                        return Ok(());
                    }
                    let cancel_token = partial_response.cancelled.clone();
                    let calls = function_calls(&partial_response);
                    if calls.is_empty() {
                        self.conversation_panel.phase = ActivePhase::None;
                        self.conversation_panel.flush_usage();
                        if let Some(pending_request) =
                            self.conversation_panel.pending_message.take()
                        {
                            self.start_request(pending_request).await
                        }
                        self.save_session();
                    } else {
                        self.run_tool_calls(calls, cancel_token);
                    }
                }
                AppEvent::ToolCallsCompleted(outputs, cancel_token) => {
                    if cancel_token.load(Ordering::Relaxed) {
                        return Ok(());
                    }
                    self.conversation_panel.phase = ActivePhase::None;
                    // A tool (e.g. /init, configure_diagnostics) may have written
                    // the profile — refresh whether LSP is configured.
                    self.lsp_configured = lsp_checker_configured();
                    // Did this batch edit any files? (Checked before the outputs
                    // are consumed, using the call_id → tool-name map.)
                    let edited_files = self.batch_edited_files(&outputs);
                    for output in outputs {
                        self.conversation_panel.add_tool_output(output);
                    }
                    // Refresh todo_list from the global file — the todo tool
                    // may have mutated it during this batch.
                    self.todo_list = crate::todos::TodoList::load();
                    self.save_session();
                    // A spawned shell can reset the console's input mode; re-assert
                    // mouse capture so scrolling/clicks keep working afterwards.
                    let _ = crossterm::execute!(
                        std::io::stdout(),
                        crossterm::event::EnableMouseCapture
                    );
                    // After an edit, run diagnostics (and maybe a PROGRAMMER.md
                    // reminder) before resuming; that path continues the turn
                    // itself. Otherwise resume immediately.
                    if !self.continue_with_diagnostics(edited_files, cancel_token.clone()) {
                        self.spawn_stream();
                    }
                }
                AppEvent::DiagnosticsCompleted {
                    snapshot,
                    reminder_due,
                    seed,
                    cancel_token,
                } => {
                    if cancel_token.load(Ordering::Relaxed) {
                        return Ok(());
                    }
                    if seed {
                        // Background baseline seed: just record it. Don't touch
                        // the phase, inject anything, or continue a turn.
                        if self.diagnostics_baseline.is_none() {
                            self.diagnostics_baseline = Some(snapshot.diagnostics);
                        }
                        return Ok(());
                    }
                    self.conversation_panel.phase = ActivePhase::None;
                    self.apply_diagnostics(snapshot, reminder_due);
                    self.save_session();
                    // Resume the turn so the model sees the feedback (if any).
                    self.spawn_stream();
                }
                AppEvent::ClassificationCompleted {
                    allowed,
                    denied,
                    cancel_token,
                } => {
                    if cancel_token.load(Ordering::Relaxed) {
                        return Ok(());
                    }
                    if allowed.is_empty() {
                        // Everything was denied: feed the denial outputs back and
                        // resume the turn so the agent can adjust.
                        self.conversation_panel.phase = ActivePhase::None;
                        for output in denied {
                            self.conversation_panel.add_tool_output(output);
                        }
                        self.save_session();
                        self.spawn_stream();
                    } else {
                        // Denials ride along and are added once by ToolCallsCompleted.
                        self.spawn_run(allowed, denied, cancel_token, "Auto".to_string());
                    }
                }
                AppEvent::Cancel => {
                    if let Some(partial) = &self.conversation_panel.receiving_response {
                        partial.cancelled.store(true, Ordering::Relaxed);
                    }
                    self.conversation_panel.abort_receiving();
                    self.conversation_panel.phase = ActivePhase::None;
                    self.conversation_panel.flush_usage();
                    self.conversation_panel
                        .add_info_string("Request cancelled by user.".to_string());
                    self.save_session();
                    if let Some(pending_request) = self.conversation_panel.pending_message.take() {
                        self.start_request(pending_request).await;
                    }
                }
                AppEvent::Quit => self.quit(),
                AppEvent::Start => self.send_message().await,
                AppEvent::StartInit => {
                    self.start_request_as(init_prompt(), InputRole::Developer).await;
                }
                AppEvent::ProvidersChanged => {
                    // Rebuild the manager so new/edited providers get clients
                    // and model discovery (bounded by the fetch timeout).
                    self.provider_manager = ProviderManager::new(&self.config).await;
                    for msg in &self.provider_manager.startup_errors {
                        self.conversation_panel.add_error_string(msg.clone());
                    }
                    // The current model may point at a removed/renamed provider.
                    if self.provider_manager.resolve(&self.current_model).is_none() {
                        self.current_model = self.provider_manager.default_model();
                        self.conversation_panel.add_info_string(format!(
                            "current model reset to: {}",
                            self.current_model
                        ));
                    }
                }
                AppEvent::McpChanged => {
                    // Re-spawn the MCP manager from the edited config. Tearing
                    // down the old one drops its child processes on drop.
                    if self.config.mcp_servers.is_empty() {
                        self.mcp_manager = None;
                        self.conversation_panel
                            .add_info_string("MCP servers cleared.".to_string());
                    } else {
                        let mcp = crate::mcp::McpManager::from_config(
                            &self.config.mcp_servers,
                        )
                        .await;
                        for err in &mcp.startup_errors {
                            self.conversation_panel.add_error_string(err.clone());
                        }
                        self.conversation_panel.add_info_string(format!(
                            "MCP reloaded: {} server(s), {} tool(s) available",
                            mcp.server_count(),
                            mcp.all_tools().len(),
                        ));
                        self.mcp_manager = Some(std::sync::Arc::new(mcp));
                    }
                }
                AppEvent::QuestionPrompt {
                    question,
                    answer_tx,
                } => {
                    self.question_panel =
                        Some(QuestionPanel::new(question, answer_tx));
                }
            },
        }
        Ok(())
    }

    /// Handle approval keys when tool calls are queued in Manual mode.
    fn handle_approval_key(&mut self, key_event: KeyEvent) -> color_eyre::Result<()> {
        match key_event.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.approval_selected = self.approval_selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                if self.approval_selected + 1 < 4 {
                    self.approval_selected += 1;
                }
            }
            KeyCode::Enter => match self.approval_selected {
                0 => {
                    if let Some((call, _)) = self.approval_queue.drain(..1).next() {
                        self.approved_calls.push(call);
                    }
                    self.approval_selected = 0;
                    self.check_approval_done();
                }
                1 => {
                    let denied = self.approval_queue.drain(..1).next();
                    if let Some((call, reason)) = denied {
                        self.deny_single_call(call, reason);
                    }
                    self.approval_selected = 0;
                    self.check_approval_done();
                }
                2 => {
                    let approved: Vec<FunctionToolCall> =
                        self.approval_queue.drain(..).map(|(c, _)| c).collect();
                    self.approved_calls.extend(approved);
                    self.approval_selected = 0;
                    self.check_approval_done();
                }
                3 => {
                    let denied: Vec<_> = self.approval_queue.drain(..).collect();
                    for (call, reason) in denied {
                        self.deny_single_call(call, reason);
                    }
                    self.approval_selected = 0;
                    self.check_approval_done();
                }
                _ => {}
            },
            _ => {}
        }
        Ok(())
    }

    fn deny_single_call(&mut self, call: FunctionToolCall, reason: String) {
        self.conversation_panel.add_info_string(format!(
            "🛡 Denied: {} ({})",
            call.name, reason
        ));
        let output = FunctionCallOutputItemParam {
            call_id: call.call_id,
            output: FunctionCallOutput::Text(format!(
                "error: tool call denied by user — {} ({})",
                call.name, reason
            )),
            id: None,
            status: None,
        };
        self.conversation_panel.add_tool_output(output);
    }

    /// If the approval queue is empty, run the approved calls and continue.
    fn check_approval_done(&mut self) {
        if !self.approval_queue.is_empty() {
            return;
        }
        let calls = std::mem::take(&mut self.approved_calls);
        if calls.is_empty() {
            // All denied – tool_running was already cleared; resume the turn
            // so denied outputs get fed back to the model.
            self.spawn_stream();
            return;
        }

        // Annotate and run approved calls.
        let sender = self.events.sender.clone();
        let cancel_token = Arc::new(AtomicBool::new(false));
        let mcp = self.mcp_manager.clone();
        tokio::spawn(async move {
            let mut outputs = Vec::new();
            for call in &calls {
                let mut out = crate::tools::run_tool_call(call, &sender, mcp.as_deref()).await;
                let text = match &out.output {
                    FunctionCallOutput::Text(t) => t.clone(),
                    _ => String::new(),
                };
                out.output = FunctionCallOutput::Text(format!(
                    "[approved by user in Manual mode]\n{text}"
                ));
                outputs.push(out);
            }
            let _ = sender.send(Event::App(AppEvent::ToolCallsCompleted(
                outputs,
                cancel_token,
            )));
        });
    }

    /// Handles text pasted into the terminal (bracketed paste). Small
    /// single-line pastes go straight into the input; larger ones are collapsed
    /// into a placeholder that expands back to the full text on send.
    fn handle_paste(&mut self, data: String) {
        let data = data.replace("\r\n", "\n").replace('\r', "\n");
        // IME-committed text arrives as paste; route to question panel first.
        if let Some(panel) = self.question_panel.as_mut() {
            panel.handle_paste(&data);
            return;
        }
        if let Some(panel) = self.provider_panel.as_mut() {
            panel.handle_paste(&data);
            return;
        }
        if let Some(panel) = self.mcp_panel.as_mut() {
            panel.handle_paste(&data);
            return;
        }
        if !data.contains('\n') && data.chars().count() <= 200 {
            self.input_panel.insert_str(&data);
        } else {
            self.input_panel.add_paste(data);
        }
        self.update_completions();
    }

    async fn send_message(&mut self) {
        let text = self.input_panel.expanded_content();
        if text.is_empty() {
            return;
        }
        self.input_panel.push_history(text.clone());
        self.input_panel.clear();
        self.start_request(text).await;
    }

    async fn start_request(&mut self, text: String) {
        self.start_request_as(text, InputRole::User).await;
    }

    /// Start a turn from a message with the given role. `User` is a normal user
    /// message; `Developer` carries a hidden instruction (like `/init`) that
    /// reaches the model and the classifier but isn't drawn as a user bubble.
    async fn start_request_as(&mut self, text: String, role: InputRole) {
        if self.conversation_panel.is_busy() {
            let is_at_bottom = self.conversation_panel.is_at_bottom();
            match self.conversation_panel.pending_message.as_mut() {
                Some(pending_message) => {
                    pending_message.push('\n');
                    pending_message.push_str(&text);
                }
                None => self.conversation_panel.pending_message = Some(text),
            }
            if is_at_bottom {
                self.conversation_panel.scroll_to_bottom()
            }
            return;
        }

        let input_message = InputMessage {
            content: vec![InputContent::InputText(InputTextContent {
                text: text.clone(),
            })],
            role,
            status: Option::from(OutputStatus::Completed),
        };

        self.conversation_panel
            .add_input_message(ApiMessageItem::Input(input_message));
        self.conversation_panel.reset_accumulated_usage();
        // Kick off a baseline diagnostics run (once per session) so the first
        // edit can be diffed against the project's pre-edit state.
        self.maybe_seed_diagnostics_baseline();
        self.save_session();
        self.spawn_stream();
    }

    /// Spawns a streaming response request for the current conversation state
    /// (including tool definitions). Used both to answer a new user message and
    /// to continue the turn after tool calls have run.
    fn spawn_stream(&mut self) {
        let cancel_token = Arc::new(AtomicBool::new(false));
        self.conversation_panel.live_expanded_items.clear();
        self.conversation_panel.phase = ActivePhase::None;
        self.conversation_panel.receiving_response =
            Some(PartialResponse::new(cancel_token.clone()));
        let (client, model_name) = match self.provider_manager.resolve(&self.current_model) {
            Some((c, m)) => (c.clone(), m),
            None => {
                // Provider not found — report error and stop.
                self.conversation_panel.abort_receiving();
                self.conversation_panel
                    .add_error_string(format!("unknown provider/model: {}", self.current_model));
                return;
            }
        };
        let sender = self.events.sender.clone();
        let retrying = self.stream_retrying.clone();
        retrying.store(false, Ordering::Relaxed);
        let input_param = self.conversation_panel.get_input_param(
            &self.current_model,
            self.skill_registry.combined_prompt().as_deref(),
        );
        let mcp = self.mcp_manager.clone();
        tokio::spawn(async move {
            let mut request = CreateResponse::default();
            request.stream = Option::from(true);
            request.input = input_param;
            request.model = Option::from(model_name);
            request.tools = Some(crate::tools::tools(mcp.as_deref()));

            const MAX_RETRIES: u32 = 10;
            let mut attempt: u32 = 0;
            let stream = loop {
                match client.responses().create_stream(request.clone()).await {
                    Ok(stream) => break Ok(stream),
                    Err(e) if is_retryable(&e) && attempt < MAX_RETRIES => {
                        if cancel_token.load(Ordering::Relaxed) {
                            retrying.store(false, Ordering::Relaxed);
                            return;
                        }
                        attempt += 1;
                        retrying.store(true, Ordering::Relaxed);
                        tokio::time::sleep(backoff_delay(attempt)).await;
                    }
                    Err(e) => break Err(e),
                }
            };
            retrying.store(false, Ordering::Relaxed);
            match stream {
                Ok(mut response_stream) => {
                    while let Some(response_stream_event) = response_stream.next().await {
                        if cancel_token.load(Ordering::Relaxed) {
                            return;
                        }
                        match response_stream_event {
                            Ok(response_event) => {
                                let _ = sender
                                    .send(Event::App(AppEvent::ChunkReceived(response_event)));
                            }
                            Err(openai_error) => {
                                let _ = sender
                                    .send(Event::App(AppEvent::OpenAIErrorReceived(openai_error)));
                            }
                        }
                    }
                }
                Err(openai_error) => {
                    if !cancel_token.load(Ordering::Relaxed) {
                        let _ =
                            sender.send(Event::App(AppEvent::OpenAIErrorReceived(openai_error)));
                    }
                }
            }
        });
    }

    /// Runs the model's requested tool calls in the background, then reports the
    /// outputs back to the event loop via `ToolCallsCompleted`.
    fn run_tool_calls(&mut self, calls: Vec<FunctionToolCall>, cancel_token: Arc<AtomicBool>) {
        // Auto mode classifies each call with an async LLM call first.
        if self.work_mode.uses_llm_classifier() {
            self.spawn_auto_classification(calls, cancel_token);
            return;
        }

        self.conversation_panel.phase = ActivePhase::ToolRunning;
        let classifier = self.work_mode.classifier(self.build_mcp_policy_map());

        // Split calls: allowed (run now), denied (error now), pending (queue
        // for approval).
        let mut allowed: Vec<FunctionToolCall> = Vec::new();
        let mut queued: Vec<(FunctionToolCall, String)> = Vec::new();
        let mut denied: Vec<FunctionCallOutputItemParam> = Vec::new();

        for call in &calls {
            match classifier.classify(&call.name, &call.arguments) {
                crate::classifier::Verdict::Allow => allowed.push(call.clone()),
                crate::classifier::Verdict::Deny { reason } => {
                    denied.push(classifier_denied_output(call, &reason))
                }
                crate::classifier::Verdict::Ask { reason } => {
                    queued.push((call.clone(), reason))
                }
            }
        }

        // Calls awaiting approval: queue them and leave the busy phase so the
        // UI can show the prompt. Feed any denials back now, since they won't
        // ride along with the later approval run.
        if !queued.is_empty() {
            for output in denied {
                self.conversation_panel.add_tool_output(output);
            }
            self.approval_queue = queued;
            self.conversation_panel.phase = ActivePhase::None;
            return; // Will be restarted when the user approves.
        }

        if allowed.is_empty() && denied.is_empty() {
            self.conversation_panel.phase = ActivePhase::None;
            return;
        }

        let label = self.work_mode.label().to_string();
        self.spawn_run(allowed, denied, cancel_token, label);
    }

    /// Auto mode: classify each mutating call with the LLM, then hand the
    /// verdicts back via [`AppEvent::ClassificationCompleted`]. Read-only tools
    /// skip the classifier and are always allowed.
    ///
    /// Mutating calls are classified concurrently, capped at
    /// [`MAX_CONCURRENT_CLASSIFICATIONS`] in-flight LLM requests so a large
    /// batch doesn't fan out into an unbounded burst against the provider.
    fn spawn_auto_classification(
        &mut self,
        calls: Vec<FunctionToolCall>,
        cancel_token: Arc<AtomicBool>,
    ) {
        self.conversation_panel.phase = ActivePhase::Classifying;

        // Resolve the classifier model (falls back to the chat model).
        let model_str = self
            .config
            .classifier_model
            .clone()
            .unwrap_or_else(|| self.current_model.clone());
        let (client, model_name) = match self.provider_manager.resolve(&model_str) {
            Some((c, m)) => (c.clone(), m),
            None => {
                self.conversation_panel.add_error_string(format!(
                    "classifier model '{model_str}' not found — set a valid \
                     classifier_model (or /classifier <provider/model>)"
                ));
                self.conversation_panel.phase = ActivePhase::None;
                return;
            }
        };

        let sender = self.events.sender.clone();
        let no_lp = self.classifier_no_logprobs.clone();
        let (light_context, full_context) = self.build_classifier_context();
        let mcp_policies = self.build_mcp_policy_map();

        tokio::spawn(async move {
            // One decision per call, preserving call order. Read-only tools
            // resolve immediately; mutating tools hit the LLM. `buffered` runs
            // up to MAX_CONCURRENT_CLASSIFICATIONS futures at once and yields
            // their results in input order.
            enum Decision {
                Allow(FunctionToolCall),
                Deny(FunctionCallOutputItemParam),
            }

            let decisions: Vec<Option<Decision>> = futures::stream::iter(
                calls.into_iter().map(|call| {
                    // Re-borrow per future so `async move` captures copies of the
                    // references, leaving the owned values in this task's scope.
                    let client = &client;
                    let model_name = &model_name;
                    let no_lp = &no_lp;
                    let light_context = &light_context;
                    let full_context = &full_context;
                    let cancel_token = &cancel_token;
                    let mcp_policies = &mcp_policies;
                    async move {
                        if cancel_token.load(Ordering::Relaxed) {
                            return None;
                        }
                        // MCP server policy check: Trusted → allow,
                        // Review → fall through to LLM classifier.
                        if let Some(verdict) =
                            crate::classifier::classify_mcp_policy(&call.name, mcp_policies)
                        {
                            return Some(match verdict {
                                crate::classifier::Verdict::Allow => Decision::Allow(call),
                                crate::classifier::Verdict::Ask { reason }
                                | crate::classifier::Verdict::Deny { reason } => {
                                    Decision::Deny(classifier_denied_output(&call, &reason))
                                }
                            });
                        }
                        if !crate::classifier::needs_review(&call.name) {
                            return Some(Decision::Allow(call));
                        }

                        let try_logprobs = !no_lp.lock().unwrap().contains(model_name);
                        let outcome = crate::classifier::classify_tool_call(
                            client,
                            model_name,
                            &call.name,
                            &call.arguments,
                            light_context,
                            full_context,
                            try_logprobs,
                        )
                        .await;
                        if outcome.logprobs_missing {
                            no_lp.lock().unwrap().insert(model_name.clone());
                        }

                        Some(match outcome.verdict {
                            crate::classifier::Verdict::Allow => Decision::Allow(call),
                            crate::classifier::Verdict::Deny { reason }
                            | crate::classifier::Verdict::Ask { reason } => {
                                Decision::Deny(classifier_denied_output(&call, &reason))
                            }
                        })
                    }
                }),
            )
            .buffered(MAX_CONCURRENT_CLASSIFICATIONS)
            .collect()
            .await;

            // A cancel that landed mid-classification: drop the whole batch.
            if cancel_token.load(Ordering::Relaxed) {
                return;
            }

            let mut allowed: Vec<FunctionToolCall> = Vec::new();
            let mut denied: Vec<FunctionCallOutputItemParam> = Vec::new();
            for decision in decisions.into_iter().flatten() {
                match decision {
                    Decision::Allow(call) => allowed.push(call),
                    Decision::Deny(output) => denied.push(output),
                }
            }

            let _ = sender.send(Event::App(AppEvent::ClassificationCompleted {
                allowed,
                denied,
                cancel_token,
            }));
        });
    }

    /// Build a map of MCP server name → [`McpPolicy`] from the config, so
    /// classifiers can look up per-server policies without touching config.
    fn build_mcp_policy_map(&self) -> HashMap<String, crate::mcp::types::McpPolicy> {
        self.config
            .mcp_servers
            .iter()
            .map(|s| (s.name.clone(), s.auto_approve))
            .collect()
    }

    /// Compact context handed to the Auto-mode classifier so it judges tool
    /// calls against the actual task, not in isolation: the working directory,
    /// the user's latest request, and the recent tool calls this turn.
    ///
    /// Build two classifier contexts:
    ///   - light: cwd + user request only (fast yes/no path, ~cheap).
    ///   - full:  light + assistant replies, tool outputs, call history
    ///            (reasoned fallback, sent only when re-evaluating).
    fn build_classifier_context(&self) -> (String, String) {
        let mut light = Vec::new();
        let mut full: Vec<String> = Vec::new();

        if let Ok(cwd) = std::env::current_dir() {
            let dir = format!("Working directory: {}", cwd.display());
            light.push(dir.clone());
            full.push(dir);
        }

        let items: Vec<&MessageItem> = self.conversation_panel.items().collect();

        // The user's most recent request drives whether an action is expected.
        // Skip hidden developer messages (diagnostics feedback, the /init prompt)
        // so they aren't mistaken for the user's own words.
        if let Some(msg) = items.iter().rev().find_map(|it| match it {
            MessageItem::Input(input) if !it.is_hidden_developer() => extract_input_text(input),
            _ => None,
        }) {
            let user = format!(
                "User's latest request:\n{}",
                truncate_chars(msg.trim(), 800)
            );
            light.push(user.clone());
            full.push(user);
        }

        // ---- full context only below this line ----

        // Assistant reply metadata only (no raw text — prevents prompt
        // injection via the model's own output or tool results it echoes).
        let assistant_count = items
            .iter()
            .rev()
            .filter(|it| matches!(it, MessageItem::Output(OutputItem::Message(_))))
            .take(3)
            .count();
        if assistant_count > 0 {
            full.push(format!("Assistant has sent {assistant_count} message(s) this turn."));
        }

        // Map each call_id to its tool name so an outcome row can report the
        // tool that produced it rather than an opaque id.
        let call_names: std::collections::HashMap<&str, &str> = items
            .iter()
            .filter_map(|it| match it {
                MessageItem::Output(OutputItem::FunctionCall(fc)) => {
                    Some((fc.call_id.as_str(), fc.name.as_str()))
                }
                _ => None,
            })
            .collect();

        // Tool call outcome metadata only (name + status + size — no raw
        // output content, to prevent injection from file contents, command
        // output, or URLs embedded in tool results).
        let tool_outcomes: Vec<String> = items
            .iter()
            .rev()
            .filter_map(|it| match it {
                MessageItem::Output(OutputItem::FunctionCall(fc)) => {
                    Some(format!("  {} — pending", fc.name))
                }
                MessageItem::Input(InputItem::Item(Item::FunctionCallOutput(fco))) => {
                    let output_text = match &fco.output {
                        FunctionCallOutput::Text(t) => t.as_str(),
                        _ => "",
                    };
                    // Fall back to the call_id if the matching call isn't in view.
                    let name = call_names
                        .get(fco.call_id.as_str())
                        .copied()
                        .unwrap_or(fco.call_id.as_str());
                    let len = output_text.len();
                    let status = if output_text.is_empty() { "empty" } else { "ok" };
                    Some(format!("  {name} — {status}, {len} chars"))
                }
                _ => None,
            })
            .take(10)
            .collect();
        if !tool_outcomes.is_empty() {
            full.push(format!(
                "Tool calls this turn:\n{}",
                tool_outcomes.into_iter().rev().collect::<Vec<_>>().join("\n")
            ));
        }

        (light.join("\n\n"), full.join("\n\n"))
    }

    /// Whether any output in this batch was produced by a file-editing tool
    /// (`write_file`/`edit_file`), determined by mapping each output's call id
    /// back to the tool that produced it.
    fn batch_edited_files(&self, outputs: &[FunctionCallOutputItemParam]) -> bool {
        let names: std::collections::HashMap<&str, &str> = self
            .conversation_panel
            .items()
            .filter_map(|it| match it {
                MessageItem::Output(OutputItem::FunctionCall(fc)) => {
                    Some((fc.call_id.as_str(), fc.name.as_str()))
                }
                _ => None,
            })
            .collect();
        outputs.iter().any(|o| {
            matches!(
                names.get(o.call_id.as_str()).copied(),
                Some(crate::tools::write_file::NAME) | Some(crate::tools::edit_file::NAME)
            )
        })
    }

    /// The call id of the most recent file-editing tool output, so post-edit
    /// feedback can be attached to the edit that triggered it.
    fn last_edit_output_call_id(&self) -> Option<String> {
        let names: std::collections::HashMap<&str, &str> = self
            .conversation_panel
            .items()
            .filter_map(|it| match it {
                MessageItem::Output(OutputItem::FunctionCall(fc)) => {
                    Some((fc.call_id.as_str(), fc.name.as_str()))
                }
                _ => None,
            })
            .collect();
        self.conversation_panel
            .items()
            .filter_map(|it| match it {
                MessageItem::Input(InputItem::Item(Item::FunctionCallOutput(fco))) => {
                    let name = names.get(fco.call_id.as_str()).copied();
                    matches!(
                        name,
                        Some(crate::tools::write_file::NAME) | Some(crate::tools::edit_file::NAME)
                    )
                    .then(|| fco.call_id.clone())
                }
                _ => None,
            })
            .last()
    }

    /// Deliver post-edit feedback (a diagnostics summary and/or a reminder).
    /// Preferred placement is inside the triggering edit's tool result, so the
    /// user can expand and see it; if the edit's output can't be found, fall
    /// back to a hidden developer message. Either way the model sees it.
    fn emit_post_edit_feedback(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        if let Some(call_id) = self.last_edit_output_call_id() {
            let block = format!("\n\n--- Post-edit check ---\n{text}");
            if self.conversation_panel.append_to_tool_output(&call_id, &block) {
                return;
            }
        }
        self.conversation_panel
            .add_input_message(make_developer_message(text));
    }

    /// After a file-editing batch, run diagnostics (if configured) and count the
    /// turn toward the periodic PROGRAMMER.md reminder. Returns `true` when it
    /// spawned an async diagnostics run that will continue the turn itself via
    /// [`AppEvent::DiagnosticsCompleted`]; `false` means the caller should
    /// resume the stream normally.
    fn continue_with_diagnostics(
        &mut self,
        edited_files: bool,
        cancel_token: Arc<AtomicBool>,
    ) -> bool {
        if !edited_files {
            return false;
        }
        self.mutating_turns += 1;
        let reminder_due = self.mutating_turns % OVERVIEW_REMINDER_EVERY == 0
            && std::path::Path::new("PROGRAMMER.md").exists();

        if std::path::Path::new(crate::diagnostics::PROFILE_PATH).exists() {
            self.spawn_diagnostics(reminder_due, cancel_token);
            return true;
        }

        // No profile to run, but the overview reminder may still be due.
        if reminder_due {
            self.emit_post_edit_feedback(overview_reminder());
            self.save_session();
        }
        false
    }

    /// Forget the diagnostics baseline and edit counter — called when the
    /// conversation is cleared or a new session starts, so the next edit
    /// re-establishes a baseline instead of diffing against a stale one.
    fn reset_diagnostics_state(&mut self) {
        self.diagnostics_baseline = None;
        self.mutating_turns = 0;
    }

    /// Spawn the diagnostics checkers in the background; the result comes back as
    /// [`AppEvent::DiagnosticsCompleted`].
    fn spawn_diagnostics(&mut self, reminder_due: bool, cancel_token: Arc<AtomicBool>) {
        self.conversation_panel.phase = ActivePhase::Checking;
        let sender = self.events.sender.clone();
        tokio::spawn(async move {
            let cwd = std::env::current_dir()
                .unwrap_or_else(|_| std::path::Path::new(".").to_path_buf());
            let snapshot = crate::diagnostics::collect(&cwd).await.unwrap_or_default();
            let _ = sender.send(Event::App(AppEvent::DiagnosticsCompleted {
                snapshot,
                reminder_due,
                seed: false,
                cancel_token,
            }));
        });
    }

    /// On the first turn of a session with a diagnostics profile, run the
    /// checkers once in the background to establish a baseline — so the *first*
    /// edit already has a "before" state to diff against, instead of silently
    /// becoming the baseline itself. No-op once a baseline exists.
    fn maybe_seed_diagnostics_baseline(&mut self) {
        if self.diagnostics_baseline.is_some() {
            return;
        }
        if !std::path::Path::new(crate::diagnostics::PROFILE_PATH).exists() {
            return;
        }
        let sender = self.events.sender.clone();
        tokio::spawn(async move {
            let cwd = std::env::current_dir()
                .unwrap_or_else(|_| std::path::Path::new(".").to_path_buf());
            let snapshot = crate::diagnostics::collect(&cwd).await.unwrap_or_default();
            let _ = sender.send(Event::App(AppEvent::DiagnosticsCompleted {
                snapshot,
                reminder_due: false,
                seed: true,
                cancel_token: Arc::new(AtomicBool::new(false)),
            }));
        });
    }

    /// Fold a fresh diagnostics snapshot into the conversation: diff it against
    /// the baseline, update the baseline, and inject a hidden developer message
    /// carrying the change summary, any checker errors, and (when due) the
    /// PROGRAMMER.md reminder. Adds nothing when there's nothing to report.
    fn apply_diagnostics(&mut self, snapshot: crate::diagnostics::Snapshot, reminder_due: bool) {
        let mut parts: Vec<String> = Vec::new();

        match &self.diagnostics_baseline {
            Some(old) => {
                let d = crate::diagnostics::diff(old, &snapshot.diagnostics);
                if let Some(summary) = d.summary() {
                    parts.push(summary);
                }
            }
            None => {
                // First run of the session only establishes the baseline. We
                // have no "before" snapshot to diff against, so we can't say
                // whether this edit caused these — just report the current count
                // neutrally and attribute changes from here on.
                if !snapshot.diagnostics.is_empty() {
                    parts.push(format!(
                        "Diagnostics baseline established: {} problem(s) \
                         currently in the project. Future edits will report \
                         changes relative to this.",
                        snapshot.diagnostics.len()
                    ));
                }
            }
        }
        for e in &snapshot.errors {
            parts.push(format!("Diagnostics checker error: {e}"));
        }
        self.diagnostics_baseline = Some(snapshot.diagnostics);

        if reminder_due {
            parts.push(overview_reminder());
        }

        self.emit_post_edit_feedback(parts.join("\n\n"));
    }

    /// Run `allowed` tool calls in the background, prepend the `denied` outputs,
    /// and report everything back via [`AppEvent::ToolCallsCompleted`].
    fn spawn_run(
        &mut self,
        allowed: Vec<FunctionToolCall>,
        denied: Vec<FunctionCallOutputItemParam>,
        cancel_token: Arc<AtomicBool>,
        label: String,
    ) {
        self.conversation_panel.phase = ActivePhase::ToolRunning;
        let sender = self.events.sender.clone();
        let mcp = self.mcp_manager.clone();
        tokio::spawn(async move {
            let mut outputs = denied;
            for call in &allowed {
                if cancel_token.load(Ordering::Relaxed) {
                    break;
                }
                let mut out = crate::tools::run_tool_call(call, &sender, mcp.as_deref()).await;
                let text = match &out.output {
                    FunctionCallOutput::Text(t) => t.clone(),
                    _ => String::new(),
                };
                out.output = FunctionCallOutput::Text(format!(
                    "[auto-approved in {label} mode]\n{text}"
                ));
                outputs.push(out);
            }
            let _ = sender.send(Event::App(AppEvent::ToolCallsCompleted(
                outputs,
                cancel_token,
            )));
        });
    }

    pub async fn handle_chunk_events(&mut self, response_stream_event: ResponseStreamEvent) {
        // Ignore chunks from a cancelled stream (receiving_response was cleared).
        if self.conversation_panel.receiving_response.is_none() {
            return;
        }
        let Some(partial_response) = self
            .conversation_panel
            .handle_response_stream_event(response_stream_event)
        else {
            // Check if the stream is now generating function calls.
            if let Some(ref receiving) = self.conversation_panel.receiving_response {
                if receiving.has_function_calls() {
                    self.conversation_panel.phase = ActivePhase::CreatingToolCall;
                } else if receiving.has_message_items() {
                    self.conversation_panel.phase = ActivePhase::Outputting;
                }
            }
            return;
        };
        // Transfer live expanded state before items become historical,
        // so reasoning/tool-call items the user expanded during streaming
        // stay expanded instead of auto-collapsing.
        let base_index = self.conversation_panel.items.len();
        for &live_idx in &self.conversation_panel.live_expanded_items {
            self.conversation_panel
                .expanded_items
                .insert(base_index + live_idx);
        }
        let cancelled = partial_response.cancelled.load(Ordering::Relaxed);
        let usage = partial_response.usage;
        self.conversation_panel.items.extend(
            partial_response
                .items
                .iter()
                .flatten()
                .filter(|item| {
                    // If the request was cancelled, drop all function calls
                    // from the response so they aren't shown or executed.
                    !cancelled || !matches!(item, OutputItem::FunctionCall(_))
                })
                .map(|item| MessageItem::Output(item.clone().into())),
        );
        if let Some((input, output)) = usage {
            self.conversation_panel.add_usage(input, output);
        }
        self.events
            .send(AppEvent::ResponseFinished(partial_response));
    }

    pub async fn handle_error_events(&mut self, error: OpenAIError) {
        // If no request is in flight, ignore (e.g. from a cancelled stream).
        if !self.conversation_panel.is_busy() {
            return;
        }
        // The stream ended in an error, so the turn is over: stop "receiving",
        // record the error, and flush any queued message so we don't get stuck.
        self.conversation_panel.abort_receiving();
        self.conversation_panel.phase = ActivePhase::None;
        self.conversation_panel.flush_usage();
        self.conversation_panel.add_error(error);
        if let Some(pending_request) = self.conversation_panel.pending_message.take() {
            self.start_request(pending_request).await;
        }
    }

    /// Enter combined with any of these modifiers inserts a newline instead of sending.
    fn is_newline_modifier(modifiers: KeyModifiers) -> bool {
        modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT)
    }

    /// Handles the key events and updates the state of [`App`].
    pub fn handle_key_events(&mut self, key_event: KeyEvent) -> color_eyre::Result<()> {
        // ---- tool-call approval (Manual mode) ----
        if !self.approval_queue.is_empty() {
            return self.handle_approval_key(key_event);
        }
        // ---- question panel (shown when model calls ask_user) ----
        if let Some(panel) = self.question_panel.as_mut() {
            match panel.handle_key(key_event) {
                crate::ui::components::question_panel::AnswerAction::Answer(text) => {
                    let question = panel.question_text().to_string();
                    panel.answer(text.clone());
                    self.question_panel = None;
                    self.conversation_panel
                        .add_info_string(format!("❓ {}\n→ {}", question, text));
                }
                crate::ui::components::question_panel::AnswerAction::None => {}
            }
            return Ok(());
        }

        // ---- todo panel (shown with /todo) ----
        if let Some(panel) = self.todo_panel.as_mut() {
            match panel.handle_key(key_event) {
                crate::ui::components::todo_panel::PanelAction::Close => {
                    self.todo_panel = None;
                    // Refresh from the global file since the panel mutated it.
                    self.todo_list = crate::todos::TodoList::load();
                }
                crate::ui::components::todo_panel::PanelAction::None => {}
            }
            return Ok(());
        }

        // ---- Ctrl+T: cycle work mode ----
        if key_event.code == KeyCode::Char('t')
            && key_event.modifiers == KeyModifiers::CONTROL
        {
            self.work_mode = self.work_mode.next();
            self.persist_config();
            return Ok(());
        }

        // ---- provider management panel (modal: swallows all input) ----
        if let Some(panel) = self.provider_panel.as_mut() {
            if matches!(key_event.code, KeyCode::Char('q' | 'Q' | 'c' | 'C'))
                && key_event.modifiers == KeyModifiers::CONTROL
            {
                self.events.send(AppEvent::Quit);
                return Ok(());
            }
            match panel.handle_key(key_event, &mut self.config, &self.provider_manager) {
                PanelAction::Close => self.provider_panel = None,
                PanelAction::Saved => {
                    self.persist_config();
                    self.events.send(AppEvent::ProvidersChanged);
                }
                PanelAction::None => {}
            }
            return Ok(());
        }

        // ---- skills management panel (modal) ----
        if let Some(panel) = self.skills_panel.as_mut() {
            use crate::ui::components::skills_panel::PanelAction as SkillsAction;
            match panel.handle_key(key_event, &mut self.skill_registry) {
                SkillsAction::Close => self.skills_panel = None,
                // Persist the new active set with the session.
                SkillsAction::Saved => self.save_session(),
                SkillsAction::None => {}
            }
            return Ok(());
        }

        // ---- MCP management panel (modal) ----
        if let Some(panel) = self.mcp_panel.as_mut() {
            use crate::ui::components::mcp_panel::PanelAction as McpAction;
            if matches!(key_event.code, KeyCode::Char('q' | 'Q' | 'c' | 'C'))
                && key_event.modifiers == KeyModifiers::CONTROL
            {
                self.events.send(AppEvent::Quit);
                return Ok(());
            }
            match panel.handle_key(key_event, &mut self.config) {
                McpAction::Close => self.mcp_panel = None,
                McpAction::Saved => {
                    self.persist_config();
                    self.events.send(AppEvent::McpChanged);
                }
                McpAction::None => {}
            }
            return Ok(());
        }

        // ---- completion-popup navigation (takes priority over text input) ----
        if self
            .input_panel
            .completion
            .as_ref()
            .map_or(false, |c| c.visible)
        {
            match key_event.code {
                KeyCode::Tab => {
                    // Accept the highlighted candidate into the input; if it is
                    // already accepted, cycle to the next candidate.
                    let content = self.input_panel.get_content();
                    if let Some(c) = self.input_panel.completion.as_mut() {
                        if content == c.line(c.selected) {
                            if c.candidates.len() == 1 {
                                // Only candidate already accepted — done with the popup.
                                self.input_panel.completion = None;
                                return Ok(());
                            }
                            c.selected = (c.selected + 1) % c.candidates.len();
                        }
                        // Keep the selection inside the visible window.
                        let visible = 10usize;
                        if c.selected < c.scroll_offset {
                            c.scroll_offset = c.selected;
                        } else if c.selected >= c.scroll_offset + visible {
                            c.scroll_offset = c.selected - visible + 1;
                        }
                        let text = c.line(c.selected);
                        self.input_panel.set_content(&text);
                    }
                    return Ok(());
                }
                KeyCode::Up => {
                    if let Some(ref mut c) = self.input_panel.completion {
                        if c.selected > 0 {
                            c.selected -= 1;
                        } else {
                            // Wrap to last item; scroll so it's visible at the bottom.
                            c.selected = c.candidates.len().saturating_sub(1);
                            let visible = 10usize;
                            if c.selected >= visible {
                                c.scroll_offset = c.selected - visible + 1;
                            }
                        }
                        // Only scroll up when selected moves above the visible window.
                        if c.selected < c.scroll_offset {
                            c.scroll_offset = c.selected;
                        }
                        let text = c.line(c.selected);
                        self.input_panel.set_content(&text);
                    }
                    return Ok(());
                }
                KeyCode::Down => {
                    if let Some(ref mut c) = self.input_panel.completion {
                        if c.selected + 1 < c.candidates.len() {
                            c.selected += 1;
                        } else {
                            // Wrap to first item; reset scroll to top.
                            c.selected = 0;
                            c.scroll_offset = 0;
                        }
                        let visible = 10usize;
                        if c.selected >= c.scroll_offset + visible {
                            c.scroll_offset = c.selected - visible + 1;
                        }
                        let text = c.line(c.selected);
                        self.input_panel.set_content(&text);
                    }
                    return Ok(());
                }
                KeyCode::Esc => {
                    self.input_panel.completion = None;
                    return Ok(());
                }
                KeyCode::Enter if Self::is_newline_modifier(key_event.modifiers) => {
                    // Ctrl/Alt/Shift+Enter: insert newline, keep completions updated.
                    self.input_panel.insert_newline();
                    self.update_completions();
                    return Ok(());
                }
                KeyCode::Enter => {
                    // Execute what's in the input as-is (navigation already wrote
                    // the highlighted candidate into it), closing the popup.
                    self.input_panel.completion = None;
                    let text = self.input_panel.get_content();
                    if text.starts_with('/') {
                        self.execute_command(&text);
                    } else {
                        self.events.send(AppEvent::Start);
                    }
                    return Ok(());
                }
                _ => {
                    // Any other key: pass through to text area, then refresh completions.
                    self.input_panel.input(key_event);
                    self.update_completions();
                    return Ok(());
                }
            }
        }

        match key_event.code {
            KeyCode::Char('q' | 'Q') if key_event.modifiers == KeyModifiers::CONTROL => {
                self.events.send(AppEvent::Quit)
            }
            KeyCode::Char('c' | 'C') if key_event.modifiers == KeyModifiers::CONTROL => {
                self.events.send(AppEvent::Quit)
            }
            KeyCode::Tab => {
                // Popup not visible: compute completions. A single candidate is
                // filled in directly; multiple candidates (re)open the popup
                // without touching the typed text.
                let content = self.input_panel.get_content();
                if content.starts_with('/') {
                    self.input_panel.completion =
                        CompletionEngine::complete(&content, &self.provider_manager, &self.skill_registry);
                    if let Some(ref c) = self.input_panel.completion {
                        if c.candidates.len() == 1 {
                            let text = c.line(0);
                            self.input_panel.set_content(&text);
                            self.input_panel.completion = None;
                        }
                    }
                }
            }
            KeyCode::Esc => {
                if self.input_panel.completion.is_some() {
                    self.input_panel.completion = None;
                } else if self.conversation_panel.is_busy() {
                    self.events.send(AppEvent::Cancel)
                } else {
                    self.input_panel.input(key_event);
                }
            }
            KeyCode::Down => {
                // Only navigate history from the last line; otherwise move the
                // cursor down within multi-line text.
                if self.input_panel.cursor_on_last_line() && self.input_panel.is_navigating_history()
                {
                    self.input_panel.history_down();
                } else {
                    self.input_panel.input(key_event);
                }
            }
            KeyCode::Up => {
                if !self.input_panel.cursor_on_first_line() {
                    // Move the cursor up within multi-line text.
                    self.input_panel.input(key_event);
                } else if self.input_panel.get_content().is_empty() {
                    // Empty input: restore the pending message if there is one,
                    // otherwise start navigating history.
                    if let Some(pending) = self.conversation_panel.pending_message.take() {
                        self.input_panel.set_content(&pending);
                    } else {
                        self.input_panel.history_up();
                    }
                } else if self.input_panel.is_navigating_history() {
                    // Already navigating: keep going to older entries.
                    self.input_panel.history_up();
                } else {
                    self.input_panel.input(key_event);
                }
            }
            KeyCode::Enter if Self::is_newline_modifier(key_event.modifiers) => {
                // Ctrl/Alt/Shift+Enter: insert newline into the text area.
                self.input_panel.insert_newline();
                self.update_completions();
            }
            KeyCode::Enter => {
                let text = self.input_panel.get_content();
                if text.starts_with('/') {
                    self.execute_command(&text);
                } else {
                    self.events.send(AppEvent::Start);
                }
            }
            _ => {
                self.input_panel.input(key_event);
                self.update_completions();
            }
        }
        Ok(())
    }

    /// Handles the tick event of the terminal.
    ///
    /// The tick event is where you can update the state of your application with any logic that
    /// needs to be updated at a fixed frame rate. E.g. polling a server, updating an animation.
    pub fn tick(&self) {}

    // ------------------------------------------------------------------
    // Slash-command support
    // ------------------------------------------------------------------

    /// Recompute tab-completion candidates from the current input text.
    fn update_completions(&mut self) {
        let content = self.input_panel.get_content();
        if content.starts_with('/') {
            self.input_panel.completion =
                CompletionEngine::complete(&content, &self.provider_manager, &self.skill_registry);
            // Always show popup when we have completions.
            if let Some(ref mut c) = self.input_panel.completion {
                c.visible = true;
            }
        } else {
            self.input_panel.completion = None;
        }
    }

    /// Parse and execute a slash command. If the command is unknown, fall back
    /// to sending it to the AI model.
    fn execute_command(&mut self, input: &str) {
        let command = Command::parse(input);
        self.input_panel.completion = None;
        let is_known = command.is_some();

        match command {
            Some(Command::Quit) => {
                self.input_panel.clear();
                self.quit();
            }
            Some(Command::Clear) => {
                self.input_panel.clear();
                self.conversation_panel.clear_messages();
                self.reset_diagnostics_state();
                self.delete_session();
                self.todo_list = crate::todos::TodoList::default();
                crate::todos::TodoList::clear_file();
                self.save_session();
            }
            Some(Command::New) => {
                self.input_panel.clear();
                // Save current session to disk before switching.
                self.save_session();
                self.conversation_panel.clear_messages();
                self.reset_diagnostics_state();
                if let Some(mgr) = &self.session_mgr {
                    let new_session = mgr.create();
                    self.session_uuid = new_session.uuid;
                }
                self.todo_list = crate::todos::TodoList::default();
                crate::todos::TodoList::clear_file();
                self.conversation_panel
                    .add_info_string("Started a new session. Previous session saved.".to_string());
                self.save_session();
            }
            Some(Command::Model(model)) => {
                self.input_panel.clear();
                let model = model.trim().to_string();
                if model.is_empty() {
                    self.conversation_panel.add_info_string(
                        "usage: /model <provider/model> — e.g. /model openai/gpt-4o",
                    );
                    self.save_session();
                    return;
                }
                match self.provider_manager.resolve(&model) {
                    Some(_) => {
                        self.current_model = model;
                        self.conversation_panel
                            .add_info_string(format!("switched to model: {}", self.current_model));
                    }
                    None => {
                        self.conversation_panel.add_error_string(format!(
                            "unknown provider/model: {model} — use /providers to list available",
                        ));
                    }
                }
                self.save_session();
            }
            Some(Command::Mode(arg)) => {
                self.input_panel.clear();
                let prev = self.work_mode;
                match arg.trim().to_lowercase().as_str() {
                    "manual" => self.work_mode = WorkMode::Manual,
                    "edits" | "edit" | "allowedits" | "allow" => {
                        self.work_mode = WorkMode::AllowEdits
                    }
                    "auto" => self.work_mode = WorkMode::Auto,
                    "yolo" => {
                        if self.config.allow_yolo {
                            self.work_mode = WorkMode::Yolo;
                        } else {
                            self.conversation_panel.add_error_string(
                                "YOLO mode runs every tool call unchecked and is \
                                 disabled by default — set `allow_yolo = true` in \
                                 config to enable it"
                                    .to_string(),
                            );
                            return;
                        }
                    }
                    "" => self.work_mode = self.work_mode.next(),
                    other => {
                        self.conversation_panel.add_error_string(format!(
                            "unknown mode '{other}' — use manual, edits, or auto"
                        ));
                        return;
                    }
                }
                if self.work_mode != prev {
                    self.persist_config();
                }
            }
            Some(Command::Classifier(arg)) => {
                self.input_panel.clear();
                let arg = arg.trim().to_string();
                match arg.as_str() {
                    "" => {
                        let current = self
                            .config
                            .classifier_model
                            .clone()
                            .unwrap_or_else(|| format!("{} (chat model)", self.current_model));
                        self.conversation_panel.add_info_string(format!(
                            "classifier model: {current}\n\
                             usage: /classifier <provider/model> to set, \
                             /classifier clear to reset to the chat model"
                        ));
                    }
                    "clear" | "default" | "reset" => {
                        self.config.classifier_model = None;
                        self.conversation_panel.add_info_string(
                            "classifier model reset — Auto mode now uses the chat model",
                        );
                        self.persist_config();
                    }
                    model => match self.provider_manager.resolve(model) {
                        Some(_) => {
                            self.config.classifier_model = Some(model.to_string());
                            self.conversation_panel
                                .add_info_string(format!("classifier model set to: {model}"));
                            self.persist_config();
                        }
                        None => {
                            self.conversation_panel.add_error_string(format!(
                                "unknown provider/model: {model} — use /providers to list available"
                            ));
                        }
                    },
                }
                self.save_session();
            }
            Some(Command::Providers(args)) => {
                self.input_panel.clear();
                match args.trim() {
                    "show" => {
                        let mut lines = vec!["Configured providers:".to_string()];
                        for name in self.provider_manager.provider_names() {
                            let models = self
                                .provider_manager
                                .models_for(name)
                                .iter()
                                .map(|m| format!("    {name}/{m}"))
                                .collect::<Vec<_>>()
                                .join("\n");
                            lines.push(format!("  {name}:"));
                            lines.push(models);
                        }
                        self.conversation_panel.add_info_string(lines.join("\n"));
                    }
                    "manage" => {
                        self.provider_panel = Some(ProviderPanel::new());
                    }
                    _ => {
                        self.conversation_panel.add_info_string(
                            "usage: /providers show — list providers and models\n\
                             \u{20}      /providers manage — open the management panel",
                        );
                    }
                }
                self.save_session();
            }
            Some(Command::Session) => {
                self.input_panel.clear();
                let item_count = self.conversation_panel.items().count();
                let msg = match &self.session_mgr {
                    Some(mgr) => {
                        let path = mgr.dir().join(format!("{}.json", self.session_uuid));
                        let exists = path.exists();
                        let saved_info = if exists {
                            "saved on disk".to_string()
                        } else {
                            "not yet saved".to_string()
                        };
                        format!(
                            "Session: {} messages, {}\n  uuid: {}\n  path: {}",
                            item_count,
                            saved_info,
                            self.session_uuid,
                            path.display()
                        )
                    }
                    None => format!("Session: {} messages (no session manager)", item_count),
                };
                self.conversation_panel.add_info_string(msg);
                self.save_session();
            }
            Some(Command::Init) => {
                self.input_panel.clear();
                self.conversation_panel.add_info_string(
                    "Initializing project: exploring the codebase, writing \
                     PROGRAMMER.md, and setting up diagnostics…"
                        .to_string(),
                );
                // Run as a normal agent turn driven by a synthetic prompt.
                self.events.send(AppEvent::StartInit);
            }
            Some(Command::Todo) => {
                self.input_panel.clear();
                let list = self.todo_list.clone();
                self.todo_panel = Some(TodoPanel::new(list));
            }
            Some(Command::Skill(arg)) => {
                self.input_panel.clear();
                match arg.as_str() {
                    "" | "list" => {
                        let lines: Vec<String> = self
                            .skill_registry
                            .names()
                            .iter()
                            .map(|name| {
                                let marker = if self
                                    .skill_registry
                                    .activated_names()
                                    .contains(name)
                                {
                                    " (*)"
                                } else {
                                    ""
                                };
                                let desc = self
                                    .skill_registry
                                    .get(name)
                                    .map(|s| s.description.as_str())
                                    .unwrap_or("");
                                format!("  {name}{marker}  {desc}")
                            })
                            .collect();
                        if lines.is_empty() {
                            self.conversation_panel
                                .add_info_string("No skills installed. Place SKILL.md files in .programmer/skills/<name>/ or ~/.config/programmer/skills/<name>/.".to_string());
                        } else {
                            let mut out =
                                vec!["Available skills ((*) = active):".to_string()];
                            out.extend(lines);
                            self.conversation_panel.add_info_string(out.join("\n"));
                        }
                    }
                    "off" | "none" | "clear" => {
                        if self.skill_registry.has_active() {
                            let names: Vec<String> = self
                                .skill_registry
                                .activated_names()
                                .iter()
                                .map(|n| n.clone())
                                .collect();
                            self.skill_registry.clear();
                            self.conversation_panel.add_info_string(format!(
                                "Skills deactivated: {}",
                                names.join(", ")
                            ));
                        } else {
                            self.conversation_panel
                                .add_info_string("No skills are currently active.");
                        }
                    }
                    "manage" => {
                        self.skills_panel = Some(SkillsPanel::new());
                    }
                    name => {
                        if self.skill_registry.activate(name) {
                            self.conversation_panel.add_info_string(format!(
                                "Skill activated: {name}"
                            ));
                        } else {
                            self.conversation_panel.add_error_string(format!(
                                "Skill '{name}' not found. Use /skill list to see available skills."
                            ));
                        }
                    }
                }
                self.save_session();
            }
            Some(Command::Mcp(args)) => {
                self.input_panel.clear();
                match args.trim() {
                    "show" => {
                        if self.config.mcp_servers.is_empty() {
                            self.conversation_panel.add_info_string(
                                "No MCP servers configured. Use /mcp manage to add one."
                                    .to_string(),
                            );
                        } else {
                            let mut lines = vec!["Configured MCP servers:".to_string()];
                            for s in &self.config.mcp_servers {
                                let connected = self
                                    .mcp_manager
                                    .as_ref()
                                    .map(|m| {
                                        m.all_tools()
                                            .iter()
                                            .filter(|(fqn, _)| {
                                                fqn.strip_prefix("mcp__")
                                                    .and_then(|r| r.split_once("__"))
                                                    .map(|(srv, _)| srv == s.name)
                                                    .unwrap_or(false)
                                            })
                                            .count()
                                    })
                                    .unwrap_or(0);
                                let cmdline = if s.args.is_empty() {
                                    s.command.clone()
                                } else {
                                    format!("{} {}", s.command, s.args.join(" "))
                                };
                                lines.push(format!(
                                    "  {}: {cmdline}  ({connected} tools)",
                                    s.name
                                ));
                            }
                            self.conversation_panel.add_info_string(lines.join("\n"));
                        }
                    }
                    "manage" => {
                        self.mcp_panel = Some(McpPanel::new());
                    }
                    _ => {
                        self.conversation_panel.add_info_string(
                            "usage: /mcp show — list MCP servers and their status\n\
                             \u{20}      /mcp manage — open the management panel",
                        );
                    }
                }
                self.save_session();
            }
            Some(Command::Help) => {
                self.input_panel.clear();
                let mut lines: Vec<String> = Command::descriptions()
                    .iter()
                    .map(|(cmd, desc)| format!("  {cmd:35} {desc}"))
                    .collect();
                lines.insert(0, "Available commands:".to_string());
                self.conversation_panel.add_info_string(lines.join("\n"));
                self.save_session();
            }
            None => {
                // Unknown slash-command; send it to the AI as a normal message.
                // Don't clear — let send_message handle it (which also pushes history).
                self.events.send(AppEvent::Start);
            }
        }

        // Push known commands to history (unknown commands go through send_message).
        if is_known {
            self.input_panel.push_history(input.to_string());
        }
    }

    /// Set running to false to quit the application.
    pub fn quit(&mut self) {
        self.save_session();
        self.running = false;
    }

    /// Persist the current conversation to the session file.
    fn save_session(&mut self) {
        let Some(mgr) = &self.session_mgr else { return };
        let items: Vec<MessageItem> = self.conversation_panel.items().cloned().collect();
        let mut session = mgr.load(&self.session_uuid).unwrap_or_else(|| {
            // File missing/corrupt — create a fresh session but keep the
            // existing UUID so we don't drift.
            let mut s = mgr.create();
            s.uuid = self.session_uuid.clone();
            s
        });
        // Capture first user message for the picker preview.
        if session.first_message.is_empty() {
            if let Some(text) = first_user_text(&items) {
                session.first_message =
                    crate::session::truncate_first_line(&text, 80);
            }
        }
        SessionManager::set_items(&mut session, items);
        session.history = self.input_panel.history.clone();
        session.work_mode = Some(self.work_mode);
        session.current_model = Some(self.current_model.clone());
        session.classifier_model = self.config.classifier_model.clone();
        session.todos = self.todo_list.todos.clone();
        session.activated_skills = self.skill_registry.activated_names().to_vec();
        if let Err(e) = mgr.save(&mut session) {
            self.conversation_panel
                .add_error_string(format!("session save: {e}"));
        }
    }

    /// Write the current config back to `config.toml` (used by the provider
    /// management panel).
    fn persist_config(&mut self) {
        let Some(config_dir) = dirs::config_dir() else {
            self.conversation_panel
                .add_error_string("cannot locate the config directory");
            return;
        };
        let path = config_dir.join("programmer").join("config.toml");
        let result = toml::to_string(&self.config)
            .map_err(|e| format!("serialize config: {e}"))
            .and_then(|s| {
                std::fs::write(&path, s).map_err(|e| format!("write {}: {e}", path.display()))
            });
        if let Err(e) = result {
            self.conversation_panel
                .add_error_string(format!("failed to save config: {e}"));
        }
    }

    /// Delete the session file (used after /clear).
    fn delete_session(&mut self) {
        if let Some(mgr) = &self.session_mgr {
            let _ = mgr.delete(&self.session_uuid);
            // Start a fresh session with a new UUID.
            let new_session = mgr.create();
            self.session_uuid = new_session.uuid;
        }
    }
}

/// Returns `true` when the error is a transport-level network failure
/// (DNS, connection refused, timeout, TLS error, etc.) — i.e. we never
/// received an HTTP response from the server.
/// Whether a failed `create_stream` is worth retrying: transport-level errors
/// with no HTTP status (connection refused, DNS, reset), plus transient server
/// responses (429 rate-limit and 5xx gateway/overload codes).
fn is_retryable(error: &OpenAIError) -> bool {
    match error {
        OpenAIError::Reqwest(e) => match e.status() {
            None => true,
            Some(status) => {
                status.as_u16() == 429
                    || matches!(status.as_u16(), 500 | 502 | 503 | 504)
            }
        },
        _ => false,
    }
}

/// Exponential backoff for retry `attempt` (1-based): `2^(attempt-1)` seconds
/// capped at 30s, plus up to ~500ms of jitter to avoid synchronized retries.
fn backoff_delay(attempt: u32) -> std::time::Duration {
    const CAP_SECS: u64 = 30;
    let base = 1u64.checked_shl(attempt - 1).unwrap_or(CAP_SECS).min(CAP_SECS);
    let jitter_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| (d.subsec_nanos() as u64) % 500)
        .unwrap_or(0);
    std::time::Duration::from_secs(base) + std::time::Duration::from_millis(jitter_ms)
}

/// Truncate to at most `max` characters (on a char boundary), appending an
/// ellipsis when clipped. Used to keep classifier context compact.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}

/// Build a `function_call_output` carrying a classifier denial, fed back to the
/// model so it learns why the call was blocked and can adjust.
/// How many file-editing turns pass between reminders to refresh PROGRAMMER.md.
const OVERVIEW_REMINDER_EVERY: usize = 5;

/// Whether the project's diagnostics profile declares at least one LSP checker.
/// Cheap enough to call when the profile may have changed (startup, after a
/// tool batch), but not every render frame.
fn lsp_checker_configured() -> bool {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::Path::new(".").to_path_buf());
    matches!(
        crate::diagnostics::DiagnosticsProfile::load(&cwd),
        Some(Ok(profile))
            if profile
                .checkers
                .iter()
                .any(|c| c.kind == crate::diagnostics::CheckerKind::Lsp)
    )
}

/// The hidden developer message nudging the agent to keep PROGRAMMER.md current.
fn overview_reminder() -> String {
    "Reminder: several edits have accumulated since PROGRAMMER.md was last \
     written. If the architecture, build/test commands, directory layout, or \
     conventions have changed, update PROGRAMMER.md now with write_file so it \
     stays an accurate map for future sessions. If nothing meaningful changed, \
     ignore this."
        .to_string()
}

/// Build a hidden `Developer`-role message. It is sent to the model (and is
/// visible to the classifier) but never rendered as a user bubble — see
/// [`MessageItem::is_hidden_developer`].
fn make_developer_message(text: String) -> ApiMessageItem {
    ApiMessageItem::Input(InputMessage {
        content: vec![InputContent::InputText(InputTextContent { text })],
        role: InputRole::Developer,
        status: Some(OutputStatus::Completed),
    })
}

/// The synthetic user prompt that drives `/init`. It reuses the normal agent
/// loop and existing tools (read/grep/blob, write_file, configure_diagnostics)
/// rather than any bespoke initialization code.
fn init_prompt() -> String {
    "Initialize this project for our future work together. Do the following, in order:\n\
     \n\
     1. Explore the repository to understand it: read the README and any build \
     manifests (Cargo.toml, package.json, pyproject.toml, go.mod, etc.), and skim \
     the main source directories to learn the architecture, entry points, and \
     conventions. Ground everything in what you actually read — do not invent.\n\
     \n\
     2. Write a concise `PROGRAMMER.md` at the repository root capturing your \
     understanding: a one-paragraph overview, the tech stack, how to build / test / \
     run, the layout of key directories, and any notable conventions or gotchas. \
     Keep it tight and factual — it is a map for future sessions, not marketing.\n\
     \n\
     3. Set up diagnostics so edits get IDE-style error feedback. Determine how \
     this project surfaces compile/lint errors and call `configure_diagnostics` \
     with a profile of one-shot checker commands. Common cases: Rust → \
     `cargo check --message-format=json` with parser `rustc-json`; TypeScript → \
     `tsc --noEmit` with parser `tsc`; C/C++/others that print \
     `file:line:col: severity: message` → parser `gnu`; anything else → parser \
     `regex` with a `pattern` you write. Prefer commands that terminate (NOT \
     watch/dev-servers). A language server may be used instead via \
     `kind = \"lsp\"` with `command` set to its launch line (e.g. `clangd`), but \
     it re-initializes each run and is slower, so favour a command checker unless \
     there's a clear reason. The tool test-runs each checker and refuses to save \
     a profile that doesn't work, so iterate until it saves. If you genuinely \
     can't find a suitable checker, note that in PROGRAMMER.md and skip this \
     step.\n\
     \n\
     When done, briefly summarize what you set up."
        .to_string()
}

fn classifier_denied_output(
    call: &FunctionToolCall,
    reason: &str,
) -> FunctionCallOutputItemParam {
    FunctionCallOutputItemParam {
        call_id: call.call_id.clone(),
        output: FunctionCallOutput::Text(format!(
            "error: tool call blocked by classifier — {reason}"
        )),
        id: None,
        status: None,
    }
}

/// Extracts the function/tool calls the model emitted in a finished response.
fn function_calls(partial_response: &PartialResponse) -> Vec<FunctionToolCall> {
    partial_response
        .items
        .iter()
        .flatten()
        .filter_map(|item| match item {
            OutputItem::FunctionCall(call) => Some(call.clone()),
            _ => None,
        })
        .collect()
}

/// Extract the text of the first user message from a list of items.
fn first_user_text(items: &[MessageItem]) -> Option<String> {
    items.iter().find_map(|item| match item {
        // Skip hidden developer prompts (e.g. `/init`) so they don't become the
        // session's preview text.
        MessageItem::Input(input) if !item.is_hidden_developer() => extract_input_text(input),
        _ => None,
    })
}

fn extract_input_text(input: &InputItem) -> Option<String> {
    match input {
        InputItem::Item(item) => match item {
            Item::Message(msg) => match msg {
                ApiMessageItem::Input(input_msg) => {
                    input_msg.content.iter().find_map(|c| match c {
                        InputContent::InputText(t) => Some(t.text.clone()),
                        _ => None,
                    })
                }
                _ => None,
            },
            _ => None,
        },
        InputItem::EasyMessage(msg) => match &msg.content {
            async_openai::types::responses::EasyInputContent::Text(t) => Some(t.clone()),
            async_openai::types::responses::EasyInputContent::ContentList(parts) => {
                parts.iter().find_map(|c| match c {
                    InputContent::InputText(t) => Some(t.text.clone()),
                    _ => None,
                })
            }
        },
        _ => None,
    }
}
