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

//! The UI-free agent runner: the turn primitives (request building, streaming,
//! classification, tool execution) that both the TUI event loop and a headless
//! driver share, plus the driver itself. Extracted incrementally from the
//! `app` event handlers so the TUI keeps working unchanged while the same logic
//! becomes reusable for headless CLI commands and, later, in-process sub-agents.

mod argument_guard;
pub(crate) mod classify;
pub(crate) mod hooks;
pub(crate) mod request;
pub(crate) mod stream;
pub(crate) mod surface;
pub(crate) mod tools;

#[cfg(test)]
pub(crate) use surface::HeadlessSurface;
pub(crate) use surface::{AgentSurface, ReviewDecision};

use crate::cancel::CancellationToken;
use crate::classifier::Classifier;
use crate::conversation::Conversation;
use crate::response::message_item::MessageItem;
use crate::response::partial_response::PartialResponse;
use crate::response::response_finish_reason::ResponseFinishReason;
use async_openai::Client;
use async_openai::config::OpenAIConfig;
use async_openai::error::OpenAIError;
use async_openai::types::responses::{
    FunctionToolCall, InputContent, InputMessage, InputRole, MessageItem as ApiMessageItem,
    OutputItem, OutputMessageContent, OutputStatus, ResponseStreamEvent,
};
use std::collections::HashSet;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

/// The Auto-mode LLM classifier's inputs, boxed into [`RunnerPolicy::Llm`] so
/// that variant isn't far larger than the others.
#[derive(Clone)]
pub(crate) struct LlmPolicy {
    pub client: Client<OpenAIConfig>,
    pub model_name: String,
    pub top_logprobs: u8,
    pub no_logprobs: Arc<Mutex<HashSet<String>>>,
}

/// How the runner decides whether a tool call may run. The TUI keeps its own
/// `WorkMode` and maps per-classification; this is the runner-level equivalent
/// for headless callers.
pub(crate) enum RunnerPolicy {
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

/// A headless agent runner: everything needed to run a turn to completion with
/// no UI. Reused by CLI headless runs and, later, in-process sub-agents.
pub(crate) struct TurnRunner {
    /// Client + resolved model id for the chat/completion calls.
    pub client: Client<OpenAIConfig>,
    pub model_name: String,
    /// `provider/model` string, named in the system-prompt banner.
    pub model_str: String,
    /// Every tool source behind one interface: the local built-ins and the
    /// connected MCP servers, each a provider. Supplies the advertised list, the
    /// read-only / interaction metadata, and call routing.
    pub tools: Arc<crate::tools::provider::ToolRegistry>,
    pub policy: RunnerPolicy,
    pub soul: Option<String>,
    pub coauthor: Option<String>,
    /// Include stored image parts in API requests. When false, the conversation
    /// preserves them but sends textual placeholders instead.
    pub vision_enabled: bool,
    /// Reasoning effort for main chat requests.
    pub thinking_level: crate::thinking::ThinkingLevel,
    /// Dedicated model used for automatic memory association before each user turn.
    pub memory_model: Option<crate::tools::memory::MemoryModel>,
    /// Pluggable turn hooks (post-edit diagnostics, the PROGRAMMER.md reminder,
    /// and any future check) run around each tool batch.
    pub hooks: Vec<Arc<dyn hooks::TurnHook>>,
    /// Set while the stream layer is retrying a dropped connection; shared so
    /// a front-end can show a "retrying" indicator.
    pub stream_retrying: Arc<AtomicBool>,
    /// Maximum retries performed inside one model request before returning an error.
    pub stream_retry_limit: u32,
    /// Maximum number of model responses in one turn. A tool-call response
    /// consumes one step; `None` leaves the agent loop unbounded.
    pub max_steps: Option<usize>,
}

pub(crate) use crate::diagnostics::DiagnosticsState;

/// The result of a completed turn.
#[derive(Debug)]
pub struct TurnResult {
    pub final_text: String,
    #[allow(dead_code)]
    pub usage: (u32, u32, u32),
}

/// Why a turn could not complete.
#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    #[error("stream error: {0}")]
    Stream(OpenAIError),
    #[error("api error {code:?}: {message}")]
    Api {
        code: Option<String>,
        message: String,
    },
    #[error("cancelled")]
    Cancelled,
    #[error("the model returned no output")]
    EmptyResponse,
    #[error("maximum model response count reached ({limit})")]
    StepLimit { limit: usize },
}

/// A coarse turn phase, surfaced so a front-end can show a status indicator.
/// Mirrors the TUI's `ActivePhase`; the headless surface ignores it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum RunnerPhase {
    /// Streaming a model response.
    Streaming,
    /// Classifying the requested tool calls.
    Classifying,
    /// Executing approved tool calls.
    RunningTools,
    /// A dedicated memory model is associating the query with stored memories.
    Associating,
    /// Running post-edit diagnostics.
    Checking,
}

impl RunnerPhase {
    /// Short lowercase label for compact spaces such as a sidebar row, where
    /// the status bar's icon-and-title wording would not fit.
    pub fn label(self) -> &'static str {
        match self {
            Self::Streaming => "streaming",
            Self::Classifying => "evaluating",
            Self::RunningTools => "running tools",
            Self::Associating => "associating",
            Self::Checking => "checking",
        }
    }
}

