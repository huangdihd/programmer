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

//! Tool-call classification, free of any `App` or event channel. The two cores
//! — the synchronous rule classifier and the Auto-mode LLM classifier — return
//! a [`ClassificationOutcome`] instead of sending an event, so the TUI wraps
//! them in a spawned task that forwards the result, while the headless runner
//! calls them inline.

use crate::cancel::CancellationToken;
use crate::classifier::{Classifier, Verdict};
use crate::response::message_item::MessageItem;
use async_openai::Client;
use async_openai::config::OpenAIConfig;
use async_openai::types::responses::{FunctionCallOutput, FunctionToolCall, OutputItem};
use futures::StreamExt;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use crate::consts::{
    CLASSIFIER_ASK_OUTPUT_CHARS, CLASSIFIER_CALL_ARGS_CHARS, CLASSIFIER_LIGHT_MSG_CHARS,
    MAX_CONCURRENT_CLASSIFICATIONS,
};

/// The partitioned result of classifying a batch of tool calls: those cleared
/// to run, those blocked (with a denial output to feed back to the model), and
/// those needing user approval (Manual mode only; the LLM core never produces
/// these).
#[derive(Default)]
pub(crate) struct ClassificationOutcome {
    pub allowed: Vec<FunctionToolCall>,
    pub denied: Vec<crate::tools::ToolOutput>,
    pub ask: Vec<(FunctionToolCall, String)>,
}

/// Build a `function_call_output` carrying a classifier denial, fed back to the
/// model so it learns why the call was blocked and can adjust.
pub(crate) fn classifier_denied_output(
    call: &FunctionToolCall,
    reason: &str,
) -> crate::tools::ToolOutput {
    crate::tools::ToolOutput {
        param: async_openai::types::responses::FunctionCallOutputItemParam {
            call_id: call.call_id.clone(),
            output: FunctionCallOutput::Text(format!(
                "error: tool call blocked by classifier — {reason}"
            )),
            id: None,
            status: None,
        },
        failed: true,
        approval_label: Some(format!("\u{274c} denied by classifier — {reason}")),
    }
}

/// Classify `calls` with a synchronous rule `classifier`, partitioning them into
/// allow / deny / ask. Returns `None` if cancelled partway.
pub(crate) fn classify_sync(
    classifier: &dyn Classifier,
    calls: &[FunctionToolCall],
    cancel: &CancellationToken,
) -> Option<ClassificationOutcome> {
    if cancel.is_cancelled() {
        return None;
    }
    let mut outcome = ClassificationOutcome::default();
    for call in calls {
        if cancel.is_cancelled() {
            return None;
        }
        match classifier.classify(&call.name, &call.arguments) {
            Verdict::Allow => outcome.allowed.push(call.clone()),
            Verdict::Deny { reason } => {
                outcome.denied.push(classifier_denied_output(call, &reason))
            }
            Verdict::Ask { reason } => outcome.ask.push((call.clone(), reason)),
        }
    }
    Some(outcome)
}

/// Classify `calls` with the Auto-mode LLM classifier, partitioning them into
/// allow / deny (Auto never asks — non-allow verdicts become denials). The
/// caller has already run the provider front gate, so every call here genuinely
/// needs review; each is sent straight to the LLM. Returns `None` if cancelled.
pub(crate) async fn classify_llm(
    client: &Client<OpenAIConfig>,
    model_name: &str,
    no_logprobs: &Arc<Mutex<HashSet<String>>>,
    light_context: &str,
    full_context: &str,
    calls: Vec<FunctionToolCall>,
    cancel: &CancellationToken,
) -> Option<ClassificationOutcome> {
    enum Decision {
        Allow(FunctionToolCall),
        Deny(crate::tools::ToolOutput),
    }

    let decisions: Vec<Option<Decision>> = futures::stream::iter(calls.into_iter().map(|call| {
        async move {
            if cancel.is_cancelled() {
                return None;
            }
            let try_logprobs = !no_logprobs.lock().unwrap().contains(model_name);
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
                no_logprobs.lock().unwrap().insert(model_name.to_string());
            }

            Some(match outcome.verdict {
                Verdict::Allow => Decision::Allow(call),
                Verdict::Deny { reason } | Verdict::Ask { reason } => {
                    Decision::Deny(classifier_denied_output(&call, &reason))
                }
            })
        }
    }))
    .buffered(MAX_CONCURRENT_CLASSIFICATIONS)
    .collect()
    .await;

    if cancel.is_cancelled() {
        return None;
    }

    let mut outcome = ClassificationOutcome::default();
    for decision in decisions.into_iter().flatten() {
        match decision {
            Decision::Allow(call) => outcome.allowed.push(call),
            Decision::Deny(output) => outcome.denied.push(output),
        }
    }
    Some(outcome)
}

