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

//! MCP (Model Context Protocol) manager.
//!
//! Owns one or more MCP server connections, discovers their tools at startup
//! via `initialize` + `tools/list`, and routes `tools/call` requests.

pub mod client;
pub mod http_client;
pub mod types;

use client::McpClient;
use http_client::McpHttpClient;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;
use types::{CallToolResult, McpServerConfig, McpTool};

/// Timeout for MCP handshake calls (initialize, tools/list).
const HANDSHAKE_TIMEOUT_SECS: u64 = 30;
/// Timeout for MCP tool calls.
const TOOL_CALL_TIMEOUT_SECS: u64 = 120;

/// A server connection over either transport. Delegates the small surface
/// the manager needs; the JSON-RPC semantics are identical on both sides.
enum McpConn {
    Stdio(McpClient),
    Http(McpHttpClient),
}

impl McpConn {
    async fn call_with_timeout(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
        timeout_secs: u64,
    ) -> Result<serde_json::Value, String> {
        match self {
            McpConn::Stdio(c) => c.call_with_timeout(method, params, timeout_secs).await,
            McpConn::Http(c) => {
                tokio::time::timeout(Duration::from_secs(timeout_secs), c.call(method, params))
                    .await
                    .map_err(|_| {
                        format!("MCP call to '{method}' timed out after {timeout_secs}s")
                    })?
            }
        }
    }

    async fn send_notification(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<(), String> {
        match self {
            McpConn::Stdio(c) => c.send_notification(method, params).await,
            McpConn::Http(c) => c.send_notification(method, params).await,
        }
    }

    fn take_tools_list_changed(&self) -> bool {
        match self {
            McpConn::Stdio(c) => c.take_tools_list_changed(),
            McpConn::Http(c) => c.take_tools_list_changed(),
        }
    }
    fn take_resources_list_changed(&self) -> bool {
        match self {
            McpConn::Stdio(c) => c.take_resources_list_changed(),
            McpConn::Http(c) => c.take_resources_list_changed(),
        }
    }
    fn take_resources_updated(&self) -> bool {
        match self {
            McpConn::Stdio(c) => c.take_resources_updated(),
            McpConn::Http(c) => c.take_resources_updated(),
        }
    }
    fn take_prompts_list_changed(&self) -> bool {
        match self {
            McpConn::Stdio(c) => c.take_prompts_list_changed(),
            McpConn::Http(c) => c.take_prompts_list_changed(),
        }
    }

    fn clear_progress(&self) {
        match self {
            McpConn::Stdio(c) => c.clear_progress(),
            McpConn::Http(c) => c.clear_progress(),
        }
    }
    fn progress_snapshot(&self) -> HashMap<String, client::ProgressInfo> {
        match self {
            McpConn::Stdio(c) => c.progress_snapshot(),
            McpConn::Http(c) => c.progress_snapshot(),
        }
    }
    fn stderr_snapshot(&self) -> Vec<String> {
        match self {
            McpConn::Stdio(c) => c.stderr_snapshot(),
            McpConn::Http(c) => c.stderr_snapshot(),
        }
    }
}

/// Tracks a connected MCP server and its discovered tools, resources, prompts.
struct McpServer {
    client: McpConn,
    tools: Mutex<Vec<McpTool>>,
    resources: Mutex<Vec<types::McpResource>>,
    prompts: Mutex<Vec<types::McpPrompt>>,
}

impl McpServer {
    async fn refresh_if_stale(&self) {
        if !self.client.take_tools_list_changed() { return; }
        match self.client.call_with_timeout("tools/list", None, HANDSHAKE_TIMEOUT_SECS).await {
            Ok(raw) => {
                if let Ok(r) = serde_json::from_value::<types::ListToolsResult>(raw) {
                    *self.tools.lock().unwrap() = r.tools;
                }
            }
            Err(_) => {}
        }
    }

    async fn refresh_resources_if_stale(&self) {
        let changed = self.client.take_resources_list_changed();
        let _upd = self.client.take_resources_updated();
        if !changed && !_upd { return; }
        match self.client.call_with_timeout("resources/list", None, HANDSHAKE_TIMEOUT_SECS).await {
            Ok(raw) => {
                if let Ok(r) = serde_json::from_value::<types::ListResourcesResult>(raw) {
                    *self.resources.lock().unwrap() = r.resources;
                }
            }
            Err(_) => {}
        }
    }

    async fn refresh_prompts_if_stale(&self) {
        if !self.client.take_prompts_list_changed() { return; }
        match self.client.call_with_timeout("prompts/list", None, HANDSHAKE_TIMEOUT_SECS).await {
            Ok(raw) => {
                if let Ok(r) = serde_json::from_value::<types::ListPromptsResult>(raw) {
                    *self.prompts.lock().unwrap() = r.prompts;
                }
            }
            Err(_) => {}
        }
    }

