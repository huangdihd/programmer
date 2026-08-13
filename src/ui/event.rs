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

use crate::cancel::CancellationToken;
use crate::tools::ask_user::Question;
use async_openai::types::responses::{FunctionToolCall, ResponseStreamEvent};
use color_eyre::eyre::OptionExt;
use crossterm::event::Event as CrosstermEvent;
use futures::{FutureExt, StreamExt};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::mpsc;

/// Representation of all possible events.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
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
    /// A raw streaming chunk of the runner's in-flight response, forwarded by
    /// the TUI surface for live token rendering. Tagged with the operation id
    /// of the turn that produced it.
    ChunkReceived(u64, Box<ResponseStreamEvent>),
    /// The runner committed the streamed response's items to the shared
    /// conversation: drop the live in-progress view (the committed copy renders
    /// from the conversation now). Tagged with the operation id.
    ResponseCommitted(u64),
    /// The runner's turn moved to a new phase (classifying, running tools, …).
    /// Tagged with the operation id.
    RunnerPhase(u64, crate::runner::RunnerPhase),
    /// Real input usage reported by a response at a call/output-safe point.
    UsageSafePoint(u64, u32),
    /// The runner asks the user to review a tool call the classifier flagged
    /// (`Ask` verdict). Carries the call, the classifier's reason, the call's
    /// 1-based position and batch total, and the oneshot the decision goes
    /// back on. Dropping the sender counts as a denial. Tagged with the
    /// operation id.
    ReviewRequest {
        call: FunctionToolCall,
        reason: String,
        position: (usize, usize),
        reply: ReplyTx,
        operation_id: u64,
        agent_id: Option<u64>,
        agent_generation: Option<u64>,
    },
    /// The runner's turn ended, successfully or not. All end-of-turn bookkeeping
    /// (usage flush, session save, pending-message start) hangs off this.
    /// Tagged with the operation id so stale turn-finishes are dropped.
    TurnFinished(
        u64,
        Result<crate::runner::TurnResult, crate::runner::RunnerError>,
    ),
    /// `/compact` finished: `Ok` carries the summary to install as the new
    /// context boundary, `Err` the error to surface. The token identifies the
    /// run so a summary from a cancelled compaction is dropped. Tagged with
    /// the operation id.
    CompactFinished(u64, usize, Result<String, String>, CancellationToken),
    /// A seamless background compaction finished. The job id and history
    /// epoch make stale summaries harmless after clear/rewind/session changes.
    AutoCompactFinished {
        job_id: u64,
        history_epoch: u64,
        cutoff: usize,
        result: Result<String, String>,
    },
    /// A background process entered a terminal state.
    TaskStateChanged(crate::tasks::TaskLifecycleEvent),
    /// An in-process sub-agent entered a terminal state.
    AgentStateChanged {
        generation: u64,
        id: u64,
    },
    /// Debounced request to hand accumulated task updates to the agent.
    FlushTaskNotifications(u64),
    /// Debounced request to hand completed sub-agent results to the parent.
    FlushAgentNotifications(u64),
    /// Cancel the current in-flight request (streaming or tool calls).
    Cancel,
    /// Quit the application.
    Quit,
    Start,
    /// `/init` was invoked: kick off the project-initialization turn with the
    /// resolved `initialize-project` skill prompt.
    StartInit(String),
    /// Provider config changed (via the management panel): rebuild the
    /// provider manager from the current config.
    ProvidersChanged,
    /// Re-fetch auto-discovered model lists for all configured providers,
    /// or for a single named provider when `Some(name)`. Automatic startup
    /// discovery stays out of the conversation; explicit user refreshes report
    /// one compact result.
    RefreshProviderModels {
        name: Option<String>,
        notify: bool,
    },
    /// Background model discovery finished: apply the fresh model lists and
    /// errors to the provider manager without blocking the event loop.
    ProviderModelsRefreshed {
        requested_providers: Vec<String>,
        models: std::collections::HashMap<String, Vec<String>>,
        startup_errors: Vec<String>,
        notify: bool,
    },
    /// MCP server config changed (via the management panel): re-spawn the
    /// MCP manager from the current config.
    McpChanged,
    /// One configured MCP server finished its background connection attempt.
    McpServerConnectionUpdated {
        generation: u64,
        server_name: String,
        state: crate::mcp::McpConnectionState,
    },
    /// A background MCP connection attempt finished. The generation prevents
    /// an older attempt from replacing a manager created for newer config.
    McpReloaded {
        generation: u64,
        manager: Box<crate::mcp::McpManager>,
    },
    /// `/diagnostics update` finished collecting the project profile. A
    /// missing snapshot means no diagnostics profile is configured.
    DiagnosticsUpdated {
        generation: u64,
        snapshot: Option<crate::diagnostics::Snapshot>,
    },
    /// The `ask_user` tool is prompting the user. Carries the question and a
    /// oneshot sender that the UI uses to send the answer back. Tagged with
    /// the operation id so questions from stale turns are dropped.
    #[allow(missing_docs)]
    QuestionPrompt {
        question: Question,
        answer_tx: AnswerTx,
        operation_id: u64,
    },
    /// A newer programmer release exists. Carries the new tag; the UI shows a
    /// one-line notice suggesting `programmer upgrade`.
    UpdateAvailable(String),
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