// ---------------------------------------------------------------------------
// Classifier context builder
// ---------------------------------------------------------------------------

/// Truncate to at most `max` characters (on a char boundary), appending an
/// ellipsis when clipped. Keeps classifier context compact.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
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

/// Build light and full classifier context strings from the conversation items.
///
/// `light_context` carries the last few user+assistant exchanges so the fast
/// yes/no path has enough to understand short replies like "好的".
///
/// `full_context` carries the **complete** conversation transcript (every user
/// message, every assistant message, every tool call with args, every tool
/// output), skipping only reasoning blocks and internal errors.  Large tool
/// outputs are truncated to keep the context manageable.
pub(crate) fn build_classifier_context(items: &[&MessageItem]) -> (String, String) {
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
                    if let Some(text) = crate::app::helpers::extract_input_text(input) {
                        recent.push(format!(
                            "[User]\n{}",
                            truncate_chars(text.trim(), CLASSIFIER_LIGHT_MSG_CHARS)
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
                            truncate_chars(text.trim(), CLASSIFIER_LIGHT_MSG_CHARS)
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

    for it in items {
        match it {
            MessageItem::Input(input) => {
                if let Some(text) = crate::app::helpers::extract_input_text(input) {
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
                    truncate_chars(&call.arguments, CLASSIFIER_CALL_ARGS_CHARS)
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
                            truncate_chars(t.trim(), CLASSIFIER_ASK_OUTPUT_CHARS)
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
            MessageItem::Compacted { summary } => {
                full_ctx.push(format!("\n[Conversation before this point was compacted; its summary]\n{summary}"));
            }
        }
    }

    (light.join("\n\n"), full_ctx.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_openai::types::responses::{
        InputContent, InputMessage, InputRole, MessageItem as ApiMessageItem, OutputStatus,
    };

    fn call(name: &str, args: &str) -> FunctionToolCall {
        FunctionToolCall {
            arguments: args.into(),
            call_id: format!("call_{name}"),
            namespace: None,
            name: name.into(),
            id: None,
            status: None,
        }
    }

    /// Returns each verdict kind based on the tool name, so the partitioning
    /// itself can be exercised independently of any work mode's rules.
    struct MockClassifier;
    impl Classifier for MockClassifier {
        fn classify(&self, name: &str, _args: &str) -> Verdict {
            match name {
                "allow_me" => Verdict::Allow,
                "deny_me" => Verdict::Deny { reason: "nope".into() },
                _ => Verdict::Ask { reason: "confirm?".into() },
            }
        }
    }

    #[test]
    fn classify_sync_partitions_by_verdict() {
        // Each verdict lands in its own bucket, preserving the call.
        let calls = vec![
            call("allow_me", "{}"),
            call("deny_me", "{}"),
            call("ask_me", "{}"),
        ];
        let cancel = CancellationToken::new();
        let outcome = classify_sync(&MockClassifier, &calls, &cancel).expect("not cancelled");
        assert_eq!(outcome.allowed.len(), 1);
        assert_eq!(outcome.allowed[0].name, "allow_me");
        assert_eq!(outcome.denied.len(), 1, "deny_me denied");
        assert_eq!(outcome.ask.len(), 1);
        assert_eq!(outcome.ask[0].0.name, "ask_me");
    }

    #[test]
    fn classify_sync_returns_none_when_cancelled() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(classify_sync(&MockClassifier, &[call("ask_me", "{}")], &cancel).is_none());
    }

    #[test]
    fn build_classifier_context_transcribes_the_conversation() {
        let user = MessageItem::Input(async_openai::types::responses::InputItem::Item(
            async_openai::types::responses::Item::Message(ApiMessageItem::Input(InputMessage {
                content: vec![InputContent::InputText("please build it".into())],
                role: InputRole::User,
                status: Some(OutputStatus::Completed),
            })),
        ));
        let tool_call = MessageItem::Output(OutputItem::FunctionCall(call("command", "{\"cmd\":\"make\"}")));
        let items: Vec<&MessageItem> = vec![&user, &tool_call];
        let (light, full) = build_classifier_context(&items);
        assert!(light.contains("please build it"), "light: {light}");
        assert!(full.contains("[User]"), "full has user: {full}");
        assert!(full.contains("[Tool call: command]"), "full has call: {full}");
        assert!(full.contains("make"), "full has args: {full}");
    }
}
