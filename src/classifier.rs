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

use serde::{Deserialize, Serialize};

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

/// Tool names considered "dangerous" — they mutate state.
const DANGEROUS_TOOLS: &[&str] = &["command", "write_file", "edit_file"];

/// Manual mode: every dangerous tool call must be approved.
pub struct ManualClassifier;

impl Classifier for ManualClassifier {
    fn classify(&self, tool_name: &str, _arguments: &str) -> Verdict {
        if DANGEROUS_TOOLS.contains(&tool_name) {
            Verdict::Ask {
                reason: format!("{tool_name} requires approval in Manual mode"),
            }
        } else {
            Verdict::Allow
        }
    }
}

/// Auto-allow edits: write/edit/command are silently approved.
pub struct AllowEditsClassifier;

impl Classifier for AllowEditsClassifier {
    fn classify(&self, _tool_name: &str, _arguments: &str) -> Verdict {
        Verdict::Allow
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
    /// Write/edit/command are auto-allowed.
    #[default]
    #[serde(alias = "edits")]
    AllowEdits,
    /// All tool calls execute without any interception.
    Yolo,
}

impl WorkMode {
    /// Human-readable label shown in the footer.
    pub fn label(&self) -> &str {
        match self {
            WorkMode::Manual => "Manual",
            WorkMode::AllowEdits => "Allow Edits",
            WorkMode::Yolo => "YOLO",
        }
    }

    /// Emoji icon for the footer.
    pub fn icon(&self) -> &str {
        match self {
            WorkMode::Manual => "🛡",
            WorkMode::AllowEdits => "✏️",
            WorkMode::Yolo => "⚡",
        }
    }

    /// Cycle to the next mode.
    pub fn next(self) -> WorkMode {
        match self {
            WorkMode::Manual => WorkMode::AllowEdits,
            WorkMode::AllowEdits => WorkMode::Yolo,
            WorkMode::Yolo => WorkMode::Manual,
        }
    }

    /// Return the classifier for this mode.
    pub fn classifier(&self) -> Box<dyn Classifier> {
        match self {
            WorkMode::Manual => Box::new(ManualClassifier),
            WorkMode::AllowEdits => Box::new(AllowEditsClassifier),
            WorkMode::Yolo => Box::new(YoloClassifier),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classify(mode: WorkMode, name: &str) -> Verdict {
        mode.classifier().classify(name, r#"{}"#)
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
    fn allow_edits_allows_all() {
        assert!(matches!(classify(WorkMode::AllowEdits, "command"), Verdict::Allow));
        assert!(matches!(classify(WorkMode::AllowEdits, "write_file"), Verdict::Allow));
        assert!(matches!(classify(WorkMode::AllowEdits, "read_file"), Verdict::Allow));
    }

    #[test]
    fn mode_cycle() {
        assert_eq!(WorkMode::Manual.next(), WorkMode::AllowEdits);
        assert_eq!(WorkMode::AllowEdits.next(), WorkMode::Yolo);
        assert_eq!(WorkMode::Yolo.next(), WorkMode::Manual);
    }
}
