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

use super::types::{JsonRpcRequest, JsonRpcResponse};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

/// A connected MCP server child process, ready for JSON-RPC calls.
pub(crate) struct McpClient {
    #[allow(dead_code)]
    child: Child,
    stdin: Mutex<ChildStdin>,
    stdout_lines: Mutex<BufReader<ChildStdout>>,
    next_id: AtomicU64,
}

impl McpClient {
    /// Spawn the server process given its configuration. Returns an error
    /// string if the process cannot be started.
    pub(crate) fn spawn(
        command: &str,
        args: &[String],
        env: &std::collections::HashMap<String, String>,
    ) -> Result<Self, String> {
        let mut cmd = Command::new(command);
        cmd.args(args);
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        // Discard the server's stderr. MCP servers log diagnostics there (e.g.
        // CodeGraph's "auto-synced" / "file watcher active" notices); inheriting
        // our stderr paints them straight onto the TUI's alternate screen and
        // corrupts the display.
        cmd.stderr(std::process::Stdio::null());
        cmd.kill_on_drop(true);

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

        Ok(McpClient {
            child,
            stdin: Mutex::new(stdin),
            stdout_lines: Mutex::new(BufReader::new(stdout)),
            next_id: AtomicU64::new(1),
        })
    }

    /// Send a JSON-RPC request and wait for the matching response.
    pub(crate) async fn call(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);

        let req = JsonRpcRequest {
            jsonrpc: "2.0",
            id,
            method: method.to_string(),
            params,
        };

        let line = serde_json::to_string(&req)
            .map_err(|e| format!("MCP serialise: {e}"))?;

        {
            let mut stdin = self.stdin.lock().await;
            stdin
                .write_all(line.as_bytes())
                .await
                .map_err(|e| format!("MCP write: {e}"))?;
            stdin
                .write_all(b"\n")
                .await
                .map_err(|e| format!("MCP write newline: {e}"))?;
            stdin
                .flush()
                .await
                .map_err(|e| format!("MCP flush: {e}"))?;
        }

        // Read lines until we get a JSON response with the matching id.
        // We loop because the server may send notifications interleaved.
        loop {
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

            let resp: JsonRpcResponse = serde_json::from_str(line.trim())
                .map_err(|e| format!("MCP parse response: {e}"))?;

            match resp {
                JsonRpcResponse::Success {
                    id: resp_id,
                    result,
                    ..
                } => {
                    if resp_id == id {
                        return Ok(result);
                    }
                    // Unexpected id — keep reading (stale response?).
                }
                JsonRpcResponse::Error {
                    id: resp_id, error, ..
                } => {
                    if resp_id == id {
                        return Err(format!("MCP error: {}", error.message));
                    }
                }
                JsonRpcResponse::Notification { .. } => {
                    // Ignore notifications while waiting for our response.
                }
            }
        }
    }
}
