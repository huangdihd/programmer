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

use async_openai::types::responses::Tool;
use serde::Deserialize;
use serde_json::json;

use super::function_tool;
use crate::memory::{MemoryKind, MemoryManager, MemoryScope};

pub const NAME: &str = "memory";

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

pub(crate) fn action_is_mutating(arguments: &str) -> bool {
    serde_json::from_str::<Args>(arguments)
        .map(|args| matches!(args.action.as_str(), "remember" | "update" | "forget"))
        .unwrap_or(true)
}

pub async fn run(arguments: &str) -> Result<String, String> {
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
            let entries = manager
                .retrieve(&query, &config)
                .map_err(|error| format!("error: {error}"))?;
            Ok(MemoryManager::render_list(&entries))
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
}
