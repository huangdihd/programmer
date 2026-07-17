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

//! HTTP MCP server: expose programmer's local tools over plain HTTP JSON-RPC,
//! served alongside a ratatui approval console (see [`super::console`]).
//!
//! Because the transport is HTTP, the process keeps its terminal — so the
//! operator watches tool calls, approves `manual`-mode ones, and switches the
//! work mode live (Ctrl+T), all in the console. Tool calls are gated through
//! the same classifier as the TUI.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use async_openai::Client;
use async_openai::config::OpenAIConfig;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use super::server::{initialize_result, tool_content, tools_list_result};
use crate::classifier::{Verdict, WorkMode};

/// A pending `manual`-mode approval, handed to the console; it replies with the
/// operator's decision over `respond`.
pub(crate) struct ApprovalRequest {
    pub(crate) tool: String,
    pub(crate) args: String,
    pub(crate) respond: oneshot::Sender<bool>,
}

/// How a log line is styled in the console.
#[derive(Clone, Copy)]
pub(crate) enum LogKind {
    Info,
    Allowed,
    Denied,
}

pub(crate) struct LogEntry {
    pub(crate) kind: LogKind,
    pub(crate) text: String,
}

/// Shared state between the HTTP handler and the console.
pub(crate) struct ServerState {
    /// Current work mode; the console mutates it on Ctrl+T.
    pub(crate) mode: Arc<Mutex<WorkMode>>,
    /// Classifier client for `auto` mode (None → auto refuses dangerous tools).
    classifier: Option<(Client<OpenAIConfig>, String)>,
    log_tx: mpsc::UnboundedSender<LogEntry>,
    approval_tx: mpsc::UnboundedSender<ApprovalRequest>,
}

/// Serve MCP over HTTP at `addr` with a ratatui approval console. Returns when
/// the operator quits the console.
pub async fn serve(
    mode: WorkMode,
    classifier: Option<(Client<OpenAIConfig>, String)>,
    addr: SocketAddr,
    allow_yolo: bool,
) -> color_eyre::Result<()> {
    let mode = Arc::new(Mutex::new(mode));
    let (log_tx, log_rx) = mpsc::unbounded_channel();
    let (approval_tx, approval_rx) = mpsc::unbounded_channel();
    let state = Arc::new(ServerState {
        mode: Arc::clone(&mode),
        classifier,
        log_tx,
        approval_tx,
    });

    let app = Router::new()
        .route("/", post(mcp_handler))
        .route("/mcp", post(mcp_handler))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    // The console owns the terminal and blocks until the operator quits.
    let result = super::console::run(mode, log_rx, approval_rx, bound, allow_yolo).await;
    server.abort();
    result
}

async fn mcp_handler(State(state): State<Arc<ServerState>>, Json(msg): Json<Value>) -> Response {
    // Notifications (no id) are acknowledged with 202 and no body.
    let Some(id) = msg.get("id").cloned() else {
        return StatusCode::ACCEPTED.into_response();
    };
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = msg.get("params").cloned();

    let outcome = match method {
        "initialize" => Ok(initialize_result(params.as_ref())),
        "tools/list" => Ok(tools_list_result()),
        "tools/call" => state.tools_call(params).await,
        "ping" => Ok(json!({})),
        other => Err((-32601i64, format!("method not found: {other}"))),
    };

    let body = match outcome {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => {
            json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
        }
    };
    Json(body).into_response()
}

impl ServerState {
    async fn tools_call(&self, params: Option<Value>) -> Result<Value, (i64, String)> {
        let params = params.ok_or((-32602i64, "missing params".to_string()))?;
        let name = params
            .get("name")
            .and_then(|n| n.as_str())
            .ok_or((-32602i64, "missing tool name".to_string()))?
            .to_string();
        let args = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}))
            .to_string();

        if name == crate::tools::ask_user::NAME || name.starts_with("mcp__") {
            return Ok(tool_content(
                format!("error: tool '{name}' is not available over MCP"),
                true,
            ));
        }

        self.log(LogKind::Info, format!("→ {name}"));
        match self.decide(&name, &args).await {
            Ok(()) => {
                let (text, is_error) = match crate::tools::run_local_tool(&name, &args).await {
                    Ok(text) => (text, false),
                    Err(text) => (text, true),
                };
                self.log(LogKind::Allowed, format!("ran {name}"));
                Ok(tool_content(text, is_error))
            }
            Err(reason) => {
                self.log(LogKind::Denied, format!("{name}: {reason}"));
                Ok(tool_content(format!("error: {reason}"), true))
            }
        }
    }

    /// Gate a tool call by the current mode. Read-only tools always run.
    async fn decide(&self, name: &str, args: &str) -> Result<(), String> {
        if !crate::classifier::needs_review(name, args) {
            return Ok(());
        }
        let mode = *self.mode.lock().unwrap();
        match mode {
            WorkMode::Yolo => Ok(()),
            WorkMode::Plan => Err(format!(
                "plan mode refuses state-mutating tools like {name}; switch mode with Ctrl+T"
            )),
            WorkMode::Auto => self.llm_approve(name, args).await,
            WorkMode::Manual => self.ask_operator(name, args).await,
        }
    }

    async fn llm_approve(&self, name: &str, args: &str) -> Result<(), String> {
        let Some((client, model)) = &self.classifier else {
            return Err("auto mode has no classifier model configured; refusing".to_string());
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

    /// Manual mode: hand the call to the console and wait for the operator.
    async fn ask_operator(&self, name: &str, args: &str) -> Result<(), String> {
        let (respond, rx) = oneshot::channel();
        let req = ApprovalRequest {
            tool: name.to_string(),
            args: args.to_string(),
            respond,
        };
        if self.approval_tx.send(req).is_err() {
            return Err("approval console is unavailable".to_string());
        }
        match rx.await {
            Ok(true) => Ok(()),
            Ok(false) => Err("denied by operator".to_string()),
            Err(_) => Err("approval cancelled".to_string()),
        }
    }

    fn log(&self, kind: LogKind, text: String) {
        let _ = self.log_tx.send(LogEntry { kind, text });
    }
}
