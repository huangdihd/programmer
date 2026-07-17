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
         `write` (send a line of input to a running task's stdin; set eof to \
         close stdin — do this when the task reads input to end-of-stream), \
         `wait` (block until a task finishes or the timeout elapses), \
         `kill` (terminate a running task). \
         Tasks start with stdin open, so a command that reads input will wait \
         for `write`/eof rather than seeing an empty stream. \
         Set `interactive: true` on create to run the command in a real \
         terminal (PTY) instead — required for full-screen/interactive programs \
         (vim, htop, REPLs, menus). Drive an interactive task with `keys` (type \
         text and/or send named keys), click with `send_mouse` (only works when \
         the program has enabled mouse reporting — `screen` reports this), and \
         read what it displays with `screen`; `write`/`output` are for pipe \
         tasks only. \
         Prefer the `command` tool for anything that finishes quickly — use \
         background tasks only when the command should outlive the current step.",
        json!({
            "action": {
                "type": "string",
                "description": "The action to perform.",
                "enum": ["create", "list", "output", "write", "wait", "kill", "screen", "keys", "send_mouse", "resize"]
            },
            "interactive": {
                "type": "boolean",
                "description": "create only: run the command in a PTY so interactive/full-screen programs work. Drive it with the keys/screen actions."
            },
            "rows": {
                "type": "integer",
                "description": "Terminal height in rows for an interactive task (create/resize; default 24)."
            },
            "cols": {
                "type": "integer",
                "description": "Terminal width in columns for an interactive task (create/resize; default 80)."
            },
            "text": {
                "type": "string",
                "description": "keys only: literal text to type into an interactive task (no newline is added — send an `enter` key to submit)."
            },
            "keys": {
                "type": "array",
                "items": { "type": "string" },
                "description": "keys only: named keys to send in order, e.g. [\"enter\"], [\"ctrl-c\"], [\"up\",\"up\",\"enter\"]. Supports enter/tab/escape/backspace/space, arrows, home/end/pageup/pagedown, delete/insert, f1-f12, and ctrl-<letter>."
            },
            "x": {
                "type": "integer",
                "description": "send_mouse only: 0-based column (matches the screen text the `screen` action returns)."
            },
            "y": {
                "type": "integer",
                "description": "send_mouse only: 0-based row (matches the screen text the `screen` action returns)."
            },
            "button": {
                "type": "string",
                "description": "send_mouse only: mouse button — left (default), middle, or right.",
                "enum": ["left", "middle", "right"]
            },
            "mouse_action": {
                "type": "string",
                "description": "send_mouse only: gesture — click (press+release, default), press, release, scroll_up, scroll_down, or move.",
                "enum": ["click", "press", "release", "scroll_up", "scroll_down", "move"]
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
                "description": "The task id (required for output, write, wait, and kill)."
            },
            "input": {
                "type": "string",
                "description": "write only: text to send to the task's stdin. A trailing newline is appended automatically if missing."
            },
            "eof": {
                "type": "boolean",
                "description": "write only: close the task's stdin after writing (may be used alone, without input, to just signal end of input)."
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
    input: Option<String>,
    #[serde(default)]
    eof: Option<bool>,
    #[serde(default)]
    timeout: Option<u64>,
    #[serde(default)]
    tail: Option<usize>,
    #[serde(default)]
    interactive: Option<bool>,
    #[serde(default)]
    rows: Option<u16>,
    #[serde(default)]
    cols: Option<u16>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    keys: Option<Vec<String>>,
    #[serde(default)]
    x: Option<u16>,
    #[serde(default)]
    y: Option<u16>,
    #[serde(default)]
    button: Option<String>,
    #[serde(default)]
    mouse_action: Option<String>,
}

/// Which task actions mutate state (start/stop processes or drive them). Used
/// by the classifier: `create` runs an arbitrary command, `kill` terminates
/// one, and `write` feeds input that can confirm destructive prompts, so they
/// are gated like the `command` tool; the rest are read-only.
pub fn action_is_mutating(arguments: &str) -> bool {
    #[derive(Deserialize)]
    struct ActionOnly {
        action: String,
    }
    match serde_json::from_str::<ActionOnly>(arguments) {
        Ok(a) => matches!(
            a.action.as_str(),
            "create" | "kill" | "write" | "keys" | "send_mouse"
        ),
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
            if args.interactive.unwrap_or(false) {
                let rows = args.rows.unwrap_or(24).clamp(4, 200);
                let cols = args.cols.unwrap_or(80).clamp(20, 400);
                let id = tasks::spawn_interactive(
                    &command,
                    args.dir.as_deref(),
                    args.name.as_deref(),
                    rows,
                    cols,
                )?;
                return Ok(format!(
                    "started interactive task {id}: {command}\n\
                     It runs in a {cols}x{rows} terminal. Read what it shows with \
                     action=screen id={id}, type into it with action=keys id={id} \
                     (text and/or named keys), and stop it with action=kill id={id}."
                ));
            }
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

        "write" => {
            let id = require_id(args.id, "write")?;
            if tasks::is_interactive(id) {
                return Err(format!(
                    "error: task {id} is interactive — send input with action=keys, not write"
                ));
            }
            let eof = args.eof.unwrap_or(false);
            let mut input = args.input.unwrap_or_default();
            if input.is_empty() && !eof {
                return Err(
                    "error: 'input' (or eof=true) is required for write".to_string()
                );
            }
            // Line-based contract: prompts read whole lines, so make sure the
            // input is terminated.
            if !input.is_empty() && !input.ends_with('\n') {
                input.push('\n');
            }
            tasks::write_stdin(id, &input, eof)?;
            let mut msg = if input.is_empty() {
                format!("closed stdin of task {id}")
            } else {
                format!("sent input to task {id}")
            };
            if eof && !input.is_empty() {
                msg.push_str(" and closed its stdin");
            }
            msg.push_str(&format!(
                "\nCheck the task's reaction with action=output id={id}."
            ));
            Ok(msg)
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

        "screen" => {
            let id = require_id(args.id, "screen")?;
            let snap = tasks::screen_snapshot(id)?;
            let mut header = format!(
                "task {id} screen ({cols}x{rows}), cursor row={r} col={c}",
                cols = snap.cols,
                rows = snap.rows,
                r = snap.cursor_row,
                c = snap.cursor_col,
            );
            if snap.alt {
                header.push_str(", alt-screen");
            }
            header.push_str(if snap.mouse {
                ", mouse ON (mouse events can be forwarded)"
            } else {
                ", mouse off"
            });
            Ok(format!("{header}\n--- screen ---\n{}", snap.text))
        }

        "keys" => {
            let id = require_id(args.id, "keys")?;
            let mut bytes: Vec<u8> = Vec::new();
            if let Some(text) = args.text.as_deref() {
                bytes.extend_from_slice(text.as_bytes());
            }
            if let Some(keys) = args.keys.as_ref() {
                for key in keys {
                    match tasks::key_to_bytes(key) {
                        Some(b) => bytes.extend_from_slice(&b),
                        None => {
                            return Err(format!(
                                "error: unknown key '{key}' — use enter/tab/escape/backspace/\
                                 space, arrows, home/end/pageup/pagedown, delete/insert, \
                                 f1-f12, or ctrl-<letter>"
                            ));
                        }
                    }
                }
            }
            if bytes.is_empty() {
                return Err(
                    "error: 'text' and/or 'keys' is required for keys".to_string()
                );
            }
            tasks::write_bytes(id, &bytes)?;
            Ok(format!(
                "sent input to task {id}\nRead its updated screen with action=screen id={id}."
            ))
        }

        "send_mouse" => {
            let id = require_id(args.id, "send_mouse")?;
            let (Some(x), Some(y)) = (args.x, args.y) else {
                return Err("error: 'x' and 'y' are required for send_mouse".to_string());
            };
            // Gate on the child's mouse mode: sending mouse to a program that
            // hasn't asked for it does nothing useful.
            let mode = tasks::with_screen(id, |s| s.mouse_protocol_mode())
                .ok_or_else(|| format!("error: task {id} is not interactive"))?;
            if mode == vt100::MouseProtocolMode::None {
                return Err(format!(
                    "error: task {id}'s program has not enabled mouse reporting; \
                     use action=keys instead"
                ));
            }
            let base = match args.button.as_deref().unwrap_or("left") {
                "left" => 0u8,
                "middle" => 1,
                "right" => 2,
                other => return Err(format!("error: unknown button '{other}'")),
            };
            let gesture = args.mouse_action.as_deref().unwrap_or("click");
            let mut bytes: Vec<u8> = Vec::new();
            match gesture {
                "click" => {
                    bytes.extend(tasks::sgr_mouse(base, x, y, false));
                    bytes.extend(tasks::sgr_mouse(base, x, y, true));
                }
                "press" => bytes.extend(tasks::sgr_mouse(base, x, y, false)),
                "release" => bytes.extend(tasks::sgr_mouse(base, x, y, true)),
                "scroll_up" => bytes.extend(tasks::sgr_mouse(64, x, y, false)),
                "scroll_down" => bytes.extend(tasks::sgr_mouse(65, x, y, false)),
                "move" => bytes.extend(tasks::sgr_mouse(3 + 32, x, y, false)),
                other => return Err(format!("error: unknown mouse_action '{other}'")),
            }
            tasks::write_bytes(id, &bytes)?;
            Ok(format!(
                "sent mouse {gesture} at ({x},{y}) to task {id}\n\
                 Read the result with action=screen id={id}."
            ))
        }

        "resize" => {
            let id = require_id(args.id, "resize")?;
            let (Some(rows), Some(cols)) = (args.rows, args.cols) else {
                return Err("error: 'rows' and 'cols' are required for resize".to_string());
            };
            tasks::resize(id, rows.clamp(4, 200), cols.clamp(20, 400))?;
            Ok(format!("resized task {id} to {cols}x{rows}"))
        }

        other => Err(format!(
            "error: unknown action '{other}' — use create, list, output, write, wait, \
             kill, screen, keys, send_mouse, or resize"
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
    fn create_kill_write_are_mutating_but_views_are_not() {
        assert!(action_is_mutating(r#"{"action":"create","command":"x"}"#));
        assert!(action_is_mutating(r#"{"action":"kill","id":1}"#));
        assert!(action_is_mutating(r#"{"action":"write","id":1,"input":"y"}"#));
        assert!(!action_is_mutating(r#"{"action":"list"}"#));
        assert!(!action_is_mutating(r#"{"action":"output","id":1}"#));
        assert!(!action_is_mutating(r#"{"action":"wait","id":1}"#));
        // Garbage arguments are treated as mutating.
        assert!(action_is_mutating("not json"));
    }

    #[test]
    fn keys_action_is_mutating_screen_is_not() {
        assert!(action_is_mutating(r#"{"action":"keys","id":1,"text":"x"}"#));
        assert!(action_is_mutating(r#"{"action":"send_mouse","id":1,"x":1,"y":1}"#));
        assert!(!action_is_mutating(r#"{"action":"screen","id":1}"#));
        assert!(!action_is_mutating(r#"{"action":"resize","id":1,"rows":30,"cols":100}"#));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn send_mouse_requires_a_mouse_enabled_program() {
        // `cat` never enables mouse reporting, so send_mouse must refuse.
        let created = run(r#"{"action":"create","command":"cat","interactive":true}"#)
            .await
            .expect("create");
        let id: u64 = created
            .split_whitespace()
            .nth(3)
            .and_then(|w| w.trim_end_matches(':').parse().ok())
            .expect("id");
        let err = run(&format!(r#"{{"action":"send_mouse","id":{id},"x":1,"y":1}}"#))
            .await
            .expect_err("cat has no mouse reporting");
        assert!(err.contains("mouse reporting"), "got: {err}");
        let _ = run(&format!(r#"{{"action":"kill","id":{id}}}"#)).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn interactive_create_keys_screen_round_trip() {
        // `cat` echoes typed input; create it interactive, type, and read back.
        let created = run(r#"{"action":"create","command":"cat","interactive":true}"#)
            .await
            .expect("create should succeed");
        assert!(created.contains("interactive task"), "got: {created}");
        let id: u64 = created
            .split_whitespace()
            .nth(3)
            .and_then(|w| w.trim_end_matches(':').parse().ok())
            .expect("id in create message");

        // Pipe stdin is refused for interactive tasks.
        let refused = run(&format!(r#"{{"action":"write","id":{id},"input":"x"}}"#))
            .await
            .expect_err("write should be refused");
        assert!(refused.contains("interactive"), "got: {refused}");

        run(&format!(
            r#"{{"action":"keys","id":{id},"text":"marker-xyz","keys":["enter"]}}"#
        ))
        .await
        .expect("keys should succeed");
        tokio::time::sleep(Duration::from_millis(500)).await;

        let screen = run(&format!(r#"{{"action":"screen","id":{id}}}"#))
            .await
            .expect("screen should succeed");
        assert!(screen.contains("marker-xyz"), "screen: {screen}");

        let _ = run(&format!(r#"{{"action":"kill","id":{id}}}"#)).await;
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
