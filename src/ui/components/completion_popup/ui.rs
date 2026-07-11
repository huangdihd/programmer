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

use super::CompletionPopup;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Clear, List, ListItem, Widget};

const BG: Color = Color::Rgb(30, 30, 40);
const FG: Color = Color::White;
const ACCENT: Color = Color::LightBlue;

impl Widget for &CompletionPopup<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // Clear whatever is behind the popup first.
        Clear.render(area, buf);

        // Fill background without a border.
        let bg_style = Style::default().bg(BG);
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_style(bg_style);
                    cell.set_symbol(" ");
                }
            }
        }

        let inner = area;

        // Use the scroll offset from state (controlled by key handlers).
        let visible_height = inner.height as usize;
        let scroll = self.scroll_offset;

        let items: Vec<ListItem> = self
            .candidates
            .iter()
            .enumerate()
            .skip(scroll)
            .take(visible_height)
            .map(|(i, text)| {
                let style = if i == self.selected {
                    Style::default()
                        .fg(Color::Black)
                        .bg(ACCENT)
                } else {
                    Style::default().fg(FG).bg(BG)
                };
                ListItem::new(text.as_str()).style(style)
            })
            .collect();

        List::new(items).render(inner, buf);
    }
}
