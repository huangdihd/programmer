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

use async_openai::types::responses::{FunctionCallOutputItemParam, OutputItem};
use ratatui::prelude::Color;
use ratatui::style::Style;
use ratatui::text::Text;
use ratatui_widgets::block::{Block, Padding};
use ratatui_widgets::paragraph::{Paragraph, Wrap};

use crate::ui::markdown_code_block::{COPY_LABEL, CodeCopyButton};

use crate::ui::components::messages::assistant::reasoning::ReasoningMessage;
use crate::ui::components::messages::assistant::text::TextMessage;
use crate::ui::components::messages::assistant::tool_call::ToolCallMessage;
use crate::ui::components::messages::assistant::unsupported::UnsupportedMessage;
use crate::ui::markdown_theme::palette;

const PAD_LEFT: u16 = 2;
const PAD_RIGHT: u16 = 2;
/// Gray panel behind expanded reasoning/tool details.
pub(crate) const EXPANDED_BG: Color = palette::SURFACE;

/// Renders one assistant output item. Each item kind has its own renderer (see
/// the `assistant` submodule); this type just dispatches on the variant and
/// wraps the result in the shared padded block.
pub struct AssistantMessage<'a> {
    output_item: &'a OutputItem,
    width: u16,
    /// Whether this item is still being streamed (not yet finalized). Drives the
    /// reasoning label between "Thinking..." and "Thought".
    in_progress: bool,
    /// Whether the user has expanded this item to see its full detail.
    expanded: bool,
    /// Monotonic frame counter for animating the "Thinking..." dots.
    frame_count: Option<u64>,
    /// For function-call items: the matching result and whether the tool
    /// reported failure, rendered inline so the call and its output appear as a
    /// single message.
    tool_output: Option<(&'a FunctionCallOutputItemParam, bool)>,
}

impl<'a> AssistantMessage<'a> {
    pub fn new(output_item: &'a OutputItem, width: u16) -> Self {
        AssistantMessage {
            output_item,
            width,
            in_progress: false,
            expanded: false,
            frame_count: None,
            tool_output: None,
        }
    }

    pub fn tool_output(
        mut self,
        tool_output: Option<(&'a FunctionCallOutputItemParam, bool)>,
    ) -> Self {
        self.tool_output = tool_output;
        self
    }

    pub fn in_progress(mut self, in_progress: bool) -> Self {
        self.in_progress = in_progress;
        self
    }

    pub fn expanded(mut self, expanded: bool) -> Self {
        self.expanded = expanded;
        self
    }

    pub fn frame_count(mut self, frame_count: u64) -> Self {
        self.frame_count = Some(frame_count);
        self
    }

    pub fn into_paragraph(self) -> (Paragraph<'static>, Vec<CodeCopyButton>) {
        let (text, codes) = match self.output_item {
            OutputItem::Message(message) => TextMessage::new(message, self.width).into_parts(),
            OutputItem::Reasoning(item) => {
                ReasoningMessage::new(self.in_progress, item, self.width)
                    .expanded(self.expanded)
                    .frame_count(self.frame_count)
                    .into_parts()
            }
            OutputItem::FunctionCall(call) => (
                ToolCallMessage::new(call)
                    .output(self.tool_output.map(|(output, _)| output))
                    .failed(self.tool_output.map(|(_, failed)| failed).unwrap_or(false))
                    .expanded(self.expanded)
                    .into_text(),
                Vec::new(),
            ),
            other => (UnsupportedMessage::new(other).into_text(), Vec::new()),
        };
        let buttons = scan_copy_buttons(&text, &codes);

        let foldable = matches!(
            self.output_item,
            OutputItem::Reasoning(_) | OutputItem::FunctionCall(_)
        );
        let mut block = Block::default().padding(Padding::new(PAD_LEFT, PAD_RIGHT, 0, 1));
        if self.expanded && foldable {
            block = block.style(Style::new().bg(EXPANDED_BG));
        }

        let mut paragraph = Paragraph::new(text).block(block);
        // An expanded tool call shows the full result, whose lines can be long;
        // wrap them like the standalone result view used to.
        if self.expanded && matches!(self.output_item, OutputItem::FunctionCall(_)) {
            paragraph = paragraph.wrap(Wrap { trim: false });
        }
        (paragraph, buttons)
    }
}

/// Locates the clickable copy labels rendered by `CodeBlockHooks` in `text` and
/// pairs each with its code block content (the k-th label belongs to the k-th
/// block). Coordinates are relative to the paragraph, including its padding.
fn scan_copy_buttons(text: &Text<'_>, codes: &[String]) -> Vec<CodeCopyButton> {
    let mut buttons = Vec::new();
    if codes.is_empty() {
        return buttons;
    }
    for (row, line) in text.lines.iter().enumerate() {
        let mut x = 0u16;
        for span in &line.spans {
            let width = span.width() as u16;
            if span.content.as_ref() == COPY_LABEL {
                if let Some(content) = codes.get(buttons.len()) {
                    buttons.push(CodeCopyButton {
                        row: row as u16,
                        x_start: PAD_LEFT + x,
                        x_end: PAD_LEFT + x + width,
                        content: content.clone(),
                    });
                }
            }
            x = x.saturating_add(width);
        }
    }
    buttons
}