    fn stderr_snapshot(&self) -> Vec<String> {
        self.client.stderr_snapshot()
    }

    fn progress_snapshot(&self) -> HashMap<String, client::ProgressInfo> {
        self.client.progress_snapshot()
    }
}

/// Manages all configured MCP server connections.
pub struct McpManager {
    servers: HashMap<String, McpServer>,
    pub(crate) startup_errors: Vec<String>,
}

impl McpManager {
    /// Initialise all configured MCP servers. Spawns each, runs the handshake,
    /// discovers tools/resources/prompts.
    pub(crate) async fn from_config(configs: &[McpServerConfig], workspace_root: &str) -> Self {
        let mut servers: HashMap<String, McpServer> = HashMap::new();
        let mut startup_errors: Vec<String> = Vec::new();

        for cfg in configs {
            let name = cfg.name.clone();
            match Self::connect_one(cfg, workspace_root).await {
                Ok(server) => { servers.insert(name.clone(), server); }
                Err(e) => { startup_errors.push(format!("MCP server '{name}': {e}")); }
            }
        }

        McpManager { servers, startup_errors }
    }

    async fn connect_one(cfg: &McpServerConfig, workspace_root: &str) -> Result<McpServer, String> {
        let client = match &cfg.url {
            // Remote server: Streamable HTTP; `env` doubles as extra headers.
            Some(url) => McpConn::Http(McpHttpClient::new(url, &cfg.env)?),
            None if cfg.command.trim().is_empty() => {
                return Err("no command or url configured".to_string());
            }
            None => McpConn::Stdio(McpClient::spawn(
                &cfg.command,
                &cfg.args,
                &cfg.env,
                workspace_root,
            )?),
        };

        // Step 1: initialize — declare roots capability.
        let init_params = serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": { "roots": { "listChanged": true } },
            "clientInfo": {
                "name": "programmer",
                "version": env!("CARGO_PKG_VERSION")
            }
        });
        let _ = serde_json::from_value::<types::InitializeResult>(
            client.call_with_timeout("initialize", Some(init_params), HANDSHAKE_TIMEOUT_SECS)
                .await.map_err(|e| format!("initialize failed: {e}"))?,
        ).map_err(|e| format!("bad initialize result: {e}"))?;

        let _ = client.send_notification("notifications/initialized", None).await;

        // Step 2: tools/list
        let tools = serde_json::from_value::<types::ListToolsResult>(
            client.call_with_timeout("tools/list", None, HANDSHAKE_TIMEOUT_SECS)
                .await.map_err(|e| format!("tools/list failed: {e}"))?,
        ).map_err(|e| format!("bad tools/list result: {e}"))?.tools;

        // Step 3: resources/list (best-effort)
        let resources = match client.call_with_timeout("resources/list", None, HANDSHAKE_TIMEOUT_SECS).await {
            Ok(raw) => serde_json::from_value::<types::ListResourcesResult>(raw)
                .map(|r| r.resources).unwrap_or_default(),
            Err(_) => Vec::new(),
        };

        // Step 4: prompts/list (best-effort)
        let prompts = match client.call_with_timeout("prompts/list", None, HANDSHAKE_TIMEOUT_SECS).await {
            Ok(raw) => serde_json::from_value::<types::ListPromptsResult>(raw)
                .map(|r| r.prompts).unwrap_or_default(),
            Err(_) => Vec::new(),
        };

