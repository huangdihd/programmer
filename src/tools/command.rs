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
    let args: Args = match serde_json::from_str(arguments) {
        Ok(args) => args,
        Err(error) => return Err(format!("error: invalid arguments: {error}")),
    };

    match execute(&args.command, args.dir.as_deref(), Some(args.timeout.unwrap_or(120))).await {
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
