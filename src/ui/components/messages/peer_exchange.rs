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

use crate::ui::markdown_theme::palette;
use ratatui::style::Style;
use ratatui::text::{Line, Span, Text};
use ratatui_widgets::paragraph::{Paragraph, Wrap};

/// UI-only peer exchange, deliberately distinct from tool execution/approval.
pub struct PeerExchangeMessage<'a> {
    from: &'a str,
    question: &'a str,
    answer: Option<&'a str>,
    expanded: bool,
}

impl<'a> PeerExchangeMessage<'a> {
    pub fn new(from: &'a str, question: &'a str, answer: Option<&'a str>) -> Self {
        Self {
            from,
            question,
            answer,
            expanded: false,
        }
    }

    pub fn expanded(mut self, expanded: bool) -> Self {
        self.expanded = expanded;
        self
    }

    pub fn into_paragraph(self) -> Paragraph<'static> {
        let accent = Style::new().fg(palette::PURPLE);
        let muted = Style::new().fg(palette::MUTED);
        let status = if self.answer.is_some() {
            "answered"
        } else {
            "pending"
        };
        let mut lines = vec![Line::from(vec![
            Span::styled(if self.expanded { "▾ " } else { "▸ " }, accent),
            Span::styled(format!("↔ {}", short_source(self.from)), accent),
            Span::styled(format!(" · {status}"), muted),
        ])];
        if self.expanded {
            for (label, text) in [
                ("Source", self.from),
                ("Question", self.question),
                ("Answer", self.answer.unwrap_or("Pending reply…")),
            ] {
                lines.push(Line::from(Span::styled(format!("  {label}"), accent)));
                lines.extend(text.lines().map(|line| {
                    Line::from(Span::styled(
                        format!("  {line}"),
                        Style::new().fg(palette::TEXT),
                    ))
                }));
            }
        }
        lines.push(Line::default());
        let paragraph = Paragraph::new(Text::from(lines));
        if self.expanded {
            paragraph.wrap(Wrap { trim: false })
        } else {
            // Never wrap the collapsed header into a second row, even in a narrow pane.
            paragraph
        }
    }
}

pub(super) fn short_source(from: &str) -> String {
    let first = from.lines().next().unwrap_or_default().trim();
    if let Ok(id) = uuid::Uuid::parse_str(first) {
        return id.to_string()[..8].to_string();
    }
    let name = first
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(first);
    let mut short: String = name.chars().take(24).collect();
    if name.chars().count() > 24 {
        short.push('…');
    }
    short
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};

    #[test]
    fn collapsed_exchange_is_one_purple_row_even_in_a_narrow_pane() {
        let area = Rect::new(0, 0, 12, 4);
        let mut buffer = Buffer::empty(area);
        PeerExchangeMessage::new("/workspace/long-project-name", "hidden question", None)
            .into_paragraph()
            .render(area, &mut buffer);
        assert_eq!(buffer[(0, 0)].symbol(), "▸");
        assert_eq!(buffer[(2, 0)].symbol(), "↔");
        assert_eq!(buffer[(0, 0)].fg, palette::PURPLE);
        for y in 1..area.height {
            for x in 0..area.width {
                assert_eq!(buffer[(x, y)].symbol(), " ");
            }
        }
    }

    #[test]
    fn expanded_exchange_shows_down_disclosure() {
        let area = Rect::new(0, 0, 40, 8);
        let mut buffer = Buffer::empty(area);
        PeerExchangeMessage::new("peer", "question", Some("answer"))
            .expanded(true)
            .into_paragraph()
            .render(area, &mut buffer);
        assert_eq!(buffer[(0, 0)].symbol(), "▾");
        assert_eq!(buffer[(2, 0)].symbol(), "↔");
    }

    #[test]
    fn short_source_is_unicode_safe_and_abbreviates_session_ids() {
        assert_eq!(
            short_source("e172f842-2450-4ac5-88ec-691aef419bfa"),
            "e172f842"
        );
        assert_eq!(short_source("/workspace/项目\nfull source details"), "项目");
        assert_eq!(
            short_source(&"问".repeat(30)),
            format!("{}…", "问".repeat(24))
        );
    }
}
