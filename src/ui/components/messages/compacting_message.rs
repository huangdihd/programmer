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

/// Transient conversation entry shown while `/compact` is summarizing the
/// current context. It disappears when the operation finishes or is cancelled.
pub struct CompactingMessage;

impl CompactingMessage {
    pub fn into_paragraph() -> Paragraph<'static> {
        notice(
            "⧉",
            palette::CYAN,
            palette::MUTED,
            "Compacting context…".to_string(),
        )
    }
}
