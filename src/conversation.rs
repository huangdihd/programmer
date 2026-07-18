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

//! The UI-free conversation model: the list of [`MessageItem`]s that is both
//! the record of a session and the source of the API request history, plus the
//! accumulated token usage of the current turn.
//!
//! This is deliberately independent of any rendering: [`ConversationPanel`]
//! embeds a `Conversation` and adds the view state (scroll, folding, selection)
//! on top, while the headless agent engine drives a bare `Conversation`. The
//! history-shaping logic in [`Conversation::to_input_param`] — call/output
//! grouping, orphaned-call synthesis, and the compaction boundary — is the one
//! piece both paths must agree on exactly, so it lives here once.
//!
//! [`ConversationPanel`]: crate::ui::components::conversation_panel::conversation_panel::ConversationPanel

use crate::prompts::SYSTEM_PROMPT;
use crate::response::message_item::MessageItem;
use async_openai::error::OpenAIError;
use async_openai::types::responses::MessageItem as ApiMessageItem;
use async_openai::types::responses::{
    FunctionCallOutput, FunctionCallOutputItemParam, InputContent, InputItem, InputMessage,
    InputParam, InputRole, InputTextContent, Item, OutputItem, OutputStatus,
};
use std::collections::HashMap;

/// The conversation history and turn-usage counter, free of any UI state.
#[derive(Debug, Default)]
pub struct Conversation {
    /// Every message in the conversation, in order. Rendered by the panel and
    /// mapped to API input items by [`Conversation::to_input_param`].
    pub(crate) items: Vec<MessageItem>,
    /// Accumulated token usage `(input, output)` across all responses in the
    /// current turn (a turn may span multiple responses when tool calls are
    /// involved). Flushed to a [`MessageItem::Usage`] at turn end.
    pub accumulated_usage: (u32, u32),
}

impl Conversation {
    pub fn new() -> Self {
        Conversation::default()
    }

    /// Appends a tool result so it is both rendered and sent back to the model
    /// on the next request. The `failed` flag is authoritative (reported by the
    /// tool via [`crate::tools::run_tool_call`]), stored alongside the output so
    /// renderers and the classifier never re-parse the text for an `error:`
    /// prefix.
    pub fn add_tool_output(&mut self, output: crate::tools::ToolOutput) {
        self.items.push(MessageItem::ToolOutput {
            output: output.param,
            failed: output.failed,
            approval_label: output.approval_label,
        });
    }

    /// Append text to the stored output of the tool call identified by
    /// `call_id`, so post-edit feedback (diagnostics) renders inside that call's
    /// result — visible when the user expands it — and is sent to the model as
    /// part of the tool result. Returns whether a matching output was found.
    ///
    /// This is the one place `items` is mutated rather than appended; the caller
    /// (the panel wrapper) drops the affected cache entry to force a re-render.
    pub fn append_to_tool_output(&mut self, call_id: &str, extra: &str) -> bool {
        for item in self.items.iter_mut() {
            if let MessageItem::ToolOutput { output, .. } = item
                && output.call_id == call_id {
                    match &mut output.output {
                        FunctionCallOutput::Text(text) => text.push_str(extra),
                        other => *other = FunctionCallOutput::Text(extra.trim_start().to_string()),
                    }
                    return true;
                }
        }
        false
    }

    pub fn add_input_message(&mut self, input_message_item: ApiMessageItem) {
        self.items
            .push(MessageItem::Input(InputItem::Item(Item::from(
                input_message_item,
            ))));
    }

    /// Push a raw output item produced by the model (message, reasoning, or a
    /// function call) into the history.
    pub fn add_output(&mut self, output: OutputItem) {
        self.items.push(MessageItem::Output(output));
    }

    pub fn add_error(&mut self, openai_error: OpenAIError) {
        // Non-conforming providers send error payloads the stream parser can't
        // deserialize; surface the embedded API error message instead of the
        // raw "missing field" noise when the payload is recognizable.
        if let OpenAIError::JSONDeserialize(_, content) = &openai_error
            && let Ok(value) = serde_json::from_str::<serde_json::Value>(content)
                && let Some(message) = value
                    .get("message")
                    .or_else(|| value.get("error").and_then(|e| e.get("message")))
                    .and_then(|m| m.as_str())
                {
                    let code = value
                        .get("code")
                        .or_else(|| value.get("error").and_then(|e| e.get("code")))
                        .and_then(|c| c.as_str())
                        .unwrap_or("api error");
                    self.add_error_string(format!("{code}: {message}"));
                    return;
                }
        self.items
            .push(MessageItem::OpenAIError(std::sync::Arc::new(openai_error)));
    }

