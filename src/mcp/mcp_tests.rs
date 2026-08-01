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

use super::*;
use super::types::McpPolicy;

impl McpManager {
    /// Initialise all configured MCP servers. Spawns each, runs the handshake,
    /// discovers tools/resources/prompts.
    pub(crate) async fn from_config(configs: &[McpServerConfig], workspace_root: &str) -> Self {
        Self::from_config_with_updates(configs, workspace_root, |_, _| {}).await
    }
}

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

#[test]
fn server_config_without_url_deserializes() {
    // Configs written before HTTP support have no `url` key.
    let cfg: McpServerConfig =
        toml::from_str("name = \"old\"\ncommand = \"npx\"\nargs = [\"-y\", \"server\"]\n")
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
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
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
        let body = serde_json::json!({
            "jsonrpc": "2.0", "id": id,
            "error": {"code": -32000, "message": msg},
        })
        .to_string();
        http_json("", &body)
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
            if !headers
                .to_ascii_lowercase()
                .contains("mcp-session-id: sess-1")
            {
                return error("missing session id");
            }
            let params = req
                .get("params")
                .and_then(|p| p.as_object());
            let tool_name = params
                .and_then(|p| p.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("");
            match tool_name {
                "echo" => {
                    let input = params
                        .and_then(|p| p.get("arguments"))
                        .and_then(|a| a.get("text"))
                        .and_then(|t| t.as_str())
                        .unwrap_or("(no text)");
                    http_json(
                        "",
                        &reply(serde_json::json!({
                            "content": [{"type": "text", "text": format!("echo: {input}")}],
                        })),
                    )
                }
                "fail" => error("intentional failure"),
                _ => {
                    http_json("", &reply(serde_json::json!({"content": [],})))
                }
            }
        }
        _ => error("unknown method"),
    }
}

async fn initialize_mock_http_client(client: &McpHttpClient) {
    client
        .call(
            "initialize",
            Some(serde_json::json!({
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": {"name": "test", "version": "0"},
            })),
        )
        .await
        .expect("initialize");
    client
        .send_notification("notifications/initialized", None)
        .await
        .expect("initialized notification");
}

#[tokio::test]
async fn http_client_echo_roundtrip() {
    let (url, _jh) = spawn_mock_http_server().await;
    let client = McpHttpClient::new(&url, &HashMap::new()).expect("connect to mock");
    initialize_mock_http_client(&client).await;
    let tools_raw = client.call("tools/list", None).await.expect("list tools");
    let tools: Vec<McpTool> = serde_json::from_value(
        tools_raw.get("tools").cloned().unwrap_or_default(),
    )
    .unwrap_or_default();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "echo");
    let result = client
        .call(
            "tools/call",
            Some(serde_json::json!({
                "name": "echo",
                "arguments": {"text": "hello-http"},
            })),
        )
        .await
        .expect("echo call");
    let text = result
        .get("content")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|c| c.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or("");
    assert_eq!(text, "echo: hello-http");
}

#[tokio::test]
async fn http_client_error_surfaces_failure() {
    let (url, _jh) = spawn_mock_http_server().await;
    let client = McpHttpClient::new(&url, &HashMap::new()).expect("connect to mock");
    initialize_mock_http_client(&client).await;
    let err = client
        .call(
            "tools/call",
            Some(serde_json::json!({
                "name": "fail",
            })),
        )
        .await
        .expect_err("fail should error");
    assert!(
        err.to_string().contains("intentional failure"),
        "error: {err}"
    );
}

#[tokio::test]
async fn full_integration_stdio_and_http_with_progress_and_roots() {
    // Find the integration test harness — the crate-relative path is fixed
    // (mcp/test-harness/), so walk up from the source file location.
    let harness_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("mcp")
        .join("test-harness");
    if !harness_dir.join("package.json").exists() {
        eprintln!("skipping (no test-harness)");
        return;
    }
    let _ = std::process::Command::new("npm")
        .arg("install")
        .current_dir(&harness_dir)
        .output();

    let cfg = McpServerConfig {
        name: "a".into(),
        command: "node".into(),
        args: vec!["index.mjs".into()],
        env: std::collections::HashMap::new(),
        url: None,
        auto_approve: McpPolicy::Trusted,
    };
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
    let r = mgr
        .call_tool("mcp__a__roots_probe", serde_json::json!({}))
        .await
        .unwrap();
    let text: Vec<_> = r
        .content
        .iter()
        .filter_map(|c| match c {
            types::ToolContent::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    let joined = text.join("");
    assert!(joined.contains("file://"), "roots response: {joined}");
    assert!(
        joined.contains("roots"),
        "response should mention roots: {joined}"
    );
}
