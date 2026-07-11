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

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders};
use ratatui_widgets::block::Padding;
use ratatui_widgets::paragraph::{Paragraph, Wrap};

use crate::ui::markdown_theme::palette;

const PAD_LEFT: u16 = 2;
const PAD_RIGHT: u16 = 2;

/// Renders an informational message (command output, status updates) as a
/// distinct, cyan-accented block so it stands out from errors and conversation.
pub struct InfoMessage {
    message: String,
}

impl InfoMessage {
    pub fn new(message: String) -> Self {
        Self { message }
    }

    pub fn into_paragraph(self) -> Paragraph<'static> {
        let cyan = palette::CYAN;
        let body = palette::MUTED;

        let mut lines: Vec<Line<'static>> = vec![Line::from(Span::styled(
            "ℹ Info",
            Style::new().fg(cyan).add_modifier(Modifier::BOLD),
        ))];

        for line in self.message.lines() {
            lines.push(Line::from(Span::styled(
                line.to_string(),
                Style::new().fg(body),
            )));
        }

        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::LEFT)
                    .border_style(Style::new().fg(cyan))
                    .padding(Padding::new(PAD_LEFT, PAD_RIGHT, 0, 0)),
            )
    }
}
