use crate::config::programmer_config::ProgrammerConfig;
use crate::ui::components::conversation_panel::ConversationPanel;
use crate::ui::components::input_panel::InputPanel;
use crate::ui::event::{AppEvent, Event, EventHandler};
use async_openai::error::OpenAIError;
use async_openai::types::responses::MessageItem::Output;
use async_openai::types::responses::ResponseStreamEvent::{ResponseCreated, ResponseOutputItemAdded, ResponseOutputItemDone, ResponseOutputTextDelta};
use async_openai::types::responses::{AssistantRole, CreateResponse, InputContent, InputMessage, InputRole, InputTextContent, MessageItem, MessagePhase, OutputItem, OutputMessage, OutputMessageContent, OutputStatus, OutputTextContent, ResponseStreamEvent};
use async_openai::{config::OpenAIConfig, Client};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind};
use futures::StreamExt;
use ratatui::DefaultTerminal;

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
    pub conversation_panel: ConversationPanel
}

impl App<'_> {
    /// Constructs a new instance of [`App`].
    pub async fn new(config: ProgrammerConfig) -> Self {
        let openai_config = OpenAIConfig::default().with_api_base(&config.base_url).with_api_key(&config.api_key);
        Self {
            running: true,
            client: Client::with_config(openai_config),
            events: EventHandler::new(),
            config,
            input_panel: InputPanel::new(),
            conversation_panel: ConversationPanel::new(),
        }
    }

    /// Run the application's main loop.
    pub async fn run(mut self, mut terminal: DefaultTerminal) -> color_eyre::Result<()> {
        while self.running {
            terminal.draw(|frame| {
                frame.render_widget(&mut self, frame.area())
            })?;
            match self.events.next().await? {
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
                        _ => {}
                    }
                    _ => {}
                },
                Event::App(app_event) => match app_event {
                    AppEvent::ChunkReceived(chunk) => self.handle_chunk_events(chunk).await,
                    AppEvent::OpenAIErrorReceived(error) => self.handle_error_events(error).await,
                    AppEvent::Quit =>self.quit(),
                    AppEvent::Start => self.start_request()
                },
            }
        }
        Ok(())
    }

    fn start_request(&mut self) {
        let client = self.client.clone();
        let sender = self.events.sender.clone();
        let model = self.config.model.clone();
        let text = self.input_panel.get_content();
        let input_message = InputMessage {
            content: vec![InputContent::InputText(InputTextContent {
                text: text.clone()
            })],
            role: InputRole::User,
            status: Option::from(OutputStatus::Completed),
        };

        self.conversation_panel.add_message(MessageItem::Input(input_message));
        self.input_panel.clear();
        let input_param = self.conversation_panel.get_input_param();
        tokio::spawn(async move {
            let mut request = CreateResponse::default();
            request.stream = Option::from(true);
            request.input = input_param;
            request.model = Option::from(model);
            let stream = client.responses().create_stream(request).await;
            match stream {
                Ok(mut response_stream) => {
                    while let Some(response_stream_event) = response_stream.next().await {
                        match response_stream_event {
                            Ok(response_event) => {
                                let _ = sender.send(Event::App(AppEvent::ChunkReceived(response_event)));
                            },
                            Err(openai_error) => {
                                let _ = sender.send(Event::App(AppEvent::OpenAIErrorReceived(openai_error)));
                            }
                        }
                    }
                }
                Err(openai_error) => {
                    let _ = sender.send(Event::App(AppEvent::OpenAIErrorReceived(openai_error)));
                }
            }
        });
    }

    pub async fn handle_chunk_events(&mut self, response_stream_event: ResponseStreamEvent){
        match response_stream_event {
            ResponseCreated(created_event) => {
                self.conversation_panel.add_message(Output(
                    OutputMessage {
                        content: vec![],
                        id: created_event.response.id,
                        role: AssistantRole::Assistant,
                        phase: Option::from(MessagePhase::Commentary),
                        status: OutputStatus::InProgress,
                    }))
            }
            ResponseOutputItemAdded(item_added_event) => {
                if let Some(Output(existing)) = self.conversation_panel.get_last_message_mut()
                    && let OutputItem::Message(new_msg) = item_added_event.item {
                    *existing = new_msg;
                    existing.content.push(OutputMessageContent::OutputText(OutputTextContent {
                        annotations: vec![],
                        logprobs: None,
                        text: "".to_string(),
                    }))
                }
            }
            ResponseOutputTextDelta(text_delta_event) => {
                if let Some(Output(existing)) = self.conversation_panel.get_last_message_mut() {
                    if let Some(OutputMessageContent::OutputText(output_text_content)) = existing.content.last_mut() {
                        output_text_content.text.push_str(text_delta_event.delta.as_str())
                    }
                }
            }
            ResponseOutputItemDone(item_done_event) => {
                if let Some(Output(existing)) = self.conversation_panel.get_last_message_mut()
                    && let OutputItem::Message(final_msg) = item_done_event.item {
                    *existing = final_msg;
                }
            }
            _ => {}
        }
    }

    pub async fn handle_error_events(&mut self, error: OpenAIError) {
        // self.response.push_str(format!("[Error]{}\n", error).as_str());
    }

    /// Handles the key events and updates the state of [`App`].
    pub fn handle_key_events(&mut self, key_event: KeyEvent) -> color_eyre::Result<()> {
        match key_event.code {
            KeyCode::Char('q' | 'Q') if key_event.modifiers == KeyModifiers::CONTROL => self.events.send(AppEvent::Quit),
            KeyCode::Char('c' | 'C') if key_event.modifiers == KeyModifiers::CONTROL => {
                self.events.send(AppEvent::Quit)
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
