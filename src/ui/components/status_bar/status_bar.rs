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
        )
    }
}

#[derive(Debug)]
pub struct StatusBar {
    pub status: StatusState,
    /// When the current busy phase began.
    busy_start: Option<Instant>,
}

impl StatusBar {
    pub fn new() -> Self {
        Self {
            status: StatusState::Idle,
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
