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

fn server(mode: WorkMode) -> McpServer {
    McpServer::new(mode, None)
}

#[test]
fn gate_read_only_always_allows() {
    assert!(matches!(
        server(WorkMode::Auto).gate("read_file", "{}"),
        Gate::Allow
    ));
    assert!(matches!(
        server(WorkMode::Manual).gate("grep", "{}"),
        Gate::Allow
    ));
}

#[test]
fn gate_yolo_allows_dangerous() {
    assert!(matches!(
        server(WorkMode::Yolo).gate("command", "{}"),
        Gate::Allow
    ));
}

#[test]
fn gate_plan_denies_mutating() {
    assert!(matches!(
        server(WorkMode::Plan).gate("write_file", "{}"),
        Gate::Deny(_)
    ));
}

#[test]
fn gate_auto_defers_to_llm() {
    assert!(matches!(
        server(WorkMode::Auto).gate("command", "{}"),
        Gate::Llm
    ));
}

#[test]
fn gate_manual_elicits_when_supported_else_denies() {
    // No elicitation capability → deny.
    assert!(matches!(
        server(WorkMode::Manual).gate("command", "{}"),
        Gate::Deny(_)
    ));
    // With capability → elicit.
    let mut s = server(WorkMode::Manual);
    s.client_elicitation = true;
    assert!(matches!(s.gate("command", "{}"), Gate::Elicit));
}

#[tokio::test]
async fn auto_without_classifier_refuses_dangerous() {
    let denial = server(WorkMode::Auto)
        .llm_approve("command", "{}")
        .await
        .expect_err("no classifier configured");
    assert!(denial.contains("no classifier"), "got: {denial}");
}

#[test]
fn initialize_reads_elicitation_capability_and_echoes_version() {
    let mut s = server(WorkMode::Manual);
    let params = json!({
        "protocolVersion": "2025-06-18",
        "capabilities": { "elicitation": {} }
    });
    let result = s.on_initialize(Some(&params));
    assert!(s.client_elicitation);
    assert_eq!(result["protocolVersion"], "2025-06-18");
    assert_eq!(result["serverInfo"]["name"], "programmer");
}

#[test]
fn tools_list_excludes_ask_user() {
    let v = tools_list_result();
    let names: Vec<&str> = v["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"read_file"));
    assert!(names.contains(&"command"));
    assert!(!names.contains(&"ask_user"));
}

#[tokio::test]
async fn manual_elicitation_accept_allows() {
    // Simulate a client that accepts the elicitation.
    let input = "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"action\":\"accept\"}}\n";
    let mut lines = BufReader::new(input.as_bytes()).lines();
    let mut out: Vec<u8> = Vec::new();
    let mut s = server(WorkMode::Manual);
    s.client_elicitation = true;
    let result = s
        .elicit_approve("command", "{}", &mut lines, &mut out)
        .await;
    assert!(result.is_ok(), "accept should approve: {result:?}");
    // The server sent an elicitation/create request.
    let sent = String::from_utf8(out).unwrap();
    assert!(sent.contains("elicitation/create"), "sent: {sent}");
}

#[tokio::test]
async fn manual_elicitation_decline_denies() {
    let input = "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"action\":\"decline\"}}\n";
    let mut lines = BufReader::new(input.as_bytes()).lines();
    let mut out: Vec<u8> = Vec::new();
    let mut s = server(WorkMode::Manual);
    s.client_elicitation = true;
    let result = s
        .elicit_approve("command", "{}", &mut lines, &mut out)
        .await;
    assert!(result.is_err(), "decline should deny");
}