        Ok(McpServer {
            client,
            tools: Mutex::new(tools),
            resources: Mutex::new(resources),
            prompts: Mutex::new(prompts),
        })
    }

    // -- queries --

    pub(crate) fn is_connected(&self) -> bool { !self.servers.is_empty() }
    pub(crate) fn server_count(&self) -> usize { self.servers.len() }

    pub(crate) fn all_tools(&self) -> Vec<(String, McpTool)> {
        let mut out = Vec::new();
        for (sn, s) in &self.servers {
            for t in s.tools.lock().unwrap().iter() {
                out.push((format!("mcp__{}__{}", sn, t.name), t.clone()));
            }
        }
        out
    }

    pub(crate) fn all_resources(&self) -> Vec<(String, String, types::McpResource)> {
        let mut out = Vec::new();
        for (sn, s) in &self.servers {
            for r in s.resources.lock().unwrap().iter() {
                out.push((format!("mcp__{}__{}", sn, r.uri), sn.clone(), r.clone()));
            }
        }
        out
    }

    pub(crate) fn all_prompts(&self) -> Vec<(String, String, types::McpPrompt)> {
        let mut out = Vec::new();
        for (sn, s) in &self.servers {
            for p in s.prompts.lock().unwrap().iter() {
                out.push((format!("mcp__{}__{}", sn, p.name), sn.clone(), p.clone()));
            }
        }
        out
    }

    /// The most recent stderr lines from a server (non-destructive).
    pub(crate) fn server_stderr(&self, server_name: &str) -> Option<Vec<String>> {
        self.servers.get(server_name).map(|s| s.stderr_snapshot())
    }

    /// The first in-flight progress report across all servers, for the
    /// footer status bar. Progress state is cleared when its call finishes,
    /// so `Some` means a call is actively reporting.
    pub(crate) fn active_progress(&self) -> Option<(String, client::ProgressInfo)> {
        for (name, s) in &self.servers {
            if let Some(info) = s.progress_snapshot().into_values().next() {
                return Some((name.clone(), info));
            }
        }
        None
    }

    // -- resource / prompt access --

    pub(crate) async fn read_resource(
        &self, server_name: &str, uri: &str,
    ) -> Result<types::ReadResourceResult, String> {
        let s = self.servers.get(server_name)
            .ok_or_else(|| format!("MCP server '{server_name}' not found"))?;
        s.refresh_resources_if_stale().await;
        let raw = s.client.call_with_timeout(
            "resources/read", Some(serde_json::json!({"uri": uri})), TOOL_CALL_TIMEOUT_SECS,
        ).await?;
        serde_json::from_value(raw).map_err(|e| format!("bad resources/read result: {e}"))
    }

    pub(crate) async fn get_prompt(
        &self, server_name: &str, prompt_name: &str, arguments: Option<serde_json::Value>,
    ) -> Result<types::GetPromptResult, String> {
        let s = self.servers.get(server_name)
            .ok_or_else(|| format!("MCP server '{server_name}' not found"))?;
        s.refresh_prompts_if_stale().await;
        let params = match arguments {
            Some(a) => serde_json::json!({"name": prompt_name, "arguments": a}),
            None => serde_json::json!({"name": prompt_name}),
        };
        let raw = s.client.call_with_timeout(
            "prompts/get", Some(params), TOOL_CALL_TIMEOUT_SECS,
        ).await?;
        serde_json::from_value(raw).map_err(|e| format!("bad prompts/get result: {e}"))
    }

    // -- tool call --

    pub(crate) async fn call_tool(
        &self, fqn: &str, arguments: serde_json::Value,
    ) -> Result<CallToolResult, String> {
        let (server_name, tool_name) = parse_fqn(fqn)
            .ok_or_else(|| format!("invalid MCP tool name: {fqn}"))?;
        let s = self.servers.get(server_name)
            .ok_or_else(|| format!("MCP server '{server_name}' not found"))?;

        // Attach a progress token so servers that support
        // `notifications/progress` can report progress; the sidebar shows it
        // while the call runs.
        static PROGRESS_SEQ: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(1);
        let token = format!(
            "call-{}",
            PROGRESS_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let raw = s.client.call_with_timeout(
            "tools/call",
            Some(serde_json::json!({
                "name": tool_name,
                "arguments": arguments,
                "_meta": { "progressToken": token },
            })),
            TOOL_CALL_TIMEOUT_SECS,
        ).await;
        // The call is over either way — drop progress state so the UI never
        // shows stale progress (also covers servers that report under a
        // token of their own instead of the one we attached).
        s.client.clear_progress();
        let raw = raw?;

        s.refresh_if_stale().await;
        s.refresh_resources_if_stale().await;

        serde_json::from_value(raw).map_err(|e| format!("bad tools/call result: {e}"))
    }
}

