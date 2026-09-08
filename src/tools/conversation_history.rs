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

//! Read-only access to exact conversation items hidden by the latest
//! compaction boundary.

use crate::conversation::Conversation;
use crate::response::message_item::MessageItem;
use async_openai::types::responses::Tool;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

use super::function_tool;

pub const NAME: &str = "conversation_history";
const DEFAULT_LIST_LIMIT: usize = 20;
const MAX_LIST_LIMIT: usize = 50;
const DEFAULT_SEARCH_RESULTS: usize = 10;
const MAX_SEARCH_RESULTS: usize = 20;
const DEFAULT_READ_CHARS: usize = 6_000;
const MAX_READ_CHARS: usize = 7_000;
const PREVIEW_CHARS: usize = 140;
const SEARCH_SNIPPET_CHARS: usize = 240;

pub fn tool() -> Tool {
    function_tool(
        NAME,
        "Search or read exact original conversation items hidden behind the latest context-\
         compaction boundary. Use search to locate details omitted or possibly distorted by the \
         summary, then read the matching item by index. Read supports character-offset paging for \
         long messages and tool outputs. This tool is available only when this session contains \
         compacted history.",
        json!({
            "action": {
                "type": "string",
                "enum": ["list", "search", "read"],
                "description": "list archived item previews, search archived content, or read one exact archived item."
            },
            "query": {
                "type": "string",
                "description": "search only: case-insensitive literal text to find."
            },
            "index": {
                "type": "integer",
                "minimum": 0,
                "description": "read only: archived conversation item index returned by list/search."
            },
            "start_index": {
                "type": "integer",
                "minimum": 0,
                "description": "list only: first archived conversation item index to consider. Default 0."
            },
            "offset": {
                "type": "integer",
                "minimum": 0,
                "description": "read only: character offset within the serialized item. Default 0."
            },
            "limit": {
                "type": "integer",
                "minimum": 1,
                "description": "list: maximum entries (default 20, max 50); read: maximum characters (default 6000, max 7000)."
            },
            "max_results": {
                "type": "integer",
                "minimum": 1,
                "maximum": 20,
                "description": "search only: maximum matching items. Default 10."
            }
        }),
        &["action"],
    )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Action {
    List,
    Search,
    Read,
}

#[derive(Debug, Deserialize)]
struct Args {
    action: Action,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    index: Option<usize>,
    #[serde(default)]
    start_index: Option<usize>,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    max_results: Option<usize>,
}

pub fn available(conversation: &Arc<Mutex<Conversation>>) -> bool {
    conversation
        .lock()
        .unwrap()
        .items()
        .any(|item| matches!(item, MessageItem::Compacted { .. }))
}

pub async fn run(
    arguments: &str,
    conversation: &Arc<Mutex<Conversation>>,
) -> Result<String, String> {
    let args: Args = serde_json::from_str(arguments)
        .map_err(|error| format!("error: invalid arguments: {error}"))?;
    let conversation = conversation.lock().unwrap();
    let items = conversation.items().collect::<Vec<_>>();
    let boundary = items
        .iter()
        .rposition(|item| matches!(item, MessageItem::Compacted { .. }))
        .ok_or_else(|| "error: this session has no compacted conversation history".to_string())?;
    let archived = &items[..boundary];

    match args.action {
        Action::List => list(archived, &args),
        Action::Search => search(archived, &args),
        Action::Read => read(archived, &args),
    }
}

fn list(items: &[&MessageItem], args: &Args) -> Result<String, String> {
    let start = args.start_index.unwrap_or(0);
    let limit = args.limit.unwrap_or(DEFAULT_LIST_LIMIT).min(MAX_LIST_LIMIT);
    if limit == 0 {
        return Err("error: limit must be at least 1".to_string());
    }
    if start >= items.len() {
        return Ok(format!(
            "no archived items at or after index {start}; archived item count={}",
            items.len()
        ));
    }

    let mut lines = vec![format!(
        "archived conversation items: {} (latest compaction boundary index: {})",
        items.len(),
        items.len()
    )];
    for (index, item) in items.iter().enumerate().skip(start).take(limit) {
        let rendered = render_item(index, item, false)?;
        lines.push(format!(
            "[{index}] {} — {}",
            item_kind(item),
            one_line_preview(&rendered, PREVIEW_CHARS)
        ));
    }
    let next = start.saturating_add(limit);
    if next < items.len() {
        lines.push(format!(
            "more items available; continue with start_index={next}"
        ));
    }
    Ok(lines.join("\n"))
}

fn search(items: &[&MessageItem], args: &Args) -> Result<String, String> {
    let query = args
        .query
        .as_deref()
        .map(str::trim)
        .filter(|query| !query.is_empty())
        .ok_or_else(|| "error: query is required for search".to_string())?;
    let needle = query.to_lowercase();
    let max_results = args
        .max_results
        .unwrap_or(DEFAULT_SEARCH_RESULTS)
        .clamp(1, MAX_SEARCH_RESULTS);
    let mut matches = Vec::new();

    for (index, item) in items.iter().enumerate() {
        let rendered = render_item(index, item, false)?;
        if let Some(line) = rendered
            .lines()
            .find(|line| line.to_lowercase().contains(&needle))
        {
            matches.push(format!(
                "[{index}] {} — {}",
                item_kind(item),
                one_line_preview(line.trim(), SEARCH_SNIPPET_CHARS)
            ));
            if matches.len() == max_results {
                break;
            }
        }
    }

    if matches.is_empty() {
        Ok(format!(
            "no archived conversation items matched literal query {query:?}"
        ))
    } else {
        let count = matches.len();
        matches.push(format!(
            "{count} match(es); use action=read with an index for exact content"
        ));
        Ok(matches.join("\n"))
    }
}

fn read(items: &[&MessageItem], args: &Args) -> Result<String, String> {
    let index = args
        .index
        .ok_or_else(|| "error: index is required for read".to_string())?;
    let item = items.get(index).ok_or_else(|| {
        format!(
            "error: archived item index {index} is out of range (count={})",
            items.len()
        )
    })?;
    let rendered = render_item(index, item, true)?;
    let total = rendered.chars().count();
    let offset = args.offset.unwrap_or(0);
    if offset > total {
        return Err(format!(
            "error: offset {offset} is past the end of archived item {index} ({total} characters)"
        ));
    }
    let limit = args.limit.unwrap_or(DEFAULT_READ_CHARS).min(MAX_READ_CHARS);
    if limit == 0 {
        return Err("error: limit must be at least 1".to_string());
    }
    let chunk = rendered
        .chars()
        .skip(offset)
        .take(limit)
        .collect::<String>();
    let consumed = chunk.chars().count();
    let next = offset + consumed;
    let mut output = format!(
        "archived item {index} ({kind}), characters {offset}..{next} of {total}\n{chunk}",
        kind = item_kind(item)
    );
    if next < total {
        output.push_str(&format!(
            "\n[more content available; read index={index} offset={next}]"
        ));
    }
    Ok(output)
}

fn render_item(index: usize, item: &MessageItem, pretty: bool) -> Result<String, String> {
    let mut value = match item {
        MessageItem::Input(input) => json!({ "index": index, "kind": "input", "value": input }),
        MessageItem::Output(output) => {
            json!({ "index": index, "kind": "output", "value": output })
        }
        MessageItem::ToolOutput {
            output,
            failed,
            approval_label,
        } => json!({
            "index": index,
            "kind": "tool_output",
            "failed": failed,
            "approval_label": approval_label,
            "value": output
        }),
        MessageItem::OpenAIError(error) => {
            json!({ "index": index, "kind": "api_error", "message": error.to_string() })
        }
        MessageItem::Error(message) => {
            json!({ "index": index, "kind": "error", "message": message })
        }
        MessageItem::Warning(message) => {
            json!({ "index": index, "kind": "warning", "message": message })
        }
        MessageItem::Info(message) => {
            json!({ "index": index, "kind": "info", "message": message })
        }
        MessageItem::Meta { label, text } => {
            json!({ "index": index, "kind": "meta", "label": label, "text": text })
        }
        MessageItem::Usage(input, output, cached) => json!({
            "index": index,
            "kind": "usage",
            "input_tokens": input,
            "output_tokens": output,
            "cached_input_tokens": cached
        }),
        MessageItem::Compacted { summary } => {
            json!({ "index": index, "kind": "earlier_compaction", "summary": summary })
        }
    };
    redact_inline_images(&mut value);
    if pretty {
        serde_json::to_string_pretty(&value)
    } else {
        serde_json::to_string(&value)
    }
    .map_err(|error| format!("error: could not serialize archived item: {error}"))
}

fn redact_inline_images(value: &mut Value) {
    match value {
        Value::Array(values) => {
            for value in values {
                redact_inline_images(value);
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                if key == "image_url" && value.as_str().is_some_and(|url| url.starts_with("data:"))
                {
                    *value = Value::String("[inline image data omitted]".to_string());
                } else {
                    redact_inline_images(value);
                }
            }
        }
        _ => {}
    }
}

fn item_kind(item: &MessageItem) -> &'static str {
    match item {
        MessageItem::Input(_) => "input",
        MessageItem::Output(_) => "output",
        MessageItem::ToolOutput { .. } => "tool_output",
        MessageItem::OpenAIError(_) => "api_error",
        MessageItem::Error(_) => "error",
        MessageItem::Warning(_) => "warning",
        MessageItem::Info(_) => "info",
        MessageItem::Meta { .. } => "meta",
        MessageItem::Usage(_, _, _) => "usage",
        MessageItem::Compacted { .. } => "earlier_compaction",
    }
}

