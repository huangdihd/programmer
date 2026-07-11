// Copyright (C) 2025 huangdihd
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
            }
        }),
        &["command"],
    )
}

#[derive(Deserialize)]
struct Args {
    command: String,
}

pub async fn run(arguments: &str) -> String {
    let args: Args = match serde_json::from_str(arguments) {
        Ok(args) => args,
        Err(error) => return format!("error: invalid arguments: {error}"),
    };

    match execute(&args.command).await {
        Ok((code, stdout, stderr)) => format_output(code, &stdout, &stderr),
        Err(error) => format!("error: failed to run command: {error}"),
    }
}

/// Runs the command through the platform's native shell.
async fn execute(command: &str) -> std::io::Result<(Option<i32>, String, String)> {
    let (program, flag) = shell();
    let mut cmd = Command::new(program);
    cmd.arg(flag).arg(command);

    // On Windows, give the child its own (windowless) console. Otherwise the
    // spawned `cmd` shares the parent console and resets its input mode, which
    // re-enables quick-edit / disables mouse input — silently killing the TUI's
    // mouse capture (scrolling and clicks) for the rest of the session.
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let output = cmd.output().await?;
    Ok((
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    ))
}

fn format_output(code: Option<i32>, stdout: &str, stderr: &str) -> String {
    let mut result = String::new();
    if !stdout.is_empty() {
        result.push_str(stdout);
    }
    if !stderr.is_empty() {
        push_line_break(&mut result);
        result.push_str(stderr);
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
