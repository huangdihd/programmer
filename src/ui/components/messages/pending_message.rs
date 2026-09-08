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
use ratatui::widgets::Borders;
use ratatui_widgets::block::Block;
use ratatui_widgets::paragraph::{Paragraph, Wrap};

use crate::ui::markdown_theme::palette;

pub struct PendingMessage<'a> {
    text: &'a str,
}

impl<'a> PendingMessage<'a> {
    pub fn new(text: &'a str) -> Self {
        Self { text }
    }

    pub fn into_paragraph(self) -> Paragraph<'a> {
        let header = Line::from(vec![
            Span::styled(
                "  QUEUED",
                Style::default()
                    .fg(palette::YELLOW)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("  ·  ", Style::default().fg(palette::FAINT)),
            Span::styled(
                " Esc ",
                Style::default()
                    .fg(palette::CODE_BG)
                    .bg(palette::YELLOW)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" interrupt  ·  ", Style::default().fg(palette::MUTED)),
            Span::styled(
                " ↑ ",
                Style::default()
                    .fg(palette::TEXT)
                    .bg(palette::SURFACE)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" edit", Style::default().fg(palette::MUTED)),
        ]);
        let mut lines = vec![header];
        for (index, line) in self.text.lines().enumerate() {
            lines.push(Line::from(vec![
                Span::styled(
                    if index == 0 { "  ↳  " } else { "     " },
                    Style::default().fg(palette::YELLOW),
                ),
                Span::styled(line, Style::default().fg(palette::TEXT)),
            ]));
        }
        let block = Block::default()
            .borders(Borders::TOP | Borders::BOTTOM)
            .border_style(Style::default().fg(palette::BORDER))
            .style(Style::default().bg(palette::CODE_BG));

        Paragraph::new(Text::from(lines))
            .block(block)
            .wrap(Wrap { trim: false })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::widgets::Widget;

    #[test]
    fn queued_strip_shows_interrupt_hint_and_message() {
        let area = Rect::new(0, 0, 60, 4);
        let mut buffer = Buffer::empty(area);
        let paragraph = PendingMessage::new("continue with the tests").into_paragraph();

        assert_eq!(paragraph.line_count(area.width), area.height as usize);
        paragraph.render(area, &mut buffer);

        let rendered = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("QUEUED"), "{rendered}");
        assert!(rendered.contains("Esc"), "{rendered}");
        assert!(rendered.contains("interrupt"), "{rendered}");
        assert!(rendered.contains("↑  edit"), "{rendered}");
        assert!(rendered.contains("continue with the tests"), "{rendered}");
    }
}
