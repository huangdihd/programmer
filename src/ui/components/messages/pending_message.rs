// Copyright (C) 2025 huangdihd
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

use ratatui::prelude::Color;
use ratatui::style::Stylize;
use ratatui_widgets::block::Block;
use ratatui_widgets::paragraph::{Paragraph, Wrap};

pub struct PendingMessage<'a> {
    text: &'a str,
}

impl<'a> PendingMessage<'a> {
    pub fn new(text: &'a str) -> Self {
        Self { text }
    }

    pub fn into_paragraph(self) -> Paragraph<'a> {
        let block = Block::default()
            .title("Pending Message")
            .title_style(Color::Green);
        Paragraph::new(self.text)
            .block(block)
            .fg(Color::LightBlue)
            .bg(Color::DarkGray)
            .wrap(Wrap { trim: true })
    }
}
