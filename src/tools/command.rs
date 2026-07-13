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
use tokio::process::Command;

use super::{function_tool, shell};

pub const NAME: &str = "command";

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
                "description": "Optional timeout in seconds. If the command runs longer, it will be killed. Default: no timeout."
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

pub async fn run(arguments: &str) -> String {
    let args: Args = match serde_json::from_str(arguments) {
        Ok(args) => args,
        Err(error) => return format!("error: invalid arguments: {error}"),
    };

    match execute(&args.command, args.dir.as_deref(), args.timeout).await {
        Ok((code, stdout, stderr)) => format_output(code, &stdout, &stderr),
        Err(error) => format!("error: failed to run command: {error}"),
    }
}

/// Runs the command through the platform's native shell.
async fn execute(
    command: &str,
    dir: Option<&str>,
    timeout_secs: Option<u64>,
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

    let child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    let output = match timeout_secs {
        Some(secs) => {
            match tokio::time::timeout(Duration::from_secs(secs), child.wait_with_output()).await {
                Ok(result) => result?,
                Err(_elapsed) => {
                    // `child` was moved into `wait_with_output`, whose future
                    // got cancelled. `kill_on_drop(true)` on the command
                    // builder ensures the child process is killed on drop.
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!("command timed out after {secs}s"),
                    ));
                }
            }
        }
        None => child.wait_with_output().await?,
    };

    Ok((
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    ))
}

fn format_output(code: Option<i32>, stdout: &str, stderr: &str) -> String {
    // CLIs often force colour and draw progress bars even when their output is
    // a pipe; clean the terminal control noise so the conversation and the
    // tokens sent to the model stay readable.
    let stdout = clean_terminal_output(stdout);
    let stderr = clean_terminal_output(stderr);
    let mut result = String::new();
    if !stdout.is_empty() {
        result.push_str(&stdout);
    }
    if !stderr.is_empty() {
        push_line_break(&mut result);
        result.push_str(&stderr);
    }
    if code.unwrap_or(-1) != 0 {
        push_line_break(&mut result);
        result.push_str(&format!("[exit code: {}]", code.unwrap_or(-1)));
    }
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
    fn collapses_progress_bar_redraws() {
        // A carriage-return progress bar keeps rewriting the same line.
        let input = "Parsing  0%\rParsing 40%\rParsing 100%";
        assert_eq!(clean_terminal_output(input), "Parsing 100%");
    }

    #[test]
    fn ansi_progress_bar_reduces_to_final_frame() {
        // ESC[K (erase) + CR redraw, as emitted by many CLIs.
        let input =
            "\u{1b}[K\rload \u{1b}[32m10%\u{1b}[0m\u{1b}[K\rload \u{1b}[32m100%\u{1b}[0m";
        assert_eq!(clean_terminal_output(input), "load 100%");
    }

    #[test]
    fn plain_text_is_unchanged() {
        let input = "line one\nline two\n";
        assert_eq!(clean_terminal_output(input), "line one\nline two\n");
    }
}

/// Strip terminal control noise from captured command output: ANSI escape
/// sequences (colours, cursor moves, line erases) and carriage-return redraws
/// (progress bars). The result is the plain text a human would ultimately see.
fn clean_terminal_output(input: &str) -> String {
    let stripped = strip_ansi(input);
    let mut out = String::with_capacity(stripped.len());
    for (i, line) in stripped.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if line.contains('\r') {
            out.push_str(&apply_carriage_returns(line));
        } else {
            out.push_str(line);
        }
    }
    out
}

/// Remove ANSI escape sequences: CSI (`ESC [ … final`), OSC (`ESC ] … BEL/ST`),
/// and other two-byte `ESC x` sequences.
fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('[') => {
                chars.next();
                // CSI: consume params/intermediates up to a final byte @–~.
                while let Some(&nc) = chars.peek() {
                    chars.next();
                    if ('\u{40}'..='\u{7e}').contains(&nc) {
                        break;
                    }
                }
            }
            Some(']') => {
                chars.next();
                // OSC: consume until BEL or ST (ESC \).
                while let Some(&nc) = chars.peek() {
                    if nc == '\u{07}' {
                        chars.next();
                        break;
                    }
                    if nc == '\u{1b}' {
                        chars.next();
                        if chars.peek() == Some(&'\\') {
                            chars.next();
                        }
                        break;
                    }
                    chars.next();
                }
            }
            // A lone ESC or a short sequence (charset selection, etc.): drop the
            // ESC and its single following byte.
            Some(_) => {
                chars.next();
            }
            None => {}
        }
    }
    out
}

/// Apply carriage-return semantics within a single line: `\r` moves the write
/// cursor back to column 0, so later text overwrites earlier text. Collapses a
/// progress bar's many redraws down to its final frame.
fn apply_carriage_returns(line: &str) -> String {
    let mut buf: Vec<char> = Vec::new();
    let mut pos = 0usize;
    for c in line.chars() {
        if c == '\r' {
            pos = 0;
        } else {
            if pos < buf.len() {
                buf[pos] = c;
            } else {
                buf.push(c);
            }
            pos += 1;
        }
    }
    buf.into_iter().collect()
}
