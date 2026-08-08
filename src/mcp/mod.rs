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
pub mod console;
pub mod http_client;
pub mod http_server;
pub mod server;
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
#[allow(clippy::large_enum_variant)]
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
            // Notifications expect no response body, but the HTTP POST still
            // awaits the server's status reply — give it a deadline too.
            McpConn::Http(c) => tokio::time::timeout(
                Duration::from_secs(HANDSHAKE_TIMEOUT_SECS),
                c.send_notification(method, params),
            )
            .await
            .map_err(|_| {
                format!("MCP notification '{method}' timed out after {HANDSHAKE_TIMEOUT_SECS}s")
            })?,
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
        if !self.client.take_tools_list_changed() {
            return;
        }
        if let Ok(raw) = self
            .client
            .call_with_timeout("tools/list", None, HANDSHAKE_TIMEOUT_SECS)
            .await
            && let Ok(r) = serde_json::from_value::<types::ListToolsResult>(raw)
        {
            *self.tools.lock().unwrap() = r.tools;
        }
    }

    async fn refresh_resources_if_stale(&self) {
        let changed = self.client.take_resources_list_changed();
        let _upd = self.client.take_resources_updated();
        if !changed && !_upd {
            return;
        }
        if let Ok(raw) = self
            .client
            .call_with_timeout("resources/list", None, HANDSHAKE_TIMEOUT_SECS)
            .await
            && let Ok(r) = serde_json::from_value::<types::ListResourcesResult>(raw)
        {
            *self.resources.lock().unwrap() = r.resources;
        }
    }

    async fn refresh_prompts_if_stale(&self) {
        if !self.client.take_prompts_list_changed() {
            return;
        }
        if let Ok(raw) = self
            .client
            .call_with_timeout("prompts/list", None, HANDSHAKE_TIMEOUT_SECS)
            .await
            && let Ok(r) = serde_json::from_value::<types::ListPromptsResult>(raw)
        {
            *self.prompts.lock().unwrap() = r.prompts;
        }
    }

    fn stderr_snapshot(&self) -> Vec<String> {
        self.client.stderr_snapshot()
    }
}

/// Manages all configured MCP server connections.
pub struct McpManager {
    servers: HashMap<String, McpServer>,
    pub(crate) startup_errors: Vec<String>,
}

/// Current connection state for one configured MCP server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpConnectionState {
    /// The server handshake is still in progress.
    Connecting,
    /// The server completed its handshake and tool discovery.
    Connected { tool_count: usize },
    /// The server failed before becoming available.
    Failed { error: String },
}

/// MCP state retained independently from the live manager so the UI can show
/// servers while they are still connecting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct McpServerStatus {
    pub(crate) name: String,
    pub(crate) state: McpConnectionState,
}

impl McpServerStatus {
    pub(crate) fn connecting(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            state: McpConnectionState::Connecting,
        }
    }
}

impl McpManager {
    /// Initialise all configured MCP servers and report each completed
    /// connection while the remaining servers continue loading.
    pub(crate) async fn from_config_with_updates<F>(
        configs: &[McpServerConfig],
        workspace_root: &str,
        mut on_update: F,
    ) -> Self
    where
        F: FnMut(String, McpConnectionState),
    {
        let mut servers: HashMap<String, McpServer> = HashMap::new();
        let mut startup_errors: Vec<String> = Vec::new();

        for cfg in configs {
            let name = cfg.name.clone();
            match Self::connect_one(cfg, workspace_root).await {
                Ok(server) => {
                    let tool_count = server.tools.lock().unwrap().len();
                    on_update(name.clone(), McpConnectionState::Connected { tool_count });
                    servers.insert(name.clone(), server);
                }
                Err(e) => {
                    on_update(
                        name.clone(),
                        McpConnectionState::Failed { error: e.clone() },
                    );
                    startup_errors.push(format!("MCP server '{name}': {e}"));
                }
            }
        }

        McpManager {
            servers,
            startup_errors,
        }
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
            client
                .call_with_timeout("initialize", Some(init_params), HANDSHAKE_TIMEOUT_SECS)
                .await
                .map_err(|e| format!("initialize failed: {e}"))?,
        )
        .map_err(|e| format!("bad initialize result: {e}"))?;

