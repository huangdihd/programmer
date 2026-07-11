use async_openai::types::responses::{ReasoningItem, ReasoningItemContent, SummaryPart};
use ratatui::text::{Line, Span, Text};

use super::muted_style;

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
                lines.push(Line::from(Span::styled(format!("  {line}"), muted_style())));
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
