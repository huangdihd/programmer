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

use ratatui::style::{Color, Modifier, Style};
use ratatui_textarea::{Input, TextArea};

#[derive(Debug, Clone)]
pub struct InputPanel<'a> {
    pub text_area: TextArea<'a>,
}

impl InputPanel<'_> {
    pub fn new() -> Self {
        let mut text_area = TextArea::default();

        text_area.set_style(Style::default().fg(Color::White));
        text_area.set_cursor_line_style(Style::default());
        text_area.set_cursor_style(Style::default().add_modifier(Modifier::REVERSED));
        text_area.set_placeholder_text("Talk with the programmer…");
        text_area.set_placeholder_style(
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        );

        InputPanel { text_area }
    }

    pub fn get_content(&self) -> String {
        self.text_area.lines().join("\n")
    }

    pub fn input(&mut self, input: impl Into<Input>) -> bool {
        self.text_area.input(input)
    }

    pub fn clear(&mut self) -> bool {
        self.text_area.clear()
    }
}
