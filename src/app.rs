use crate::config::programmer_config::ProgrammerConfig;
use crate::ui::event::{AppEvent, Event, EventHandler};
use async_openai::error::OpenAIError;
use async_openai::types::responses::ResponseStreamEvent::ResponseOutputTextDelta;
use async_openai::types::responses::{CreateResponse, InputParam, ResponseStreamEvent};
use async_openai::{config::OpenAIConfig, Client};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use ratatui::DefaultTerminal;
use ratatui_textarea::TextArea;

/// Application.
#[derive(Debug)]
pub struct App<'a> {
    /// Is the application running?
    pub running: bool,
    /// OpenAI client.
    pub client: Client<OpenAIConfig>,
    /// Event handler.
    pub events: EventHandler,
    pub response: String,
    pub config: ProgrammerConfig,
    pub textarea: TextArea<'a>
}

impl App<'_> {
    /// Constructs a new instance of [`App`].
    pub async fn new(config: ProgrammerConfig) -> Self {
        let openai_config = OpenAIConfig::default().with_api_base(&config.base_url).with_api_key(&config.api_key);
        Self {
            running: true,
            client: Client::with_config(openai_config),
            events: EventHandler::new(),
            response: String::new(),
            config,
            textarea: Default::default(),
        }
    }

    /// Run the application's main loop.
    pub async fn run(mut self, mut terminal: DefaultTerminal) -> color_eyre::Result<()> {
        while self.running {
            terminal.draw(|frame| frame.render_widget(&self, frame.area()))?;
            match self.events.next().await? {
                Event::Tick => self.tick(),
                Event::Crossterm(event) => match event {
                    crossterm::event::Event::Key(key_event)
                        if key_event.kind == crossterm::event::KeyEventKind::Press =>
                    {
                        self.handle_key_events(key_event)?
                    }
                    _ => {}
                },
                Event::App(app_event) => match app_event {
                    AppEvent::ChunkReceived(chunk) => self.handle_chunk_events(chunk).await,
                    AppEvent::OpenAIErrorReceived(error) => self.handle_error_events(error).await,
                    AppEvent::Quit =>self.quit(),
                    AppEvent::Start => {
                        let client = self.client.clone();
                        let sender = self.events.sender.clone();
                        let model = self.config.model.clone();
                        let text = self.textarea.lines().join("\n");
                        self.textarea.clear();
                        tokio::spawn(async move {
                            let mut request = CreateResponse::default();
                            request.stream = Option::from(true);
                            request.input = InputParam::Text(text);
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
                },
            }
        }
        Ok(())
    }

    pub async fn handle_chunk_events(&mut self, response_stream_event: ResponseStreamEvent){
        match response_stream_event {
            ResponseOutputTextDelta(text_delta_event) => {
                self.response.push_str(text_delta_event.delta.as_str());
            },
            _ => {}
        }
    }

    pub async fn handle_error_events(&mut self, error: OpenAIError) {
        self.response.push_str(format!("[Error]{}\n", error).as_str());
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
            _ if key_event.kind != KeyEventKind::Release => {
                self.textarea.input(key_event);
            }
            _ => {}
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
