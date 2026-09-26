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
use crate::ui::theme::role;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Clear, List, ListItem, Widget};

const BG: Color = Color::Rgb(30, 30, 40);
const FG: Color = Color::White;

impl<T> Widget for &CompletionPopup<'_, T> {
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
            .map(|(i, candidate)| {
                let style = if i == self.selected {
                    Style::default()
                        .fg(role::SELECTION_TEXT)
                        .bg(role::SELECTION_BG)
                } else {
                    Style::default().fg(FG).bg(BG)
                };
                ListItem::new((self.label)(candidate)).style(style)
            })
            .collect();

        List::new(items).render(inner, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::{self, Theme};

    #[test]
    fn selected_row_is_filled_edge_to_edge_in_both_themes() {
        let candidates = ["auto", "light", "dark"];
        let popup = CompletionPopup {
            candidates: &candidates,
            label: |value| *value,
            selected: 0,
            scroll_offset: 0,
        };
        let area = Rect::new(2, 1, 12, 3);
        for mode in [Theme::Light, Theme::Dark] {
            let mut buffer = Buffer::empty(Rect::new(0, 0, 18, 5));
            (&popup).render(area, &mut buffer);
            theme::apply(mode, buffer.area, &mut buffer);
            let (fg, bg) = if mode == Theme::Light {
                (Color::Rgb(56, 58, 66), Color::Rgb(225, 235, 253))
            } else {
                (Color::Black, Color::LightBlue)
            };
            for x in area.x..area.right() {
                assert_eq!(buffer[(x, area.y)].bg, bg);
                assert_eq!(buffer[(x, area.y)].fg, fg);
            }
        }
    }
}
