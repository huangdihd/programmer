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

//! Project-wide tunable constants, gathered in one place so the values that
//! shape runtime behaviour are easy to find and adjust.

/// Maximum characters of a single tool's output kept before truncation. The
/// rest is discarded and a truncation notice is appended so the model knows the
/// output was cut short.
pub(crate) const MAX_OUTPUT_LENGTH: usize = 8000;

/// Maximum number of Auto-mode classifier LLM requests in flight at once.
pub(crate) const MAX_CONCURRENT_CLASSIFICATIONS: usize = 4;

/// Maximum number of read-only tool calls executed concurrently within a batch.
/// Writes are never run concurrently; see `spawn_run`.
pub(crate) const MAX_CONCURRENT_READ_TOOLS: usize = 8;

/// Maximum number of times a streaming request is retried on a retryable
/// connection error before giving up.
pub(crate) const MAX_STREAM_RETRIES: u32 = 10;

/// Maximum number of model↔tool round-trips the headless engine runs for a
/// single turn before giving up, so a tool-calling loop can't spin forever.
pub(crate) const ENGINE_MAX_ITERATIONS: usize = 40;

/// The frequency at which tick events are emitted.
pub(crate) const TICK_FPS: f64 = 30.0;

/// How many file-editing turns pass between reminders to refresh PROGRAMMER.md.
pub(crate) const OVERVIEW_REMINDER_EVERY: usize = 5;

/// Character budget for each user/assistant message in the classifier's *light*
/// context (the fast yes/no path).
pub(crate) const CLASSIFIER_LIGHT_MSG_CHARS: usize = 600;

/// Character budget for a tool call's arguments in the classifier's *full*
/// context.
pub(crate) const CLASSIFIER_CALL_ARGS_CHARS: usize = 1000;

/// Character budget for an `ask_user` answer in the classifier's *full*
/// context.
pub(crate) const CLASSIFIER_ASK_OUTPUT_CHARS: usize = 500;
