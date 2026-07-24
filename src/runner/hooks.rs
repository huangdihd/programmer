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

//! Pluggable turn hooks: concepts the runner runs *around* a tool batch.
//!
//! Post-edit diagnostics used to be hard-wired into the turn loop. It is now one
//! of an open-ended list of [`TurnHook`]s the runner drives — diagnostics, the
//! PROGRAMMER.md reminder, and any future lint / test / custom check are each an
//! independent hook the front-end attaches. A hook fires both *before* and
//! *after* a tool batch (each phase defaults to a no-op, so an implementation
//! overrides only what it needs) and self-gates on the [`BatchSummary`] rather
//! than the runner deciding for it.
//!
//! A hook communicates back in two ways, weakest first:
//!  * Return `Some(text)` and the runner injects that feedback into the
//!    conversation (folded into the batch's edit output, as before).
//!  * For richer changes, mutate `ctx.conversation` directly and return `None`.

use super::{AgentSurface, DiagnosticsState, RunnerEvent, RunnerPhase};
use crate::conversation::Conversation;
use std::sync::{Arc, Mutex};

/// A summary of the tool batch a hook is reacting to.
pub(crate) struct BatchSummary {
    /// The tool names in this batch, in call order.
    #[allow(dead_code)]
    pub tool_names: Vec<String>,
    /// True when a file-editing tool (`write_file`/`edit_file`) ran successfully
    /// in this batch. Always false for the before-batch phase, where the tools
    /// have not run yet.
    pub edited: bool,
}

/// Which side of the tool batch a hook is being invoked on.
#[derive(Clone, Copy)]
pub(crate) enum HookPhase {
    /// After classification, before the approved calls execute.
    Before,
    /// After the calls executed and their outputs were committed.
    After,
}

/// Everything a hook is handed for one invocation: the shared conversation (read
/// history, or mutate it directly for advanced use), the surface (to emit phase
/// events), and the batch summary it self-gates on.
pub(crate) struct HookContext<'a> {
    /// The shared conversation. The built-in hooks report back via the returned
    /// feedback string, but this handle lets an advanced hook read history or
    /// mutate the conversation directly (the "can modify" affordance).
    #[allow(dead_code)]
    pub conversation: &'a Mutex<Conversation>,
    pub surface: &'a dyn AgentSurface,
    pub batch: &'a BatchSummary,
}

/// A pluggable concept the runner runs around a tool batch. Attach any number to
/// a [`TurnRunner`](super::TurnRunner); each fires on the phase(s) it overrides
/// and self-gates on `ctx.batch`.
#[async_trait::async_trait]
pub(crate) trait TurnHook: Send + Sync {
    /// A short identifier, for debugging and status display.
    #[allow(dead_code)]
    fn name(&self) -> &str;

    /// Runs after classification but before the approved calls execute. Return
    /// feedback to inject beforehand, or `None`.
    async fn before_tool_batch(&self, _ctx: &HookContext<'_>) -> Option<String> {
        None
    }

    /// Runs after the calls executed and their outputs were committed. Return
    /// feedback to inject, or `None`.
    async fn after_tool_batch(&self, _ctx: &HookContext<'_>) -> Option<String> {
        None
    }
}

/// Post-edit diagnostics. After an editing batch, run the configured checkers,
/// diff against the running baseline, and report the delta so the model reacts
/// to problems it just introduced. The baseline lives behind an
/// `Arc<Mutex<DiagnosticsState>>` shared with the front-end (the UI renders it,
/// and it survives across the per-turn runners).
pub(crate) struct DiagnosticsHook {
    pub state: Arc<Mutex<DiagnosticsState>>,
}

#[async_trait::async_trait]
impl TurnHook for DiagnosticsHook {
    fn name(&self) -> &str {
        "diagnostics"
    }

    async fn after_tool_batch(&self, ctx: &HookContext<'_>) -> Option<String> {
        if !ctx.batch.edited {
            return None;
        }
        if !std::path::Path::new(crate::diagnostics::PROFILE_PATH).exists() {
            return None;
        }
        ctx.surface
            .on_event(RunnerEvent::Phase(RunnerPhase::Checking));
        let cwd =
            std::env::current_dir().unwrap_or_else(|_| std::path::Path::new(".").to_path_buf());
        // No state lock held across this await — see the run_turn comment.
        let snapshot = crate::diagnostics::collect(&cwd).await.unwrap_or_default();

        let mut parts: Vec<String> = Vec::new();
        let mut st = self.state.lock().unwrap();
        match &st.baseline {
            Some(old) => {
                if let Some(summary) =
                    crate::diagnostics::diff(old, &snapshot.diagnostics).summary()
                {
                    parts.push(summary);
                }
            }
            None => {
                if !snapshot.diagnostics.is_empty() {
                    parts.push(format!(
                        "Diagnostics baseline established: {} problem(s) currently \
                         in the project. Future edits will report changes relative \
                         to this.",
                        snapshot.diagnostics.len()
                    ));
                }
            }
        }
        for e in &snapshot.errors {
            parts.push(format!("Diagnostics checker error: {e}"));
        }
        st.baseline = Some(snapshot.diagnostics);
        drop(st);

        if parts.is_empty() {
            None
        } else {
            Some(parts.join("\n\n"))
        }
    }
}

/// Periodic PROGRAMMER.md refresh reminder. Every `every` editing batches — when
/// the project actually has a `PROGRAMMER.md` — nudge the model to keep the
/// overview current. Shares the edit-batch counter with the diagnostics baseline
/// state so both reset together when diagnostics is reconfigured.
pub(crate) struct OverviewReminderHook {
    pub state: Arc<Mutex<DiagnosticsState>>,
    pub every: usize,
}

#[async_trait::async_trait]
impl TurnHook for OverviewReminderHook {
    fn name(&self) -> &str {
        "overview-reminder"
    }

    async fn after_tool_batch(&self, ctx: &HookContext<'_>) -> Option<String> {
        if !ctx.batch.edited {
            return None;
        }
        // Count every editing batch, regardless of whether a reminder is due, so
        // the cadence matches wall-clock editing activity.
        let count = {
            let mut st = self.state.lock().unwrap();
            st.mutating_turns += 1;
            st.mutating_turns
        };
        let due = self.every != 0
            && count.is_multiple_of(self.every)
            && std::path::Path::new("PROGRAMMER.md").exists();
        if due {
            Some(crate::prompts::OVERVIEW_REMINDER.to_string())
        } else {
            None
        }
    }
}
