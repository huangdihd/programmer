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

use crate::response::response_finish_reason::ResponseFinishReason;
use async_openai::types::responses::ResponseStreamEvent::{
    ResponseCodeInterpreterCallCodeDelta, ResponseCodeInterpreterCallCodeDone, ResponseCompleted,
    ResponseContentPartAdded, ResponseContentPartDone, ResponseCustomToolCallInputDelta,
    ResponseCustomToolCallInputDone, ResponseError, ResponseFailed,
    ResponseFunctionCallArgumentsDelta, ResponseFunctionCallArgumentsDone, ResponseIncomplete,
    ResponseMCPCallArgumentsDelta, ResponseMCPCallArgumentsDone, ResponseOutputItemAdded,
    ResponseOutputItemDone, ResponseOutputTextAnnotationAdded, ResponseOutputTextDelta,
    ResponseOutputTextDone, ResponseReasoningSummaryPartAdded, ResponseReasoningSummaryPartDone,
    ResponseReasoningSummaryTextDelta, ResponseReasoningSummaryTextDone,
    ResponseReasoningTextDelta, ResponseReasoningTextDone, ResponseRefusalDelta,
    ResponseRefusalDone,
};
use async_openai::types::responses::{
    Annotation, OutputContent, OutputItem, OutputMessageContent, ReasoningItemContent,
    ReasoningTextContent, Response, ResponseStreamEvent, SummaryPart, SummaryTextContent,
};
use crate::cancel::CancellationToken;

#[derive(Debug, thiserror::Error)]
pub enum FinalizeError {
    #[error("stream has not finished yet")]
    NotFinished,
    #[error("api error {code:?}: {message}")]
    ApiError {
        code: Option<String>,
        message: String,
        param: Option<String>,
    },
    #[error("stream error: {0}")]
    StreamError(#[from] async_openai::error::OpenAIError),
}

#[derive(Debug)]
pub struct PartialResponse {
    pub items: Vec<Option<OutputItem>>,
    /// Parallel to `items`: whether each output index has received its
    /// `output_item.done` event. Used to tell whether a reasoning item is still
    /// being generated ("Thinking...") or complete ("Thought"), since the item's
    /// own `status` field is only populated for finalized items.
    finished_items: Vec<bool>,
    finish_reason: Option<ResponseFinishReason>,
    /// Cancelled when the user presses Escape to stop the current request.
    pub cancelled: CancellationToken,
    /// Token usage from the completed response: (input_tokens, output_tokens).
    pub usage: Option<(u32, u32)>,
}

impl PartialResponse {
    pub fn new(cancelled: CancellationToken) -> Self {
        PartialResponse {
            items: vec![],
            finished_items: vec![],
            finish_reason: None,
            cancelled,
            usage: None,
        }
    }

    fn set_item(&mut self, item: OutputItem, output_index: u32) {
        if self.items.len() <= output_index as usize {
            self.items.resize((output_index + 1) as usize, None);
            self.finished_items
                .resize((output_index + 1) as usize, false);
        }
        self.items[output_index as usize] = Some(item);
    }

    fn mark_finished(&mut self, output_index: u32) {
        if let Some(finished) = self.finished_items.get_mut(output_index as usize) {
            *finished = true;
        }
    }

    /// Returns the current output items paired with whether each is still in
    /// progress (i.e. has not yet received its `output_item.done` event).
    pub fn get_message_items(&self) -> Vec<(OutputItem, bool)> {
        self.items
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| {
                slot.as_ref().map(|item| {
                    let in_progress = !self.finished_items.get(index).copied().unwrap_or(false);
                    (item.clone(), in_progress)
                })
            })
            .collect()
    }

