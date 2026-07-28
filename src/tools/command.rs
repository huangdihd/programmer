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

pub const NAME: &str = "command";

/// Snapshot the live (still-running) output for a command tool call, cleaned of
/// terminal control sequences, or `None` if that call isn't currently running.
/// Read by the conversation panel to render command output as it streams in.
pub fn live_output(call_id: &str) -> Option<String> {
    let raw = crate::tasks::command_live_output(call_id)?;
    Some(clean_terminal_output(&raw))
}

pub fn tool() -> Tool {
    function_tool(
        NAME,
        "Execute a shell command in the user's project directory and return its \
         combined stdout/stderr and exit code. The command runs through the host \
         platform's shell (see the environment info in the system prompt).",
        json!({
            "command": {
                "type": "string",
                "description": "The shell command to execute."
            },
            "timeout": {
                "type": "integer",
                "description": "Optional timeout in seconds. Default: 120."
            },
            "timeout_action": {
                "type": "string",
                "enum": ["kill", "background"],
                "description": "What to do when timeout elapses: kill the command (default), or keep it running as a background task."
            },
            "dir": {
                "type": "string",
                "description": "Optional working directory for the command. Default: the project directory."
            }
        }),
        &["command"],
    )
}

#[derive(Deserialize)]
struct Args {
    command: String,
    #[serde(default)]
    timeout: Option<u64>,
    #[serde(default)]
    timeout_action: TimeoutAction,
    #[serde(default)]
    dir: Option<String>,
}

#[derive(Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum TimeoutAction {
    #[default]
    Kill,
    Background,
}

pub async fn run(arguments: &str) -> Result<String, String> {
    run_inner(arguments, None, &crate::cancel::CancellationToken::new()).await
}

/// Like [`run`], but streams the command's output to the live registry under
/// `call_id` while it runs, so the TUI can render it in real time. Used by the
/// agent's tool path (which has a call id); the plain [`run`] is used by the
/// MCP server and headless callers that have nowhere to show live output.
pub async fn run_with_live(
    arguments: &str,
    call_id: &str,
    cancel: &crate::cancel::CancellationToken,
) -> Result<String, String> {
    run_inner(arguments, Some(call_id), cancel).await
}

async fn run_inner(
    arguments: &str,
    live_id: Option<&str>,
    cancel: &crate::cancel::CancellationToken,
) -> Result<String, String> {
    let args: Args = match serde_json::from_str(arguments) {
        Ok(args) => args,
        Err(error) => return Err(format!("error: invalid arguments: {error}")),
    };

    let id = crate::tasks::spawn_command(&args.command, args.dir.as_deref(), live_id)
        .map_err(|error| command_spawn_error(&error))?;
    let timeout_secs = args.timeout.unwrap_or(120);
    let wait = wait_for_finish_or_promotion(id);
    let wait_or_cancel = async {
        tokio::select! {
            biased;
            result = wait => Some(result),
            _ = cancel.wait() => None,
        }
    };
    let outcome = tokio::time::timeout(Duration::from_secs(timeout_secs), wait_or_cancel).await;

    match outcome {
        Ok(Some(Ok(CommandOutcome::Finished(snapshot)))) => completed_result(id, snapshot),
        Ok(Some(Ok(CommandOutcome::Promoted))) => Ok(background_result(id)),
        Ok(Some(Err(error))) => {
            crate::tasks::forget_command(id);
            Err(error)
        }
        Ok(None) if crate::tasks::is_background(id) => Ok(background_result(id)),
        Ok(None) => {
            stop_command(id).await;
            crate::tasks::forget_command(id);
            Err("error: failed to run command: cancelled".to_string())
        }
        Err(_) if args.timeout_action == TimeoutAction::Background => {
            match crate::tasks::promote_command(id) {
                Ok(()) => Ok(background_result(id)),
                Err(_) => completed_result(id, crate::tasks::wait_until_finished(id).await?),
            }
        }
        Err(_) if crate::tasks::is_background(id) => Ok(background_result(id)),
        Err(_) => {
            stop_command(id).await;
            crate::tasks::forget_command(id);
            Err(format!(
                "error: failed to run command: command timed out after {timeout_secs}s"
            ))
        }
    }
}

