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
        // Expose MCP resource operations as synthetic tools.
        // Names like `mcp__<server>__resources_list` / `__resources_read`
        // are unlikely to collide with real MCP tools.
        for (_fqn, server_name, _resource) in mgr.all_resources() {
            let list_fqn = format!("mcp__{}__resources_list", server_name);
            if !tools.iter().any(|t| tool_name(t) == Some(&list_fqn[..])) {
                tools.push(resources_list_tool(&list_fqn, &server_name));
            }
            let read_fqn = format!("mcp__{}__resources_read", server_name);
            if !tools.iter().any(|t| tool_name(t) == Some(&read_fqn[..])) {
                let desc = format!("Read a resource from MCP server '{}'. Call resources_list first to see available URIs, then pass the desired URI.", server_name);
                tools.push(resources_read_tool(&read_fqn, &desc));
            }
        }
        // Expose MCP prompt operations as synthetic tools.
        for (_fqn, server_name, _prompt) in mgr.all_prompts() {
            let list_fqn = format!("mcp__{}__prompts_list", server_name);
            if !tools.iter().any(|t| tool_name(t) == Some(&list_fqn[..])) {
                let desc = format!(
                    "List all prompt templates from MCP server '{server_name}'."
                );
                tools.push(mcp_function_tool(&list_fqn, Some(desc), serde_json::json!({ "type": "object", "properties": {} })));
            }
            let get_fqn = format!("mcp__{}__prompts_get", server_name);
            if !tools.iter().any(|t| tool_name(t) == Some(&get_fqn[..])) {
                let desc = format!(
                    "Get a prompt template from MCP server '{server_name}'. \
                     Call prompts_list first to see available prompts, then \
                     pass the prompt name and any optional arguments."
                );
                tools.push(mcp_function_tool(&get_fqn, Some(desc), serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "The prompt name (as returned by prompts_list)."
                        },
                        "arguments": {
                            "type": "object",
                            "description": "Optional prompt arguments (key-value pairs)."
                        }
                    },
                    "required": ["name"]
                })));
            }
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

/// Extract the tool name from a [`Tool`] enum. Returns `None` for
/// non-Function variants (which we don't produce).
fn tool_name(tool: &Tool) -> Option<&str> {
    match tool {
        Tool::Function(f) => Some(&f.name),
        _ => None,
    }
}

/// Synthetic tool: list all resources for an MCP server.
fn resources_list_tool(fqn: &str, server: &str) -> Tool {
    mcp_function_tool(
        fqn,
        Some(format!("List all resources exposed by MCP server '{server}'.")),
        serde_json::json!({ "type": "object", "properties": {} }),
    )
}

/// Synthetic tool: read a specific MCP resource by URI.
fn resources_read_tool(fqn: &str, desc: &str) -> Tool {
    mcp_function_tool(
        fqn,
        Some(desc.to_string()),
        serde_json::json!({
            "type": "object",
            "properties": {
                "uri": {
                    "type": "string",
                    "description": "The URI of the resource to read (as returned by resources_list)."
                }
            },
            "required": ["uri"]
        }),
    )
}

