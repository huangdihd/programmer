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

use crate::app::App;
use crate::ui::components::completion_popup::CompletionPopup;
use crate::ui::components::logo::Logo;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};

impl Widget for &mut App<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // Update footer state from conversation panel.
        let is_receiving = self.conversation_panel.receiving_response.is_some();
        let is_outputting_message = self.conversation_panel.outputting_message;
        let is_creating_tool_call = self.conversation_panel.creating_tool_call;
        let is_tool_running = self.conversation_panel.tool_running;
        self.footer.update(
            is_receiving,
            is_outputting_message,
            is_creating_tool_call,
            is_tool_running,
        );
        self.footer.current_model = self.current_model.clone();

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(2),
                Constraint::Length(3),
                Constraint::Length(1),
            ])
            .split(area);
        let logo = Logo::new();
        logo.render(chunks[0], buf);
        self.conversation_panel.render(chunks[1], buf);
        self.input_panel.render(chunks[2], buf);
        (&self.footer).render(chunks[3], buf);

        // ---- completion popup (floats above the input panel) ----
        if let Some(ref completion) = self.input_panel.completion {
            if completion.visible {
                let max_visible = 10u16;
                let count = (completion.candidates.len() as u16).min(max_visible);
                let popup_height = count; // no borders

                // Align the popup with the token being completed: input text
                // starts after the "❯ " prompt (2 cells), plus the prefix width.
                let token_x = chunks[2].x + 2 + completion.prefix.len() as u16;

                // Size the popup to its longest candidate, clamped to the panel.
                let longest = completion
                    .candidates
                    .iter()
                    .map(|c| c.len())
                    .max()
                    .unwrap_or(0) as u16;
                let popup_width = (longest + 2).clamp(10, chunks[2].width);

                let popup_area = Rect {
                    // Pull the popup back left if it would overflow the panel.
                    x: token_x.min(chunks[2].right().saturating_sub(popup_width)),
                    y: chunks[2].y.saturating_sub(popup_height),
                    width: popup_width,
                    height: popup_height.min(chunks[2].y),
                };

                let popup = CompletionPopup {
                    candidates: &completion.candidates,
                    selected: completion.selected,
                    scroll_offset: completion.scroll_offset,
                };
                popup.render(popup_area, buf);
            }
        }
    }
}
