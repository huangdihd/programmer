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
//! on top, while the headless agent runner drives a bare `Conversation`. The
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
    EasyInputContent, FunctionCallOutput, FunctionCallOutputItemParam, InputContent, InputItem,
    InputMessage, InputParam, InputRole, InputTextContent, Item, OutputItem, OutputStatus,
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
    /// Bumped whenever an *existing* item is mutated in place (or the list is
    /// replaced wholesale), as opposed to appended to. The renderer's cache is
    /// keyed by item index, so appends are naturally cache-coherent — this
    /// counter is how it notices in-place edits (e.g. the runner folding
    /// post-edit diagnostics into a tool output) now that the runner writes to
    /// the conversation without going through the panel.
    pub(crate) mutation_version: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UsageSummary {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub turns: usize,
    pub last_turn: Option<(u32, u32)>,
}

impl UsageSummary {
    pub fn total_tokens(self) -> u64 {
        self.input_tokens + self.output_tokens
    }
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
    /// This is the one place `items` is mutated rather than appended;
    /// `mutation_version` is bumped so the renderer drops its cached layout.
    pub fn append_to_tool_output(&mut self, call_id: &str, extra: &str) -> bool {
        for item in self.items.iter_mut() {
            if let MessageItem::ToolOutput { output, .. } = item
                && output.call_id == call_id
            {
                match &mut output.output {
                    FunctionCallOutput::Text(text) => text.push_str(extra),
                    FunctionCallOutput::Content(content) => {
                        content.push(InputContent::InputText(InputTextContent {
                            text: extra.trim_start().to_string(),
                        }));
                    }
                }
                self.mutation_version += 1;
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

    /// Remove every warning with the given text.
    pub fn remove_warning_string(&mut self, message: &str) -> bool {
        let original_len = self.items.len();
        self.items
            .retain(|item| !matches!(item, MessageItem::Warning(text) if text == message));
        let removed = self.items.len() != original_len;
        if removed {
            self.mutation_version += 1;
        }
        removed
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

    /// Return the insertion point for a compaction boundary that leaves the
    /// requested number of most-recent turns verbatim.
    pub fn compaction_cutoff(&self, keep_recent_turns: usize) -> Option<usize> {
        self.compaction_cutoff_before(keep_recent_turns, self.items.len())
    }

    /// Like compaction_cutoff, but only considers the stable prefix before
    /// stable_end. Automatic compaction passes the start of the in-flight turn
    /// here so that turn is never summarized.
    pub fn compaction_cutoff_before(
        &self,
        keep_recent_turns: usize,
        stable_end: usize,
    ) -> Option<usize> {
        let stable_end = stable_end.min(self.items.len());
        let live_start = self.items[..stable_end]
            .iter()
            .rposition(|item| matches!(item, MessageItem::Compacted { .. }))
            .map_or(0, |index| index + 1);

        // A request begins with one or more adjacent user/developer messages.
        // Count that group as a single turn. This deliberately does not depend
        // on Usage items because some compatible providers omit usage data.
        let mut turn_starts = Vec::new();
        let mut in_initial_input_group = false;
        for (offset, item) in self.items[live_start..stable_end].iter().enumerate() {
            let is_request_input = matches!(
                item,
                MessageItem::Input(InputItem::Item(Item::Message(ApiMessageItem::Input(_))))
            );
            if is_request_input {
                if !in_initial_input_group {
                    turn_starts.push(live_start + offset);
                }
                in_initial_input_group = true;
            } else {
                in_initial_input_group = false;
            }
        }
        if turn_starts.len() <= keep_recent_turns {
            return None;
        }
        let cutoff = if keep_recent_turns == 0 {
            stable_end
        } else {
            turn_starts[turn_starts.len() - keep_recent_turns]
        };
        self.items[live_start..cutoff]
            .iter()
            .any(|item| matches!(item, MessageItem::Input(_) | MessageItem::Output(_)))
            .then_some(cutoff)
    }

    /// Build the model input for the stable prefix ending at `cutoff`.
    pub fn input_param_for_prefix(
        &self,
        cutoff: usize,
        current_model: &str,
        vision_enabled: bool,
    ) -> InputParam {
        let prefix = Conversation {
            items: self.items[..cutoff.min(self.items.len())].to_vec(),
            accumulated_usage: (0, 0),
            mutation_version: 0,
        };
        prefix.to_input_param_with_vision(current_model, None, None, None, vision_enabled)
    }

    /// Install a compaction boundary at a snapshotted historical cutoff. New
    /// messages appended while the summary was generated remain after it.
    pub fn apply_compaction_at(&mut self, cutoff: usize, summary: String) -> bool {
        if cutoff > self.items.len() {
            return false;
        }
        self.items
            .insert(cutoff, MessageItem::Compacted { summary });
        self.mutation_version = self.mutation_version.wrapping_add(1);
        true
    }

    pub fn add_usage(&mut self, input_tokens: u32, output_tokens: u32) {
        self.accumulated_usage.0 += input_tokens;
        self.accumulated_usage.1 += output_tokens;
    }

    pub fn usage_summary(&self) -> UsageSummary {
        let mut summary = UsageSummary::default();
        for item in &self.items {
            if let MessageItem::Usage(input, output) = item {
                summary.input_tokens += u64::from(*input);
                summary.output_tokens += u64::from(*output);
                summary.turns += 1;
                summary.last_turn = Some((*input, *output));
            }
        }

        let (input, output) = self.accumulated_usage;
        if input > 0 || output > 0 {
            summary.input_tokens += u64::from(input);
            summary.output_tokens += u64::from(output);
            summary.turns += 1;
            summary.last_turn = Some((input, output));
        }
        summary
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
        self.mutation_version += 1;
    }

    /// Replace the conversation with a previous session's items.
    pub fn restore_items(&mut self, items: Vec<MessageItem>) {
        self.items = items;
        self.mutation_version += 1;
    }

    pub fn truncate(&mut self, cutoff: usize) {
        self.items.truncate(cutoff);
        self.accumulated_usage = (0, 0);
        self.mutation_version = self.mutation_version.wrapping_add(1);
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
        self.to_input_param_with_vision(current_model, skill_prompt, plan_prompt, coauthor, true)
    }

    /// Build API input while optionally omitting stored images. Disabling
    /// vision is reversible: the original message items remain untouched and
    /// are included again after `/vision on`.
    pub fn to_input_param_with_vision(
        &self,
        current_model: &str,
        skill_prompt: Option<&str>,
        plan_prompt: Option<&str>,
        coauthor: Option<&str>,
        vision_enabled: bool,
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
                recorded_outputs
                    .entry(output.call_id.as_str())
                    .or_insert(output);
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
                    input_items.push(input_for_vision(input_item, vision_enabled));
                }
                MessageItem::Meta { text, .. } => {
                    input_items.append(&mut pending_outputs);
                    input_items.push(InputItem::from(Item::Message(ApiMessageItem::Input(
                        InputMessage {
                            content: vec![InputContent::InputText(InputTextContent {
                                text: text.clone(),
                            })],
                            role: InputRole::User,
                            status: Some(OutputStatus::Completed),
                        },
                    ))));
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
                            Some(output) => function_output_for_vision(output, vision_enabled),
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

    /// Number of image parts retained in the full session history.
    pub fn image_count(&self) -> usize {
        self.items
            .iter()
            .map(|item| match item {
                MessageItem::Input(input) => input_image_count(input),
                MessageItem::ToolOutput { output, .. } => {
                    function_output_image_count(&output.output)
                }
                _ => 0,
            })
            .sum()
    }
}

fn input_for_vision(input: &InputItem, vision_enabled: bool) -> InputItem {
    if vision_enabled {
        return input.clone();
    }
    match input {
        InputItem::Item(Item::Message(ApiMessageItem::Input(message))) => {
            let mut message = message.clone();
            message.content = omit_images(&message.content);
            InputItem::from(Item::Message(ApiMessageItem::Input(message)))
        }
        InputItem::EasyMessage(message) => {
            let mut message = message.clone();
            if let EasyInputContent::ContentList(content) = &message.content {
                message.content = EasyInputContent::ContentList(omit_images(content));
            }
            InputItem::EasyMessage(message)
        }
        _ => input.clone(),
    }
}

fn omit_images(content: &[InputContent]) -> Vec<InputContent> {
    content
        .iter()
        .map(|part| match part {
            InputContent::InputImage(_) => InputContent::InputText(InputTextContent {
                text: "[image omitted: vision is off]".to_string(),
            }),
            _ => part.clone(),
        })
        .collect()
}

fn function_output_for_vision(
    output: &FunctionCallOutputItemParam,
    vision_enabled: bool,
) -> FunctionCallOutputItemParam {
    if vision_enabled {
        return output.clone();
    }
    let mut output = output.clone();
    if let FunctionCallOutput::Content(content) = &output.output {
        output.output = FunctionCallOutput::Content(omit_images(content));
    }
    output
}

fn content_image_count(content: &[InputContent]) -> usize {
    content
        .iter()
        .filter(|part| matches!(part, InputContent::InputImage(_)))
        .count()
}

fn function_output_image_count(output: &FunctionCallOutput) -> usize {
    match output {
        FunctionCallOutput::Content(content) => content_image_count(content),
        FunctionCallOutput::Text(_) => 0,
    }
}

fn input_image_count(input: &InputItem) -> usize {
    let content = match input {
        InputItem::Item(Item::Message(ApiMessageItem::Input(message))) => {
            Some(message.content.as_slice())
        }
        InputItem::EasyMessage(message) => match &message.content {
            EasyInputContent::ContentList(content) => Some(content.as_slice()),
            EasyInputContent::Text(_) => None,
        },
        _ => None,
    };
    content.map_or(0, content_image_count)
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

    fn tool_output_content(items: &[InputItem]) -> Option<&[InputContent]> {
        items.iter().find_map(|item| match item {
            InputItem::Item(Item::FunctionCallOutput(output)) => match &output.output {
                FunctionCallOutput::Content(content) => Some(content.as_slice()),
                FunctionCallOutput::Text(_) => None,
            },
            _ => None,
        })
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
    fn remove_warning_string_removes_only_matching_warnings() {
        let mut conv = Conversation::new();
        conv.add_warning_string("temporary");
        conv.add_info_string("keep");
        conv.add_warning_string("temporary");
        conv.add_warning_string("keep warning");

        assert!(conv.remove_warning_string("temporary"));
        assert_eq!(conv.mutation_version, 1);
        assert_eq!(conv.items.len(), 2);
        assert!(matches!(&conv.items[0], MessageItem::Info(text) if text == "keep"));
        assert!(matches!(&conv.items[1], MessageItem::Warning(text) if text == "keep warning"));
        assert!(!conv.remove_warning_string("missing"));
        assert_eq!(conv.mutation_version, 1);
    }

    #[test]
    fn compaction_cutoff_keeps_recent_complete_and_active_turns() {
        let mut conv = Conversation::new();
        for turn in 1..=3 {
            conv.add_input_message(user_message(&format!("turn {turn}")));
            conv.add_output(assistant_text(&format!("answer {turn}")));
            conv.add_usage(10, 2);
            conv.flush_usage();
        }
        let stable_end = conv.items.len();
        conv.add_input_message(user_message("active turn"));

        let cutoff = conv
            .compaction_cutoff_before(1, stable_end)
            .expect("old prefix");
        assert!(matches!(conv.items[cutoff - 1], MessageItem::Usage(_, _)));
        assert!(
            conv.items[cutoff..]
                .iter()
                .any(|item| { matches!(item, MessageItem::Input(_)) })
        );

        assert!(conv.apply_compaction_at(cutoff, "summary".to_string()));
        let InputParam::Items(items) = conv.to_input_param("test/model", None, None, None) else {
            panic!("expected item input");
        };
        let rendered = format!("{items:?}");
        assert!(rendered.contains("summary"));
        assert!(rendered.contains("turn 3"));
        assert!(rendered.contains("active turn"));
        assert!(!rendered.contains("turn 1"));
    }

    #[test]
    fn compaction_cutoff_does_not_require_provider_usage() {
        let mut conv = Conversation::new();
        for turn in 1..=3 {
            conv.add_input_message(user_message(&format!("turn {turn}")));
            conv.add_output(assistant_text(&format!("answer {turn}")));
        }

        let cutoff = conv.compaction_cutoff(1).expect("old prefix");
        let prefix = format!(
            "{:?}",
            conv.input_param_for_prefix(cutoff, "test/model", false)
        );
        assert!(prefix.contains("turn 1"));
        assert!(prefix.contains("turn 2"));
        assert!(!prefix.contains("turn 3"));
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
    fn vision_off_omits_but_retains_images() {
        use async_openai::types::responses::{ImageDetail, InputImageContent};

        let mut conv = Conversation::new();
        conv.add_input_message(ApiMessageItem::Input(InputMessage {
            content: vec![
                InputContent::InputText("inspect @cat.png".into()),
                InputContent::InputImage(InputImageContent {
                    detail: ImageDetail::Auto,
                    file_id: None,
                    image_url: Some("data:image/png;base64,AAAA".to_string()),
                }),
            ],
            role: InputRole::User,
            status: Some(OutputStatus::Completed),
        }));
        assert_eq!(conv.image_count(), 1);

        let InputParam::Items(off) =
            conv.to_input_param_with_vision("test/model", None, None, None, false)
        else {
            panic!("expected items");
        };
        let off_content = off
            .iter()
            .find_map(|item| match item {
                InputItem::Item(Item::Message(ApiMessageItem::Input(message)))
                    if message.role == InputRole::User =>
                {
                    Some(&message.content)
                }
                _ => None,
            })
            .unwrap();
        assert!(
            !off_content
                .iter()
                .any(|part| matches!(part, InputContent::InputImage(_)))
        );
        assert!(off_content.iter().any(|part| matches!(
            part,
            InputContent::InputText(text) if text.text.contains("image omitted")
        )));

        let InputParam::Items(on) =
            conv.to_input_param_with_vision("test/model", None, None, None, true)
        else {
            panic!("expected items");
        };
        assert!(on.iter().any(|item| matches!(
            item,
            InputItem::Item(Item::Message(ApiMessageItem::Input(message)))
                if message.content.iter().any(|part| matches!(part, InputContent::InputImage(_)))
        )));
    }

    #[test]
    fn vision_switch_filters_images_returned_by_tools() {
        use async_openai::types::responses::{ImageDetail, InputImageContent};

        let mut conv = Conversation::new();
        conv.add_output(call("read_image_call"));
        conv.add_tool_output(crate::tools::ToolOutput {
            param: FunctionCallOutputItemParam {
                call_id: "read_image_call".to_string(),
                output: FunctionCallOutput::Content(vec![
                    InputContent::InputText("Read image cat.png (1x1).".into()),
                    InputContent::InputImage(InputImageContent {
                        detail: ImageDetail::Auto,
                        file_id: None,
                        image_url: Some("data:image/png;base64,AAAA".to_string()),
                    }),
                ]),
                id: None,
                status: None,
            },
            failed: false,
            approval_label: None,
        });
        assert_eq!(conv.image_count(), 1);

        let InputParam::Items(off) =
            conv.to_input_param_with_vision("test/model", None, None, None, false)
        else {
            panic!("expected items");
        };
        let off_content = tool_output_content(&off).expect("multimodal tool output");
        assert!(
            !off_content
                .iter()
                .any(|part| matches!(part, InputContent::InputImage(_)))
        );
        assert!(off_content.iter().any(|part| matches!(
            part,
            InputContent::InputText(text) if text.text.contains("image omitted")
        )));

        let InputParam::Items(on) =
            conv.to_input_param_with_vision("test/model", None, None, None, true)
        else {
            panic!("expected items");
        };
        assert!(
            tool_output_content(&on)
                .expect("multimodal tool output")
                .iter()
                .any(|part| matches!(part, InputContent::InputImage(_)))
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

    #[test]
    fn usage_summary_includes_finished_and_current_turns() {
        let mut conv = Conversation::new();
        conv.add_usage(10, 5);
        assert!(conv.flush_usage());
        conv.add_usage(3, 2);

        assert_eq!(
            conv.usage_summary(),
            UsageSummary {
                input_tokens: 13,
                output_tokens: 7,
                turns: 2,
                last_turn: Some((3, 2)),
            }
        );
        assert_eq!(conv.usage_summary().total_tokens(), 20);
    }
}
