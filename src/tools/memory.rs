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

use async_openai::Client;
use async_openai::config::OpenAIConfig;
use async_openai::types::responses::{
    CreateResponse, InputParam, OutputItem, OutputMessageContent, Response, Tool,
};
use serde::Deserialize;
use serde_json::json;

use super::function_tool;
use crate::memory::{MemoryEntry, MemoryKind, MemoryManager, MemoryScope};

pub const NAME: &str = "memory";

#[derive(Clone)]
pub(crate) struct MemoryModel {
    pub client: Client<OpenAIConfig>,
    pub model: String,
}

impl MemoryModel {
    /// Automatically associate the current turn with stored memories. The
    /// memory model sees the surrounding conversation, not just the latest
    /// user line, so an elliptical follow-up ("do it the same way") can still
    /// match. Recalled entries are reinforced so fresh, often-used memories
    /// outrank stale ones next time.
    pub(crate) async fn associate(
        &self,
        context: &str,
        query: &str,
    ) -> Result<Vec<MemoryEntry>, String> {
        let manager = MemoryManager::for_current_dir()?;
        let config = crate::config::programmer_config::MemoryConfig::default();
        let mut entries = manager.retrieve(query, &config)?;
        let ids = self.select(context, query, &entries).await?;
        entries.sort_by_key(|entry| {
            ids.iter()
                .position(|id| id == &entry.id)
                .unwrap_or(usize::MAX)
        });
        entries.retain(|entry| ids.contains(&entry.id));
        entries.truncate(5);
        if !entries.is_empty() {
            let ids = entries
                .iter()
                .map(|entry| entry.id.clone())
                .collect::<Vec<_>>();
            // Reinforcement is best-effort: a failed write must not lose the
            // memories we already selected for this turn.
            let _ = manager.touch(&ids);
        }
        Ok(entries)
    }

    async fn select(
        &self,
        context: &str,
        query: &str,
        entries: &[MemoryEntry],
    ) -> Result<Vec<String>, String> {
        if entries.is_empty() {
            return Ok(Vec::new());
        }
        let manifest = entries
            .iter()
            .map(|entry| {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                json!({
                    "id": entry.id,
                    "kind": entry.kind,
                    "description": entry.content.lines().next().unwrap_or(""),
                    "age_days": now.saturating_sub(entry.updated_at) / 86_400,
                    "recalled_before": entry.use_count,
                })
            })
            .collect::<Vec<_>>();
        let context = context.trim();
        let context = if context.is_empty() {
            "(no earlier conversation)".to_string()
        } else {
            context.to_string()
        };
        let prompt = format!(
            "Conversation so far:\n{context}\n\nCurrent user request:\n{query}\n\nMemory manifest:\n{}\n\nReturn a JSON array containing only the IDs of memories that are relevant to the current request, most relevant first. Prefer memories that are still accurate; ignore stale entries that the conversation contradicts. Return at most 5 IDs and no explanation.",
            serde_json::to_string(&manifest).map_err(|error| error.to_string())?
        );
        let request = CreateResponse {
            model: Some(self.model.clone()),
            input: InputParam::Text(prompt),
            instructions: Some(
                "You are a fast memory association model. Select only memories that materially help answer the current request."
                    .to_string(),
            ),
            // Picking IDs out of a handful of candidates is not a reasoning
            // task, and a thinking model would spend its whole budget before
            // answering. The classifier makes the same call for the same
            // reason; leaving this unset let the model's default effort burn
            // through the timeout.
            reasoning: Some(async_openai::types::responses::ReasoningEffort::None.into()),
            temperature: Some(0.0),
            // Reasoning tokens count against this cap, so it needs headroom
            // beyond the tiny JSON array we ask for: a model that thinks even
            // briefly would otherwise return an empty, incomplete message.
            max_output_tokens: Some(1024),
            store: Some(false),
            ..Default::default()
        };
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(crate::consts::MEMORY_ASSOCIATION_TIMEOUT_SECS),
            self.client.responses().create(request),
        )
        .await
        .map_err(|_| {
            format!(
                "timed out after {}s",
                crate::consts::MEMORY_ASSOCIATION_TIMEOUT_SECS
            )
        })?
        .map_err(|error| error.to_string())?;
        let text = selection_text(&response)?;
        let json = text
            .trim()
            .strip_prefix("```json")
            .or_else(|| text.trim().strip_prefix("```"))
            .unwrap_or(text.trim())
            .strip_suffix("```")
            .unwrap_or(text.trim())
            .trim();
        let mut ids: Vec<String> = serde_json::from_str(json).map_err(|error| {
            format!(
                "invalid memory selection: {error} (response started with {:?})",
                preview(json)
            )
        })?;
        ids.retain(|id| entries.iter().any(|entry| &entry.id == id));
        ids.truncate(5);
        Ok(ids)
    }
}

