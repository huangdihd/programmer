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
pub mod grep;
pub mod read_file;
pub mod todo;
pub mod write_file;

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

/// The full set of tool definitions advertised to the model on every request.
///
/// When `mcp` is provided, tools discovered from MCP servers are merged into
/// the list with `mcp__<server>__<tool>` names.
pub(crate) fn tools(mcp: Option<&crate::mcp::McpManager>) -> Vec<Tool> {
    let mut tools: Vec<Tool> = vec![
        command::tool(),
        read_file::tool(),
        write_file::tool(),
        edit_file::tool(),
        grep::tool(),
        blob::tool(),
        ask_user::tool(),
        configure_diagnostics::tool(),
        diagnostics::tool(),
        todo::tool(),
    ];

    if let Some(mgr) = mcp {
        for (fqn, mcp_tool) in mgr.all_tools() {
            tools.push(mcp_function_tool(
                &fqn,
                mcp_tool.description,
                mcp_tool.inputSchema,
            ));
        }
    }

    tools
}

/// Bridge one MCP tool into an OpenAI function tool.
///
/// An MCP tool's `inputSchema` is already a complete JSON Schema object (with
/// its own `properties`/`required`), so it becomes the function `parameters`
/// verbatim. Wrapping it in another `{ properties: … }` would misplace its
/// `required` array as a property value and the API rejects the whole tool.
/// MCP schemas rarely satisfy OpenAI's strict-mode constraints, so strict
/// validation is disabled for them.
fn mcp_function_tool(
    fqn: &str,
    description: Option<String>,
    input_schema: serde_json::Value,
) -> Tool {
    use async_openai::types::responses::FunctionTool;

    let description = description.unwrap_or_else(|| format!("MCP tool: {fqn}"));
    let parameters = if input_schema.is_object() {
        input_schema
    } else {
        // No-parameter tools may send a null/empty schema; give the API a
        // valid empty object rather than a bare scalar.
        serde_json::json!({ "type": "object", "properties": {} })
    };
    Tool::Function(FunctionTool {
        name: fqn.to_string(),
        description: Some(description),
        parameters: Some(parameters),
        strict: Some(false),
        defer_loading: None,
    })
}

/// Maximum characters of tool output kept before truncation. The rest is
/// discarded and a truncation notice is appended so the model knows the output
/// was cut short.
const MAX_OUTPUT_LENGTH: usize = 8000;

/// Executes a single tool call and wraps the result as a `function_call_output`
/// item ready to be sent back to the model.
///
/// When `mcp` is provided and the tool name starts with `mcp__`, the call is
/// forwarded to the appropriate MCP server.
pub(crate) async fn run_tool_call(
    call: &FunctionToolCall,
    sender: &tokio::sync::mpsc::UnboundedSender<crate::ui::event::Event>,
    mcp: Option<&crate::mcp::McpManager>,
) -> FunctionCallOutputItemParam {
    let output = if call.name.starts_with("mcp__") {
        if let Some(mgr) = mcp {
            match mgr
                .call_tool(
                    &call.name,
                    serde_json::from_str(&call.arguments).unwrap_or(serde_json::Value::Null),
                )
                .await
            {
                Ok(result) => result
                    .content
                    .iter()
                    .map(|c| match c {
                        crate::mcp::types::ToolContent::Text { text } => text.clone(),
                        _ => "[non-text MCP content]".to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                Err(e) => format!("error: MCP tool call failed: {e}"),
            }
        } else {
            "error: MCP not available (no servers connected)".to_string()
        }
    } else {
        match call.name.as_str() {
            command::NAME => command::run(&call.arguments).await,
            read_file::NAME => read_file::run(&call.arguments).await,
            write_file::NAME => write_file::run(&call.arguments).await,
            edit_file::NAME => edit_file::run(&call.arguments).await,
            grep::NAME => grep::run(&call.arguments).await,
            blob::NAME => blob::run(&call.arguments).await,
            ask_user::NAME => ask_user::run(&call.arguments, sender).await,
            configure_diagnostics::NAME => configure_diagnostics::run(&call.arguments).await,
            diagnostics::NAME => diagnostics::run(&call.arguments).await,
            todo::NAME => todo::run(&call.arguments).await,
            other => format!("error: unknown tool '{other}'"),
        }
    };

    let output = truncate_output(output);

    FunctionCallOutputItemParam {
        call_id: call.call_id.clone(),
        output: FunctionCallOutput::Text(output),
        id: None,
        status: None,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_schema_passes_through_without_rewrapping() {
        // A typical MCP inputSchema with a required field.
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "query": { "type": "string" } },
            "required": ["query"]
        });
        let Tool::Function(f) =
            mcp_function_tool("mcp__codegraph__search", Some("desc".into()), schema.clone())
        else {
            panic!("expected a function tool");
        };
        // Parameters must be the schema verbatim — NOT wrapped so that
        // `required` lands inside `properties` (which the API rejects).
        assert_eq!(f.parameters.as_ref().unwrap(), &schema);
        assert_eq!(f.parameters.as_ref().unwrap()["required"], serde_json::json!(["query"]));
        assert!(f.parameters.as_ref().unwrap()["properties"].get("required").is_none());
        assert_eq!(f.strict, Some(false));
        assert_eq!(f.name, "mcp__codegraph__search");
    }

    #[test]
    fn mcp_non_object_schema_becomes_empty_object() {
        let Tool::Function(f) =
            mcp_function_tool("mcp__x__y", None, serde_json::Value::Null)
        else {
            panic!("expected a function tool");
        };
        assert_eq!(f.parameters.as_ref().unwrap()["type"], "object");
        assert!(f.description.as_deref().unwrap().contains("mcp__x__y"));
    }

    #[tokio::test]
    async fn command_runs_and_captures_output() {
        let out = command::run(r#"{"command":"echo hello"}"#).await;
        assert!(out.contains("hello"), "unexpected output: {out}");
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
        .await;
        assert!(wrote.starts_with("wrote"), "unexpected: {wrote}");

        let read = read_file::run(&format!(r#"{{"path":"{json_path}"}}"#)).await;
        assert_eq!(read, "alpha\nbeta");

        let edited = edit_file::run(&format!(
            r#"{{"path":"{json_path}","old_string":"alpha","new_string":"gamma"}}"#
        ))
        .await;
        assert_eq!(edited, format!("edited {}", path.to_string_lossy()));

        let read_again = read_file::run(&format!(r#"{{"path":"{json_path}"}}"#)).await;
        assert_eq!(read_again, "gamma\nbeta");

        let missing = edit_file::run(&format!(
            r#"{{"path":"{json_path}","old_string":"nope","new_string":"x"}}"#
        ))
        .await;
        assert!(
            missing.starts_with("error: old_string not found"),
            "got: {missing}"
        );

        let _ = std::fs::remove_dir_all(&dir);
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
