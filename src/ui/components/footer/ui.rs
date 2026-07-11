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

use super::footer::Footer;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::Widget;

const DIM: Color = Color::DarkGray;
const ACCENT: Color = Color::LightBlue;

impl Widget for &Footer {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let model_len = self.current_model.len() as u16;
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(if model_len > 0 { model_len + 2 } else { 0 }),
                Constraint::Length(28), // "GPL-3.0-or-later · © 2026"
            ])
            .split(area);

        // Left: status indicator
        (&self.status).render(chunks[0], buf);

        // Middle: current model name
        if !self.current_model.is_empty() {
            ratatui::widgets::Paragraph::new(format!(" {} ", self.current_model))
                .style(Style::default().fg(ACCENT))
                .render(chunks[1], buf);
        }

        // Right: copyright
        ratatui::widgets::Paragraph::new("GPL-3.0-or-later \u{b7} \u{a9} 2026")
            .style(Style::default().fg(DIM))
            .render(chunks[2], buf);
    }
}
