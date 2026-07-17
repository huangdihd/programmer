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
//! Run with `programmer --mcp-server`. Tool calls are gated through the same
//! classifier as the TUI, chosen with `--mcp-mode`:
//! - `yolo` runs everything; `plan` refuses state-mutating tools.
//! - `auto` sends dangerous calls to the LLM classifier (needs a configured
//!   model) and runs them only if it approves.
//! - `manual` asks the human through MCP **elicitation** — the server prompts
//!   the client, which surfaces a confirmation to its user. Clients without
//!   elicitation support fall back to a refusal.
//!
//! Nothing but protocol messages is written to stdout.

use async_openai::Client;
use async_openai::config::OpenAIConfig;
use async_openai::types::responses::Tool;
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader, Lines};

use crate::classifier::{Verdict, WorkMode};

/// The MCP protocol version advertised when the client doesn't request one.
const DEFAULT_PROTOCOL_VERSION: &str = "2024-11-05";

/// A running MCP server: the gating mode, an optional classifier client (for
/// `auto`), whether the connected client supports elicitation (for `manual`),
/// and a counter for server-initiated request ids.
pub struct McpServer {
    mode: WorkMode,
    classifier: Option<(Client<OpenAIConfig>, String)>,
    client_elicitation: bool,
    next_request_id: u64,
}

/// What a static (no-IO) gate decision resolves to.
enum Gate {
    /// Run the tool.
    Allow,
    /// Refuse with this reason.
    Deny(String),
    /// Ask the LLM classifier (auto mode).
    Llm,
    /// Ask the human via elicitation (manual mode, client supports it).
    Elicit,
}

impl McpServer {
    pub fn new(mode: WorkMode, classifier: Option<(Client<OpenAIConfig>, String)>) -> Self {
        McpServer {
            mode,
            classifier,
            client_elicitation: false,
            next_request_id: 1,
        }
    }

    /// Serve over the process's stdin/stdout until EOF.
    pub async fn run(mut self) -> std::io::Result<()> {
        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        let mut stdout = tokio::io::stdout();
        self.serve(&mut lines, &mut stdout).await
    }

    async fn serve<R, W>(&mut self, lines: &mut Lines<R>, stdout: &mut W) -> std::io::Result<()>
    where
        R: AsyncBufRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        while let Some(line) = lines.next_line().await? {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let msg: Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(_) => {
                    write_line(stdout, error_envelope(&Value::Null, -32700, "parse error")).await?;
                    continue;
                }
            };
            // No id → notification (e.g. notifications/initialized): no reply.
            let Some(id) = msg.get("id").cloned() else {
                continue;
            };
            let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
            let params = msg.get("params").cloned();

            let outcome = match method {
                "initialize" => Ok(self.on_initialize(params.as_ref())),
                "tools/list" => Ok(tools_list_result()),
                "tools/call" => self.on_tools_call(params, lines, stdout).await,
                "ping" => Ok(json!({})),
                other => Err((-32601i64, format!("method not found: {other}"))),
            };

