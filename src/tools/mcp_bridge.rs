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

//! Bridges MCP servers into the model's tool list.
//!
//! Discovery side: every MCP tool becomes an OpenAI function tool named
//! `mcp__<server>__<tool>`, and servers with resources/prompts get synthetic
//! `resources_list` / `resources_read` / `prompts_list` / `prompts_get`
//! tools. Execution side: [`run_mcp_call`] routes a call with an `mcp__`
//! prefix to the right server and operation.

use async_openai::types::responses::{FunctionToolCall, Tool};

use crate::mcp::McpManager;

// ---------------------------------------------------------------------------
// Discovery: MCP → tool definitions
// ---------------------------------------------------------------------------

/// Append every MCP-derived tool definition to `tools`.
pub(crate) fn extend_with_mcp_tools(tools: &mut Vec<Tool>, mgr: &McpManager) {
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
            tools.push(mcp_function_tool(
                &list_fqn,
                Some(format!("List all resources exposed by MCP server '{server_name}'.")),
                serde_json::json!({ "type": "object", "properties": {} }),
            ));
        }
        let read_fqn = format!("mcp__{}__resources_read", server_name);
        if !tools.iter().any(|t| tool_name(t) == Some(&read_fqn[..])) {
            let desc = format!(
                "Read a resource from MCP server '{server_name}'. Call \
                 resources_list first to see available URIs, then pass the \
                 desired URI."
            );
            tools.push(mcp_function_tool(
                &read_fqn,
                Some(desc),
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
            ));
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

// ---------------------------------------------------------------------------
// Execution: dispatch an mcp__ call
// ---------------------------------------------------------------------------

/// Run a tool call whose name starts with `mcp__`, routing resource and
/// prompt operations separately from regular tool calls.
pub(crate) async fn run_mcp_call(
    call: &FunctionToolCall,
    mcp: Option<&McpManager>,
) -> Result<String, String> {
    let Some(mgr) = mcp else {
        return Err("error: MCP not available (no servers connected)".to_string());
    };
    if let Some(server) = suffixed_server(&call.name, "__resources_list") {
        resources_list(mgr, server)
    } else if let Some(server) = suffixed_server(&call.name, "__resources_read") {
        resources_read(mgr, server, &call.arguments).await
    } else if let Some(server) = suffixed_server(&call.name, "__prompts_list") {
        prompts_list(mgr, server)
    } else if let Some(server) = suffixed_server(&call.name, "__prompts_get") {
        prompts_get(mgr, server, &call.arguments).await
    } else {
        call_tool(mgr, &call.name, &call.arguments).await
    }
}

/// `mcp__<server><suffix>` → `<server>`.
fn suffixed_server<'a>(name: &'a str, suffix: &str) -> Option<&'a str> {
    name.strip_prefix("mcp__")?.strip_suffix(suffix)
}

fn resources_list(mgr: &McpManager, server: &str) -> Result<String, String> {
    let resources = mgr.all_resources();
    let server_resources: Vec<_> = resources
        .iter()
        .filter(|(_, s, _)| s == server)
        .collect();
    if server_resources.is_empty() {
        return Ok(format!("No resources available from MCP server '{server}'."));
    }
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

async fn resources_read(
    mgr: &McpManager,
    server: &str,
    arguments: &str,
) -> Result<String, String> {
    #[derive(serde::Deserialize)]
    struct ReadArgs {
        uri: String,
    }
    let args: ReadArgs = serde_json::from_str(arguments)
        .map_err(|e| format!("error: invalid arguments: {e}"))?;
    match mgr.read_resource(server, &args.uri).await {
        Ok(result) => Ok(result
            .contents
            .iter()
            .map(|c| match c {
                crate::mcp::types::ResourceContent::Text { text, .. } => text.clone(),
                _ => "[non-text resource content]".to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n")),
        Err(e) => Err(format!("error: MCP resource read failed: {e}")),
    }
}

fn prompts_list(mgr: &McpManager, server: &str) -> Result<String, String> {
    let prompts = mgr.all_prompts();
    let server_prompts: Vec<_> = prompts
        .iter()
        .filter(|(_, s, _)| s == server)
        .collect();
    if server_prompts.is_empty() {
        return Ok(format!("No prompts available from MCP server '{server}'."));
    }
    let lines: Vec<String> = server_prompts
        .iter()
        .map(|(_, _, p)| {
            let mut s = format!(
                "name: {}{}",
                p.name,
                p.description
                    .as_ref()
                    .map(|d| format!("\n  description: {d}"))
                    .unwrap_or_default()
            );
            if let Some(args) = &p.arguments {
                s.push_str("\n  arguments:");
                for a in args {
                    let req = if a.required == Some(true) { " (required)" } else { "" };
                    s.push_str(&format!(
                        "\n    - {}:{}{}",
                        a.name,
                        a.description.as_ref().map(|d| format!(" {d}")).unwrap_or_default(),
                        req
                    ));
                }
            }
            s
        })
        .collect();
    Ok(lines.join("\n\n"))
}

async fn prompts_get(
    mgr: &McpManager,
    server: &str,
    arguments: &str,
) -> Result<String, String> {
    #[derive(serde::Deserialize)]
    struct PromptGetArgs {
        name: String,
        #[serde(default)]
        arguments: Option<serde_json::Value>,
    }
    let args: PromptGetArgs = serde_json::from_str(arguments)
        .map_err(|e| format!("error: invalid arguments: {e}"))?;
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
}

async fn call_tool(
    mgr: &McpManager,
    fqn: &str,
    arguments: &str,
) -> Result<String, String> {
    match mgr
        .call_tool(
            fqn,
            serde_json::from_str(arguments).unwrap_or(serde_json::Value::Null),
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

    #[test]
    fn suffixed_server_parses_operation_names() {
        assert_eq!(suffixed_server("mcp__fs__resources_list", "__resources_list"), Some("fs"));
        assert_eq!(suffixed_server("mcp__fs__read", "__resources_list"), None);
        assert_eq!(suffixed_server("not_mcp", "__resources_list"), None);
    }
}
