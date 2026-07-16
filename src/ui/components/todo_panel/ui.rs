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

use super::{AddMode, TodoPanel};
use crate::todos::TodoStatus;
use crate::ui::text::truncate_to_width;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget, Wrap};

impl TodoPanel {
    /// Render the todo panel into the given area.
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(Span::styled(
                " Todos ",
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ));
        let inner = block.inner(area);
        block.render(area, buf);

        match &self.add_mode {
            AddMode::Hidden => self.render_list(inner, buf),
            AddMode::Title { input } => self.render_add_title(inner, buf, input),
            AddMode::Description { input, .. } => {
                self.render_add_description(inner, buf, input)
            }
        }
    }

    fn render_list(&self, area: Rect, buf: &mut Buffer) {
        if self.list.todos.is_empty() {
            let msg = Paragraph::new("No todos yet. Press 'a' to add one.")
                .style(Style::default().fg(Color::DarkGray))
                .wrap(Wrap { trim: true });
            msg.render(area, buf);

            // Still show the hint bar.
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(1), Constraint::Length(1)])
                .split(area);
            self.render_hint(chunks[1], buf);
            return;
        }

        // Layout: list area + hint bar.
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(area);

        let visible_count = Self::VISIBLE_ITEMS.min(self.list.todos.len());
        let list_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(
                (0..visible_count)
                    .map(|_| Constraint::Length(1))
                    .collect::<Vec<_>>(),
            )
            .split(chunks[0]);

        let highlight_style = Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD);

        for i in 0..visible_count {
            let idx = self.scroll_offset + i;
            if idx >= self.list.todos.len() {
                break;
            }
            let todo = &self.list.todos[idx];
            let is_selected = idx == self.selected;

            let (icon, status_color) = match todo.status {
                TodoStatus::Pending => (TodoStatus::Pending.icon(), Color::DarkGray),
                TodoStatus::InProgress => (TodoStatus::InProgress.icon(), Color::Yellow),
                TodoStatus::Completed => (TodoStatus::Completed.icon(), Color::Green),
                TodoStatus::Cancelled => (TodoStatus::Cancelled.icon(), Color::Red),
            };

            let marker = if is_selected { "❯" } else { " " };
            let title = truncate_to_width(&todo.title, 50);

            let line_style = if is_selected {
                highlight_style
            } else {
                Style::default()
            };

            let line = if is_selected {
                Line::from(vec![
                    Span::styled(
                        format!("{marker} [{icon}] {title}"),
                        line_style,
                    ),
                ])
            } else {
                Line::from(vec![
                    Span::raw(format!("{marker} [")),
                    Span::styled(icon, Style::default().fg(status_color).bold()),
                    Span::raw(format!("] {title}")),
                ])
            };

            Paragraph::new(line).render(list_chunks[i], buf);
        }

        // Scroll indicator.
        if self.list.todos.len() > Self::VISIBLE_ITEMS {
            let pct = self.scroll_offset as f64
                / (self.list.todos.len() - Self::VISIBLE_ITEMS).max(1) as f64
                * 100.0;
            let scroll = Line::from(vec![Span::styled(
                format!(
                    "  {}/{} ({:.0}%)",
                    self.scroll_offset + 1,
                    self.list.todos.len(),
                    pct
                ),
                Style::default().fg(Color::DarkGray),
            )]);
            // Overlay scroll info in the bottom-right corner of the list area.
            let indicator_x = chunks[0]
                .width
                .saturating_sub(20)
                .min(chunks[0].width.saturating_sub(1));
            if indicator_x < chunks[0].width && chunks[0].height > 0 {
                let indicator_area = Rect {
                    x: chunks[0].x + indicator_x,
                    y: chunks[0].y + chunks[0].height.saturating_sub(1),
                    width: (chunks[0].width - indicator_x).min(20),
                    height: 1,
                };
                Paragraph::new(scroll).render(indicator_area, buf);
            }
        }

        self.render_hint(chunks[1], buf);
    }

    fn render_add_title(&self, area: Rect, buf: &mut Buffer, input: &str) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area);

        Paragraph::new(Line::from(vec![Span::styled(
            "New todo — title:",
            Style::default().fg(Color::White).bold(),
        )]))
        .render(chunks[0], buf);

        let input_line = Line::from(vec![
            Span::styled("  ❯ ", Style::default().fg(Color::Cyan)),
            Span::styled(input, Style::default().fg(Color::White)),
            Span::styled("▏", Style::default().fg(Color::Cyan)),
        ]);
        Paragraph::new(input_line).render(chunks[1], buf);

        let hint = Line::from(vec![
            Span::styled("Enter", Style::default().fg(Color::Green).bold()),
            Span::styled(" next (description)  ", Style::default().fg(Color::DarkGray)),
            Span::styled("Esc", Style::default().fg(Color::Cyan).bold()),
            Span::styled(" cancel", Style::default().fg(Color::DarkGray)),
        ]);
        Paragraph::new(hint).render(chunks[2], buf);
    }

    fn render_add_description(&self, area: Rect, buf: &mut Buffer, input: &str) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area);

        Paragraph::new(Line::from(vec![Span::styled(
            "Description (optional, Enter to save):",
            Style::default().fg(Color::White).bold(),
        )]))
        .render(chunks[0], buf);

        let input_line = Line::from(vec![
            Span::styled("  ❯ ", Style::default().fg(Color::Cyan)),
            Span::styled(input, Style::default().fg(Color::White)),
            Span::styled("▏", Style::default().fg(Color::Cyan)),
        ]);
        Paragraph::new(input_line).render(chunks[1], buf);

        let hint = Line::from(vec![
            Span::styled("Enter", Style::default().fg(Color::Green).bold()),
            Span::styled(" save  ", Style::default().fg(Color::DarkGray)),
            Span::styled("Esc", Style::default().fg(Color::Cyan).bold()),
            Span::styled(" skip description", Style::default().fg(Color::DarkGray)),
        ]);
        Paragraph::new(hint).render(chunks[2], buf);
    }

    fn render_hint(&self, area: Rect, buf: &mut Buffer) {
        let hints = vec![
            Span::styled("↑↓", Style::default().fg(Color::Cyan).bold()),
            Span::styled(" navigate  ", Style::default().fg(Color::DarkGray)),
            Span::styled("Enter", Style::default().fg(Color::Green).bold()),
            Span::styled(" toggle  ", Style::default().fg(Color::DarkGray)),
            Span::styled("a", Style::default().fg(Color::Green).bold()),
            Span::styled(" add  ", Style::default().fg(Color::DarkGray)),
            Span::styled("d", Style::default().fg(Color::Red).bold()),
            Span::styled(" delete  ", Style::default().fg(Color::DarkGray)),
            Span::styled("Esc", Style::default().fg(Color::Cyan).bold()),
            Span::styled(" close", Style::default().fg(Color::DarkGray)),
        ];
        Paragraph::new(Line::from(hints)).render(area, buf);
    }
}