enum CommandOutcome {
    Finished(crate::tasks::TaskSnapshot),
    Promoted,
}

async fn wait_for_finish_or_promotion(id: u64) -> Result<CommandOutcome, String> {
    tokio::select! {
        biased;
        result = crate::tasks::wait_until_finished(id) => {
            result.map(CommandOutcome::Finished)
        }
        result = crate::tasks::wait_until_promoted(id) => {
            match result? {
                true => Ok(CommandOutcome::Promoted),
                false => crate::tasks::wait_until_finished(id)
                    .await
                    .map(CommandOutcome::Finished),
            }
        }
    }
}

fn completed_result(id: u64, snapshot: crate::tasks::TaskSnapshot) -> Result<String, String> {
    // The exit code is the authoritative success signal — a non-zero status
    // means the command failed, regardless of what it printed.
    let output = format_output(snapshot.exit_code, &snapshot.output, &snapshot.stderr);
    crate::tasks::forget_command(id);
    if snapshot.exit_code.unwrap_or(-1) == 0 {
        Ok(output)
    } else {
        Err(output)
    }
}

fn background_result(id: u64) -> String {
    format!(
        "Command is still running as background task {id}.\n\
         Its stdin is closed. Use the task tool or /terminal {id} to inspect or stop it."
    )
}

fn command_spawn_error(error: &str) -> String {
    let detail = error
        .strip_prefix("error: failed to spawn task: ")
        .unwrap_or(error);
    format!("error: failed to run command: {detail}")
}

async fn stop_command(id: u64) {
    let _ = crate::tasks::kill(id);
    let _ = crate::tasks::wait_until_finished(id).await;
}

fn format_output(code: Option<i32>, stdout: &str, stderr: &str) -> String {
    let failed = code.unwrap_or(-1) != 0;
    // CLIs often force colour and draw progress bars even when their output is
    // a pipe; clean the terminal control noise so the conversation and the
    // tokens sent to the model stay readable.
    let stdout = clean_terminal_output(stdout);
    let stderr = clean_terminal_output(stderr);
    let mut body = String::new();
    if !stdout.is_empty() {
        body.push_str(&stdout);
    }
    if !stderr.is_empty() {
        push_line_break(&mut body);
        body.push_str(&stderr);
    }
    if failed {
        push_line_break(&mut body);
        body.push_str(&format!("[exit code: {}]", code.unwrap_or(-1)));
    }
    let mut result = String::new();
    if failed {
        result.push_str("error: ");
    }
    result.push_str(&body);
    if result.is_empty() {
        result.push_str("[no output]");
    }
    result
}

fn push_line_break(text: &mut String) {
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
}

