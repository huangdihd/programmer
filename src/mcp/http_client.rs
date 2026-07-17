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

//! JSON-RPC 2.0 client over MCP Streamable HTTP for remote servers.
//!
//! Each request is POSTed to the server URL; the response arrives either as
//! a plain JSON body or as an SSE stream that is read until the matching
//! response message appears. The `Mcp-Session-Id` header returned by
//! `initialize` is echoed on every subsequent request, and the negotiated
//! protocol version is sent via `MCP-Protocol-Version`.
//!
//! Not implemented (not needed for tool calling): the standalone GET
//! listening stream, stream resumability, server→client requests, and
//! explicit DELETE session teardown. Notifications that arrive on a POST's
//! SSE stream (progress, list_changed) are handled.

use super::client::ProgressInfo;
use super::types::{JsonRpcRequest, JsonRpcResponse};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex as StdMutex;

/// Transport-level log lines shown in the MCP panel where a child process's
/// stderr would otherwise appear.
const LOG_BUFFER_LINES: usize = 200;

/// A remote MCP server reachable over Streamable HTTP.
pub(crate) struct McpHttpClient {
    url: String,
    /// Extra request headers from the config (e.g. `Authorization`).
    headers: Vec<(String, String)>,
    client: reqwest::Client,
    next_id: AtomicU64,
    /// Session id issued by the server on `initialize`, echoed thereafter.
    session_id: StdMutex<Option<String>>,
    /// Protocol version negotiated at `initialize`, echoed thereafter.
    protocol_version: StdMutex<Option<String>>,
    // --- notification flags (set from SSE-delivered notifications) ---
    tools_list_changed: AtomicBool,
    resources_list_changed: AtomicBool,
    resources_updated: AtomicBool,
    prompts_list_changed: AtomicBool,
    // --- progress ---
    progress: StdMutex<HashMap<String, ProgressInfo>>,
    // --- transport log (the panel's "stderr" pane) ---
    log: StdMutex<VecDeque<String>>,
}

impl McpHttpClient {
    pub(crate) fn new(
        url: &str,
        headers: &HashMap<String, String>,
    ) -> Result<Self, String> {
        let parsed = reqwest::Url::parse(url).map_err(|e| format!("invalid URL '{url}': {e}"))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(format!("unsupported URL scheme '{}'", parsed.scheme()));
        }
        let client = reqwest::Client::builder()
            // Per-call deadlines come from `call_with_timeout`; no global one.
            .user_agent(concat!("programmer/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| format!("cannot build HTTP client: {e}"))?;
        Ok(McpHttpClient {
            url: url.to_string(),
            headers: headers.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            client,
            next_id: AtomicU64::new(1),
            session_id: StdMutex::new(None),
            protocol_version: StdMutex::new(None),
            tools_list_changed: AtomicBool::new(false),
            resources_list_changed: AtomicBool::new(false),
            resources_updated: AtomicBool::new(false),
            prompts_list_changed: AtomicBool::new(false),
            progress: StdMutex::new(HashMap::new()),
            log: StdMutex::new(VecDeque::new()),
        })
    }

    fn log_line(&self, line: String) {
        let mut guard = self.log.lock().unwrap();
        if guard.len() >= LOG_BUFFER_LINES {
            guard.pop_front();
        }
        guard.push_back(line);
    }

    /// Build a POST with the shared MCP headers attached.
    fn post(&self, body: String) -> reqwest::RequestBuilder {
        let mut req = self
            .client
            .post(&self.url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::ACCEPT, "application/json, text/event-stream")
            .body(body);
        if let Some(sid) = self.session_id.lock().unwrap().as_deref() {
            req = req.header("Mcp-Session-Id", sid);
        }
        if let Some(ver) = self.protocol_version.lock().unwrap().as_deref() {
            req = req.header("MCP-Protocol-Version", ver);
        }
        for (k, v) in &self.headers {
            req = req.header(k.as_str(), v.as_str());
        }
        req
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
        let body = serde_json::to_string(&req).map_err(|e| format!("MCP serialise: {e}"))?;

        let resp = self.post(body).send().await.map_err(|e| {
            let msg = format!("MCP HTTP request failed: {}", error_chain(&e));
            self.log_line(msg.clone());
            msg
        })?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let snippet: String = body.chars().take(200).collect();
            let msg = format!("MCP HTTP {status}: {snippet}");
            self.log_line(msg.clone());
            return Err(msg);
        }

        // `initialize` binds the session: remember the server-issued id.
        if let Some(sid) = resp
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            *self.session_id.lock().unwrap() = Some(sid.to_string());
        }

        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();

