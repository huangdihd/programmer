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

use crate::config::programmer_config::ProgrammerConfig;
use crate::response::message_item;
use crate::response::partial_response::PartialResponse;
use crate::ui::components::conversation_panel::conversation_panel::ConversationPanel;
use crate::ui::components::footer::footer::Footer;
use crate::ui::components::input_panel::input_panel::InputPanel;
use crate::ui::event::{AppEvent, Event, EventHandler};
use async_openai::error::OpenAIError;
use async_openai::types::responses::{
    CreateResponse, FunctionToolCall, InputContent, InputMessage, InputRole, InputTextContent,
    MessageItem, OutputItem, OutputStatus, ResponseStreamEvent,
};
use async_openai::{Client, config::OpenAIConfig};
use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use futures::StreamExt;
use ratatui::DefaultTerminal;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Application.
#[derive(Debug)]
pub struct App<'a> {
    /// Is the application running?
    pub running: bool,
    /// OpenAI client.
    pub client: Client<OpenAIConfig>,
    /// Event handler.
    pub events: EventHandler,
    pub config: ProgrammerConfig,
    pub input_panel: InputPanel<'a>,
    pub conversation_panel: ConversationPanel,
    pub footer: Footer,
}

impl App<'_> {
    /// Constructs a new instance of [`App`].
    pub async fn new(config: ProgrammerConfig) -> Self {
        let openai_config = OpenAIConfig::default()
            .with_api_base(&config.base_url)
            .with_api_key(&config.api_key);
        Self {
            running: true,
            client: Client::with_config(openai_config),
            events: EventHandler::new(),
            config,
            input_panel: InputPanel::new(),
            conversation_panel: ConversationPanel::new(),
            footer: Footer::new(),
        }
    }

    /// Run the application's main loop.
    pub async fn run(mut self, mut terminal: DefaultTerminal) -> color_eyre::Result<()> {
        while self.running {
            terminal.draw(|frame| frame.render_widget(&mut self, frame.area()))?;
            // Block for at least one event, then drain everything else that is
            // already queued before redrawing. During streaming, chunk events
            // arrive far faster than a frame can be drawn; handling the whole
            // burst per redraw collapses dozens of expensive full renders into
            // a single one.
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

    async fn handle_event(&mut self, event: Event) -> color_eyre::Result<()> {
        match event {
            Event::Tick => self.tick(),
            Event::Crossterm(event) => match event {
                crossterm::event::Event::Key(key_event)
                    if key_event.kind == KeyEventKind::Press =>
                {
                    self.handle_key_events(key_event)?
                }
                crossterm::event::Event::Mouse(mouse) => match mouse.kind {
                    MouseEventKind::ScrollDown => self.conversation_panel.scroll_down(),
                    MouseEventKind::ScrollUp => self.conversation_panel.scroll_up(),
                    MouseEventKind::Down(MouseButton::Left) => self
                        .conversation_panel
                        .handle_click(mouse.column, mouse.row),
                    _ => {}
                },
                _ => {}
            },
            Event::App(app_event) => match app_event {
                AppEvent::ChunkReceived(chunk) => self.handle_chunk_events(chunk).await,
                AppEvent::OpenAIErrorReceived(error) => self.handle_error_events(error).await,
                AppEvent::ResponseFinished(partial_response) => {
                    if partial_response.cancelled.load(Ordering::Relaxed) {
                        return Ok(());
                    }
                    let cancel_token = partial_response.cancelled.clone();
                    let calls = function_calls(&partial_response);
                    if calls.is_empty() {
                        self.conversation_panel.creating_tool_call = false;
                        self.conversation_panel.outputting_message = false;
                        if let Some(pending_request) =
                            self.conversation_panel.pending_message.take()
                        {
                            self.start_request(pending_request).await
                        }
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
                    if let Some(pending_request) = self.conversation_panel.pending_message.take() {
                        self.start_request(pending_request).await;
                    }
                }
                AppEvent::Quit => self.quit(),
                AppEvent::Start => self.send_message().await,
            },
        }
        Ok(())
    }

    async fn send_message(&mut self) {
        let text = self.input_panel.get_content();
        if text.is_empty() {
            return;
        }
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
            .add_input_message(MessageItem::Input(input_message));
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
        let client = self.client.clone();
        let sender = self.events.sender.clone();
        let model = self.config.model.clone();
        let input_param = self.conversation_panel.get_input_param();
        tokio::spawn(async move {
            let mut request = CreateResponse::default();
            request.stream = Option::from(true);
            request.input = input_param;
            request.model = Option::from(model);
            request.tools = Some(crate::tools::tools());
            let stream = client.responses().create_stream(request).await;
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
                        let _ = sender.send(Event::App(AppEvent::OpenAIErrorReceived(openai_error)));
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
        let sender = self.events.sender.clone();
        tokio::spawn(async move {
            let mut outputs = Vec::with_capacity(calls.len());
            for call in &calls {
                if cancel_token.load(Ordering::Relaxed) {
                    break;
                }
                outputs.push(crate::tools::run_tool_call(call).await);
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
                .map(|item| message_item::MessageItem::Output(item.clone().into())),
        );
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
        self.conversation_panel.add_error(error);
        if let Some(pending_request) = self.conversation_panel.pending_message.take() {
            self.start_request(pending_request).await;
        }
    }

    /// Handles the key events and updates the state of [`App`].
    pub fn handle_key_events(&mut self, key_event: KeyEvent) -> color_eyre::Result<()> {
        match key_event.code {
            KeyCode::Char('q' | 'Q') if key_event.modifiers == KeyModifiers::CONTROL => {
                self.events.send(AppEvent::Quit)
            }
            KeyCode::Char('c' | 'C') if key_event.modifiers == KeyModifiers::CONTROL => {
                self.events.send(AppEvent::Quit)
            }
            KeyCode::Esc => {
                if self.conversation_panel.is_busy() {
                    self.events.send(AppEvent::Cancel)
                } else {
                    self.input_panel.input(key_event);
                }
            }
            KeyCode::Up => {
                if self.input_panel.get_content().is_empty() {
                    if let Some(pending) = self.conversation_panel.pending_message.take() {
                        self.input_panel.set_content(&pending);
                    }
                } else {
                    self.input_panel.input(key_event);
                }
            }
            KeyCode::Enter if key_event.modifiers != KeyModifiers::CONTROL => {
                self.events.send(AppEvent::Start)
            }
            _ => {
                self.input_panel.input(key_event);
            }
        }
        Ok(())
    }

    /// Handles the tick event of the terminal.
    ///
    /// The tick event is where you can update the state of your application with any logic that
    /// needs to be updated at a fixed frame rate. E.g. polling a server, updating an animation.
    pub fn tick(&self) {}

    /// Set running to false to quit the application.
    pub fn quit(&mut self) {
        self.running = false;
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