/// Strip ANSI escape sequences (CSI, OSC, ESC) and apply carriage-return
/// overwrite semantics so progress bars collapse to their final frame. The
/// model sees clean text and we don't burn tokens on terminal control noise.
fn clean_terminal_output(input: &str) -> String {
    // Fast path: most output has no escapes or carriage returns.
    if !input.contains('\u{1b}') && !input.contains('\r') {
        return input.to_string();
    }

    // Process the input in terminal order: ANSI escape sequences (especially
    // CSI erase-in-line) and carriage returns are applied line by line against
    // a virtual buffer, so the final result matches what a real terminal would
    // display.
    let mut lines: Vec<String> = Vec::new();
    let mut buf: Vec<char> = Vec::new();
    let mut pos = 0usize;
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '\n' => {
                lines.push(buf.iter().collect());
                buf.clear();
                pos = 0;
            }
            '\r' => {
                pos = 0;
            }
            '\u{1b}' => match chars.peek().copied() {
                Some('[') => {
                    chars.next(); // consume '['
                    // Collect CSI parameter bytes (digits, semicolons, '?', etc.)
                    let mut params = String::new();
                    let final_byte = loop {
                        match chars.next() {
                            Some(b) if ('\u{40}'..='\u{7e}').contains(&b) => break b,
                            Some(b) => {
                                params.push(b);
                            }
                            None => {
                                // Truncated escape at end of input.
                                lines.push(buf.iter().collect());
                                return lines.join("\n");
                            }
                        }
                    };
                    match final_byte {
                        'K' => {
                            // Erase-in-line.
                            match params.as_str() {
                                "0" | "" => {
                                    // Clear from cursor to end.
                                    buf.truncate(pos);
                                }
                                "1" => {
                                    // Clear from beginning to cursor.
                                    let keep = buf.len().saturating_sub(pos);
                                    buf.drain(..pos);
                                    pos = 0;
                                    let _ = keep;
                                }
                                "2" => {
                                    // Clear entire line.
                                    buf.clear();
                                    pos = 0;
                                }
                                _ => {} // unknown, ignore
                            }
                        }
                        'J' => {
                            // Erase-in-display — ignore for line-based cleanup.
                        }
                        _ => {
                            // Other CSI (colours, cursor movement, etc.) — ignore.
                        }
                    }
                }
                Some(']') => {
                    chars.next(); // consume ']'
                    // OSC: consume until BEL or ST (ESC \).
                    loop {
                        match chars.next() {
                            Some('\u{07}') => break,
                            Some('\u{1b}') => {
                                if chars.peek() == Some(&'\\') {
                                    chars.next();
                                }
                                break;
                            }
                            None => break,
                            _ => {}
                        }
                    }
                }
                Some(_) => {
                    // Lone ESC + single byte (charset selection, etc.) — drop both.
                    chars.next();
                }
                None => {}
            },
            _ => {
                // Normal character: write at current position.
                if pos < buf.len() {
                    buf[pos] = c;
                } else {
                    buf.push(c);
                }
                pos += 1;
            }
        }
    }
    // Flush the last line.
    lines.push(buf.iter().collect());

    lines.join("\n")
}

#[cfg(test)]
mod clean_tests {
    use super::*;

    #[test]
    fn strips_sgr_colour_codes() {
        let input = "\u{1b}[38;2;206;146;23m\u{1b}[1mhi\u{1b}[0m there";
        assert_eq!(clean_terminal_output(input), "hi there");
    }

    #[test]
    fn strips_erase_line_and_keeps_text() {
        let input = "start\u{1b}[Kend";
        assert_eq!(clean_terminal_output(input), "startend");
    }

    #[test]
    fn carriage_return_collapses_overwrites() {
        let input = "downloading\u{1b}[K\r\u{1b}[2Kdone";
        assert_eq!(clean_terminal_output(input), "done");
    }

    #[test]
    fn osc_hyperlink_is_removed() {
        let input = "\u{1b}]8;;https://eg.com\u{1b}\\link\u{1b}]8;;\u{1b}\\";
        assert_eq!(clean_terminal_output(input), "link");
    }

    #[test]
    fn alternat_screen_buffer_clear_survive() {
        let input = "before\u{1b}[?1049h\u{1b}[2J\u{1b}[?1049lafter";
        assert_eq!(clean_terminal_output(input), "beforeafter");
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;

    fn long_command() -> &'static str {
        if cfg!(windows) {
            "ping -n 30 127.0.0.1 > NUL"
        } else {
            "sleep 30"
        }
    }