/// Progress events emitted during a turn. Print mode ignores these; the TUI
/// surface renders them; sub-agents forward them up. Because the conversation
/// is shared with the front-end, *committed* history is read straight from it —
/// these events carry only the in-flight/ephemeral state that isn't in the
/// conversation yet (live stream tokens, phase, the commit boundary).
#[allow(dead_code)]
pub(crate) enum RunnerEvent<'a> {
    /// A raw streaming chunk of the in-flight response, for live token
    /// rendering before the response is committed. Boxed because a
    /// `ResponseStreamEvent` dwarfs the other variants.
    StreamChunk(Box<ResponseStreamEvent>),
    /// The streamed response's items were just committed to the shared
    /// conversation; a live renderer should now drop its in-progress view so
    /// the same content isn't shown twice.
    ResponseCommitted,
    /// Interrupted output was archived by the runner. Indices address the
    /// compact live items retained at the given absolute history boundary.
    ResponseAborted {
        start: usize,
        retained_indices: Vec<usize>,
    },
    /// The model produced assistant text this iteration.
    Assistant(&'a str),
    /// A tool call is about to run.
    ToolCall { name: &'a str },
    /// The turn moved to a new phase.
    Phase(RunnerPhase),
    /// More precise work boundary, including waits after the final response.
    Activity(&'a str),
    /// A recoverable problem worth telling the user about, such as a memory
    /// association that failed and therefore recalled nothing. Front-ends
    /// render it as an informational line; it never stops the turn.
    Notice(&'a str),
    /// A completed API response reported its real input token count and all
    /// function calls from that response (if any) now have paired outputs.
    UsageSafePoint { input_tokens: u32 },
    /// The main runner entered or left a blocking `agent wait` call.
    WaitingSubagents(bool),
}

/// When a turn is cancelled after assistant function calls have already been
/// committed to the conversation, add exactly one synthetic "cancelled" tool
/// output per unmatched call so the API message ordering stays valid (every
/// function-call item must be followed by a matching tool-output item before
/// the next request).
struct MemoryAssociationTask {
    handle: Option<tokio::task::JoinHandle<Result<Vec<crate::memory::MemoryEntry>, String>>>,
}

impl MemoryAssociationTask {
    fn is_finished(&self) -> bool {
        self.handle
            .as_ref()
            .is_none_or(tokio::task::JoinHandle::is_finished)
    }
}

impl Drop for MemoryAssociationTask {
    fn drop(&mut self) {
        if let Some(handle) = &self.handle {
            handle.abort();
        }
    }
}

enum AssociationPoll {
    Cancelled,
    Pending,
    Finished(Result<Vec<crate::memory::MemoryEntry>, String>),
}

fn spawn_memory_association(
    model: crate::tools::memory::MemoryModel,
    context: String,
    query: String,
    excluded: std::collections::HashSet<String>,
) -> MemoryAssociationTask {
    MemoryAssociationTask {
        handle: Some(tokio::spawn(async move {
            model.associate(&context, &query, &excluded).await
        })),
    }
}

async fn poll_memory_association(
    task: &mut MemoryAssociationTask,
    grace: std::time::Duration,
    cancel: &CancellationToken,
) -> AssociationPoll {
    let Some(handle) = task.handle.as_mut() else {
        return AssociationPoll::Pending;
    };
    if !handle.is_finished() && grace.is_zero() {
        return AssociationPoll::Pending;
    }
    let joined = if handle.is_finished() {
        task.handle.take().unwrap().await
    } else {
        match cancel.wait_or(tokio::time::timeout(grace, handle)).await {
            None => return AssociationPoll::Cancelled,
            Some(Ok(joined)) => {
                // The borrowed handle has completed; remove it so Drop does
                // not retain or abort a finished task.
                task.handle.take();
                joined
            }
            Some(Err(_)) => return AssociationPoll::Pending,
        }
    };
    AssociationPoll::Finished(
        joined.unwrap_or_else(|error| Err(format!("memory association task failed: {error}"))),
    )
}

fn ensure_tool_output_pairing(conversation: &Mutex<Conversation>) {
    use async_openai::types::responses::{FunctionCallOutput, OutputItem};
    use std::collections::HashSet;

    let mut conv = conversation.lock().unwrap();
    // Collect call_ids from committed function-call items.
    let call_ids: HashSet<String> = conv
        .items()
        .filter_map(|item| match item {
            MessageItem::Output(OutputItem::FunctionCall(c)) => Some(c.call_id.clone()),
            _ => None,
        })
        .collect();
    // Collect call_ids that already have a tool output.
    let matched: HashSet<String> = conv
        .items()
        .filter_map(|item| match item {
            MessageItem::ToolOutput { output, .. } => Some(output.call_id.clone()),
            _ => None,
        })
        .collect();
    // For each unmatched call, inject a synthetic cancelled output.
    for call_id in call_ids.difference(&matched) {
        conv.add_tool_output(crate::tools::ToolOutput {
            param: async_openai::types::responses::FunctionCallOutputItemParam {
                call_id: call_id.clone(),
                output: FunctionCallOutput::Text("cancelled".to_string()),
                id: None,
                status: None,
            },
            failed: true,
            approval_label: Some("\u{2716} cancelled by user".to_string()),
        });
    }
}

/// Archive interrupted output before notifying any renderer. The UI copy is
/// never a source of conversation data, including when cancellation races a chunk.
fn commit_aborted_response(
    conversation: &Mutex<Conversation>,
    partial: PartialResponse,
    cancelled: bool,
    surface: &dyn AgentSurface,
) {
    let retained_indices = partial
        .message_item_refs()
        .iter()
        .enumerate()
        .filter_map(|(index, (item, in_progress))| {
            (!matches!(item, OutputItem::FunctionCall(_)) || (!cancelled && !in_progress))
                .then_some(index)
        })
        .collect();
    let usage = partial.usage;
    let items = if cancelled {
        partial
            .items
            .into_iter()
            .flatten()
            .filter(|item| !matches!(item, OutputItem::FunctionCall(_)))
            .collect()
    } else {
        partial.into_aborted_items()
    };
    let start = {
        let mut conversation = conversation.lock().unwrap();
        let start = conversation.items.len();
        // No output and no usage means there is no model response to account for.
        if !items.is_empty() || usage.is_some() {
            conversation.record_response_usage(usage);
        }
        for item in items {
            conversation.add_output(item);
        }
        start
    };
    // Retained completed calls were not executed. Pair them before another
    // request can observe this history, just as on cancellation after a commit.
    ensure_tool_output_pairing(conversation);
    surface.on_event(RunnerEvent::ResponseAborted {
        start,
        retained_indices,
    });
}

impl TurnRunner {
    /// Run a full turn: stream a response, run any tool calls it requests, and
    /// loop until the model answers with no tool calls (returning its text) or
    /// an error is hit. `conversation` is mutated in place with every
    /// output and tool result, exactly as the TUI would record them.
    pub(crate) async fn run_turn(
        &self,
        conversation: &Mutex<Conversation>,
        cancel: &CancellationToken,
        surface: &dyn AgentSurface,
    ) -> Result<TurnResult, RunnerError> {
        conversation.lock().unwrap().recalled_memories = Some(0);
        let retrying = &self.stream_retrying;
        let mut steps = 0usize;
        let mut argument_failures = 0usize;
        let mut post_edit_reminder_added = false;
        let mut programmer_md_edited = false;
        let mut association_grace_used = false;
        // Start association beside the first model request instead of putting
        // it on the first-token path. A result can join a later step after a
        // tool round; a one-response turn never waits for it.
        let mut association = self.memory_model.as_ref().and_then(|memory_model| {
            association_context(conversation).map(|(context, query)| {
                spawn_memory_association(
                    memory_model.clone(),
                    context,
                    query,
                    conversation.lock().unwrap().context_memory_ids(),
                )
            })
        });
        loop {
            if cancel.is_cancelled() {
                ensure_tool_output_pairing(conversation);
                return Err(RunnerError::Cancelled);
            }
            if let Some(task) = &mut association {
                let should_wait = steps > 0 && !association_grace_used && !task.is_finished();
                let grace = if should_wait {
                    association_grace_used = true;
                    surface.on_event(RunnerEvent::Phase(RunnerPhase::Associating));
                    std::time::Duration::from_millis(crate::consts::MEMORY_ASSOCIATION_GRACE_MS)
                } else {
                    std::time::Duration::ZERO
                };
                match poll_memory_association(task, grace, cancel).await {
                    AssociationPoll::Cancelled => {
                        ensure_tool_output_pairing(conversation);
                        return Err(RunnerError::Cancelled);
                    }
                    AssociationPoll::Pending => {}
                    AssociationPoll::Finished(Ok(entries)) => {
                        conversation.lock().unwrap().recalled_memories = Some(entries.len());
                        append_associated_memories(conversation, &entries);
                        association = None;
                    }
                    AssociationPoll::Finished(Err(error)) => {
                        // Continuing without memories is the right call, but
                        // doing it silently makes a failed lookup
                        // indistinguishable from finding nothing relevant.
                        let notice = association_failure_notice(&error);
                        surface.on_event(RunnerEvent::Notice(&notice));
                        association = None;
                    }
                }
            }
            if let Some(limit) = self.max_steps
                && steps >= limit
            {
                ensure_tool_output_pairing(conversation);
                return Err(RunnerError::StepLimit { limit });
            }
            steps += 1;

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
                    soul: self.soul.as_deref(),
                    skill_prompt: skill_prompt.as_deref(),
                    plan_prompt: surface.plan_prompt(),
                    coauthor: self.coauthor.as_deref(),
                    vision_enabled: self.vision_enabled,
                    thinking_level: self.thinking_level,
                };
                let conv = conversation.lock().unwrap();
                request::build_request(&conv, &ctx, self.model_name.clone(), self.tools.tools())
            };
            surface.on_event(RunnerEvent::Phase(RunnerPhase::Streaming));
            let mut partial = PartialResponse::new(cancel.child());
            let mut stream_err: Option<OpenAIError> = None;
            let mut argument_guard = argument_guard::ArgumentGuard::default();
            let mut argument_failed = false;
            stream::stream_until(
                &self.client,
                &req,
                cancel,
                retrying,
                self.stream_retry_limit,
                |result| {
                    match result {
                        Ok(ev) => {
                            if !argument_guard.accept(&ev) {
                                argument_failed = true;
                                return std::ops::ControlFlow::Break(());
                            }
                            // Forward each chunk for live rendering, then fold it
                            // into our own partial to extract the committed items.
                            surface.on_event(RunnerEvent::StreamChunk(Box::new(ev.clone())));
                            partial.handle_response_stream_event(ev);
                        }
                        Err(e) => stream_err = Some(e),
                    }
                    std::ops::ControlFlow::Continue(())
                },
            )
            .await;

            if argument_failed {
                // Discard the entire response, including completed parallel calls.
                // Only synthetic empty calls and paired failures enter history.
                let start = conversation.lock().unwrap().items.len();
                surface.on_event(RunnerEvent::ResponseAborted {
                    start,
                    retained_indices: Vec::new(),
                });
                let identities_complete = argument_guard.identities_complete();
                let calls = argument_guard.calls.into_values().collect::<Vec<_>>();
                let mut call_ids = HashSet::new();
                let valid = identities_complete
                    && !calls.is_empty()
                    && calls.iter().all(|call| {
                        !call.name.trim().is_empty()
                            && !call.call_id.trim().is_empty()
                            && call_ids.insert(call.call_id.as_str())
                    });
                if !valid {
                    return Err(RunnerError::Api {
                        code: Some("tool_argument_generation".into()),
                        message: argument_guard::FAILURE.into(),
                    });
                }
                {
                    let mut conversation = conversation.lock().unwrap();
                    for call in calls {
                        let call_id = call.call_id.clone();
                        conversation.add_output(OutputItem::FunctionCall(call));
                        conversation.add_tool_output(crate::tools::ToolOutput {
                            param: async_openai::types::responses::FunctionCallOutputItemParam {
                                call_id,
                                output: async_openai::types::responses::FunctionCallOutput::Text(
                                    argument_guard::FAILURE.into(),
                                ),
                                id: None,
                                status: None,
                            },
                            failed: true,
                            approval_label: None,
                        });
                    }
                }
                argument_failures += 1;
                if argument_failures >= argument_guard::MAX_CONSECUTIVE_FAILURES {
                    return Err(RunnerError::Api {
                        code: Some("tool_argument_generation".into()),
                        message: argument_guard::FAILURE.into(),
                    });
                }
                continue;
            }
            argument_failures = 0;

            if cancel.is_cancelled() {
                commit_aborted_response(conversation, partial, true, surface);
                return Err(RunnerError::Cancelled);
            }
            if let Some(error) = stream_err {
                commit_aborted_response(conversation, partial, false, surface);
                return Err(RunnerError::Stream(error));
            }

            let error = match partial.take_finish_reason() {
                Some(ResponseFinishReason::ApiError { code, message, .. }) => {
                    Some(RunnerError::Api { code, message })
                }
                Some(ResponseFinishReason::Failed(response)) => Some(RunnerError::Api {
                    code: response
                        .error
                        .as_ref()
                        .map(|error| format!("{:?}", error.code)),
                    message: response
                        .error
                        .map(|error| error.message)
                        .unwrap_or_else(|| "model response failed".to_string()),
                }),
                Some(ResponseFinishReason::StreamError(error)) => Some(RunnerError::Stream(error)),
                None => Some(RunnerError::EmptyResponse),
                _ => None,
            };
            if let Some(error) = error {
                commit_aborted_response(conversation, partial, false, surface);
                return Err(error);
            }
            let usage = partial.usage;
            let (_, items) = partial.into_parts();

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
                conv.record_response_usage(usage);
            }
            // Committed to the shared conversation: tell a live renderer to drop
            // its in-progress view now (after the commit, so there is no frame
            // where neither the in-flight nor the committed copy shows).
            surface.on_event(RunnerEvent::ResponseCommitted);
            if !assistant_text.is_empty() {
                surface.on_event(RunnerEvent::Assistant(&assistant_text));
            }

            // ---- no tool calls → the turn is done ----
            if calls.is_empty() {
                surface.on_event(RunnerEvent::Activity(
                    "final response committed; finishing turn",
                ));
                if let Some((input_tokens, _, _)) = usage {
                    surface.on_event(RunnerEvent::Activity(
                        "context usage safe point / compaction",
                    ));
                    surface.usage_safe_point(input_tokens).await;
                }
                let usage = conversation.lock().unwrap().accumulated_usage;
                return Ok(TurnResult {
                    final_text: assistant_text,
                    usage,
                });
            }

            // ---- classify, then run the calls ----
            for call in &calls {
                surface.on_event(RunnerEvent::ToolCall { name: &call.name });
            }
            // Snapshot the batch metadata the hooks self-gate on — but only when
            // any are attached, so a hook-free headless run does no extra work. We need
            // the edit call-ids to tell afterwards whether a file was actually
            // written; the tool names go into the summary handed to each hook.
            let tool_names = calls.iter().map(|c| c.name.clone()).collect::<Vec<_>>();
            let edit_call_ids = calls
                .iter()
                .filter(|c| is_file_edit_tool(&c.name))
                .map(|c| c.call_id.clone())
                .collect::<HashSet<_>>();
            let programmer_md_call_ids = calls
                .iter()
                .filter(|c| is_file_edit_tool(&c.name) && edit_targets_programmer_md(&c.arguments))
                .map(|c| c.call_id.clone())
                .collect::<HashSet<_>>();
            let hook_meta = (!self.hooks.is_empty()).then_some(tool_names);

            // Before-batch hooks (tools have not run, so `edited` is false).
            if let Some(tool_names) = &hook_meta {
                let summary = hooks::BatchSummary {
                    tool_names: tool_names.clone(),
                    edited: false,
                };
                self.run_hooks(
                    hooks::HookPhase::Before,
                    conversation,
                    &summary,
                    surface,
                    cancel,
                )
                .await?;
            }

            let outputs = self.run_calls(conversation, calls, cancel, surface).await;
            match outputs {
                Some(outputs) => {
                    let edited = outputs
                        .iter()
                        .any(|o| !o.failed && edit_call_ids.contains(&o.param.call_id));
                    let edited_programmer_md = outputs
                        .iter()
                        .any(|o| !o.failed && programmer_md_call_ids.contains(&o.param.call_id));
                    {
                        let mut conv = conversation.lock().unwrap();
                        for out in outputs {
                            conv.add_tool_output(out);
                        }
                    }
                    // After-batch hooks. Each self-gates on the summary, so we run
                    // them after every batch and let e.g. the diagnostics hook
                    // bow out when nothing was edited.
                    if let Some(tool_names) = hook_meta {
                        let summary = hooks::BatchSummary { tool_names, edited };
                        self.run_hooks(
                            hooks::HookPhase::After,
                            conversation,
                            &summary,
                            surface,
                            cancel,
                        )
                        .await?;
                    }
                    if edited_programmer_md {
                        programmer_md_edited = true;
                        if post_edit_reminder_added {
                            conversation
                                .lock()
                                .unwrap()
                                .remove_last_developer_message(crate::prompts::POST_EDIT_REMINDER);
                            post_edit_reminder_added = false;
                        }
                    }
                    if edited && !programmer_md_edited && !post_edit_reminder_added {
                        append_post_edit_reminder(conversation);
                        post_edit_reminder_added = true;
                    }
                    if let Some((input_tokens, _, _)) = usage {
                        surface.on_event(RunnerEvent::Activity(
                            "context usage safe point / compaction",
                        ));
                        surface.usage_safe_point(input_tokens).await;
                    }
                }
                None => {
                    ensure_tool_output_pairing(conversation);
                    return Err(RunnerError::Cancelled);
                }
            }
        }
    }

    /// Classify and execute one batch of tool calls, returning the outputs (or
    /// `None` if cancelled). Interactive tools are pre-denied before
    /// classification when the surface has no UI, so they cannot block forever
    /// on a dead answer channel.
    async fn run_calls(
        &self,
        conversation: &Mutex<Conversation>,
        calls: Vec<FunctionToolCall>,
        cancel: &CancellationToken,
        surface: &dyn AgentSurface,
    ) -> Option<Vec<crate::tools::ToolOutput>> {
        // Interactive tools need a front-end to answer them. A surface that
        // provides a question endpoint (the TUI) can; without one (headless),
        // pre-deny them so they cannot hang on a dead answer channel.
        let questions = surface.questions();
        let mut denied: Vec<crate::tools::ToolOutput> = Vec::new();
        let mut classifiable: Vec<FunctionToolCall> = Vec::new();
        for call in calls {
            if let Err(reason) = self.tools.validate(&call) {
                denied.push(classify::invalid_tool_call_output(&call, &reason));
            } else if self.tools.requires_interaction(&call.name) && questions.is_none() {
                let reason = format!("{} is unavailable in non-interactive mode", call.name);
                denied.push(classify::classifier_denied_output(&call, &reason));
            } else {
                classifiable.push(call);
            }
        }

        // Front gate: the owning provider's policy decides whether each call
        // needs the classifier at all. Auto-approved calls (read-only built-ins,
        // explicitly read-only MCP tools) skip it entirely; only the rest are
        // classified.
        let mut auto_allowed: Vec<FunctionToolCall> = Vec::new();
        let mut to_classify: Vec<FunctionToolCall> = Vec::new();
        for call in classifiable {
            match self.tools.approval(&call.name, &call.arguments) {
                crate::tools::provider::ToolApproval::AutoApprove => auto_allowed.push(call),
                crate::tools::provider::ToolApproval::Classify => to_classify.push(call),
            }
        }

        surface.on_event(RunnerEvent::Phase(RunnerPhase::Classifying));
        let mut outcome = match &self.policy {
            RunnerPolicy::Yolo => classify::ClassificationOutcome {
                allowed: to_classify,
                denied: Vec::new(),
                ask: Vec::new(),
            },
            RunnerPolicy::Sync(classifier) => {
                classify::classify_sync(classifier.as_ref(), &to_classify, cancel)?
            }
            RunnerPolicy::Llm(p) => {
                let (light, full) = {
                    let conv = conversation.lock().unwrap();
                    let items: Vec<&MessageItem> = conv.items().collect();
                    classify::build_classifier_context(&items)
                };
                let ctx = crate::classifier::ClassifyContext {
                    client: &p.client,
                    model: &p.model_name,
                    light_context: &light,
                    full_context: &full,
                    top_logprobs: p.top_logprobs,
                };
                classify::classify_llm(&ctx, &p.no_logprobs, to_classify, cancel).await?
            }
        };
        // Front-gate auto-approvals join whatever the classifier cleared.
        outcome.allowed.extend(auto_allowed);

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

        let phase = if allowed.iter().any(|call| {
            call.name == crate::tools::memory::NAME
                && crate::tools::memory::action_is_recall(&call.arguments)
        }) {
            RunnerPhase::Associating
        } else {
            RunnerPhase::RunningTools
        };
        surface.on_event(RunnerEvent::Phase(phase));
        let op_id = surface.operation_id();
        let waiting_subagents = allowed.iter().any(|call| {
            call.name == crate::tools::agent::NAME && crate::tools::agent::is_wait(&call.arguments)
        });
        if waiting_subagents {
            surface.on_event(RunnerEvent::WaitingSubagents(true));
        }
        let tool_names = allowed
            .iter()
            .map(|call| call.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        surface.on_event(RunnerEvent::Activity(&format!("tool batch: {tool_names}")));
        let outputs = tools::run_tool_batch(
            allowed,
            denied,
            cancel.clone(),
            surface.approval_label(),
            questions.unwrap_or_default(),
            self.tools.clone(),
            op_id,
        )
        .await;
        if waiting_subagents {
            surface.on_event(RunnerEvent::WaitingSubagents(false));
        }
        Some(outputs)
    }

    /// Drive every attached hook for one `phase` of a tool batch, collecting the
    /// feedback each returns and injecting the combined text into the
    /// conversation. After-batch feedback is folded into the most recent editing
    /// tool's output when present (so it renders inside that call) and otherwise
    /// added as a system note; before-batch feedback — where there is no fresh
    /// edit output to attach to — is always a system note.
    async fn run_hooks(
        &self,
        phase: hooks::HookPhase,
        conversation: &Mutex<Conversation>,
        summary: &hooks::BatchSummary,
        surface: &dyn AgentSurface,
        cancel: &CancellationToken,
    ) -> Result<(), RunnerError> {
        let ctx = hooks::HookContext {
            conversation,
            surface,
            batch: summary,
            cancel,
        };
        let mut parts: Vec<String> = Vec::new();
        for hook in &self.hooks {
            let side = match phase {
                hooks::HookPhase::Before => "before tools",
                hooks::HookPhase::After => "after tools",
            };
            surface.on_event(RunnerEvent::Activity(&format!(
                "hook {} ({side})",
                hook.name()
            )));
            // Hooks need not implement cooperative cancellation themselves.
            // Keep committed calls paired before returning control to the owner.
            let Some(out) = cancel
                .wait_or(async {
                    match phase {
                        hooks::HookPhase::Before => hook.before_tool_batch(&ctx).await,
                        hooks::HookPhase::After => hook.after_tool_batch(&ctx).await,
                    }
                })
                .await
            else {
                ensure_tool_output_pairing(conversation);
                return Err(RunnerError::Cancelled);
            };
            if let Some(text) = out
                && !text.is_empty()
            {
                parts.push(text);
            }
        }
        if parts.is_empty() {
            return Ok(());
        }
        let combined = parts.join("\n\n");
        match phase {
            hooks::HookPhase::After => inject_post_edit_feedback(conversation, &combined),
            hooks::HookPhase::Before => {
                conversation
                    .lock()
                    .unwrap()
                    .add_meta("\u{25B8} System", &combined);
            }
        }
        Ok(())
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

fn is_file_edit_tool(name: &str) -> bool {
    name == crate::tools::write_file::NAME || name == crate::tools::edit_file::NAME
}

fn edit_targets_programmer_md(arguments: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(arguments)
        .ok()
        .and_then(|value| value.get("path")?.as_str().map(str::to_owned))
        .and_then(|path| {
            std::path::PathBuf::from(path)
                .file_name()
                .map(|name| name.to_owned())
        })
        .is_some_and(|name| name == "PROGRAMMER.md")
}

/// Everything the memory model needs for one association pass: the latest user
/// request, plus a bounded digest of the surrounding conversation so elliptical
/// follow-ups can still match a stored memory.
///
/// The digest deliberately carries only user and assistant prose. Tool traffic
/// is noisy and the associated-memory block we inject ourselves is a developer
/// message, so neither can feed back into the next association pass.
fn association_context(conversation: &Mutex<Conversation>) -> Option<(String, String)> {
    const MAX_CONTEXT_CHARS: usize = 4000;
    const MAX_TURNS: usize = 12;
    let conv = conversation.lock().unwrap();
    let items: Vec<_> = conv.items().collect();
    let mut query = None;
    let mut lines: Vec<String> = Vec::new();
    for item in items {
        match item {
            MessageItem::Input(async_openai::types::responses::InputItem::Item(
                async_openai::types::responses::Item::Message(ApiMessageItem::Input(message)),
            )) if message.role == InputRole::User => {
                for content in &message.content {
                    if let InputContent::InputText(text) = content {
                        query = Some(text.text.clone());
                        lines.push(format!("User: {}", text.text));
                    }
                }
            }
            MessageItem::Output(OutputItem::Message(message)) => {
                for content in &message.content {
                    if let OutputMessageContent::OutputText(text) = content {
                        lines.push(format!("Assistant: {}", text.text));
                    }
                }
            }
            _ => {}
        }
    }
    let query = query?;
    let context = bounded_digest(&lines, MAX_TURNS, MAX_CONTEXT_CHARS);
    Some((context, query))
}

/// Keep the most recent turns and the tail of the transcript, trimming each
/// line so a single huge paste cannot crowd out the rest of the digest.
fn bounded_digest(lines: &[String], max_turns: usize, max_chars: usize) -> String {
    const MAX_LINE_CHARS: usize = 600;
    let start = lines.len().saturating_sub(max_turns);
    let mut digest = lines[start..]
        .iter()
        .map(|line| {
            if line.chars().count() > MAX_LINE_CHARS {
                let head: String = line.chars().take(MAX_LINE_CHARS).collect();
                format!("{head}…")
            } else {
                line.clone()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    if digest.chars().count() > max_chars {
        digest = digest
            .chars()
            .skip(digest.chars().count() - max_chars)
            .collect();
    }
    digest
}

/// What to tell the user when the memory model could not be consulted or
/// answered with something unusable.
fn association_failure_notice(error: &str) -> String {
    format!("Memory association failed ({error}); continuing without recalled memories.")
}

fn append_associated_memories(
    conversation: &Mutex<Conversation>,
    entries: &[crate::memory::MemoryEntry],
) {
    if entries.is_empty() {
        return;
    }
    let mut conv = conversation.lock().unwrap();
    let excluded = conv.context_memory_ids();
    let entries: Vec<_> = entries
        .iter()
        .filter(|entry| !excluded.contains(&entry.id))
        .collect();
    if entries.is_empty() {
        return;
    }
    let ids: Vec<_> = entries.iter().map(|entry| &entry.id).collect();
    let mut text = format!(
        "<!-- programmer-associated-memory-ids: {} -->\n<!-- programmer-associated-memory: persistent recalled context -->\nRelevant memories from previous sessions:\n",
        serde_json::to_string(&ids).expect("memory IDs serialize")
    );
    for entry in entries {
        // Claude Code style: fresh memories get a short age prefix, older ones
        // get the staleness caveat instead, so the two never repeat each other.
        // Injection only appends to the end of the conversation, so this never
        // invalidates the cached prompt prefix.
        let header = crate::memory::freshness_caveat(entry, crate::memory::now_secs())
            .unwrap_or_else(|| {
                format!(
                    "saved {}",
                    crate::memory::age_label(entry, crate::memory::now_secs())
                )
            });
        text.push_str(&format!(
            "- [{:?}] ({header}) {}\n",
            entry.kind, entry.content
        ));
    }
    conv.add_input_message(ApiMessageItem::Input(InputMessage {
        content: vec![InputContent::InputText(
            async_openai::types::responses::InputTextContent { text },
        )],
        role: InputRole::Developer,
        status: Some(OutputStatus::Completed),
    }));
}

fn append_post_edit_reminder(conversation: &Mutex<Conversation>) {
    conversation
        .lock()
        .unwrap()
        .add_input_message(ApiMessageItem::Input(InputMessage {
            content: vec![InputContent::InputText(
                crate::prompts::POST_EDIT_REMINDER.into(),
            )],
            role: InputRole::Developer,
            status: Some(OutputStatus::Completed),
        }));
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

    struct ArchiveSurface {
        cancel: Option<CancellationToken>,
        boundaries: Mutex<Vec<(usize, Vec<usize>)>>,
        commits: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl AgentSurface for ArchiveSurface {
        fn on_event(&self, event: RunnerEvent<'_>) {
            match event {
                RunnerEvent::StreamChunk(_) => {
                    if let Some(cancel) = &self.cancel {
                        cancel.cancel();
                    }
                }
                RunnerEvent::ResponseAborted {
                    start,
                    retained_indices,
                } => {
                    self.boundaries
                        .lock()
                        .unwrap()
                        .push((start, retained_indices));
                }
                RunnerEvent::ResponseCommitted => {
                    self.commits.fetch_add(1, Ordering::SeqCst);
                }
                _ => {}
            }
        }
        async fn review(&self, _: &FunctionToolCall, _: &str, _: (usize, usize)) -> ReviewDecision {
            panic!("interrupted calls must not execute")
        }
    }

    fn archive_surface(cancel: Option<CancellationToken>) -> ArchiveSurface {
        ArchiveSurface {
            cancel,
            boundaries: Mutex::new(Vec::new()),
            commits: AtomicUsize::new(0),
        }
    }

    #[test]
    fn interrupted_archive_filters_calls_and_records_usage_once() {
        use async_openai::types::responses::{ReasoningItem, ResponseOutputItemDoneEvent};
        for cancelled in [false, true] {
            let conversation = Mutex::new(Conversation::new());
            let mut partial = PartialResponse::new(CancellationToken::new());
            partial.items = vec![
                Some(call_item("unfinished", "grep", "{")),
                None,
                Some(message_item("partial text")),
                Some(OutputItem::Reasoning(ReasoningItem {
                    id: Some("reasoning".into()),
                    summary: vec![],
                    content: None,
                    encrypted_content: None,
                    status: None,
                })),
            ];
            partial.handle_response_stream_event(ResponseStreamEvent::ResponseOutputItemDone(
                ResponseOutputItemDoneEvent {
                    sequence_number: 1,
                    output_index: 4,
                    item: call_item("finished", "grep", "{}"),
                },
            ));
            partial.usage = Some((100, 20, 30));
            let surface = archive_surface(None);
            commit_aborted_response(&conversation, partial, cancelled, &surface);
            let conversation = conversation.lock().unwrap();
            assert_eq!(conversation.usage_summary().total_tokens(), 120);
            assert_eq!(conversation.usage_summary().cached_input_tokens, 30);
            assert_eq!(
                conversation
                    .items()
                    .filter(|item| matches!(item, MessageItem::Output(_)))
                    .count(),
                if cancelled { 2 } else { 3 }
            );
            assert_eq!(
                conversation
                    .items()
                    .filter(|item| matches!(item, MessageItem::ToolOutput { .. }))
                    .count(),
                if cancelled { 0 } else { 1 }
            );
            assert_eq!(
                *surface.boundaries.lock().unwrap(),
                vec![(0, if cancelled { vec![1, 2] } else { vec![1, 2, 3] })]
            );
        }
    }

    #[tokio::test]
    async fn stream_interruption_is_archived_without_a_ui() {
        for cancelled in [false, true] {
            let body = item_added_frame(1, 0, &message_item("saved partial"));
            let (base_url, server) = spawn_mock_responses(vec![body]).await;
            let mut runner = engine_for(&base_url);
            runner.stream_retry_limit = 0;
            let conversation = Mutex::new(Conversation::new());
            let cancel = CancellationToken::new();
            let surface = archive_surface(cancelled.then(|| cancel.clone()));
            let result = runner.run_turn(&conversation, &cancel, &surface).await;
            server.abort();
            assert!(result.is_err());
            assert_eq!(matches!(result, Err(RunnerError::Cancelled)), cancelled);
            assert_eq!(conversation.lock().unwrap().items().count(), 1);
            assert_eq!(surface.boundaries.lock().unwrap().len(), 1);
            assert_eq!(surface.commits.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn failed_api_response_archives_output_and_usage() {
        let body = format!(
            "{}{}",
            item_added_frame(1, 0, &message_item("partial")),
            frame(serde_json::json!({
                "type": "response.failed", "sequence_number": 2,
                "response": { "created_at": 0, "id": "failed", "model": "mock", "object": "response",
                    "output": [], "status": "failed", "error": { "code": "server_error", "message": "failed upstream" },
                    "usage": { "input_tokens": 100, "output_tokens": 20, "total_tokens": 120,
                        "input_tokens_details": { "cached_tokens": 30 }, "output_tokens_details": { "reasoning_tokens": 0 } }
                }
            }))
        );
        let (base_url, server) = spawn_mock_responses(vec![body]).await;
        let conversation = Mutex::new(Conversation::new());
        let surface = archive_surface(None);
        let result = engine_for(&base_url)
            .run_turn(&conversation, &CancellationToken::new(), &surface)
            .await;
        server.abort();
        assert!(matches!(result, Err(RunnerError::Api { .. })));
        assert_eq!(conversation.lock().unwrap().items().count(), 1);
        assert_eq!(
            conversation.lock().unwrap().usage_summary().total_tokens(),
            120
        );
        assert_eq!(surface.boundaries.lock().unwrap().len(), 1);
        assert_eq!(surface.commits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn normal_response_commits_once_without_abort_archive() {
        let body = format!(
            "{}{}",
            item_added_frame(1, 0, &message_item("done")),
            completed_frame(2)
        );
        let (base_url, server) = spawn_mock_responses(vec![body]).await;
        let conversation = Mutex::new(Conversation::new());
        let surface = archive_surface(None);
        let result = engine_for(&base_url)
            .run_turn(&conversation, &CancellationToken::new(), &surface)
            .await;
        server.abort();
        assert_eq!(result.unwrap().final_text, "done");
        assert_eq!(conversation.lock().unwrap().items().count(), 1);
        assert!(surface.boundaries.lock().unwrap().is_empty());
        assert_eq!(surface.commits.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn only_file_edit_tools_trigger_the_post_edit_reminder() {
        assert!(is_file_edit_tool(crate::tools::write_file::NAME));
        assert!(is_file_edit_tool(crate::tools::edit_file::NAME));
        assert!(!is_file_edit_tool(crate::tools::command::NAME));
        assert!(!is_file_edit_tool(crate::tools::read_file::NAME));
    }

    #[test]
    fn programmer_md_edit_arguments_are_detected_by_file_name() {
        assert!(edit_targets_programmer_md(r#"{"path":"PROGRAMMER.md"}"#));
        assert!(edit_targets_programmer_md(
            r#"{"path":"/workspace/PROGRAMMER.md"}"#
        ));
        assert!(!edit_targets_programmer_md(r#"{"path":"README.md"}"#));
        assert!(!edit_targets_programmer_md("not json"));
    }

    #[test]
    fn post_edit_reminder_is_a_separate_developer_message() {
        let conversation = Mutex::new(Conversation::new());
        append_post_edit_reminder(&conversation);
        let conv = conversation.lock().unwrap();
        let Some(MessageItem::Input(async_openai::types::responses::InputItem::Item(
            async_openai::types::responses::Item::Message(ApiMessageItem::Input(message)),
        ))) = conv.items().next()
        else {
            panic!("expected a standalone input message");
        };
        assert_eq!(message.role, InputRole::Developer);
        let InputContent::InputText(text) = &message.content[0] else {
            panic!("expected text reminder");
        };
        assert!(text.text.contains("PROGRAMMER.md"));
        assert!(text.text.contains("documentation"));
        assert!(text.text.contains("tests"));
    }

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
        spawn_recorded_responses(bodies, Arc::new(Mutex::new(Vec::new()))).await
    }

    async fn spawn_recorded_responses(
        bodies: Vec<String>,
        requests: Arc<Mutex<Vec<serde_json::Value>>>,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let bodies = Arc::new(bodies);
        let counter = Arc::new(AtomicUsize::new(0));
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let bodies = bodies.clone();
                let counter = counter.clone();
                let requests = requests.clone();
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
                            requests.lock().unwrap().push(
                                serde_json::from_slice(
                                    &buf[body_start..body_start + content_length],
                                )
                                .unwrap(),
                            );
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

    /// Decode actual SDK SSE events rather than deserializing fixture JSON directly.
    async fn reasoning_fixture_events(body: &str) -> Vec<Result<ResponseStreamEvent, OpenAIError>> {
        use futures::StreamExt;

        let (base_url, server) = spawn_mock_responses(vec![body.to_string()]).await;
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let client = Client::with_config(
                OpenAIConfig::new()
                    .with_api_base(base_url)
                    .with_api_key("synthetic-fixture-key"),
            );
            let request = serde_json::from_value(serde_json::json!({
                "model": "mock", "input": "Synthetic regression", "stream": true
            }))
            .unwrap();
            let mut stream = client.responses().create_stream(request).await.unwrap();
            let mut events = Vec::new();
            while let Some(event) = stream.next().await {
                let terminal = event.is_err()
                    || matches!(&event, Ok(ResponseStreamEvent::ResponseCompleted(_)));
                events.push(event);
                if terminal {
                    break;
                }
            }
            events
        })
        .await;
        server.abort();
        result.expect("reasoning fixture stream must terminate within five seconds")
    }

    async fn fold_reasoning_fixture(body: &str) -> PartialResponse {
        let events = reasoning_fixture_events(body).await;
        assert!(matches!(
            events.last(),
            Some(Ok(ResponseStreamEvent::ResponseCompleted(_)))
        ));
        let mut partial = PartialResponse::new(CancellationToken::new());
        for event in events {
            partial.handle_response_stream_event(event.expect("valid fixture event"));
        }
        partial
    }

    fn reasoning_fixture_item(partial: &PartialResponse) -> serde_json::Value {
        assert_eq!(partial.items.len(), 1);
        serde_json::to_value(partial.items[0].as_ref().unwrap()).unwrap()
    }

    #[tokio::test]
    async fn reasoning_sse_mixed_deltas_done_and_sparse_item_done_preserve_text() {
        let events =
            reasoning_fixture_events(include_str!("../../tests/fixtures/reasoning-mixed.sse"))
                .await;
        assert_eq!(events.len(), 10);
        let mut partial = PartialResponse::new(CancellationToken::new());
        for (index, event) in events.into_iter().enumerate() {
            partial.handle_response_stream_event(event.unwrap());
            if index == 5 {
                let item = reasoning_fixture_item(&partial);
                assert_eq!(item["content"][0]["text"], "Raw draft");
                assert_eq!(item["summary"][0]["text"], "Brief draft");
            }
            if index >= 7 {
                let item = reasoning_fixture_item(&partial);
                assert_eq!(item["content"][0]["text"], "Raw final.");
                assert_eq!(item["summary"][0]["text"], "Brief final.");
            }
        }
        assert_eq!(reasoning_fixture_item(&partial)["status"], "completed");
    }

    #[tokio::test]
    async fn reasoning_sse_done_only_initializes_missing_content_parts() {
        let partial =
            fold_reasoning_fixture(include_str!("../../tests/fixtures/reasoning-done-only.sse"))
                .await;
        let item = reasoning_fixture_item(&partial);
        assert_eq!(item["content"].as_array().unwrap().len(), 2);
        assert_eq!(item["content"][0]["text"], "Done without delta.");
        assert_eq!(item["content"][1]["text"], "Second done-only part.");
    }

    #[tokio::test]
    async fn reasoning_sse_content_and_summary_survive_serialization_roundtrip() {
        let partial = fold_reasoning_fixture(include_str!(
            "../../tests/fixtures/reasoning-both-fields.sse"
        ))
        .await;
        let item = partial.items[0].as_ref().unwrap();
        let serialized = serde_json::to_value(item).unwrap();
        assert_eq!(serialized["content"][0]["text"], "Stored raw text.");
        assert_eq!(serialized["summary"][0]["text"], "Stored summary.");
        let restored: OutputItem = serde_json::from_value(serialized).unwrap();
        assert_eq!(&restored, item);
    }

    #[tokio::test]
    async fn reasoning_sse_huge_content_index_and_wrong_item_type_are_ignored() {
        let events = reasoning_fixture_events(include_str!(
            "../../tests/fixtures/reasoning-invalid-targets.sse"
        ))
        .await;
        assert_eq!(events.len(), 7);
        let mut partial = PartialResponse::new(CancellationToken::new());
        let mut original_message = None;
        for (index, event) in events.into_iter().enumerate() {
            partial.handle_response_stream_event(event.unwrap());
            let Some(OutputItem::Reasoning(reasoning)) = &partial.items[0] else {
                panic!("expected reasoning item");
            };
            assert!(
                reasoning.content.is_none(),
                "invalid index allocated content"
            );
            assert!(reasoning.summary.is_empty());
            if index == 3 {
                original_message = partial.items[1].clone();
            }
            if index >= 3 {
                assert_eq!(partial.items.len(), 2);
                assert_eq!(partial.items[1], original_message);
            }
        }
    }

    #[tokio::test]
    async fn reasoning_sse_missing_delta_is_a_stream_decode_error() {
        let events =
            reasoning_fixture_events(include_str!("../../tests/fixtures/reasoning-malformed.sse"))
                .await;
        assert_eq!(events.len(), 2);
        assert!(matches!(
            &events[0],
            Ok(ResponseStreamEvent::ResponseOutputItemAdded(_))
        ));
        let error = events[1]
            .as_ref()
            .expect_err("missing delta must be rejected");
        assert!(error.to_string().contains("delta"), "{error}");
    }

    fn runaway_body(call_id: &str, name: &str, parallel: Option<&OutputItem>) -> String {
        let mut body = String::new();
        if let Some(item) = parallel {
            body.push_str(&frame(serde_json::json!({
                "type": "response.output_item.done", "sequence_number": 0,
                "output_index": 1, "item": item,
            })));
        }
        body.push_str(&item_added_frame(1, 0, &call_item(call_id, name, "")));
        for sequence in 2..80 {
            body.push_str(&frame(serde_json::json!({
                "type": "response.function_call_arguments.delta",
                "sequence_number": sequence, "output_index": 0, "item_id": "fc_1",
                "delta": "RUNAWAY维拉".repeat(64),
            })));
        }
        // Deliberately no completed event: recovery must drop the stream,
        // rather than wait for the SDK to reconnect an incomplete response.
        body
    }

    #[tokio::test]
    async fn runaway_sse_pairs_parallel_calls_without_executing_or_replaying_garbage() {
        let path = std::env::current_dir()
            .unwrap()
            .join(format!("runaway-must-not-write-{}", uuid::Uuid::new_v4()));
        let parallel = call_item(
            "parallel",
            "write_file",
            &serde_json::json!({"path": path, "content": "must not run"}).to_string(),
        );
        let bodies = vec![
            runaway_body("runaway", "write_file", Some(&parallel)),
            format!(
                "{}{}",
                item_added_frame(1, 0, &message_item("recovered")),
                completed_frame(2)
            ),
        ];
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (base, server) = spawn_recorded_responses(bodies, requests.clone()).await;
        let conversation = Mutex::new(Conversation::new());
        let cancel = CancellationToken::new();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            engine_for(&base).run_turn(&conversation, &cancel, &HeadlessSurface),
        )
        .await;
        server.abort();
        assert_eq!(result.unwrap().unwrap().final_text, "recovered");
        assert!(!cancel.is_cancelled());
        assert!(
            !path.exists(),
            "completed parallel write must never execute"
        );
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        let next = requests[1].to_string();
        assert!(!next.contains("RUNAWAY"));
        assert!(!next.contains("must not run"));
        let input = requests[1]["input"].as_array().unwrap();
        for call_id in ["runaway", "parallel"] {
            let call = input
                .iter()
                .find(|item| item["type"] == "function_call" && item["call_id"] == call_id)
                .unwrap();
            assert_eq!(call["arguments"], "{}");
            let outputs: Vec<_> = input
                .iter()
                .filter(|item| item["type"] == "function_call_output" && item["call_id"] == call_id)
                .collect();
            assert_eq!(outputs.len(), 1);
            assert_eq!(outputs[0]["output"], argument_guard::FAILURE);
        }
        for item in conversation.lock().unwrap().items() {
            if let MessageItem::ToolOutput { failed, .. } = item {
                assert!(failed);
            }
        }
    }

    #[tokio::test]
    async fn runaway_sse_missing_identity_fails_without_retry_or_history() {
        for (call_id, name) in [("", "write_file"), ("call", "")] {
            let requests = Arc::new(Mutex::new(Vec::new()));
            let (base, server) =
                spawn_recorded_responses(vec![runaway_body(call_id, name, None)], requests.clone())
                    .await;
            let conversation = Mutex::new(Conversation::new());
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                engine_for(&base).run_turn(
                    &conversation,
                    &CancellationToken::new(),
                    &HeadlessSurface,
                ),
            )
            .await;
            server.abort();
            assert!(matches!(result.unwrap(), Err(RunnerError::Api { .. })));
            assert_eq!(requests.lock().unwrap().len(), 1);
            assert_eq!(conversation.lock().unwrap().items().count(), 0);
        }
    }

    fn terminal_output_frame(status: &str, output: Vec<OutputItem>) -> String {
        frame(serde_json::json!({
            "type": format!("response.{status}"), "sequence_number": 3,
            "response": {
                "created_at": 0, "id": "resp_1", "model": "mock",
                "object": "response", "output": output, "status": status
            }
        }))
    }

    #[tokio::test]
    async fn runaway_terminal_output_is_checked_and_all_calls_are_paired() {
        for status in ["completed", "failed", "incomplete"] {
            for arguments in ["RUNAWAY".repeat(3000), "x".repeat(8 * 1024 * 1024 + 1)] {
                let body = terminal_output_frame(
                    status,
                    vec![
                        call_item("runaway", "write_file", &arguments),
                        call_item("parallel", "write_file", "{}"),
                    ],
                );
                let requests = Arc::new(Mutex::new(Vec::new()));
                let (base, server) = spawn_recorded_responses(
                    vec![
                        body,
                        format!(
                            "{}{}",
                            item_added_frame(1, 0, &message_item("recovered")),
                            completed_frame(2)
                        ),
                    ],
                    requests.clone(),
                )
                .await;
                let conversation = Mutex::new(Conversation::new());
                let result = tokio::time::timeout(
                    std::time::Duration::from_secs(60),
                    engine_for(&base).run_turn(
                        &conversation,
                        &CancellationToken::new(),
                        &HeadlessSurface,
                    ),
                )
                .await;
                server.abort();
                assert_eq!(result.unwrap().unwrap().final_text, "recovered", "{status}");
                let requests = requests.lock().unwrap();
                assert_eq!(requests.len(), 2);
                let input = requests[1]["input"].as_array().unwrap();
                for call_id in ["runaway", "parallel"] {
                    let calls: Vec<_> = input
                        .iter()
                        .filter(|item| {
                            item["type"] == "function_call" && item["call_id"] == call_id
                        })
                        .collect();
                    assert_eq!(calls.len(), 1);
                    assert_eq!(calls[0]["arguments"], "{}");
                    let outputs: Vec<_> = input
                        .iter()
                        .filter(|item| {
                            item["type"] == "function_call_output" && item["call_id"] == call_id
                        })
                        .collect();
                    assert_eq!(outputs.len(), 1);
                    assert_eq!(outputs[0]["output"], argument_guard::FAILURE);
                }
                assert!(!requests[1].to_string().contains("RUNAWAY"));
            }
        }
    }

    #[tokio::test]
    async fn runaway_ambiguous_identities_fail_before_any_history_is_written() {
        let mut bodies = vec![runaway_body(
            "duplicate",
            "write_file",
            Some(&call_item("duplicate", "read_file", "{}")),
        )];
        for status in ["completed", "failed", "incomplete"] {
            bodies.push(terminal_output_frame(
                status,
                vec![
                    call_item("duplicate", "write_file", &"RUNAWAY".repeat(3000)),
                    call_item("duplicate", "read_file", "{}"),
                ],
            ));
        }
        for (call_id, name) in [("changed", "write_file"), ("original", "read_file")] {
            let initial = item_added_frame(1, 0, &call_item("original", "write_file", ""));
            bodies.push(format!(
                "{initial}{}",
                frame(serde_json::json!({
                    "type": "response.output_item.done", "sequence_number": 2,
                    "output_index": 0, "item": call_item(call_id, name, "{}")
                }))
            ));
            for status in ["completed", "failed", "incomplete"] {
                bodies.push(format!(
                    "{initial}{}",
                    terminal_output_frame(status, vec![call_item(call_id, name, "{}")])
                ));
            }
        }
        for body in bodies {
            let requests = Arc::new(Mutex::new(Vec::new()));
            let (base, server) = spawn_recorded_responses(vec![body], requests.clone()).await;
            let conversation = Mutex::new(Conversation::new());
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                engine_for(&base).run_turn(
                    &conversation,
                    &CancellationToken::new(),
                    &HeadlessSurface,
                ),
            )
            .await;
            server.abort();
            assert!(matches!(result.unwrap(), Err(RunnerError::Api { .. })));
            assert_eq!(requests.lock().unwrap().len(), 1);
            assert_eq!(conversation.lock().unwrap().items().count(), 0);
        }
    }

    #[tokio::test]
    async fn runaway_sse_repeated_failures_are_bounded_and_paired() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let bodies = (0..3)
            .map(|index| runaway_body(&format!("call_{index}"), "write_file", None))
            .collect();
        let (base, server) = spawn_recorded_responses(bodies, requests.clone()).await;
        let conversation = Mutex::new(Conversation::new());
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            engine_for(&base).run_turn(&conversation, &CancellationToken::new(), &HeadlessSurface),
        )
        .await;
        server.abort();
        assert!(matches!(result.unwrap(), Err(RunnerError::Api { .. })));
        assert_eq!(requests.lock().unwrap().len(), 3);
        assert_eq!(conversation.lock().unwrap().items().count(), 6);
        assert!(
            !format!(
                "{:?}",
                conversation.lock().unwrap().items().collect::<Vec<_>>()
            )
            .contains("RUNAWAY")
        );
    }

    fn engine_for(base_url: &str) -> TurnRunner {
        let client = Client::with_config(OpenAIConfig::new().with_api_base(base_url.to_string()));
        TurnRunner {
            client,
            model_name: "mock".to_string(),
            model_str: "mock/mock".to_string(),
            tools: std::sync::Arc::new(crate::tools::provider::ToolRegistry::new(vec![
                std::sync::Arc::new(crate::tools::provider::LocalToolProvider::default()),
            ])),
            policy: RunnerPolicy::Yolo,
            soul: None,
            coauthor: None,
            vision_enabled: true,
            thinking_level: crate::thinking::ThinkingLevel::Auto,
            memory_model: None,
            hooks: Vec::new(),
            stream_retrying: Arc::new(AtomicBool::new(false)),
            stream_retry_limit: crate::consts::MAX_STREAM_RETRIES,
            max_steps: None,
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

    #[test]
    fn association_context_carries_the_whole_recent_conversation() {
        let conversation = Mutex::new(Conversation::new());
        {
            let mut conv = conversation.lock().unwrap();
            conv.add_input_message(user("add dark mode to the theme"));
            conv.add_output(message_item("I picked the slate palette."));
            conv.add_input_message(user("now make the toggle match it"));
        }
        let (context, query) = association_context(&conversation).expect("context");
        assert_eq!(query, "now make the toggle match it");
        // The earlier turn is what lets an elliptical follow-up match a memory.
        assert!(context.contains("User: add dark mode to the theme"));
        assert!(context.contains("Assistant: I picked the slate palette."));
        assert!(context.contains("User: now make the toggle match it"));
    }

    #[test]
    fn association_context_ignores_injected_memories_and_tool_output() {
        let conversation = Mutex::new(Conversation::new());
        {
            let mut conv = conversation.lock().unwrap();
            conv.add_input_message(user("carry on"));
        }
        append_associated_memories(
            &conversation,
            &[crate::memory::MemoryEntry {
                id: "mem_1".into(),
                scope: crate::memory::MemoryScope::Global,
                kind: crate::memory::MemoryKind::Preference,
                name: "indentation-preference".into(),
                description: "User prefers tabs".into(),
                content: "prefers tabs".into(),
                tags: Vec::new(),
                related_memories: Vec::new(),
                created_at: 0,
                updated_at: 0,
                last_used_at: None,
                use_count: 0,
                source: crate::memory::MemorySource::AgentTool,
                confidence: crate::memory::MemoryConfidence::Explicit,
                status: crate::memory::MemoryStatus::Active,
                supersedes: None,
            }],
        );
        let (context, _) = association_context(&conversation).expect("context");
        assert!(!context.contains("prefers tabs"));
    }

    fn pending_association(
        receiver: tokio::sync::oneshot::Receiver<Result<Vec<crate::memory::MemoryEntry>, String>>,
    ) -> MemoryAssociationTask {
        MemoryAssociationTask {
            handle: Some(tokio::spawn(async move {
                receiver
                    .await
                    .unwrap_or_else(|_| Err("sender dropped".into()))
            })),
        }
    }

    #[tokio::test]
    async fn memory_association_grace_cancels_promptly_and_aborts_prefetch() {
        let (mut sender, receiver) = tokio::sync::oneshot::channel();
        let mut task = pending_association(receiver);
        let cancel = CancellationToken::new();
        {
            let poll =
                poll_memory_association(&mut task, std::time::Duration::from_secs(60), &cancel);
            tokio::pin!(poll);
            tokio::select! {
                biased;
                _ = &mut poll => panic!("pending association returned before cancellation"),
                _ = tokio::task::yield_now() => {}
            }
            cancel.cancel();
            let result = tokio::time::timeout(std::time::Duration::from_millis(100), &mut poll)
                .await
                .expect("cancellation must interrupt association grace");
            assert!(matches!(result, AssociationPoll::Cancelled));
        }
        drop(task);
        tokio::time::timeout(std::time::Duration::from_millis(100), sender.closed())
            .await
            .expect("turn exit must abort the association task");
    }

    #[tokio::test]
    async fn first_step_never_waits_for_memory_association() {
        let (_sender, receiver) = tokio::sync::oneshot::channel();
        let mut task = pending_association(receiver);

        let poll = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            poll_memory_association(
                &mut task,
                std::time::Duration::ZERO,
                &CancellationToken::new(),
            ),
        )
        .await
        .expect("a zero-grace poll must return immediately");

        assert!(matches!(poll, AssociationPoll::Pending));
        assert!(task.handle.is_some());
    }

    #[tokio::test]
    async fn a_later_step_can_collect_memory_during_its_grace_period() {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let mut task = pending_association(receiver);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            let _ = sender.send(Ok(Vec::new()));
        });

        let poll = poll_memory_association(
            &mut task,
            std::time::Duration::from_millis(100),
            &CancellationToken::new(),
        )
        .await;

        assert!(matches!(poll, AssociationPoll::Finished(Ok(entries)) if entries.is_empty()));
        assert!(task.handle.is_none());
    }

    #[tokio::test]
    async fn an_expired_grace_keeps_the_prefetch_running() {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let mut task = pending_association(receiver);

        let poll = poll_memory_association(
            &mut task,
            std::time::Duration::from_millis(5),
            &CancellationToken::new(),
        )
        .await;
        assert!(matches!(poll, AssociationPoll::Pending));
        assert!(task.handle.is_some());

        sender.send(Ok(Vec::new())).unwrap();
        let poll = poll_memory_association(
            &mut task,
            std::time::Duration::from_millis(100),
            &CancellationToken::new(),
        )
        .await;
        assert!(matches!(poll, AssociationPoll::Finished(Ok(entries)) if entries.is_empty()));
    }

    #[test]
    fn association_failure_note_keeps_the_models_reason() {
        let notice = association_failure_notice("timed out after 30s");

        // The reason matters: "timed out" and "no relevant memories" both show
        // up as `Memories recalled: 0`, so the text has to tell them apart.
        assert!(notice.contains("timed out after 30s"));
        assert!(notice.contains("continuing without recalled memories"));
    }

    #[test]
    fn association_context_requires_a_user_message() {
        let conversation = Mutex::new(Conversation::new());
        assert!(association_context(&conversation).is_none());
    }

    fn memory_entry(updated_at: u64, content: &str) -> crate::memory::MemoryEntry {
        crate::memory::MemoryEntry {
            id: format!("mem_{updated_at:032x}"),
            scope: crate::memory::MemoryScope::Project,
            kind: crate::memory::MemoryKind::ProjectFact,
            name: "project-fact".into(),
            description: content.into(),
            content: content.into(),
            tags: Vec::new(),
            related_memories: Vec::new(),
            created_at: updated_at,
            updated_at,
            last_used_at: None,
            use_count: 0,
            source: crate::memory::MemorySource::AgentTool,
            confidence: crate::memory::MemoryConfidence::Explicit,
            status: crate::memory::MemoryStatus::Active,
            supersedes: None,
        }
    }

    /// Text of every developer input message, which is where associations land.
    fn developer_text(conversation: &Mutex<Conversation>) -> String {
        conversation
            .lock()
            .unwrap()
            .items()
            .filter_map(|item| match item {
                MessageItem::Input(async_openai::types::responses::InputItem::Item(
                    async_openai::types::responses::Item::Message(ApiMessageItem::Input(message)),
                )) if message.role == InputRole::Developer => Some(
                    message
                        .content
                        .iter()
                        .filter_map(|content| match content {
                            InputContent::InputText(text) => Some(text.text.as_str()),
                            _ => None,
                        })
                        .collect::<String>(),
                ),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn association_excludes_live_ids_but_can_add_new_memories_and_recall_after_compaction() {
        let conversation = Mutex::new(Conversation::new());
        let first = memory_entry(1, "first preference");
        let second = memory_entry(2, "second preference");
        append_associated_memories(&conversation, std::slice::from_ref(&first));
        append_associated_memories(&conversation, &[first.clone(), second.clone()]);
        let text = developer_text(&conversation);
        assert_eq!(text.matches("first preference").count(), 1);
        assert_eq!(text.matches("second preference").count(), 1);
        assert_eq!(conversation.lock().unwrap().context_memory_ids().len(), 2);
        conversation
            .lock()
            .unwrap()
            .apply_compaction("summary".into());
        append_associated_memories(&conversation, std::slice::from_ref(&first));
        assert_eq!(
            conversation.lock().unwrap().context_memory_ids(),
            std::collections::HashSet::from([first.id])
        );
    }

    #[test]
    fn associated_memories_carry_an_age_or_a_staleness_caveat() {
        let now = crate::memory::now_secs();
        let conversation = Mutex::new(Conversation::new());
        append_associated_memories(
            &conversation,
            &[
                memory_entry(now, "prefers tabs"),
                memory_entry(
                    now - 47 * 86_400,
                    "the retry helper lives at src/net/retry.rs:88",
                ),
            ],
        );

        let text = developer_text(&conversation);
        assert!(text.contains("(saved today) prefers tabs"));
        // Stale entries replace the age prefix with the caveat, never both.
        assert!(text.contains("This memory is 47 days old"));
        assert!(text.contains("Verify against current code before asserting as fact"));
        assert!(text.contains("the retry helper lives at src/net/retry.rs:88"));
    }

    #[test]
    fn bounded_digest_keeps_recent_turns_and_shortens_long_lines() {
        let lines = vec![
            "old".to_string(),
            format!("User: {}", "x".repeat(900)),
            "Assistant: newest".to_string(),
        ];
        let digest = bounded_digest(&lines, 2, 4000);
        assert!(!digest.contains("old"));
        assert!(digest.contains("Assistant: newest"));
        assert!(digest.contains('…'));

        let digest = bounded_digest(&lines[1..], 2, 50);
        assert_eq!(digest.chars().count(), 50);
    }

    #[tokio::test]
    async fn run_turn_drives_a_tool_call_then_final_text() {
        // Response 1: a `read_file` tool call. Response 2: the final message.
        let body1 = format!(
            "{}{}",
            item_added_frame(
                1,
                0,
                &call_item("c1", "read_file", "{\"path\":\"Cargo.toml\"}")
            ),
            completed_frame(2),
        );
        let body2 = format!(
            "{}{}",
            item_added_frame(1, 0, &message_item("all done")),
            completed_frame(2),
        );
        let (base, _server) = spawn_mock_responses(vec![body1, body2]).await;

        let runner = engine_for(&base);
        let mut c = Conversation::new();
        c.add_input_message(user("read the manifest"));
        let conv = Mutex::new(c);
        let cancel = CancellationToken::new();
        let result = runner
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
    async fn run_turn_stops_at_the_model_response_limit() {
        let body1 = format!(
            "{}{}",
            item_added_frame(
                1,
                0,
                &call_item("c1", "read_file", "{\"path\":\"Cargo.toml\"}")
            ),
            completed_frame(2),
        );
        let body2 = format!(
            "{}{}",
            item_added_frame(1, 0, &message_item("should not be requested")),
            completed_frame(2),
        );
        let (base, _server) = spawn_mock_responses(vec![body1, body2]).await;

        let mut runner = engine_for(&base);
        runner.max_steps = Some(1);
        let mut conversation = Conversation::new();
        conversation.add_input_message(user("read once"));
        let conversation = Mutex::new(conversation);
        let error = runner
            .run_turn(&conversation, &CancellationToken::new(), &HeadlessSurface)
            .await
            .expect_err("a second model response should be rejected");

        assert!(matches!(error, RunnerError::StepLimit { limit: 1 }));
    }

    async fn assert_hanging_hook_cancels(after: bool) {
        use crate::runner::hooks::{HookContext, TurnHook};

        struct HangingHook {
            after: bool,
            entered: Arc<tokio::sync::Notify>,
        }
        #[async_trait::async_trait]
        impl TurnHook for HangingHook {
            fn name(&self) -> &str {
                "hanging"
            }
            async fn before_tool_batch(&self, _context: &HookContext<'_>) -> Option<String> {
                if self.after {
                    return None;
                }
                self.entered.notify_one();
                std::future::pending().await
            }
            async fn after_tool_batch(&self, _context: &HookContext<'_>) -> Option<String> {
                if !self.after {
                    return None;
                }
                self.entered.notify_one();
                std::future::pending().await
            }
        }

        let body = format!(
            "{}{}",
            item_added_frame(
                1,
                0,
                &call_item("c1", "read_file", "{\"path\":\"Cargo.toml\"}")
            ),
            completed_frame(2),
        );
        let (base, _server) = spawn_mock_responses(vec![body]).await;
        let entered = Arc::new(tokio::sync::Notify::new());
        let mut runner = engine_for(&base);
        runner.hooks = vec![Arc::new(HangingHook {
            after,
            entered: entered.clone(),
        })];
        let mut conversation = Conversation::new();
        conversation.add_input_message(user("read the manifest"));
        let conversation = Mutex::new(conversation);
        let cancel = CancellationToken::new();
        let turn = runner.run_turn(&conversation, &cancel, &HeadlessSurface);
        tokio::pin!(turn);
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::select! {
                _ = entered.notified() => {}
                result = &mut turn => panic!("turn exited before hook: {result:?}"),
            }
        })
        .await
        .expect("hook must be entered");
        cancel.cancel();
        let result = tokio::time::timeout(std::time::Duration::from_millis(100), &mut turn)
            .await
            .expect("cancellation must interrupt a hanging hook");
        assert!(matches!(result, Err(RunnerError::Cancelled)));
        let conversation = conversation.lock().unwrap();
        let outputs = conversation
            .items()
            .filter_map(|item| match item {
                MessageItem::ToolOutput { output, failed, .. } => Some((output, failed)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            outputs.len(),
            1,
            "committed call must have exactly one output"
        );
        assert_eq!(outputs[0].0.call_id, "c1");
        assert_eq!(
            *outputs[0].1, !after,
            "after-hook cancellation preserves the real output"
        );
    }

    #[tokio::test]
    async fn hanging_before_hook_cancels_and_pairs_committed_call() {
        assert_hanging_hook_cancels(false).await;
    }

    #[tokio::test]
    async fn hanging_after_hook_cancels_and_preserves_committed_output() {
        assert_hanging_hook_cancels(true).await;
    }

    #[tokio::test]
    async fn a_custom_hook_fires_after_a_tool_batch_and_injects_feedback() {
        // Demonstrates the pluggable-hook abstraction end to end: an arbitrary
        // hook (not diagnostics) attached to the runner runs after the tool batch
        // and its returned feedback lands in the conversation.
        use crate::runner::hooks::{HookContext, TurnHook};

        struct MarkerHook;
        #[async_trait::async_trait]
        impl TurnHook for MarkerHook {
            fn name(&self) -> &str {
                "marker"
            }
            async fn after_tool_batch(&self, _ctx: &HookContext<'_>) -> Option<String> {
                Some("HOOK-RAN".to_string())
            }
        }

        // Response 1: a read_file call; Response 2: the final message.
        let body1 = format!(
            "{}{}",
            item_added_frame(
                1,
                0,
                &call_item("c1", "read_file", "{\"path\":\"Cargo.toml\"}")
            ),
            completed_frame(2),
        );
        let body2 = format!(
            "{}{}",
            item_added_frame(1, 0, &message_item("done")),
            completed_frame(2),
        );
        let (base, _server) = spawn_mock_responses(vec![body1, body2]).await;

        let mut runner = engine_for(&base);
        runner.hooks = vec![Arc::new(MarkerHook)];
        let mut c = Conversation::new();
        c.add_input_message(user("go"));
        let conv = Mutex::new(c);
        let cancel = CancellationToken::new();
        runner
            .run_turn(&conv, &cancel, &HeadlessSurface)
            .await
            .expect("turn completes");

        // read_file is not an edit, so the feedback is added as a system note.
        let has_marker =
            conv.lock().unwrap().items().any(
                |it| matches!(it, MessageItem::Meta { text, .. } if text.contains("HOOK-RAN")),
            );
        assert!(has_marker, "the custom hook's feedback should be injected");
    }

    #[tokio::test]
    async fn run_turn_pre_denies_interactive_tools_and_continues() {
        // Both question tools must be denied before classification, not hang.
        // Response 2 finishes.
        let body1 = format!(
            "{}{}{}",
            item_added_frame(
                1,
                0,
                &call_item("a1", "ask_user", "{\"question\":\"?\",\"kind\":\"text\"}",),
            ),
            item_added_frame(
                2,
                1,
                &call_item(
                    "p1",
                    "request_permission",
                    r#"{"kind":"sandbox","mode":"network","operation":null,"path":null,"reason":"download dependencies"}"#
                ),
            ),
            completed_frame(3),
        );
        let body2 = format!(
            "{}{}",
            item_added_frame(1, 0, &message_item("done anyway")),
            completed_frame(2),
        );
        let (base, _server) = spawn_mock_responses(vec![body1, body2]).await;

        let runner = engine_for(&base);
        let mut c = Conversation::new();
        c.add_input_message(user("hi"));
        let conv = Mutex::new(c);
        let cancel = CancellationToken::new();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            runner.run_turn(&conv, &cancel, &HeadlessSurface),
        )
        .await
        .expect("must not hang on ask_user")
        .expect("turn completes");
        assert_eq!(result.final_text, "done anyway");
        // Each interactive call got a denial tool output.
        let denied = conv
            .lock()
            .unwrap()
            .items()
            .filter(|it| {
                matches!(
                    it,
                    MessageItem::ToolOutput { output, failed: true, .. }
                        if matches!(&output.output,
                            async_openai::types::responses::FunctionCallOutput::Text(t)
                                if t.contains("non-interactive"))
                )
            })
            .count();
        assert_eq!(denied, 2, "both interactive tools should be denied");
    }

    /// A surface that decides every `review` the same way and records the
    /// notifications it receives — a stand-in for the TUI / a parent agent.
    struct TestSurface {
        approve: bool,
        events: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl AgentSurface for TestSurface {
        fn on_event(&self, event: RunnerEvent<'_>) {
            let label = match event {
                RunnerEvent::Assistant(t) => format!("assistant:{t}"),
                RunnerEvent::ToolCall { name } => format!("tool:{name}"),
                RunnerEvent::Notice(text) => format!("notice:{text}"),
                // Ephemeral progress events aren't asserted on in these tests.
                RunnerEvent::StreamChunk(_)
                | RunnerEvent::ResponseCommitted
                | RunnerEvent::ResponseAborted { .. }
                | RunnerEvent::Phase(_)
                | RunnerEvent::Activity(_)
                | RunnerEvent::UsageSafePoint { .. }
                | RunnerEvent::WaitingSubagents(_) => return,
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

    fn sync_engine(policy_mode: crate::classifier::WorkMode) -> TurnRunner {
        // A base_url is required to build the client, but `run_calls` never
        // streams, so any address works.
        let client = Client::with_config(OpenAIConfig::new().with_api_base("http://127.0.0.1:1"));
        TurnRunner {
            client,
            model_name: "mock".to_string(),
            model_str: "mock/mock".to_string(),
            tools: std::sync::Arc::new(crate::tools::provider::ToolRegistry::new(vec![
                std::sync::Arc::new(crate::tools::provider::LocalToolProvider::default()),
            ])),
            policy: RunnerPolicy::Sync(policy_mode.classifier()),
            soul: None,
            coauthor: None,
            vision_enabled: true,
            thinking_level: crate::thinking::ThinkingLevel::Auto,
            memory_model: None,
            hooks: Vec::new(),
            stream_retrying: Arc::new(AtomicBool::new(false)),
            stream_retry_limit: crate::consts::MAX_STREAM_RETRIES,
            max_steps: None,
        }
    }

    #[tokio::test]
    async fn ask_verdict_bubbles_to_surface_approve_runs_the_call() {
        // Manual mode asks about write_file; an approving surface lets it run.
        let tmp = std::env::temp_dir().join(format!("engine_review_ok_{}", std::process::id()));
        // JSON-encode the path so Windows backslashes survive as valid JSON.
        let path = serde_json::to_string(&tmp.to_string_lossy()).unwrap();
        let _ = std::fs::remove_file(&tmp);

        let runner = sync_engine(crate::classifier::WorkMode::Manual);
        let conv = Mutex::new(Conversation::new());
        let cancel = CancellationToken::new();
        let surface = TestSurface {
            approve: true,
            events: std::sync::Mutex::new(Vec::new()),
        };
        let calls = vec![FunctionToolCall {
            arguments: format!("{{\"path\":{path},\"content\":\"surfaced\"}}"),
            call_id: "w1".into(),
            namespace: None,
            name: "write_file".into(),
            id: None,
            status: None,
        }];
        let outputs = runner
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
        let path = serde_json::to_string(&tmp.to_string_lossy()).unwrap();
        let _ = std::fs::remove_file(&tmp);

        let runner = sync_engine(crate::classifier::WorkMode::Manual);
        let conv = Mutex::new(Conversation::new());
        let cancel = CancellationToken::new();
        let surface = TestSurface {
            approve: false,
            events: std::sync::Mutex::new(Vec::new()),
        };
        let calls = vec![FunctionToolCall {
            arguments: format!("{{\"path\":{path},\"content\":\"surfaced\"}}"),
            call_id: "w1".into(),
            namespace: None,
            name: "write_file".into(),
            id: None,
            status: None,
        }];
        let outputs = runner
            .run_calls(&conv, calls, &cancel, &surface)
            .await
            .expect("not cancelled");

        assert_eq!(outputs.len(), 1);
        assert!(outputs[0].failed, "denied write is a failed output");
        let denial = match &outputs[0].param.output {
            async_openai::types::responses::FunctionCallOutput::Text(t) => t.clone(),
            _ => String::new(),
        };
        assert!(
            denial.contains("surface refused"),
            "carries surface reason: {denial}"
        );
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
        assert!(
            out.contains("wrote file"),
            "keeps the original output: {out}"
        );
        assert!(
            out.contains("--- Post-edit check ---"),
            "adds the header: {out}"
        );
        assert!(out.contains("2 new errors"), "carries the feedback: {out}");
        // Folded into the output, so no separate system note.
        assert!(
            !conv
                .lock()
                .unwrap()
                .items()
                .any(|it| matches!(it, MessageItem::Meta { .. }))
        );
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
        // With the feedback hooks disabled, a write_file
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
                &call_item(
                    "w1",
                    "write_file",
                    &format!("{{\"path\":\"{path}\",\"content\":\"x\"}}")
                ),
            ),
            completed_frame(2),
        );
        let body2 = format!(
            "{}{}",
            item_added_frame(1, 0, &message_item("done")),
            completed_frame(2),
        );
        let (base, _server) = spawn_mock_responses(vec![body1, body2]).await;

        let runner = engine_for(&base); // Yolo; diagnostics disabled by default.
        let mut c = Conversation::new();
        c.add_input_message(user("write it"));
        let conv = Mutex::new(c);
        let cancel = CancellationToken::new();
        runner
            .run_turn(&conv, &cancel, &HeadlessSurface)
            .await
            .expect("turn completes");

        assert!(
            !conv
                .lock()
                .unwrap()
                .items()
                .any(|it| matches!(it, MessageItem::Meta { .. })),
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

    #[test]
    fn ensure_tool_output_pairing_adds_synthetic_outputs() {
        use async_openai::types::responses::{FunctionCallOutput, OutputItem, OutputStatus};
        let conv = Mutex::new(Conversation::new());

        // Simulate: two function calls committed, only one has an output.
        conv.lock()
            .unwrap()
            .add_output(OutputItem::FunctionCall(FunctionToolCall {
                arguments: "{}".into(),
                call_id: "call_1".into(),
                namespace: None,
                name: "read_file".into(),
                id: Some("fc_1".into()),
                status: Some(OutputStatus::Completed),
            }));
        conv.lock()
            .unwrap()
            .add_output(OutputItem::FunctionCall(FunctionToolCall {
                arguments: "{}".into(),
                call_id: "call_2".into(),
                namespace: None,
                name: "write_file".into(),
                id: Some("fc_2".into()),
                status: Some(OutputStatus::Completed),
            }));
        conv.lock()
            .unwrap()
            .add_tool_output(crate::tools::ToolOutput {
                param: async_openai::types::responses::FunctionCallOutputItemParam {
                    call_id: "call_1".into(),
                    output: FunctionCallOutput::Text("ok".into()),
                    id: None,
                    status: None,
                },
                failed: false,
                approval_label: None,
            });

        ensure_tool_output_pairing(&conv);

        // Now both call_ids should have outputs.
        let items: Vec<_> = conv.lock().unwrap().items().cloned().collect();
        let outputs: Vec<_> = items
            .iter()
            .filter_map(|it| match it {
                MessageItem::ToolOutput { output, .. } => Some(output.call_id.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(outputs.len(), 2);
        assert!(outputs.contains(&"call_1".to_string()));
        assert!(outputs.contains(&"call_2".to_string()));
        // The synthetic output for call_2 should be marked failed and cancelled.
        let syn: Vec<_> = items
            .iter()
            .filter_map(|it| match it {
                MessageItem::ToolOutput {
                    output,
                    failed,
                    approval_label,
                } if output.call_id == "call_2" => Some((*failed, approval_label.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(syn.len(), 1);
        assert!(syn[0].0, "synthetic output is failed");
        assert!(
            syn[0].1.as_deref().is_some_and(|l| l.contains("cancelled")),
            "synthetic output label mentions cancellation"
        );
    }

    #[test]
    fn ensure_tool_output_pairing_is_idempotent() {
        use async_openai::types::responses::{FunctionCallOutput, OutputItem, OutputStatus};
        let conv = Mutex::new(Conversation::new());

        conv.lock()
            .unwrap()
            .add_output(OutputItem::FunctionCall(FunctionToolCall {
                arguments: "{}".into(),
                call_id: "c1".into(),
                namespace: None,
                name: "read_file".into(),
                id: Some("fc_1".into()),
                status: Some(OutputStatus::Completed),
            }));
        conv.lock()
            .unwrap()
            .add_tool_output(crate::tools::ToolOutput {
                param: async_openai::types::responses::FunctionCallOutputItemParam {
                    call_id: "c1".into(),
                    output: FunctionCallOutput::Text("ok".into()),
                    id: None,
                    status: None,
                },
                failed: false,
                approval_label: None,
            });

        // Called twice; should not add duplicates.
        ensure_tool_output_pairing(&conv);
        ensure_tool_output_pairing(&conv);

        let outputs: Vec<_> = conv
            .lock()
            .unwrap()
            .items()
            .filter_map(|it| match it {
                MessageItem::ToolOutput { output, .. } => Some(output.call_id.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(outputs.len(), 1, "should not duplicate outputs");
    }

    #[tokio::test]
    async fn review_can_be_interrupted_by_cancel_token() {
        use crate::cancel::CancellationToken;
        use crate::runner::AgentSurface;
        use crate::ui::event::{AppEvent, Event};
        use std::time::Duration;
        use tokio::sync::mpsc;

        let (tx, mut rx) = mpsc::unbounded_channel::<Event>();
        let cancel = CancellationToken::new();
        let surface = crate::app::surface::TuiSurface {
            tx: tx.clone(),
            skill_prompt: None,
            plan_prompt: None,
            approval_label: "test".into(),
            operation_id: crate::cancel::OperationId(1),
            cancel: cancel.clone(),
        };

        // Spawn review() in a task — it will block waiting for a reply.
        let review_handle = tokio::spawn({
            let surface_call = FunctionToolCall {
                arguments: "{}".into(),
                call_id: "call_review".into(),
                namespace: None,
                name: "write_file".into(),
                id: Some("fc_review".into()),
                status: Some(async_openai::types::responses::OutputStatus::InProgress),
            };
            async move { surface.review(&surface_call, "test reason", (1, 3)).await }
        });

        // Wait for the ReviewRequest to arrive on the channel.
        let ev = tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .expect("review request should be sent")
            .expect("channel open");
        assert!(
            matches!(ev, Event::App(AppEvent::ReviewRequest { .. })),
            "expected ReviewRequest event"
        );

        // Cancel before sending a reply — review() should unblock with Deny.
        cancel.cancel();

        let decision = tokio::time::timeout(Duration::from_millis(500), review_handle)
            .await
            .expect("review should complete")
            .expect("review task should not panic");

        assert!(
            matches!(decision, crate::runner::ReviewDecision::Deny { .. }),
            "cancelled review should return Deny"
        );
    }
}
