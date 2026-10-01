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

use async_openai::error::OpenAIError;
use async_openai::types::responses::{
    FunctionCallOutputItemParam, InputContent, InputItem, MessageItem as ApiMessageItem, OutputItem,
};
use std::sync::Arc;

/// Display-only observations; never an execution authorization or completion claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerDelegationState {
    Pending,
    AcceptedQueued,
    Started,
    Rejected,
    Cancelled,
}

#[derive(Debug)]
pub enum MessageItem {
    Input(InputItem),
    Output(OutputItem),
    /// A tool result with a pre-computed failure flag — set once in
    /// `add_tool_output` so renderers and the classifier never parse text.
    ToolOutput {
        output: FunctionCallOutputItemParam,
        failed: bool,
        /// Human-readable label explaining why this tool call was approved or
        /// denied (e.g. "approved by Auto mode", "denied in Manual mode by user").
        approval_label: Option<String>,
    },
    /// A streaming/API error. Held behind an [`Arc`] because [`OpenAIError`] is
    /// not itself `Clone`; the `Arc` lets [`MessageItem::clone`] preserve the
    /// original error losslessly instead of degrading it to a string.
    OpenAIError(Arc<OpenAIError>),
    Error(String),
    Warning(String),
    Info(String),
    /// Persisted UI-only record of a lightweight cross-session exchange.
    PeerExchange {
        id: String,
        from: String,
        question: String,
        answer: Option<String>,
    },
    PeerDelegation {
        id: String,
        from: String,
        /// Source-side status notifications do not carry the original task.
        body: Option<String>,
        state: PeerDelegationState,
    },
    Meta {
        label: String,
        text: String,
    },
    Usage(u32, u32, u32, Option<usize>), // (input, output, cached input, recalled memories)
    /// A `/compact` boundary: everything before this item was summarized into
    /// `summary`, which is sent to the model in place of that history. The
    /// older items stay in the list for the UI scrollback but are no longer
    /// part of the API input.
    Compacted {
        summary: String,
    },
}

impl Clone for MessageItem {
    fn clone(&self) -> Self {
        match self {
            MessageItem::Input(i) => MessageItem::Input(i.clone()),
            MessageItem::Output(o) => MessageItem::Output(o.clone()),
            MessageItem::ToolOutput {
                output,
                failed,
                approval_label,
            } => MessageItem::ToolOutput {
                output: output.clone(),
                failed: *failed,
                approval_label: approval_label.clone(),
            },
            MessageItem::OpenAIError(e) => MessageItem::OpenAIError(e.clone()),
            MessageItem::Error(s) => MessageItem::Error(s.clone()),
            MessageItem::Warning(s) => MessageItem::Warning(s.clone()),
            MessageItem::Info(s) => MessageItem::Info(s.clone()),
            MessageItem::PeerExchange {
                id,
                from,
                question,
                answer,
            } => MessageItem::PeerExchange {
                id: id.clone(),
                from: from.clone(),
                question: question.clone(),
                answer: answer.clone(),
            },
            MessageItem::PeerDelegation {
                id,
                from,
                body,
                state,
            } => MessageItem::PeerDelegation {
                id: id.clone(),
                from: from.clone(),
                body: body.clone(),
                state: *state,
            },
            MessageItem::Meta { label, text } => MessageItem::Meta {
                label: label.clone(),
                text: text.clone(),
            },
            MessageItem::Usage(i, o, cached, recalled) => {
                MessageItem::Usage(*i, *o, *cached, *recalled)
            }
            MessageItem::Compacted { summary } => MessageItem::Compacted {
                summary: summary.clone(),
            },
        }
    }
}

/// Extract the first text part of an input message, regardless of its role.
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

    #[test]
    fn extract_input_text_handles_non_message() {
        let input: InputItem = serde_json::from_value(serde_json::json!({
            "type": "function_call_output",
            "call_id": "c1",
            "output": "result"
        }))
        .unwrap();
        assert!(extract_input_text(&input).is_none());
    }

    #[test]
    fn extract_input_text_handles_easy_text_without_filtering_roles() {
        let input = InputItem::EasyMessage(
            serde_json::from_value(serde_json::json!({
                "role": "developer",
                "content": "instructions"
            }))
            .unwrap(),
        );
        assert_eq!(extract_input_text(&input).as_deref(), Some("instructions"));
    }

    #[test]
    fn extract_input_text_preserves_first_text_part_for_both_message_forms() {
        use async_openai::types::responses::Item;

        for content in [
            serde_json::json!([
                {"type": "input_image", "image_url": "https://example.com/image.png", "detail": "auto"},
                {"type": "input_text", "text": " first "},
                {"type": "input_text", "text": "second"}
            ]),
            serde_json::json!([
                {"type": "input_text", "text": ""},
                {"type": "input_text", "text": "second"}
            ]),
            serde_json::json!([]),
            serde_json::json!([
                {"type": "input_image", "image_url": "https://example.com/image.png", "detail": "auto"}
            ]),
        ] {
            let expected = content
                .as_array()
                .unwrap()
                .iter()
                .find_map(|part| part.get("text").and_then(serde_json::Value::as_str));
            let message = serde_json::json!({"role": "user", "content": content});
            let easy = InputItem::EasyMessage(serde_json::from_value(message.clone()).unwrap());
            let structured = InputItem::Item(Item::Message(ApiMessageItem::Input(
                serde_json::from_value(message).unwrap(),
            )));
            assert_eq!(extract_input_text(&easy).as_deref(), expected);
            assert_eq!(extract_input_text(&structured).as_deref(), expected);
        }
    }
}
