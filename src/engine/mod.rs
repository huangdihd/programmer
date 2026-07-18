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

//! The UI-free agent engine: the turn primitives (request building, streaming,
//! classification, tool execution) that both the TUI event loop and a headless
//! driver share, plus the driver itself. Extracted incrementally from the
//! `app` event handlers so the TUI keeps working unchanged while the same logic
//! becomes reusable for a print mode and, later, in-process sub-agents.

pub(crate) mod classify;
pub(crate) mod request;
pub(crate) mod stream;
pub(crate) mod surface;
pub(crate) mod tools;

pub(crate) use surface::{AgentSurface, HeadlessSurface, ReviewDecision};

use crate::cancel::CancellationToken;
use crate::classifier::Classifier;
use crate::conversation::Conversation;
use crate::mcp::McpManager;
use crate::mcp::types::McpPolicy;
use crate::response::partial_response::PartialResponse;
use crate::response::response_finish_reason::ResponseFinishReason;
use async_openai::Client;
use async_openai::config::OpenAIConfig;
use async_openai::error::OpenAIError;
use async_openai::types::responses::{FunctionToolCall, OutputItem, OutputMessageContent, Tool};
use std::collections::HashSet;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

/// The Auto-mode LLM classifier's inputs, boxed into [`EnginePolicy::Llm`] so
/// that variant isn't far larger than the others.
pub(crate) struct LlmPolicy {
    pub client: Client<OpenAIConfig>,
    pub model_name: String,
    pub no_logprobs: Arc<Mutex<HashSet<String>>>,
    pub mcp_policies: std::collections::HashMap<String, McpPolicy>,
}

/// How the engine decides whether a tool call may run. The TUI keeps its own
/// `WorkMode` and maps per-classification; this is the engine-level equivalent
/// for headless callers.
pub(crate) enum EnginePolicy {
    /// Everything is allowed without review.
    Yolo,
    /// A synchronous rule classifier (e.g. a Manual-style policy). Its `Ask`
    /// verdicts are routed to the surface for a decision. `dyn Classifier` is
    /// already `Send + Sync` via the trait's supertrait bounds, so this matches
    /// what [`crate::classifier::WorkMode::classifier`] returns directly.
    #[allow(dead_code)]
    Sync(Box<dyn Classifier>),
    /// The Auto-mode LLM classifier.
    Llm(Box<LlmPolicy>),
}

/// A headless agent engine: everything needed to run a turn to completion with
/// no UI. Reused by the `-p` print mode and, later, in-process sub-agents.
pub(crate) struct Engine {
    /// Client + resolved model id for the chat/completion calls.
    pub client: Client<OpenAIConfig>,
    pub model_name: String,
    /// `provider/model` string, named in the system-prompt banner.
    pub model_str: String,
    /// The advertised tool set (must exclude `ask_user` — see `run_turn`).
    pub tools: Vec<Tool>,
    pub policy: EnginePolicy,
    pub mcp: Option<Arc<McpManager>>,
    pub coauthor: Option<String>,
    pub max_iterations: usize,
}

/// The result of a completed turn.
#[derive(Debug)]
pub(crate) struct TurnResult {
    pub final_text: String,
    #[allow(dead_code)]
    pub usage: (u32, u32),
}

/// Why a turn could not complete.
#[derive(Debug, thiserror::Error)]
pub(crate) enum EngineError {
    #[error("stream error: {0}")]
    Stream(OpenAIError),
    #[error("api error {code:?}: {message}")]
    Api { code: Option<String>, message: String },
    #[error("cancelled")]
    Cancelled,
    #[error("exceeded the {0}-iteration tool-loop cap")]
    IterationCap(usize),
    #[error("the model returned no output")]
    EmptyResponse,
}