    pub fn handle_response_stream_event(&mut self, response_stream_event: ResponseStreamEvent) {
        match response_stream_event {
            ResponseOutputItemAdded(item_added_event) => {
                self.set_item(item_added_event.item, item_added_event.output_index);
            }

            ResponseOutputItemDone(item_done_event) => {
                let output_index = item_done_event.output_index as usize;
                let mut incoming = item_done_event.item;

                if let OutputItem::Reasoning(incoming_reasoning) = &mut incoming
                    && let Some(Some(OutputItem::Reasoning(existing))) =
                        self.items.get(output_index)
                    {
                        if incoming_reasoning.content.is_none() && existing.content.is_some() {
                            incoming_reasoning.content = existing.content.clone();
                        }
                        if incoming_reasoning.summary.is_empty() && !existing.summary.is_empty() {
                            incoming_reasoning.summary = existing.summary.clone();
                        }
                    }

                self.set_item(incoming, item_done_event.output_index);
                self.mark_finished(item_done_event.output_index);
            }

            ResponseContentPartAdded(part_added_event) => {
                if let Some(Some(output_item)) =
                    self.items.get_mut(part_added_event.output_index as usize)
                {
                    let content_index = part_added_event.content_index as usize;
                    match output_item {
                        OutputItem::Message(output_message) => {
                            if output_message.content.len() <= content_index {
                                match part_added_event.part {
                                    OutputContent::OutputText(output_text) => output_message
                                        .content
                                        .push(OutputMessageContent::OutputText(output_text)),
                                    OutputContent::Refusal(refusal) => output_message
                                        .content
                                        .push(OutputMessageContent::Refusal(refusal)),
                                    _ => {}
                                }
                            }
                        }
                        OutputItem::Reasoning(reasoning_item) => {
                            let contents = reasoning_item.content.get_or_insert_with(Vec::new);
                            if contents.len() <= content_index
                                && let OutputContent::ReasoningText(reasoning_text) =
                                    part_added_event.part
                                {
                                    contents
                                        .push(ReasoningItemContent::ReasoningText(reasoning_text));
                                }
                        }
                        _ => {}
                    }
                }
            }

            ResponseContentPartDone(part_done_event) => {
                let Some(Some(OutputItem::Message(output_message))) =
                    self.items.get_mut(part_done_event.output_index as usize)
                else {
                    return;
                };
                let content_index = part_done_event.content_index as usize;
                if content_index < output_message.content.len() {
                    match part_done_event.part {
                        OutputContent::OutputText(output_text) => {
                            output_message.content[content_index] =
                                OutputMessageContent::OutputText(output_text);
                        }
                        OutputContent::Refusal(refusal) => {
                            output_message.content[content_index] =
                                OutputMessageContent::Refusal(refusal);
                        }
                        _ => {}
                    }
                }
            }

            ResponseOutputTextDelta(text_delta_event) => {
                let Some(Some(OutputItem::Message(output_message))) =
                    self.items.get_mut(text_delta_event.output_index as usize)
                else {
                    return;
                };
                let Some(OutputMessageContent::OutputText(output_text)) = output_message
                    .content
                    .get_mut(text_delta_event.content_index as usize)
                else {
                    return;
                };
                output_text.text.push_str(&text_delta_event.delta);
            }

            ResponseOutputTextDone(text_done_event) => {
                let Some(Some(OutputItem::Message(output_message))) =
                    self.items.get_mut(text_done_event.output_index as usize)
                else {
                    return;
                };
                let Some(OutputMessageContent::OutputText(output_text)) = output_message
                    .content
                    .get_mut(text_done_event.content_index as usize)
                else {
                    return;
                };
                output_text.text = text_done_event.text;
            }

            ResponseOutputTextAnnotationAdded(annotation_added_event) => {
                let Some(Some(OutputItem::Message(output_message))) = self
                    .items
                    .get_mut(annotation_added_event.output_index as usize)
                else {
                    return;
                };
                let Some(OutputMessageContent::OutputText(output_text)) = output_message
                    .content
                    .get_mut(annotation_added_event.content_index as usize)
                else {
                    return;
                };
                if output_text.annotations.len() <= annotation_added_event.annotation_index as usize
                    && let Ok(annotation) =
                        serde_json::from_value::<Annotation>(annotation_added_event.annotation)
                    {
                        output_text.annotations.push(annotation);
                    }
            }

            ResponseRefusalDelta(refusal_delta_event) => {
                let Some(Some(OutputItem::Message(output_message))) = self
                    .items
                    .get_mut(refusal_delta_event.output_index as usize)
                else {
                    return;
                };
                let Some(OutputMessageContent::Refusal(refusal)) = output_message
                    .content
                    .get_mut(refusal_delta_event.content_index as usize)
                else {
                    return;
                };
                refusal.refusal.push_str(&refusal_delta_event.delta);
            }

            ResponseRefusalDone(refusal_done_event) => {
                let Some(Some(OutputItem::Message(output_message))) =
                    self.items.get_mut(refusal_done_event.output_index as usize)
                else {
                    return;
                };
                let Some(OutputMessageContent::Refusal(refusal)) = output_message
                    .content
                    .get_mut(refusal_done_event.content_index as usize)
                else {
                    return;
                };
                refusal.refusal = refusal_done_event.refusal;
            }

            ResponseReasoningTextDelta(reasoning_text_delta_event) => {
                let Some(Some(OutputItem::Reasoning(reasoning_item))) = self
                    .items
                    .get_mut(reasoning_text_delta_event.output_index as usize)
                else {
                    return;
                };

                let contents = reasoning_item.content.get_or_insert_with(Vec::new);
                let content_index = reasoning_text_delta_event.content_index as usize;
                while contents.len() <= content_index {
                    contents.push(ReasoningItemContent::ReasoningText(ReasoningTextContent {
                        text: String::new(),
                    }));
                }
                let ReasoningItemContent::ReasoningText(reasoning_text) =
                    &mut contents[content_index];
                reasoning_text
                    .text
                    .push_str(&reasoning_text_delta_event.delta);
            }

            ResponseReasoningTextDone(reasoning_text_done_event) => {
                let Some(Some(OutputItem::Reasoning(reasoning_item))) = self
                    .items
                    .get_mut(reasoning_text_done_event.output_index as usize)
                else {
                    return;
                };
                let Some(contents) = reasoning_item.content.as_mut() else {
                    return;
                };
                if let Some(ReasoningItemContent::ReasoningText(reasoning_text)) =
                    contents.get_mut(reasoning_text_done_event.content_index as usize)
                {
                    reasoning_text.text = reasoning_text_done_event.text;
                }
            }

            ResponseReasoningSummaryPartAdded(summary_part_added_event) => {
                let Some(Some(OutputItem::Reasoning(reasoning_item))) = self
                    .items
                    .get_mut(summary_part_added_event.output_index as usize)
                else {
                    return;
                };
                let summary_index = summary_part_added_event.summary_index as usize;
                if reasoning_item.summary.len() <= summary_index {
                    reasoning_item.summary.push(summary_part_added_event.part);
                }
            }

            ResponseReasoningSummaryPartDone(summary_part_done_event) => {
                let Some(Some(OutputItem::Reasoning(reasoning_item))) = self
                    .items
                    .get_mut(summary_part_done_event.output_index as usize)
                else {
                    return;
                };
                let summary_index = summary_part_done_event.summary_index as usize;
                if summary_index < reasoning_item.summary.len() {
                    reasoning_item.summary[summary_index] = summary_part_done_event.part;
                }
            }

            ResponseReasoningSummaryTextDelta(summary_text_delta_event) => {
                let Some(Some(OutputItem::Reasoning(reasoning_item))) = self
                    .items
                    .get_mut(summary_text_delta_event.output_index as usize)
                else {
                    return;
                };

                let summary_index = summary_text_delta_event.summary_index as usize;
                while reasoning_item.summary.len() <= summary_index {
                    reasoning_item
                        .summary
                        .push(SummaryPart::SummaryText(SummaryTextContent {
                            text: String::new(),
                        }));
                }

                let SummaryPart::SummaryText(summary_text) =
                    &mut reasoning_item.summary[summary_index];
                summary_text.text.push_str(&summary_text_delta_event.delta);
            }

            ResponseReasoningSummaryTextDone(summary_text_done_event) => {
                let Some(Some(OutputItem::Reasoning(reasoning_item))) = self
                    .items
                    .get_mut(summary_text_done_event.output_index as usize)
                else {
                    return;
                };
                let summary_index = summary_text_done_event.summary_index as usize;
                if summary_index < reasoning_item.summary.len() {
                    let SummaryPart::SummaryText(summary_text) =
                        &mut reasoning_item.summary[summary_index];
                    summary_text.text = summary_text_done_event.text;
                }
            }

            ResponseFunctionCallArgumentsDelta(function_call_arguments_delta_event) => {
                let Some(Some(OutputItem::FunctionCall(function_call))) = self
                    .items
                    .get_mut(function_call_arguments_delta_event.output_index as usize)
                else {
                    return;
                };
                function_call
                    .arguments
                    .push_str(&function_call_arguments_delta_event.delta);
            }

            ResponseFunctionCallArgumentsDone(function_call_arguments_done_event) => {
                let Some(Some(OutputItem::FunctionCall(function_call))) = self
                    .items
                    .get_mut(function_call_arguments_done_event.output_index as usize)
                else {
                    return;
                };
                function_call.arguments = function_call_arguments_done_event.arguments;
            }

            ResponseMCPCallArgumentsDelta(mcp_call_arguments_delta_event) => {
                let Some(Some(OutputItem::McpCall(mcp_call))) = self
                    .items
                    .get_mut(mcp_call_arguments_delta_event.output_index as usize)
                else {
                    return;
                };
                mcp_call
                    .arguments
                    .push_str(&mcp_call_arguments_delta_event.delta);
            }

            ResponseMCPCallArgumentsDone(mcp_call_arguments_done_event) => {
                let Some(Some(OutputItem::McpCall(mcp_call))) = self
                    .items
                    .get_mut(mcp_call_arguments_done_event.output_index as usize)
                else {
                    return;
                };
                mcp_call.arguments = mcp_call_arguments_done_event.arguments;
            }

            ResponseCustomToolCallInputDelta(custom_tool_call_input_delta_event) => {
                let Some(Some(OutputItem::CustomToolCall(custom_tool_call))) = self
                    .items
                    .get_mut(custom_tool_call_input_delta_event.output_index as usize)
                else {
                    return;
                };
                custom_tool_call
                    .input
                    .push_str(&custom_tool_call_input_delta_event.delta);
            }

            ResponseCustomToolCallInputDone(custom_tool_call_input_done_event) => {
                let Some(Some(OutputItem::CustomToolCall(custom_tool_call))) = self
                    .items
                    .get_mut(custom_tool_call_input_done_event.output_index as usize)
                else {
                    return;
                };
                custom_tool_call.input = custom_tool_call_input_done_event.input;
            }

            ResponseCodeInterpreterCallCodeDelta(code_delta_event) => {
                let Some(Some(OutputItem::CodeInterpreterCall(code_interpreter_call))) =
                    self.items.get_mut(code_delta_event.output_index as usize)
                else {
                    return;
                };
                code_interpreter_call
                    .code
                    .get_or_insert_with(String::new)
                    .push_str(&code_delta_event.delta);
            }

            ResponseCodeInterpreterCallCodeDone(code_done_event) => {
                let Some(Some(OutputItem::CodeInterpreterCall(code_interpreter_call))) =
                    self.items.get_mut(code_done_event.output_index as usize)
                else {
                    return;
                };
                code_interpreter_call.code = Some(code_done_event.code);
            }

            ResponseCompleted(response_completed_event) => {
                if let Some(ref u) = response_completed_event.response.usage {
                    self.usage = Some((u.input_tokens, u.output_tokens));
                }
                self.finish_reason = Some(ResponseFinishReason::Completed(
                    response_completed_event.response,
                ));
            }
            ResponseFailed(response_failed_event) => {
                if let Some(ref u) = response_failed_event.response.usage {
                    self.usage = Some((u.input_tokens, u.output_tokens));
                }
                self.finish_reason =
                    Some(ResponseFinishReason::Failed(response_failed_event.response));
            }
            ResponseIncomplete(response_incomplete_event) => {
                if let Some(ref u) = response_incomplete_event.response.usage {
                    self.usage = Some((u.input_tokens, u.output_tokens));
                }
                self.finish_reason = Some(ResponseFinishReason::Incomplete(
                    response_incomplete_event.response,
                ));
            }
            ResponseError(response_error_event) => {
                self.finish_reason = Some(ResponseFinishReason::ApiError {
                    code: response_error_event.code,
                    message: response_error_event.message,
                    param: response_error_event.param,
                });
            }
            _ => {}
        }
    }