            let response = match outcome {
                Ok(result) => {
                    serde_json::to_string(&json!({"jsonrpc": "2.0", "id": id, "result": result}))
                        .unwrap_or_else(|_| error_envelope(&id, -32603, "internal error"))
                }
                Err((code, message)) => error_envelope(&id, code, &message),
            };
            write_line(stdout, response).await?;
        }
        Ok(())
    }

    fn on_initialize(&mut self, params: Option<&Value>) -> Value {
        // Remember whether the client can handle elicitation (needed for
        // manual-mode human confirmation).
        self.client_elicitation = params
            .and_then(|p| p.get("capabilities"))
            .and_then(|c| c.get("elicitation"))
            .is_some();
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

    async fn on_tools_call<R, W>(
        &mut self,
        params: Option<Value>,
        lines: &mut Lines<R>,
        stdout: &mut W,
    ) -> Result<Value, (i64, String)>
    where
        R: AsyncBufRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        let params = params.ok_or((-32602i64, "missing params".to_string()))?;
        let name = params
            .get("name")
            .and_then(|n| n.as_str())
            .ok_or((-32602i64, "missing tool name".to_string()))?;
        let arguments = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
        let args_str = arguments.to_string();

        // UI-bound and passthrough tools aren't exposed over MCP.
        if name == crate::tools::ask_user::NAME || name.starts_with("mcp__") {
            return Ok(tool_content(
                format!("error: tool '{name}' is not available over MCP"),
                true,
            ));
        }

        // Gate the call. Static decisions resolve immediately; auto/manual need
        // an async round-trip (LLM call / elicitation).
        let denial = match self.gate(name, &args_str) {
            Gate::Allow => None,
            Gate::Deny(reason) => Some(reason),
            Gate::Llm => self.llm_approve(name, &args_str).await.err(),
            Gate::Elicit => self.elicit_approve(name, &args_str, lines, stdout).await.err(),
        };
        if let Some(reason) = denial {
            return Ok(tool_content(format!("error: {reason}"), true));
        }

        let (text, is_error) = match crate::tools::run_local_tool(name, &args_str).await {
            Ok(text) => (text, false),
            Err(text) => (text, true),
        };
        Ok(tool_content(text, is_error))
    }

    /// The static part of the decision: read-only tools always run; dangerous
    /// tools resolve by mode (with auto/manual deferring to an async gate).
    fn gate(&self, name: &str, args: &str) -> Gate {
        if !crate::classifier::needs_review(name, args) {
            return Gate::Allow;
        }
        match self.mode {
            WorkMode::Yolo => Gate::Allow,
            WorkMode::Plan => Gate::Deny(format!(
                "plan mode refuses state-mutating tools like {name}; use --mcp-mode auto or yolo"
            )),
            WorkMode::Auto => Gate::Llm,
            WorkMode::Manual => {
                if self.client_elicitation {
                    Gate::Elicit
                } else {
                    Gate::Deny(format!(
                        "manual mode needs a client that supports MCP elicitation to confirm \
                         {name}; use --mcp-mode auto or yolo instead"
                    ))
                }
            }
        }
    }

    /// Auto mode: let the LLM classifier decide. Empty context — the server has
    /// no conversation, so the call is judged on its own merits.
    async fn llm_approve(&self, name: &str, args: &str) -> Result<(), String> {
        let Some((client, model)) = &self.classifier else {
            return Err(
                "auto mode has no classifier model configured (set classifier_model or a \
                 default model); refusing"
                    .to_string(),
            );
        };
        let outcome =
            crate::classifier::classify_tool_call(client, model, name, args, "", "", true).await;
        match outcome.verdict {
            Verdict::Allow => Ok(()),
            Verdict::Deny { reason } | Verdict::Ask { reason } => {
                Err(format!("classifier denied: {reason}"))
            }
        }
    }

    /// Manual mode: ask the human through the client via `elicitation/create`.
    async fn elicit_approve<R, W>(
        &mut self,
        name: &str,
        args: &str,
        lines: &mut Lines<R>,
        stdout: &mut W,
    ) -> Result<(), String>
    where
        R: AsyncBufRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        let id = self.next_request_id;
        self.next_request_id += 1;
        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "elicitation/create",
            "params": {
                "message": format!("Approve tool `{name}`?\n{args}"),
                "requestedSchema": { "type": "object", "properties": {}, "required": [] },
            },
        });
        write_line(stdout, request.to_string())
            .await
            .map_err(|e| format!("elicitation request failed: {e}"))?;

        // The client is blocked awaiting the tool result, so the next line is
        // its elicitation response.
        let line = match lines.next_line().await {
            Ok(Some(l)) => l,
            Ok(None) => return Err("no elicitation response (client closed)".to_string()),
            Err(e) => return Err(format!("elicitation read failed: {e}")),
        };
        let value: Value = serde_json::from_str(line.trim())
            .map_err(|_| "malformed elicitation response".to_string())?;
        let action = value
            .get("result")
            .and_then(|r| r.get("action"))
            .and_then(|a| a.as_str())
            .unwrap_or("decline");
        if action == "accept" {
            Ok(())
        } else {
            Err(format!("user {action}ed the request"))
        }
    }
}

