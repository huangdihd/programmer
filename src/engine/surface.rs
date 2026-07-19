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

//! The engine's counterpart: whatever drives a turn reports progress to, and
//! asks approval of, an [`AgentSurface`]. This is the seam that lets the same
//! [`Engine`](super::Engine) serve a headless `-p` run, the interactive TUI, and
//! (later) an in-process sub-agent that bubbles its approval requests up to the
//! main UI — each is just a different `AgentSurface` implementation.
//!
//! Two distinct interactions live here on purpose:
//!
//!  * [`AgentSurface::on_event`] is fire-and-forget — a notification the surface
//!    renders (or ignores). It never blocks the engine.
//!  * [`AgentSurface::review`] is request/response — a classifier `Ask` verdict
//!    bubbles up and the engine waits for a decision. This is what a sub-agent
//!    forwards to its parent, and ultimately to the human at the TUI.

use super::EngineEvent;
use crate::ui::event::Event;
use async_openai::types::responses::FunctionToolCall;
use tokio::sync::mpsc::UnboundedSender;

/// The outcome of asking a surface to review a tool call whose classifier
/// verdict was `Ask`.
pub(crate) enum ReviewDecision {
    /// Run the call.
    Approve,
    /// Block the call. The surface constructs the full denial output so each
    /// front-end keeps its own wording (the headless surface reuses the
    /// classifier phrasing verbatim; the TUI uses its "denied by user" text)
    /// — the engine records it as the call's failed result either way.
    Deny { output: crate::tools::ToolOutput },
}

/// What the engine reports to and asks of. Implemented once per front-end:
/// [`HeadlessSurface`] for non-interactive runs, a TUI surface that routes
/// `review` to the approval console, and a sub-agent surface that forwards up.
#[async_trait::async_trait]
pub(crate) trait AgentSurface: Send + Sync {
    /// A progress notification for this turn. Must not block — the engine calls
    /// it inline between iterations.
    fn on_event(&self, event: EngineEvent<'_>);

    /// A classifier `Ask` verdict needs a decision. `reason` is the classifier's
    /// explanation for why the call was flagged; `position` is this call's
    /// 1-based index and the batch total, so an interactive surface can show
    /// "2 of 5" progress. The surface returns whether the call may run; a
    /// `Deny` carries the full denial output to record.
    async fn review(
        &self,
        call: &FunctionToolCall,
        reason: &str,
        position: (usize, usize),
    ) -> ReviewDecision;

    // --- Front-end context (defaulted; the headless surface takes the None /
    // headless answer, so `-p` behaviour is unchanged). ---

    /// The channel tools use to reach the front-end — `ask_user`'s prompt, live
    /// task updates. `None` (the default) means there is no interactive
    /// front-end, so `ask_user` is pre-denied rather than left hanging on a dead
    /// answer channel.
    fn tool_event_sender(&self) -> Option<UnboundedSender<Event>> {
        None
    }

    /// The combined skill system-prompt to fold into every request this turn, if
    /// any. The engine has no skill registry of its own; the front-end supplies
    /// the resolved text.
    fn skill_prompt(&self) -> Option<String> {
        None
    }

    /// The plan-mode system-prompt snippet for this turn, if any.
    fn plan_prompt(&self) -> Option<&str> {
        None
    }

    /// The approval label recorded on auto-approved tool outputs (shown in the
    /// UI). Defaults to the headless wording.
    fn approval_label(&self) -> String {
        format!(
            "{} auto-approved (headless)",
            crate::classifier::WorkMode::Auto.icon()
        )
    }
}

/// The surface for non-interactive runs (the `-p` print mode): progress events
/// are dropped, and any `Ask` is denied because there is no one to ask. This
/// preserves the pre-surface headless behaviour, where `Ask` verdicts folded
/// straight into denials carrying the classifier's own reason.
pub(crate) struct HeadlessSurface;

#[async_trait::async_trait]
impl AgentSurface for HeadlessSurface {
    fn on_event(&self, _event: EngineEvent<'_>) {}

    async fn review(
        &self,
        call: &FunctionToolCall,
        reason: &str,
        _position: (usize, usize),
    ) -> ReviewDecision {
        ReviewDecision::Deny {
            output: super::classify::classifier_denied_output(call, reason),
        }
    }
}