    pub fn into_parts(self) -> (Option<ResponseFinishReason>, Vec<OutputItem>) {
        let PartialResponse {
            items,
            finish_reason,
            ..
        } = self;
        (finish_reason, items.into_iter().flatten().collect())
    }

    /// Consumes `self` and returns only the output items that are safe to keep
    /// when aborting mid-stream. Incomplete function calls (which lack fully
    /// formed arguments) are dropped so they aren't sent to the model.
    pub fn into_aborted_items(self) -> Vec<OutputItem> {
        self.items
            .into_iter()
            .enumerate()
            .filter_map(|(i, item)| {
                let finished = self.finished_items.get(i).copied().unwrap_or(false);
                item.filter(|item| !matches!(item, OutputItem::FunctionCall(_)) || finished)
            })
            .collect()
    }

    pub fn finished(&self) -> bool {
        self.finish_reason.is_some()
    }

    /// Whether any output has arrived yet. False in the window between the
    /// request being sent and the first output item streaming back, which the
    /// UI shows as "Connecting" rather than "Thinking".
    pub fn started(&self) -> bool {
        self.items.iter().any(Option::is_some) || self.usage.is_some()
    }

    /// Returns true if any output item in this partial response is a function
    /// call (possibly still incomplete).
    pub fn has_function_calls(&self) -> bool {
        self.items.iter().any(|item| {
            item.as_ref()
                .is_some_and(|i| matches!(i, OutputItem::FunctionCall(_)))
        })
    }

