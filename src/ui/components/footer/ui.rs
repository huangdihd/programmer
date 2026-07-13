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
use crate::classifier::WorkMode;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::Widget;

const DIM: Color = Color::DarkGray;
const ACCENT: Color = Color::LightBlue;

impl Widget for &Footer {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let mode_text = format!(
            "  {} {} ",
            self.work_mode.icon(),
            self.work_mode.label()
        );
        let mode_len = mode_text.len() as u16;
        let model_len = self.current_model.len() as u16;

        // LSP indicator: shown whenever the project has an LSP checker
        // configured (from startup) or a server is live. Command-backend
        // projects see no extra clutter.
        let lsp = crate::diagnostics::lsp_status();
        let (lsp_text, lsp_style) = if lsp.checking {
            // Running a check.
            (" \u{25c8} LSP \u{27f3} ".to_string(), Style::default().fg(Color::LightYellow))
        } else if lsp.failed {
            // Last attempt failed (server wouldn't start / snapshot errored).
            (" \u{25c8} LSP \u{2717} ".to_string(), Style::default().fg(Color::LightRed))
        } else if lsp.servers > 0 {
            // One or more warm servers.
            (
                format!(" \u{25c8} LSP {} ", lsp.servers),
                Style::default().fg(Color::LightGreen),
            )
        } else if self.lsp_configured {
            // Configured but no server started yet — idle.
            (" \u{25c8} LSP ".to_string(), Style::default().fg(DIM))
        } else {
            (String::new(), Style::default())
        };
        let lsp_len = lsp_text.chars().count() as u16;

        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(mode_len), // work mode (leftmost)
                Constraint::Min(1),           // status
                Constraint::Length(if model_len > 0 { model_len + 2 } else { 0 }), // model
                Constraint::Length(lsp_len),  // LSP status
                Constraint::Length(28),       // "GPL-3.0-or-later · © 2026"
            ])
            .split(area);

        // Far left: work mode pill
        let mode_style = match self.work_mode {
            WorkMode::Manual => Style::default().fg(Color::LightRed),
            WorkMode::AllowEdits => Style::default().fg(Color::LightGreen),
            WorkMode::Auto => Style::default().fg(Color::LightCyan),
            WorkMode::Yolo => Style::default().fg(Color::LightYellow),
        };
        ratatui::widgets::Paragraph::new(mode_text)
            .style(mode_style)
            .render(chunks[0], buf);

        // Status indicator
        (&self.status).render(chunks[1], buf);

        // Model name
        if !self.current_model.is_empty() {
            ratatui::widgets::Paragraph::new(format!(" {} ", self.current_model))
                .style(Style::default().fg(ACCENT))
                .render(chunks[2], buf);
        }

        // LSP status (empty string renders nothing)
        if !lsp_text.is_empty() {
            ratatui::widgets::Paragraph::new(lsp_text)
                .style(lsp_style)
                .render(chunks[3], buf);
        }

        // Right: copyright
        ratatui::widgets::Paragraph::new("GPL-3.0-or-later \u{b7} \u{a9} 2026")
            .style(Style::default().fg(DIM))
            .render(chunks[4], buf);
    }
}