        let result = if content_type.contains("text/event-stream") {
            self.read_sse_response(resp, id).await
        } else {
            let body = resp
                .text()
                .await
                .map_err(|e| format!("MCP read body: {e}"))?;
            self.parse_response(&body, id)
        };

        // Remember the negotiated protocol version for subsequent requests.
        if method == "initialize"
            && let Ok(v) = &result
                && let Some(ver) = v.get("protocolVersion").and_then(|v| v.as_str()) {
                    *self.protocol_version.lock().unwrap() = Some(ver.to_string());
                }
        if let Err(e) = &result {
            self.log_line(e.clone());
        }
        result
    }

    /// Parse a plain JSON body as the response to request `id`.
    fn parse_response(&self, body: &str, id: u64) -> Result<serde_json::Value, String> {
        let resp: JsonRpcResponse =
            serde_json::from_str(body).map_err(|e| format!("MCP parse response: {e}"))?;
        match resp {
            JsonRpcResponse::Success { id: rid, result, .. } if rid == id => Ok(result),
            JsonRpcResponse::Error { id: rid, error, .. } if rid == id => {
                Err(format!("MCP error: {}", error.message))
            }
            _ => Err("MCP response id mismatch".to_string()),
        }
    }

    /// Read an SSE stream until the response to request `id` appears.
    /// Notifications encountered on the way are handled; anything else is
    /// ignored.
    async fn read_sse_response(
        &self,
        mut resp: reqwest::Response,
        id: u64,
    ) -> Result<serde_json::Value, String> {
        let mut parser = SseParser::default();
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| format!("MCP read SSE: {e}"))?
        {
            for data in parser.push(&String::from_utf8_lossy(&chunk)) {
                if let Some(result) = self.dispatch_sse_message(&data, id)? {
                    return Ok(result);
                }
            }
        }
        // Stream ended: flush any unterminated final event.
        if let Some(data) = parser.finish()
            && let Some(result) = self.dispatch_sse_message(&data, id)? {
                return Ok(result);
            }
        Err("MCP SSE stream ended without a response".to_string())
    }

    /// Handle one SSE `data:` payload. Returns `Ok(Some(result))` when it is
    /// the response to `id`.
    fn dispatch_sse_message(
        &self,
        data: &str,
        id: u64,
    ) -> Result<Option<serde_json::Value>, String> {
        let raw: serde_json::Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => return Ok(None), // non-JSON keep-alive — ignore.
        };
        // Notification (`method`, no `id`).
        if raw.get("method").is_some() && raw.get("id").is_none() {
            self.handle_notification(&raw);
            return Ok(None);
        }
        // Response to our request?
        if let Ok(resp) = serde_json::from_value::<JsonRpcResponse>(raw) {
            match resp {
                JsonRpcResponse::Success { id: rid, result, .. } if rid == id => {
                    return Ok(Some(result));
                }
                JsonRpcResponse::Error { id: rid, error, .. } if rid == id => {
                    return Err(format!("MCP error: {}", error.message));
                }
                _ => {}
            }
        }
        // Server→client request or stale message — not supported over HTTP;
        // ignore.
        Ok(None)
    }

    /// Handle a JSON-RPC notification delivered on an SSE stream.
    fn handle_notification(&self, raw: &serde_json::Value) {
        let Some(method) = raw.get("method").and_then(|v| v.as_str()) else {
            return;
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
                        message: p
                            .get("message")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string()),
                    };
                    self.progress.lock().unwrap().insert(token, info);
                }
            }
            _ => {}
        }
    }

    /// Send a JSON-RPC notification (no id, no response expected).
    pub(crate) async fn send_notification(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<(), String> {
        let mut body = serde_json::json!({ "jsonrpc": "2.0", "method": method });
        if let Some(p) = params {
            body["params"] = p;
        }
        let body = serde_json::to_string(&body).map_err(|e| format!("MCP serialise: {e}"))?;
        let resp = self
            .post(body)
            .send()
            .await
            .map_err(|e| format!("MCP HTTP notification failed: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("MCP HTTP {} on notification", resp.status()));
        }
        Ok(())
    }

    // --- notification flag accessors (parity with the stdio client) ---

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

    // --- progress ---

    pub(crate) fn clear_progress(&self) {
        self.progress.lock().unwrap().clear();
    }

    pub(crate) fn progress_snapshot(&self) -> HashMap<String, ProgressInfo> {
        self.progress.lock().unwrap().clone()
    }

    // --- transport log (the panel's "stderr" pane) ---

    pub(crate) fn stderr_snapshot(&self) -> Vec<String> {
        self.log.lock().unwrap().iter().cloned().collect()
    }
}

/// Render an error with its full source chain — reqwest's top-level Display
/// ("error sending request") hides the actual cause.
fn error_chain(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut cur = e.source();
    while let Some(c) = cur {
        out.push_str(": ");
        out.push_str(&c.to_string());
        cur = c.source();
    }
    out
}

// ---------------------------------------------------------------------------
// SSE parsing
// ---------------------------------------------------------------------------

/// Minimal incremental SSE parser: feeds on text chunks, emits the joined
/// `data:` payload of each complete event. `event:`/`id:`/`retry:` fields and
/// comments are ignored — MCP only uses `data`.
#[derive(Default)]
struct SseParser {
    /// Unconsumed text (possibly a partial line) from previous chunks.
    buf: String,
    /// `data:` lines of the event currently being assembled.
    data: Vec<String>,
}

impl SseParser {
    /// Feed a chunk; returns the data payloads of any events completed by it.
    fn push(&mut self, chunk: &str) -> Vec<String> {
        self.buf.push_str(chunk);
        let mut out = Vec::new();
        while let Some(nl) = self.buf.find('\n') {
            let line = self.buf[..nl].trim_end_matches('\r').to_string();
            self.buf.drain(..=nl);
            if line.is_empty() {
                // Blank line terminates the event.
                if !self.data.is_empty() {
                    out.push(self.data.join("\n"));
                    self.data.clear();
                }
            } else if let Some(rest) = line.strip_prefix("data:") {
                self.data.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
            }
            // Other fields and `:` comments are ignored.
        }
        out
    }

    /// Flush an event left unterminated when the stream ends.
    fn finish(&mut self) -> Option<String> {
        // A partial last line may still be a data line.
        let line = std::mem::take(&mut self.buf);
        let line = line.trim_end_matches('\r');
        if let Some(rest) = line.strip_prefix("data:") {
            self.data
                .push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
        }
        if self.data.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.data).join("\n"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_parser_handles_split_chunks_and_crlf() {
        let mut p = SseParser::default();
        assert!(p.push("data: {\"a\"").is_empty(), "partial event");
        let events = p.push(":1}\r\n\r\ndata: second\n\n");
        assert_eq!(events, vec![r#"{"a":1}"#.to_string(), "second".to_string()]);
    }

    #[test]
    fn sse_parser_joins_multiline_data_and_skips_other_fields() {
        let mut p = SseParser::default();
        let events = p.push("event: message\nid: 3\ndata: line1\ndata: line2\n\n: comment\n\n");
        assert_eq!(events, vec!["line1\nline2".to_string()]);
    }

    #[test]
    fn sse_parser_flushes_unterminated_event_on_finish() {
        let mut p = SseParser::default();
        assert!(p.push("data: tail").is_empty());
        assert_eq!(p.finish(), Some("tail".to_string()));
        assert_eq!(p.finish(), None);
    }
}
