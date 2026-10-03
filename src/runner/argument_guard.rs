//! Bound tool argument generation before it reaches rendering or execution.
use async_openai::types::responses::{FunctionToolCall, OutputItem, ResponseStreamEvent};
use std::collections::BTreeMap;

// A large file edit is legitimate; 8 MiB per response is an emergency memory
// ceiling, not a normal payload budget. Exact repetition needs 16 KiB and at
// least 16 copies, so ordinary repeated code/indentation does not trip it.
const MAX_ARGUMENT_BYTES: usize = 8 * 1024 * 1024;
const REPEATED_TAIL_BYTES: usize = 16 * 1024;
const MAX_PERIOD_BYTES: usize = 1024;
const CHECK_INTERVAL_BYTES: usize = 1024;
pub(super) const MAX_CONSECUTIVE_FAILURES: usize = 3;
pub(super) const FAILURE: &str = "Tool arguments exceeded generation safety limits. No tools from this response ran. Retry with concise arguments.";

#[derive(Default)]
pub(super) struct ArgumentGuard {
    arguments: BTreeMap<u32, String>,
    pub calls: BTreeMap<u32, FunctionToolCall>,
    checked: BTreeMap<u32, usize>,
    total: usize,
    identity_conflict: bool,
}

impl ArgumentGuard {
    /// Returns false before forwarding the offending event to any consumer.
    pub fn accept(&mut self, event: &ResponseStreamEvent) -> bool {
        use ResponseStreamEvent::*;
        match event {
            ResponseOutputItemAdded(event) => self.item(event.output_index, &event.item),
            ResponseOutputItemDone(event) => self.item(event.output_index, &event.item),
            ResponseFunctionCallArgumentsDelta(event) => {
                self.arguments(event.output_index, &event.delta, false)
            }
            ResponseFunctionCallArgumentsDone(event) => {
                self.arguments(event.output_index, &event.arguments, true)
            }
            ResponseCompleted(event) => self.output(&event.response.output),
            ResponseFailed(event) => self.output(&event.response.output),
            ResponseIncomplete(event) => self.output(&event.response.output),
            _ => true,
        }
    }

    pub fn identities_complete(&self) -> bool {
        !self.identity_conflict
            && self
                .arguments
                .keys()
                .all(|index| self.calls.contains_key(index))
    }

    fn output(&mut self, output: &[OutputItem]) -> bool {
        let mut accepted = true;
        // Inspect every identity even after a size violation: recovery must not
        // choose a representative from an ambiguous terminal response.
        for (index, item) in output.iter().enumerate() {
            let Ok(index) = u32::try_from(index) else {
                self.identity_conflict = true;
                return false;
            };
            accepted &= self.item(index, item);
        }
        accepted
    }

    fn item(&mut self, index: u32, item: &OutputItem) -> bool {
        let OutputItem::FunctionCall(call) = item else {
            if self.calls.contains_key(&index) || self.arguments.contains_key(&index) {
                self.identity_conflict = true;
                return false;
            }
            return true;
        };
        if let Some(previous) = self.calls.get(&index)
            && (previous.call_id != call.call_id
                || previous.name != call.name
                || previous.namespace != call.namespace
                || matches!((&previous.id, &call.id), (Some(left), Some(right)) if left != right))
        {
            self.identity_conflict = true;
            return false;
        }
        let sanitized = FunctionToolCall {
            arguments: "{}".into(),
            call_id: call.call_id.clone(),
            name: call.name.clone(),
            namespace: call.namespace.clone(),
            id: call.id.clone(),
            status: call.status,
        };
        self.calls.insert(index, sanitized);
        self.arguments(index, &call.arguments, true)
    }

    fn arguments(&mut self, index: u32, text: &str, replace: bool) -> bool {
        let arguments = self.arguments.entry(index).or_default();
        let previous = arguments.len();
        let length = if replace {
            text.len()
        } else {
            previous.saturating_add(text.len())
        };
        let total = self.total.saturating_sub(previous).saturating_add(length);
        if total > MAX_ARGUMENT_BYTES {
            return false;
        }
        self.total = total;
        if replace {
            arguments.clear();
        }
        arguments.push_str(text);
        let checked = self.checked.entry(index).or_default();
        if !replace && length < checked.saturating_add(CHECK_INTERVAL_BYTES) {
            return true;
        }
        *checked = length;
        !repeated_tail(arguments.as_bytes())
    }
}

fn repeated_tail(arguments: &[u8]) -> bool {
    if arguments.len() < REPEATED_TAIL_BYTES {
        return false;
    }
    let tail = &arguments[arguments.len() - REPEATED_TAIL_BYTES..];
    // Byte comparison is intentional: UTF-8 chunk boundaries never require
    // slicing a str, and an exact multibyte cycle is detected just like ASCII.
    (1..=MAX_PERIOD_BYTES).any(|period| tail[period..] == tail[..tail.len() - period])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_events_allow_normal_arguments_and_enforce_aggregate_budget() {
        for status in ["completed", "failed", "incomplete"] {
            let event: ResponseStreamEvent = serde_json::from_value(serde_json::json!({
                "type": format!("response.{status}"), "sequence_number": 1,
                "response": {
                    "created_at": 0, "id": "response", "model": "mock",
                    "object": "response", "status": status,
                    "output": [{
                        "type": "function_call", "name": "read_file",
                        "call_id": "call", "arguments": "{}"
                    }]
                }
            }))
            .unwrap();
            let mut guard = ArgumentGuard::default();
            assert!(guard.accept(&event));
            assert!(guard.identities_complete());
            assert_eq!(guard.calls.len(), 1);
            assert_eq!(guard.total, 2);
            // Terminal snapshots replace, rather than double-count, deltas.
            assert!(guard.accept(&event));
            assert_eq!(guard.total, 2);

            let mut guard = ArgumentGuard {
                total: MAX_ARGUMENT_BYTES,
                ..Default::default()
            };
            assert!(!guard.accept(&event));
            assert!(guard.identities_complete());
        }
    }

    #[test]
    fn repetition_crosses_chunks_and_utf8_boundaries() {
        let mut guard = ArgumentGuard::default();
        let mut accepted = true;
        for _ in 0..10000 {
            accepted = guard.arguments(0, "维拉abc", false);
            if !accepted {
                break;
            }
        }
        assert!(!accepted);
        assert!(guard.arguments(1, "{}", false));
    }

    #[test]
    fn long_normal_code_and_short_repetition_are_allowed() {
        let code: String = (0..20000)
            .map(|index| format!("let value_{index} = {index};\n"))
            .collect();
        assert!(ArgumentGuard::default().arguments(0, &code, true));
        assert!(ArgumentGuard::default().arguments(0, &"a".repeat(16000), true));
    }

    #[test]
    fn orphan_arguments_cannot_be_recovered_as_another_call() {
        let mut guard = ArgumentGuard::default();
        assert!(!guard.arguments(9, &"x".repeat(REPEATED_TAIL_BYTES), false));
        assert!(!guard.identities_complete());
    }

    #[test]
    fn full_values_and_aggregate_size_are_checked() {
        assert!(!ArgumentGuard::default().arguments(0, &"ab".repeat(10000), true));
        // Bypass the repetition heuristic to isolate the aggregate hard cap.
        let mut guard = ArgumentGuard {
            total: MAX_ARGUMENT_BYTES,
            ..Default::default()
        };
        assert!(!guard.arguments(1, "x", false));
        assert!(guard.arguments[&1].is_empty());
    }
}
