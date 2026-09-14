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

use crate::cancel::CancellationToken;
use crate::runner::{AgentSurface, ReviewDecision, RunnerEvent};
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
    /// The monotonically increasing operation id assigned to this turn.
    pub operation_id: u64,
    /// The turn's root cancellation token, so `review()` can race the
    /// approval wait against an Esc press.
    pub cancel: CancellationToken,
}

#[async_trait::async_trait]
impl AgentSurface for TuiSurface {
    fn on_event(&self, ev: RunnerEvent<'_>) {
        let app_ev = match ev {
            RunnerEvent::StreamChunk(b) => AppEvent::ChunkReceived(self.operation_id, Box::new(*b)),
            RunnerEvent::ResponseCommitted => AppEvent::ResponseCommitted(self.operation_id),
            RunnerEvent::Phase(p) => AppEvent::RunnerPhase(self.operation_id, p),
            RunnerEvent::UsageSafePoint { input_tokens } => {
                let (resume, _receiver) = oneshot::channel();
                AppEvent::UsageSafePoint(self.operation_id, input_tokens, resume)
            }
            RunnerEvent::WaitingSubagents(waiting) => {
                AppEvent::WaitingSubagents(self.operation_id, waiting)
            }
            RunnerEvent::Notice(text) => AppEvent::Notice(self.operation_id, text.to_string()),
            // These are read from the shared conversation directly.
            RunnerEvent::Assistant(_) | RunnerEvent::ToolCall { .. } => return,
        };
        let _ = self.tx.send(Event::App(app_ev));
    }

    async fn usage_safe_point(&self, input_tokens: u32) {
        let (resume_tx, resume_rx) = oneshot::channel();
        let _ = self.tx.send(Event::App(AppEvent::UsageSafePoint(
            self.operation_id,
            input_tokens,
            resume_tx,
        )));
        let _ = self.cancel.wait_or(resume_rx).await;
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
            operation_id: self.operation_id,
            agent_id: None,
            agent_generation: None,
        }));
        // Race the user's approval against Esc (cancel). If the cancel token
        // fires first, the ReplyTx is dropped and `reply_rx` sees a closed
        // channel → denial.
        match self.cancel.wait_or(reply_rx).await {
            Some(Ok(decision)) => decision,
            _ => ReviewDecision::Deny {
                output: crate::runner::classify::classifier_denied_output(call, "cancelled"),
            },
        }
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

    fn operation_id(&self) -> u64 {
        self.operation_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn usage_safe_point_waits_for_frontend_resume() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let surface = TuiSurface {
            tx,
            skill_prompt: None,
            plan_prompt: None,
            approval_label: "test".to_string(),
            operation_id: 42,
            cancel: CancellationToken::new(),
        };
        let waiter = tokio::spawn(async move { surface.usage_safe_point(150_000).await });

        let Event::App(AppEvent::UsageSafePoint(operation_id, tokens, resume)) =
            rx.recv().await.expect("safe-point event")
        else {
            panic!("unexpected event")
        };
        assert_eq!(operation_id, 42);
        assert_eq!(tokens, 150_000);
        assert!(!waiter.is_finished());
        resume.send(()).expect("resume runner");
        waiter.await.expect("runner resumed");
    }
}
