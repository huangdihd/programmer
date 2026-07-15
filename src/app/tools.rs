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
pub(crate) async fn run_tool_calls(
    app: &mut App<'_>,
    calls: Vec<FunctionToolCall>,
    cancel_token: Arc<AtomicBool>,
) {
    let mcp_policies = build_mcp_policy_map(app);

    // Build one Arc<dyn Classifier> for every mode — the only divergence
    // between Auto and others is which implementation sits behind the trait.
    let classifier: Arc<dyn crate::classifier::Classifier> =
        if app.work_mode == crate::classifier::WorkMode::Auto {
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
            let (light_context, full_context) = build_classifier_context(app);
            Arc::new(crate::classifier::AutoClassifier::new(
                mcp_policies,
                crate::classifier::AutoClassifierParams {
                    client,
                    model: model_name,
                    light_context,
                    full_context,
                    no_logprobs: app.classifier_no_logprobs.clone(),
                },
            ))
        } else {
            app.work_mode.classifier(mcp_policies, None)
        };

    app.conversation_panel.phase = ActivePhase::Classifying;

    // Unified concurrent pipeline for every mode. Sync classifiers are
    // instant, so `buffered` degenerates to a fast sequential fold.
    let outcomes: Vec<Option<(FunctionToolCall, crate::classifier::Verdict)>> =
        futures::stream::iter(calls.into_iter().map(|call| {
            let classifier = classifier.clone();
            let ct = &cancel_token;
            async move {
                if ct.load(Ordering::Relaxed) {
                    return None;
                }
                let verdict = classifier.classify(&call.name, &call.arguments).await;
                Some((call, verdict))
            }
        }))
        .buffered(MAX_CONCURRENT_CLASSIFICATIONS)
        .collect()
        .await;

    if cancel_token.load(Ordering::Relaxed) {
        app.conversation_panel.phase = ActivePhase::None;
        return;
    }

    let mut allowed: Vec<FunctionToolCall> = Vec::new();
    let mut queued: Vec<(FunctionToolCall, String)> = Vec::new();
    let mut denied: Vec<crate::tools::ToolOutput> = Vec::new();

    for (call, verdict) in outcomes.into_iter().flatten() {
        match verdict {
            crate::classifier::Verdict::Allow => allowed.push(call),
            crate::classifier::Verdict::Deny { reason } => {
                denied.push(helpers::classifier_denied_output(&call, &reason))
            }
            crate::classifier::Verdict::Ask { reason } => {
                queued.push((call, reason))
            }
        }
    }

    if !queued.is_empty() {
        for output in denied {
            app.conversation_panel.add_tool_output(output);
        }
        app.approval_queue = queued;
        app.conversation_panel.phase = ActivePhase::None;
        return;
    }

    if allowed.is_empty() && denied.is_empty() {
        app.conversation_panel.phase = ActivePhase::None;
        return;
    }

    let label = app.work_mode.label().to_string();
    spawn_run(app, allowed, denied, cancel_token, label);
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
            MessageItem::ToolOutput { output: fco, failed } => {
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
    _label: String,
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
            let out = crate::tools::run_tool_call(call, &sender, mcp.as_deref()).await;
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