/// Pull the selection out of a non-streaming response, or explain why there
/// was nothing usable in it.
///
/// An empty message item and a response that never produced one look identical
/// once parsed — both fail as unparseable JSON — so the reason is resolved
/// here, while the response is still available.
fn selection_text(response: &Response) -> Result<String, String> {
    let mut refusal = None;
    for item in &response.output {
        let OutputItem::Message(message) = item else {
            continue;
        };
        for content in &message.content {
            match content {
                OutputMessageContent::OutputText(text) if !text.text.trim().is_empty() => {
                    return Ok(text.text.clone());
                }
                OutputMessageContent::Refusal(part) => refusal = Some(part.refusal.clone()),
                _ => {}
            }
        }
    }
    match refusal {
        Some(reason) => Err(format!("memory model refused: {reason}")),
        None => Err(format!(
            "memory model returned no selection ({})",
            describe_response(response)
        )),
    }
}

/// Summarize what came back, so an empty selection carries its own diagnosis:
/// a truncated response names the status, and a model that thought anyway
/// names the reasoning tokens it spent.
fn describe_response(response: &Response) -> String {
    let items = response
        .output
        .iter()
        .map(|item| match item {
            OutputItem::Message(_) => "message",
            OutputItem::Reasoning(_) => "reasoning",
            OutputItem::FunctionCall(_) => "function_call",
            OutputItem::WebSearchCall(_) => "web_search_call",
            OutputItem::FileSearchCall(_) => "file_search_call",
            OutputItem::ImageGenerationCall(_) => "image_generation_call",
            OutputItem::CodeInterpreterCall(_) => "code_interpreter_call",
            OutputItem::ComputerCall(_) => "computer_call",
            _ => "other",
        })
        .collect::<Vec<_>>()
        .join(",");
    let mut detail = format!("status {:?}, items [{items}]", response.status);
    if let Some(incomplete) = &response.incomplete_details {
        detail.push_str(&format!(", incomplete: {}", incomplete.reason));
    }
    if let Some(usage) = &response.usage {
        detail.push_str(&format!(
            ", reasoning tokens {}",
            usage.output_tokens_details.reasoning_tokens
        ));
    }
    detail
}

/// A bounded single-line view of the model's answer for error messages.
fn preview(text: &str) -> String {
    let mut preview: String = text.trim().chars().take(120).collect();
    if preview.is_empty() {
        return "(nothing)".to_string();
    }
    if text.trim().chars().count() > 120 {
        preview.push('…');
    }
    preview.replace('\n', " ")
}

pub fn tool() -> Tool {
    function_tool(
        NAME,
        "Manage durable local memory across sessions. Use `remember` only for explicit, stable user preferences or verified facts/decisions. Store one atomic, independently updateable fact per entry: split mixed subjects and split cross-project environment knowledge (`global`) from repository-specific facts (`project`), using multiple calls when needed. Check existing memories and update rather than duplicating them. Never store credentials, transient progress, or unverified guesses. Actions: `remember`, `recall`, `list`, `update`, and `forget`.",
        json!({
            "action": {
                "type": "string",
                "enum": ["remember", "recall", "list", "update", "forget"],
                "description": "The memory operation."
            },
            "scope": {
                "type": "string",
                "enum": ["global", "project"],
                "description": "Choose the narrowest valid scope: global is reusable across unrelated repositories (user preferences or stable machine/access environment facts); project is specific to the current repository (its paths, services, architecture, or workflow)."
            },
            "kind": {
                "type": "string",
                "enum": ["preference", "project_fact", "decision", "convention", "workflow", "constraint", "known_issue"],
                "description": "Memory category, required for remember and optional for update."
            },
            "content": {
                "type": "string",
                "description": "One self-contained, atomic fact or preference to remember, or replacement content for update. Do not bundle facts with different scopes or lifecycles."
            },
            "tags": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Optional retrieval tags."
            },
            "query": {
                "type": "string",
                "description": "Search query required for recall."
            },
            "id": {
                "type": "string",
                "description": "Memory id required for update and forget."
            }
        }),
        &["action"],
    )
}

#[derive(Deserialize)]
struct Args {
    action: String,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tags: Option<Vec<String>>,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    id: Option<String>,
}

pub(crate) fn action_is_recall(arguments: &str) -> bool {
    serde_json::from_str::<Args>(arguments).is_ok_and(|args| args.action == "recall")
}

