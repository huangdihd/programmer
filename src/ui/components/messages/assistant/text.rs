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
    let md = normalize_math(md);
    let blocks = renderer.parse(&md);
    Text::from(renderer.render(&blocks, &AppTheme))
}

fn normalize_math(markdown: &str) -> String {
    let mut output = Vec::new();
    let mut display_math = None::<Vec<&str>>;
    let mut fenced_code = None::<(char, usize)>;

    for line in markdown.split('\n') {
        let trimmed = line.trim();
        if let Some(lines) = display_math.as_mut() {
            if trimmed == "$$" {
                let fence = "`".repeat(math_fence_len(lines));
                output.push(format!("{fence}math"));
                output.extend(lines.drain(..).map(str::to_string));
                output.push(fence);
                display_math = None;
            } else {
                lines.push(line);
            }
            continue;
        }

        if let Some((marker, length)) = fenced_code {
            output.push(line.to_string());
            if closing_fence(line, marker, length) {
                fenced_code = None;
            }
            continue;
        }
        if let Some(fence) = opening_fence(line) {
            fenced_code = Some(fence);
            output.push(line.to_string());
            continue;
        }

        if trimmed == "$$" {
            display_math = Some(Vec::new());
        } else if let Some(formula) = trimmed
            .strip_prefix("$$")
            .and_then(|value| value.strip_suffix("$$"))
            .filter(|value| !value.is_empty())
        {
            let formula = formula.trim();
            let fence = "`".repeat(math_fence_len(&[formula]));
            output.push(format!("{fence}math"));
            output.push(formula.to_string());
            output.push(fence);
        } else {
            output.push(normalize_inline_math(line));
        }
    }

    if let Some(lines) = display_math {
        output.push("$$".to_string());
        output.extend(lines.into_iter().map(str::to_string));
    }
    output.join("\n")
}

fn opening_fence(line: &str) -> Option<(char, usize)> {
    let trimmed = line.trim_start();
    let marker = trimmed.chars().next()?;
    if !matches!(marker, '`' | '~') {
        return None;
    }
    let length = trimmed.chars().take_while(|ch| *ch == marker).count();
    (length >= 3).then_some((marker, length))
}

fn closing_fence(line: &str, marker: char, opening_length: usize) -> bool {
    let trimmed = line.trim();
    let length = trimmed.chars().take_while(|ch| *ch == marker).count();
    length >= opening_length && trimmed.chars().skip(length).all(char::is_whitespace)
}

fn math_fence_len(lines: &[&str]) -> usize {
    lines
        .iter()
        .flat_map(|line| line.split(|ch| ch != '`'))
        .map(str::len)
        .max()
        .unwrap_or(0)
        .max(2)
        + 1
}

fn normalize_inline_math(line: &str) -> String {
    let mut output = String::with_capacity(line.len());
    let mut index = 0;

    while index < line.len() {
        let Some(relative) = line[index..].find(['`', '$']) else {
            output.push_str(&line[index..]);
            break;
        };
        let start = index + relative;
        output.push_str(&line[index..start]);

        if line.as_bytes()[start] == b'`' {
            let ticks = line[start..]
                .bytes()
                .take_while(|byte| *byte == b'`')
                .count();
            if delimiter_is_escaped(line, start) {
                output.push_str(&line[start..start + ticks]);
                index = start + ticks;
            } else if let Some(end) = code_span_end(line, start + ticks, ticks) {
                output.push_str(&line[start..end]);
                index = end;
            } else {
                output.push_str(&line[start..start + ticks]);
                index = start + ticks;
            }
            continue;
        }

        if line[start + 1..].starts_with('$') {
            output.push_str("$$");
            index = start + 2;
            continue;
        }
        if delimiter_is_escaped(line, start)
            || line[start + 1..]
                .chars()
                .next()
                .is_none_or(|ch| ch.is_whitespace() || ch.is_ascii_digit())
        {
            output.push('$');
            index = start + 1;
            continue;
        }

        let mut search = start + 1;
        let mut closing = None;
        while let Some(relative) = line[search..].find('$') {
            let candidate = search + relative;
            let before = line[..candidate].chars().next_back();
            let after = line[candidate + 1..].chars().next();
            if !delimiter_is_escaped(line, candidate)
                && before.is_some_and(|ch| !ch.is_whitespace() && ch != '$')
                && after.is_none_or(|ch| !ch.is_ascii_digit() && ch != '$')
            {
                closing = Some(candidate);
                break;
            }
            search = candidate + 1;
        }

        if let Some(end) = closing {
            output.push_str(&crate::ui::markdown_code_block::latex_to_unicode(
                &line[start + 1..end],
            ));
            index = end + 1;
        } else {
            output.push('$');
            index = start + 1;
        }
    }
    output
}

fn code_span_end(line: &str, mut search: usize, ticks: usize) -> Option<usize> {
    while let Some(relative) = line[search..].find('`') {
        let candidate = search + relative;
        let length = line[candidate..]
            .bytes()
            .take_while(|byte| *byte == b'`')
            .count();
        if length == ticks {
            return Some(candidate + ticks);
        }
        search = candidate + length;
    }
    None
}

fn delimiter_is_escaped(line: &str, index: usize) -> bool {
    line[..index]
        .bytes()
        .rev()
        .take_while(|byte| *byte == b'\\')
        .count()
        % 2
        == 1
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
    fn math_normalization_handles_inline_display_and_code_literals() {
        let markdown =
            "Euler: $e^{i\\pi} + 1 = 0$\n\n$$\n\\frac{1}{2}\n$$\n\n~~~sh\necho '$HOME'\n~~~";
        let normalized = normalize_math(markdown);

        assert!(normalized.contains("Euler: e^(iπ) + 1 = 0"), "{normalized}");
        assert!(
            normalized.contains("```math\n\\frac{1}{2}\n```"),
            "{normalized}"
        );
        assert!(normalized.contains("echo '$HOME'"), "{normalized}");
    }

    #[test]
    fn inline_math_preserves_currency_escapes_and_code_spans() {
        let line = r"Pay $5 or $10; `$x$`, ``$y$``, `code `` $hidden$`, \`literal $z$\`, \$HOME, and $$w$$ stay literal; solve $x^2$.";

        assert_eq!(
            normalize_inline_math(line),
            r"Pay $5 or $10; `$x$`, ``$y$``, `code `` $hidden$`, \`literal z\`, \$HOME, and $$w$$ stay literal; solve x²."
        );
    }

    #[test]
    fn display_math_uses_safe_fences_and_leaves_unclosed_blocks_literal() {
        assert_eq!(normalize_math("$$\na ``` b\n$$"), "````math\na ``` b\n````");
        assert_eq!(normalize_math("before\n$$\nx + y"), "before\n$$\nx + y");
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
