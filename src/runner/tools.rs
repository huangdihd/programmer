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

//! Running a batch of approved tool calls, free of any `App`. Consecutive
//! read-only calls run concurrently; writes and other side-effecting tools run
//! one at a time. Output order always matches call order, so a read never
//! observes a half-applied write. The TUI wraps this in a spawned task that
//! reports the outputs via `ToolCallsCompleted`; the headless runner awaits it
//! inline.

use crate::cancel::CancellationToken;
use crate::tools::provider::{ToolCtx, ToolRegistry};
use crate::ui::event::Event;
use async_openai::types::responses::FunctionToolCall;
use futures::StreamExt;
use std::sync::Arc;

/// Execute one tool call through the registry and stamp the approval label
/// (unless the classifier already set one). Takes everything by value so each
/// future is self-contained and can be driven concurrently in a `buffered`
/// stream; the captured handles (`sender`, `registry`) are cheap `Arc`-backed
/// clones.
async fn run_labeled_call(
    call: FunctionToolCall,
    sender: tokio::sync::mpsc::UnboundedSender<Event>,
    registry: Arc<ToolRegistry>,
    label: String,
) -> crate::tools::ToolOutput {
    let result = registry.call(&call, &ToolCtx { sender: &sender }).await;
    let mut out = crate::tools::make_tool_output(&call.call_id, result);
    if out.approval_label.is_none() {
        out.approval_label = Some(label);
    }
    out
}

/// Run `allowed` tool calls, prepending the already-decided `denied` outputs.
///
/// Consecutive read-only calls run concurrently (bounded by
/// [`crate::consts::MAX_CONCURRENT_READ_TOOLS`]); writes and other side-effecting
/// tools run one at a time. Output order always matches call order, and the
/// ordering between a write and the reads around it is preserved, so a read
/// never observes a half-applied write. Stops early if `cancel` fires, returning
/// whatever ran so far (with the denials).
pub(crate) async fn run_tool_batch(
    allowed: Vec<FunctionToolCall>,
    denied: Vec<crate::tools::ToolOutput>,
    cancel: CancellationToken,
    approval_label: String,
    sender: tokio::sync::mpsc::UnboundedSender<Event>,
    registry: Arc<ToolRegistry>,
) -> Vec<crate::tools::ToolOutput> {
    let mut outputs = denied;
    let mut i = 0;
    while i < allowed.len() {
        if cancel.is_cancelled() {
            break;
        }
        if registry.is_read_only(&allowed[i].name) {
            // Take the maximal run of consecutive read-only calls and run
            // them concurrently, preserving order.
            let start = i;
            while i < allowed.len() && registry.is_read_only(&allowed[i].name) {
                i += 1;
            }
            let futs: Vec<_> = allowed[start..i]
                .iter()
                .map(|call| {
                    run_labeled_call(
                        call.clone(),
                        sender.clone(),
                        registry.clone(),
                        approval_label.clone(),
                    )
                })
                .collect();
            let mut batch: Vec<crate::tools::ToolOutput> = futures::stream::iter(futs)
                .buffered(crate::consts::MAX_CONCURRENT_READ_TOOLS)
                .collect()
                .await;
            outputs.append(&mut batch);
        } else {
            let out = run_labeled_call(
                allowed[i].clone(),
                sender.clone(),
                registry.clone(),
                approval_label.clone(),
            )
            .await;
            outputs.push(out);
            i += 1;
        }
    }
    outputs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, args: &str) -> FunctionToolCall {
        FunctionToolCall {
            arguments: args.into(),
            call_id: format!("call_{name}_{args}"),
            namespace: None,
            name: name.into(),
            id: None,
            status: None,
        }
    }

    fn text_of(out: &crate::tools::ToolOutput) -> String {
        match &out.param.output {
            async_openai::types::responses::FunctionCallOutput::Text(t) => t.clone(),
            _ => String::new(),
        }
    }

    /// A registry with just the local built-ins — enough for the batch tests.
    fn local_registry() -> Arc<ToolRegistry> {
        Arc::new(ToolRegistry::new(vec![Arc::new(
            crate::tools::provider::LocalToolProvider,
        )]))
    }

    #[tokio::test]
    async fn denied_come_first_and_output_order_matches_call_order() {
        // A read, a write, a read — mixing the concurrent and serial paths.
        let tmp = std::env::temp_dir().join(format!("engine_batch_{}", std::process::id()));
        // JSON-encode the path so Windows backslashes survive as valid JSON.
        let path = serde_json::to_string(&tmp.to_string_lossy()).unwrap();
        let allowed = vec![
            call("read_file", &format!("{{\"path\":{path}}}")),
            call("write_file", &format!("{{\"path\":{path},\"content\":\"hi\"}}")),
            call("read_file", &format!("{{\"path\":{path}}}")),
        ];
        let denied = vec![crate::runner::classify::classifier_denied_output(
            &call("command", "{}"),
            "blocked for the test",
        )];
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let outputs = run_tool_batch(
            allowed,
            denied,
            CancellationToken::new(),
            "test-label".to_string(),
            tx,
            local_registry(),
        )
        .await;

        // Denied output leads, then the three calls in call order.
        assert_eq!(outputs.len(), 4);
        assert_eq!(outputs[0].param.call_id, "call_command_{}");
        assert!(outputs[0].failed, "denied output is failed");
        assert!(outputs[1].param.call_id.starts_with("call_read_file"));
        assert!(outputs[2].param.call_id.starts_with("call_write_file"));
        assert!(outputs[3].param.call_id.starts_with("call_read_file"));
        // The trailing read observes the write (order preserved).
        assert!(text_of(&outputs[3]).contains("hi"), "read after write: {}", text_of(&outputs[3]));

        let _ = std::fs::remove_file(&tmp);
    }

    #[tokio::test]
    async fn cancelled_batch_stops_early_but_keeps_denials() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let denied = vec![crate::runner::classify::classifier_denied_output(
            &call("command", "{}"),
            "blocked",
        )];
        let outputs = run_tool_batch(
            vec![call("read_file", "{\"path\":\"/nonexistent\"}")],
            denied,
            cancel,
            "test-label".to_string(),
            tx,
            local_registry(),
        )
        .await;
        // Cancelled before running anything allowed: only the denial remains.
        assert_eq!(outputs.len(), 1);
        assert!(outputs[0].failed);
    }
}
