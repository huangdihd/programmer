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

use crate::ui::components::input_panel::input_panel::InputPanel;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

const ACCENT: Color = Color::LightBlue;
/// Accent while the input is a `!command` (shell mode) — the green of the
/// terminal panel's grabbed state.
const BANG_ACCENT: Color = Color::LightGreen;

impl Widget for &InputPanel<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // A leading `!` flips the panel into shell mode: green accent, `$`
        // prompt, and a title saying where the command will run.
        let bang = self.get_content().starts_with('!');
        let (title, accent, prompt) = if bang {
            (" ! Shell — runs in the interactive terminal ", BANG_ACCENT, "$ ")
        } else {
            (" Input ", ACCENT, "❯ ")
        };

        let block = Block::default()
            .title(title)
            .title_style(Style::default().fg(accent).add_modifier(Modifier::BOLD))
            .borders(Borders::TOP | Borders::BOTTOM)
            .border_style(Style::default().fg(accent));

        let inner = block.inner(area);
        block.render(area, buf);

        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(2), Constraint::Min(1)])
            .split(inner);

        Paragraph::new(prompt)
            .style(Style::default().fg(accent).add_modifier(Modifier::BOLD))
            .render(chunks[0], buf);

        self.text_area.render(chunks[1], buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_to_text(panel: &InputPanel<'_>) -> String {
        let area = Rect::new(0, 0, 60, 5);
        let mut buf = Buffer::empty(area);
        panel.render(area, &mut buf);
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn bang_input_switches_to_shell_mode() {
        let mut panel = InputPanel::new();
        let normal = render_to_text(&panel);
        assert!(normal.contains(" Input "), "normal title: {normal}");
        assert!(normal.contains("❯"), "normal prompt: {normal}");

        panel.set_content("!cargo test");
        let bang = render_to_text(&panel);
        assert!(bang.contains(" ! Shell "), "bang title: {bang}");
        assert!(bang.contains("$ !cargo test"), "bang prompt + text: {bang}");

        // Deleting the `!` flips straight back.
        panel.set_content("cargo test");
        let back = render_to_text(&panel);
        assert!(back.contains(" Input "), "back to normal: {back}");
    }
}