pub(crate) fn action_is_mutating(arguments: &str) -> bool {
    serde_json::from_str::<Args>(arguments)
        .map(|args| matches!(args.action.as_str(), "remember" | "update" | "forget"))
        .unwrap_or(true)
}

pub async fn run(arguments: &str, model: Option<&MemoryModel>) -> Result<String, String> {
    let args: Args = serde_json::from_str(arguments)
        .map_err(|error| format!("error: invalid arguments: {error}"))?;
    let manager = MemoryManager::for_current_dir().map_err(|error| format!("error: {error}"))?;

    match args.action.as_str() {
        "remember" => {
            let scope = required_scope(args.scope.as_deref())?;
            let kind = required_kind(args.kind.as_deref())?;
            let content = required(args.content, "content")?;
            let entry = manager
                .remember(scope, kind, content, args.tags.unwrap_or_default())
                .map_err(|error| format!("error: {error}"))?;
            Ok(format!(
                "Remembered {} memory {}: {}",
                scope_label(scope),
                entry.id,
                entry.content
            ))
        }
        "recall" => {
            let query = required(args.query, "query")?;
            let mut config = crate::config::programmer_config::MemoryConfig::default();
            if let Some(scope) = optional_scope(args.scope.as_deref())? {
                config.global_enabled = scope == MemoryScope::Global;
                config.project_enabled = scope == MemoryScope::Project;
            }
            let mut entries = manager
                .retrieve(&query, &config)
                .map_err(|error| format!("error: {error}"))?;
            if let Some(model) = model
                && let Ok(ids) = model.select(&query, &query, &entries).await
            {
                entries.sort_by_key(|entry| {
                    ids.iter()
                        .position(|id| id == &entry.id)
                        .unwrap_or(usize::MAX)
                });
                entries.retain(|entry| ids.contains(&entry.id));
            }
            Ok(recall_output(&manager, &entries))
        }
        "list" => {
            let scope = optional_scope(args.scope.as_deref())?;
            let entries = manager
                .list(scope)
                .map_err(|error| format!("error: {error}"))?;
            Ok(MemoryManager::render_list(&entries))
        }
        "update" => {
            let id = required(args.id, "id")?;
            let kind = args.kind.as_deref().map(parse_kind).transpose()?;
            if args.content.is_none() && kind.is_none() && args.tags.is_none() {
                return Err("error: update requires content, kind, or tags".to_string());
            }
            let entry = manager
                .update(&id, args.content, kind, args.tags)
                .map_err(|error| format!("error: {error}"))?;
            Ok(format!("Updated memory {}: {}", entry.id, entry.content))
        }
        "forget" => {
            let id = required(args.id, "id")?;
            manager
                .forget(&id)
                .map_err(|error| format!("error: {error}"))?;
            Ok(format!("Forgot memory {id}"))
        }
        other => Err(format!(
            "error: unknown action '{other}' — use remember, recall, list, update, or forget"
        )),
    }
}

/// An explicit recall is still a recall: reinforce the entries it returned
/// so they age like any other memory that was actually used.
fn recall_output(manager: &MemoryManager, entries: &[MemoryEntry]) -> String {
    if !entries.is_empty() {
        let ids = entries
            .iter()
            .map(|entry| entry.id.clone())
            .collect::<Vec<_>>();
        // Best-effort, like automatic association: a failed write must not
        // lose the memories we are about to return.
        let _ = manager.touch(&ids);
    }
    MemoryManager::render_list(entries)
}

fn required(value: Option<String>, name: &str) -> Result<String, String> {
    value
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("error: '{name}' is required"))
}

fn optional_scope(value: Option<&str>) -> Result<Option<MemoryScope>, String> {
    value.map(parse_scope).transpose()
}

fn required_scope(value: Option<&str>) -> Result<MemoryScope, String> {
    value
        .ok_or_else(|| "error: 'scope' is required".to_string())
        .and_then(parse_scope)
}

fn parse_scope(value: &str) -> Result<MemoryScope, String> {
    MemoryScope::parse(value)
        .ok_or_else(|| format!("error: invalid scope '{value}' — use global or project"))
}

fn required_kind(value: Option<&str>) -> Result<MemoryKind, String> {
    value
        .ok_or_else(|| "error: 'kind' is required".to_string())
        .and_then(parse_kind)
}

fn parse_kind(value: &str) -> Result<MemoryKind, String> {
    MemoryKind::parse(value).ok_or_else(|| format!("error: invalid memory kind '{value}'"))
}