fn one_line_preview(text: &str, max_chars: usize) -> String {
    let flattened = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flattened.chars().count() <= max_chars {
        flattened
    } else {
        let mut preview = flattened.chars().take(max_chars).collect::<String>();
        preview.push('…');
        preview
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_openai::types::responses::{
        InputContent, InputMessage, InputRole, MessageItem as ApiMessageItem, OutputStatus,
    };

    fn compacted_conversation() -> Arc<Mutex<Conversation>> {
        let mut conversation = Conversation::new();
        conversation.add_input_message(ApiMessageItem::Input(InputMessage {
            content: vec![InputContent::InputText(
                "the exact secret detail is cerulean".into(),
            )],
            role: InputRole::User,
            status: Some(OutputStatus::Completed),
        }));
        conversation.add_info_string("another archived entry");
        conversation.apply_compaction("summary without the detail".to_string());
        Arc::new(Mutex::new(conversation))
    }

    #[tokio::test]
    async fn searches_and_reads_exact_archived_items() {
        let conversation = compacted_conversation();
        let found = run(r#"{"action":"search","query":"CERULEAN"}"#, &conversation)
            .await
            .unwrap();
        assert!(found.contains("[0] input"));

        let exact = run(r#"{"action":"read","index":0}"#, &conversation)
            .await
            .unwrap();
        assert!(exact.contains("the exact secret detail is cerulean"));
        assert!(!exact.contains("summary without the detail"));
    }

    #[tokio::test]
    async fn long_items_support_character_offset_paging() {
        let conversation = compacted_conversation();
        let first = run(
            r#"{"action":"read","index":0,"offset":0,"limit":20}"#,
            &conversation,
        )
        .await
        .unwrap();
        assert!(first.contains("offset=20"));

        let second = run(
            r#"{"action":"read","index":0,"offset":20,"limit":20}"#,
            &conversation,
        )
        .await
        .unwrap();
        assert!(second.contains("characters 20.."));
    }

    #[tokio::test]
    async fn rejects_sessions_without_compacted_history() {
        let conversation = Arc::new(Mutex::new(Conversation::new()));
        let error = run(r#"{"action":"list"}"#, &conversation)
            .await
            .unwrap_err();
        assert!(error.contains("no compacted conversation history"));
    }
}
