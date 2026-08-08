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

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span, Text};
use ratatui_widgets::paragraph::{Paragraph, Wrap};

/// Builds a lightweight status message without card borders or background
/// chrome. Continuation lines align with the first line's content.
pub(super) fn notice(
    icon: &'static str,
    accent: Color,
    body: Color,
    message: String,
) -> Paragraph<'static> {
    let mut message_lines = message.lines();
    let first = message_lines.next().unwrap_or_default();
    let mut lines = vec![Line::from(vec![
        Span::styled(icon, Style::new().fg(accent)),
        Span::raw("  "),
        Span::styled(first.to_string(), Style::new().fg(body)),
    ])];

    lines.extend(message_lines.map(|line| {
        Line::from(vec![
            Span::raw("   "),
            Span::styled(line.to_string(), Style::new().fg(body)),
        ])
    }));

    Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false })
}

#[cfg(test)]
mod tests {
    use super::notice;
    use crate::ui::markdown_theme::palette;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::widgets::Widget;

    #[test]
    fn notice_has_no_card_border_and_indents_continuations() {
        let area = Rect::new(0, 0, 40, 2);
        let mut buffer = Buffer::empty(area);
        notice(
            "ℹ",
            palette::CYAN,
            palette::MUTED,
            "first line\nsecond line".to_string(),
        )
        .render(area, &mut buffer);

        let rendered = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .filter_map(|x| buffer.cell((x, y)))
                    .map(|cell| cell.symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.starts_with("ℹ  first line"), "{rendered}");
        assert!(rendered.contains("\n   second line"), "{rendered}");
        assert!(!rendered.contains('│'), "{rendered}");
    }
}