/// Progress events emitted during a turn. Print mode ignores these; sub-agents
/// will stream them.
#[allow(dead_code)]
pub(crate) enum EngineEvent<'a> {
    /// The model produced assistant text this iteration.
    Assistant(&'a str),
    /// A tool call is about to run.
    ToolCall { name: &'a str },
}

impl Engine {
    /// Run a full turn: stream a response, run any tool calls it requests, and
    /// loop until the model answers with no tool calls (returning its text) or
    /// an error/cap is hit. `conversation` is mutated in place with every
    /// output and tool result, exactly as the TUI would record them.
    pub(crate) async fn run_turn(
        &self,
        conversation: &mut Conversation,
        cancel: &CancellationToken,
        surface: &dyn AgentSurface,
    ) -> Result<TurnResult, EngineError> {
        let retrying = AtomicBool::new(false);

        for _ in 0..self.max_iterations {
            if cancel.is_cancelled() {
                return Err(EngineError::Cancelled);
            }

            // ---- stream one response, folding it locally ----
            let ctx = request::SystemContext {
                current_model: &self.model_str,
                skill_prompt: None,
                plan_prompt: None,
                coauthor: self.coauthor.as_deref(),
            };
            let req = request::build_request(
                conversation,
                &ctx,
                self.model_name.clone(),
                self.tools.clone(),
            );
            let mut partial = PartialResponse::new(cancel.child());
            let mut stream_err: Option<OpenAIError> = None;
            stream::stream_with_retries(&self.client, &req, cancel, &retrying, |result| {
                match result {
                    Ok(ev) => partial.handle_response_stream_event(ev),
                    Err(e) => stream_err = Some(e),
                }
            })
            .await;

            if cancel.is_cancelled() {
                return Err(EngineError::Cancelled);
            }
            if let Some(e) = stream_err {
                return Err(EngineError::Stream(e));
            }

            let usage = partial.usage;
            let (finish_reason, items) = partial.into_parts();
            match finish_reason {
                Some(ResponseFinishReason::ApiError { code, message, .. }) => {
                    return Err(EngineError::Api { code, message });
                }
                Some(ResponseFinishReason::StreamError(e)) => {
                    return Err(EngineError::Stream(e));
                }
                None => return Err(EngineError::EmptyResponse),
                _ => {}
            }

            // ---- commit outputs to the conversation ----
            let calls: Vec<FunctionToolCall> = items
                .iter()
                .filter_map(|item| match item {
                    OutputItem::FunctionCall(c) => Some(c.clone()),
                    _ => None,
                })
                .collect();
            let assistant_text = message_text(&items);

            for item in items {
                conversation.add_output(item);
            }
            if let Some((input, output)) = usage {
                conversation.add_usage(input, output);
            }
            if !assistant_text.is_empty() {
                surface.on_event(EngineEvent::Assistant(&assistant_text));
            }

            // ---- no tool calls → the turn is done ----
            if calls.is_empty() {
                return Ok(TurnResult {
                    final_text: assistant_text,
                    usage: conversation.accumulated_usage,
                });
            }

            // ---- classify, then run the calls ----
            for call in &calls {
                surface.on_event(EngineEvent::ToolCall { name: &call.name });
            }
            let outputs = self.run_calls(conversation, calls, cancel, surface).await;
            match outputs {
                Some(outputs) => {
                    for out in outputs {
                        conversation.add_tool_output(out);
                    }
                }
                None => return Err(EngineError::Cancelled),
            }
        }

        Err(EngineError::IterationCap(self.max_iterations))
    }

    /// Classify and execute one batch of tool calls, returning the outputs (or
    /// `None` if cancelled). `ask_user` is pre-denied before classification so
    /// it never reaches the executor — in a non-interactive run it would block
    /// forever on a dead answer channel.
    async fn run_calls(
        &self,
        conversation: &Conversation,
        calls: Vec<FunctionToolCall>,
        cancel: &CancellationToken,
        surface: &dyn AgentSurface,
    ) -> Option<Vec<crate::tools::ToolOutput>> {
        // Split off ask_user calls: they cannot be answered headlessly.
        let mut denied: Vec<crate::tools::ToolOutput> = Vec::new();
        let mut classifiable: Vec<FunctionToolCall> = Vec::new();
        for call in calls {
            if call.name == crate::tools::ask_user::NAME {
                denied.push(classify::classifier_denied_output(
                    &call,
                    "ask_user is unavailable in non-interactive mode",
                ));
            } else {
                classifiable.push(call);
            }
        }

        let outcome = match &self.policy {
            EnginePolicy::Yolo => classify::ClassificationOutcome {
                allowed: classifiable,
                denied: Vec::new(),
                ask: Vec::new(),
            },
            EnginePolicy::Sync(classifier) => {
                classify::classify_sync(classifier.as_ref(), &classifiable, cancel)?
            }
            EnginePolicy::Llm(p) => {
                let items: Vec<&crate::response::message_item::MessageItem> =
                    conversation.items().collect();
                let (light, full) = classify::build_classifier_context(&items);
                classify::classify_llm(
                    &p.client,
                    &p.model_name,
                    &p.no_logprobs,
                    &p.mcp_policies,
                    &light,
                    &full,
                    classifiable,
                    cancel,
                )
                .await?
            }
        };

        // Bubble each `Ask` verdict to the surface for a decision. A headless
        // surface denies (folding the classifier's reason into the denial, as
        // before); an interactive surface routes it to the user; a sub-agent's
        // surface forwards it up the tree.
        let mut allowed = outcome.allowed;
        for (call, reason) in outcome.ask {
            if cancel.is_cancelled() {
                return None;
            }
            match surface.review(&call, &reason).await {
                ReviewDecision::Approve => allowed.push(call),
                ReviewDecision::Deny { reason } => {
                    denied.push(classify::classifier_denied_output(&call, &reason))
                }
            }
        }
        denied.extend(outcome.denied);

        // The sender only matters for ask_user, which is already pre-denied, so
        // a throwaway channel (with a dropped receiver) is safe here.
        let (sender, _rx) = tokio::sync::mpsc::unbounded_channel();
        let outputs = tools::run_tool_batch(
            allowed,
            denied,
            cancel.clone(),
            format!("{} auto-approved (headless)", crate::classifier::WorkMode::Auto.icon()),
            sender,
            self.mcp.clone(),
        )
        .await;
        Some(outputs)
    }
}

/// Concatenate the text of every assistant message in `items` (skipping
/// reasoning, refusals, and tool calls).
fn message_text(items: &[OutputItem]) -> String {
    let mut out = String::new();
    for item in items {
        if let OutputItem::Message(msg) = item {
            for content in &msg.content {
                if let OutputMessageContent::OutputText(t) = content {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(&t.text);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::response::message_item::MessageItem;
    use async_openai::types::responses::{
        AssistantRole, FunctionToolCall, InputContent, InputMessage, InputRole,
        MessageItem as ApiMessageItem, OutputMessage, OutputMessageContent as OMC, OutputStatus,
        OutputTextContent,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// One `data: <json>\n\n` SSE frame.
    fn frame(value: serde_json::Value) -> String {
        format!("data: {}\n\n", serde_json::to_string(&value).unwrap())
    }

    /// A `response.completed` frame carrying a minimal valid Response.
    fn completed_frame(seq: u64) -> String {
        frame(serde_json::json!({
            "type": "response.completed",
            "sequence_number": seq,
            "response": {
                "created_at": 0, "id": "resp_1", "model": "mock",
                "object": "response", "output": [], "status": "completed"
            }
        }))
    }

    /// A `response.output_item.added` frame carrying a serialized OutputItem.
    fn item_added_frame(seq: u64, index: u32, item: &OutputItem) -> String {
        frame(serde_json::json!({
            "type": "response.output_item.added",
            "sequence_number": seq,
            "output_index": index,
            "item": item,
        }))
    }

    fn sse_response(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
             Content-Length: {}\r\n\r\n{body}",
            body.len()
        )
    }

    fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }

    /// Mock `/responses` server that serves `bodies[n]` for the n-th request,
    /// clamping at the last body for any request beyond the list.
    async fn spawn_mock_responses(bodies: Vec<String>) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let bodies = Arc::new(bodies);
        let counter = Arc::new(AtomicUsize::new(0));
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { break };
                let bodies = bodies.clone();
                let counter = counter.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
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
                                break;
                            }
                            buf.drain(..body_start + content_length);
                            let n = counter.fetch_add(1, Ordering::SeqCst);
                            let body = &bodies[n.min(bodies.len() - 1)];
                            if sock.write_all(sse_response(body).as_bytes()).await.is_err() {
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
        (format!("http://{addr}"), handle)
    }

    fn engine_for(base_url: &str, max_iterations: usize) -> Engine {
        let client = Client::with_config(OpenAIConfig::new().with_api_base(base_url.to_string()));
        Engine {
            client,
            model_name: "mock".to_string(),
            model_str: "mock/mock".to_string(),
            tools: crate::tools::tools(None),
            policy: EnginePolicy::Yolo,
            mcp: None,
            coauthor: None,
            max_iterations,
        }
    }

    fn call_item(call_id: &str, name: &str, args: &str) -> OutputItem {
        OutputItem::FunctionCall(FunctionToolCall {
            arguments: args.into(),
            call_id: call_id.into(),
            namespace: None,
            name: name.into(),
            id: Some("fc_1".into()),
            status: Some(OutputStatus::Completed),
        })
    }

    fn message_item(text: &str) -> OutputItem {
        OutputItem::Message(OutputMessage {
            content: vec![OMC::OutputText(OutputTextContent {
                annotations: vec![],
                logprobs: None,
                text: text.into(),
            })],
            id: "msg_1".into(),
            role: AssistantRole::Assistant,
            phase: None,
            status: OutputStatus::Completed,
        })
    }

    fn user(text: &str) -> ApiMessageItem {
        ApiMessageItem::Input(InputMessage {
            content: vec![InputContent::InputText(text.into())],
            role: InputRole::User,
            status: Some(OutputStatus::Completed),
        })
    }

    #[tokio::test]
    async fn run_turn_drives_a_tool_call_then_final_text() {
        // Response 1: a `read_file` tool call. Response 2: the final message.
        let body1 = format!(
            "{}{}",
            item_added_frame(1, 0, &call_item("c1", "read_file", "{\"path\":\"Cargo.toml\"}")),
            completed_frame(2),
        );
        let body2 = format!(
            "{}{}",
            item_added_frame(1, 0, &message_item("all done")),
            completed_frame(2),
        );
        let (base, _server) = spawn_mock_responses(vec![body1, body2]).await;

        let engine = engine_for(&base, 10);
        let mut conv = Conversation::new();
        conv.add_input_message(user("read the manifest"));
        let cancel = CancellationToken::new();
        let result = engine
            .run_turn(&mut conv, &cancel, &HeadlessSurface)
            .await
            .expect("turn completes");

        assert_eq!(result.final_text, "all done");
        // History order: user, call, tool output, final message.
        let kinds: Vec<&str> = conv
            .items()
            .map(|it| match it {
                MessageItem::Input(_) => "input",
                MessageItem::Output(OutputItem::FunctionCall(_)) => "call",
                MessageItem::Output(OutputItem::Message(_)) => "message",
                MessageItem::ToolOutput { .. } => "output",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, vec!["input", "call", "output", "message"]);
    }

    #[tokio::test]
    async fn run_turn_pre_denies_ask_user_and_continues() {
        // Response 1 calls ask_user (which must be denied, not hang).
        // Response 2 finishes.
        let body1 = format!(
            "{}{}",
            item_added_frame(1, 0, &call_item("a1", "ask_user", "{\"question\":\"?\"}")),
            completed_frame(2),
        );
        let body2 = format!(
            "{}{}",
            item_added_frame(1, 0, &message_item("done anyway")),
            completed_frame(2),
        );
        let (base, _server) = spawn_mock_responses(vec![body1, body2]).await;

        let engine = engine_for(&base, 10);
        let mut conv = Conversation::new();
        conv.add_input_message(user("hi"));
        let cancel = CancellationToken::new();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            engine.run_turn(&mut conv, &cancel, &HeadlessSurface),
        )
        .await
        .expect("must not hang on ask_user")
        .expect("turn completes");
        assert_eq!(result.final_text, "done anyway");
        // The ask_user call got a denial tool output.
        let denied = conv.items().any(|it| matches!(
            it,
            MessageItem::ToolOutput { output, failed: true, .. }
                if matches!(&output.output,
                    async_openai::types::responses::FunctionCallOutput::Text(t)
                        if t.contains("non-interactive"))
        ));
        assert!(denied, "ask_user should be denied");
    }

    #[tokio::test]
    async fn run_turn_hits_the_iteration_cap() {
        // Every response calls a tool, so the loop never terminates on its own.
        let body = format!(
            "{}{}",
            item_added_frame(1, 0, &call_item("c1", "read_file", "{\"path\":\"Cargo.toml\"}")),
            completed_frame(2),
        );
        let (base, _server) = spawn_mock_responses(vec![body]).await;
        let engine = engine_for(&base, 3);
        let mut conv = Conversation::new();
        conv.add_input_message(user("loop"));
        let cancel = CancellationToken::new();
        let err = engine.run_turn(&mut conv, &cancel, &HeadlessSurface).await.unwrap_err();
        assert!(matches!(err, EngineError::IterationCap(3)), "got {err:?}");
    }

    /// A surface that decides every `review` the same way and records the
    /// notifications it receives — a stand-in for the TUI / a parent agent.
    struct TestSurface {
        approve: bool,
        events: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl AgentSurface for TestSurface {
        fn on_event(&self, event: EngineEvent<'_>) {
            let label = match event {
                EngineEvent::Assistant(t) => format!("assistant:{t}"),
                EngineEvent::ToolCall { name } => format!("tool:{name}"),
            };
            self.events.lock().unwrap().push(label);
        }
        async fn review(&self, _call: &FunctionToolCall, reason: &str) -> ReviewDecision {
            if self.approve {
                ReviewDecision::Approve
            } else {
                ReviewDecision::Deny {
                    reason: format!("surface refused: {reason}"),
                }
            }
        }
    }

    fn sync_engine(policy_mode: crate::classifier::WorkMode) -> Engine {
        // A base_url is required to build the client, but `run_calls` never
        // streams, so any address works.
        let client = Client::with_config(OpenAIConfig::new().with_api_base("http://127.0.0.1:1"));
        Engine {
            client,
            model_name: "mock".to_string(),
            model_str: "mock/mock".to_string(),
            tools: crate::tools::tools(None),
            policy: EnginePolicy::Sync(policy_mode.classifier(std::collections::HashMap::new())),
            mcp: None,
            coauthor: None,
            max_iterations: 10,
        }
    }

    #[tokio::test]
    async fn ask_verdict_bubbles_to_surface_approve_runs_the_call() {
        // Manual mode asks about write_file; an approving surface lets it run.
        let tmp = std::env::temp_dir().join(format!("engine_review_ok_{}", std::process::id()));
        let path = tmp.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&tmp);

        let engine = sync_engine(crate::classifier::WorkMode::Manual);
        let conv = Conversation::new();
        let cancel = CancellationToken::new();
        let surface = TestSurface {
            approve: true,
            events: std::sync::Mutex::new(Vec::new()),
        };
        let calls = vec![FunctionToolCall {
            arguments: format!("{{\"path\":\"{path}\",\"content\":\"surfaced\"}}"),
            call_id: "w1".into(),
            namespace: None,
            name: "write_file".into(),
            id: None,
            status: None,
        }];
        let outputs = engine
            .run_calls(&conv, calls, &cancel, &surface)
            .await
            .expect("not cancelled");

        assert_eq!(outputs.len(), 1);
        assert!(!outputs[0].failed, "approved write should succeed");
        assert_eq!(
            std::fs::read_to_string(&tmp).unwrap_or_default(),
            "surfaced",
            "the approved write actually ran"
        );
        let _ = std::fs::remove_file(&tmp);
    }

    #[tokio::test]
    async fn ask_verdict_bubbles_to_surface_deny_blocks_the_call() {
        // The same call, but a refusing surface turns it into a denial and the
        // file is never written.
        let tmp = std::env::temp_dir().join(format!("engine_review_no_{}", std::process::id()));
        let path = tmp.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&tmp);

        let engine = sync_engine(crate::classifier::WorkMode::Manual);
        let conv = Conversation::new();
        let cancel = CancellationToken::new();
        let surface = TestSurface {
            approve: false,
            events: std::sync::Mutex::new(Vec::new()),
        };
        let calls = vec![FunctionToolCall {
            arguments: format!("{{\"path\":\"{path}\",\"content\":\"surfaced\"}}"),
            call_id: "w1".into(),
            namespace: None,
            name: "write_file".into(),
            id: None,
            status: None,
        }];
        let outputs = engine
            .run_calls(&conv, calls, &cancel, &surface)
            .await
            .expect("not cancelled");

        assert_eq!(outputs.len(), 1);
        assert!(outputs[0].failed, "denied write is a failed output");
        let denial = match &outputs[0].param.output {
            async_openai::types::responses::FunctionCallOutput::Text(t) => t.clone(),
            _ => String::new(),
        };
        assert!(denial.contains("surface refused"), "carries surface reason: {denial}");
        assert!(!tmp.exists(), "the denied write never ran");
    }
}
