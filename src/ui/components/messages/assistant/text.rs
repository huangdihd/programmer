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

use async_openai::types::responses::{OutputMessage, OutputMessageContent};
use ratatui::text::Text;
use ratatui_markdown::markdown::MarkdownRenderer;

use crate::ui::markdown_code_block::CodeBlockHooks;
use crate::ui::markdown_theme::AppTheme;

/// Matches the horizontal padding of the parent `AssistantMessage` block, so the
/// wrapped markdown lines up with the block's content area.
const HORIZONTAL_PAD: u16 = 4;

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

    pub fn into_text(self) -> Text<'static> {
        let md = self
            .message
            .content
            .iter()
            .map(|content| match content {
                OutputMessageContent::OutputText(text) => text.text.clone(),
                OutputMessageContent::Refusal(refusal) => refusal.refusal.clone(),
            })
            .collect::<Vec<_>>()
            .join("\n");

        let render_width = self.width.saturating_sub(HORIZONTAL_PAD).min(100);
        let renderer = MarkdownRenderer::new(render_width as usize)
            .with_render_hooks(Box::new(CodeBlockHooks::new(render_width as usize)));
        let blocks = renderer.parse(&md);
        Text::from(renderer.render(&blocks, &AppTheme))
    }
}
