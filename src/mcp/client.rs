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

//! JSON-RPC 2.0 client over stdio for MCP servers.
//!
//! Spawns a child process, sends newline-delimited JSON-RPC requests on
//! stdin, and reads responses from stdout. Requests are serialised by a
//! monotonic id counter; responses are matched back to pending calls.
//!
//! Also handles server→client requests (e.g. `roots/list`) inline in the
//! read loop.

use super::types::{JsonRpcRequest, JsonRpcResponse};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

const STDERR_BUFFER_LINES: usize = 200;
type StderrBuffer = Arc<StdMutex<VecDeque<String>>>;

/// Progress info tracked per `progressToken` from `notifications/progress`.
#[derive(Debug, Clone, Default)]
pub(crate) struct ProgressInfo {
    pub progress: f64,
    pub total: Option<f64>,
    pub message: Option<String>,
}

/// A connected MCP server child process, ready for JSON-RPC calls.
pub(crate) struct McpClient {
    #[allow(dead_code)]
    child: Child,
    stdin: Mutex<ChildStdin>,
    stdout_lines: Mutex<BufReader<ChildStdout>>,
    next_id: AtomicU64,
    // --- notification flags ---
    tools_list_changed: AtomicBool,
    resources_list_changed: AtomicBool,
    resources_updated: AtomicBool,
    prompts_list_changed: AtomicBool,
    // --- cancellation ---
    /// Currently-cancelled request id, set by `notifications/cancelled`.
    cancelled_id: StdMutex<Option<u64>>,
    // --- progress ---
    /// Progress info keyed by `progressToken` (stringified).
    progress: StdMutex<HashMap<String, ProgressInfo>>,
    // --- roots ---
    /// Workspace root path reported via `roots/list`.
    workspace_root: String,
    // --- stderr ---
    stderr_buf: StderrBuffer,
    _stderr_task: JoinHandle<()>,
}

impl McpClient {
    /// Spawn the server process.
    pub(crate) fn spawn(
        command: &str,
        args: &[String],
        env: &std::collections::HashMap<String, String>,
        workspace_root: &str,
    ) -> Result<Self, String> {
        let mut cmd = Command::new(command);
        cmd.args(args);
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.kill_on_drop(true);

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("cannot spawn MCP server '{command}': {e}"))?;

        let stdin = child.stdin.take().ok_or_else(|| "child has no stdin".to_string())?;
        let stdout = child.stdout.take().ok_or_else(|| "child has no stdout".to_string())?;
        let stderr = child.stderr.take().ok_or_else(|| "child has no stderr".to_string())?;

        let stderr_buf: StderrBuffer = Arc::new(StdMutex::new(VecDeque::new()));
        let buf = stderr_buf.clone();
        let stderr_task = tokio::spawn(async move {
            let mut reader = BufReader::new(stderr).lines();
            loop {
                match reader.next_line().await {
                    Ok(Some(line)) => {
                        let mut guard = buf.lock().unwrap();
                        if guard.len() >= STDERR_BUFFER_LINES {
                            guard.pop_front();
                        }
                        guard.push_back(line);
                    }
                    _ => break,
                }
            }
        });

