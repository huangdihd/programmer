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
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Maximum number of Auto-mode classifier LLM requests in flight at once.
const MAX_CONCURRENT_CLASSIFICATIONS: usize = 4;

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
    cancel_token: Arc<AtomicBool>,
) {
    app.conversation_panel.phase = ActivePhase::Classifying;

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
    cancel_token: Arc<AtomicBool>,
) {
    let classifier = app.work_mode.classifier(build_mcp_policy_map(app));
    let sender = app.events.sender.clone();

    tokio::spawn(async move {
        if cancel_token.load(Ordering::Relaxed) {
            return;
        }

        let mut allowed: Vec<FunctionToolCall> = Vec::new();
        let mut denied: Vec<crate::tools::ToolOutput> = Vec::new();
        let mut ask_queue: Vec<(FunctionToolCall, String)> = Vec::new();

        for call in &calls {
            if cancel_token.load(Ordering::Relaxed) {
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
    cancel_token: Arc<AtomicBool>,
) {
    if cancel_token.load(Ordering::Relaxed) {
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
    cancel_token: Arc<AtomicBool>,
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
                    if cancel_token.load(Ordering::Relaxed) {
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
                    if !crate::classifier::needs_review(&call.name) {
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

        if cancel_token.load(Ordering::Relaxed) {
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

/// Extract ask_user Q&A summaries from conversation items for the classifier.
/// Each entry is a two-line summary: the question the model asked and the
/// answer the user gave. Returns empty Vec if no ask_user calls are found.
fn build_ask_user_qa<'a>(
    items: &[&'a MessageItem],
    call_meta: &std::collections::HashMap<&'a str, (&'a str, &'a str)>,
) -> Vec<String> {
    let mut qa: Vec<String> = Vec::new();
    for it in items.iter() {
        if let MessageItem::ToolOutput { output: fco, .. } = it {
            if let Some((name, args_json)) = call_meta.get(fco.call_id.as_str()) {
                if *name == crate::tools::ask_user::NAME {
                    let question = serde_json::from_str::<serde_json::Value>(args_json)
                        .ok()
                        .and_then(|v| v.get("question")?.as_str().map(String::from))
                        .unwrap_or_else(|| "(parse error)".to_string());
                    let answer = match &fco.output {
                        FunctionCallOutput::Text(t) => helpers::truncate_chars(t.trim(), 500),
                        _ => "(no text)".to_string(),
                    };
                    qa.push(format!(
                        "The model asked the user: \"{}\"\nThe user answered: \"{}\"",
                        question, answer
                    ));
                }
            }
        }
    }
    qa
}

/// Build light and full classifier context strings from the conversation.
pub(crate) fn build_classifier_context(app: &App<'_>) -> (String, String) {
    let mut light = Vec::new();
    let mut full: Vec<String> = Vec::new();

    if let Ok(cwd) = std::env::current_dir() {
        let dir = format!("Working directory: {}", cwd.display());
        light.push(dir.clone());
        full.push(dir);
    }

    let items: Vec<&MessageItem> = app.conversation_panel.items().collect();

    if let Some(msg) = items.iter().rev().find_map(|it| match it {
        MessageItem::Input(input) => helpers::extract_input_text(input),
        _ => None,
    }) {
        let user = format!(
            "User's latest request:\n{}",
            helpers::truncate_chars(msg.trim(), 800)
        );
        light.push(user.clone());
        full.push(user);
    }

    let assistant_count = items
        .iter()
        .rev()
        .filter(|it| matches!(it, MessageItem::Output(OutputItem::Message(_))))
        .take(3)
        .count();
    if assistant_count > 0 {
        full.push(format!("Assistant has sent {assistant_count} message(s) this turn."));
    }

    // Collect function-call metadata keyed by call_id: (tool_name, arguments_json).
    let call_meta: std::collections::HashMap<&str, (&str, &str)> = items
        .iter()
        .filter_map(|it| match it {
            MessageItem::Output(OutputItem::FunctionCall(fc)) => {
                Some((fc.call_id.as_str(), (fc.name.as_str(), fc.arguments.as_str())))
            }
            _ => None,
        })
        .collect();

    // Build ask_user Q&A summaries for the classifier.
    let ask_user_qa = build_ask_user_qa(&items, &call_meta);

    let tool_outcomes: Vec<String> = items
        .iter()
        .rev()
        .filter_map(|it| match it {
            MessageItem::Output(OutputItem::FunctionCall(fc)) => {
                Some(format!("  {} — pending", fc.name))
            }
            MessageItem::ToolOutput { output: fco, failed, .. } => {
                let output_text = match &fco.output {
                    FunctionCallOutput::Text(t) => t.as_str(),
                    _ => "",
                };
                let name = call_meta
                    .get(fco.call_id.as_str())
                    .map(|(n, _)| *n)
                    .unwrap_or(fco.call_id.as_str());
                let len = output_text.len();
                let status = if output_text.is_empty() {
                    "empty"
                } else if *failed {
                    "FAILED"
                } else {
                    "ok"
                };
                Some(format!("  {name} — {status}, {len} chars"))
            }
            _ => None,
        })
        .take(10)
        .collect();

    // Push ask_user Q&A into both light and full context — it is crucial
    // for the classifier to know what the user explicitly approved/denied.
    for qa in &ask_user_qa {
        light.push(qa.clone());
        full.push(qa.clone());
    }

    if !tool_outcomes.is_empty() {
        full.push(format!(
            "Tool calls this turn:\n{}",
            tool_outcomes.into_iter().rev().collect::<Vec<_>>().join("\n")
        ));
    }

    (light.join("\n\n"), full.join("\n\n"))
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

/// Run `allowed` tool calls in the background, prepend the `denied` outputs,
/// and report everything back via [`AppEvent::ToolCallsCompleted`].
fn spawn_run(
    app: &mut App<'_>,
    allowed: Vec<FunctionToolCall>,
    denied: Vec<crate::tools::ToolOutput>,
    cancel_token: Arc<AtomicBool>,
    mode_icon: String,
    mode_name: String,
) {
    app.conversation_panel.phase = ActivePhase::ToolRunning;
    let sender = app.events.sender.clone();
    let mcp = app.mcp_manager.clone();
    tokio::spawn(async move {
        let mut outputs = denied;
        for call in &allowed {
            if cancel_token.load(Ordering::Relaxed) {
                break;
            }
            let mut out = crate::tools::run_tool_call(call, &sender, mcp.as_deref()).await;
            // Only set if the classifier_denied_output path didn't already set one.
            if out.approval_label.is_none() {
                out.approval_label = Some(format!("{mode_icon} approved by {mode_name} mode"));
            }
            outputs.push(out);
        }
        let _ = sender.send(Event::App(AppEvent::ToolCallsCompleted(
            outputs,
            cancel_token,
        )));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_openai::types::responses::{FunctionCallOutput, FunctionCallOutputItemParam};
    use serde_json::json;

    fn make_fc_call(id: &str, name: &str, args: &str) -> MessageItem {
        MessageItem::Output(OutputItem::FunctionCall(
            serde_json::from_value(json!({
                "id": format!("fc_{id}"),
                "call_id": id,
                "name": name,
                "arguments": args,
                "type": "function_call",
                "status": "completed",
            }))
            .unwrap(),
        ))
    }

    fn make_tool_output(call_id: &str, text: &str) -> MessageItem {
        MessageItem::ToolOutput {
            output: FunctionCallOutputItemParam {
                call_id: call_id.to_string(),
                output: FunctionCallOutput::Text(text.to_string()),
                id: None,
                status: None,
            },
            failed: false,
            approval_label: None,
        }
    }

    #[test]
    fn ask_user_qa_included() {
        let items = vec![
            make_fc_call(
                "call_1",
                "ask_user",
                &json!({"question": "Can I create a file?", "kind": "yes_no"}).to_string(),
            ),
            make_tool_output("call_1", "Yes"),
        ];
        let item_refs: Vec<&MessageItem> = items.iter().collect();
        let call_meta: std::collections::HashMap<&str, (&str, &str)> = item_refs
            .iter()
            .filter_map(|it| match it {
                MessageItem::Output(OutputItem::FunctionCall(fc)) => {
                    Some((fc.call_id.as_str(), (fc.name.as_str(), fc.arguments.as_str())))
                }
                _ => None,
            })
            .collect();

        let qa = build_ask_user_qa(&item_refs, &call_meta);
        assert_eq!(qa.len(), 1);
        assert!(qa[0].contains("Can I create a file?"));
        assert!(qa[0].contains("Yes"));
    }

    #[test]
    fn ask_user_qa_empty_when_no_ask_user() {
        let items = vec![
            make_fc_call("call_1", "command", r#"{"command":"ls"}"#),
            make_tool_output("call_1", "file1.txt"),
        ];
        let item_refs: Vec<&MessageItem> = items.iter().collect();
        let call_meta: std::collections::HashMap<&str, (&str, &str)> = item_refs
            .iter()
            .filter_map(|it| match it {
                MessageItem::Output(OutputItem::FunctionCall(fc)) => {
                    Some((fc.call_id.as_str(), (fc.name.as_str(), fc.arguments.as_str())))
                }
                _ => None,
            })
            .collect();

        let qa = build_ask_user_qa(&item_refs, &call_meta);
        assert!(qa.is_empty());
    }

    #[test]
    fn ask_user_qa_missing_answer_no_panic() {
        // ToolOutput exists but no matching FunctionCall in call_meta — should not crash.
        let items = vec![make_tool_output("orphan_call", "orphan answer")];
        let item_refs: Vec<&MessageItem> = items.iter().collect();
        let call_meta: std::collections::HashMap<&str, (&str, &str)> =
            std::collections::HashMap::new();

        let qa = build_ask_user_qa(&item_refs, &call_meta);
        assert!(qa.is_empty());
    }
}
