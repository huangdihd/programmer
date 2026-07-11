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

use super::{detail_style, muted_style};

/// Renders the reasoning indicator. Collapsed it's a single line ("Thinking..."
/// while streaming, "Thought" once done); expanded it also shows the reasoning
/// summary/content text. A caret hints that the line can be clicked to toggle.
pub struct ReasoningMessage<'a> {
    in_progress: bool,
    item: &'a ReasoningItem,
    expanded: bool,
}

impl<'a> ReasoningMessage<'a> {
    pub fn new(in_progress: bool, item: &'a ReasoningItem) -> Self {
        Self {
            in_progress,
            item,
            expanded: false,
        }
    }

    pub fn expanded(mut self, expanded: bool) -> Self {
        self.expanded = expanded;
        self
    }

    pub fn into_text(self) -> Text<'static> {
        let label = if self.in_progress {
            "✻ Thinking..."
        } else {
            "✻ Thought"
        };

        let text = reasoning_text(self.item);
        let caret = if text.is_empty() {
            ""
        } else if self.expanded {
            "  ▾"
        } else {
            "  ▸"
        };

        let mut lines = vec![Line::from(Span::styled(
            format!("{label}{caret}"),
            muted_style(),
        ))];

        if self.expanded {
            for line in text.lines() {
                lines.push(Line::from(Span::styled(format!("  {line}"), detail_style())));
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