        Ok(McpClient {
            child,
            stdin: Mutex::new(stdin),
            stdout_lines: Mutex::new(BufReader::new(stdout)),
            next_id: AtomicU64::new(1),
            tools_list_changed: AtomicBool::new(false),
            resources_list_changed: AtomicBool::new(false),
            resources_updated: AtomicBool::new(false),
            prompts_list_changed: AtomicBool::new(false),
            cancelled_id: StdMutex::new(None),
            progress: StdMutex::new(HashMap::new()),
            workspace_root: workspace_root.to_string(),
            stderr_buf,
            _stderr_task: stderr_task,
        })
    }

    /// Send a JSON-RPC request and wait for the matching response.
    pub(crate) async fn call(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.send_request(id, method, params).await?;
        self.read_response(id).await
    }

    /// Write a request to stdin.
    async fn send_request(
        &self,
        id: u64,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<(), String> {
        let req = JsonRpcRequest {
            jsonrpc: "2.0",
            id,
            method: method.to_string(),
            params,
        };
        let line = serde_json::to_string(&req).map_err(|e| format!("MCP serialise: {e}"))?;
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(line.as_bytes()).await.map_err(|e| format!("MCP write: {e}"))?;
        stdin.write_all(b"\n").await.map_err(|e| format!("MCP write newline: {e}"))?;
        stdin.flush().await.map_err(|e| format!("MCP flush: {e}"))?;
        Ok(())
    }

    /// Send a JSON-RPC response (for server→client requests).
    async fn send_response(&self, id: u64, result: serde_json::Value) -> Result<(), String> {
        let resp = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        });
        let line = serde_json::to_string(&resp).map_err(|e| format!("MCP serialise: {e}"))?;
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(line.as_bytes()).await.map_err(|e| format!("MCP write: {e}"))?;
        stdin.write_all(b"\n").await.map_err(|e| format!("MCP write newline: {e}"))?;
        stdin.flush().await.map_err(|e| format!("MCP flush: {e}"))?;
        Ok(())
    }

    /// Read lines until we get a matching response to `id`.
    /// Handles server→client requests, notifications, and cancellation inline.
    async fn read_response(&self, id: u64) -> Result<serde_json::Value, String> {
        loop {
            // Check for cancellation between each line.
            {
                let mut c = self.cancelled_id.lock().unwrap();
                if *c == Some(id) {
                    *c = None;
                    return Err("MCP call cancelled by server".to_string());
                }
            }

            let mut line = String::new();
            {
                let mut reader = self.stdout_lines.lock().await;
                reader.read_line(&mut line).await.map_err(|e| format!("MCP read: {e}"))?;
            }

            if line.trim().is_empty() {
                continue;
            }

            let raw: serde_json::Value = serde_json::from_str(line.trim())
                .map_err(|e| format!("MCP parse JSON: {e}"))?;

            // --- server→client request? (has `method` + `id`, no `result`/`error`) ---
            if raw.get("method").and_then(|v| v.as_str()).is_some()
                && raw.get("id").and_then(|v| v.as_u64()).is_some()
                && raw.get("result").is_none()
                && raw.get("error").is_none()
            {
                let req_id = raw["id"].as_u64().unwrap();
                let method = raw["method"].as_str().unwrap();
                let params = raw.get("params").cloned();
                self.handle_server_request(req_id, method, params).await;
                continue;
            }

            // --- notification? (`method` without `id`) ---
            if raw.get("method").is_some() && raw.get("id").is_none() {
                self.handle_notification(&raw);
                continue;
            }

            // --- response to our request ---
            let resp: JsonRpcResponse = serde_json::from_value(raw)
                .map_err(|e| format!("MCP parse response: {e}"))?;

            match resp {
                JsonRpcResponse::Success { id: rid, result, .. } if rid == id => return Ok(result),
                JsonRpcResponse::Error { id: rid, error, .. } if rid == id => {
                    return Err(format!("MCP error: {}", error.message));
                }
                _ => {} // stale response — keep reading.
            }
        }
    }

    /// Handle a server→client request (e.g. `roots/list`).
    async fn handle_server_request(
        &self,
        req_id: u64,
        method: &str,
        _params: Option<serde_json::Value>,
    ) {
        match method {
            "roots/list" => {
                let roots = serde_json::json!([{
                    "uri": format!("file://{}", self.workspace_root.replace('\\', "/")),
                    "name": "project",
                }]);
                let _ = self.send_response(req_id, serde_json::json!({"roots": roots})).await;
            }
            _ => {
                // Unknown request — respond with method-not-found error.
                let err = serde_json::json!({
                    "error": {
                        "code": -32601,
                        "message": format!("Method not found: {method}"),
                    }
                });
                let _ = self
                    .send_response(req_id, err)
                    .await;
            }
        }
    }

    /// Handle a JSON-RPC notification from the server.
    fn handle_notification(&self, raw: &serde_json::Value) {
        let method = match raw.get("method").and_then(|v| v.as_str()) {
            Some(m) => m,
            None => return,
        };
        let params = raw.get("params");

        match method {
            "notifications/tools/list_changed" => {
                self.tools_list_changed.store(true, Ordering::Relaxed);
            }
            "notifications/resources/list_changed" => {
                self.resources_list_changed.store(true, Ordering::Relaxed);
            }
            "notifications/resources/updated" => {
                self.resources_updated.store(true, Ordering::Relaxed);
            }
            "notifications/prompts/list_changed" => {
                self.prompts_list_changed.store(true, Ordering::Relaxed);
            }
            "notifications/cancelled" => {
                if let Some(p) = params {
                    if let Some(rid) = p.get("requestId").and_then(|v| v.as_u64()) {
                        *self.cancelled_id.lock().unwrap() = Some(rid);
                    }
                }
            }
            "notifications/progress" => {
                if let Some(p) = params {
                    let token = match p.get("progressToken") {
                        Some(serde_json::Value::Number(n)) => n.to_string(),
                        Some(serde_json::Value::String(s)) => s.clone(),
                        _ => return,
                    };
                    let info = ProgressInfo {
                        progress: p.get("progress").and_then(|v| v.as_f64()).unwrap_or(0.0),
                        total: p.get("total").and_then(|v| v.as_f64()),
                        message: p.get("message").and_then(|v| v.as_str()).map(|s| s.to_string()),
                    };
                    self.progress.lock().unwrap().insert(token, info);
                }
            }
            _ => {}
        }
    }

    // --- notification flag accessors ---

    pub(crate) fn take_tools_list_changed(&self) -> bool {
        self.tools_list_changed.swap(false, Ordering::Relaxed)
    }
    pub(crate) fn take_resources_list_changed(&self) -> bool {
        self.resources_list_changed.swap(false, Ordering::Relaxed)
    }
    pub(crate) fn take_resources_updated(&self) -> bool {
        self.resources_updated.swap(false, Ordering::Relaxed)
    }
    pub(crate) fn take_prompts_list_changed(&self) -> bool {
        self.prompts_list_changed.swap(false, Ordering::Relaxed)
    }

    // --- cancellation ---

    pub(crate) fn take_cancelled(&self) -> Option<u64> {
        self.cancelled_id.lock().unwrap().take()
    }

    // --- progress ---

    /// Snapshot and clear progress for a given token.
    pub(crate) fn take_progress(&self, token: &str) -> Option<ProgressInfo> {
        self.progress.lock().unwrap().remove(token)
    }

    /// All currently tracked progress tokens and their info (non-destructive).
    pub(crate) fn progress_snapshot(&self) -> HashMap<String, ProgressInfo> {
        self.progress.lock().unwrap().clone()
    }

    // --- stderr ---

    pub(crate) fn take_stderr_lines(&self) -> Vec<String> {
        let mut guard = self.stderr_buf.lock().unwrap();
        guard.drain(..).collect()
    }

    // --- notification sender ---

    pub(crate) async fn send_notification(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<(), String> {
        let req = JsonRpcRequest {
            jsonrpc: "2.0",
            id: 0,
            method: method.to_string(),
            params,
        };
        let line = serde_json::to_string(&req).map_err(|e| format!("MCP serialise: {e}"))?;
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(line.as_bytes()).await.map_err(|e| format!("MCP write: {e}"))?;
        stdin.write_all(b"\n").await.map_err(|e| format!("MCP write newline: {e}"))?;
        stdin.flush().await.map_err(|e| format!("MCP flush: {e}"))?;
        Ok(())
    }

    // --- timeout wrapper ---

    pub(crate) async fn call_with_timeout(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
        timeout_secs: u64,
    ) -> Result<serde_json::Value, String> {
        tokio::time::timeout(Duration::from_secs(timeout_secs), self.call(method, params))
            .await
            .map_err(|_| format!("MCP call to '{method}' timed out after {timeout_secs}s"))?
    }
}