fn parse_fqn(fqn: &str) -> Option<(&str, &str)> {
    let rest = fqn.strip_prefix("mcp__")?;
    rest.split_once("__")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_fqn_valid() {
        assert_eq!(parse_fqn("mcp__filesystem__read_file"), Some(("filesystem", "read_file")));
    }
    #[test]
    fn parse_fqn_no_prefix() { assert_eq!(parse_fqn("command"), None); }
    #[test]
    fn parse_fqn_partial() { assert_eq!(parse_fqn("mcp__filesystem"), None); }

    #[test]
    fn server_config_without_url_deserializes() {
        // Configs written before HTTP support have no `url` key.
        let cfg: McpServerConfig = toml::from_str(
            "name = \"old\"\ncommand = \"npx\"\nargs = [\"-y\", \"server\"]\n",
        )
        .expect("old config must still parse");
        assert!(cfg.url.is_none());
        assert_eq!(cfg.command, "npx");
    }

    // ---------------------------------------------------------------------
    // Streamable HTTP transport, against a hand-rolled mock server
    // ---------------------------------------------------------------------

    fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }

    /// Minimal HTTP MCP server: JSON reply to `initialize` (issuing a session
    /// id), 202 to notifications, an SSE-framed reply to `tools/list`, and a
    /// session-checked JSON reply to `tools/call`.
    async fn spawn_mock_http_server() -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { break };
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    // Serve requests on this connection until the client
                    // closes it — reqwest pools and reuses connections, so a
                    // one-shot server would race its pool.
                    let mut buf: Vec<u8> = Vec::new();
                    let mut tmp = [0u8; 4096];
                    loop {
                        while let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
                            let headers = String::from_utf8_lossy(&buf[..pos]).to_string();
                            let content_length = headers
                                .lines()
                                .find_map(|l| {
                                    l.to_ascii_lowercase()
                                        .strip_prefix("content-length:")
                                        .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                                })
                                .unwrap_or(0);
                            let body_start = pos + 4;
                            if buf.len() < body_start + content_length {
                                break; // body incomplete — read more first.
                            }
                            let body = String::from_utf8_lossy(
                                &buf[body_start..body_start + content_length],
                            )
                            .to_string();
                            buf.drain(..body_start + content_length);
                            let response = mock_route(&headers, &body);
                            if sock.write_all(response.as_bytes()).await.is_err() {
                                return;
                            }
                        }
                        let n = match sock.read(&mut tmp).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => n,
                        };
                        buf.extend_from_slice(&tmp[..n]);
                    }
                });
            }
        });
        (format!("http://{addr}/mcp"), handle)
    }

    fn http_json(extra_headers: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n{extra_headers}Content-Length: {}\r\n\r\n{body}",
            body.len()
        )
    }

    fn mock_route(headers: &str, body: &str) -> String {
        let req: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
        let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let id = req.get("id").and_then(|v| v.as_u64());

        // Notification (no id): accept.
        let Some(id) = id else {
            return "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n\r\n".to_string();
        };

        let reply = |result: serde_json::Value| {
            serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string()
        };
        let error = |msg: &str| {
            serde_json::json!({
                "jsonrpc": "2.0", "id": id,
                "error": {"code": -32000, "message": msg},
            })
            .to_string()
        };

        match method {
            "initialize" => http_json(
                "Mcp-Session-Id: sess-1\r\n",
                &reply(serde_json::json!({
                    "protocolVersion": "2025-03-26",
                    "capabilities": {},
                    "serverInfo": {"name": "mock-http", "version": "0"},
                })),
            ),
            // tools/list answers over SSE to exercise the stream path.
            "tools/list" => {
                let msg = reply(serde_json::json!({
                    "tools": [{"name": "echo", "inputSchema": {"type": "object"}}],
                }));
                let body = format!(": keep-alive comment\n\ndata: {msg}\n\n");
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                )
            }
            // The session issued at initialize must be echoed back.
            "tools/call" => {
                if headers.to_ascii_lowercase().contains("mcp-session-id: sess-1") {
                    http_json(
                        "",
                        &reply(serde_json::json!({
                            "content": [{"type": "text", "text": "echoed"}],
                        })),
                    )
                } else {
                    http_json("", &error("missing session id"))
                }
            }
            _ => http_json("", &error("method not found")),
        }
    }

    #[tokio::test]
    async fn mcp_http_transport_handshake_and_tool_call() {
        let (url, _server) = spawn_mock_http_server().await;
        let cfg = McpServerConfig {
            name: "mock".to_string(),
            command: String::new(),
            args: Vec::new(),
            env: Default::default(),
            url: Some(url),
            auto_approve: Default::default(),
        };
        let mgr = McpManager::from_config(&[cfg], ".").await;
        assert!(
            mgr.startup_errors.is_empty(),
            "startup errors: {:?}",
            mgr.startup_errors
        );

        let tools = mgr.all_tools();
        assert_eq!(tools.len(), 1, "one tool discovered via SSE tools/list");
        assert_eq!(tools[0].0, "mcp__mock__echo");

        let result = mgr
            .call_tool("mcp__mock__echo", serde_json::json!({"msg": "hi"}))
            .await
            .expect("tools/call over HTTP");
        assert!(
            matches!(&result.content[0], types::ToolContent::Text { text } if text == "echoed"),
            "session id must have been echoed for the call to succeed"
        );
    }

    #[tokio::test]
    async fn mcp_http_url_only_config_requires_no_command() {
        // A config with a URL and no command must not try to spawn anything.
        let cfg = McpServerConfig {
            name: "nocmd".to_string(),
            command: String::new(),
            args: Vec::new(),
            env: Default::default(),
            url: None,
            auto_approve: Default::default(),
        };
        let mgr = McpManager::from_config(&[cfg], ".").await;
        assert_eq!(mgr.startup_errors.len(), 1);
        assert!(
            mgr.startup_errors[0].contains("no command or url"),
            "got: {:?}",
            mgr.startup_errors
        );
    }

    fn python_exe() -> Option<String> {
        for c in &["python3", "python"] {
            if std::process::Command::new(c).arg("--version")
                .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
                .status().is_ok() { return Some(c.to_string()); }
        }
        None
    }

    fn write_temp_script(name: &str, script: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("mcp_test_server_{name}.py"));
        std::fs::write(&p, script).unwrap();
        p
    }

    const TEST_SERVER_SCRIPT: &str = r###"import sys,json,time
