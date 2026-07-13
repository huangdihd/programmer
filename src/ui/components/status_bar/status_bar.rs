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
    /// The model is generating reasoning tokens (thinking phase).
    Thinking,
    /// The model is outputting a normal text message.
    Outputting,
    /// The model is generating tool call arguments in the current stream.
    CreatingToolCall,
    /// Tool calls returned by the model are executing in the background.
    ToolRunning,
    /// The model called `ask_user` and is waiting for the user's response.
    WaitingAnswer,
    /// Tool calls are queued for approval in Manual mode.
    WaitingApproval,
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

    pub fn update(
        &mut self,
        is_receiving: bool,
        is_outputting_message: bool,
        is_creating_tool_call: bool,
        is_tool_running: bool,
    ) {
        let new_state = if is_tool_running {
            StatusState::ToolRunning
        } else if is_creating_tool_call {
            StatusState::CreatingToolCall
        } else if is_outputting_message {
            StatusState::Outputting
        } else if is_receiving {
            StatusState::Thinking
        } else {
            StatusState::Idle
        };

        if new_state != self.status {
            self.status = new_state;
            self.busy_start = if matches!(new_state, StatusState::Thinking | StatusState::Outputting | StatusState::CreatingToolCall | StatusState::ToolRunning) {
                Some(Instant::now())
            } else {
                None
            };
        }
    }

    pub fn elapsed(&self) -> Option<std::time::Duration> {
        self.busy_start.map(|start| start.elapsed())
    }
}
