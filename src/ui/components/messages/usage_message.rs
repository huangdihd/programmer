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

/// Renders a token-usage summary after each response.
pub struct UsageMessage {
    input_tokens: u32,
    output_tokens: u32,
}

impl UsageMessage {
    pub fn new(input_tokens: u32, output_tokens: u32) -> Self {
        Self {
            input_tokens,
            output_tokens,
        }
    }

    pub fn into_paragraph(self) -> Paragraph<'static> {
        let muted = palette::MUTED;
        let total = self.input_tokens + self.output_tokens;

        Paragraph::new(Text::from(vec![Line::from(Span::styled(
            format!(
                "↑ {} input  ↓ {} output  Σ {} tokens",
                self.input_tokens, self.output_tokens, total
            ),
            Style::new().fg(muted).add_modifier(Modifier::ITALIC),
        ))]))
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .borders(Borders::LEFT)
                .border_style(Style::new().fg(muted))
                .padding(Padding::new(PAD_LEFT, PAD_RIGHT, 0, 0)),
        )
    }
}
