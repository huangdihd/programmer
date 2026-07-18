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
use crate::response::message_item::MessageItem;
use crate::ui::components::conversation_panel::conversation_panel::ActivePhase;
use crate::ui::event::{AppEvent, Event};
use async_openai::types::responses::{FunctionToolCall, OutputItem};
use std::collections::HashMap;
use crate::cancel::CancellationToken;

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
        let Some(outcome) =
            crate::engine::classify::classify_sync(classifier.as_ref(), &calls, &cancel_token)
        else {
            return; // cancelled
        };
        let _ = sender.send(Event::App(AppEvent::ClassificationCompleted {
            allowed: outcome.allowed,
            denied: outcome.denied,
            ask_queue: outcome.ask,
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
        let Some(outcome) = crate::engine::classify::classify_llm(
            &client,
            &model_name,
            &no_lp,
            &mcp_policies,
            &light_context,
            &full_context,
            calls,
            &cancel_token,
        )
        .await
        else {
            return; // cancelled
        };
        let _ = sender.send(Event::App(AppEvent::ClassificationCompleted {
            allowed: outcome.allowed,
            denied: outcome.denied,
            ask_queue: outcome.ask,
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

/// Build light and full classifier context strings from the current
/// conversation. Thin wrapper over [`crate::engine::classify::build_classifier_context`].
pub(crate) fn build_classifier_context(app: &App<'_>) -> (String, String) {
    let items: Vec<&MessageItem> = app.conversation_panel.items().collect();
    crate::engine::classify::build_classifier_context(&items)
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
/// and report everything back via [`AppEvent::ToolCallsCompleted`]. The
/// batching/ordering logic lives in [`crate::engine::tools::run_tool_batch`].
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
        let outputs = crate::engine::tools::run_tool_batch(
            allowed,
            denied,
            cancel_token.clone(),
            label,
            sender.clone(),
            mcp,
        )
        .await;
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