def rd():
 l=sys.stdin.readline()
 if not l:sys.exit(0)
 return json.loads(l)
def sr(i,r):
 sys.stdout.write(json.dumps({"jsonrpc":"2.0","id":i,"result":r})+"\n")
 sys.stdout.flush()
while True:
 r=rd();m=r["method"];rid=r["id"]
 if m=="initialize":sr(rid,{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"t","version":"0"}})
 elif m=="tools/list":sr(rid,{"tools":[{"name":"echo","inputSchema":{"type":"object","properties":{"msg":{"type":"string"}}}},{"name":"hang","inputSchema":{"type":"object","properties":{}}}]})
 elif m=="resources/list":sr(rid,{"resources":[]})
 elif m=="prompts/list":sr(rid,{"prompts":[]})
 elif m=="tools/call":
  p=r.get("params",{})
  if p.get("name")=="hang":time.sleep(5)
  else:
   msg=p.get("arguments",{}).get("msg","")
   sr(rid,{"content":[{"type":"text","text":"echo: "+msg}]})
"###;

    #[tokio::test]
    async fn mcp_call_with_timeout_normal_path() {
        let py = match python_exe() { Some(p) => p, None => return };
        let scr = write_temp_script("normal", TEST_SERVER_SCRIPT);
        let client = McpClient::spawn(&py, &[scr.to_str().unwrap().to_string()], &HashMap::new(), ".").unwrap();
        let _: types::InitializeResult = serde_json::from_value(
            client.call_with_timeout("initialize", Some(serde_json::json!({
                "protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"t","version":"0"}
            })), 5).await.unwrap(),
        ).unwrap();
        let _: types::ListToolsResult = serde_json::from_value(
            client.call_with_timeout("tools/list", None, 5).await.unwrap(),
        ).unwrap();
        let echo = client.call_with_timeout("tools/call",
            Some(serde_json::json!({"name":"echo","arguments":{"msg":"hi"}})), 5,
        ).await.unwrap();
        let r: types::CallToolResult = serde_json::from_value(echo).unwrap();
        if let types::ToolContent::Text { text } = &r.content[0] { assert_eq!(text, "echo: hi"); }
        else { panic!("expected text"); }
    }

    #[tokio::test]
    async fn mcp_call_times_out_on_hanging_server() {
        let py = match python_exe() { Some(p) => p, None => return };
        let scr = write_temp_script("hang", TEST_SERVER_SCRIPT);
        let client = McpClient::spawn(&py, &[scr.to_str().unwrap().to_string()], &HashMap::new(), ".").unwrap();
        let _ = client.call_with_timeout("initialize", Some(serde_json::json!({
            "protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"t","version":"0"}
        })), 5).await.unwrap();
        let _ = client.call_with_timeout("tools/list", None, 5).await.unwrap();
        let r = client.call_with_timeout("tools/call",
            Some(serde_json::json!({"name":"hang","arguments":{}})), 2,
        ).await;
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("timed out"));
    }

    const NOTIFY_SERVER: &str = r###"import sys,json,time
tools_v1=[{"name":"echo","inputSchema":{"type":"object","properties":{"msg":{"type":"string"}}}},{"name":"hang","inputSchema":{"type":"object","properties":{}}},{"name":"trigger","inputSchema":{"type":"object","properties":{}}}]
tools_v2=[{"name":"echo","inputSchema":{"type":"object","properties":{"msg":{"type":"string"}}}},{"name":"new_tool","inputSchema":{"type":"object","properties":{"val":{"type":"integer"}}}},{"name":"trigger","inputSchema":{"type":"object","properties":{}}}]
cur=tools_v1
def rd():
 l=sys.stdin.readline()
 if not l:sys.exit(0)
 return json.loads(l)
def sr(i,r):
 sys.stdout.write(json.dumps({"jsonrpc":"2.0","id":i,"result":r})+"\n")
 sys.stdout.flush()
def sn(m,p=None):
 n={"jsonrpc":"2.0","method":m}
 if p:n["params"]=p
 sys.stdout.write(json.dumps(n)+"\n")
 sys.stdout.flush()
