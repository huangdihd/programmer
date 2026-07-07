use std::sync::Arc;
use async_openai::{Client, config::OpenAIConfig };
use async_openai::error::OpenAIError;
use async_openai::traits::EventType;
use async_openai::types::responses::{CreateResponse, InputParam, ResponseStream, ResponseStreamEvent};
use async_openai::types::responses::ResponseStreamEvent::ResponseOutputTextDelta;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use futures::StreamExt;
use ratatui::DefaultTerminal;
use tokio::sync::Mutex;
use crate::ui::event::{AppEvent, Event, EventHandler};

/// Application.
#[derive(Debug)]
pub struct App {
    /// Is the application running?
    pub running: bool,
    /// OpenAI client.
    pub client: Arc<Mutex<Client<OpenAIConfig>>>,
    /// Event handler.
    pub events: EventHandler,
    pub response: Arc<Mutex<String>>
}

impl Default for App {
    fn default() -> Self {
        Self {
            running: true,
            client: Arc::from(Mutex::from(Client::new())),
            events: EventHandler::new(),
            response: Arc::from(Mutex::from(String::new())),
        }
    }
}

impl App {
    /// Constructs a new instance of [`App`].
    pub async fn new() -> Self {
        let openai_config = OpenAIConfig::default().with_api_base("https://agent.shouldbe.top/api/openai/v1").with_api_key("1e55e1dd6fed55ed0b7a430a3beeb1695bd8900f6a48312b01a8e822d9b86674");
        Self {
            running: true,
            client: Arc::from(Mutex::from(Client::with_config(openai_config))),
            events: EventHandler::new(),
            response: Arc::from(Mutex::from(String::new())),
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
                        tokio::spawn(async move {
                            let guard_client = client.lock().await;
                            let mut request = CreateResponse::default();
                            request.stream = Option::from(true);
                            request.input = InputParam::Text("Hello".to_string());
                            request.model = Option::from("deepseek/deepseek-v4-pro".to_string());
                            let stream = guard_client.responses().create_stream(request).await;
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
                let mut guard_response = self.response.lock().await;
                guard_response.push_str(text_delta_event.delta.as_str());
            },
            _ => {
            }
        }
    }

    pub async fn handle_error_events(&mut self, error: OpenAIError) {
        
    }

    /// Handles the key events and updates the state of [`App`].
    pub fn handle_key_events(&mut self, key_event: KeyEvent) -> color_eyre::Result<()> {
        match key_event.code {
            KeyCode::Esc | KeyCode::Char('q') => self.events.send(AppEvent::Quit),
            KeyCode::Char('c' | 'C') if key_event.modifiers == KeyModifiers::CONTROL => {
                self.events.send(AppEvent::Quit)
            }
            KeyCode::Char('s' | 'S') => {
                self.events.send(AppEvent::Start)
            }
            // Other handlers you could add here.
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
