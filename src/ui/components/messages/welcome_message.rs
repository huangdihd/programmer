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

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Widget, Wrap};

const ACCENT: Color = Color::LightBlue;
const DIM: Color = Color::Gray;
const TEXT: Color = Color::White;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Default)]
pub struct WelcomeMessage;

impl Widget for &WelcomeMessage {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block = self.block();
        let inner = block.inner(area);
        block.render(area, buf);

        let cols = WelcomeMessage::split_columns(inner);

        self.render_left(cols[0], buf);
        self.render_right(cols[1], buf);
    }
}

impl WelcomeMessage {
    pub fn line_count(&self, total_width: u16) -> u16 {
        let dummy = Rect::new(0, 0, total_width, u16::MAX);
        let inner = self.block().inner(dummy);
        let cols = Self::split_columns(inner);

        let left_lines = self.left_content().len() as u16;

        let right_chunks = Self::split_right(cols[1]);
        let right_lines = Paragraph::new(self.right_content())
            .wrap(Wrap { trim: true })
            .line_count(right_chunks[1].width) as u16;

        left_lines.max(right_lines) + 2 // 上下边框
    }

    fn block(&self) -> Block<'static> {
        Block::default()
            .title(format!(" programmer v{} ", VERSION))
            .title_style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(ACCENT))
    }

    fn split_columns(inner: Rect) -> std::rc::Rc<[Rect]> {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(38), Constraint::Percentage(62)])
            .split(inner)
    }

    fn split_right(area: Rect) -> std::rc::Rc<[Rect]> {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(2), Constraint::Min(1)])
            .split(area)
    }

    fn left_content(&self) -> Vec<Line<'static>> {
        vec![
            Line::from(""),
            Line::from(Span::styled(
                "Welcome back!",
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(" ▄▄▄▄▄▄ ", Style::default().fg(ACCENT))),
            Line::from(Span::styled(" █ ▀▀ █ ", Style::default().fg(ACCENT))),
            Line::from(Span::styled(" ▀▄▄▄▄▀ ", Style::default().fg(ACCENT))),
            Line::from(""),
            Line::from(Span::styled(
                "A coding agent written in rust",
                Style::default().fg(DIM),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "© 2025 huangdihd  |  GPL-3.0-or-later  |  No warranty",
                Style::default().fg(DIM),
            )),
        ]
    }

    fn right_content(&self) -> Vec<Line<'static>> {
        vec![
            Line::from(""),
            Line::from(Span::styled(
                "Tips for getting started",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "Type your task and press Enter",
                Style::default().fg(TEXT),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Keyboard shortcuts",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            )),
            Line::from(vec![
                Span::styled("  Enter", Style::default().fg(ACCENT)),
                Span::styled("          Send message", Style::default().fg(TEXT)),
            ]),
            Line::from(vec![
                Span::styled("  Ctrl+Q", Style::default().fg(ACCENT)),
                Span::styled("         Quit", Style::default().fg(TEXT)),
            ]),
            Line::from(vec![
                Span::styled("  Ctrl+C", Style::default().fg(ACCENT)),
                Span::styled("         Quit", Style::default().fg(TEXT)),
            ]),
            Line::from(vec![
                Span::styled("  Scroll", Style::default().fg(ACCENT)),
                Span::styled("         Navigate conversation", Style::default().fg(TEXT)),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                "Quote",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "Initially, computer means a person who computes.",
                Style::default().fg(DIM).add_modifier(Modifier::ITALIC),
            )),
            Line::from(Span::styled(
                "When will we pass programmer to coding agents?",
                Style::default().fg(DIM).add_modifier(Modifier::ITALIC),
            )),
        ]
    }

    fn render_left(&self, area: Rect, buf: &mut Buffer) {
        Paragraph::new(self.left_content())
            .alignment(Alignment::Center)
            .render(area, buf);
    }

    fn render_right(&self, area: Rect, buf: &mut Buffer) {
        let chunks = Self::split_right(area);

        Block::default()
            .borders(Borders::LEFT)
            .border_style(Style::default().fg(DIM))
            .render(chunks[0], buf);

        Paragraph::new(self.right_content())
            .wrap(Wrap { trim: true })
            .render(chunks[1], buf);
    }
}