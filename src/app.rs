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
    ConversationPanel, SelectionEnd,
};
use crate::ui::components::footer::footer::Footer;
use crate::ui::components::input_panel::input_panel::InputPanel;
use crate::ui::components::provider_panel::{PanelAction, ProviderPanel};
use crate::ui::components::question_panel::QuestionPanel;
use crate::ui::event::{AppEvent, Event, EventHandler};
use async_openai::error::OpenAIError;
use async_openai::types::responses::{
    CreateResponse, FunctionCallOutput, FunctionCallOutputItemParam, FunctionToolCall,
    InputContent, InputItem, InputMessage, InputRole, InputTextContent, Item,
    MessageItem as ApiMessageItem, OutputItem, OutputStatus, ResponseStreamEvent,
};
use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use futures::StreamExt;
use ratatui::DefaultTerminal;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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
    /// Modal question panel shown when the model calls `ask_user`.
    pub question_panel: Option<QuestionPanel>,
    /// Current safety/work mode.
    pub work_mode: WorkMode,
    /// Pending tool-call approvals in Manual mode: (call, reason).
    /// Processed one-at-a-time; approved calls are collected and run afterwards.
    pub(crate) approval_queue: Vec<(FunctionToolCall, String)>,
    /// Calls the user has approved so far (waiting for all to be decided).
    pub(crate) approved_calls: Vec<FunctionToolCall>,
    /// Which option is highlighted in the approval UI (0=approve,1=deny,2=approve all,3=deny all).
    pub(crate) approval_selected: usize,
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
        config: ProgrammerConfig,
        saved_items: Vec<MessageItem>,
        saved_history: Vec<String>,
        session_uuid: String,
        session_mgr: Option<SessionManager>,
        startup_messages: Vec<String>,
        open_provider_panel: bool,
    ) -> Self {
        let provider_manager = ProviderManager::new(&config).await;
        let current_model = provider_manager.default_model();
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
        Self {
            running: true,
            provider_manager,
            current_model,
            events: EventHandler::new(),
            config,
            input_panel,
            conversation_panel,
            footer: Footer::new(),
            provider_panel: open_provider_panel.then(ProviderPanel::new),
            question_panel: None,
            work_mode: WorkMode::default(),
            approval_queue: Vec::new(),
            approved_calls: Vec::new(),
            approval_selected: 0,
            session_uuid,
            session_mgr,
        }
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
                        self.conversation_panel.creating_tool_call = false;
                        self.conversation_panel.outputting_message = false;
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
                    self.conversation_panel.tool_running = false;
                    for output in outputs {
                        self.conversation_panel.add_tool_output(output);
                    }
                    self.save_session();
                    // A spawned shell can reset the console's input mode; re-assert
                    // mouse capture so scrolling/clicks keep working afterwards.
                    let _ = crossterm::execute!(
                        std::io::stdout(),
                        crossterm::event::EnableMouseCapture
                    );
                    // Continue the turn: send the tool results back to the model.
                    self.spawn_stream();
                }
                AppEvent::Cancel => {
                    if let Some(partial) = &self.conversation_panel.receiving_response {
                        partial.cancelled.store(true, Ordering::Relaxed);
                    }
                    self.conversation_panel.abort_receiving();
                    self.conversation_panel.tool_running = false;
                    self.conversation_panel.creating_tool_call = false;
                    self.conversation_panel.outputting_message = false;
                    self.conversation_panel.flush_usage();
                    self.save_session();
                    if let Some(pending_request) = self.conversation_panel.pending_message.take() {
                        self.start_request(pending_request).await;
                    }
                }
                AppEvent::Quit => self.quit(),
                AppEvent::Start => self.send_message().await,
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
        tokio::spawn(async move {
            let mut outputs = Vec::new();
            for call in &calls {
                let mut out = crate::tools::run_tool_call(call, &sender).await;
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
            role: InputRole::User,
            status: Option::from(OutputStatus::Completed),
        };

        self.conversation_panel
            .add_input_message(ApiMessageItem::Input(input_message));
        self.conversation_panel.reset_accumulated_usage();
        self.save_session();
        self.spawn_stream();
    }

    /// Spawns a streaming response request for the current conversation state
    /// (including tool definitions). Used both to answer a new user message and
    /// to continue the turn after tool calls have run.
    fn spawn_stream(&mut self) {
        let cancel_token = Arc::new(AtomicBool::new(false));
        self.conversation_panel.live_expanded_items.clear();
        self.conversation_panel.creating_tool_call = false;
        self.conversation_panel.outputting_message = false;
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
        let input_param = self.conversation_panel.get_input_param(&self.current_model);
        tokio::spawn(async move {
            let mut request = CreateResponse::default();
            request.stream = Option::from(true);
            request.input = input_param;
            request.model = Option::from(model_name);
            request.tools = Some(crate::tools::tools());

            const MAX_RETRIES: u32 = 10;
            let mut attempt: u32 = 0;
            let stream = loop {
                match client.responses().create_stream(request.clone()).await {
                    Ok(stream) => break Ok(stream),
                    Err(e) if is_network_error(&e) && attempt < MAX_RETRIES => {
                        if cancel_token.load(Ordering::Relaxed) {
                            return;
                        }
                        attempt += 1;
                        let delay = std::time::Duration::from_secs(1 << (attempt - 1));
                        tokio::time::sleep(delay).await;
                    }
                    Err(e) => break Err(e),
                }
            };
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
        self.conversation_panel.tool_running = true;
        self.conversation_panel.creating_tool_call = false;
        self.conversation_panel.outputting_message = false;

        let classifier = self.work_mode.classifier();

        // Split calls: allowed (run now), denied (error now), pending (queue
        // for approval).
        let mut allowed: Vec<&FunctionToolCall> = Vec::new();
        let mut queued: Vec<(FunctionToolCall, String)> = Vec::new();
        let mut denied_outputs = Vec::new();

        for call in &calls {
            match classifier.classify(&call.name, &call.arguments) {
                crate::classifier::Verdict::Allow => {
                    allowed.push(call);
                }
                crate::classifier::Verdict::Deny { reason } => {
                    denied_outputs.push(FunctionCallOutputItemParam {
                        call_id: call.call_id.clone(),
                        output: FunctionCallOutput::Text(format!(
                            "error: tool call blocked by classifier — {reason}"
                        )),
                        id: None,
                        status: None,
                    });
                }
                crate::classifier::Verdict::Ask { reason } => {
                    queued.push((call.clone(), reason));
                }
            }
        }

        // If there are denied outputs, add them to conversation immediately.
        for output in &denied_outputs {
            self.conversation_panel.add_tool_output(output.clone());
        }

        // If there are calls awaiting approval, queue them and exit tool_running
        // so the UI can show the prompt.
        if !queued.is_empty() {
            self.approval_queue = queued;
            self.conversation_panel.tool_running = false;
            return; // Will be restarted when user approves.
        }

        // No approval needed — run allowed calls immediately.
        if !allowed.is_empty() || !denied_outputs.is_empty() {
            let sender = self.events.sender.clone();
            let calls_to_run: Vec<FunctionToolCall> =
                allowed.iter().map(|c| (*c).clone()).collect();
            let label = self.work_mode.label().to_string();
            tokio::spawn(async move {
                let mut outputs = Vec::new();
                outputs.extend(denied_outputs);
                for call in &calls_to_run {
                    if cancel_token.load(Ordering::Relaxed) {
                        break;
                    }
                    let mut out = crate::tools::run_tool_call(call, &sender).await;
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
        } else {
            // All calls were denied — send empty list to unblock the turn.
            self.conversation_panel.tool_running = false;
        }
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
                    self.conversation_panel.creating_tool_call = true;
                } else if receiving.has_message_items() {
                    self.conversation_panel.outputting_message = true;
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
        self.conversation_panel.tool_running = false;
        self.conversation_panel.creating_tool_call = false;
        self.conversation_panel.outputting_message = false;
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

        // ---- Ctrl+T: cycle work mode ----
        if key_event.code == KeyCode::Char('t')
            && key_event.modifiers == KeyModifiers::CONTROL
        {
            self.work_mode = self.work_mode.next();
            self.conversation_panel.add_info_string(format!(
                "{} Work mode: {}",
                self.work_mode.icon(),
                self.work_mode.label()
            ));
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
                        CompletionEngine::complete(&content, &self.provider_manager);
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
                CompletionEngine::complete(&content, &self.provider_manager);
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
                self.delete_session();
                self.save_session();
            }
            Some(Command::New) => {
                self.input_panel.clear();
                // Save current session to disk before switching.
                self.save_session();
                self.conversation_panel.clear_messages();
                if let Some(mgr) = &self.session_mgr {
                    let new_session = mgr.create();
                    self.session_uuid = new_session.uuid;
                }
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
                    "yolo" => self.work_mode = WorkMode::Yolo,
                    "" => self.work_mode = self.work_mode.next(),
                    other => {
                        self.conversation_panel.add_error_string(format!(
                            "unknown mode '{other}' — use manual, edits, or yolo"
                        ));
                        return;
                    }
                }
                if self.work_mode != prev {
                    self.persist_config();
                }
                self.conversation_panel.add_info_string(format!(
                    "{} Work mode: {}",
                    self.work_mode.icon(),
                    self.work_mode.label()
                ));
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
fn is_network_error(error: &OpenAIError) -> bool {
    match error {
        OpenAIError::Reqwest(e) => e.status().is_none(),
        _ => false,
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
        MessageItem::Input(input) => extract_input_text(input),
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
