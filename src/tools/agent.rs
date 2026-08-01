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
use std::time::Duration;

use super::function_tool;
use crate::agents::{AgentManager, AgentRuntime, AgentSnapshot, AgentStatus};
use crate::ui::event::Event;

pub const NAME: &str = "agent";
const DEFAULT_WAIT_SECS: u64 = 60;
const MAX_WAIT_SECS: u64 = 600;

pub fn tool() -> Tool {
    function_tool(
        NAME,
        "Delegate independent work to in-process sub-agents. Actions: `spawn` starts a child with its own conversation and returns immediately; `list` shows all children; `result` reads current status and any final answer; `wait` blocks for completion up to a timeout; `cancel` stops a running child. Sub-agents share the project directory and tools but cannot create nested agents. The parent is notified when a child finishes, so prefer spawn and continue useful work instead of polling.",
        json!({
            "action": {
                "type": "string",
                "enum": ["spawn", "list", "result", "wait", "cancel"],
                "description": "The lifecycle action to perform."
            },
            "prompt": {
                "type": "string",
                "description": "spawn only: a concrete, bounded task with enough context to complete independently."
            },
            "name": {
                "type": "string",
                "description": "spawn only: optional short label shown in the Agents sidebar."
            },
            "model": {
                "type": "string",
                "description": "spawn only: optional provider/model override; defaults to the parent's current model."
            },
            "thinking": {
                "type": "string",
                "enum": ["auto", "none", "minimal", "low", "medium", "high", "xhigh"],
                "description": "spawn only: optional reasoning effort override."
            },
            "id": {
                "type": "integer",
                "description": "Sub-agent id, required for result, wait, and cancel."
            },
            "timeout": {
                "type": "integer",
                "description": "wait only: seconds to block, default 60 and maximum 600."
            }
        }),
        &["action"],
    )
}

#[derive(Deserialize)]
struct Args {
    action: String,
    prompt: Option<String>,
    name: Option<String>,
    model: Option<String>,
    thinking: Option<String>,
    id: Option<u64>,
    timeout: Option<u64>,
}

pub(crate) fn is_observational(arguments: &str) -> bool {
    serde_json::from_str::<Args>(arguments)
        .map(|args| matches!(args.action.as_str(), "list" | "result" | "wait"))
        .unwrap_or(false)
}

pub(crate) async fn run(
    arguments: &str,
    manager: &AgentManager,
    runtime: &AgentRuntime,
    events: tokio::sync::mpsc::UnboundedSender<Event>,
) -> Result<String, String> {
    let args: Args = serde_json::from_str(arguments)
        .map_err(|error| format!("error: invalid arguments: {error}"))?;
    match args.action.as_str() {
        "spawn" => {
            let prompt = args
                .prompt
                .filter(|prompt| !prompt.trim().is_empty())
                .ok_or_else(|| "error: 'prompt' is required for spawn".to_string())?;
            let runtime =
                runtime.with_overrides(args.model.as_deref(), args.thinking.as_deref())?;
            let id = manager.spawn(prompt.clone(), args.name, runtime, events)?;
            Ok(format!(
                "started sub-agent {id}: {}\nThe parent will be notified when it finishes. Continue useful work or use action=wait id={id} only when the result is required immediately.",
                prompt.lines().next().unwrap_or("delegated task")
            ))
        }
        "list" => {
            let agents = manager.snapshot_all();
            if agents.is_empty() {
                return Ok("no sub-agents".to_string());
            }
            Ok(agents
                .iter()
                .map(render_summary)
                .collect::<Vec<_>>()
                .join("\n"))
        }
        "result" => {
            let id = require_id(args.id, "result")?;
            let snapshot = manager
                .snapshot(id)
                .ok_or_else(|| format!("error: no sub-agent with id {id}"))?;
            if snapshot.status.is_terminal() {
                manager.consume_notification(id);
            }
            Ok(render_full(&snapshot))
        }
        "wait" => {
            let id = require_id(args.id, "wait")?;
            let timeout = args.timeout.unwrap_or(DEFAULT_WAIT_SECS).min(MAX_WAIT_SECS);
            let snapshot = manager.wait(id, Duration::from_secs(timeout)).await?;
            let mut result = render_full(&snapshot);
            if snapshot.status == AgentStatus::Running {
                result.push_str(&format!(
                    "\n[still running after {timeout}s — the parent will be notified on completion]"
                ));
            }
            Ok(result)
        }
        "cancel" => {
            let id = require_id(args.id, "cancel")?;
            manager.cancel(id)?;
            Ok(format!("cancellation requested for sub-agent {id}"))
        }
        other => Err(format!(
            "error: unknown action '{other}' — use spawn, list, result, wait, or cancel"
        )),
    }
}

fn require_id(id: Option<u64>, action: &str) -> Result<u64, String> {
    id.ok_or_else(|| format!("error: 'id' is required for {action}"))
}

fn render_summary(snapshot: &AgentSnapshot) -> String {
    format!(
        "[{}] {} ({}s): {}",
        snapshot.id,
        snapshot.status.label(),
        snapshot.elapsed.as_secs(),
        snapshot.name
    )
}

fn render_full(snapshot: &AgentSnapshot) -> String {
    let mut text = render_summary(snapshot);
    text.push_str(&format!("\nTask: {}", snapshot.prompt));
    if let Some(result) = &snapshot.result {
        text.push_str("\n--- result ---\n");
        text.push_str(result);
    } else {
        text.push_str("\n(no result yet)");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observational_actions_bypass_spawn_and_cancel() {
        assert!(is_observational(r#"{"action":"list"}"#));
        assert!(is_observational(r#"{"action":"result","id":1}"#));
        assert!(is_observational(r#"{"action":"wait","id":1}"#));
        assert!(!is_observational(r#"{"action":"spawn","prompt":"x"}"#));
        assert!(!is_observational(r#"{"action":"cancel","id":1}"#));
    }

    #[test]
    fn result_render_includes_task_and_final_text() {
        let snapshot = AgentSnapshot {
            id: 4,
            name: "review".into(),
            prompt: "review the parser".into(),
            status: AgentStatus::Completed,
            elapsed: Duration::from_secs(2),
            result: Some("no issues".into()),
        };
        let rendered = render_full(&snapshot);
        assert!(rendered.contains("[4] completed"));
        assert!(rendered.contains("review the parser"));
        assert!(rendered.contains("no issues"));
    }
}