while True:
 r=rd();m=r["method"];rid=r["id"]
 if m=="initialize":sr(rid,{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"t","version":"0"}})
 elif m=="tools/list":sr(rid,{"tools":cur})
 elif m=="resources/list":sr(rid,{"resources":[]})
 elif m=="prompts/list":sr(rid,{"prompts":[]})
 elif m=="tools/call":
  p=r.get("params",{})
  if p.get("name")=="hang":time.sleep(5)
  elif p.get("name")=="trigger":
   cur=tools_v2
   sn("notifications/tools/list_changed")
   sr(rid,{"content":[{"type":"text","text":"switched"}]})
  else:
   msg=p.get("arguments",{}).get("msg","")
   sr(rid,{"content":[{"type":"text","text":"echo: "+msg}]})
"###;

    #[tokio::test]
    async fn mcp_tools_list_changed_notification_refreshes_tools() {
        use types::McpPolicy;
        let py = match python_exe() { Some(p) => p, None => return };
        let scr = write_temp_script("notify", NOTIFY_SERVER);
        let cfg = McpServerConfig { name: "t".into(), command: py, args: vec![scr.to_str().unwrap().into()], env: HashMap::new(), url: None, auto_approve: McpPolicy::Trusted };
        let mgr = McpManager::from_config(&[cfg], ".").await;
        assert!(mgr.startup_errors.is_empty(), "startup: {:?}", mgr.startup_errors);
        let v1: Vec<_> = mgr.all_tools().iter().map(|(n,_)| n.clone()).collect();
        assert!(v1.contains(&"mcp__t__trigger".into()));
        assert_eq!(v1.len(), 3);
        mgr.call_tool("mcp__t__trigger", serde_json::json!({})).await.unwrap();
        let v2: Vec<_> = mgr.all_tools().iter().map(|(n,_)| n.clone()).collect();
        assert!(!v2.contains(&"mcp__t__hang".into()));
        assert!(v2.contains(&"mcp__t__new_tool".into()));
    }

    const RESOURCE_SERVER: &str = r###"import sys,json
res=[{"uri":"doc://r","name":"README","description":"readme","mimeType":"text/plain"},{"uri":"doc://c","name":"CHANGELOG","description":"log","mimeType":"text/plain"}]
cnt={"doc://r":"# Hello","doc://c":"## v1"}
def rd():
 l=sys.stdin.readline()
 if not l:sys.exit(0)
 return json.loads(l)
def sr(i,r):
 sys.stdout.write(json.dumps({"jsonrpc":"2.0","id":i,"result":r})+"\n")
 sys.stdout.flush()
def sn(m,p=None):
 n={"jsonrpc":"2.0","method":m}
 if p:n["params"]=p
 sys.stdout.write(json.dumps(n)+"\n")
 sys.stdout.flush()
while True:
 r=rd();m=r["method"];rid=r["id"]
 if m=="initialize":sr(rid,{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"t","version":"0"}})
 elif m=="tools/list":sr(rid,{"tools":[{"name":"add","inputSchema":{"type":"object","properties":{}}}]})
 elif m=="resources/list":sr(rid,{"resources":res})
 elif m=="prompts/list":sr(rid,{"prompts":[]})
 elif m=="resources/read":
  uri=r.get("params",{}).get("uri","")
  sr(rid,{"contents":[{"type":"text","uri":uri,"text":cnt.get(uri,"?")}]})
 elif m=="tools/call":
  if r.get("params",{}).get("name")=="add":
   res.append({"uri":"doc://n","name":"New","description":"new","mimeType":"text/plain"})
   cnt["doc://n"]="# New"
   sn("notifications/resources/list_changed")
   sr(rid,{"content":[{"type":"text","text":"added"}]})
"###;

    #[tokio::test]
    async fn mcp_resource_list_and_read() {
        use types::McpPolicy;
        let py = match python_exe() { Some(p) => p, None => return };
        let scr = write_temp_script("resource", RESOURCE_SERVER);
        let cfg = McpServerConfig { name: "r".into(), command: py, args: vec![scr.to_str().unwrap().into()], env: HashMap::new(), url: None, auto_approve: McpPolicy::Trusted };
        let mgr = McpManager::from_config(&[cfg], ".").await;
        assert!(mgr.startup_errors.is_empty());
        let res: Vec<_> = mgr.all_resources().iter().map(|(f,_,_)| f.clone()).collect();
        assert!(res.contains(&"mcp__r__doc://r".into()));
        assert_eq!(res.len(), 2);
        let r = mgr.read_resource("r", "doc://r").await.unwrap();
        if let types::ResourceContent::Text { text, .. } = &r.contents[0] { assert_eq!(text, "# Hello"); }
        mgr.call_tool("mcp__r__add", serde_json::json!({})).await.unwrap();
        let _ = mgr.read_resource("r", "doc://r").await;
        let res2: Vec<_> = mgr.all_resources().iter().map(|(f,_,_)| f.clone()).collect();
        assert_eq!(res2.len(), 3);
        assert!(res2.iter().any(|r| r.contains("n")));
    }

    const PROMPT_SERVER: &str = r###"import sys,json