/// Wraps the `oneshot::Sender` a [`AppEvent::ReviewRequest`] decision goes back
/// on. Manual Debug impl because `oneshot::Sender` does not implement Debug.
pub struct ReplyTx(pub tokio::sync::oneshot::Sender<crate::runner::ReviewDecision>);

impl std::fmt::Debug for ReplyTx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReplyTx").finish()
    }
}

impl std::fmt::Debug for AppEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ChunkReceived(id, _) => f
                .debug_tuple("ChunkReceived")
                .field(id)
                .field(&"..")
                .finish(),
            Self::ResponseCommitted(id) => f.debug_tuple("ResponseCommitted").field(id).finish(),
            Self::RunnerPhase(id, _) => f.debug_tuple("RunnerPhase").field(id).finish(),
            Self::UsageSafePoint(id, tokens) => f
                .debug_tuple("UsageSafePoint")
                .field(id)
                .field(tokens)
                .finish(),
            Self::ReviewRequest {
                call, operation_id, ..
            } => f
                .debug_struct("ReviewRequest")
                .field("call", &call.name)
                .field("operation_id", operation_id)
                .finish(),
            Self::TurnFinished(id, r) => f
                .debug_tuple("TurnFinished")
                .field(id)
                .field(&r.as_ref().map(|_| "..").map_err(|e| e.to_string()))
                .finish(),
            Self::CompactFinished(id, cutoff, r, _) => f
                .debug_tuple("CompactFinished")
                .field(id)
                .field(cutoff)
                .field(&r.as_ref().map(|_| ".."))
                .finish(),
            Self::AutoCompactFinished { job_id, result, .. } => f
                .debug_struct("AutoCompactFinished")
                .field("job_id", job_id)
                .field("result", &result.as_ref().map(|_| ".."))
                .finish(),
            Self::TaskStateChanged(event) => f
                .debug_tuple("TaskStateChanged")
                .field(&event.task_id)
                .field(&event.new_status)
                .finish(),
            Self::AgentStateChanged { generation, id } => f
                .debug_struct("AgentStateChanged")
                .field("generation", generation)
                .field("id", id)
                .finish(),
            Self::FlushTaskNotifications(token) => f
                .debug_tuple("FlushTaskNotifications")
                .field(token)
                .finish(),
            Self::FlushAgentNotifications(token) => f
                .debug_tuple("FlushAgentNotifications")
                .field(token)
                .finish(),
            Self::Cancel => write!(f, "Cancel"),
            Self::Quit => write!(f, "Quit"),
            Self::Start => write!(f, "Start"),
            Self::StartInit(_) => write!(f, "StartInit"),
            Self::ProvidersChanged => write!(f, "ProvidersChanged"),
            Self::RefreshProviderModels { name, notify } => f
                .debug_struct("RefreshProviderModels")
                .field("name", name)
                .field("notify", notify)
                .finish(),
            Self::ProviderModelsRefreshed { .. } => write!(f, "ProviderModelsRefreshed"),
            Self::McpChanged => write!(f, "McpChanged"),
            Self::McpServerConnectionUpdated {
                generation,
                server_name,
                ..
            } => f
                .debug_struct("McpServerConnectionUpdated")
                .field("generation", generation)
                .field("server_name", server_name)
                .finish(),
            Self::McpReloaded { generation, .. } => f
                .debug_struct("McpReloaded")
                .field("generation", generation)
                .finish(),
            Self::DiagnosticsUpdated {
                generation,
                snapshot,
            } => f
                .debug_struct("DiagnosticsUpdated")
                .field("generation", generation)
                .field("configured", &snapshot.is_some())
                .finish(),
            Self::QuestionPrompt {
                question,
                operation_id,
                ..
            } => f
                .debug_struct("QuestionPrompt")
                .field("question", &question.text)
                .field("operation_id", operation_id)
                .finish(),
            Self::UpdateAvailable(tag) => f.debug_tuple("UpdateAvailable").field(tag).finish(),
        }
    }
}

