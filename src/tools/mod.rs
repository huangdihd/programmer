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

pub mod ask_user;
pub mod blob;
pub mod command;
pub mod configure_diagnostics;
pub mod diagnostics;
pub mod edit_file;
pub mod fetch;
pub mod grep;
pub(crate) mod mcp_bridge;
pub(crate) mod provider;
pub mod read_file;
pub mod task;
pub mod todo;
pub mod write_file;

use crate::consts::MAX_OUTPUT_LENGTH;
use async_openai::types::responses::{
    FunctionCallOutput, FunctionCallOutputItemParam, FunctionToolCall, Tool,
};

/// The host shell used by the `command` tool: `(program, flag)`.
pub fn shell() -> (&'static str, &'static str) {
    if cfg!(windows) {
        ("cmd", "/C")
    } else {
        ("sh", "-c")
    }
}

/// Resolve a program name into a concrete invocation: the executable to spawn
/// plus any arguments that must precede the caller's own.
///
/// On Windows, npm/pnpm/yarn global installs create `.cmd` and `.ps1` shims
/// rather than `.exe`s. `Command::new("codegraph")` ultimately calls
/// `CreateProcess`, which resolves a bare name only against `.exe` — so the
/// shim is never found and the spawn fails with "program not found" even
/// though `codegraph` is on `PATH`. Resolution order here:
///
/// 1. An explicit `.ps1` is wrapped in a `powershell.exe -File` invocation —
///    `CreateProcess` cannot start PowerShell scripts and std does not
///    special-case them.
/// 2. Otherwise resolve via the `which` crate (`PATH × PATHEXT`, what
///    `cmd.exe` itself would find); a `.cmd`/`.bat` hit comes back as a full
///    path and std then runs it via `cmd.exe` automatically.
/// 3. If that misses, look for `<name>.ps1` on `PATH` — `.PS1` is not in
///    `PATHEXT`, but PowerShell Gallery's `Install-Script` and hand-written
///    script dirs ship bare `.ps1` files with no `.cmd` companion — and wrap
///    it in PowerShell.
///
/// On non-Windows the name is returned unchanged with no extra arguments
/// (`execvp` handles `PATH` and shebangs natively).
pub fn resolve_program(program: &str) -> (String, Vec<String>) {
    #[cfg(windows)]
    {
        /// `.ps1` cannot be spawned directly; run it under the PowerShell
        /// host. `-ExecutionPolicy Bypass` scopes to this one process only.
        fn ps_wrap(script: String) -> (String, Vec<String>) {
            (
                "powershell.exe".to_string(),
                vec![
                    "-NoProfile".to_string(),
                    "-ExecutionPolicy".to_string(),
                    "Bypass".to_string(),
                    "-File".to_string(),
                    script,
                ],
            )
        }

        if program.to_ascii_lowercase().ends_with(".ps1") {
            // Resolve a bare script name to its PATH location if possible;
            // `-File` alone only looks in the current directory.
            let script = which::which(program)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| program.to_string());
            return ps_wrap(script);
        }
        if let Ok(found) = which::which(program) {
            return (found.to_string_lossy().into_owned(), Vec::new());
        }
        if let Ok(found) = which::which(format!("{program}.ps1")) {
            return ps_wrap(found.to_string_lossy().into_owned());
        }
    }
    (program.to_string(), Vec::new())
}

/// A short description of the runtime environment, appended to the system prompt
/// so the model knows which OS/shell/working directory it is operating in.
pub fn environment_info() -> String {
    let cwd = std::env::current_dir()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    let (program, _) = shell();
    let locale = std::env::var("LANG")
        .or_else(|_| std::env::var("LC_ALL"))
        .unwrap_or_else(|_| "unknown".to_string());

    let mut info = format!(
        "# Environment info\n\
         - Operating system: {os} ({arch})\n\
         - Shell for the `command` tool: {shell}\n\
         - Working directory: {cwd}\n\
         - System language / locale: {locale}",
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
        shell = program,
        cwd = cwd,
        locale = locale,
    );

    // Point the model at project resources without spending tokens on their
    // contents — it can read them on demand when relevant.
    if std::path::Path::new("PROGRAMMER.md").exists() {
        info.push_str(
            "\n- A project overview exists at PROGRAMMER.md — read it with \
             read_file when you need project context.",
        );
    }
    if std::path::Path::new(crate::diagnostics::PROFILE_PATH).exists() {
        info.push_str(
            "\n- A diagnostics profile is configured; edits are checked \
             automatically. Re-run setup any time with the /init flow or by \
             calling configure_diagnostics.",
        );
    }

    info
}

