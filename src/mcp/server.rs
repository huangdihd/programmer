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

//! MCP server: expose programmer's own local tools to any MCP client over the
//! stdio transport (newline-delimited JSON-RPC 2.0).
//!
//! Run with `programmer --mcp-server`. The client spawns that process and can
//! then `tools/list` and `tools/call` programmer's file/command/grep/fetch/
//! diagnostics/task tools. Nothing but protocol messages is written to stdout.

use async_openai::types::responses::Tool;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// The MCP protocol version advertised when the client doesn't request one.
const DEFAULT_PROTOCOL_VERSION: &str = "2024-11-05";

/// Read JSON-RPC messages from stdin and write responses to stdout until EOF.
pub async fn run_stdio_server() -> std::io::Result<()> {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        if let Some(response) = handle_message(&line).await {
            stdout.write_all(response.as_bytes()).await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
        }
    }
    Ok(())
}

/// Handle one JSON-RPC message. Returns the serialized response line, or `None`
/// for notifications (messages without an `id`) and blank lines.
pub(crate) async fn handle_message(line: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let msg: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return Some(error_envelope(&Value::Null, -32700, "parse error")),
    };

    // No id → notification (e.g. notifications/initialized): never answered.
    let id = msg.get("id").cloned()?;
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = msg.get("params").cloned();

    let outcome = match method {
        "initialize" => Ok(initialize_result(params.as_ref())),
        "tools/list" => Ok(tools_list_result()),
        "tools/call" => tools_call_result(params).await,
        "ping" => Ok(json!({})),
        other => Err((-32601i64, format!("method not found: {other}"))),
    };

    Some(match outcome {
        Ok(result) => {
            serde_json::to_string(&json!({"jsonrpc": "2.0", "id": id, "result": result}))
                .unwrap_or_else(|_| error_envelope(&id, -32603, "internal error"))
        }
        Err((code, message)) => error_envelope(&id, code, &message),
    })
}

fn error_envelope(id: &Value, code: i64, message: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
    .to_string()
}

fn initialize_result(params: Option<&Value>) -> Value {
    // Echo the client's requested protocol version when present.
    let version = params
        .and_then(|p| p.get("protocolVersion"))
        .and_then(|v| v.as_str())
        .unwrap_or(DEFAULT_PROTOCOL_VERSION);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": {
            "name": "programmer",
            "version": env!("CARGO_PKG_VERSION"),
        },
    })
}

fn tools_list_result() -> Value {
    let tools: Vec<Value> = crate::tools::mcp_server_tools()
        .iter()
        .filter_map(tool_to_spec)
        .collect();
    json!({ "tools": tools })
}

/// Convert a function tool definition into an MCP tool spec.
fn tool_to_spec(tool: &Tool) -> Option<Value> {
    let Tool::Function(f) = tool else {
        return None;
    };
    Some(json!({
        "name": f.name,
        "description": f.description.clone().unwrap_or_default(),
        "inputSchema": f
            .parameters
            .clone()
            .unwrap_or_else(|| json!({ "type": "object" })),
    }))
}

async fn tools_call_result(params: Option<Value>) -> Result<Value, (i64, String)> {
    let params = params.ok_or((-32602i64, "missing params".to_string()))?;
    let name = params
        .get("name")
        .and_then(|n| n.as_str())
        .ok_or((-32602i64, "missing tool name".to_string()))?;
    // Arguments are already an object; tools take a JSON string.
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let args_str = arguments.to_string();

    // UI-bound and passthrough tools aren't exposed over MCP.
    if name == crate::tools::ask_user::NAME || name.starts_with("mcp__") {
        return Ok(tool_content(
            format!("error: tool '{name}' is not available over MCP"),
            true,
        ));
    }

    let (text, is_error) = match crate::tools::run_local_tool(name, &args_str).await {
        Ok(text) => (text, false),
        Err(text) => (text, true),
    };
    Ok(tool_content(text, is_error))
}

fn tool_content(text: String, is_error: bool) -> Value {
    json!({
        "content": [ { "type": "text", "text": text } ],
        "isError": is_error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn initialize_echoes_protocol_and_advertises_tools() {
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#;
        let resp = handle_message(req).await.expect("response");
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["id"], 1);
        assert_eq!(v["result"]["protocolVersion"], "2025-06-18");
        assert!(v["result"]["capabilities"]["tools"].is_object());
        assert_eq!(v["result"]["serverInfo"]["name"], "programmer");
    }

    #[tokio::test]
    async fn tools_list_includes_local_tools_but_not_ask_user() {
        let resp = handle_message(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#)
            .await
            .expect("response");
        let v: Value = serde_json::from_str(&resp).unwrap();
        let names: Vec<&str> = v["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"command"));
        assert!(!names.contains(&"ask_user"));
        // Every tool carries an inputSchema object.
        for t in v["result"]["tools"].as_array().unwrap() {
            assert!(t["inputSchema"].is_object(), "tool: {t}");
        }
    }

    #[tokio::test]
    async fn notifications_get_no_response() {
        assert!(
            handle_message(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn tools_call_runs_a_local_tool() {
        let req = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"command","arguments":{"command":"echo mcp-server-hi"}}}"#;
        let resp = handle_message(req).await.expect("response");
        let v: Value = serde_json::from_str(&resp).unwrap();
        let text = v["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("mcp-server-hi"), "got: {text}");
        assert_eq!(v["result"]["isError"], false);
    }

    #[tokio::test]
    async fn unknown_method_is_a_jsonrpc_error() {
        let resp = handle_message(r#"{"jsonrpc":"2.0","id":9,"method":"bogus"}"#)
            .await
            .expect("response");
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["error"]["code"], -32601);
    }
}
