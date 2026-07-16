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

//! Tool-call orchestration: classification (sync and async), execution, and
//! post-execution helpers.

use super::App;
use super::helpers;
use crate::response::message_item::MessageItem;
use crate::ui::components::conversation_panel::conversation_panel::ActivePhase;
use crate::ui::event::{AppEvent, Event};
use async_openai::types::responses::{FunctionCallOutput, FunctionToolCall, OutputItem};
use futures::StreamExt;
use std::collections::HashMap;
use crate::cancel::CancellationToken;

use crate::consts::{
    CLASSIFIER_ASK_OUTPUT_CHARS, CLASSIFIER_CALL_ARGS_CHARS, CLASSIFIER_LIGHT_MSG_CHARS,
    MAX_CONCURRENT_CLASSIFICATIONS,
};

// ---------------------------------------------------------------------------
// Main entry: run tool calls (dispatching by mode)
// ---------------------------------------------------------------------------

/// Runs the model's requested tool calls in the background, then reports the
/// outputs back to the event loop via `ToolCallsCompleted`.
///
/// Classification is always done asynchronously via a spawned task for every
/// mode: Auto mode makes LLM calls in the background; all other modes run
/// their sync classifier in a spawned task so the event loop is never blocked
/// and can always process Cancel / UI events.
pub(crate) fn run_tool_calls(
    app: &mut App<'_>,
    calls: Vec<FunctionToolCall>,
    cancel_token: CancellationToken,
) {
    app.conversation_panel.phase = ActivePhase::Classifying;
    // `cancel_token` is a child of the turn's root token (`app.cancel.active`),
    // so Esc — which cancels the root — reaches the classification/tool phases
    // even though the stream that started the turn is already finished.

    if app.work_mode.uses_llm_classifier() {
        spawn_auto_classification(app, calls, cancel_token);
    } else {
        spawn_sync_classification(app, calls, cancel_token);
    }
}

/// Spawn sync classification for non-Auto modes. The sync classifier runs
/// in a spawned task and sends its verdicts back via
/// [`AppEvent::ClassificationCompleted`] just like Auto mode, keeping the
/// event loop unblocked.
fn spawn_sync_classification(
    app: &mut App<'_>,
    calls: Vec<FunctionToolCall>,
    cancel_token: CancellationToken,
) {
    let classifier = app.work_mode.classifier(build_mcp_policy_map(app));
    let sender = app.events.sender.clone();

    tokio::spawn(async move {
        if cancel_token.is_cancelled() {
            return;
        }

        let mut allowed: Vec<FunctionToolCall> = Vec::new();
        let mut denied: Vec<crate::tools::ToolOutput> = Vec::new();
        let mut ask_queue: Vec<(FunctionToolCall, String)> = Vec::new();

        for call in &calls {
            if cancel_token.is_cancelled() {
                return;
            }
            match classifier.classify(&call.name, &call.arguments) {
                crate::classifier::Verdict::Allow => allowed.push(call.clone()),
                crate::classifier::Verdict::Deny { reason } => {
                    denied.push(helpers::classifier_denied_output(call, &reason))
                }
                crate::classifier::Verdict::Ask { reason } => {
                    ask_queue.push((call.clone(), reason))
                }
            }
        }

        let _ = sender.send(Event::App(AppEvent::ClassificationCompleted {
            allowed,
            denied,
            ask_queue,
            cancel_token,
        }));
    });
}

/// Process final classification verdicts: run allowed tools, report denied
/// outputs back to the model. All classification (sync or LLM) has already
/// happened in the spawned task; verdicts here are final.
pub(crate) fn process_classification_results(
    app: &mut App<'_>,
    allowed: Vec<FunctionToolCall>,
    denied: Vec<crate::tools::ToolOutput>,
    cancel_token: CancellationToken,
) {
    if cancel_token.is_cancelled() {
        app.conversation_panel.phase = ActivePhase::None;
        return;
    }
    if allowed.is_empty() && denied.is_empty() {
        app.conversation_panel.phase = ActivePhase::None;
        return;
    }

    let mode_icon = app.work_mode.icon().to_string();
    let mode_name = app.work_mode.label().to_string();
    spawn_run(app, allowed, denied, cancel_token, mode_icon, mode_name);
}

// ---------------------------------------------------------------------------
// Auto mode: LLM classification
// ---------------------------------------------------------------------------

