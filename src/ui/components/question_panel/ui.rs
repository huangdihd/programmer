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
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap};

impl QuestionPanel {
    /// Render the question panel centered on screen.
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);

        // Measure content to size the modal.
        let question_width = self.question.text.len().min(60);
        let max_option_width = match &self.question.kind {
            QuestionKind::Choice { options, .. } => {
                options.iter().map(|o| o.len()).max().unwrap_or(0)
            }
            QuestionKind::Text { .. } => 30,
        };
        let content_width = question_width.max(max_option_width).max(30) as u16 + 8;
        let modal_width = content_width.min(area.width.saturating_sub(4));

        let line_count = match &self.question.kind {
            QuestionKind::Choice { options, .. } => {
                // Question text (wrapped) + blank + options + hint
                3 + options.len() as u16 + 2
            }
            QuestionKind::Text { .. } => {
                // Question text (wrapped) + blank + input line + hint
                6
            }
        };
        let modal_height = (line_count + 2).min(area.height.saturating_sub(4));

        let modal_area = Rect {
            x: area.x + (area.width.saturating_sub(modal_width)) / 2,
            y: area.y + (area.height.saturating_sub(modal_height)) / 2,
            width: modal_width,
            height: modal_height,
        };

        // Background fill.
        let bg = Style::default().bg(Color::Rgb(20, 20, 30));
        for y in modal_area.y..modal_area.bottom() {
            for x in modal_area.x..modal_area.right() {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_style(bg);
                    cell.set_symbol(" ");
                }
            }
        }

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(" Question ")
            .title_style(Style::default().fg(Color::Cyan).bold());
        let inner = block.inner(modal_area);
        block.render(modal_area, buf);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(2),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(inner);

        // Question text.
        Paragraph::new(self.question.text.as_str())
            .style(Style::default().fg(Color::White))
            .wrap(Wrap { trim: true })
            .render(chunks[0], buf);

        // Choices or text input.
        match (&self.question.kind, &self.mode) {
            (QuestionKind::Choice { options, .. }, Mode::Choice { selected }) => {
                let lines: Vec<Line> = options
                    .iter()
                    .enumerate()
                    .map(|(i, opt)| {
                        let is_sel = i == *selected;
                        let prefix = if is_sel { "❯ " } else { "  " };
                        let style = if is_sel {
                            Style::default()
                                .fg(Color::Black)
                                .bg(Color::Cyan)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(Color::Gray)
                        };
                        Line::from(Span::styled(
                            format!("{prefix}{opt}"),
                            style,
                        ))
                    })
                    .collect();
                Paragraph::new(lines).render(chunks[1], buf);

                let hint = Line::from(vec![
                    Span::styled("↑↓", Style::default().fg(Color::Cyan).bold()),
                    Span::styled(" navigate  ", Style::default().fg(Color::DarkGray)),
                    Span::styled("Enter", Style::default().fg(Color::Green).bold()),
                    Span::styled(" select", Style::default().fg(Color::DarkGray)),
                ]);
                Paragraph::new(hint)
                    .alignment(Alignment::Right)
                    .render(chunks[2], buf);
            }
            (QuestionKind::Choice { .. }, Mode::Text { input }) => {
                // "Other…" text input.
                let cursor = "▏";
                let input_line = Line::from(vec![
                    Span::styled("  Other: ", Style::default().fg(Color::Cyan)),
                    Span::styled(input.clone(), Style::default().fg(Color::White)),
                    Span::styled(cursor, Style::default().fg(Color::Cyan)),
                ]);
                Paragraph::new(input_line).render(chunks[1], buf);

                let hint = Line::from(vec![
                    Span::styled("Enter", Style::default().fg(Color::Green).bold()),
                    Span::styled(" submit  ", Style::default().fg(Color::DarkGray)),
                    Span::styled("Esc", Style::default().fg(Color::Cyan).bold()),
                    Span::styled(" back to choices", Style::default().fg(Color::DarkGray)),
                ]);
                Paragraph::new(hint)
                    .alignment(Alignment::Right)
                    .render(chunks[2], buf);
            }
            (QuestionKind::Text { .. }, Mode::Text { input }) => {
                let cursor = "▏";
                let input_line = Line::from(vec![
                    Span::styled(input.clone(), Style::default().fg(Color::White)),
                    Span::styled(cursor, Style::default().fg(Color::Cyan)),
                ]);
                Paragraph::new(input_line)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(Color::DarkGray)),
                    )
                    .render(chunks[1], buf);

                let hint = Line::from(vec![
                    Span::styled("Enter", Style::default().fg(Color::Green).bold()),
                    Span::styled(" submit", Style::default().fg(Color::DarkGray)),
                ]);
                Paragraph::new(hint)
                    .alignment(Alignment::Right)
                    .render(chunks[2], buf);
            }
            _ => {} // Shouldn't happen: Mode/QuestionKind mismatch.
        }
    }
}
