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

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui_widgets::paragraph::{Paragraph, Wrap};

use crate::ui::markdown_theme::palette;

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
        let total = self.input_tokens + self.output_tokens;

        Paragraph::new(Line::from(vec![
            Span::styled("↳  ", Style::new().fg(palette::FAINT)),
            Span::styled(
                self.input_tokens.to_string(),
                Style::new().fg(palette::CYAN),
            ),
            Span::styled(" input", Style::new().fg(palette::MUTED)),
            Span::styled("  ·  ", Style::new().fg(palette::FAINT)),
            Span::styled(
                self.output_tokens.to_string(),
                Style::new().fg(palette::PURPLE),
            ),
            Span::styled(" output", Style::new().fg(palette::MUTED)),
            Span::styled("  ·  ", Style::new().fg(palette::FAINT)),
            Span::styled(total.to_string(), Style::new().fg(palette::TEXT)),
            Span::styled(" total tokens", Style::new().fg(palette::MUTED)),
        ]))
        .wrap(Wrap { trim: false })
    }
}

#[cfg(test)]
mod tests {
    use super::UsageMessage;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::widgets::Widget;

    #[test]
    fn usage_is_a_compact_borderless_summary() {
        let area = Rect::new(0, 0, 60, 1);
        let mut buffer = Buffer::empty(area);
        UsageMessage::new(13, 7)
            .into_paragraph()
            .render(area, &mut buffer);

        let rendered = (0..area.width)
            .filter_map(|x| buffer.cell((x, 0)))
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(rendered.starts_with("↳  13 input  ·  7 output  ·  20 total tokens"));
        assert!(!rendered.contains('│'));
    }
}
