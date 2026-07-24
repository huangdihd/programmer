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

//! The TUI-side [`AgentSurface`] implementation: translates runner callbacks
//! (stream chunks, phase changes, review requests) into [`AppEvent`]s on the
//! app's event channel. A fresh instance is built for each turn.

use crate::runner::{AgentSurface, RunnerEvent, ReviewDecision};
use crate::ui::event::{AppEvent, Event, ReplyTx};
use async_openai::types::responses::FunctionToolCall;
use tokio::sync::mpsc;
use tokio::sync::oneshot;

pub(crate) struct TuiSurface {
    /// The app's event channel — on_event and review both push here.
    pub tx: mpsc::UnboundedSender<Event>,
    /// The resolved skill system prompt for this turn.
    pub skill_prompt: Option<String>,
    /// The plan-mode prompt snippet for this turn (Planning vs. None).
    pub plan_prompt: Option<&'static str>,
    /// The label stamped on auto-approved tool outputs, e.g.
    /// "🤖 approved by Auto mode".
    pub approval_label: String,
}

#[async_trait::async_trait]
impl AgentSurface for TuiSurface {
    fn on_event(&self, ev: RunnerEvent<'_>) {
        let app_ev = match ev {
            RunnerEvent::StreamChunk(b) => AppEvent::ChunkReceived(Box::new(*b)),
            RunnerEvent::ResponseCommitted => AppEvent::ResponseCommitted,
            RunnerEvent::Phase(p) => AppEvent::RunnerPhase(p),
            // These are read from the shared conversation directly.
            RunnerEvent::Assistant(_) | RunnerEvent::ToolCall { .. } => return,
        };
        let _ = self.tx.send(Event::App(app_ev));
    }

    async fn review(
        &self,
        call: &FunctionToolCall,
        reason: &str,
        position: (usize, usize),
    ) -> ReviewDecision {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self.tx.send(Event::App(AppEvent::ReviewRequest {
            call: call.clone(),
            reason: reason.to_string(),
            position,
            reply: ReplyTx(reply_tx),
        }));
        reply_rx.await.unwrap_or_else(|_| {
            ReviewDecision::Deny {
                output: crate::runner::classify::classifier_denied_output(call, "cancelled"),
            }
        })
    }

    fn tool_event_sender(&self) -> Option<mpsc::UnboundedSender<Event>> {
        Some(self.tx.clone())
    }

    fn skill_prompt(&self) -> Option<String> {
        self.skill_prompt.clone()
    }

    fn plan_prompt(&self) -> Option<&str> {
        self.plan_prompt
    }

    fn approval_label(&self) -> String {
        self.approval_label.clone()
    }
}
