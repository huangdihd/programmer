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
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

const STDERR_BUFFER_LINES: usize = 200;
type StderrBuffer = Arc<StdMutex<VecDeque<String>>>;

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
        // Resolve `.cmd`/`.bat`/`.ps1` shims (npm global installs, PS Gallery
        // scripts) on Windows so a bare name like `codegraph` is found; the
        // `.ps1` case comes back as a `powershell.exe -File …` invocation
        // whose prefix args must precede the server's own. A no-op elsewhere.
        let (program, mut argv) = crate::tools::resolve_program(command);
        argv.extend(args.iter().cloned());
        let sandbox = crate::security::active()
            .map(|security| {
                security.sandbox_program_invocation(&program, &argv, Some(workspace_root))
            })
            .transpose()?
            .flatten();
        let mut cmd = if let Some(invocation) = sandbox {
            let mut command = Command::new(&invocation.program);
            crate::security::sandbox::configure_tokio_command(&mut command, invocation);
            command
        } else {
            let mut command = Command::new(&program);
            command.args(&argv);
            command
        };
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.kill_on_drop(true);

        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("cannot spawn MCP server '{command}': {e}"))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "child has no stdin".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "child has no stdout".to_string())?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| "child has no stderr".to_string())?;

        let stderr_buf: StderrBuffer = Arc::new(StdMutex::new(VecDeque::new()));
        let buf = stderr_buf.clone();
        let stderr_task = tokio::spawn(async move {
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                let mut guard = buf.lock().unwrap();
                if guard.len() >= STDERR_BUFFER_LINES {
                    guard.pop_front();
                }
                guard.push_back(line);
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
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| format!("MCP write: {e}"))?;
        stdin
            .write_all(b"\n")
            .await
            .map_err(|e| format!("MCP write newline: {e}"))?;
        stdin.flush().await.map_err(|e| format!("MCP flush: {e}"))?;
        Ok(())
    }

    /// Send a JSON-RPC response (for server→client requests).
    async fn send_response(
        &self,
        id: serde_json::Value,
        result: serde_json::Value,
    ) -> Result<(), String> {
        let resp = response_message(id, result);
        let line = serde_json::to_string(&resp).map_err(|e| format!("MCP serialise: {e}"))?;
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| format!("MCP write: {e}"))?;
        stdin
            .write_all(b"\n")
            .await
            .map_err(|e| format!("MCP write newline: {e}"))?;
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
                reader
                    .read_line(&mut line)
                    .await
                    .map_err(|e| format!("MCP read: {e}"))?;
            }

            if line.trim().is_empty() {
                continue;
            }

            let raw: serde_json::Value =
                serde_json::from_str(line.trim()).map_err(|e| format!("MCP parse JSON: {e}"))?;

            // --- server→client request? (has `method` + `id`, no `result`/`error`) ---
            // JSON-RPC ids may be strings or numbers. CodeGraph uses ids such
            // as `cg-srv-1` for roots/list when the current folder is not
            // indexed, so preserve the raw id and echo it in our response.
            if let Some((req_id, method, params)) = server_request(&raw) {
                self.handle_server_request(req_id, method, params).await;
                continue;
            }

            // --- notification? (`method` without `id`) ---
            if raw.get("method").is_some() && raw.get("id").is_none() {
                self.handle_notification(&raw);
                continue;
            }

            // --- response to our request ---
            let resp: JsonRpcResponse =
                serde_json::from_value(raw).map_err(|e| format!("MCP parse response: {e}"))?;

            match resp {
                JsonRpcResponse::Success {
                    id: rid, result, ..
                } if rid == id => return Ok(result),
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
        req_id: serde_json::Value,
        method: &str,
        _params: Option<serde_json::Value>,
    ) {
        match method {
            "roots/list" => {
                let roots = serde_json::json!([{
                    "uri": format!("file://{}", self.workspace_root.replace('\\', "/")),
                    "name": "project",
                }]);
                let _ = self
                    .send_response(req_id, serde_json::json!({"roots": roots}))
                    .await;
            }
            _ => {
                // Unknown request — respond with method-not-found error.
                let err = serde_json::json!({
                    "error": {
                        "code": -32601,
                        "message": format!("Method not found: {method}"),
                    }
                });
                let _ = self.send_response(req_id, err).await;
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
                if let Some(p) = params
                    && let Some(rid) = p.get("requestId").and_then(|v| v.as_u64())
                {
                    *self.cancelled_id.lock().unwrap() = Some(rid);
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

    // --- stderr ---

    /// The most recent stderr lines from the server (non-destructive), for
    /// display in the MCP panel.
    pub(crate) fn stderr_snapshot(&self) -> Vec<String> {
        self.stderr_buf.lock().unwrap().iter().cloned().collect()
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
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| format!("MCP write: {e}"))?;
        stdin
            .write_all(b"\n")
            .await
            .map_err(|e| format!("MCP write newline: {e}"))?;
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

/// Extract a server-initiated JSON-RPC request while preserving its id type.
fn server_request(
    raw: &serde_json::Value,
) -> Option<(serde_json::Value, &str, Option<serde_json::Value>)> {
    if raw.get("result").is_some() || raw.get("error").is_some() {
        return None;
    }
    let method = raw.get("method")?.as_str()?;
    let id = raw.get("id")?;
    if id.is_null() {
        return None;
    }
    Some((id.clone(), method, raw.get("params").cloned()))
}

fn response_message(id: serde_json::Value, result: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
}

#[cfg(test)]
mod tests {
    use super::{response_message, server_request};

    #[test]
    fn recognizes_codegraph_string_id_server_request() {
        let raw = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "cg-srv-1",
            "method": "roots/list"
        });

        let (id, method, params) = server_request(&raw).expect("server request");
        assert_eq!(id, "cg-srv-1");
        assert_eq!(method, "roots/list");
        assert!(params.is_none());

        let response = response_message(id, serde_json::json!({"roots": []}));
        assert_eq!(response["id"], "cg-srv-1");
    }

    #[test]
    fn does_not_misclassify_json_rpc_responses_as_requests() {
        let raw = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": {"tools": []}
        });

        assert!(server_request(&raw).is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stdio_round_trip_handles_string_id_roots_request() {
        let script = r#"
            IFS= read -r initialize
            printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"fake","version":"1"}}}'
            IFS= read -r initialized
            IFS= read -r tools
            printf '%s\n' '{"jsonrpc":"2.0","id":"cg-srv-1","method":"roots/list"}'
            IFS= read -r roots
            case "$roots" in
                *'"id":"cg-srv-1"'*)
                    printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[]}}'
                    ;;
                *)
                    printf '%s\n' '{"jsonrpc":"2.0","id":2,"error":{"code":-32000,"message":"string id was not echoed"}}'
                    ;;
            esac
        "#;
        let client = super::McpClient::spawn(
            "sh",
            &["-c".to_string(), script.to_string()],
            &std::collections::HashMap::new(),
            "/tmp/project",
        )
        .unwrap();

        client
            .call(
                "initialize",
                Some(serde_json::json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {"roots": {"listChanged": true}},
                    "clientInfo": {"name": "test", "version": "1"}
                })),
            )
            .await
            .unwrap();
        client
            .send_notification("notifications/initialized", None)
            .await
            .unwrap();

        let tools = client.call("tools/list", None).await.unwrap();
        assert_eq!(tools["tools"], serde_json::json!([]));
    }
}
