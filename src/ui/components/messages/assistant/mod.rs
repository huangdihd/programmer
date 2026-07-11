// Copyright (C) 2025 huangdihd
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

pub mod text;
pub mod reasoning;
pub mod unsupported;
pub mod tool_call;

use ratatui::style::{Modifier, Style};

use crate::ui::markdown_theme::palette;

/// Dim, italic style shared by the non-text assistant items (the reasoning label
/// and the unsupported-item placeholder).
pub(crate) fn muted_style() -> Style {
    Style::new()
        .fg(palette::MUTED)
        .add_modifier(Modifier::DIM | Modifier::ITALIC)
}

/// White style for expanded detail content (reasoning, tool calls, results).
pub(crate) fn detail_style() -> Style {
    Style::new().fg(palette::TEXT)
}