fn scope_label(scope: MemoryScope) -> &'static str {
    match scope {
        MemoryScope::Global => "global",
        MemoryScope::Project => "project",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutation_detection_splits_read_and_write_actions() {
        assert!(!action_is_mutating(r#"{"action":"list"}"#));
        assert!(!action_is_mutating(r#"{"action":"recall","query":"rust"}"#));
        assert!(action_is_mutating(r#"{"action":"remember"}"#));
        assert!(action_is_mutating("not json"));
    }

    fn temp_manager() -> MemoryManager {
        let root = std::env::temp_dir().join(format!(
            "programmer-memory-tool-test-{}",
            uuid::Uuid::new_v4()
        ));
        MemoryManager::new(root.join("config"), root.join("workspace"))
    }

    #[test]
    fn recall_output_shows_the_age_and_reinforces_what_it_returns() {
        let manager = temp_manager();
        let entry = manager
            .remember(
                MemoryScope::Project,
                MemoryKind::ProjectFact,
                "Store sessions as JSON".into(),
                vec![],
            )
            .unwrap();

        let output = recall_output(&manager, std::slice::from_ref(&entry));

        assert!(output.contains("Store sessions as JSON"));
        assert!(output.contains("saved today"));
        let stored = manager.list(Some(MemoryScope::Project)).unwrap();
        assert_eq!(stored[0].use_count, 1);
        assert!(stored[0].last_used_at.is_some());
    }

    #[test]
    fn recall_output_of_nothing_stays_inert() {
        let manager = temp_manager();
        assert_eq!(recall_output(&manager, &[]), "No active memories.");
    }

    /// A response body with only the fields the API always sends.
    fn response(output: serde_json::Value, extra: serde_json::Value) -> Response {
        let mut body = json!({
            "created_at": 0,
            "id": "resp_1",
            "model": "mock",
            "object": "response",
            "status": "completed",
            "output": output,
        });
        for (key, value) in extra.as_object().unwrap() {
            body[key] = value.clone();
        }
        serde_json::from_value(body).unwrap()
    }

    #[test]
    fn selection_text_reads_the_message_text() {
        let response = response(
            json!([{
                "type": "message",
                "id": "msg_1",
                "role": "assistant",
                "status": "completed",
                "content": [{"type": "output_text", "text": "[\"mem_a\"]", "annotations": []}],
            }]),
            json!({}),
        );

        assert_eq!(selection_text(&response).unwrap(), "[\"mem_a\"]");
    }

    #[test]
    fn an_empty_selection_explains_itself_instead_of_failing_as_json() {
        // Nothing in the output at all.
        let bare = response(json!([]), json!({}));
        let error = selection_text(&bare).unwrap_err();
        assert!(error.contains("no selection"), "{error}");
        assert!(error.contains("status Completed"), "{error}");

        // A model that thought anyway and ran out of budget: the empty text is
        // the symptom, the reasoning tokens are the cause.
        let truncated = response(
            json!([
                {"type": "reasoning", "summary": []},
                {
                    "type": "message",
                    "id": "msg_1",
                    "role": "assistant",
                    "status": "completed",
                    "content": [{"type": "output_text", "text": "   ", "annotations": []}],
                },
            ]),
            json!({
                "status": "incomplete",
                "incomplete_details": {"reason": "max_output_tokens"},
                "usage": {
                    "input_tokens": 10,
                    "input_tokens_details": {"cached_tokens": 0},
                    "output_tokens": 256,
                    "output_tokens_details": {"reasoning_tokens": 256},
                    "total_tokens": 266,
                },
            }),
        );
        let error = selection_text(&truncated).unwrap_err();
        assert!(error.contains("items [reasoning,message]"), "{error}");
        assert!(error.contains("incomplete: max_output_tokens"), "{error}");
        assert!(error.contains("reasoning tokens 256"), "{error}");
    }

    #[test]
    fn a_refusal_is_reported_verbatim() {
        let response = response(
            json!([{
                "status": "completed",
                "type": "message",
                "id": "msg_1",
                "role": "assistant",
                "content": [{"type": "refusal", "refusal": "I can't help with that."}],
            }]),
            json!({}),
        );

        assert_eq!(
            selection_text(&response).unwrap_err(),
            "memory model refused: I can't help with that."
        );
    }

    #[test]
    fn preview_is_single_line_and_bounded() {
        assert_eq!(preview("  \n "), "(nothing)");
        assert_eq!(preview("a\nb"), "a b");
        let long = preview(&"x".repeat(200));
        assert_eq!(long.chars().count(), 121, "120 chars plus the ellipsis");
        assert!(long.ends_with('…'));
    }
}
