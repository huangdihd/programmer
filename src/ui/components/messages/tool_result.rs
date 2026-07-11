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

use async_openai::types::responses::{FunctionCallOutput, FunctionCallOutputItemParam};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui_widgets::block::{Block, Padding};
use ratatui_widgets::paragraph::{Paragraph, Wrap};

use crate::ui::components::messages::assistant::detail_style;
use crate::ui::components::messages::assistant_message::EXPANDED_BG;
use crate::ui::markdown_theme::palette;

const PAD_LEFT: u16 = 2;
const PAD_RIGHT: u16 = 2;

/// Renders the result of a tool call (a `function_call_output`). Collapsed it is
/// a single line showing the first line of output; expanded it shows the full
/// output. A caret hints the line can be clicked to toggle.
pub struct ToolResultMessage<'a> {
    output: &'a FunctionCallOutputItemParam,
    expanded: bool,
}

impl<'a> ToolResultMessage<'a> {
    pub fn new(output: &'a FunctionCallOutputItemParam) -> Self {
        Self {
            output,
            expanded: false,
        }
    }

    pub fn expanded(mut self, expanded: bool) -> Self {
        self.expanded = expanded;
        self
    }

    pub fn into_paragraph(self) -> Paragraph<'static> {
        let muted = Style::new().fg(palette::MUTED).add_modifier(Modifier::DIM);

        let text = match &self.output.output {
            FunctionCallOutput::Text(text) => text.clone(),
            FunctionCallOutput::Content(_) => "[non-text output]".to_string(),
        };

        let all: Vec<&str> = text.lines().collect();
        let multiline = all.len() > 1;
        let block = Block::default().padding(Padding::new(PAD_LEFT, PAD_RIGHT, 0, 1));

        if !self.expanded {
            // Collapsed: a single (unwrapped) line — the first line of output.
            let first = all.first().copied().unwrap_or("[no output]");
            let caret = if multiline { "▸ " } else { "" };
            let suffix = if multiline { "..." } else { "" };
            let line = Line::from(Span::styled(format!("{caret}⎿ {first}{suffix}"), muted));
            return Paragraph::new(Text::from(line)).block(block);
        }

        // Expanded: the full output.
        let mut lines: Vec<Line<'static>> = all
            .iter()
            .enumerate()
            .map(|(index, line)| {
                if index == 0 {
                    let caret = if multiline { "▾ " } else { "" };
                    Line::from(Span::styled(format!("{caret}⎿ {line}"), detail_style()))
                } else {
                    Line::from(Span::styled(format!("  {line}"), detail_style()))
                }
            })
            .collect();
        if lines.is_empty() {
            lines.push(Line::from(Span::styled("⎿ [no output]", detail_style())));
        }

        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .block(block.style(Style::new().bg(EXPANDED_BG)))
    }
}
