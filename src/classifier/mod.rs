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

//! Tool-call classification via per-mode classifier traits.
//!
//! Each [`WorkMode`] owns a [`Classifier`] implementation that decides
//! whether a tool call is allowed, denied, or requires user approval.
//! Adding a new mode (e.g. "Paranoid" with parameter-level rules) only
//! requires implementing the trait and wiring up a new variant.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::mcp::types::McpPolicy;

// ---------------------------------------------------------------------------
// Verdict
// ---------------------------------------------------------------------------

/// The result of classifying a single tool call.
#[derive(Debug, Clone)]
pub enum Verdict {
    /// Run the tool without asking.
    Allow,
    /// Block the tool and return an error message to the model.
    Deny { reason: String },
    /// Pause and ask the user via the approval UI.
    Ask { reason: String },
}

// ---------------------------------------------------------------------------
// Classifier trait
// ---------------------------------------------------------------------------

/// Implemented by each work mode to classify tool calls.
///
/// Receives the tool name and its raw JSON arguments so future
/// implementations can inspect the payload (e.g. forbid `rm -rf`).
pub trait Classifier: Send + Sync {
    fn classify(&self, tool_name: &str, arguments: &str) -> Verdict;
}

// ---------------------------------------------------------------------------
// Standard classifiers
// ---------------------------------------------------------------------------

/// Tool names considered "dangerous" — they mutate state or run commands.
/// `configure_diagnostics` writes a profile and test-runs its checker commands,
/// so it is gated like `command`.
///
/// MCP tools (names starting with `mcp__`) are not listed here because we can't
/// know their semantics at compile time. Instead they are routed through
/// [`classify_mcp_policy`] first, which resolves per-server policies.
/// A [`McpPolicy::Trusted`] tool is allowed immediately; a [`McpPolicy::Review`]
/// tool falls through to the normal classifier.
const DANGEROUS_TOOLS: &[&str] =
    &["command", "write_file", "edit_file", "configure_diagnostics"];

/// Tool names that are read-only — always safe, even in Plan/Planning phase.
const READ_ONLY_TOOLS: &[&str] = &[
    "read_file",
    "grep",
    "blob",
    // `fetch` refuses private/internal addresses itself, so it is safe to
    // classify as read-only despite touching the network.
    "fetch",
    "ask_user",
    "diagnostics",
    "todo",
];

/// The `task` tool is action-dependent: `create` runs an arbitrary command and
/// `kill` terminates a process (gated like `command`), while `list`/`output`/
/// `wait` only observe existing tasks and are safe everywhere.
fn is_mutating(tool_name: &str, arguments: &str) -> bool {
    if tool_name == crate::tools::task::NAME {
        return crate::tools::task::action_is_mutating(arguments);
    }
    DANGEROUS_TOOLS.contains(&tool_name)
}

/// Extract the MCP server name from a fully-qualified tool name like
/// `mcp__codegraph__search` → `"codegraph"`. Returns `None` for built-in tools.
pub fn mcp_server_name(tool_name: &str) -> Option<&str> {
    tool_name
        .strip_prefix("mcp__")
        .and_then(|rest| rest.split_once("__"))
        .map(|(server, _tool)| server)
}

/// Resolve the [`Verdict`] for an MCP tool based on its server's configured
/// [`McpPolicy`]. Returns `Some(Allow)` for [`McpPolicy::Trusted`]; returns
/// `None` for [`McpPolicy::Review`], meaning the caller should fall through
/// to the normal classifier (sync or LLM, depending on the current work mode).
pub(crate) fn classify_mcp_policy(
    tool_name: &str,
    policies: &HashMap<String, McpPolicy>,
) -> Option<Verdict> {
    let server = mcp_server_name(tool_name)?;
    match policies.get(server).unwrap_or(&McpPolicy::Review) {
        McpPolicy::Trusted => Some(Verdict::Allow),
        McpPolicy::Review => None,
    }
}

/// Manual mode: every dangerous tool call must be approved.
/// MCP tools are classified according to their server's [`McpPolicy`].
pub struct ManualClassifier {
    mcp_policies: HashMap<String, McpPolicy>,
}

impl ManualClassifier {
    pub(crate) fn new(mcp_policies: HashMap<String, McpPolicy>) -> Self {
        ManualClassifier { mcp_policies }
    }
}

impl Classifier for ManualClassifier {
    fn classify(&self, tool_name: &str, arguments: &str) -> Verdict {
        // MCP tools: consult per-server policy first.
        if let Some(verdict) = classify_mcp_policy(tool_name, &self.mcp_policies) {
            return verdict;
        }
        // Built-in tools: ask for dangerous, allow for safe.
        // Unknown MCP tools (not in any server's policy map) are treated as
        // dangerous — we can't know their semantics.
        if is_mutating(tool_name, arguments) || tool_name.starts_with("mcp__") {
            Verdict::Ask {
                reason: format!("{tool_name} requires approval in Manual mode"),
            }
        } else {
            Verdict::Allow
        }
    }
}

