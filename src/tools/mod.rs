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
pub mod read_image;
pub mod task;
pub mod todo;
pub mod write_file;

use crate::consts::MAX_OUTPUT_LENGTH;
use async_openai::types::responses::{
    FunctionCallOutput, FunctionCallOutputItemParam, FunctionToolCall, Tool,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

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
    // Every branch yields a `Result<FunctionCallOutput, String>`: `Ok` is a
    // successful text or multimodal result, `Err` is a failure. This is the
    // single source of truth for the `failed` flag below.
    let result: Result<FunctionCallOutput, String> = if call.name.starts_with("mcp__") {
        mcp_bridge::run_mcp_call(call, mcp)
            .await
            .map(FunctionCallOutput::Text)
    } else if call.name == ask_user::NAME {
        // ask_user needs the UI channel, so it isn't part of run_local_tool.
        ask_user::run(
            &call.arguments,
            sender,
            &crate::cancel::CancellationToken::new(),
            0,
        )
        .await
        .map(FunctionCallOutput::Text)
    } else if call.name == read_image::NAME {
        read_image::run(&call.arguments).await
    } else {
        run_local_tool(&call.name, &call.arguments)
            .await
            .map(FunctionCallOutput::Text)
    };

    make_tool_output_for_call(call, result)
}

/// Wrap a call result into a [`ToolOutput`], archiving it first when it exceeds
/// the inline output budget.
pub(crate) fn make_tool_output_for_call(
    call: &FunctionToolCall,
    result: Result<FunctionCallOutput, String>,
) -> ToolOutput {
    make_tool_output_named(&call.name, &call.call_id, result)
}

fn make_tool_output_named(
    tool_name: &str,
    call_id: &str,
    result: Result<FunctionCallOutput, String>,
) -> ToolOutput {
    let (output, failed) = match result {
        Ok(FunctionCallOutput::Text(text)) => (
            FunctionCallOutput::Text(archive_and_truncate(tool_name, call_id, text)),
            false,
        ),
        Ok(output) => (output, false),
        Err(text) => (
            FunctionCallOutput::Text(archive_and_truncate(tool_name, call_id, text)),
            true,
        ),
    };

    ToolOutput {
        param: FunctionCallOutputItemParam {
            call_id: call_id.to_string(),
            output,
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
        todo::NAME => Err(
            "error: the todo tool requires a session and is unavailable in standalone MCP mode"
                .to_string(),
        ),
        task::NAME => task::run(arguments).await,
        other => Err(format!("error: unknown tool '{other}'")),
    }
}

pub(crate) async fn run_local_tool_secure(
    name: &str,
    arguments: &str,
    security: &crate::security::SecurityManager,
) -> Result<String, String> {
    security.authorize_tool_call(name, arguments)?;
    match name {
        command::NAME => {
            command::run_with_live_secure(
                arguments,
                "headless-command",
                &crate::cancel::CancellationToken::new(),
                security,
            )
            .await
        }
        read_file::NAME => read_file::run_with_security(arguments, security).await,
        write_file::NAME => write_file::run_with_security(arguments, security).await,
        edit_file::NAME => edit_file::run_with_security(arguments, security).await,
        task::NAME => task::run_with_security(arguments, security).await,
        _ => run_local_tool(name, arguments).await,
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
        task::tool(),
    ]
}

/// Archive long output before returning a bounded head/tail excerpt.
fn archive_and_truncate(tool_name: &str, call_id: &str, output: String) -> String {
    let len = output.chars().count();
    if len <= MAX_OUTPUT_LENGTH {
        return output;
    }

    let archive_note = match std::env::current_dir()
        .map_err(|error| error.to_string())
        .and_then(|root| archive_output_in(&root, tool_name, call_id, &output))
    {
        Ok(path) => format!("full output saved to {}", path.display()),
        Err(error) => format!("full output could not be saved: {error}"),
    };
    truncate_output(output, &archive_note)
}

fn truncate_output(output: String, archive_note: &str) -> String {
    let len = output.chars().count();
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
        "{head}\n\n... [truncated: {total} chars total, {skipped} chars skipped; {archive_note}] ...\n\n{tail}",
        total = len,
        skipped = len - head_keep - tail_keep,
    )
}

fn archive_output_in(
    root: &Path,
    tool_name: &str,
    call_id: &str,
    output: &str,
) -> Result<PathBuf, String> {
    static NEXT_ARCHIVE_ID: AtomicU64 = AtomicU64::new(1);

    let relative_dir = Path::new(".programmer").join("outputs");
    let output_dir = root.join(&relative_dir);
    std::fs::create_dir_all(&output_dir)
        .map_err(|error| format!("create {}: {error}", output_dir.display()))?;

    let sequence = NEXT_ARCHIVE_ID.fetch_add(1, Ordering::Relaxed);
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let filename = format!(
        "{}-{}-{timestamp}-{}-{sequence}.txt",
        safe_filename_part(tool_name),
        safe_filename_part(call_id),
        std::process::id(),
    );
    let relative_path = relative_dir.join(&filename);
    let path = root.join(&relative_path);
    let temporary = output_dir.join(format!(".{filename}.tmp"));

    std::fs::write(&temporary, output)
        .map_err(|error| format!("write {}: {error}", temporary.display()))?;
    if let Err(error) = std::fs::rename(&temporary, &path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("rename to {}: {error}", path.display()));
    }
    Ok(relative_path)
}

fn safe_filename_part(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .take(48)
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "output".to_string()
    } else {
        sanitized
    }
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

    #[test]
    fn archives_long_output_without_losing_content() {
        let root = std::env::temp_dir().join(format!(
            "programmer-output-archive-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let original = "résumé-output\n".repeat(MAX_OUTPUT_LENGTH);
        let relative =
            archive_output_in(&root, "mcp/unsafe", "../call", &original).expect("archive");
        assert!(relative.starts_with(".programmer/outputs"));
        assert_eq!(
            std::fs::read_to_string(root.join(relative)).unwrap(),
            original
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn safe_filename_part_removes_path_separators() {
        assert_eq!(safe_filename_part("../mcp/tool"), "___mcp_tool");
        assert_eq!(safe_filename_part(""), "output");
    }

    #[test]
    fn truncated_output_keeps_unicode_head_tail_and_archive_path() {
        let original = "α".repeat(MAX_OUTPUT_LENGTH) + &"ω".repeat(20);
        let text = truncate_output(
            original,
            "full output saved to .programmer/outputs/command-call.txt",
        );
        assert!(text.starts_with('α'));
        assert!(text.ends_with('ω'));
        assert!(text.contains(".programmer/outputs/command-call.txt"));
        assert!(text.contains("8020 chars total"));
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
        assert_eq!(
            prefix.last().map(String::as_str),
            Some(r"C:\scripts\deploy.ps1")
        );
        assert!(prefix.contains(&"-File".to_string()));
    }

    #[test]
    fn resolve_program_passes_unknown_through() {
        let (program, prefix) = resolve_program("definitely_not_a_real_tool_xyz");
        assert_eq!(program, "definitely_not_a_real_tool_xyz");
        assert!(prefix.is_empty());
    }
}