// The advertised tool list is now assembled by the `provider` layer: the
// built-ins are `provider::LocalToolProvider`, MCP servers are
// `provider::McpToolProvider`, and `provider::ToolRegistry` aggregates them.

/// A tool call's `function_call_output` together with whether the tool reported
/// failure. The flag is authoritative — it comes from the tool's own `Result`,
/// not from parsing the output text — so renderers, the classifier, and session
/// storage all read the same pre-computed answer instead of sniffing for an
/// `error:` prefix.
#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub param: FunctionCallOutputItemParam,
    pub failed: bool,
    /// Human-readable label explaining why this tool call was approved or
    /// denied (e.g. "approved by Auto mode", "denied in Manual mode by user").
    pub approval_label: Option<String>,
}

/// Executes a single tool call and wraps the result as a [`ToolOutput`] ready to
/// be sent back to the model and rendered.
///
/// When `mcp` is provided and the tool name starts with `mcp__`, the call is
/// forwarded to the appropriate MCP server.
/// The pre-provider dispatcher, kept for its focused tests: it exercises the
/// mcp / ask_user / local branches and the authoritative `failed` flag. The
/// agent path now dispatches through [`provider::ToolRegistry`] instead.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) async fn run_tool_call(
    call: &FunctionToolCall,
    sender: &tokio::sync::mpsc::UnboundedSender<crate::ui::event::Event>,
    mcp: Option<&crate::mcp::McpManager>,
) -> ToolOutput {
    // Every branch yields a `Result<String, String>`: `Ok` is a successful
    // result, `Err` is a failure. This is the single source of truth for the
    // `failed` flag below.
    let result: Result<String, String> = if call.name.starts_with("mcp__") {
        mcp_bridge::run_mcp_call(call, mcp).await
    } else if call.name == ask_user::NAME {
        // ask_user needs the UI channel, so it isn't part of run_local_tool.
        ask_user::run(&call.arguments, sender).await
    } else {
        run_local_tool(&call.name, &call.arguments).await
    };

    make_tool_output(&call.call_id, result)
}

/// Wrap a raw tool result into a [`ToolOutput`]: the `failed` flag comes straight
/// from `Ok`/`Err` (the authoritative source, never sniffed from the text), and
/// the text is truncated to the output budget.
pub(crate) fn make_tool_output(call_id: &str, result: Result<String, String>) -> ToolOutput {
    let (text, failed) = match result {
        Ok(text) => (text, false),
        Err(text) => (text, true),
    };
    let text = truncate_output(text);

    ToolOutput {
        param: FunctionCallOutputItemParam {
            call_id: call_id.to_string(),
            output: FunctionCallOutput::Text(text),
            id: None,
            status: None,
        },
        failed,
        approval_label: None,
    }
}

/// Dispatch a local (non-MCP, non-`ask_user`) tool by name. Shared by the
/// agent loop and the MCP server so both run tools the same way.
pub(crate) async fn run_local_tool(name: &str, arguments: &str) -> Result<String, String> {
    match name {
        command::NAME => command::run(arguments).await,
        read_file::NAME => read_file::run(arguments).await,
        write_file::NAME => write_file::run(arguments).await,
        edit_file::NAME => edit_file::run(arguments).await,
        grep::NAME => grep::run(arguments).await,
        blob::NAME => blob::run(arguments).await,
        fetch::NAME => fetch::run(arguments).await,
        configure_diagnostics::NAME => configure_diagnostics::run(arguments).await,
        diagnostics::NAME => diagnostics::run(arguments).await,
        todo::NAME => todo::run(arguments).await,
        task::NAME => task::run(arguments).await,
        other => Err(format!("error: unknown tool '{other}'")),
    }
}

/// The local tools exposed when running as an MCP server (`--mcp-server`).
/// Excludes `ask_user` (needs the interactive UI) and MCP passthrough tools.
pub(crate) fn mcp_server_tools() -> Vec<Tool> {
    vec![
        command::tool(),
        read_file::tool(),
        write_file::tool(),
        edit_file::tool(),
        grep::tool(),
        blob::tool(),
        fetch::tool(),
        diagnostics::tool(),
        todo::tool(),
        task::tool(),
    ]
}

/// Truncates `output` to at most [`MAX_OUTPUT_LENGTH`] characters. When the
/// output exceeds the limit the first half and the last quarter are preserved,
/// with a truncation marker in between so the model sees both the beginning and
/// the tail of a long result (the middle is often the least interesting part).
fn truncate_output(output: String) -> String {
    let len = output.chars().count();
    if len <= MAX_OUTPUT_LENGTH {
        return output;
    }
    let head_keep = MAX_OUTPUT_LENGTH * 3 / 4;
    let tail_keep = MAX_OUTPUT_LENGTH - head_keep;

    let head: String = output.chars().take(head_keep).collect();
    let tail: String = output
        .chars()
        .rev()
        .take(tail_keep)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();

    format!(
        "{head}\n\n... [truncated: {total} chars total, {skipped} chars skipped] ...\n\n{tail}",
        total = len,
        skipped = len - head_keep - tail_keep,
    )
}