    pub fn add_error_string(&mut self, message: impl Into<String>) {
        self.items.push(MessageItem::Error(message.into()));
    }

    pub fn add_info_string(&mut self, message: impl Into<String>) {
        self.items.push(MessageItem::Info(message.into()));
    }

    pub fn add_meta(&mut self, label: impl Into<String>, text: impl Into<String>) {
        self.items.push(MessageItem::Meta {
            label: label.into(),
            text: text.into(),
        });
    }

    pub fn add_warning_string(&mut self, message: impl Into<String>) {
        self.items.push(MessageItem::Warning(message.into()));
    }

    /// Whether there is API-visible history worth compacting: any input/output
    /// item after the last `/compact` boundary.
    pub fn has_compactable_history(&self) -> bool {
        let start = self
            .items
            .iter()
            .rposition(|item| matches!(item, MessageItem::Compacted { .. }))
            .map_or(0, |i| i + 1);
        self.items[start..]
            .iter()
            .any(|item| matches!(item, MessageItem::Input(_) | MessageItem::Output(_)))
    }

    /// Record a finished `/compact`: push the boundary carrying `summary`.
    /// History before it stays visible in the UI but stops being sent to the
    /// API (see [`Conversation::to_input_param`]).
    pub fn apply_compaction(&mut self, summary: String) {
        self.items.push(MessageItem::Compacted { summary });
    }

    pub fn add_usage(&mut self, input_tokens: u32, output_tokens: u32) {
        self.accumulated_usage.0 += input_tokens;
        self.accumulated_usage.1 += output_tokens;
    }

    /// Flush the accumulated usage as a [`MessageItem::Usage`] and reset the
    /// counter. Returns whether anything was pushed (so a caller can update its
    /// own view state).
    pub fn flush_usage(&mut self) -> bool {
        let (input, output) = self.accumulated_usage;
        if input > 0 || output > 0 {
            self.items.push(MessageItem::Usage(input, output));
            self.accumulated_usage = (0, 0);
            true
        } else {
            false
        }
    }

    /// Reset the accumulated usage counter (on /clear, new session, etc.).
    pub fn reset_accumulated_usage(&mut self) {
        self.accumulated_usage = (0, 0);
    }

    /// Clear all conversation history and usage.
    pub fn clear(&mut self) {
        self.items.clear();
        self.accumulated_usage = (0, 0);
    }

    /// Replace the conversation with a previous session's items.
    pub fn restore_items(&mut self, items: Vec<MessageItem>) {
        self.items = items;
    }

    /// Iterate over the current conversation items (for persistence and the
    /// classifier context).
    pub fn items(&self) -> impl Iterator<Item = &MessageItem> {
        self.items.iter()
    }

