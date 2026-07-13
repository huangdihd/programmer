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
pub mod types;

use client::McpClient;
use std::collections::HashMap;
use types::{CallToolResult, McpServerConfig, McpTool};

/// Tracks a connected MCP server and its discovered tools.
struct McpServer {
    client: McpClient,
    tools: Vec<McpTool>,
}

/// Manages all configured MCP server connections.
pub struct McpManager {
    servers: HashMap<String, McpServer>,
    /// Errors encountered during startup (reported to the UI).
    pub(crate) startup_errors: Vec<String>,
}

impl McpManager {
    /// No-op manager when no MCP servers are configured.
    pub(crate) fn empty() -> Self {
        McpManager {
            servers: HashMap::new(),
            startup_errors: Vec::new(),
        }
    }

    /// Initialise all configured MCP servers. Spawns each process, runs the
    /// `initialize` handshake, discovers tools via `tools/list`, and keeps
    /// the connections alive for subsequent tool calls.
    ///
    /// Servers that fail to start or discover tools are logged and skipped;
    /// the rest continue normally.
    pub(crate) async fn from_config(configs: &[McpServerConfig]) -> Self {
        let mut servers: HashMap<String, McpServer> = HashMap::new();
        let mut startup_errors: Vec<String> = Vec::new();

        for cfg in configs {
            let name = cfg.name.clone();
            match Self::connect_one(cfg).await {
                Ok(server) => {
                    servers.insert(name.clone(), server);
                }
                Err(e) => {
                    startup_errors.push(format!("MCP server '{name}': {e}"));
                }
            }
        }

        McpManager {
            servers,
            startup_errors,
        }
    }

    /// Spawn and handshake a single server.
    async fn connect_one(cfg: &McpServerConfig) -> Result<McpServer, String> {
        let client = McpClient::spawn(&cfg.command, &cfg.args, &cfg.env)?;

        // Step 1: initialize
        let init_params = serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {
                "name": "programmer",
                "version": env!("CARGO_PKG_VERSION")
            }
        });
        let _init_result: types::InitializeResult = serde_json::from_value(
            client
                .call("initialize", Some(init_params))
                .await
                .map_err(|e| format!("initialize failed: {e}"))?,
        )
        .map_err(|e| format!("bad initialize result: {e}"))?;

        // Step 2: send initialized notification (fire-and-forget via call
        // but we don't need the result).
        let _ = client
            .call("notifications/initialized", None)
            .await;

        // Step 3: tools/list
        let tools_result: types::ListToolsResult = serde_json::from_value(
            client
                .call("tools/list", None)
                .await
                .map_err(|e| format!("tools/list failed: {e}"))?,
        )
        .map_err(|e| format!("bad tools/list result: {e}"))?;

        Ok(McpServer {
            client,
            tools: tools_result.tools,
        })
    }

    // -- queries --

    /// True when at least one MCP server is connected.
    pub(crate) fn is_connected(&self) -> bool {
        !self.servers.is_empty()
    }

    /// Number of connected servers.
    pub(crate) fn server_count(&self) -> usize {
        self.servers.len()
    }

    /// All discovered tools across all servers, with names prefixed as
    /// `mcp__<server>__<tool>` to avoid collisions with built-in tools.
    pub(crate) fn all_tools(&self) -> Vec<(String, McpTool)> {
        let mut tools = Vec::new();
        for (server_name, server) in &self.servers {
            for tool in &server.tools {
                let fqn = format!("mcp__{}__{}", server_name, tool.name);
                tools.push((fqn, tool.clone()));
            }
        }
        tools
    }

    /// Execute a tool call on the appropriate MCP server. `fqn` is the
    /// fully-qualified tool name: `mcp__<server>__<tool>`.
    pub(crate) async fn call_tool(
        &self,
        fqn: &str,
        arguments: serde_json::Value,
    ) -> Result<CallToolResult, String> {
        let (server_name, tool_name) = parse_fqn(fqn)
            .ok_or_else(|| format!("invalid MCP tool name: {fqn}"))?;

        let server = self
            .servers
            .get(server_name)
            .ok_or_else(|| format!("MCP server '{server_name}' not found"))?;

        let params = serde_json::json!({
            "name": tool_name,
            "arguments": arguments,
        });

        let raw = server
            .client
            .call("tools/call", Some(params))
            .await?;

        let result: CallToolResult = serde_json::from_value(raw)
            .map_err(|e| format!("bad tools/call result: {e}"))?;

        Ok(result)
    }
}

/// Parse `mcp__server__tool` → `("server", "tool")`.
fn parse_fqn(fqn: &str) -> Option<(&str, &str)> {
    let rest = fqn.strip_prefix("mcp__")?;
    let (server, tool) = rest.split_once("__")?;
    Some((server, tool))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_fqn_valid() {
        assert_eq!(
            parse_fqn("mcp__filesystem__read_file"),
            Some(("filesystem", "read_file"))
        );
    }

    #[test]
    fn parse_fqn_no_prefix() {
        assert_eq!(parse_fqn("command"), None);
    }

    #[test]
    fn parse_fqn_partial() {
        assert_eq!(parse_fqn("mcp__filesystem"), None);
    }
}
