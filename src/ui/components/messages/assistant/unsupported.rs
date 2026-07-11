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
