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

//! Shared context-compaction request construction and response handling.

use async_openai::types::responses::{
    InputContent, InputMessage, InputRole, InputTextContent, MessageItem as ApiMessageItem,
    OutputStatus,
};

/// A completed context-compaction request and its provider-reported token
/// usage.
pub struct CompactionResult {
    pub(crate) summary: String,
    pub(crate) input_tokens: Option<u32>,
    pub(crate) output_tokens: Option<u32>,
}

/// Build the no-tools request shared by foreground and background compaction.
pub(crate) fn build_compact_request(
    input_items: Vec<async_openai::types::responses::InputItem>,
    model_name: String,
    thinking_level: crate::thinking::ThinkingLevel,
) -> async_openai::types::responses::CreateResponse {
    async_openai::types::responses::CreateResponse {
        input: async_openai::types::responses::InputParam::Items(input_items),
        model: Some(model_name),
        reasoning: thinking_level.reasoning(),
        ..Default::default()
    }
}

pub(crate) fn compact_input_items(
    input: async_openai::types::responses::InputParam,
) -> Vec<async_openai::types::responses::InputItem> {
    use async_openai::types::responses::{InputItem, InputParam, Item};
    let mut items = match input {
        InputParam::Items(items) => items,
        InputParam::Text(text) => vec![InputItem::from(Item::Message(ApiMessageItem::Input(
            InputMessage {
                content: vec![InputContent::InputText(InputTextContent { text })],
                role: InputRole::User,
                status: Some(OutputStatus::Completed),
            },
        )))],
    };
    items.push(InputItem::from(Item::Message(ApiMessageItem::Input(
        InputMessage {
            content: vec![InputContent::InputText(InputTextContent {
                text: crate::prompts::COMPACT_PROMPT.to_string(),
            })],
            role: InputRole::User,
            status: Some(OutputStatus::Completed),
        },
    ))));
    items
}

pub(crate) fn compact_response_text(
    response: async_openai::types::responses::Response,
) -> Result<String, String> {
    use async_openai::types::responses::{OutputItem, OutputMessageContent};
    let text = response
        .output
        .iter()
        .filter_map(|item| match item {
            OutputItem::Message(message) => {
                Some(message.content.iter().filter_map(|content| match content {
                    OutputMessageContent::OutputText(text) => Some(text.text.as_str()),
                    _ => None,
                }))
            }
            _ => None,
        })
        .flatten()
        .collect::<Vec<_>>()
        .join("\n");
    if text.trim().is_empty() {
        Err("the model returned an empty summary".to_string())
    } else {
        Ok(text)
    }
}

/// Stream a compaction response so long summaries keep the provider connection
/// active, then finalize the accumulated events through the same response
/// machinery as a normal turn.
pub(crate) async fn stream_compact_response(
    client: &async_openai::Client<async_openai::config::OpenAIConfig>,
    request: async_openai::types::responses::CreateResponse,
    cancel: crate::cancel::CancellationToken,
) -> Result<CompactionResult, String> {
    let mut partial = crate::response::partial_response::PartialResponse::new(cancel.clone());
    let mut stream_error = None;
    let retrying = std::sync::atomic::AtomicBool::new(false);

    crate::runner::stream::stream_with_retries(
        client,
        &request,
        &cancel,
        &retrying,
        crate::consts::MAX_STREAM_RETRIES,
        |event| match event {
            Ok(event) => partial.handle_response_stream_event(event),
            Err(error) => stream_error = Some(error),
        },
    )
    .await;

    if cancel.is_cancelled() {
        return Err("cancelled".to_string());
    }
    if let Some(error) = stream_error {
        return Err(error_chain(&error));
    }

    let usage = partial.usage;
    let summary = partial
        .finalize()
        .map_err(|error| error_chain(&error))
        .and_then(compact_response_text)?;
    Ok(CompactionResult {
        summary,
        input_tokens: usage.map(|(input, _, _)| input),
        output_tokens: usage.map(|(_, output, _)| output),
    })
}

/// Include nested transport causes hidden by reqwest's top-level Display text.
pub(crate) fn error_chain(error: &dyn std::error::Error) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}
