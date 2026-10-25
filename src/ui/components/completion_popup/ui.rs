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
use crate::ui::markdown_theme::palette;
use crate::ui::theme::role;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Clear, List, ListItem, Widget};

const BG: Color = palette::CODE_BG;
const FG: Color = palette::TEXT;

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
                let hovered = self.hovered == Some(i);
                let style = if hovered && self.pressed == Some(i) {
                    Style::default()
                        .fg(role::SELECTION_TEXT)
                        .bg(role::INTERACTION_PRESSED_BG)
                } else if hovered {
                    Style::default().fg(FG).bg(self.hover_background)
                } else if i == self.selected {
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
    fn pointer_feedback_takes_precedence_over_keyboard_selection() {
        let candidates = ["auto", "light"];
        let area = Rect::new(0, 0, 10, 2);
        let mut buffer = Buffer::empty(area);
        let popup = CompletionPopup {
            candidates: &candidates,
            label: |value| *value,
            selected: 0,
            scroll_offset: 0,
            hovered: Some(0),
            pressed: None,
            hover_background: palette::SURFACE_HOVER_SOFT,
        };
        (&popup).render(area, &mut buffer);
        assert_eq!(buffer[(0, 0)].bg, palette::SURFACE_HOVER_SOFT);

        let pressed = CompletionPopup {
            pressed: Some(0),
            ..popup
        };
        (&pressed).render(area, &mut buffer);
        assert_eq!(buffer[(0, 0)].bg, role::INTERACTION_PRESSED_BG);
    }

    #[test]
    fn selected_row_is_filled_edge_to_edge_in_both_themes() {
        let candidates = ["auto", "light", "dark"];
        let popup = CompletionPopup {
            candidates: &candidates,
            label: |value| *value,
            selected: 0,
            scroll_offset: 0,
            hovered: None,
            pressed: None,
            hover_background: palette::SURFACE_HOVER,
        };
        let area = Rect::new(2, 1, 12, 3);
        for mode in [Theme::Light, Theme::Dark] {
            let mut buffer = Buffer::empty(Rect::new(0, 0, 18, 5));
            (&popup).render(area, &mut buffer);
            theme::apply(mode, buffer.area, &mut buffer);
            let (fg, bg) = if mode == Theme::Light {
                (Color::Rgb(37, 42, 53), Color::Rgb(225, 231, 245))
            } else {
                (palette::TEXT, palette::SURFACE)
            };
            for x in area.x..area.right() {
                assert_eq!(buffer[(x, area.y)].bg, bg);
                assert_eq!(buffer[(x, area.y)].fg, fg);
            }
        }
    }
}
