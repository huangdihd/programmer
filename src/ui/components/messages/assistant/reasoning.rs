use ratatui::text::{Line, Span, Text};

use super::muted_style;

/// Renders the one-line reasoning indicator. While the reasoning item is still
/// streaming it reads "Thinking...", and "Thought" once complete.
pub struct ReasoningMessage {
    in_progress: bool,
}

impl ReasoningMessage {
    pub fn new(in_progress: bool) -> Self {
        Self { in_progress }
    }

    pub fn into_text(self) -> Text<'static> {
        let label = if self.in_progress {
            "✻ Thinking..."
        } else {
            "✻ Thought"
        };
        Text::from(Line::from(Span::styled(label, muted_style())))
    }
}