    #[tokio::test]
    async fn live_output_streams_while_running_then_clears() {
        // A command that prints a marker immediately, then stays alive briefly,
        // so the live buffer can be observed before the command finishes.
        let call_id = "live-output-test";
        let args = if cfg!(windows) {
            r#"{"command":"echo streaming-marker && ping -n 3 127.0.0.1 > NUL"}"#
        } else {
            r#"{"command":"echo streaming-marker && sleep 1"}"#
        };

        let cancel = crate::cancel::CancellationToken::new();
        let handle = tokio::spawn(async move { run_with_live(args, call_id, &cancel).await });

        // Poll for the marker to appear in the live buffer while running.
        let mut seen = false;
        for _ in 0..60 {
            if let Some(out) = live_output(call_id)
                && out.contains("streaming-marker")
            {
                seen = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(
            seen,
            "live output should show the marker while the command runs"
        );

        let result = handle.await.unwrap();
        assert!(result.unwrap().contains("streaming-marker"));
        // Once finished, the live buffer is removed so the committed result
        // renders instead.
        assert!(
            live_output(call_id).is_none(),
            "live buffer should be cleared after the command finishes"
        );
    }

    #[tokio::test]
    async fn timeout_kills_the_command_task() {
        let args = format!(r#"{{"command":{},"timeout":0}}"#, json!(long_command()));
        let error = run(&args).await.expect_err("command should time out");
        assert!(
            error.contains("command timed out after 0s"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn cancellation_kills_the_command_task() {
        let args = format!(r#"{{"command":{}}}"#, json!(long_command()));
        let cancel = crate::cancel::CancellationToken::new();
        let child = cancel.child();
        let handle =
            tokio::spawn(async move { run_with_live(&args, "cancel-command-test", &child).await });
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel.cancel();

        let error = handle
            .await
            .expect("join")
            .expect_err("command should be cancelled");
        assert!(error.contains("cancelled"), "unexpected error: {error}");
        assert!(live_output("cancel-command-test").is_none());
    }

    #[tokio::test]
    async fn timeout_can_promote_instead_of_kill() {
        let args = format!(
            r#"{{"command":{},"timeout":0,"timeout_action":"background"}}"#,
            json!(long_command())
        );
        let result = run(&args).await.expect("command should move to background");
        let id = background_task_id(&result);
        let snapshot = crate::tasks::snapshot_all()
            .into_iter()
            .find(|task| task.id == id)
            .expect("promoted task should be listed");
        assert_eq!(snapshot.status, crate::tasks::TaskStatus::Running);

        crate::tasks::kill(id).expect("kill promoted task");
        let _ = crate::tasks::wait_until_finished(id)
            .await
            .expect("task stops");
    }

    #[tokio::test]
    async fn manual_promotion_completes_the_tool_call_without_killing() {
        let call_id = "manual-promote-command";
        let command = if cfg!(windows) {
            "echo manual-ready && ping -n 30 127.0.0.1 > NUL"
        } else {
            "echo manual-ready && sleep 30"
        };
        let args = format!(r#"{{"command":{}}}"#, json!(command));
        let cancel = crate::cancel::CancellationToken::new();
        let child = cancel.child();
        let handle = tokio::spawn(async move { run_with_live(&args, call_id, &child).await });

        let mut ready = false;
        for _ in 0..60 {
            if live_output(call_id).is_some_and(|output| output.contains("manual-ready")) {
                ready = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(ready, "command should be running before manual promotion");
        let id = crate::tasks::promote_command_for_call(call_id).expect("manual promotion");
        // Once ownership changes, cancelling the foreground turn must not
        // reclaim and kill the promoted process.
        cancel.cancel();
        let result = handle
            .await
            .expect("join")
            .expect("promotion is a successful tool result");
        assert_eq!(background_task_id(&result), id);
        assert_eq!(
            crate::tasks::snapshot(id).expect("same task").status,
            crate::tasks::TaskStatus::Running
        );

        crate::tasks::kill(id).expect("kill promoted task");
        let _ = crate::tasks::wait_until_finished(id)
            .await
            .expect("task stops");
    }

    fn background_task_id(result: &str) -> u64 {
        result
            .strip_prefix("Command is still running as background task ")
            .and_then(|rest| rest.split('.').next())
            .and_then(|id| id.parse().ok())
            .unwrap_or_else(|| panic!("missing background task id in: {result}"))
    }
}