/// Plan Planning phase: all read-only tools are allowed; anything that
/// mutates state (write_file, edit_file, command, configure_diagnostics)
/// is denied with a clear message to output a plan instead.
/// MCP tools are classified according to their server's [`McpPolicy`].
pub struct PlanPlanningClassifier {
    mcp_policies: HashMap<String, McpPolicy>,
}

impl PlanPlanningClassifier {
    pub(crate) fn new(mcp_policies: HashMap<String, McpPolicy>) -> Self {
        PlanPlanningClassifier { mcp_policies }
    }
}

impl Classifier for PlanPlanningClassifier {
    fn classify(&self, tool_name: &str, arguments: &str) -> Verdict {
        // MCP tools: consult per-server policy first.
        if let Some(verdict) = classify_mcp_policy(tool_name, &self.mcp_policies) {
            return verdict;
        }
        // Read-only tools always allowed, including the task tool's
        // observing actions (list/output/wait).
        if READ_ONLY_TOOLS.contains(&tool_name)
            || (tool_name == crate::tools::task::NAME
                && !is_mutating(tool_name, arguments))
        {
            return Verdict::Allow;
        }
        // Everything else: denied. Tell the model to output a plan.
        Verdict::Deny {
            reason: format!(
                "You are in Plan mode (Planning phase). Use read-only tools to \
                 explore and output a step-by-step plan. Do NOT call {tool_name}. \
                 Stop after presenting the plan — the user will choose how to \
                 execute it."
            ),
        }
    }
}

/// YOLO mode: everything is allowed, no questions asked.
pub struct YoloClassifier;

impl Classifier for YoloClassifier {
    fn classify(&self, _tool_name: &str, _arguments: &str) -> Verdict {
        Verdict::Allow
    }
}

// ---------------------------------------------------------------------------
// Work mode
// ---------------------------------------------------------------------------

/// The current safety/work mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WorkMode {
    /// Every write/edit/command tool call requires user approval.
    Manual,
    /// An LLM classifier decides per tool call whether to auto-approve.
    #[default]
    Auto,
    /// All tool calls execute without any interception. Gated behind
    /// `allow_yolo` in the config.
    Yolo,
    /// Plan-first mode: agent explores and outputs a plan before executing.
    /// In Planning phase only read-only tools are allowed; after user
    /// approval, execution uses the selected execution mode.
    Plan,
}

/// Sub-phase of Plan mode. Approving a plan exits Plan mode entirely (the
/// work mode switches to the chosen execution mode), so there is no
/// "executing" phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlanPhase {
    /// Agent explores with read-only tools and outputs a plan.
    #[default]
    Planning,
    /// Plan is complete, waiting for user to choose execution mode.
    Reviewing,
}

impl WorkMode {
    /// Human-readable label shown in the footer.
    pub fn label(&self) -> &str {
        match self {
            WorkMode::Manual => "Manual",
            WorkMode::Auto => "Auto",
            WorkMode::Yolo => "YOLO",
            WorkMode::Plan => "Plan",
        }
    }

    /// Emoji icon for the footer.
    pub fn icon(&self) -> &str {
        match self {
            WorkMode::Manual => "\u{1f6e1}",
            WorkMode::Auto => "\u{1f916}",
            WorkMode::Yolo => "\u{26a1}",
            WorkMode::Plan => "\u{1f4cb}",
        }
    }

    /// Cycle to the next mode. Plan is always in the cycle; YOLO is included
    /// when `allow_yolo` is true.
    pub fn next(self, allow_yolo: bool) -> WorkMode {
        match self {
            WorkMode::Manual => WorkMode::Auto,
            WorkMode::Auto => WorkMode::Plan,
            WorkMode::Plan => {
                if allow_yolo {
                    WorkMode::Yolo
                } else {
                    WorkMode::Manual
                }
            }
            WorkMode::Yolo => WorkMode::Manual,
        }
    }

    /// Whether this mode classifies tool calls with an async LLM call rather
    /// than the synchronous rule-based [`Classifier`].
    pub fn uses_llm_classifier(&self) -> bool {
        matches!(self, WorkMode::Auto)
    }

    /// Return the synchronous classifier for this mode. Auto has no sync
    /// classifier (it uses [`classify_tool_call`]); it falls back to asking so
    /// callers that ignore [`uses_llm_classifier`] stay safe.
    pub(crate) fn classifier(&self, mcp_policies: HashMap<String, McpPolicy>) -> Box<dyn Classifier> {
        match self {
            WorkMode::Manual | WorkMode::Auto => {
                Box::new(ManualClassifier::new(mcp_policies))
            }
            WorkMode::Yolo => Box::new(YoloClassifier),
            WorkMode::Plan => Box::new(PlanPlanningClassifier::new(mcp_policies)),
        }
    }
}