/// Application event handler.
#[derive(Debug)]
pub struct EventHandler {
    /// Event sender channel.
    pub sender: mpsc::UnboundedSender<Event>,
    /// Event receiver channel.
    receiver: mpsc::UnboundedReceiver<Event>,
    /// At most one tick may wait in the FIFO, so a slow render cannot bury
    /// keyboard and mouse events under stale animation work.
    tick_queued: Arc<AtomicBool>,
    /// The task that reads crossterm events and emits ticks.
    _task: tokio::task::JoinHandle<()>,
}

impl EventHandler {
    /// Constructs a new instance of [`EventHandler`].
    pub fn new() -> Self {
        let tick_rate = tick_interval();
        let (sender, receiver) = mpsc::unbounded_channel();
        let _sender = sender.clone();
        let tick_queued = Arc::new(AtomicBool::new(false));
        let producer_tick_queued = tick_queued.clone();
        let _task = tokio::spawn(async move {
            let mut reader = crossterm::event::EventStream::new();
            let mut tick = tokio::time::interval(tick_rate);
            loop {
                let tick_delay = tick.tick();
                let crossterm_event = reader.next().fuse();
                tokio::select! {
                  _ = _sender.closed() => {
                    break;
                  }
                  _ = tick_delay => {
                    queue_tick(&_sender, &producer_tick_queued);
                  }
                  Some(Ok(evt)) = crossterm_event => {
                    let _ = _sender.send(Event::Crossterm(evt));
                  }
                }
            }
        });

        Self {
            sender,
            receiver,
            tick_queued,
            _task,
        }
    }

    /// Receive the next event from the handler.
    ///
    /// This function will block the current thread until an event is received. The event can be a
    /// tick event, a crossterm event, or an application event.
    pub async fn next(&mut self) -> color_eyre::Result<Event> {
        let event = self
            .receiver
            .recv()
            .await
            .ok_or_eyre("application event channel closed unexpectedly")?;
        self.mark_received(&event);
        Ok(event)
    }

    /// Attempt to receive an event without blocking.
    ///
    /// This can be used to drain the event queue between frames.
    pub fn try_next(&mut self) -> Option<Event> {
        let event = self.receiver.try_recv().ok()?;
        self.mark_received(&event);
        Some(event)
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

    fn mark_received(&self, event: &Event) {
        if matches!(event, Event::Tick) {
            self.tick_queued.store(false, Ordering::Release);
        }
    }
}

fn queue_tick(sender: &mpsc::UnboundedSender<Event>, queued: &AtomicBool) {
    if queued
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
        && sender.send(Event::Tick).is_err()
    {
        queued.store(false, Ordering::Release);
    }
}

fn tick_interval() -> Duration {
    Duration::from_secs_f64(1.0 / crate::consts::TICK_FPS)
}

#[cfg(test)]
mod tests {
    use super::{Event, queue_tick, tick_interval};
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::sync::mpsc;

    #[test]
    fn tick_interval_is_one_over_tick_fps() {
        // EventHandler::new() uses this exact helper, so the production call
        // site cannot accidentally pass FPS as a duration again.
        let ms = tick_interval().as_millis();
        assert!(
            (30..=35).contains(&ms),
            "expected ~33ms per tick, got {ms}ms — is the formula 1.0 / TICK_FPS?"
        );
    }

    #[test]
    fn queued_ticks_are_coalesced_until_consumed() {
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let queued = AtomicBool::new(false);

        queue_tick(&sender, &queued);
        queue_tick(&sender, &queued);
        assert!(matches!(receiver.try_recv(), Ok(Event::Tick)));
        assert!(receiver.try_recv().is_err());

        queued.store(false, Ordering::Release);
        queue_tick(&sender, &queued);
        assert!(matches!(receiver.try_recv(), Ok(Event::Tick)));
    }
}
