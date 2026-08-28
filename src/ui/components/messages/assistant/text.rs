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

use async_openai::types::responses::{OutputMessage, OutputMessageContent};
use ratatui::text::Text;
use ratatui_markdown::markdown::MarkdownRenderer;

use crate::ui::markdown_code_block::CodeBlockHooks;
use crate::ui::markdown_theme::AppTheme;

/// Matches the horizontal padding of the parent `AssistantMessage` block, so the
/// wrapped markdown lines up with the block's content area.
const HORIZONTAL_PAD: u16 = 2;

/// Renders a regular assistant text message as themed, syntax-highlighted
/// markdown.
pub struct TextMessage<'a> {
    message: &'a OutputMessage,
    width: u16,
}

impl<'a> TextMessage<'a> {
    pub fn new(message: &'a OutputMessage, width: u16) -> Self {
        Self { message, width }
    }

    /// Renders the message, also returning the raw content of every code block
    /// (in render order) so copy buttons can be wired up.
    pub fn into_parts(self) -> (Text<'static>, Vec<String>) {
        let md = markdown_source(self.message);
        render_markdown(&md, markdown_width(self.width))
    }
}

/// Flatten the protocol content parts into the Markdown document shown by the
/// assistant message. Kept here so the live worker and finished-message path
/// cannot drift while the worker renders that source incrementally.
pub(crate) fn markdown_source(message: &OutputMessage) -> String {
    message
        .content
        .iter()
        .map(|content| match content {
            OutputMessageContent::OutputText(text) => text.text.as_str(),
            OutputMessageContent::Refusal(refusal) => refusal.refusal.as_str(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn markdown_width(width: u16) -> u16 {
    width.saturating_sub(HORIZONTAL_PAD)
}

pub(crate) fn render_markdown(md: &str, render_width: u16) -> (Text<'static>, Vec<String>) {
    let hooks = CodeBlockHooks::new(render_width as usize);
    let codes = hooks.codes();
    let renderer = MarkdownRenderer::new(render_width as usize).with_render_hooks(Box::new(hooks));
    let blocks = renderer.parse(md);
    let text = Text::from(renderer.render(&blocks, &AppTheme));
    let codes = codes.lock().map(|c| c.clone()).unwrap_or_default();
    (text, codes)
}
