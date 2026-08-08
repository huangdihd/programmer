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

use ratatui_widgets::paragraph::Paragraph;

use super::notice_message::notice;
use crate::ui::markdown_theme::palette;

/// Renders command output and status updates as a lightweight inline notice.
pub struct InfoMessage {
    message: String,
}

impl InfoMessage {
    pub fn new(message: String) -> Self {
        Self { message }
    }

    pub fn into_paragraph(self) -> Paragraph<'static> {
        notice("ℹ", palette::CYAN, palette::MUTED, self.message)
    }
}