/// Whether a tool call needs LLM review in Auto mode. Read-only tools are
/// always safe; only state-mutating tools are sent to the classifier.
/// MCP tools are always treated as potentially dangerous (we can't know their
/// semantics at compile time), but their server's [`McpPolicy`] is checked
/// first in [`spawn_auto_classification`] — a [`McpPolicy::Trusted`] server
/// bypasses the LLM entirely.
pub fn needs_review(tool_name: &str, arguments: &str) -> bool {
    is_mutating(tool_name, arguments) || tool_name.starts_with("mcp__")
}

// ---------------------------------------------------------------------------
// LLM classifier (Auto mode)
// ---------------------------------------------------------------------------

mod llm;
pub use llm::classify_tool_call;

#[cfg(test)]
mod tests {
    use super::*;

    fn classify(mode: WorkMode, name: &str) -> Verdict {
        mode.classifier(HashMap::new()).classify(name, r#"{}"#)
    }

    #[test]
    fn yolo_allows_everything() {
        assert!(matches!(classify(WorkMode::Yolo, "command"), Verdict::Allow));
        assert!(matches!(classify(WorkMode::Yolo, "write_file"), Verdict::Allow));
        assert!(matches!(classify(WorkMode::Yolo, "read_file"), Verdict::Allow));
    }

    #[test]
    fn manual_asks_for_dangerous() {
        assert!(matches!(classify(WorkMode::Manual, "command"), Verdict::Ask { .. }));
        assert!(matches!(classify(WorkMode::Manual, "write_file"), Verdict::Ask { .. }));
        assert!(matches!(classify(WorkMode::Manual, "read_file"), Verdict::Allow));
        assert!(matches!(classify(WorkMode::Manual, "grep"), Verdict::Allow));
    }

    #[test]
    fn plan_planning_allows_read_only_denies_mutating() {
        // Read-only: Allow in Plan Planning.
        assert!(matches!(classify(WorkMode::Plan, "read_file"), Verdict::Allow));
        assert!(matches!(classify(WorkMode::Plan, "grep"), Verdict::Allow));
        assert!(matches!(classify(WorkMode::Plan, "blob"), Verdict::Allow));
        assert!(matches!(classify(WorkMode::Plan, "ask_user"), Verdict::Allow));
        assert!(matches!(classify(WorkMode::Plan, "todo"), Verdict::Allow));
        // Mutating: Deny.
        assert!(matches!(classify(WorkMode::Plan, "command"), Verdict::Deny { .. }));
        assert!(matches!(classify(WorkMode::Plan, "write_file"), Verdict::Deny { .. }));
        assert!(matches!(classify(WorkMode::Plan, "edit_file"), Verdict::Deny { .. }));
    }

    #[test]
    fn plan_deny_message_mentions_plan() {
        if let Verdict::Deny { reason } = classify(WorkMode::Plan, "command") {
            assert!(reason.contains("plan"), "reason: {reason}");
        } else {
            panic!("expected Deny");
        }
    }

    #[test]
    fn mode_cycle_includes_plan() {
        assert_eq!(WorkMode::Manual.next(false), WorkMode::Auto);
        assert_eq!(WorkMode::Auto.next(false), WorkMode::Plan);
        assert_eq!(WorkMode::Plan.next(false), WorkMode::Manual);
        assert_ne!(WorkMode::Plan.next(false), WorkMode::Yolo);
    }

    #[test]
    fn mode_cycle_includes_yolo_when_allowed() {
        assert_eq!(WorkMode::Manual.next(true), WorkMode::Auto);
        assert_eq!(WorkMode::Auto.next(true), WorkMode::Plan);
        assert_eq!(WorkMode::Plan.next(true), WorkMode::Yolo);
        assert_eq!(WorkMode::Yolo.next(true), WorkMode::Manual);
    }

    #[test]
    fn auto_uses_llm_classifier() {
        assert!(WorkMode::Auto.uses_llm_classifier());
        assert!(!WorkMode::Manual.uses_llm_classifier());
        assert!(!WorkMode::Plan.uses_llm_classifier());
    }

    #[test]
    fn needs_review_only_mutating() {
        assert!(needs_review("command", "{}"));
        assert!(needs_review("write_file", "{}"));
        assert!(!needs_review("read_file", "{}"));
        assert!(!needs_review("grep", "{}"));
        assert!(!needs_review("todo", "{}"));
    }

    #[test]
    fn task_tool_is_classified_by_action() {
        let create = r#"{"action":"create","command":"cargo watch"}"#;
        let kill = r#"{"action":"kill","id":1}"#;
        let list = r#"{"action":"list"}"#;
        let output = r#"{"action":"output","id":1}"#;
        let wait = r#"{"action":"wait","id":1}"#;

        // Manual: mutating actions ask, observing actions pass.
        let manual = WorkMode::Manual.classifier(HashMap::new());
        assert!(matches!(manual.classify("task", create), Verdict::Ask { .. }));
        assert!(matches!(manual.classify("task", kill), Verdict::Ask { .. }));
        assert!(matches!(manual.classify("task", list), Verdict::Allow));
        assert!(matches!(manual.classify("task", output), Verdict::Allow));
        assert!(matches!(manual.classify("task", wait), Verdict::Allow));

        // Plan Planning: mutating actions denied, observing actions pass.
        let plan = WorkMode::Plan.classifier(HashMap::new());
        assert!(matches!(plan.classify("task", create), Verdict::Deny { .. }));
        assert!(matches!(plan.classify("task", kill), Verdict::Deny { .. }));
        assert!(matches!(plan.classify("task", list), Verdict::Allow));
        assert!(matches!(plan.classify("task", output), Verdict::Allow));

        // Auto's pre-filter mirrors the same split.
        assert!(needs_review("task", create));
        assert!(needs_review("task", kill));
        assert!(!needs_review("task", list));
        assert!(!needs_review("task", output));
        assert!(!needs_review("task", wait));
    }

    #[test]
    fn work_mode_labels() {
        assert_eq!(WorkMode::Manual.label(), "Manual");
        assert_eq!(WorkMode::Auto.label(), "Auto");
        assert_eq!(WorkMode::Yolo.label(), "YOLO");
        assert_eq!(WorkMode::Plan.label(), "Plan");
    }

    #[test]
    fn mcp_policy_trusted_returns_allow() {
        let mut policies = HashMap::new();
        policies.insert("codegraph".to_string(), McpPolicy::Trusted);
        assert!(matches!(
            classify_mcp_policy("mcp__codegraph__search", &policies),
            Some(Verdict::Allow)
        ));
    }

    #[test]
    fn mcp_policy_review_returns_none() {
        let mut policies = HashMap::new();
        policies.insert("codegraph".to_string(), McpPolicy::Review);
        assert!(classify_mcp_policy("mcp__codegraph__search", &policies).is_none());
    }

    #[test]
    fn mcp_server_name_parsing() {
        assert_eq!(mcp_server_name("mcp__codegraph__search"), Some("codegraph"));
        assert_eq!(mcp_server_name("command"), None);
        assert_eq!(mcp_server_name("mcp__"), None);
    }

    #[test]
    fn builtin_tool_not_affected_by_mcp_policy() {
        let mut policies = HashMap::new();
        policies.insert("command".to_string(), McpPolicy::Trusted);
        // "command" is a built-in tool, not an MCP tool — policy doesn't apply.
        let v = classify_mcp_policy("command", &policies);
        assert!(v.is_none());
    }

    #[test]
    fn manual_mode_trusted_mcp_allowed() {
        let mut policies = HashMap::new();
        policies.insert("fs".to_string(), McpPolicy::Trusted);
        let c = ManualClassifier::new(policies);
        // MCP tool with Trusted policy: auto-approved even in Manual mode.
        assert!(matches!(c.classify("mcp__fs__write", r#"{}"#), Verdict::Allow));
    }

    #[test]
    fn manual_mode_review_mcp_asks() {
        let mut policies = HashMap::new();
        policies.insert("db".to_string(), McpPolicy::Review);
        let c = ManualClassifier::new(policies);
        // MCP tool with Review policy: Ask in Manual mode.
        assert!(matches!(
            c.classify("mcp__db__query", r#"{}"#),
            Verdict::Ask { .. }
        ));
        // Built-in dangerous: still Ask.
        assert!(matches!(c.classify("command", r#"{}"#), Verdict::Ask { .. }));
    }

    #[test]
    fn plan_planning_trusted_mcp_allowed() {
        let mut policies = HashMap::new();
        policies.insert("fs".to_string(), McpPolicy::Trusted);
        let c = PlanPlanningClassifier::new(policies);
        // MCP tool with Trusted policy: auto-approved even in Plan Planning.
        assert!(matches!(c.classify("mcp__fs__read", r#"{}"#), Verdict::Allow));
        assert!(matches!(c.classify("mcp__fs__write", r#"{}"#), Verdict::Allow));
    }

    #[test]
    fn plan_planning_review_mcp_denied() {
        let mut policies = HashMap::new();
        policies.insert("db".to_string(), McpPolicy::Review);
        let c = PlanPlanningClassifier::new(policies);
        // MCP tool with Review policy: denied in Plan Planning (not read-only).
        assert!(matches!(
            c.classify("mcp__db__query", r#"{}"#),
            Verdict::Deny { .. }
        ));
    }
}
