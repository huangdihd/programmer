use crate::config::programmer_config::ProgrammerConfig;
use crate::response::partial_response::PartialResponse;
use crate::ui::components::conversation_panel::conversation_panel::ConversationPanel;
use crate::ui::components::input_panel::input_panel::InputPanel;
use crate::ui::event::{AppEvent, Event, EventHandler};
use async_openai::error::OpenAIError;
use async_openai::types::responses::{CreateResponse, InputContent, InputMessage, InputRole, InputTextContent, MessageItem, OutputStatus, ResponseStreamEvent};
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
                    AppEvent::ResponseFinished(_, _) => {
                        if let Some(pending_request) = self.conversation_panel.pending_message.take() {
                            self.start_request(pending_request).await
                        }
                    }
                    AppEvent::Quit =>self.quit(),
                    AppEvent::Start => self.send_message().await
                },
            }
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
        if self.conversation_panel.receiving_response.is_some() {
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
                text: text.clone()
            })],
            role: InputRole::User,
            status: Option::from(OutputStatus::Completed),
        };

        self.conversation_panel.add_input_message(MessageItem::Input(input_message));
        self.conversation_panel.receiving_response = Some(PartialResponse::new());
        let client = self.client.clone();
        let sender = self.events.sender.clone();
        let model = self.config.model.clone();
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
        let Some((finish_reason, items)) = self.conversation_panel.handle_response_stream_event(response_stream_event) else {
            return;
        };
        self.events.send(AppEvent::ResponseFinished(finish_reason, items));
    }

    pub async fn handle_error_events(&mut self, error: OpenAIError) {
        self.conversation_panel.add_error(error)
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
