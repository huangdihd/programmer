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

use crate::ui::components::status_bar::status_bar::StatusBar;

/// Bottom bar: status indicator on the left, copyright on the right.
#[derive(Debug)]
pub struct Footer {
    pub status: StatusBar,
}

impl Footer {
    pub fn new() -> Self {
        Self {
            status: StatusBar::new(),
        }
    }

    pub fn update(&mut self, is_receiving: bool, is_outputting_message: bool, is_creating_tool_call: bool, is_tool_running: bool) {
        self.status
            .update(is_receiving, is_outputting_message, is_creating_tool_call, is_tool_running);
    }
}
