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

use crate::classifier::WorkMode;
use crate::ui::components::status_bar::status_bar::StatusBar;

/// Bottom bar: status indicator on the left, work mode, model name in
/// the middle, copyright on the right.
#[derive(Debug)]
pub struct Footer {
    pub status: StatusBar,
    pub current_model: String,
    pub work_mode: WorkMode,
    /// Whether the project has an LSP checker configured, so the LSP block shows
    /// even before a server has started.
    pub lsp_configured: bool,
    /// Comma-separated names of active skills for display.
    pub active_skills: String,
}

impl Footer {
    pub fn new() -> Self {
        Self {
            status: StatusBar::new(),
            current_model: String::new(),
            work_mode: WorkMode::default(),
            lsp_configured: false,
            active_skills: String::new(),
        }
    }

}