/// Maximum characters of tool output kept before truncation. The rest is
/// discarded and a truncation notice is appended so the model knows the output
/// was cut short.
const MAX_OUTPUT_LENGTH: usize = 8000;

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
pub(crate) async fn run_tool_call(
    call: &FunctionToolCall,
    sender: &tokio::sync::mpsc::UnboundedSender<crate::ui::event::Event>,
    mcp: Option<&crate::mcp::McpManager>,
) -> ToolOutput {
    // Every branch yields a `Result<String, String>`: `Ok` is a successful
    // result, `Err` is a failure. This is the single source of truth for the
    // `failed` flag below.
    let result: Result<String, String> = if call.name.starts_with("mcp__") {
        if let Some(mgr) = mcp {
            // Resource operations: dispatch separately from regular tool calls.
            if call.name.ends_with("__resources_list") {
                let server = call
                    .name
                    .strip_prefix("mcp__")
                    .and_then(|s| s.strip_suffix("__resources_list"))
                    .unwrap_or("");
                let resources = mgr.all_resources();
                let server_resources: Vec<_> = resources
                    .iter()
                    .filter(|(_, s, _)| s == server)
                    .collect();
                if server_resources.is_empty() {
                    Ok(format!(
                        "No resources available from MCP server '{server}'."
                    ))
                } else {
                    let lines: Vec<String> = server_resources
                        .iter()
                        .map(|(_, _, r)| {
                            format!(
                                "uri: {}\n  name: {}{}",
                                r.uri,
                                r.name,
                                r.description
                                    .as_ref()
                                    .map(|d| format!("\n  description: {d}"))
                                    .unwrap_or_default(),
                            )
                        })
                        .collect();
                    Ok(lines.join("\n\n"))
                }
            } else if call.name.ends_with("__resources_read") {
                let server = call
                    .name
                    .strip_prefix("mcp__")
                    .and_then(|s| s.strip_suffix("__resources_read"))
                    .unwrap_or("");
                #[derive(serde::Deserialize)]
                struct ReadArgs {
                    uri: String,
                }
                let args: ReadArgs = match serde_json::from_str(&call.arguments) {
                    Ok(a) => a,
                    Err(e) => {
                        return ToolOutput {
                            param: FunctionCallOutputItemParam {
                                call_id: call.call_id.clone(),
                                output: FunctionCallOutput::Text(format!(
                                    "error: invalid arguments: {e}"
                                )),
                                id: None,
                                status: None,
                            },
                            failed: true,
                            approval_label: None,
                        }
                    }
                };
                match mgr.read_resource(server, &args.uri).await {
                    Ok(result) => Ok(result
                        .contents
                        .iter()
                        .map(|c| match c {
                            crate::mcp::types::ResourceContent::Text {
                                text, ..
                            } => text.clone(),
                            _ => "[non-text resource content]".to_string(),
                        })
                        .collect::<Vec<_>>()
                        .join("\n")),
                    Err(e) => Err(format!("error: MCP resource read failed: {e}")),
                }
            } else if call.name.ends_with("__prompts_list") {
                let server = call
                    .name
                    .strip_prefix("mcp__")
                    .and_then(|s| s.strip_suffix("__prompts_list"))
                    .unwrap_or("");
                let prompts = mgr.all_prompts();
                let server_prompts: Vec<_> = prompts
                    .iter()
                    .filter(|(_, s, _)| s == server)
                    .collect();
                if server_prompts.is_empty() {
                    Ok(format!(
                        "No prompts available from MCP server '{server}'."
                    ))
                } else {
                    let lines: Vec<String> = server_prompts
                        .iter()
                        .map(|(_, _, p)| {
                            let mut s = format!("name: {}{}",
                                p.name,
                                p.description
                                    .as_ref()
                                    .map(|d| format!("\n  description: {d}"))
                                    .unwrap_or_default());
                            if let Some(args) = &p.arguments {
                                s.push_str("\n  arguments:");
                                for a in args {
                                    let req = if a.required == Some(true) { " (required)" } else { "" };
                                    s.push_str(&format!("\n    - {}:{}{}",
                                        a.name,
                                        a.description.as_ref().map(|d| format!(" {d}")).unwrap_or_default(),
                                        req));
                                }
                            }
                            s
                        })
                        .collect();
                    Ok(lines.join("\n\n"))
                }
            } else if call.name.ends_with("__prompts_get") {
                let server = call
                    .name
                    .strip_prefix("mcp__")
                    .and_then(|s| s.strip_suffix("__prompts_get"))
                    .unwrap_or("");
                #[derive(serde::Deserialize)]
                struct PromptGetArgs {
                    name: String,
                    #[serde(default)]
                    arguments: Option<serde_json::Value>,
                }
                let args: PromptGetArgs = match serde_json::from_str(&call.arguments) {
                    Ok(a) => a,
                    Err(e) => {
                        return ToolOutput {
                            param: FunctionCallOutputItemParam {
                                call_id: call.call_id.clone(),
                                output: FunctionCallOutput::Text(format!(
                                    "error: invalid arguments: {e}"
                                )),
                                id: None,
                                status: None,
                            },
                            failed: true,
                            approval_label: None,
                        }
                    }
                };
                match mgr.get_prompt(server, &args.name, args.arguments).await {
                    Ok(result) => {
                        let mut lines = Vec::new();
                        if let Some(desc) = &result.description {
                            lines.push(format!("description: {desc}"));
                        }
                        for msg in &result.messages {
                            let role = &msg.role;
                            let text = match &msg.content {
                                crate::mcp::types::PromptContent::Text { text } => text.clone(),
                                _ => "[non-text prompt content]".to_string(),
                            };
                            lines.push(format!("[{role}]\n{text}"));
                        }
                        Ok(lines.join("\n\n"))
                    }
                    Err(e) => Err(format!("error: MCP prompt get failed: {e}")),
                }
            } else {
                match mgr
                    .call_tool(
                        &call.name,
                        serde_json::from_str(&call.arguments)
                            .unwrap_or(serde_json::Value::Null),
                    )
                    .await
                {
                    Ok(result) => Ok(result
                        .content
                        .iter()
                        .map(|c| match c {
                            crate::mcp::types::ToolContent::Text { text } => text.clone(),
                            _ => "[non-text MCP content]".to_string(),
                        })
                        .collect::<Vec<_>>()
                        .join("\n")),
                    Err(e) => Err(format!("error: MCP tool call failed: {e}")),
                }
            }
        } else {
            Err("error: MCP not available (no servers connected)".to_string())
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
            other => Err(format!("error: unknown tool '{other}'")),
        }
    };

    let (text, failed) = match result {
        Ok(text) => (text, false),
        Err(text) => (text, true),
    };
    let text = truncate_output(text);

    ToolOutput {
        param: FunctionCallOutputItemParam {
            call_id: call.call_id.clone(),
            output: FunctionCallOutput::Text(text),
            id: None,
            status: None,
        },
        failed,
        approval_label: None,
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
