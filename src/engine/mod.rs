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
use crate::response::message_item::MessageItem;
use crate::response::partial_response::PartialResponse;
use crate::response::response_finish_reason::ResponseFinishReason;
use async_openai::Client;
use async_openai::config::OpenAIConfig;
use async_openai::error::OpenAIError;
use async_openai::types::responses::{
    FunctionToolCall, OutputItem, OutputMessageContent, ResponseStreamEvent, Tool,
};
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
    /// Post-edit diagnostics feedback, driven inside the turn loop. Off by
    /// default, so `-p` runs stay lean.
    pub diagnostics: DiagnosticsFeedback,
    /// Set while the stream layer is retrying a dropped connection; shared so
    /// a front-end can show a "retrying" indicator.
    pub stream_retrying: Arc<AtomicBool>,
}

/// Configuration and running state for the engine's post-edit feedback: after a
/// tool batch that edited files, run the configured diagnostics checkers, diff
/// against the baseline, and inject the delta (plus periodic PROGRAMMER.md
/// reminders) back into the conversation — the same behaviour the TUI's
/// post-edit pipeline provided, now owned by the shared loop. When `enabled` is
/// false none of it runs, and the turn resumes straight after the tool outputs.
///
/// The mutable [`DiagnosticsState`] lives behind an `Arc<Mutex<_>>` so it
/// survives across turns even though the TUI builds a fresh `Engine` per turn:
/// the front-end holds the state and hands each turn's engine a clone.
#[derive(Default, Clone)]
pub(crate) struct DiagnosticsFeedback {
    /// Master switch. The TUI turns this on; `-p` leaves it off (for now).
    pub enabled: bool,
    /// Emit a PROGRAMMER.md refresh reminder every N file-editing turns, when set.
    pub reminder_every: Option<usize>,
    /// Baseline + edit-turn counter, shared so it persists across per-turn engines.
    pub state: Arc<Mutex<DiagnosticsState>>,
}