async fn write_line<W: AsyncWrite + Unpin>(stdout: &mut W, line: String) -> std::io::Result<()> {
    stdout.write_all(line.as_bytes()).await?;
    stdout.write_all(b"\n").await?;
    stdout.flush().await
}

fn error_envelope(id: &Value, code: i64, message: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
    .to_string()
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

fn tool_content(text: String, is_error: bool) -> Value {
    json!({
        "content": [ { "type": "text", "text": text } ],
        "isError": is_error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(mode: WorkMode) -> McpServer {
        McpServer::new(mode, None)
    }

    #[test]
    fn gate_read_only_always_allows() {
        assert!(matches!(server(WorkMode::Auto).gate("read_file", "{}"), Gate::Allow));
        assert!(matches!(server(WorkMode::Manual).gate("grep", "{}"), Gate::Allow));
    }

    #[test]
    fn gate_yolo_allows_dangerous() {
        assert!(matches!(server(WorkMode::Yolo).gate("command", "{}"), Gate::Allow));
    }

    #[test]
    fn gate_plan_denies_mutating() {
        assert!(matches!(server(WorkMode::Plan).gate("write_file", "{}"), Gate::Deny(_)));
    }

    #[test]
    fn gate_auto_defers_to_llm() {
        assert!(matches!(server(WorkMode::Auto).gate("command", "{}"), Gate::Llm));
    }

    #[test]
    fn gate_manual_elicits_when_supported_else_denies() {
        // No elicitation capability → deny.
        assert!(matches!(server(WorkMode::Manual).gate("command", "{}"), Gate::Deny(_)));
        // With capability → elicit.
        let mut s = server(WorkMode::Manual);
        s.client_elicitation = true;
        assert!(matches!(s.gate("command", "{}"), Gate::Elicit));
    }

    #[tokio::test]
    async fn auto_without_classifier_refuses_dangerous() {
        let denial = server(WorkMode::Auto)
            .llm_approve("command", "{}")
            .await
            .expect_err("no classifier configured");
        assert!(denial.contains("no classifier"), "got: {denial}");
    }

    #[test]
    fn initialize_reads_elicitation_capability_and_echoes_version() {
        let mut s = server(WorkMode::Manual);
        let params = json!({
            "protocolVersion": "2025-06-18",
            "capabilities": { "elicitation": {} }
        });
        let result = s.on_initialize(Some(&params));
        assert!(s.client_elicitation);
        assert_eq!(result["protocolVersion"], "2025-06-18");
        assert_eq!(result["serverInfo"]["name"], "programmer");
    }

    #[test]
    fn tools_list_excludes_ask_user() {
        let v = tools_list_result();
        let names: Vec<&str> = v["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"command"));
        assert!(!names.contains(&"ask_user"));
    }

    #[tokio::test]
    async fn manual_elicitation_accept_allows() {
        // Simulate a client that accepts the elicitation.
        let input = "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"action\":\"accept\"}}\n";
        let mut lines = BufReader::new(input.as_bytes()).lines();
        let mut out: Vec<u8> = Vec::new();
        let mut s = server(WorkMode::Manual);
        s.client_elicitation = true;
        let result = s
            .elicit_approve("command", "{}", &mut lines, &mut out)
            .await;
        assert!(result.is_ok(), "accept should approve: {result:?}");
        // The server sent an elicitation/create request.
        let sent = String::from_utf8(out).unwrap();
        assert!(sent.contains("elicitation/create"), "sent: {sent}");
    }

    #[tokio::test]
    async fn manual_elicitation_decline_denies() {
        let input = "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"action\":\"decline\"}}\n";
        let mut lines = BufReader::new(input.as_bytes()).lines();
        let mut out: Vec<u8> = Vec::new();
        let mut s = server(WorkMode::Manual);
        s.client_elicitation = true;
        let result = s
            .elicit_approve("command", "{}", &mut lines, &mut out)
            .await;
        assert!(result.is_err(), "decline should deny");
    }
}