    /// Build the API request input from the conversation history: a developer
    /// system message followed by the post-compaction items, with every
    /// function call immediately followed by its output.
    pub fn to_input_param(
        &self,
        current_model: &str,
        skill_prompt: Option<&str>,
        plan_prompt: Option<&str>,
        coauthor: Option<&str>,
    ) -> InputParam {
        let mut system_prompt = format!(
            "{SYSTEM_PROMPT}\n\nYou are running as model: {current_model}\n\n{}",
            crate::tools::environment_info()
        );
        if let Some(coauthor) = coauthor.map(str::trim).filter(|c| !c.is_empty()) {
            system_prompt.push_str(&format!(
                "\n\nWhen you create a git commit, add this trailer as the last \
                 line(s) of the commit message, after a blank line:\n\
                 Co-Authored-By: {coauthor}"
            ));
        }
        if let Some(prompt) = skill_prompt {
            system_prompt.push_str("\n\n");
            system_prompt.push_str(prompt);
        }
        if let Some(prompt) = plan_prompt {
            system_prompt.push_str("\n\n");
            system_prompt.push_str(prompt);
        }
        let developer_message =
            InputItem::from(Item::Message(ApiMessageItem::Input(InputMessage {
                content: vec![InputContent::InputText(system_prompt.into())],
                role: InputRole::Developer,
                status: Some(OutputStatus::Completed),
            })));

        // A `/compact` boundary replaces everything before it with its summary:
        // only items after the last boundary reach the API.
        let (compact_summary, live_items) = match self
            .items
            .iter()
            .rposition(|item| matches!(item, MessageItem::Compacted { .. }))
        {
            Some(idx) => {
                let MessageItem::Compacted { summary } = &self.items[idx] else {
                    unreachable!()
                };
                (Some(summary.as_str()), &self.items[idx + 1..])
            }
            None => (None, &self.items[..]),
        };

        // Recorded function_call_output items, keyed by call id.
        let mut recorded_outputs: HashMap<&str, &FunctionCallOutputItemParam> = HashMap::new();
        for item in live_items {
            if let MessageItem::ToolOutput { output, .. } = item {
                recorded_outputs.entry(output.call_id.as_str()).or_insert(output);
            }
        }

        // Outputs must stay grouped after the whole assistant output block, in
        // call order (`reasoning, call_1, call_2, output_1, output_2`), matching
        // how OpenAI documents multi-turn tool use. Interleaving them between
        // the calls makes chat-completions-backed providers split the block
        // into several assistant messages, and thinking models (e.g. DeepSeek)
        // then reject the later ones for missing reasoning content.
        let mut input_items = vec![developer_message];
        // The compacted history enters as a user message right after the
        // developer message (not inside it), so the static system-prompt
        // prefix keeps hitting the provider's KV cache.
        if let Some(summary) = compact_summary {
            input_items.push(InputItem::from(Item::Message(ApiMessageItem::Input(
                InputMessage {
                    content: vec![InputContent::InputText(InputTextContent {
                        text: format!(
                            "[The earlier conversation was compacted. Summary of \
                             everything before this point:]\n\n{summary}"
                        ),
                    })],
                    role: InputRole::User,
                    status: Some(OutputStatus::Completed),
                },
            ))));
        }
        let mut pending_outputs: Vec<InputItem> = Vec::new();
        for message_item in live_items {
            match message_item {
                // A stored output marks the boundary after an assistant block:
                // flush that block's outputs here, in call order.
                MessageItem::ToolOutput { .. } => {
                    input_items.append(&mut pending_outputs);
                }
                MessageItem::Input(input_item) => {
                    input_items.append(&mut pending_outputs);
                    input_items.push(input_item.clone());
                }
                MessageItem::Meta { text, .. } => {
                    input_items.append(&mut pending_outputs);
                    input_items.push(InputItem::from(Item::Message(ApiMessageItem::Input(InputMessage {
                        content: vec![InputContent::InputText(InputTextContent { text: text.clone() })],
                        role: InputRole::User,
                        status: Some(OutputStatus::Completed),
                    }))));
                }
                MessageItem::Output(output_item) => {
                    // A non-call output (an assistant message or reasoning the
                    // model emitted *after* its tool calls) closes the tool-call
                    // block: flush the pending outputs first so every
                    // `function_call` stays immediately followed by its
                    // `function_call_output`. Otherwise a trailing message wedges
                    // between a call and its output, and chat-completions-backed
                    // providers reject the assistant tool_calls message for not
                    // being followed by tool results.
                    if !matches!(output_item, OutputItem::FunctionCall(_)) {
                        input_items.append(&mut pending_outputs);
                    }
                    input_items.push(output_item.clone().into());
                    if let OutputItem::FunctionCall(call) = output_item {
                        let output = match recorded_outputs.remove(call.call_id.as_str()) {
                            Some(output) => output.clone(),
                            // A call with no recorded output (e.g. the user
                            // cancelled while the tool was running) would make
                            // the API reject the whole history; answer it
                            // synthetically.
                            None => FunctionCallOutputItemParam {
                                call_id: call.call_id.clone(),
                                output: FunctionCallOutput::Text(
                                    "error: tool execution was cancelled before it completed"
                                        .to_string(),
                                ),
                                id: None,
                                status: None,
                            },
                        };
                        pending_outputs.push(InputItem::from(Item::FunctionCallOutput(output)));
                    }
                }
                _ => {}
            }
        }
        input_items.append(&mut pending_outputs);

        InputParam::Items(input_items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_openai::types::responses::{
        AssistantRole, FunctionToolCall, OutputMessage, OutputMessageContent, OutputTextContent,
    };

    fn user_message(text: &str) -> ApiMessageItem {
        ApiMessageItem::Input(InputMessage {
            content: vec![InputContent::InputText(text.into())],
            role: InputRole::User,
            status: Some(OutputStatus::Completed),
        })
    }

    fn call(call_id: &str) -> OutputItem {
        OutputItem::FunctionCall(FunctionToolCall {
            arguments: "{}".into(),
            call_id: call_id.into(),
            namespace: None,
            name: "command".into(),
            id: None,
            status: None,
        })
    }

    fn output(call_id: &str) -> crate::tools::ToolOutput {
        crate::tools::ToolOutput {
            param: FunctionCallOutputItemParam {
                call_id: call_id.into(),
                output: FunctionCallOutput::Text("ok".into()),
                id: None,
                status: None,
            },
            failed: false,
            approval_label: None,
        }
    }

    fn assistant_text(text: &str) -> OutputItem {
        OutputItem::Message(OutputMessage {
            content: vec![OutputMessageContent::OutputText(OutputTextContent {
                annotations: vec![],
                logprobs: None,
                text: text.into(),
            })],
            id: "msg_1".into(),
            role: AssistantRole::Assistant,
            phase: None,
            status: OutputStatus::Completed,
        })
    }

    #[test]
    fn function_call_outputs_stay_grouped_after_the_assistant_block() {
        let mut conv = Conversation::new();
        conv.add_input_message(user_message("hi"));
        // The model emitted text *after* the call within the same response, so
        // the recorded output ended up separated from its call by that text.
        conv.add_output(call("call_1"));
        conv.add_output(assistant_text("trailing text"));
        conv.add_tool_output(output("call_1"));
        // An orphaned call with no recorded output (cancelled mid-run).
        conv.add_output(call("call_2"));

        let InputParam::Items(items) = conv.to_input_param("test/model", None, None, None) else {
            panic!("expected an item list");
        };
        let kind = |item: &InputItem| match item {
            InputItem::Item(Item::FunctionCall(c)) => format!("call:{}", c.call_id),
            InputItem::Item(Item::FunctionCallOutput(o)) => format!("output:{}", o.call_id),
            InputItem::Item(Item::Message(_)) => "message".to_string(),
            _ => "other".to_string(),
        };
        let kinds: Vec<String> = items.iter().map(kind).collect();
        // Each call must be immediately followed by its output; a message the
        // model emitted after the call is pushed out to *after* that output.
        assert_eq!(
            kinds,
            vec![
                "message", // developer
                "message", // user
                "call:call_1",
                "output:call_1",
                "message", // trailing assistant text, moved after the output
                "call:call_2",
                "output:call_2", // synthesized for the orphaned call
            ]
        );
    }

    #[test]
    fn compaction_replaces_older_history_with_the_summary() {
        let mut conv = Conversation::new();
        conv.add_input_message(user_message("old message one"));
        conv.add_input_message(user_message("old message two"));
        assert!(conv.has_compactable_history());

        conv.apply_compaction("the compact summary".to_string());
        assert!(!conv.has_compactable_history(), "boundary resets history");

        conv.add_input_message(user_message("new message"));
        assert!(conv.has_compactable_history());

        let InputParam::Items(items) = conv.to_input_param("test/model", None, None, None) else {
            panic!("expected an item list");
        };
        let texts: Vec<String> = items
            .iter()
            .filter_map(|item| match item {
                InputItem::Item(Item::Message(ApiMessageItem::Input(m))) => {
                    m.content.iter().find_map(|c| match c {
                        InputContent::InputText(t) => Some(t.text.clone()),
                        _ => None,
                    })
                }
                _ => None,
            })
            .collect();
        // developer message, then the summary, then only post-boundary items.
        assert_eq!(texts.len(), 3, "got: {texts:#?}");
        assert!(texts[1].contains("the compact summary"));
        assert!(texts[2].contains("new message"));
        assert!(
            !texts.iter().any(|t| t.contains("old message")),
            "compacted history must not reach the API"
        );
    }

    #[test]
    fn parallel_call_outputs_are_not_interleaved_between_calls() {
        let mut conv = Conversation::new();
        conv.add_input_message(user_message("run two things"));
        // Two calls in one assistant block, then both outputs.
        conv.add_output(call("call_a"));
        conv.add_output(call("call_b"));
        conv.add_tool_output(output("call_a"));
        conv.add_tool_output(output("call_b"));

        let InputParam::Items(items) = conv.to_input_param("test/model", None, None, None) else {
            panic!("expected an item list");
        };
        let kind = |item: &InputItem| match item {
            InputItem::Item(Item::FunctionCall(c)) => format!("call:{}", c.call_id),
            InputItem::Item(Item::FunctionCallOutput(o)) => format!("output:{}", o.call_id),
            InputItem::Item(Item::Message(_)) => "message".to_string(),
            _ => "other".to_string(),
        };
        let kinds: Vec<String> = items.iter().map(kind).collect();
        assert_eq!(
            kinds,
            vec![
                "message", // developer
                "message", // user
                "call:call_a",
                "call:call_b",
                "output:call_a",
                "output:call_b",
            ]
        );
    }

    #[test]
    fn usage_accumulates_and_flushes_once() {
        let mut conv = Conversation::new();
        conv.add_usage(10, 5);
        conv.add_usage(3, 2);
        assert_eq!(conv.accumulated_usage, (13, 7));
        assert!(conv.flush_usage());
        assert_eq!(conv.accumulated_usage, (0, 0));
        assert!(matches!(conv.items.last(), Some(MessageItem::Usage(13, 7))));
        // A second flush with nothing accumulated pushes nothing.
        assert!(!conv.flush_usage());
    }
}
