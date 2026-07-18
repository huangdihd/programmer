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
use async_openai::types::responses::FunctionToolCall;

/// The outcome of asking a surface to review a tool call whose classifier
/// verdict was `Ask`.
pub(crate) enum ReviewDecision {
    /// Run the call. Only an interactive surface (the TUI, or a parent agent
    /// relaying a human decision) produces this; the headless surface never
    /// approves, so it is unconstructed until that wiring lands.
    #[allow(dead_code)]
    Approve,
    /// Block the call; `reason` is fed back to the model as the denial text.
    Deny { reason: String },
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
    /// explanation for why the call was flagged. The surface returns whether the
    /// call may run; a `Deny` carries the reason to feed back to the model.
    async fn review(&self, call: &FunctionToolCall, reason: &str) -> ReviewDecision;
}

/// The surface for non-interactive runs (the `-p` print mode): progress events
/// are dropped, and any `Ask` is denied because there is no one to ask. This
/// preserves the pre-surface headless behaviour, where `Ask` verdicts folded
/// straight into denials carrying the classifier's own reason.
pub(crate) struct HeadlessSurface;

#[async_trait::async_trait]
impl AgentSurface for HeadlessSurface {
    fn on_event(&self, _event: EngineEvent<'_>) {}

    async fn review(&self, _call: &FunctionToolCall, reason: &str) -> ReviewDecision {
        ReviewDecision::Deny {
            reason: reason.to_string(),
        }
    }
}