/// A `Tool::Function` definition with a strict JSON-schema object for parameters.
/// `required` should list every property name for strict mode to validate.
fn function_tool(
    name: &str,
    description: &str,
    properties: serde_json::Value,
    required: &[&str],
) -> Tool {
    use async_openai::types::responses::FunctionTool;
    use serde_json::json;

    Tool::Function(FunctionTool {
        name: name.to_string(),
        description: Some(description.to_string()),
        parameters: Some(json!({
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false,
        })),
        strict: Some(true),
        defer_loading: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn command_runs_and_captures_output() {
        let out = command::run(r#"{"command":"echo hello"}"#)
            .await
            .expect("echo should succeed");
        assert!(out.contains("hello"), "unexpected output: {out}");
    }

    /// `run_tool_call` must set `failed` from the tool's own `Result`, not by
    /// parsing the output text — the whole point of the authoritative flag.
    #[tokio::test]
    async fn run_tool_call_reports_failure_authoritatively() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let call = |name: &str, args: &str| FunctionToolCall {
            arguments: args.to_string(),
            call_id: "c1".to_string(),
            namespace: None,
            name: name.to_string(),
            id: None,
            status: None,
        };

        // An unknown tool fails.
        let out = run_tool_call(&call("does_not_exist", "{}"), &tx, None).await;
        assert!(out.failed, "unknown tool should be marked failed");

        // A command with a non-zero exit fails; a clean one succeeds.
        let bad = run_tool_call(&call(command::NAME, r#"{"command":"exit 3"}"#), &tx, None).await;
        assert!(bad.failed, "non-zero exit should be marked failed");
        let good = run_tool_call(&call(command::NAME, r#"{"command":"exit 0"}"#), &tx, None).await;
        assert!(!good.failed, "zero exit should not be marked failed");
    }

    #[tokio::test]
    async fn write_read_edit_round_trip() {
        let dir = std::env::temp_dir().join(format!("programmer_tools_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("scratch.txt");
        // Escape backslashes so the path is valid inside a JSON string (Windows).
        let json_path = path.to_string_lossy().replace('\\', "\\\\");

        let wrote = write_file::run(&format!(
            r#"{{"path":"{json_path}","content":"alpha\nbeta\n"}}"#
        ))
        .await
        .expect("write should succeed");
        assert!(wrote.starts_with("wrote"), "unexpected: {wrote}");

        let read = read_file::run(&format!(r#"{{"path":"{json_path}"}}"#))
            .await
            .expect("read should succeed");
        assert_eq!(read, "alpha\nbeta");

        let edited = edit_file::run(&format!(
            r#"{{"path":"{json_path}","old_string":"alpha","new_string":"gamma"}}"#
        ))
        .await
        .expect("edit should succeed");
        assert_eq!(edited, format!("edited {}", path.to_string_lossy()));

        let read_again = read_file::run(&format!(r#"{{"path":"{json_path}"}}"#))
            .await
            .expect("read should succeed");
        assert_eq!(read_again, "gamma\nbeta");

        let missing = edit_file::run(&format!(
            r#"{{"path":"{json_path}","old_string":"nope","new_string":"x"}}"#
        ))
        .await
        .expect_err("edit with missing old_string should fail");
        assert!(
            missing.starts_with("error: old_string not found"),
            "got: {missing}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(windows)]
    fn resolve_program_finds_exe_on_path() {
        let (program, prefix) = resolve_program("cmd");
        assert!(
            program.to_ascii_lowercase().ends_with("cmd.exe"),
            "got: {program}"
        );
        assert!(prefix.is_empty());
    }

    #[test]
    #[cfg(windows)]
    fn resolve_program_wraps_explicit_ps1() {
        let (program, prefix) = resolve_program(r"C:\scripts\deploy.ps1");
        assert_eq!(program, "powershell.exe");
        assert_eq!(prefix.last().map(String::as_str), Some(r"C:\scripts\deploy.ps1"));
        assert!(prefix.contains(&"-File".to_string()));
    }

    #[test]
    fn resolve_program_passes_unknown_through() {
        let (program, prefix) = resolve_program("definitely_not_a_real_tool_xyz");
        assert_eq!(program, "definitely_not_a_real_tool_xyz");
        assert!(prefix.is_empty());
    }
}
