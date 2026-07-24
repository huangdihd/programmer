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

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use async_openai::types::responses::Tool;
use serde::Deserialize;
use serde_json::json;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use super::{function_tool, shell};

pub const NAME: &str = "command";

/// Cap on the live output buffer kept per running command. Only the tail is
/// rendered in the UI while a command runs, so older bytes are dropped once the
/// buffer grows past this.
const MAX_LIVE_OUTPUT: usize = 16_384;

/// In-flight command output, keyed by tool-call id. The `command` tool appends
/// to it as bytes arrive; the TUI reads it every frame (see
/// [`live_output`]) to render output in real time; the entry is removed when
/// the command finishes, after which the committed tool result renders instead.
fn live_registry() -> &'static Mutex<HashMap<String, String>> {
    static LIVE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    LIVE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Append a raw output chunk to a running command's live buffer, keeping only
/// the tail under [`MAX_LIVE_OUTPUT`]. Escape sequences are kept as-is and
/// cleaned lazily on read by [`live_output`].
fn append_live(call_id: &str, chunk: &str) {
    let mut reg = live_registry().lock().unwrap();
    let buf = reg.entry(call_id.to_string()).or_default();
    buf.push_str(chunk);
    if buf.len() > MAX_LIVE_OUTPUT {
        let mut cut = buf.len() - MAX_LIVE_OUTPUT;
        while !buf.is_char_boundary(cut) {
            cut += 1;
        }
        buf.replace_range(..cut, "");
    }
}

/// Drop a finished command's live buffer.
fn finish_live(call_id: &str) {
    live_registry().lock().unwrap().remove(call_id);
}

/// Snapshot the live (still-running) output for a command tool call, cleaned of
/// terminal control sequences, or `None` if that call isn't currently running.
/// Read by the conversation panel to render command output as it streams in.
pub fn live_output(call_id: &str) -> Option<String> {
    let raw = live_registry().lock().unwrap().get(call_id).cloned()?;
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
                "description": "Optional timeout in seconds. If the command runs longer, it will be killed. Default: 120."
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
    dir: Option<String>,
}

pub async fn run(arguments: &str) -> Result<String, String> {
    run_inner(arguments, None).await
}

/// Like [`run`], but streams the command's output to the live registry under
/// `call_id` while it runs, so the TUI can render it in real time. Used by the
/// agent's tool path (which has a call id); the plain [`run`] is used by the
/// MCP server and headless callers that have nowhere to show live output.
pub async fn run_with_live(arguments: &str, call_id: &str) -> Result<String, String> {
    let result = run_inner(arguments, Some(call_id)).await;
    // Always drop the live buffer — success, failure, or timeout — so the
    // committed result takes over and the registry never leaks an entry.
    finish_live(call_id);
    result
}

async fn run_inner(arguments: &str, live_id: Option<&str>) -> Result<String, String> {
    let args: Args = match serde_json::from_str(arguments) {
        Ok(args) => args,
        Err(error) => return Err(format!("error: invalid arguments: {error}")),
    };

    match execute(
        &args.command,
        args.dir.as_deref(),
        Some(args.timeout.unwrap_or(120)),
        live_id,
    )
    .await
    {
        Ok((code, stdout, stderr)) => {
            // The exit code is the authoritative success signal — a non-zero
            // status means the command failed, regardless of what it printed.
            let output = format_output(code, &stdout, &stderr);
            if code.unwrap_or(-1) != 0 {
                Err(output)
            } else {
                Ok(output)
            }
        }
        Err(error) => Err(format!("error: failed to run command: {error}")),
    }
}

/// Runs the command through the platform's native shell, draining stdout and
/// stderr incrementally so a full pipe never blocks the child and — when
/// `live_id` is set — the output is mirrored to the live registry as it
/// arrives. Returns the exit code and the complete stdout/stderr.
async fn execute(
    command: &str,
    dir: Option<&str>,
    timeout_secs: Option<u64>,
    live_id: Option<&str>,
) -> std::io::Result<(Option<i32>, String, String)> {
    let (program, flag) = shell();
    let mut cmd = Command::new(program);
    cmd.arg(flag);
    // On Windows, `Command::arg` re-quotes arguments that contain spaces,
    // which breaks the quoting already present in `command` (e.g.
    // `git commit -m "hello world"`).  `raw_arg` passes the string verbatim
    // so the shell receives the intended quoting.
    #[cfg(windows)]
    cmd.raw_arg(command);
    #[cfg(not(windows))]
    cmd.arg(command);
    cmd.kill_on_drop(true);

    if let Some(dir) = dir {
        cmd.current_dir(dir);
    }

    // On Windows, give the child its own (windowless) console. Otherwise the
    // spawned `cmd` shares the parent console and resets its input mode, which
    // re-enables quick-edit / disables mouse input — silently killing the TUI's
    // mouse capture (scrolling and clicks) for the rest of the session.
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    let live_id = live_id.map(str::to_string);
    let out_fut = drain(child.stdout.take(), live_id.clone());
    let err_fut = drain(child.stderr.take(), live_id);

    // Drain both pipes concurrently with the wait so a chatty child can't fill
    // a pipe and deadlock, and the live buffer keeps updating while we wait.
    let combined = async { tokio::join!(out_fut, err_fut, child.wait()) };

    let (stdout, stderr, status) = match timeout_secs {
        Some(secs) => match tokio::time::timeout(Duration::from_secs(secs), combined).await {
            Ok(triple) => triple,
            Err(_elapsed) => {
                // The `combined` future (holding the `child.wait()` borrow) is
                // dropped here; returning drops `child` itself, and
                // `kill_on_drop(true)` kills the process.
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("command timed out after {secs}s"),
                ));
            }
        },
        None => combined.await,
    };

    Ok((status?.code(), stdout, stderr))
}

/// Read a child pipe to EOF, returning everything it produced. When `live_id`
/// is set, each chunk is also appended to that call's live buffer so the UI can
/// render it before the command finishes.
async fn drain<R>(stream: Option<R>, live_id: Option<String>) -> String
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut acc = String::new();
    let Some(mut stream) = stream else {
        return acc;
    };
    let mut buf = [0u8; 4096];
    loop {
        match stream.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let chunk = String::from_utf8_lossy(&buf[..n]);
                if let Some(id) = &live_id {
                    append_live(id, &chunk);
                }
                acc.push_str(&chunk);
            }
        }
    }
    acc
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

        let handle = tokio::spawn(async move { run_with_live(args, call_id).await });

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
        assert!(seen, "live output should show the marker while the command runs");

        let result = handle.await.unwrap();
        assert!(result.unwrap().contains("streaming-marker"));
        // Once finished, the live buffer is removed so the committed result
        // renders instead.
        assert!(
            live_output(call_id).is_none(),
            "live buffer should be cleared after the command finishes"
        );
    }
}
