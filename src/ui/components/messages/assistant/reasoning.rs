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

use async_openai::types::responses::{ReasoningItem, ReasoningItemContent, SummaryPart};
use ratatui::text::{Line, Span, Text};
use ratatui_markdown::markdown::MarkdownRenderer;

use crate::ui::markdown_code_block::CodeBlockHooks;
use crate::ui::markdown_theme::AppTheme;

use super::muted_style;

/// Combined horizontal padding of the parent [`AssistantMessage`] block
/// (`PAD_LEFT` + `PAD_RIGHT`).
const BLOCK_PAD: u16 = 4;
/// Extra left indent applied to expanded reasoning lines, keeping a visual
/// nesting relationship under the "✻ Thought" header.
const INDENT: u16 = 2;

/// Renders the reasoning indicator. Collapsed it's a single line ("Thinking..."
/// while streaming with animated dots, "Thought" once done); expanded it also
/// shows the reasoning summary/content text. A caret hints that the line can be
/// clicked to toggle.
pub struct ReasoningMessage<'a> {
    in_progress: bool,
    item: &'a ReasoningItem,
    expanded: bool,
    /// Monotonic frame counter; when set and `in_progress` is true, the
    /// "Thinking" label cycles its trailing dots between 0 and 3 to give a
    /// subtle animation effect.
    frame_count: Option<u64>,
    /// Available width for rendering (outer content area before block padding).
    width: u16,
}

impl<'a> ReasoningMessage<'a> {
    pub fn new(in_progress: bool, item: &'a ReasoningItem, width: u16) -> Self {
        Self {
            in_progress,
            item,
            expanded: false,
            frame_count: None,
            width,
        }
    }

    pub fn expanded(mut self, expanded: bool) -> Self {
        self.expanded = expanded;
        self
    }

    pub fn frame_count(mut self, frame_count: Option<u64>) -> Self {
        self.frame_count = frame_count;
        self
    }

    pub fn into_text(self) -> Text<'static> {
        // Cycle through 0..=3 dots every ~8 frames → ~133 ms per state at 60 fps.
        let dots = if self.in_progress {
            match self.frame_count {
                Some(n) => ".".repeat(((n / 8) % 4) as usize),
                None => "...".to_string(),
            }
        } else {
            String::new()
        };

        let label = if self.in_progress {
            format!("✻ Thinking{dots}")
        } else {
            "✻ Thought".to_string()
        };

        let text = reasoning_text(self.item);
        let caret = if text.is_empty() {
            ""
        } else if self.expanded {
            "▾ "
        } else {
            "▸ "
        };

        let mut lines = vec![Line::from(Span::styled(
            format!("{caret}{label}"),
            muted_style(),
        ))];

        if self.expanded && !text.is_empty() {
            let render_width = self
                .width
                .saturating_sub(BLOCK_PAD + INDENT)
                .min(100);
            let renderer = MarkdownRenderer::new(render_width as usize)
                .with_render_hooks(Box::new(CodeBlockHooks::new(render_width as usize)));
            let blocks = renderer.parse(&text);
            let md_lines = renderer.render(&blocks, &AppTheme);
            for line in md_lines {
                let mut spans = vec![Span::raw("  ")];
                spans.extend(
                    line.spans
                        .into_iter()
                        .map(|s| Span::styled(s.content, s.style)),
                );
                lines.push(Line::from(spans));
            }
        }

        Text::from(lines)
    }
}

/// The reasoning text, preferring the summary and falling back to the raw
/// reasoning content.
fn reasoning_text(item: &ReasoningItem) -> String {
    let mut parts: Vec<String> = item
        .summary
        .iter()
        .map(|SummaryPart::SummaryText(summary)| summary.text.clone())
        .collect();

    if parts.is_empty() {
        if let Some(contents) = &item.content {
            parts = contents
                .iter()
                .map(|ReasoningItemContent::ReasoningText(content)| content.text.clone())
                .collect();
        }
    }

    parts.join("\n")
}