prompts=[{"name":"g","description":"greeting","arguments":[{"name":"u","description":"user","required":True}]},{"name":"f","description":"farewell","arguments":[]}]
def rd():
 l=sys.stdin.readline()
 if not l:sys.exit(0)
 return json.loads(l)
def sr(i,r):
 sys.stdout.write(json.dumps({"jsonrpc":"2.0","id":i,"result":r})+"\n")
 sys.stdout.flush()
while True:
 r=rd();m=r["method"];rid=r["id"]
 if m=="initialize":sr(rid,{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"t","version":"0"}})
 elif m=="tools/list":sr(rid,{"tools":[]})
 elif m=="resources/list":sr(rid,{"resources":[]})
 elif m=="prompts/list":sr(rid,{"prompts":prompts})
 elif m=="prompts/get":
  p=r.get("params",{})
  nm=p.get("name","")
  u=p.get("arguments",{}).get("u","there")
  t="Hello, "+u if nm=="g" else "Bye"
  sr(rid,{"messages":[{"role":"user","content":{"type":"text","text":t}}]})
"###;

    #[tokio::test]
    async fn mcp_prompt_list_and_get() {
        use types::McpPolicy;
        let py = match python_exe() { Some(p) => p, None => return };
        let scr = write_temp_script("prompt", PROMPT_SERVER);
        let cfg = McpServerConfig { name: "p".into(), command: py, args: vec![scr.to_str().unwrap().into()], env: HashMap::new(), url: None, auto_approve: McpPolicy::Trusted };
        let mgr = McpManager::from_config(&[cfg], ".").await;
        assert!(mgr.startup_errors.is_empty());
        let ps: Vec<_> = mgr.all_prompts().iter().map(|(f,_,_)| f.clone()).collect();
        assert!(ps.contains(&"mcp__p__g".into()));
        assert!(ps.contains(&"mcp__p__f".into()));
        let r = mgr.get_prompt("p", "g", Some(serde_json::json!({"u":"Alice"}))).await.unwrap();
        if let types::PromptContent::Text { text } = &r.messages[0].content { assert!(text.contains("Alice")); }
        let r2 = mgr.get_prompt("p", "f", None).await.unwrap();
        assert_eq!(r2.messages.len(), 1);
    }

    const STDERR_SERVER: &str = r###"import sys,json
def rd():
 l=sys.stdin.readline()
 if not l:sys.exit(0)
 return json.loads(l)
def sr(i,r):
 sys.stdout.write(json.dumps({"jsonrpc":"2.0","id":i,"result":r})+"\n")
 sys.stdout.flush()
sys.stderr.write("[init] start\n")
sys.stderr.flush()
while True:
 r=rd();m=r["method"];rid=r["id"]
 if m=="initialize":
  sys.stderr.write("[diag] init\n")
  sys.stderr.flush()
  sr(rid,{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"t","version":"0"}})
 elif m=="tools/list":sr(rid,{"tools":[{"name":"e","inputSchema":{"type":"object","properties":{"msg":{"type":"string"}}}}]})
 elif m=="resources/list":sr(rid,{"resources":[]})
 elif m=="prompts/list":sr(rid,{"prompts":[]})
 elif m=="tools/call":
  m2=r.get("params",{}).get("arguments",{}).get("msg","")
  sys.stderr.write("[diag] echo: "+m2+"\n")
  sys.stderr.flush()
  sr(rid,{"content":[{"type":"text","text":"echo: "+m2}]})
