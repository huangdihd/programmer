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

//! Standalone helper functions and constants that don't depend on `App`.

use crate::response::message_item::MessageItem;
use async_openai::types::responses::{InputContent, InputItem, MessageItem as ApiMessageItem};

// ---------------------------------------------------------------------------
// PROJECT.md overview reminder
// ---------------------------------------------------------------------------

/// Whether the project's diagnostics profile declares at least one LSP checker.
pub(crate) fn lsp_checker_configured() -> bool {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::Path::new(".").to_path_buf());
    matches!(
        crate::diagnostics::DiagnosticsProfile::load(&cwd),
        Some(Ok(profile))
            if profile
                .checkers
                .iter()
                .any(|c| c.kind == crate::diagnostics::CheckerKind::Lsp)
    )
}

// ---------------------------------------------------------------------------
// Response parsing helpers
// ---------------------------------------------------------------------------

/// Extract the text of the first user message from a list of items.
pub(crate) fn first_user_text(items: &[MessageItem]) -> Option<String> {
    items.iter().find_map(|item| match item {
        MessageItem::Input(input) => extract_input_text(input),
        _ => None,
    })
}

pub(crate) fn extract_input_text(input: &InputItem) -> Option<String> {
    use async_openai::types::responses::Item;

    match input {
        InputItem::Item(Item::Message(ApiMessageItem::Input(input_msg))) => {
            input_msg.content.iter().find_map(|c| match c {
                InputContent::InputText(t) => Some(t.text.clone()),
                _ => None,
            })
        }
        InputItem::EasyMessage(msg) => match &msg.content {
            async_openai::types::responses::EasyInputContent::Text(t) => Some(t.clone()),
            async_openai::types::responses::EasyInputContent::ContentList(parts) => {
                parts.iter().find_map(|c| match c {
                    InputContent::InputText(t) => Some(t.text.clone()),
                    _ => None,
                })
            }
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_input(text: &str) -> MessageItem {
        let input: InputItem = serde_json::from_value(serde_json::json!({
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": text}]
        }))
        .unwrap();
        MessageItem::Input(input)
    }

    #[test]
    fn first_user_text_extracts_from_input() {
        assert_eq!(
            first_user_text(&[user_input("hello")]).as_deref(),
            Some("hello")
        );
    }

    #[test]
    fn first_user_text_skips_non_input_items() {
        let items = vec![
            MessageItem::Info("skip me".to_string()),
            user_input("user says hi"),
            user_input("second message"),
        ];
        assert_eq!(first_user_text(&items).as_deref(), Some("user says hi"));
    }

    #[test]
    fn first_user_text_returns_none_for_empty() {
        assert!(first_user_text(&[]).is_none());
        assert!(first_user_text(&[MessageItem::Info("no user input".to_string())]).is_none());
    }

    #[test]
    fn extract_input_text_handles_non_message() {
        // serde_json will just fail to deserialize a function call as an InputItem
        let input: InputItem = serde_json::from_value(serde_json::json!({
            "type": "function_call_output",
            "call_id": "c1",
            "output": "result"
        }))
        .unwrap();
        assert!(extract_input_text(&input).is_none());
    }
}