        let _ = client
            .send_notification("notifications/initialized", None)
            .await;

        // Step 2: tools/list
        let tools = serde_json::from_value::<types::ListToolsResult>(
            client
                .call_with_timeout("tools/list", None, HANDSHAKE_TIMEOUT_SECS)
                .await
                .map_err(|e| format!("tools/list failed: {e}"))?,
        )
        .map_err(|e| format!("bad tools/list result: {e}"))?
        .tools;

        // Step 3: resources/list (best-effort)
        let resources = match client
            .call_with_timeout("resources/list", None, HANDSHAKE_TIMEOUT_SECS)
            .await
        {
            Ok(raw) => serde_json::from_value::<types::ListResourcesResult>(raw)
                .map(|r| r.resources)
                .unwrap_or_default(),
            Err(_) => Vec::new(),
        };

        // Step 4: prompts/list (best-effort)
        let prompts = match client
            .call_with_timeout("prompts/list", None, HANDSHAKE_TIMEOUT_SECS)
            .await
        {
            Ok(raw) => serde_json::from_value::<types::ListPromptsResult>(raw)
                .map(|r| r.prompts)
                .unwrap_or_default(),
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

    pub(crate) fn has_server(&self, name: &str) -> bool {
        self.servers.contains_key(name)
    }

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

    // -- resource / prompt access --

    pub(crate) async fn read_resource(
        &self,
        server_name: &str,
        uri: &str,
    ) -> Result<types::ReadResourceResult, String> {
        let s = self
            .servers
            .get(server_name)
            .ok_or_else(|| format!("MCP server '{server_name}' not found"))?;
        s.refresh_resources_if_stale().await;
        let raw = s
            .client
            .call_with_timeout(
                "resources/read",
                Some(serde_json::json!({"uri": uri})),
                TOOL_CALL_TIMEOUT_SECS,
            )
            .await?;
        serde_json::from_value(raw).map_err(|e| format!("bad resources/read result: {e}"))
    }

    pub(crate) async fn get_prompt(
        &self,
        server_name: &str,
        prompt_name: &str,
        arguments: Option<serde_json::Value>,
    ) -> Result<types::GetPromptResult, String> {
        let s = self
            .servers
            .get(server_name)
            .ok_or_else(|| format!("MCP server '{server_name}' not found"))?;
        s.refresh_prompts_if_stale().await;
        let params = match arguments {
            Some(a) => serde_json::json!({"name": prompt_name, "arguments": a}),
            None => serde_json::json!({"name": prompt_name}),
        };
        let raw = s
            .client
            .call_with_timeout("prompts/get", Some(params), TOOL_CALL_TIMEOUT_SECS)
            .await?;
        serde_json::from_value(raw).map_err(|e| format!("bad prompts/get result: {e}"))
    }

    // -- tool call --

    pub(crate) async fn call_tool(
        &self,
        fqn: &str,
        arguments: serde_json::Value,
    ) -> Result<CallToolResult, String> {
        let (server_name, tool_name) =
            parse_fqn(fqn).ok_or_else(|| format!("invalid MCP tool name: {fqn}"))?;
        let s = self
            .servers
            .get(server_name)
            .ok_or_else(|| format!("MCP server '{server_name}' not found"))?;

        let raw = s
            .client
            .call_with_timeout(
                "tools/call",
                Some(serde_json::json!({
                    "name": tool_name,
                    "arguments": arguments,
                })),
                TOOL_CALL_TIMEOUT_SECS,
            )
            .await?;

        s.refresh_if_stale().await;
        s.refresh_resources_if_stale().await;

        serde_json::from_value(raw).map_err(|e| format!("bad tools/call result: {e}"))
    }
}

fn parse_fqn(fqn: &str) -> Option<(&str, &str)> {
    let rest = fqn.strip_prefix("mcp__")?;
    rest.split_once("__")
}

#[cfg(test)]
#[path = "mcp_tests.rs"]
mod tests;
