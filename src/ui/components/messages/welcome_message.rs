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
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Widget, Wrap};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const COLUMN_LAYOUT_MIN_WIDTH: u16 = 72;
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

fn heading(label: &'static str) -> Line<'static> {
    Line::from(Span::styled(
        label,
        Style::default()
            .fg(role::HEADING)
            .add_modifier(Modifier::BOLD),
    ))
}

fn paragraph_height(text: Text<'static>, width: u16, trim: bool) -> usize {
    if width == 0 {
        return 0;
    }
    Paragraph::new(text).wrap(Wrap { trim }).line_count(width)
}

#[derive(Debug, Clone, Default)]
pub struct WelcomeMessage;

impl Widget for &WelcomeMessage {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        if area.is_empty() {
            return;
        }

        let block = self.block();
        let inner = block.inner(area);
        block.render(area, buffer);

        if WelcomeMessage::uses_columns(area.width) {
            let (left, right) = WelcomeMessage::split_columns(inner);
            self.render_left(left, buffer);
            self.render_right(right, buffer);
        } else {
            Paragraph::new(self.compact_content())
                .wrap(Wrap { trim: false })
                .render(WelcomeMessage::compact_area(inner), buffer);
        }
    }
}

impl WelcomeMessage {
    fn block(&self) -> Block<'static> {
        Block::default()
            .title(format!(" PROGRAMMER · v{VERSION} "))
            .title_style(
                Style::default()
                    .fg(role::HEADING)
                    .add_modifier(Modifier::BOLD),
            )
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(role::BORDER))
    }

    fn uses_columns(width: u16) -> bool {
        width >= COLUMN_LAYOUT_MIN_WIDTH
    }

    fn split_columns(area: Rect) -> (Rect, Rect) {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(38), Constraint::Percentage(62)])
            .split(area);
        (columns[0], columns[1])
    }

    fn split_right(area: Rect) -> (Rect, Rect) {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(2), Constraint::Min(1)])
            .split(area);
        (columns[0], columns[1])
    }

    fn compact_area(area: Rect) -> Rect {
        let horizontal = u16::from(area.width >= 24);
        Rect {
            x: area.x.saturating_add(horizontal),
            y: area.y,
            width: area.width.saturating_sub(horizontal.saturating_mul(2)),
            height: area.height,
        }
    }

    fn left_content(&self) -> Text<'static> {
        Text::from(vec![
            Line::from(""),
            Line::from(Span::styled(" ▄▄▄▄▄▄ ", Style::default().fg(role::LOGO))),
            Line::from(Span::styled(" █ ▀▀ █ ", Style::default().fg(role::LOGO))),
            Line::from(Span::styled(" ▀▄▄▄▄▀ ", Style::default().fg(role::LOGO))),
            Line::from(""),
            Line::from(Span::styled(
                "AI coding in your terminal",
                Style::default()
                    .fg(palette::TEXT)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "Rust / Ratatui",
                Style::default().fg(palette::MUTED),
            )),
            Line::from(""),
            Line::from(vec![
                Span::styled("/help", Style::default().fg(role::HEADING)),
                Span::styled("  shortcuts", Style::default().fg(palette::FAINT)),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                PROJECT_URL,
                Style::default()
                    .fg(role::FOCUS)
                    .add_modifier(Modifier::UNDERLINED),
            )),
        ])
    }

    fn right_content(&self) -> Text<'static> {
        Text::from(vec![
            Line::from(""),
            heading("Keyboard shortcuts"),
            Line::from(vec![key("Enter"), action("send")]),
            Line::from(vec![key("Alt+Enter"), action("new line")]),
            Line::from(vec![key("Ctrl+T"), action("work mode")]),
            Line::from(vec![key("Ctrl+B"), action("sidebar")]),
            Line::from(vec![key("Mouse"), action("select / open / scroll")]),
            Line::from(""),
            heading("Quote"),
            Line::from(Span::styled(
                "Initially, computer means a person who computes.",
                Style::default()
                    .fg(palette::MUTED)
                    .add_modifier(Modifier::ITALIC),
            )),
            Line::from(Span::styled(
                "When will we pass programmer to coding agents?",
                Style::default()
                    .fg(palette::MUTED)
                    .add_modifier(Modifier::ITALIC),
            )),
        ])
    }

    fn compact_content(&self) -> Text<'static> {
        Text::from(vec![
            Line::from(Span::styled(
                "AI coding in your terminal  ·  Rust / Ratatui",
                Style::default().fg(palette::TEXT),
            )),
            Line::from(""),
            Line::from(vec![key("Enter"), action("send")]),
            Line::from(vec![key("Alt+Enter"), action("new line")]),
            Line::from(vec![key("Ctrl+T"), action("work mode")]),
            Line::from(vec![key("Ctrl+B"), action("sidebar")]),
            Line::from(vec![key("Mouse"), action("select / open / scroll")]),
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
        ])
    }

    fn render_left(&self, area: Rect, buffer: &mut Buffer) {
        Paragraph::new(self.left_content())
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true })
            .render(area, buffer);
    }

    fn render_right(&self, area: Rect, buffer: &mut Buffer) {
        let (divider, content) = Self::split_right(area);
        Block::default()
            .borders(Borders::LEFT)
            .border_style(Style::default().fg(role::WELCOME_DIVIDER))
            .render(divider, buffer);
        Paragraph::new(self.right_content())
            .wrap(Wrap { trim: true })
            .render(content, buffer);
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
        let area = Rect::new(0, 0, total_width, u16::MAX);
        let inner = self.block().inner(area);
        let content_height = if Self::uses_columns(total_width) {
            let (left, right) = Self::split_columns(inner);
            let (_, right_content) = Self::split_right(right);
            paragraph_height(self.left_content(), left.width, true).max(paragraph_height(
                self.right_content(),
                right_content.width,
                true,
            ))
        } else {
            paragraph_height(
                self.compact_content(),
                Self::compact_area(inner).width,
                false,
            )
        };
        content_height.saturating_add(2).min(usize::from(u16::MAX)) as u16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(width: u16) -> (u16, Buffer) {
        let welcome = WelcomeMessage;
        let height = welcome.line_count(width);
        let mut buffer = Buffer::empty(Rect::new(0, 0, width, height));
        (&welcome).render(buffer.area, &mut buffer);
        (height, buffer)
    }

    fn rendered_text(buffer: &Buffer) -> String {
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn wide_layout_restores_the_rounded_two_column_card() {
        let (height, buffer) = render(120);
        assert_eq!(buffer[(0, 0)].symbol(), "╭");
        assert_eq!(buffer[(119, 0)].symbol(), "╮");
        assert_eq!(buffer[(0, height - 1)].symbol(), "╰");
        assert_eq!(buffer[(119, height - 1)].symbol(), "╯");

        let inner = WelcomeMessage.block().inner(buffer.area);
        let (_, right) = WelcomeMessage::split_columns(inner);
        assert_eq!(buffer[(right.x, right.y)].symbol(), "│");

        let text = rendered_text(&buffer);
        assert!(text.contains("AI coding in your terminal"));
        assert!(text.contains("Keyboard shortcuts"));
        assert!(text.contains("Initially, computer means a person who computes."));
        assert!(text.contains("When will we pass programmer to coding agents?"));
    }

    #[test]
    fn narrow_layout_keeps_the_card_and_stacks_the_content() {
        let (height, buffer) = render(COLUMN_LAYOUT_MIN_WIDTH - 1);
        assert_eq!(buffer[(0, 0)].symbol(), "╭");
        assert_eq!(buffer[(0, height - 1)].symbol(), "╰");
        let text = rendered_text(&buffer);
        assert!(text.contains("AI coding in your terminal"));
        assert!(text.contains("select / open / scroll"));
        assert!(!text.contains("Keyboard shortcuts"));
    }

    #[test]
    fn project_url_hit_test_matches_visible_cells_across_layouts() {
        for width in [
            24,
            40,
            COLUMN_LAYOUT_MIN_WIDTH - 1,
            COLUMN_LAYOUT_MIN_WIDTH,
            100,
            200,
        ] {
            let welcome = WelcomeMessage;
            let height = welcome.line_count(width);
            let mut buffer = Buffer::empty(Rect::new(0, 0, width, height));
            (&welcome).render(buffer.area, &mut buffer);
            let mut hits = 0;
            for y in 0..height {
                for x in 0..width {
                    let cell = &buffer[(x, y)];
                    let expected = cell.fg == role::FOCUS
                        && cell.modifier.contains(Modifier::UNDERLINED)
                        && !cell.symbol().trim().is_empty();
                    assert_eq!(welcome.link_at(width, x, y).is_some(), expected);
                    if expected {
                        assert_eq!(welcome.link_at(width, x, y), Some(PROJECT_URL));
                        hits += 1;
                    }
                }
            }
            assert!(hits > 0, "project URL was not visible at width {width}");
            assert_eq!(welcome.link_at(width, width, 0), None);
            assert_eq!(welcome.link_at(width, 0, height), None);
        }
    }
}
