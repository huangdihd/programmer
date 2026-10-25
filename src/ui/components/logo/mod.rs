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

use crate::ui::markdown_theme::palette;
use crate::ui::theme::role;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::prelude::Widget;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use unicode_width::UnicodeWidthStr;

/// U+1F4AD SPEECH BALLOON — the Dream indicator, shown while the background
/// worker is consolidating memory.
const DREAM_ICON: &str = "\u{1f4ad}";

pub struct Logo<'a> {
    title: &'a str,
    /// Whether a background consolidation pass is running.
    dream: bool,
    /// Absolute column where the header rule meets a vertical pane divider.
    divider_x: Option<u16>,
}

impl<'a> Logo<'a> {
    pub fn new(title: &'a str) -> Self {
        Logo {
            title,
            dream: false,
            divider_x: None,
        }
    }

    /// Show the Dream indicator at the right edge of the title row.
    pub fn with_dream(mut self, dream: bool) -> Self {
        self.dream = dream;
        self
    }

    /// Join the header separator to a vertical pane divider below it.
    pub fn with_divider_at(mut self, x: u16) -> Self {
        self.divider_x = Some(x);
        self
    }
}

impl Widget for Logo<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }

        Block::default()
            .style(Style::default().bg(palette::CODE_BG))
            .render(Rect::new(area.x, area.y, area.width, 1), buf);

        let title_style = Style::default()
            .fg(role::TITLE)
            .add_modifier(Modifier::BOLD);
        let title = if self.title.width() + 3 <= usize::from(area.width) {
            Line::from(vec![
                Span::styled(
                    "  ",
                    Style::default().fg(role::LOGO).add_modifier(Modifier::BOLD),
                ),
                Span::styled(self.title, title_style),
            ])
        } else {
            Line::styled(self.title, title_style)
        };
        Paragraph::new(title).render(Rect::new(area.x, area.y, area.width, 1), buf);
        if area.height > 1 {
            let separator_style = Style::default().fg(role::DIVIDER).bg(palette::CODE_BG);
            for x in area.x..area.right() {
                if let Some(cell) = buf.cell_mut((x, area.y + 1)) {
                    let symbol = if self.divider_x == Some(x) {
                        "┬"
                    } else {
                        "─"
                    };
                    cell.set_symbol(symbol).set_style(separator_style);
                }
            }
        }
        if self.dream {
            render_dream_indicator(area, buf);
        }
    }
}