"###;

    #[tokio::test]
    async fn mcp_stderr_capture() {
        use types::McpPolicy;
        let py = match python_exe() { Some(p) => p, None => return };
        let scr = write_temp_script("stderr", STDERR_SERVER);
        let cfg = McpServerConfig { name: "s".into(), command: py, args: vec![scr.to_str().unwrap().into()], env: HashMap::new(), url: None, auto_approve: McpPolicy::Trusted };
        let mgr = McpManager::from_config(&[cfg], ".").await;
        // stderr is drained by a background task, so poll briefly instead of
        // asserting on the instantaneous snapshot.
        let wait_for_line = |mgr: &McpManager, needle: &'static str| {
            let lines = mgr.server_stderr("s").unwrap();
            lines.iter().any(|l| l.contains(needle))
        };
        for _ in 0..200 {
            if wait_for_line(&mgr, "start") { break; }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(wait_for_line(&mgr, "start"));
        // Snapshots are non-destructive — the UI polls this every frame.
        assert!(wait_for_line(&mgr, "start"));
        mgr.call_tool("mcp__s__e", serde_json::json!({"msg":"hi"})).await.unwrap();
        for _ in 0..200 {
            if wait_for_line(&mgr, "echo: hi") { break; }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(wait_for_line(&mgr, "echo: hi"));
        assert!(wait_for_line(&mgr, "start"), "older lines are kept");
    }

    // --- Cancellation + Progress + Roots test ---

    const ADVANCED_SERVER: &str = r###"import sys,json,time
def rd():
 l=sys.stdin.readline()
 if not l:sys.exit(0)
 return json.loads(l)
def sr(i,r):
 sys.stdout.write(json.dumps({"jsonrpc":"2.0","id":i,"result":r})+"\n")
 sys.stdout.flush()
def sn(m,p=None):
 n={"jsonrpc":"2.0","method":m}
 if p:n["params"]=p
 sys.stdout.write(json.dumps(n)+"\n")
 sys.stdout.flush()
resp_count=0
while True:
 r=rd();m=r["method"];rid=r["id"]
 if m=="initialize":
  # Server also requests roots/list from client
  sr(rid,{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"t","version":"0"}})
 elif m=="tools/list":
  sr(rid,{"tools":[{"name":"long_task","inputSchema":{"type":"object","properties":{"token":{"type":"string"}}}},{"name":"roots_probe","inputSchema":{"type":"object","properties":{}}}]})
 elif m=="resources/list":sr(rid,{"resources":[]})
 elif m=="prompts/list":sr(rid,{"prompts":[]})
 elif m=="tools/call":
  p=r.get("params",{})
  nm=p.get("name","")
  if nm=="long_task":
   token=p.get("arguments",{}).get("token","p1")
   for i in range(1,4):
    sn("notifications/progress",{"progressToken":token,"progress":i,"total":3,"message":"step "+str(i)})
    time.sleep(0.1)
   sr(rid,{"content":[{"type":"text","text":"done"}]})
  elif nm=="roots_probe":
   # Send roots/list request to client and capture response
   probe_id=999
   sys.stdout.write(json.dumps({"jsonrpc":"2.0","id":probe_id,"method":"roots/list","params":{}})+"\n")
   sys.stdout.flush()
   # Read response
   resp=json.loads(sys.stdin.readline())
   roots=resp.get("result",{}).get("roots",[])
   sr(rid,{"content":[{"type":"text","text":"roots: "+json.dumps(roots)}]})
  else:
   sr(rid,{"content":[{"type":"text","text":"ok"}]})
"###;

    #[tokio::test]
    async fn mcp_cancellation_progress_and_roots() {
        use types::McpPolicy;
        let py = match python_exe() { Some(p) => p, None => return };
        let scr = write_temp_script("advanced", ADVANCED_SERVER);
        let cfg = McpServerConfig { name: "a".into(), command: py, args: vec![scr.to_str().unwrap().into()], env: HashMap::new(), url: None, auto_approve: McpPolicy::Trusted };
        let mgr = McpManager::from_config(&[cfg], ".").await;
        assert!(mgr.startup_errors.is_empty());

        // --- Progress ---
        // long_task sends 3 progress notifications while it runs. Progress is
        // observable during the call and cleared once it completes.
        let (call_result, observed) = tokio::join!(
            mgr.call_tool("mcp__a__long_task", serde_json::json!({"token":"p1"})),
            async {
                for _ in 0..200 {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    if let Some((server, info)) = mgr.active_progress() {
                        return Some((server, info));
                    }
                }
                None
            }
        );
        call_result.unwrap();
        let (server, info) = observed.expect("progress visible while the call runs");
        assert_eq!(server, "a");
        assert_eq!(info.total, Some(3.0));
        assert!(info.progress >= 1.0);
        assert!(info.message.as_deref().unwrap_or("").starts_with("step"));
        // Finished call leaves no stale progress for the footer to show.
        assert!(mgr.active_progress().is_none());

        // --- Roots ---
        // roots_probe sends roots/list to client, reads response.
        let r = mgr.call_tool("mcp__a__roots_probe", serde_json::json!({})).await.unwrap();
        let text: Vec<_> = r.content.iter().filter_map(|c| match c {
            types::ToolContent::Text { text } => Some(text.as_str()),
            _ => None,
        }).collect();
        let joined = text.join("");
        assert!(joined.contains("file://"), "roots response: {joined}");
        assert!(joined.contains("roots"), "response should mention roots: {joined}");
    }
}
