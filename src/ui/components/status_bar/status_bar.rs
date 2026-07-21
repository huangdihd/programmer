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

use std::time::Instant;

/// What the agent is currently doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusState {
    /// Waiting for the user to type and send a message.
    Idle,
    /// The request has been sent but no output has streamed back yet.
    Connecting,
    /// Connecting failed transiently; backing off before another attempt.
    Retrying,
    /// The model is generating reasoning tokens (thinking phase).
    Thinking,
    /// The model is outputting a normal text message.
    Outputting,
    /// The model is generating tool call arguments in the current stream.
    CreatingToolCall,
    /// Tool calls returned by the model are executing in the background.
    ToolRunning,
    /// The Auto-mode LLM classifier is deciding whether to approve tool calls.
    Classifying,
    /// Diagnostics checkers are running after an edit.
    Checking,
    /// `/compact` is summarizing the conversation.
    Compacting,
    /// The model called `ask_user` and is waiting for the user's response.
    WaitingAnswer,
    /// Tool calls are queued for approval in Manual mode.
    WaitingApproval,
}

impl StatusState {
    /// Whether this is an active-work phase that runs a live elapsed timer.
    pub fn is_busy(self) -> bool {
        matches!(
            self,
            StatusState::Connecting
                | StatusState::Retrying
                | StatusState::Thinking
                | StatusState::Outputting
                | StatusState::CreatingToolCall
                | StatusState::ToolRunning
                | StatusState::Classifying
                | StatusState::Compacting
        )
    }

    /// Emoji + short label suitable for terminal title bars.
    pub(crate) fn emoji_label(self) -> &'static str {
        match self {
            StatusState::Idle => "\u{25cf} Ready",
            StatusState::Connecting => "\u{25cc} Connecting",
            StatusState::Retrying => "\u{21bb} Retrying",
            StatusState::Thinking => "\u{25cf} Thinking",
            StatusState::Outputting => "\u{25b8} Outputting",
            StatusState::CreatingToolCall => "\u{2692} Creating tool call",
            StatusState::ToolRunning => "\u{26a1} Running tools",
            StatusState::Classifying => "\u{25cd} Evaluating",
            StatusState::Checking => "\u{25c7} Checking diagnostics",
            StatusState::Compacting => "\u{29c9} Compacting",
            StatusState::WaitingAnswer => "? Waiting for answer",
            StatusState::WaitingApproval => "\u{1f6e1} Waiting for approval",
        }
    }
}

#[derive(Debug)]
pub struct StatusBar {
    pub status: StatusState,
    /// Extra context appended after the label, e.g. live MCP progress
    /// ("codegraph: 67% step 2"). Cleared by the owner when stale.
    pub detail: Option<String>,
    /// When the current busy phase began.
    busy_start: Option<Instant>,
}

impl StatusBar {
    pub fn new() -> Self {
        Self {
            status: StatusState::Idle,
            detail: None,
            busy_start: None,
        }
    }

    /// Set the current status, starting the elapsed timer when entering a busy
    /// phase and clearing it otherwise. The caller ([`App::resolve_status`])
    /// owns the precedence logic; this just tracks timing.
    pub fn set(&mut self, new_state: StatusState) {
        if new_state != self.status {
            self.status = new_state;
            self.busy_start = new_state.is_busy().then(Instant::now);
        }
    }

    pub fn elapsed(&self) -> Option<std::time::Duration> {
        self.busy_start.map(|start| start.elapsed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every status variant has a human-readable emoji label.
    #[test]
    fn emoji_label_all_variants_non_empty() {
        let variants = [
            StatusState::Idle,
            StatusState::Connecting,
            StatusState::Retrying,
            StatusState::Thinking,
            StatusState::Outputting,
            StatusState::CreatingToolCall,
            StatusState::ToolRunning,
            StatusState::Classifying,
            StatusState::Checking,
            StatusState::Compacting,
            StatusState::WaitingAnswer,
            StatusState::WaitingApproval,
        ];
        for v in variants {
            let label = v.emoji_label();
            assert!(!label.is_empty(), "{v:?} returned empty label");
        }
    }

    /// Each variant has a visually distinct label so the user can tell
    /// states apart at a glance.
    #[test]
    fn emoji_label_all_distinct() {
        use std::collections::HashSet;
        let variants = [
            StatusState::Idle,
            StatusState::Connecting,
            StatusState::Retrying,
            StatusState::Thinking,
            StatusState::Outputting,
            StatusState::CreatingToolCall,
            StatusState::ToolRunning,
            StatusState::Classifying,
            StatusState::Checking,
            StatusState::Compacting,
            StatusState::WaitingAnswer,
            StatusState::WaitingApproval,
        ];
        let labels: HashSet<&str> = variants.iter().map(|v| v.emoji_label()).collect();
        assert_eq!(
            labels.len(),
            variants.len(),
            "duplicate labels detected"
        );
    }
}
