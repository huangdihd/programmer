// Copyright (C) 2026 huangdihd
// SPDX-License-Identifier: GPL-3.0-or-later

/// The active work phase of a turn. Exactly one is in effect at a time; the
/// old design tracked these as separate booleans that could, in principle,
/// contradict each other. "Thinking" is intentionally absent — it is derived
/// from [`ActivePhase::None`] plus an in-flight `receiving_response`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ActivePhase {
    /// Neither outputting, calling tools, nor classifying. When a response is
    /// still streaming this reads as "Thinking"; otherwise the turn is idle.
    #[default]
    None,
    /// The model is streaming a normal text message.
    Outputting,
    /// The model is streaming tool-call arguments.
    CreatingToolCall,
    /// Tool calls are executing in the background.
    ToolRunning,
    /// The Auto-mode LLM classifier is deciding tool-call approvals.
    Classifying,
    /// The memory model is selecting relevant memories.
    Associating,
    /// Diagnostics checkers are running after an edit.
    Checking,
    /// `/compact` is summarizing the conversation to shrink the context.
    Compacting,
    /// The user pressed Esc; the runner has been signalled to cancel but
    /// hasn't finished yet. The UI stays in this state until the matching
    /// `TurnFinished` event arrives.
    Cancelling,
}
