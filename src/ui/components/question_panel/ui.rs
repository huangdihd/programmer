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

use super::{Mode, QuestionPanel};
use crate::tools::ask_user::QuestionKind;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget, Wrap};

impl QuestionPanel {
    /// The number of rows this panel needs (excluding borders).
    pub fn needed_height(&self) -> u16 {
        let question_lines = (self.question.text.len().max(1) as u16 / 40).max(1);
        let choice_count = match &self.question.kind {
            QuestionKind::Choice { options, .. } => options.len() as u16,
            QuestionKind::Text { .. } => 0,
        };
        // question + blank + options + blank + hint
        question_lines + 1 + choice_count + 1 + 1
    }

    /// Render the question panel into the given area (bottom of screen).
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(Color::Cyan));
        let inner = block.inner(area);
        block.render(area, buf);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .split(inner);

        // Question text.
        Paragraph::new(self.question.text.as_str())
            .style(Style::default().fg(Color::White).bold())
            .wrap(Wrap { trim: true })
            .render(chunks[0], buf);

        // Choices + input + hint bar in the bottom row.
        match (&self.question.kind, &self.mode) {
            (QuestionKind::Choice { options, .. }, Mode::Choice { selected }) => {
                // Render options inline horizontally when they fit, or stacked.
                let options_area = Rect {
                    y: inner.bottom().saturating_sub(options.len() as u16 + 1),
                    height: options.len() as u16 + 1,
                    ..inner
                };
                let lines: Vec<Line> = options
                    .iter()
                    .enumerate()
                    .map(|(i, opt)| {
                        let is_sel = i == *selected;
                        let marker = if is_sel { "❯" } else { " " };
                        let style = if is_sel {
                            Style::default()
                                .fg(Color::Black)
                                .bg(Color::Cyan)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(Color::Gray)
                        };
                        Line::from(vec![
                            Span::styled("  ", Style::default()),
                            Span::styled(
                                format!("{marker} {opt}"),
                                style,
                            ),
                        ])
                    })
                    .collect();
                Paragraph::new(lines).render(options_area, buf);

                // Hint in the very last row.
                let hint = Line::from(vec![
                    Span::styled("↑↓", Style::default().fg(Color::Cyan).bold()),
                    Span::styled(" select  ", Style::default().fg(Color::DarkGray)),
                    Span::styled("Enter", Style::default().fg(Color::Green).bold()),
                    Span::styled(" confirm", Style::default().fg(Color::DarkGray)),
                ]);
                Paragraph::new(hint).render(
                    Rect {
                        y: inner.bottom().saturating_sub(1),
                        height: 1,
                        ..inner
                    },
                    buf,
                );
            }
            (QuestionKind::Choice { .. }, Mode::Text { input }) => {
                // "Other…" free text on one line.
                let input_line = Line::from(vec![
                    Span::styled("  ❯ ", Style::default().fg(Color::Cyan)),
                    Span::styled(input.clone(), Style::default().fg(Color::White)),
                    Span::styled("▏", Style::default().fg(Color::Cyan)),
                ]);
                let input_area = Rect {
                    y: inner.bottom().saturating_sub(2),
                    height: 1,
                    ..inner
                };
                Paragraph::new(input_line).render(input_area, buf);

                let hint = Line::from(vec![
                    Span::styled("Enter", Style::default().fg(Color::Green).bold()),
                    Span::styled(" submit  ", Style::default().fg(Color::DarkGray)),
                    Span::styled("Esc", Style::default().fg(Color::Cyan).bold()),
                    Span::styled(" back", Style::default().fg(Color::DarkGray)),
                ]);
                Paragraph::new(hint).render(
                    Rect {
                        y: inner.bottom().saturating_sub(1),
                        height: 1,
                        ..inner
                    },
                    buf,
                );
            }
            (QuestionKind::Text { .. }, Mode::Text { input }) => {
                let input_line = Line::from(vec![
                    Span::styled("  ❯ ", Style::default().fg(Color::Cyan)),
                    Span::styled(input.clone(), Style::default().fg(Color::White)),
                    Span::styled("▏", Style::default().fg(Color::Cyan)),
                ]);
                let input_area = Rect {
                    y: inner.bottom().saturating_sub(2),
                    height: 1,
                    ..inner
                };
                Paragraph::new(input_line).render(input_area, buf);

                let hint = Line::from(vec![
                    Span::styled("Enter", Style::default().fg(Color::Green).bold()),
                    Span::styled(" submit", Style::default().fg(Color::DarkGray)),
                ]);
                Paragraph::new(hint).render(
                    Rect {
                        y: inner.bottom().saturating_sub(1),
                        height: 1,
                        ..inner
                    },
                    buf,
                );
            }
            _ => {}
        }
    }
}
