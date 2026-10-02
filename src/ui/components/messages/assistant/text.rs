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
const HORIZONTAL_PAD: u16 = 1;

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
    let text = render_markdown_with_hooks(md, render_width, hooks);
    let codes = codes.lock().map(|c| c.clone()).unwrap_or_default();
    (text, codes)
}

/// Render conversation-styled Markdown without an unwired copy control.
/// `render_width` is the available content width; callers own outer padding.
pub(crate) fn render_read_only_markdown(md: &str, render_width: u16) -> Text<'static> {
    let hooks = CodeBlockHooks::new(render_width as usize).without_copy_label();
    render_markdown_with_hooks(md, render_width, hooks)
}

fn render_markdown_with_hooks(md: &str, render_width: u16, hooks: CodeBlockHooks) -> Text<'static> {
    let renderer = MarkdownRenderer::new(render_width as usize).with_render_hooks(Box::new(hooks));
    let blocks = renderer.parse(md);
    Text::from(renderer.render(&blocks, &AppTheme))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::markdown_code_block::COPY_LABEL;

    #[test]
    fn read_only_markdown_matches_conversation_lines_spans_and_styles() {
        let documents = [
            "# Heading\n\n## Subheading",
            "- First item\n- Second item\n\n1. Ordered item\n2. Another item",
            "| Name | Value |\n| --- | --- |\n| alpha | **bold** |\n| beta | `code` |",
            "> Quoted text\n>\n> Another paragraph",
            "Plain **bold**, *italic*, ~~strike~~, `inline code`, and [link](https://example.com).",
        ];

        for width in [16, 40, 80] {
            for document in documents {
                let (conversation, codes) = render_markdown(document, width);
                let read_only = render_read_only_markdown(document, width);
                assert!(codes.is_empty());
                assert!(!conversation.lines.is_empty());
                assert_eq!(read_only, conversation, "width {width}, {document:?}");
            }
        }
    }

    #[test]
    fn read_only_fenced_code_matches_conversation_except_copy_label_rows() {
        let document = "Before **code**.\n\n```rust\nfn main() {\n\tprintln!(\"hello\");\n}\n```\n\nBetween blocks.\n\n```\nplain text\n```\n\nAfter code.";

        for width in [8, 40, 80] {
            let (conversation, codes) = render_markdown(document, width);
            let read_only = render_read_only_markdown(document, width);
            assert_eq!(codes.len(), 2);
            assert_eq!(read_only.style, conversation.style);
            assert_eq!(read_only.alignment, conversation.alignment);
            assert_eq!(read_only.lines.len(), conversation.lines.len());

            let mut copy_rows = 0;
            for (actual, expected) in read_only.lines.iter().zip(&conversation.lines) {
                assert!(!actual.spans.iter().any(|span| span.content == COPY_LABEL));
                if expected.spans.iter().any(|span| span.content == COPY_LABEL) {
                    copy_rows += 1;
                    assert_eq!(actual.width(), expected.width());
                    assert_eq!(actual.style, expected.style);
                    assert_eq!(actual.alignment, expected.alignment);
                    assert_eq!(actual.spans[0], expected.spans[0]);
                    if expected.spans[1].content == "rust" {
                        assert_eq!(actual.spans[1], expected.spans[1]);
                    }
                    continue;
                }
                // Includes highlighted code bodies, padding, and surrounding Markdown.
                assert_eq!(actual, expected, "width {width}");
            }
            assert_eq!(copy_rows, if width == 8 { 0 } else { 2 });
        }
    }
}
