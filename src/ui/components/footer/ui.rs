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
use crate::security::SandboxMode;
use crate::ui::markdown_theme::palette;
use crate::ui::theme::role;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Widget};
use unicode_width::UnicodeWidthStr;

const DIM: Color = palette::MUTED;

impl Widget for &Footer {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // Cancellation context must not be squeezed out by model and branding.
        if self.status.status
            == crate::ui::components::status_bar::status_bar::StatusState::Cancelling
        {
            (&self.status).render(area, buf);
            return;
        }
        Block::default()
            .style(Style::default().bg(palette::SURFACE))
            .render(area, buf);

        let mode_text = format!("  {} {} ", self.work_mode.icon(), self.work_mode.label());
        let mode_len = mode_text.width() as u16;
        let sandbox_text = self.sandbox_text();
        let sandbox_len = sandbox_text.width() as u16;
        let model_text = self.model_text();
        let thinking_text = self.thinking_text();
        let (model_len, thinking_len) = self.model_and_thinking_widths(area.width);

        // LSP indicator: shown whenever the project has an LSP checker
        // configured (from startup) or a server is live. Command-backend
        // projects see no extra clutter.
        let lsp = crate::diagnostics::lsp_status();
        let (lsp_text, lsp_style) = if lsp.checking {
            // Running a check.
            (
                " \u{25c8} LSP \u{27f3} ".to_string(),
                Style::default().fg(palette::YELLOW),
            )
        } else if lsp.failed {
            // Last attempt failed (server wouldn't start / snapshot errored).
            (
                " \u{25c8} LSP \u{2717} ".to_string(),
                Style::default().fg(palette::RED),
            )
        } else if lsp.servers > 0 {
            // One or more warm servers.
            (
                format!(" \u{25c8} LSP {} ", lsp.servers),
                Style::default().fg(palette::GREEN),
            )
        } else if self.lsp_configured {
            // Configured but no server started yet — idle.
            (" \u{25c8} LSP ".to_string(), Style::default().fg(DIM))
        } else {
            (String::new(), Style::default())
        };
        let lsp_len = lsp_text.width() as u16;

        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(mode_len),     // work mode (leftmost)
                Constraint::Length(sandbox_len),  // sandbox mode
                Constraint::Min(1),               // status
                Constraint::Length(lsp_len),      // LSP status
                Constraint::Length(model_len),    // model
                Constraint::Length(thinking_len), // thinking level
            ])
            .split(area);

        // Far left: work mode pill
        let mode_style = match self.work_mode {
            WorkMode::Manual => Style::default().fg(palette::RED),
            WorkMode::Auto => Style::default().fg(palette::GREEN),
            WorkMode::Yolo => Style::default().fg(palette::YELLOW),
            WorkMode::Plan => Style::default().fg(palette::CYAN),
        };
        ratatui::widgets::Paragraph::new(mode_text)
            .style(mode_style)
            .render(chunks[0], buf);

        let sandbox_style = match self.sandbox_mode {
            SandboxMode::Restricted => Style::default().fg(palette::GREEN),
            SandboxMode::Network => Style::default().fg(palette::YELLOW),
            SandboxMode::Off => Style::default().fg(palette::RED),
        };
        ratatui::widgets::Paragraph::new(sandbox_text)
            .style(sandbox_style)
            .render(chunks[1], buf);

        // Status indicator
        (&self.status).render(chunks[2], buf);

        // LSP status (empty string renders nothing)
        if !lsp_text.is_empty() {
            ratatui::widgets::Paragraph::new(lsp_text)
                .style(lsp_style)
                .render(chunks[3], buf);
        }

        // Keep the active model and independently clickable thinking level
        // anchored at the right edge.
        if !model_text.is_empty() {
            ratatui::widgets::Paragraph::new(model_text)
                .right_aligned()
                .style(Style::default().fg(role::SUBTLE))
                .render(chunks[4], buf);
            ratatui::widgets::Paragraph::new(thinking_text)
                .style(Style::default().fg(role::SUBTLE))
                .render(chunks[5], buf);
        }
    }
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    use crate::ui::components::status_bar::status_bar::StatusState;

    #[test]
    fn cancelling_footer_prioritizes_wait_context_over_model_and_branding() {
        let mut footer = Footer::new();
        footer.current_model = "a-very-long-provider-model-name".into();
        footer.status.set(StatusState::Cancelling);
        footer.status.detail = Some("waiting for tool batch: command (last observed)".into());
        let area = Rect::new(0, 0, 80, 1);
        let mut buffer = Buffer::empty(area);
        (&footer).render(area, &mut buffer);
        let line = (0..area.width)
            .map(|x| buffer[(x, 0)].symbol())
            .collect::<String>();
        assert!(line.contains("Cancelling"));
        assert!(line.contains("tool batch: command"));
        assert!(!line.contains("provider"));
        assert!(!line.contains("GPL"));
    }
}