/// Auto mode: classify each mutating call with the LLM, then hand the
/// verdicts back via [`AppEvent::ClassificationCompleted`].
fn spawn_auto_classification(
    app: &mut App<'_>,
    calls: Vec<FunctionToolCall>,
    cancel_token: CancellationToken,
) {
    app.conversation_panel.phase = ActivePhase::Classifying;

    let model_str = app
        .config
        .classifier_model
        .clone()
        .unwrap_or_else(|| app.current_model.clone());
    let (client, model_name) = match app.provider_manager.resolve(&model_str) {
        Some((c, m)) => (c.clone(), m),
        None => {
            app.conversation_panel.add_error_string(format!(
                "classifier model '{model_str}' not found — set a valid \
                 classifier_model (or /classifier <provider/model>)"
            ));
            app.conversation_panel.phase = ActivePhase::None;
            return;
        }
    };

    let sender = app.events.sender.clone();
    let no_lp = app.classifier_no_logprobs.clone();
    let (light_context, full_context) = build_classifier_context(app);
    let mcp_policies = build_mcp_policy_map(app);

    tokio::spawn(async move {
        enum Decision {
            Allow(FunctionToolCall),
            Deny(crate::tools::ToolOutput),
        }

        let decisions: Vec<Option<Decision>> = futures::stream::iter(
            calls.into_iter().map(|call| {
                let client = &client;
                let model_name = &model_name;
                let no_lp = &no_lp;
                let light_context = &light_context;
                let full_context = &full_context;
                let cancel_token = &cancel_token;
                let mcp_policies = &mcp_policies;
                async move {
                    if cancel_token.is_cancelled() {
                        return None;
                    }
                    if let Some(verdict) =
                        crate::classifier::classify_mcp_policy(&call.name, mcp_policies)
                    {
                        return Some(match verdict {
                            crate::classifier::Verdict::Allow => Decision::Allow(call),
                            crate::classifier::Verdict::Ask { reason }
                            | crate::classifier::Verdict::Deny { reason } => {
                                Decision::Deny(helpers::classifier_denied_output(&call, &reason))
                            }
                        });
                    }
                    if !crate::classifier::needs_review(&call.name, &call.arguments) {
                        return Some(Decision::Allow(call));
                    }

                    let try_logprobs = !no_lp.lock().unwrap().contains(model_name);
                    let outcome = crate::classifier::classify_tool_call(
                        client,
                        model_name,
                        &call.name,
                        &call.arguments,
                        light_context,
                        full_context,
                        try_logprobs,
                    )
                    .await;
                    if outcome.logprobs_missing {
                        no_lp.lock().unwrap().insert(model_name.clone());
                    }

                    Some(match outcome.verdict {
                        crate::classifier::Verdict::Allow => Decision::Allow(call),
                        crate::classifier::Verdict::Deny { reason }
                        | crate::classifier::Verdict::Ask { reason } => {
                            Decision::Deny(helpers::classifier_denied_output(&call, &reason))
                        }
                    })
                }
            }),
        )
        .buffered(MAX_CONCURRENT_CLASSIFICATIONS)
        .collect()
        .await;

        if cancel_token.is_cancelled() {
            return;
        }

        let mut allowed: Vec<FunctionToolCall> = Vec::new();
        let mut denied: Vec<crate::tools::ToolOutput> = Vec::new();
        for decision in decisions.into_iter().flatten() {
            match decision {
                Decision::Allow(call) => allowed.push(call),
                Decision::Deny(output) => denied.push(output),
            }
        }

        let _ = sender.send(Event::App(AppEvent::ClassificationCompleted {
            allowed,
            denied,
            ask_queue: Vec::new(),
            cancel_token,
        }));
    });
}

// ---------------------------------------------------------------------------
// Classifier context builders
// ---------------------------------------------------------------------------

/// Build a map of MCP server name → [`McpPolicy`] from the config.
pub(crate) fn build_mcp_policy_map(app: &App<'_>) -> HashMap<String, crate::mcp::types::McpPolicy> {
    app.config
        .mcp_servers
        .iter()
        .map(|s| (s.name.clone(), s.auto_approve))
        .collect()
}