/// The cross-turn mutable state behind [`DiagnosticsFeedback`].
#[derive(Default)]
pub(crate) struct DiagnosticsState {
    /// The last diagnostics snapshot to diff against; `None` until the first run
    /// establishes the baseline.
    pub baseline: Option<Vec<crate::diagnostics::Diagnostic>>,
    /// File-editing turns seen so far, driving the reminder cadence.
    pub mutating_turns: usize,
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

/// A coarse turn phase, surfaced so a front-end can show a status indicator.
/// Mirrors the TUI's `ActivePhase`; the headless surface ignores it.
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub(crate) enum EnginePhase {
    /// Streaming a model response.
    Streaming,
    /// Classifying the requested tool calls.
    Classifying,
    /// Executing approved tool calls.
    RunningTools,
    /// Running post-edit diagnostics.
    Checking,
}

/// Progress events emitted during a turn. Print mode ignores these; the TUI
/// surface renders them; sub-agents forward them up. Because the conversation
/// is shared with the front-end, *committed* history is read straight from it —
/// these events carry only the in-flight/ephemeral state that isn't in the
/// conversation yet (live stream tokens, phase, the commit boundary).
#[allow(dead_code)]
pub(crate) enum EngineEvent<'a> {
    /// A raw streaming chunk of the in-flight response, for live token
    /// rendering before the response is committed. Boxed because a
    /// `ResponseStreamEvent` dwarfs the other variants.
    StreamChunk(Box<ResponseStreamEvent>),
    /// The streamed response's items were just committed to the shared
    /// conversation; a live renderer should now drop its in-progress view so
    /// the same content isn't shown twice.
    ResponseCommitted,
    /// The model produced assistant text this iteration.
    Assistant(&'a str),
    /// A tool call is about to run.
    ToolCall { name: &'a str },
    /// The turn moved to a new phase.
    Phase(EnginePhase),
}

impl Engine {
    /// Run a full turn: stream a response, run any tool calls it requests, and
    /// loop until the model answers with no tool calls (returning its text) or
    /// an error/cap is hit. `conversation` is mutated in place with every
    /// output and tool result, exactly as the TUI would record them.
    pub(crate) async fn run_turn(
        &self,
        conversation: &Mutex<Conversation>,
        cancel: &CancellationToken,
        surface: &dyn AgentSurface,
    ) -> Result<TurnResult, EngineError> {
        let retrying = &self.stream_retrying;

        for _ in 0..self.max_iterations {
            if cancel.is_cancelled() {
                return Err(EngineError::Cancelled);
            }

            // ---- stream one response, folding it locally ----
            // The conversation is shared with the front-end (the TUI renders it
            // every frame), so every touch takes a brief lock and releases it
            // before the next await — a guard held across `.await` would make
            // this future non-`Send` and fail to compile under `tokio::spawn`,
            // which is exactly the discipline we want enforced.
            let skill_prompt = surface.skill_prompt();
            let req = {
                let ctx = request::SystemContext {
                    current_model: &self.model_str,
                    skill_prompt: skill_prompt.as_deref(),
                    plan_prompt: surface.plan_prompt(),
                    coauthor: self.coauthor.as_deref(),
                };
                let conv = conversation.lock().unwrap();
                request::build_request(&conv, &ctx, self.model_name.clone(), self.tools.clone())
            };
            surface.on_event(EngineEvent::Phase(EnginePhase::Streaming));
            let mut partial = PartialResponse::new(cancel.child());
            let mut stream_err: Option<OpenAIError> = None;
            stream::stream_with_retries(&self.client, &req, cancel, retrying, |result| {
                match result {
                    Ok(ev) => {
                        // Forward each chunk for live rendering, then fold it
                        // into our own partial to extract the committed items.
                        surface.on_event(EngineEvent::StreamChunk(Box::new(ev.clone())));
                        partial.handle_response_stream_event(ev);
                    }
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

            {
                let mut conv = conversation.lock().unwrap();
                for item in items {
                    conv.add_output(item);
                }
                if let Some((input, output)) = usage {
                    conv.add_usage(input, output);
                }
            }
            // Committed to the shared conversation: tell a live renderer to drop
            // its in-progress view now (after the commit, so there is no frame
            // where neither the in-flight nor the committed copy shows).
            surface.on_event(EngineEvent::ResponseCommitted);
            if !assistant_text.is_empty() {
                surface.on_event(EngineEvent::Assistant(&assistant_text));
            }

            // ---- no tool calls → the turn is done ----
            if calls.is_empty() {
                let usage = conversation.lock().unwrap().accumulated_usage;
                return Ok(TurnResult {
                    final_text: assistant_text,
                    usage,
                });
            }

            // ---- classify, then run the calls ----
            for call in &calls {
                surface.on_event(EngineEvent::ToolCall { name: &call.name });
            }
            // Remember which calls in this batch are file edits, so we know
            // afterwards whether to run post-edit diagnostics.
            let edit_call_ids: HashSet<String> = if self.diagnostics.enabled {
                calls
                    .iter()
                    .filter(|c| {
                        c.name == crate::tools::write_file::NAME
                            || c.name == crate::tools::edit_file::NAME
                    })
                    .map(|c| c.call_id.clone())
                    .collect()
            } else {
                HashSet::new()
            };
            let outputs = self.run_calls(conversation, calls, cancel, surface).await;
            match outputs {
                Some(outputs) => {
                    let edited = self.diagnostics.enabled
                        && outputs
                            .iter()
                            .any(|o| !o.failed && edit_call_ids.contains(&o.param.call_id));
                    {
                        let mut conv = conversation.lock().unwrap();
                        for out in outputs {
                            conv.add_tool_output(out);
                        }
                    }
                    if edited {
                        self.run_post_edit_diagnostics(conversation, surface).await;
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
        conversation: &Mutex<Conversation>,
        calls: Vec<FunctionToolCall>,
        cancel: &CancellationToken,
        surface: &dyn AgentSurface,
    ) -> Option<Vec<crate::tools::ToolOutput>> {
        // ask_user needs an interactive front-end to answer it. A surface that
        // provides a tool-event channel (the TUI) can; without one (headless),
        // pre-deny it so it doesn't hang forever on a dead answer channel.
        let tool_sender = surface.tool_event_sender();
        let mut denied: Vec<crate::tools::ToolOutput> = Vec::new();
        let mut classifiable: Vec<FunctionToolCall> = Vec::new();
        for call in calls {
            if call.name == crate::tools::ask_user::NAME && tool_sender.is_none() {
                denied.push(classify::classifier_denied_output(
                    &call,
                    "ask_user is unavailable in non-interactive mode",
                ));
            } else {
                classifiable.push(call);
            }
        }

        surface.on_event(EngineEvent::Phase(EnginePhase::Classifying));
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
                let (light, full) = {
                    let conv = conversation.lock().unwrap();
                    let items: Vec<&MessageItem> = conv.items().collect();
                    classify::build_classifier_context(&items)
                };
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
        let ask_total = outcome.ask.len();
        for (idx, (call, reason)) in outcome.ask.into_iter().enumerate() {
            if cancel.is_cancelled() {
                return None;
            }
            match surface.review(&call, &reason, (idx + 1, ask_total)).await {
                ReviewDecision::Approve => allowed.push(call),
                ReviewDecision::Deny { output } => denied.push(output),
            }
        }
        denied.extend(outcome.denied);

        surface.on_event(EngineEvent::Phase(EnginePhase::RunningTools));
        // Use the front-end's tool channel when it has one (so ask_user and live
        // task updates reach the UI); otherwise a throwaway channel with a
        // dropped receiver — safe because ask_user is already pre-denied there.
        let sender = tool_sender.unwrap_or_else(|| tokio::sync::mpsc::unbounded_channel().0);
        let outputs = tools::run_tool_batch(
            allowed,
            denied,
            cancel.clone(),
            surface.approval_label(),
            sender,
            self.mcp.clone(),
        )
        .await;
        Some(outputs)
    }

    /// After a file-editing batch, run the diagnostics checkers (if a profile is
    /// configured), diff against the running baseline, and inject the delta —
    /// plus a periodic PROGRAMMER.md reminder when due — back into the
    /// conversation so the model reacts to the errors it just introduced. The
    /// feedback is appended to the most recent editing tool's output when one is
    /// present (so it renders inside that call) and otherwise added as its own
    /// system note. Mirrors the TUI's old `continue_with_diagnostics` +
    /// `apply_diagnostics`, but sequential inside the loop rather than spawned.
    async fn run_post_edit_diagnostics(
        &self,
        conversation: &Mutex<Conversation>,
        surface: &dyn AgentSurface,
    ) {
        let mutating_turns = {
            let mut st = self.diagnostics.state.lock().unwrap();
            st.mutating_turns += 1;
            st.mutating_turns
        };
        let reminder_due = self
            .diagnostics
            .reminder_every
            .is_some_and(|n| n != 0 && mutating_turns.is_multiple_of(n))
            && std::path::Path::new("PROGRAMMER.md").exists();

        let mut parts: Vec<String> = Vec::new();

        if std::path::Path::new(crate::diagnostics::PROFILE_PATH).exists() {
            surface.on_event(EngineEvent::Phase(EnginePhase::Checking));
            let cwd = std::env::current_dir()
                .unwrap_or_else(|_| std::path::Path::new(".").to_path_buf());
            // No state lock held across this await — see the run_turn comment.
            let snapshot = crate::diagnostics::collect(&cwd).await.unwrap_or_default();
            let mut st = self.diagnostics.state.lock().unwrap();
            match &st.baseline {
                Some(old) => {
                    if let Some(summary) =
                        crate::diagnostics::diff(old, &snapshot.diagnostics).summary()
                    {
                        parts.push(summary);
                    }
                }
                None => {
                    if !snapshot.diagnostics.is_empty() {
                        parts.push(format!(
                            "Diagnostics baseline established: {} problem(s) currently \
                             in the project. Future edits will report changes relative \
                             to this.",
                            snapshot.diagnostics.len()
                        ));
                    }
                }
            }
            for e in &snapshot.errors {
                parts.push(format!("Diagnostics checker error: {e}"));
            }
            st.baseline = Some(snapshot.diagnostics);
        }

        if reminder_due {
            parts.push(crate::prompts::OVERVIEW_REMINDER.to_string());
        }

        if parts.is_empty() {
            return;
        }
        inject_post_edit_feedback(conversation, &parts.join("\n\n"));
    }
}

/// Attach post-edit feedback `text` to `conversation`: appended inside the most
/// recent editing tool's output when one is present (so it renders as part of
/// that call's result and is sent back with it), otherwise added as its own
/// system note.
fn inject_post_edit_feedback(conversation: &Mutex<Conversation>, text: &str) {
    let mut conv = conversation.lock().unwrap();
    let call_id = last_edit_output_call_id(&conv);
    if let Some(call_id) = call_id {
        let block = format!("\n\n--- Post-edit check ---\n{text}");
        if conv.append_to_tool_output(&call_id, &block) {
            return;
        }
    }
    conv.add_meta("\u{25B8} System", text);
}

/// The call id of the most recent file-editing tool output in `conversation`, so
/// post-edit feedback can be appended inside that call's result.
fn last_edit_output_call_id(conversation: &Conversation) -> Option<String> {
    let names: std::collections::HashMap<&str, &str> = conversation
        .items()
        .filter_map(|it| match it {
            MessageItem::Output(OutputItem::FunctionCall(fc)) => {
                Some((fc.call_id.as_str(), fc.name.as_str()))
            }
            _ => None,
        })
        .collect();
    conversation
        .items()
        .filter_map(|it| match it {
            MessageItem::ToolOutput { output, .. } => {
                let name = names.get(output.call_id.as_str()).copied();
                matches!(
                    name,
                    Some(crate::tools::write_file::NAME) | Some(crate::tools::edit_file::NAME)
                )
                .then(|| output.call_id.clone())
            }
            _ => None,
        })
        .last()
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
            diagnostics: DiagnosticsFeedback::default(),
            stream_retrying: Arc::new(AtomicBool::new(false)),
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
        let mut c = Conversation::new();
        c.add_input_message(user("read the manifest"));
        let conv = Mutex::new(c);
        let cancel = CancellationToken::new();
        let result = engine
            .run_turn(&conv, &cancel, &HeadlessSurface)
            .await
            .expect("turn completes");

        assert_eq!(result.final_text, "all done");
        // History order: user, call, tool output, final message.
        let kinds: Vec<&str> = conv
            .lock()
            .unwrap()
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
        let mut c = Conversation::new();
        c.add_input_message(user("hi"));
        let conv = Mutex::new(c);
        let cancel = CancellationToken::new();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            engine.run_turn(&conv, &cancel, &HeadlessSurface),
        )
        .await
        .expect("must not hang on ask_user")
        .expect("turn completes");
        assert_eq!(result.final_text, "done anyway");
        // The ask_user call got a denial tool output.
        let denied = conv.lock().unwrap().items().any(|it| matches!(
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
        let mut c = Conversation::new();
        c.add_input_message(user("loop"));
        let conv = Mutex::new(c);
        let cancel = CancellationToken::new();
        let err = engine.run_turn(&conv, &cancel, &HeadlessSurface).await.unwrap_err();
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
                // Ephemeral progress events aren't asserted on in these tests.
                EngineEvent::StreamChunk(_)
                | EngineEvent::ResponseCommitted
                | EngineEvent::Phase(_) => return,
            };
            self.events.lock().unwrap().push(label);
        }
        async fn review(
            &self,
            call: &FunctionToolCall,
            reason: &str,
            _position: (usize, usize),
        ) -> ReviewDecision {
            if self.approve {
                ReviewDecision::Approve
            } else {
                ReviewDecision::Deny {
                    output: classify::classifier_denied_output(
                        call,
                        &format!("surface refused: {reason}"),
                    ),
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
            diagnostics: DiagnosticsFeedback::default(),
            stream_retrying: Arc::new(AtomicBool::new(false)),
        }
    }

    #[tokio::test]
    async fn ask_verdict_bubbles_to_surface_approve_runs_the_call() {
        // Manual mode asks about write_file; an approving surface lets it run.
        let tmp = std::env::temp_dir().join(format!("engine_review_ok_{}", std::process::id()));
        let path = tmp.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&tmp);

        let engine = sync_engine(crate::classifier::WorkMode::Manual);
        let conv = Mutex::new(Conversation::new());
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
        let conv = Mutex::new(Conversation::new());
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

    #[test]
    fn post_edit_feedback_appends_inside_the_edit_output() {
        use async_openai::types::responses::{FunctionCallOutput, FunctionCallOutputItemParam};
        let mut c = Conversation::new();
        c.add_output(OutputItem::FunctionCall(FunctionToolCall {
            arguments: "{}".into(),
            call_id: "e1".into(),
            namespace: None,
            name: "write_file".into(),
            id: None,
            status: None,
        }));
        c.add_tool_output(crate::tools::ToolOutput {
            param: FunctionCallOutputItemParam {
                call_id: "e1".into(),
                output: FunctionCallOutput::Text("wrote file".into()),
                id: None,
                status: None,
            },
            failed: false,
            approval_label: None,
        });
        let conv = Mutex::new(c);

        inject_post_edit_feedback(&conv, "2 new errors");

        let out = conv
            .lock()
            .unwrap()
            .items()
            .find_map(|it| match it {
                MessageItem::ToolOutput { output, .. } if output.call_id == "e1" => {
                    match &output.output {
                        FunctionCallOutput::Text(t) => Some(t.clone()),
                        _ => None,
                    }
                }
                _ => None,
            })
            .expect("edit output present");
        assert!(out.contains("wrote file"), "keeps the original output: {out}");
        assert!(out.contains("--- Post-edit check ---"), "adds the header: {out}");
        assert!(out.contains("2 new errors"), "carries the feedback: {out}");
        // Folded into the output, so no separate system note.
        assert!(!conv.lock().unwrap().items().any(|it| matches!(it, MessageItem::Meta { .. })));
    }

    #[test]
    fn post_edit_feedback_without_an_edit_output_adds_a_system_note() {
        let mut c = Conversation::new();
        c.add_input_message(user("hi"));
        let conv = Mutex::new(c);
        inject_post_edit_feedback(&conv, "reminder text");
        let meta = conv
            .lock()
            .unwrap()
            .items()
            .find_map(|it| match it {
                MessageItem::Meta { text, .. } => Some(text.clone()),
                _ => None,
            })
            .expect("a system note was added");
        assert_eq!(meta, "reminder text");
    }

    #[tokio::test]
    async fn disabled_diagnostics_inject_nothing_after_an_edit() {
        // With the feedback switch off (the default, i.e. `-p`), a write_file
        // batch must leave the conversation exactly as before: no system note
        // and no "Post-edit check" appended to the write output.
        let tmp = std::env::temp_dir().join(format!("engine_diag_off_{}", std::process::id()));
        let path = tmp.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&tmp);

        let body1 = format!(
            "{}{}",
            item_added_frame(
                1,
                0,
                &call_item("w1", "write_file", &format!("{{\"path\":\"{path}\",\"content\":\"x\"}}")),
            ),
            completed_frame(2),
        );
        let body2 = format!(
            "{}{}",
            item_added_frame(1, 0, &message_item("done")),
            completed_frame(2),
        );
        let (base, _server) = spawn_mock_responses(vec![body1, body2]).await;

        let engine = engine_for(&base, 10); // Yolo; diagnostics disabled by default.
        let mut c = Conversation::new();
        c.add_input_message(user("write it"));
        let conv = Mutex::new(c);
        let cancel = CancellationToken::new();
        engine
            .run_turn(&conv, &cancel, &HeadlessSurface)
            .await
            .expect("turn completes");

        assert!(
            !conv.lock().unwrap().items().any(|it| matches!(it, MessageItem::Meta { .. })),
            "no system note when diagnostics feedback is off"
        );
        let out = conv
            .lock()
            .unwrap()
            .items()
            .find_map(|it| match it {
                MessageItem::ToolOutput { output, .. } if output.call_id == "w1" => {
                    match &output.output {
                        async_openai::types::responses::FunctionCallOutput::Text(t) => {
                            Some(t.clone())
                        }
                        _ => None,
                    }
                }
                _ => None,
            })
            .unwrap_or_default();
        assert!(!out.contains("Post-edit check"), "nothing appended: {out}");
        let _ = std::fs::remove_file(&tmp);
    }
}