    /// Returns true if any output item is a normal message (not reasoning,
    /// not a function call).
    pub fn has_message_items(&self) -> bool {
        self.items.iter().any(|item| {
            item.as_ref()
                .is_some_and(|i| matches!(i, OutputItem::Message(_)))
        })
    }

    pub fn finalize(self) -> Result<Response, FinalizeError> {
        let PartialResponse {
            items,
            finish_reason,
            ..
        } = self;

        let mut response = match finish_reason {
            Some(ResponseFinishReason::Completed(response))
            | Some(ResponseFinishReason::Failed(response))
            | Some(ResponseFinishReason::Incomplete(response)) => response,
            Some(ResponseFinishReason::ApiError {
                code,
                message,
                param,
            }) => {
                return Err(FinalizeError::ApiError {
                    code,
                    message,
                    param,
                });
            }
            Some(ResponseFinishReason::StreamError(stream_error)) => {
                return Err(FinalizeError::StreamError(stream_error));
            }
            None => return Err(FinalizeError::NotFinished),
        };

        let merged: Vec<OutputItem> = items
            .into_iter()
            .enumerate()
            .filter_map(|(index, slot)| slot.or_else(|| response.output.get(index).cloned()))
            .collect();

        if merged.len() >= response.output.len() {
            response.output = merged;
        }

        Ok(response)
    }
}
