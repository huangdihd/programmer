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

//! MCP (Model Context Protocol) types.
//!
//! These mirror the JSON-RPC 2.0 + MCP spec types needed for server
//! communication. Not exhaustive — only the messages we actually send
//! or receive are modelled.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// JSON-RPC 2.0 envelope
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub(crate) struct JsonRpcRequest {
    pub(crate) jsonrpc: &'static str, // always "2.0"
    pub(crate) id: u64,
    pub(crate) method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) params: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum JsonRpcResponse {
    Success {
        #[allow(dead_code)]
        jsonrpc: String,
        id: u64,
        result: serde_json::Value,
    },
    Error {
        #[allow(dead_code)]
        jsonrpc: String,
        id: u64,
        error: JsonRpcError,
    },
}

#[derive(Debug, Deserialize)]
pub(crate) struct JsonRpcError {
    #[allow(dead_code)]
    pub(crate) code: i64,
    pub(crate) message: String,
    #[allow(dead_code)]
    #[serde(default)]
    pub(crate) data: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// MCP server configuration
// ---------------------------------------------------------------------------

/// Per-MCP-server tool-approval policy for the classifier.
///
/// Controls how tools from this server are handled across all work modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub(crate) enum McpPolicy {
    /// Tools from this server are trusted: auto-approved in every mode except
    /// Manual, where all tools require confirmation regardless.
    #[default]
    Trusted,
    /// Tools from this server must be reviewed: Auto mode sends them through
    /// the LLM classifier, Allow Edits and Manual pop up an approval prompt.
    Review,
}

/// A configured MCP server entry, deserialized from `programmer.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct McpServerConfig {
    /// Human-readable name for this server (e.g. "filesystem").
    pub(crate) name: String,
    /// The command to spawn (e.g. "npx", "python", "node"). Ignored (and may
    /// be empty) when `url` is set.
    #[serde(default)]
    pub(crate) command: String,
    /// Arguments passed to the command (stdio transport only).
    #[serde(default)]
    pub(crate) args: Vec<String>,
    /// stdio transport: environment variables injected into the child
    /// process. HTTP transport: extra request headers (e.g.
    /// `Authorization=Bearer xyz`).
    #[serde(default)]
    pub(crate) env: std::collections::HashMap<String, String>,
    /// URL of a remote server (MCP Streamable HTTP transport, e.g.
    /// `https://mcp.exa.ai/mcp`). When set, the server is reached over HTTP
    /// and `command`/`args` are ignored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) url: Option<String>,
    /// Per-server tool-approval policy. Defaults to `auto` (defer to the
    /// current work mode's classifier).
    #[serde(default)]
    pub(crate) auto_approve: McpPolicy,
}

// ---------------------------------------------------------------------------
// MCP protocol: initialize
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[allow(non_snake_case)]
pub(crate) struct InitializeResult {
    #[allow(dead_code)]
    pub(crate) protocolVersion: String,
    #[allow(dead_code)]
    pub(crate) capabilities: serde_json::Value,
    #[allow(dead_code)]
    pub(crate) serverInfo: ServerInfo,
}

#[derive(Debug, Deserialize)]
#[allow(non_snake_case)]
pub(crate) struct ServerInfo {
    #[allow(dead_code)]
    pub(crate) name: String,
    #[allow(dead_code)]
    #[serde(default)]
    pub(crate) version: String,
}

// ---------------------------------------------------------------------------
// MCP protocol: tools/list
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct ListToolsResult {
    pub(crate) tools: Vec<McpTool>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[allow(non_snake_case)]
pub(crate) struct McpTool {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) description: Option<String>,
    pub(crate) inputSchema: serde_json::Value,
}

// ---------------------------------------------------------------------------
// MCP protocol: tools/call
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[allow(non_snake_case)]
pub(crate) struct CallToolResult {
    pub(crate) content: Vec<ToolContent>,
    #[serde(default)]
    #[allow(dead_code)]
    pub(crate) isError: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[allow(non_snake_case)]
#[serde(tag = "type")]
pub(crate) enum ToolContent {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    #[allow(dead_code)]
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
    #[serde(rename = "resource")]
    #[allow(dead_code)]
    Resource {
        #[allow(dead_code)]
        resource: serde_json::Value,
    },
}

// ---------------------------------------------------------------------------
// MCP protocol: resources/list
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct ListResourcesResult {
    pub(crate) resources: Vec<McpResource>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct McpResource {
    pub(crate) uri: String,
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) description: Option<String>,
    #[serde(rename = "mimeType")]
    #[serde(default)]
    pub(crate) mime_type: Option<String>,
}

// ---------------------------------------------------------------------------
// MCP protocol: resources/read
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct ReadResourceResult {
    pub(crate) contents: Vec<ResourceContent>,
}

#[derive(Debug, Deserialize)]
#[allow(non_snake_case)]
#[serde(tag = "type")]
pub(crate) enum ResourceContent {
    #[serde(rename = "text")]
    Text {
        #[allow(dead_code)]
        uri: String,
        #[serde(rename = "mimeType")]
        #[serde(default)]
        #[allow(dead_code)]
        mime_type: Option<String>,
        text: String,
    },
    #[serde(rename = "blob")]
    #[allow(dead_code)]
    Blob {
        uri: String,
        #[serde(rename = "mimeType")]
        #[serde(default)]
        #[allow(dead_code)]
        mime_type: Option<String>,
        #[allow(dead_code)]
        blob: String,
    },
}

// ---------------------------------------------------------------------------
// MCP protocol: prompts/list
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct ListPromptsResult {
    pub(crate) prompts: Vec<McpPrompt>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct McpPrompt {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) description: Option<String>,
    /// Prompt arguments (template variables).
    #[serde(default)]
    pub(crate) arguments: Option<Vec<McpPromptArgument>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct McpPromptArgument {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) description: Option<String>,
    #[serde(default)]
    pub(crate) required: Option<bool>,
}

// ---------------------------------------------------------------------------
// MCP protocol: prompts/get
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct GetPromptResult {
    #[serde(default)]
    pub(crate) description: Option<String>,
    pub(crate) messages: Vec<PromptMessage>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PromptMessage {
    pub(crate) role: String,
    pub(crate) content: PromptContent,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub(crate) enum PromptContent {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    #[allow(dead_code)]
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
    #[serde(rename = "resource")]
    #[allow(dead_code)]
    Resource {
        #[allow(dead_code)]
        resource: serde_json::Value,
    },
}