/// Draw the Dream icon at the far right of the title row. Skipped when the
/// centered title already reaches that far, so a long title keeps its last
/// characters instead of being overwritten by the indicator.
fn render_dream_indicator(area: Rect, buf: &mut Buffer) {
    let width = DREAM_ICON.width() as u16;
    if area.width < width || area.height == 0 {
        return;
    }
    let x = area.right() - width;
    let occupied = (x..area.right()).any(|x| {
        let symbol = buf[(x, area.y)].symbol();
        // A wide title glyph leaves its trailing cell empty, which is also
        // "not free" even though it holds no character of its own.
        !symbol.is_empty() && symbol != " "
    });
    if occupied {
        return;
    }
    buf.set_string(
        x,
        area.y,
        DREAM_ICON,
        Style::default().fg(role::LOGO).bg(palette::CODE_BG),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_the_supplied_session_title() {
        let area = Rect::new(0, 0, 30, 2);
        let mut buffer = Buffer::empty(area);

        Logo::new("Fix session header").render(area, &mut buffer);

        let first_line = (0..area.width)
            .map(|x| buffer[(x, 0)].symbol())
            .collect::<String>();
        assert!(first_line.contains("Fix session header"));
        assert!(!first_line.contains("Programmer"));
    }

    fn row(area: Rect, buffer: &Buffer) -> String {
        (0..area.width)
            .map(|x| buffer[(x, area.y)].symbol())
            .collect::<String>()
    }

    #[test]
    fn separator_fills_the_complete_row_at_a_nonzero_origin() {
        let area = Rect::new(7, 3, 30, 2);
        let mut buffer = Buffer::empty(area);

        Logo::new("Fix session header").render(area, &mut buffer);

        for x in area.x..area.right() {
            let cell = &buffer[(x, area.y + 1)];
            assert_eq!(cell.symbol(), "─", "missing cell at x={x}");
            assert_eq!(
                cell.bg,
                palette::CODE_BG,
                "missing separator background at x={x}"
            );
        }
    }

    #[test]
    fn separator_joins_a_vertical_pane_divider() {
        let area = Rect::new(7, 3, 30, 2);
        let mut buffer = Buffer::empty(area);
        let divider_x = 27;

        Logo::new("Fix session header")
            .with_divider_at(divider_x)
            .render(area, &mut buffer);

        assert_eq!(buffer[(divider_x - 1, area.y + 1)].symbol(), "─");
        assert_eq!(buffer[(divider_x, area.y + 1)].symbol(), "┬");
        assert_eq!(buffer[(divider_x + 1, area.y + 1)].symbol(), "─");
    }

    #[test]
    fn separator_and_sidebar_border_form_one_continuous_junction() {
        use ratatui::widgets::Borders;

        let area = Rect::new(7, 3, 40, 8);
        let mut buffer = Buffer::empty(area);
        let divider_x = 34;
        let header = Rect::new(area.x, area.y, area.width, 2);
        let sidebar = Rect::new(
            divider_x,
            header.bottom(),
            area.right() - divider_x,
            area.bottom() - header.bottom(),
        );

        Logo::new("Fix session header")
            .with_divider_at(divider_x)
            .render(header, &mut buffer);
        Block::default()
            .borders(Borders::LEFT)
            .render(sidebar, &mut buffer);

        assert_eq!(buffer[(divider_x - 1, header.bottom() - 1)].symbol(), "─");
        assert_eq!(buffer[(divider_x, header.bottom() - 1)].symbol(), "┬");
        assert_eq!(buffer[(divider_x + 1, header.bottom() - 1)].symbol(), "─");
        assert_eq!(buffer[(divider_x, header.bottom())].symbol(), "│");
        assert_eq!(buffer[(divider_x, header.bottom() + 1)].symbol(), "│");
    }

    #[test]
    fn the_dream_indicator_is_absent_until_a_pass_is_running() {
        let area = Rect::new(0, 0, 30, 2);
        let mut buffer = Buffer::empty(area);

        Logo::new("Fix session header").render(area, &mut buffer);

        assert!(!row(area, &buffer).contains(DREAM_ICON));
    }

    #[test]
    fn the_dream_indicator_sits_at_the_right_edge_of_the_title_row() {
        let area = Rect::new(0, 0, 30, 2);
        let mut buffer = Buffer::empty(area);

        Logo::new("Fix session header")
            .with_dream(true)
            .render(area, &mut buffer);

        let first_line = row(area, &buffer);
        assert!(first_line.contains("Fix session header"), "{first_line}");
        assert!(
            first_line.contains(DREAM_ICON),
            "the indicator must be drawn: {first_line:?}"
        );
        // Flush right: its first cell sits exactly where the icon ends.
        let x = area.width - DREAM_ICON.width() as u16;
        assert_eq!(buffer[(x, 0)].symbol(), DREAM_ICON, "{first_line:?}");
        assert_eq!(buffer[(x, 1)].symbol(), "─", "the separator must survive");
        // The separator row below is untouched.
        let second_row = Rect::new(0, 1, area.width, 1);
        assert!(row(second_row, &buffer).starts_with("─────"));
    }

    #[test]
    fn a_title_filling_the_row_keeps_its_characters() {
        let area = Rect::new(0, 0, 12, 2);
        let mut buffer = Buffer::empty(area);

        Logo::new("0123456789ab")
            .with_dream(true)
            .render(area, &mut buffer);

        let first_line = row(area, &buffer);
        assert!(first_line.contains("0123456789ab"), "{first_line:?}");
        assert!(!first_line.contains(DREAM_ICON), "{first_line:?}");
    }
}
