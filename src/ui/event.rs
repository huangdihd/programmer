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

use crate::response::partial_response::PartialResponse;
use crate::tools::ask_user::Question;
use async_openai::error::OpenAIError;
use async_openai::types::responses::{FunctionCallOutputItemParam, ResponseStreamEvent};
use color_eyre::eyre::OptionExt;
use crossterm::event::Event as CrosstermEvent;
use futures::{FutureExt, StreamExt};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use tokio::sync::mpsc;

/// The frequency at which tick events are emitted.
const TICK_FPS: f64 = 30.0;

/// Representation of all possible events.
#[derive(Debug)]
pub enum Event {
    /// An event that is emitted on a regular schedule.
    ///
    /// Use this event to run any code which has to run outside of being a direct response to a user
    /// event. e.g. polling exernal systems, updating animations, or rendering the ui based on a
    /// fixed frame rate.
    Tick,
    /// Crossterm events.
    ///
    /// These events are emitted by the terminal.
    Crossterm(CrosstermEvent),
    /// Application events.
    ///
    /// Use this event to emit custom events that are specific to your application.
    App(AppEvent),
}

/// Application events.
///
/// You can extend this enum with your own custom events.
pub enum AppEvent {
    /// Receive a chunk.
    ChunkReceived(ResponseStreamEvent),
    /// Receive an openai error.
    OpenAIErrorReceived(OpenAIError),
    ResponseFinished(PartialResponse),
    /// All tool calls from the last response have run; carries their outputs to
    /// be fed back to the model, plus the cancel token so stale completions
    /// from cancelled requests can be ignored.
    ToolCallsCompleted(Vec<FunctionCallOutputItemParam>, Arc<AtomicBool>),
    /// Cancel the current in-flight request (streaming or tool calls).
    Cancel,
    /// Quit the application.
    Quit,
    Start,
    /// Provider config changed (via the management panel): rebuild the
    /// provider manager from the current config.
    ProvidersChanged,
    /// The `ask_user` tool is prompting the user. Carries the question and a
    /// oneshot sender that the UI uses to send the answer back.
    #[allow(missing_docs)]
    QuestionPrompt {
        question: Question,
        answer_tx: AnswerTx,
    },
}

/// Wraps a `oneshot::Sender<String>` for the `ask_user` tool answer channel.
///
/// Manual Debug impl because `oneshot::Sender` does not implement Debug.
pub struct AnswerTx(pub tokio::sync::oneshot::Sender<String>);

impl std::fmt::Debug for AnswerTx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnswerTx").finish()
    }
}

impl std::fmt::Debug for AppEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ChunkReceived(_) => f.debug_tuple("ChunkReceived").field(&"..").finish(),
            Self::OpenAIErrorReceived(e) => f.debug_tuple("OpenAIErrorReceived").field(e).finish(),
            Self::ResponseFinished(_) => f.debug_tuple("ResponseFinished").field(&"..").finish(),
            Self::ToolCallsCompleted(_, _) => {
                f.debug_tuple("ToolCallsCompleted").field(&"..").finish()
            }
            Self::Cancel => write!(f, "Cancel"),
            Self::Quit => write!(f, "Quit"),
            Self::Start => write!(f, "Start"),
            Self::ProvidersChanged => write!(f, "ProvidersChanged"),
            Self::QuestionPrompt { question, .. } => {
                f.debug_struct("QuestionPrompt")
                    .field("question", question)
                    .finish()
            }
        }
    }
}

/// Terminal event handler.
#[derive(Debug)]
pub struct EventHandler {
    /// Event sender channel.
    pub(crate) sender: mpsc::UnboundedSender<Event>,
    /// Event receiver channel.
    receiver: mpsc::UnboundedReceiver<Event>,
}

impl EventHandler {
    /// Constructs a new instance of [`EventHandler`] and spawns a new thread to handle events.
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        let actor = EventTask::new(sender.clone());
        tokio::spawn(async { actor.run().await });
        Self { sender, receiver }
    }

    /// Receives an event from the sender.
    ///
    /// This function blocks until an event is received.
    ///
    /// # Errors
    ///
    /// This function returns an error if the sender channel is disconnected. This can happen if an
    /// error occurs in the event thread. In practice, this should not happen unless there is a
    /// problem with the underlying terminal.
    pub async fn next(&mut self) -> color_eyre::Result<Event> {
        self.receiver
            .recv()
            .await
            .ok_or_eyre("Failed to receive event")
    }

    /// Returns an already-queued event without waiting, or `None` if the queue
    /// is currently empty.
    ///
    /// Used to drain a burst of events (e.g. a flurry of streaming chunks) and
    /// process them all before the next redraw, so the UI is drawn once per
    /// batch instead of once per event.
    pub fn try_next(&mut self) -> Option<Event> {
        self.receiver.try_recv().ok()
    }

    /// Queue an app event to be sent to the event receiver.
    ///
    /// This is useful for sending events to the event handler which will be processed by the next
    /// iteration of the application's event loop.
    pub fn send(&mut self, app_event: AppEvent) {
        // Ignore the result as the reciever cannot be dropped while this struct still has a
        // reference to it
        let _ = self.sender.send(Event::App(app_event));
    }
}

/// A thread that handles reading crossterm events and emitting tick events on a regular schedule.
struct EventTask {
    /// Event sender channel.
    sender: mpsc::UnboundedSender<Event>,
}

impl EventTask {
    /// Constructs a new instance of [`EventThread`].
    fn new(sender: mpsc::UnboundedSender<Event>) -> Self {
        Self { sender }
    }

    /// Runs the event thread.
    ///
    /// This function emits tick events at a fixed rate and polls for crossterm events in between.
    async fn run(self) -> color_eyre::Result<()> {
        let tick_rate = Duration::from_secs_f64(1.0 / TICK_FPS);
        let mut reader = crossterm::event::EventStream::new();
        let mut tick = tokio::time::interval(tick_rate);
        loop {
            let tick_delay = tick.tick();
            let crossterm_event = reader.next().fuse();
            tokio::select! {
              _ = self.sender.closed() => {
                break;
              }
              _ = tick_delay => {
                self.send(Event::Tick);
              }
              Some(Ok(evt)) = crossterm_event => {
                self.send(Event::Crossterm(evt));
              }
            }
        }
        Ok(())
    }

    /// Sends an event to the receiver.
    fn send(&self, event: Event) {
        // Ignores the result because shutting down the app drops the receiver, which causes the send
        // operation to fail. This is expected behavior and should not panic.
        let _ = self.sender.send(event);
    }
}
