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

use std::time::Duration;

use async_openai::types::responses::Tool;
use serde::Deserialize;
use serde_json::json;

use super::function_tool;
use crate::tasks::{self, TaskSnapshot, TaskStatus};

pub const NAME: &str = "task";

/// Default and maximum number of seconds `wait` blocks for.
const DEFAULT_WAIT_SECS: u64 = 60;
const MAX_WAIT_SECS: u64 = 600;

/// How many characters from the tail of a task's output the `output` and
/// `wait` actions return by default.
const DEFAULT_TAIL_CHARS: usize = 4000;

pub fn tool() -> Tool {
    function_tool(
        NAME,
        "Manage background tasks: shell commands that keep running while the \
         conversation continues (dev servers, watchers, long builds). \
         Actions: \
         `create` (start a command in the background, returns its task id), \
         `list` (all tasks with status and runtime), \
         `output` (a task's current status and captured output), \
         `wait` (block until a task finishes or the timeout elapses), \
         `kill` (terminate a running task). \
         Prefer the `command` tool for anything that finishes quickly — use \
         background tasks only when the command should outlive the current step.",
        json!({
            "action": {
                "type": "string",
                "description": "The action to perform.",
                "enum": ["create", "list", "output", "wait", "kill"]
            },
            "command": {
                "type": "string",
                "description": "The shell command to run in the background (required for create)."
            },
            "name": {
                "type": "string",
                "description": "Optional short label for the task, shown in the sidebar (create only)."
            },
            "dir": {
                "type": "string",
                "description": "Optional working directory for the command (create only). Default: the project directory."
            },
            "id": {
                "type": "integer",
                "description": "The task id (required for output, wait, and kill)."
            },
            "timeout": {
                "type": "integer",
                "description": "wait only: seconds to block before giving up. Default 60, max 600."
            },
            "tail": {
                "type": "integer",
                "description": "output/wait only: how many trailing characters of output to return. Default 4000."
            }
        }),
        &["action"],
    )
}

#[derive(Deserialize)]
struct Args {
    action: String,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    dir: Option<String>,
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    timeout: Option<u64>,
    #[serde(default)]
    tail: Option<usize>,
}

/// Which task actions mutate state (start/stop processes). Used by the
/// classifier: `create` runs an arbitrary command and `kill` terminates one,
/// so they are gated like the `command` tool; the rest are read-only.
pub fn action_is_mutating(arguments: &str) -> bool {
    #[derive(Deserialize)]
    struct ActionOnly {
        action: String,
    }
    match serde_json::from_str::<ActionOnly>(arguments) {
        Ok(a) => matches!(a.action.as_str(), "create" | "kill"),
        // Unparseable arguments: assume the worst.
        Err(_) => true,
    }
}

pub async fn run(arguments: &str) -> Result<String, String> {
    let args: Args = match serde_json::from_str(arguments) {
        Ok(a) => a,
        Err(e) => return Err(format!("error: invalid arguments: {e}")),
    };

    match args.action.as_str() {
        "create" => {
            let command = match args.command {
                Some(ref c) if !c.trim().is_empty() => c.clone(),
                _ => return Err("error: 'command' is required for create".to_string()),
            };
            let id = tasks::spawn(&command, args.dir.as_deref(), args.name.as_deref())?;
            Ok(format!(
                "started background task {id}: {command}\n\
                 Check on it with action=output id={id}, or block on it with \
                 action=wait id={id}."
            ))
        }

        "list" => {
            let all = tasks::snapshot_all();
            if all.is_empty() {
                return Ok("no background tasks".to_string());
            }
            let lines: Vec<String> = all.iter().map(render_summary).collect();
            Ok(lines.join("\n"))
        }

        "output" => {
            let id = require_id(args.id, "output")?;
            let snap = tasks::snapshot(id)
                .ok_or_else(|| format!("error: no task with id {id}"))?;
            Ok(render_full(&snap, args.tail.unwrap_or(DEFAULT_TAIL_CHARS)))
        }

        "wait" => {
            let id = require_id(args.id, "wait")?;
            let timeout = args
                .timeout
                .unwrap_or(DEFAULT_WAIT_SECS)
                .min(MAX_WAIT_SECS);
            let (snap, still_running) =
                tasks::wait(id, Duration::from_secs(timeout)).await?;
            let mut text = render_full(&snap, args.tail.unwrap_or(DEFAULT_TAIL_CHARS));
            if still_running {
                text.push_str(&format!(
                    "\n[still running after {timeout}s — wait again, check later \
                     with action=output, or stop it with action=kill]"
                ));
            }
            Ok(text)
        }

        "kill" => {
            let id = require_id(args.id, "kill")?;
            tasks::kill(id)?;
            Ok(format!("kill signal sent to task {id}"))
        }

        other => Err(format!(
            "error: unknown action '{other}' — use create, list, output, wait, or kill"
        )),
    }
}