/// Pull readable text out of an assistant's output message (concatenate all
/// `OutputText` parts, skip refusals).
fn extract_msg_text(msg: &async_openai::types::responses::OutputMessage) -> String {
    use async_openai::types::responses::OutputMessageContent;
    msg.content
        .iter()
        .filter_map(|c| match c {
            OutputMessageContent::OutputText(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

/// Build light and full classifier context strings from the conversation.
///
/// `light_context` carries the last few user+assistant exchanges so the fast
/// yes/no path has enough to understand short replies like "好的".
///
/// `full_context` carries the **complete** conversation transcript (every user
/// message, every assistant message, every tool call with args, every tool
/// output), skipping only reasoning blocks and internal errors.  Large tool
/// outputs are truncated to keep the context manageable.
pub(crate) fn build_classifier_context(app: &App<'_>) -> (String, String) {
    let items: Vec<&MessageItem> = app.conversation_panel.items().collect();

    // ---- light context ---------------------------------------------------
    let mut light = Vec::new();

    if let Ok(cwd) = std::env::current_dir() {
        light.push(format!("Working directory: {}", cwd.display()));
    }

    // Walk backwards collecting the last few user/assistant messages.
    // Tool calls and tool outputs are omitted from the light path — it only
    // needs conversational context to anchor a yes/no decision.
    {
        let mut recent: Vec<String> = Vec::new();
        let mut user_count = 0u32;
        for it in items.iter().rev() {
            match it {
                MessageItem::Input(input) => {
                    if let Some(text) = helpers::extract_input_text(input) {
                        recent.push(format!(
                            "[User]\n{}",
                            helpers::truncate_chars(text.trim(), CLASSIFIER_LIGHT_MSG_CHARS)
                        ));
                        user_count += 1;
                        if user_count >= 3 {
                            break;
                        }
                    }
                }
                MessageItem::Output(OutputItem::Message(msg)) => {
                    let text = extract_msg_text(msg);
                    if !text.is_empty() {
                        recent.push(format!(
                            "[Assistant]\n{}",
                            helpers::truncate_chars(text.trim(), CLASSIFIER_LIGHT_MSG_CHARS)
                        ));
                    }
                }
                _ => {}
            }
        }
        recent.reverse();
        light.extend(recent);
    }

    // ---- full context ----------------------------------------------------
    let mut full_ctx: Vec<String> = Vec::new();

    if let Ok(cwd) = std::env::current_dir() {
        full_ctx.push(format!("Working directory: {}", cwd.display()));
    }

    // Map call_id → tool name so we can label tool outputs.
    let call_meta: HashMap<&str, &str> = items
        .iter()
        .filter_map(|it| match it {
            MessageItem::Output(OutputItem::FunctionCall(fc)) => {
                Some((fc.call_id.as_str(), fc.name.as_str()))
            }
            _ => None,
        })
        .collect();

    for it in &items {
        match it {
            MessageItem::Input(input) => {
                if let Some(text) = helpers::extract_input_text(input) {
                    full_ctx.push(format!("\n[User]\n{}", text.trim()));
                }
            }
            MessageItem::Output(OutputItem::Message(msg)) => {
                let text = extract_msg_text(msg);
                if !text.is_empty() {
                    full_ctx.push(format!("\n[Assistant]\n{}", text.trim()));
                }
            }
            MessageItem::Output(OutputItem::Reasoning(_)) => {
                // Skip — internal model chatter, not useful for classification.
            }
            MessageItem::Output(OutputItem::FunctionCall(call)) => {
                full_ctx.push(format!(
                    "\n[Tool call: {}]\n{}",
                    call.name,
                    helpers::truncate_chars(&call.arguments, CLASSIFIER_CALL_ARGS_CHARS)
                ));
            }
            MessageItem::Output(_) => {
                // Other output types (file search, web search, etc.) —
                // not useful for classification, skip.
            }
            MessageItem::ToolOutput {
                output: fco,
                failed,
                approval_label,
            } => {
                let name = call_meta
                    .get(fco.call_id.as_str())
                    .copied()
                    .unwrap_or(fco.call_id.as_str());
                let status = if *failed { "FAILED" } else { "ok" };
                // ask_user: pass the full answer so the classifier knows
                // what the user explicitly approved/denied.
                if name == crate::tools::ask_user::NAME {
                    let text = match &fco.output {
                        FunctionCallOutput::Text(t) => {
                            helpers::truncate_chars(t.trim(), CLASSIFIER_ASK_OUTPUT_CHARS)
                        }
                        _ => String::new(),
                    };
                    let mut line = format!("\n[Tool output: {name}] ({status})");
                    if let Some(label) = approval_label {
                        line.push_str(&format!(" {label}"));
                    }
                    if text.is_empty() {
                        line.push_str("\n(empty)");
                    } else {
                        line.push_str(&format!("\n{text}"));
                    }
                    full_ctx.push(line);
                } else {
                    // Other tools: only pass status + approval label,
                    // never the output text (untrusted external content).
                    let mut line = format!("\n[Tool output: {name}] ({status})");
                    if let Some(label) = approval_label {
                        line.push_str(&format!(" {label}"));
                    }
                    full_ctx.push(line);
                }
            }
            MessageItem::OpenAIError(e) => {
                full_ctx.push(format!("\n[Error]\n{}", e));
            }
            MessageItem::Error(s) => {
                full_ctx.push(format!("\n[Error]\n{}", s));
            }
            MessageItem::Warning(s) => {
                full_ctx.push(format!("\n[Warning]\n{}", s));
            }
            MessageItem::Info(s) => {
                full_ctx.push(format!("\n[Info]\n{}", s));
            }
            MessageItem::Meta { label, text } => {
                full_ctx.push(format!("\n[{label}]\n{text}"));
            }
            MessageItem::Usage(_, _) => {
                // Token usage counters — not useful for classification.
            }
        }
    }

    (light.join("\n\n"), full_ctx.join("\n"))
}

// ---------------------------------------------------------------------------
// Execution helpers
// ---------------------------------------------------------------------------

/// Whether any output in this batch was produced by a file-editing tool.
pub(crate) fn batch_edited_files(app: &App<'_>, outputs: &[crate::tools::ToolOutput]) -> bool {
    let names: std::collections::HashMap<&str, &str> = app
        .conversation_panel
        .items()
        .filter_map(|it| match it {
            MessageItem::Output(OutputItem::FunctionCall(fc)) => {
                Some((fc.call_id.as_str(), fc.name.as_str()))
            }
            _ => None,
        })
        .collect();
    outputs.iter().any(|o| {
        matches!(
            names.get(o.param.call_id.as_str()).copied(),
            Some(crate::tools::write_file::NAME) | Some(crate::tools::edit_file::NAME)
        )
    })
}

/// Whether a tool has no side effects and so may run concurrently with other
/// such calls. Only read-only tools qualify; everything else (writes, shell
/// commands, MCP calls, and anything unrecognised) runs serially so a read can
/// never race ahead of a write it might depend on.
fn is_parallel_safe(name: &str) -> bool {
    matches!(
        name,
        crate::tools::read_file::NAME
            | crate::tools::grep::NAME
            | crate::tools::blob::NAME
    )
}

/// Execute one tool call and stamp the approval label (unless the classifier
/// already set one). Takes everything by value so each future is self-contained
/// and can be driven concurrently in a `buffered` stream; the captured handles
/// (`sender`, `mcp`) are cheap `Arc`-backed clones.
async fn run_labeled_call(
    call: FunctionToolCall,
    sender: tokio::sync::mpsc::UnboundedSender<Event>,
    mcp: Option<std::sync::Arc<crate::mcp::McpManager>>,
    label: String,
) -> crate::tools::ToolOutput {
    let mut out = crate::tools::run_tool_call(&call, &sender, mcp.as_deref()).await;
    if out.approval_label.is_none() {
        out.approval_label = Some(label);
    }
    out
}

/// Run `allowed` tool calls in the background, prepend the `denied` outputs,
/// and report everything back via [`AppEvent::ToolCallsCompleted`].
///
/// Consecutive read-only calls run concurrently (bounded by
/// [`MAX_CONCURRENT_READ_TOOLS`]); writes and other side-effecting tools run one
/// at a time. Output order always matches call order, and the ordering between a
/// write and the reads around it is preserved, so a read never observes a
/// half-applied write.
fn spawn_run(
    app: &mut App<'_>,
    allowed: Vec<FunctionToolCall>,
    denied: Vec<crate::tools::ToolOutput>,
    cancel_token: CancellationToken,
    mode_icon: String,
    mode_name: String,
) {
    app.conversation_panel.phase = ActivePhase::ToolRunning;
    let sender = app.events.sender.clone();
    let mcp = app.mcp_manager.clone();
    let label = format!("{mode_icon} approved by {mode_name} mode");
    tokio::spawn(async move {
        let mut outputs = denied;
        let mut i = 0;
        while i < allowed.len() {
            if cancel_token.is_cancelled() {
                break;
            }
            if is_parallel_safe(&allowed[i].name) {
                // Take the maximal run of consecutive read-only calls and run
                // them concurrently, preserving order.
                let start = i;
                while i < allowed.len() && is_parallel_safe(&allowed[i].name) {
                    i += 1;
                }
                let futs: Vec<_> = allowed[start..i]
                    .iter()
                    .map(|call| {
                        run_labeled_call(call.clone(), sender.clone(), mcp.clone(), label.clone())
                    })
                    .collect();
                let mut batch: Vec<crate::tools::ToolOutput> = futures::stream::iter(futs)
                    .buffered(crate::consts::MAX_CONCURRENT_READ_TOOLS)
                    .collect()
                    .await;
                outputs.append(&mut batch);
            } else {
                let out =
                    run_labeled_call(allowed[i].clone(), sender.clone(), mcp.clone(), label.clone())
                        .await;
                outputs.push(out);
                i += 1;
            }
        }
        let _ = sender.send(Event::App(AppEvent::ToolCallsCompleted(
            outputs,
            cancel_token,
        )));
    });
}

#[cfg(test)]
mod tests {
    // Tests for build_classifier_context require an App instance and are
    // covered by integration tests (manual mode or Auto mode with a real
    // conversation).
}
