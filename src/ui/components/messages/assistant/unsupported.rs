use async_openai::types::responses::OutputItem;
use ratatui::text::{Line, Span, Text};

use super::muted_style;

/// Placeholder for assistant output items the UI doesn't render specially yet
/// (tool calls, etc.). Shows the item's `type` so it's still identifiable.
pub struct UnsupportedMessage<'a> {
    output_item: &'a OutputItem,
}

impl<'a> UnsupportedMessage<'a> {
    pub fn new(output_item: &'a OutputItem) -> Self {
        Self { output_item }
    }

    pub fn into_text(self) -> Text<'static> {
        let ty = serde_json::to_value(self.output_item)
            .ok()
            .and_then(|value| value.get("type").and_then(|t| t.as_str()).map(String::from))
            .unwrap_or_else(|| "unknown".to_string());
        Text::from(Line::from(Span::styled(
            format!("[Unsupported message: {ty}]"),
            muted_style(),
        )))
    }
}