fn require_id(id: Option<u64>, action: &str) -> Result<u64, String> {
    id.ok_or_else(|| format!("error: 'id' is required for {action}"))
}

/// One-line summary used by `list`.
fn render_summary(snap: &TaskSnapshot) -> String {
    let exit = match (snap.status, snap.exit_code) {
        (TaskStatus::Running, _) => String::new(),
        (_, Some(code)) => format!(", exit {code}"),
        (_, None) => String::new(),
    };
    format!(
        "[{id}] {status}{exit} ({secs}s): {command}",
        id = snap.id,
        status = snap.status.label(),
        secs = snap.elapsed.as_secs(),
        command = snap.command,
    )
}

/// Status header plus the trailing `tail` chars of output, for `output`/`wait`.
fn render_full(snap: &TaskSnapshot, tail: usize) -> String {
    let mut text = render_summary(snap);
    if snap.output.is_empty() {
        text.push_str("\n(no output captured yet)");
        return text;
    }
    let total = snap.output.chars().count();
    if total > tail {
        let tail_text: String = snap
            .output
            .chars()
            .skip(total - tail)
            .collect();
        text.push_str(&format!(
            "\n--- output (last {tail} of {total} chars) ---\n{tail_text}"
        ));
    } else {
        text.push_str(&format!("\n--- output ---\n{}", snap.output));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_kill_are_mutating_but_views_are_not() {
        assert!(action_is_mutating(r#"{"action":"create","command":"x"}"#));
        assert!(action_is_mutating(r#"{"action":"kill","id":1}"#));
        assert!(!action_is_mutating(r#"{"action":"list"}"#));
        assert!(!action_is_mutating(r#"{"action":"output","id":1}"#));
        assert!(!action_is_mutating(r#"{"action":"wait","id":1}"#));
        // Garbage arguments are treated as mutating.
        assert!(action_is_mutating("not json"));
    }

    #[tokio::test]
    async fn create_view_wait_round_trip() {
        let created = run(r#"{"action":"create","command":"echo task-tool-test"}"#)
            .await
            .expect("create should succeed");
        let id: u64 = created
            .split_whitespace()
            .nth(3)
            .and_then(|w| w.trim_end_matches(':').parse().ok())
            .expect("id in create message");

        let waited = run(&format!(r#"{{"action":"wait","id":{id},"timeout":10}}"#))
            .await
            .expect("wait should succeed");
        assert!(waited.contains("completed"), "got: {waited}");
        assert!(waited.contains("task-tool-test"), "got: {waited}");

        let listed = run(r#"{"action":"list"}"#).await.expect("list");
        assert!(listed.contains(&format!("[{id}]")), "got: {listed}");

        let missing = run(r#"{"action":"output","id":999999}"#)
            .await
            .expect_err("unknown id should fail");
        assert!(missing.contains("no task"), "got: {missing}");
    }
}
