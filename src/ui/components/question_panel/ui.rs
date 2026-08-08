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
    /// The number of rows this panel needs (including the top border).
    pub fn needed_height(&self) -> u16 {
        let question_lines = (self.question.text.len() as u16 / 40 + 1).max(1);
        let content = match &self.question.kind {
            QuestionKind::Choice { options, .. } => {
                (question_lines + options.len() as u16 + 1).max(3)
            }
            QuestionKind::Text => {
                // question + input + hint
                (question_lines + 2).max(3)
            }
        };
        content + 1 // top border
    }

    /// Render the question panel into the given area (bottom of screen).
    /// TextArea::render handles cursor positioning via the buffer.
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(Color::Cyan));
        let inner = block.inner(area);
        block.render(area, buf);

        match (&self.question.kind, &self.mode) {
            (QuestionKind::Choice { options, .. }, Mode::Choice { selected }) => {
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Min(1),
                        Constraint::Length(options.len() as u16),
                        Constraint::Length(1),
                    ])
                    .split(inner);

                Paragraph::new(self.question.text.as_str())
                    .style(Style::default().fg(Color::White).bold())
                    .wrap(Wrap { trim: true })
                    .render(chunks[0], buf);

                let lines: Vec<Line> = options
                    .iter()
                    .enumerate()
                    .map(|(i, opt)| {
                        let is_sel = i == *selected;
                        let marker = if is_sel { "\u{2771}" } else { " " };
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
                            Span::styled(format!("{marker} {opt}"), style),
                        ])
                    })
                    .collect();
                Paragraph::new(lines).render(chunks[1], buf);

                let hint = Line::from(vec![
                    Span::styled("\u{2191}\u{2193}", Style::default().fg(Color::Cyan).bold()),
                    Span::styled(" select  ", Style::default().fg(Color::DarkGray)),
                    Span::styled("Enter", Style::default().fg(Color::Green).bold()),
                    Span::styled(" confirm  ", Style::default().fg(Color::DarkGray)),
                    Span::styled("Esc", Style::default().fg(Color::Cyan).bold()),
                    Span::styled(" cancel", Style::default().fg(Color::DarkGray)),
                ]);
                Paragraph::new(hint).render(chunks[2], buf);
            }
            (QuestionKind::Choice { .. }, Mode::Text { textarea })
            | (QuestionKind::Text, Mode::Text { textarea }) => {
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Min(1),
                        Constraint::Length(1),
                        Constraint::Length(1),
                    ])
                    .split(inner);

                Paragraph::new(self.question.text.as_str())
                    .style(Style::default().fg(Color::White).bold())
                    .wrap(Wrap { trim: true })
                    .render(chunks[0], buf);

                textarea.render(chunks[1], buf);

                let hint = Line::from(vec![
                    Span::styled("Enter", Style::default().fg(Color::Green).bold()),
                    Span::styled(" submit  ", Style::default().fg(Color::DarkGray)),
                    Span::styled("Esc", Style::default().fg(Color::Cyan).bold()),
                    Span::styled(" cancel", Style::default().fg(Color::DarkGray)),
                ]);
                Paragraph::new(hint).render(chunks[2], buf);
            }
            _ => {}
        }
    }
}
