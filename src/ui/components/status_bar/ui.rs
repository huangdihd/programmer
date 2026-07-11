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

use super::status_bar::{StatusBar, StatusState};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Widget;

const DIM: Color = Color::DarkGray;
const ACCENT: Color = Color::LightBlue;
const OUTPUT: Color = Color::LightGreen;
const WARN: Color = Color::Yellow;

impl Widget for &StatusBar {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let (icon, label, color) = match self.status {
            StatusState::Idle => ("●", "Ready", DIM),
            StatusState::Thinking => ("●", "Thinking", ACCENT),
            StatusState::Outputting => ("▸", "Outputting", OUTPUT),
            StatusState::CreatingToolCall => ("⚒", "Creating tool call", WARN),
            StatusState::ToolRunning => ("⚡", "Running tools", WARN),
        };

        // Build the status text: icon + label + optional elapsed time.
        let mut text = format!(" {} {} ", icon, label);
        if let Some(dur) = self.elapsed() {
            let secs = dur.as_secs_f64();
            if secs < 60.0 {
                text.push_str(&format!("({:.1}s)", secs));
            } else {
                let m = (secs / 60.0) as u64;
                let s = secs % 60.0;
                text.push_str(&format!("({}m {:.0}s)", m, s));
            }
        }

        ratatui::widgets::Paragraph::new(text)
            .style(Style::default().fg(color).add_modifier(Modifier::BOLD))
            .render(area, buf);
    }
}
