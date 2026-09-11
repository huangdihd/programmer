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

use ratatui_widgets::paragraph::Paragraph;

use super::notice_message::notice;
use crate::ui::markdown_theme::palette;

/// Renders a queued user message as a lightweight status notice.
pub struct PendingMessage<'a> {
    text: &'a str,
}

impl<'a> PendingMessage<'a> {
    pub fn new(text: &'a str) -> Self {
        Self { text }
    }

    pub fn into_paragraph(self) -> Paragraph<'static> {
        notice(
            "↳",
            palette::YELLOW,
            palette::MUTED,
            format!("Queued  {}  ·  ↑ edit", self.text),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::widgets::Widget;

    #[test]
    fn queued_notice_shows_status_and_message() {
        let area = Rect::new(0, 0, 60, 1);
        let mut buffer = Buffer::empty(area);
        PendingMessage::new("continue with the tests")
            .into_paragraph()
            .render(area, &mut buffer);

        let rendered = (0..area.width)
            .map(|x| buffer[(x, 0)].symbol())
            .collect::<String>();
        assert!(rendered.contains("Queued"), "{rendered}");
        assert!(rendered.contains("continue with the tests"), "{rendered}");
        assert!(!rendered.contains("Esc"), "{rendered}");
        assert!(
            rendered.contains("↑") && rendered.contains("edit"),
            "{rendered}"
        );
    }
}
