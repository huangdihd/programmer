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
use crate::ui::theme::role;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Paragraph, Widget, Wrap};

const VERSION: &str = env!("CARGO_PKG_VERSION");
pub(crate) const PROJECT_URL: &str = "https://github.com/huangdihd/programmer";

fn key(label: &'static str) -> Span<'static> {
    Span::styled(
        format!("{label:<11}"),
        Style::default()
            .fg(role::HEADING)
            .add_modifier(Modifier::BOLD),
    )
}

fn action(label: &'static str) -> Span<'static> {
    Span::styled(label, Style::default().fg(palette::TEXT))
}

#[derive(Debug, Clone, Default)]
pub struct WelcomeMessage;

impl Widget for &WelcomeMessage {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let content_area = WelcomeMessage::content_area(area);
        Paragraph::new(self.content(area.width))
            .wrap(Wrap { trim: false })
            .render(content_area, buf);
    }
}

impl WelcomeMessage {
    fn content_area(area: Rect) -> Rect {
        let horizontal = if area.width >= 24 { 2 } else { 0 };
        Rect {
            x: area.x.saturating_add(horizontal),
            y: area.y,
            width: area.width.saturating_sub(horizontal.saturating_mul(2)),
            height: area.height,
        }
    }

    fn content(&self, width: u16) -> Text<'static> {
        let mut lines = vec![
            Line::from(vec![
                Span::styled(
                    "PROGRAMMER",
                    Style::default()
                        .fg(role::HEADING)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("  v{VERSION}"), Style::default().fg(palette::FAINT)),
            ]),
            Line::from(vec![
                Span::styled(
                    "AI coding in your terminal",
                    Style::default().fg(palette::TEXT),
                ),
                Span::styled("  ·  Rust / Ratatui", Style::default().fg(palette::MUTED)),
            ]),
            Line::from(""),
        ];

        if width >= 72 {
            lines.extend([
                Line::from(vec![
                    key("Enter"),
                    action("send"),
                    Span::styled("     ", Style::default()),
                    key("Alt+Enter"),
                    action("new line"),
                    Span::styled("     ", Style::default()),
                    key("Ctrl+T"),
                    action("work mode"),
                ]),
                Line::from(vec![
                    key("Ctrl+B"),
                    action("sidebar"),
                    Span::styled("  ", Style::default()),
                    key("Mouse"),
                    action("select / open / scroll"),
                ]),
            ]);
        } else {
            lines.extend([
                Line::from(vec![key("Enter"), action("send")]),
                Line::from(vec![key("Alt+Enter"), action("new line")]),
                Line::from(vec![key("Ctrl+T"), action("work mode")]),
                Line::from(vec![key("Ctrl+B"), action("sidebar")]),
                Line::from(vec![key("Mouse"), action("select / open / scroll")]),
            ]);
        }

        lines.extend([
            Line::from(""),
            Line::from(vec![
                Span::styled("│ ", Style::default().fg(role::WELCOME_DIVIDER)),
                Span::styled(
                    "Initially, computer means a person who computes.",
                    Style::default()
                        .fg(palette::MUTED)
                        .add_modifier(Modifier::ITALIC),
                ),
            ]),
            Line::from(vec![
                Span::styled("│ ", Style::default().fg(role::WELCOME_DIVIDER)),
                Span::styled(
                    "When will we pass programmer to coding agents?",
                    Style::default()
                        .fg(palette::MUTED)
                        .add_modifier(Modifier::ITALIC),
                ),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("/help", Style::default().fg(role::HEADING)),
                Span::styled("  shortcuts    ", Style::default().fg(palette::FAINT)),
                Span::styled(
                    PROJECT_URL,
                    Style::default()
                        .fg(role::FOCUS)
                        .add_modifier(Modifier::UNDERLINED),
                ),
            ]),
        ]);

        Text::from(lines)
    }

    /// Hit-test the actually rendered URL, including wrapping and clipping.
    pub(crate) fn link_at(&self, width: u16, x: u16, y: u16) -> Option<&'static str> {
        let height = self.line_count(width);
        if x >= width || y >= height || usize::from(width) * usize::from(height) > 1_000_000 {
            return None;
        }
        let mut buffer = Buffer::empty(Rect::new(0, 0, width, height));
        self.render(buffer.area, &mut buffer);
        let cell = &buffer[(x, y)];
        (cell.fg == role::FOCUS
            && cell.modifier.contains(Modifier::UNDERLINED)
            && !cell.symbol().trim().is_empty())
        .then_some(PROJECT_URL)
    }

    pub fn line_count(&self, total_width: u16) -> u16 {
        let area = Self::content_area(Rect::new(0, 0, total_width, u16::MAX));
        Paragraph::new(self.content(total_width))
            .wrap(Wrap { trim: false })
            .line_count(area.width) as u16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn welcome_keeps_the_project_quote() {
        let welcome = WelcomeMessage;
        let text = welcome
            .content(100)
            .lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(text.contains("Initially, computer means a person who computes."));
        assert!(text.contains("When will we pass programmer to coding agents?"));
    }

    #[test]
    fn project_url_hit_test_matches_visible_cells_at_different_widths() {
        for width in [40, 100, 200] {
            let welcome = WelcomeMessage;
            let height = welcome.line_count(width);
            let mut buffer = Buffer::empty(Rect::new(0, 0, width, height));
            (&welcome).render(buffer.area, &mut buffer);
            let mut hits = 0;
            for y in 0..height {
                for x in 0..width {
                    let cell = &buffer[(x, y)];
                    let expected = cell.modifier.contains(Modifier::UNDERLINED)
                        && !cell.symbol().trim().is_empty();
                    assert_eq!(welcome.link_at(width, x, y).is_some(), expected);
                    if expected {
                        assert_eq!(welcome.link_at(width, x, y), Some(PROJECT_URL));
                        hits += 1;
                    }
                }
            }
            assert!(hits > 0);
            assert_eq!(welcome.link_at(width, width, 0), None);
            assert_eq!(welcome.link_at(width, 0, height), None);
        }
    }
}
