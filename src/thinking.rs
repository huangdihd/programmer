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

//! User-selectable reasoning effort for chat and compaction requests.

use async_openai::types::responses::{Reasoning, ReasoningEffort};
use serde::{Deserialize, Serialize};

/// Reasoning effort applied to the main conversation and `/compact`.
///
/// `Auto` deliberately omits the API field so each provider/model can use its
/// own default. Every other variant sends an explicit OpenAI-compatible value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ThinkingLevel {
    #[default]
    Auto,
    None,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
}

impl ThinkingLevel {
    pub(crate) const COMPLETIONS: &'static [&'static str] =
        &["auto", "none", "minimal", "low", "medium", "high", "xhigh"];
    pub(crate) const VALUES: &'static str = "auto, none, minimal, low, medium, high, or xhigh";

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" | "default" | "reset" => Some(Self::Auto),
            "none" => Some(Self::None),
            "minimal" => Some(Self::Minimal),
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            "xhigh" => Some(Self::Xhigh),
            _ => None,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::None => "none",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
        }
    }

    /// Build the API reasoning object, or `None` for provider/model defaults.
    pub(crate) fn reasoning(self) -> Option<Reasoning> {
        let effort = match self {
            Self::Auto => return None,
            Self::None => ReasoningEffort::None,
            Self::Minimal => ReasoningEffort::Minimal,
            Self::Low => ReasoningEffort::Low,
            Self::Medium => ReasoningEffort::Medium,
            Self::High => ReasoningEffort::High,
            Self::Xhigh => ReasoningEffort::Xhigh,
        };
        Some(effort.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_omits_reasoning_and_explicit_levels_serialize() {
        assert!(ThinkingLevel::Auto.reasoning().is_none());

        for (level, expected) in [
            (ThinkingLevel::None, "none"),
            (ThinkingLevel::Minimal, "minimal"),
            (ThinkingLevel::Low, "low"),
            (ThinkingLevel::Medium, "medium"),
            (ThinkingLevel::High, "high"),
            (ThinkingLevel::Xhigh, "xhigh"),
        ] {
            let value = serde_json::to_value(level.reasoning()).unwrap();
            assert_eq!(value["effort"], expected);
        }
    }

    #[test]
    fn parsing_uses_protocol_names_and_auto_aliases() {
        assert_eq!(ThinkingLevel::parse("NONE"), Some(ThinkingLevel::None));
        assert_eq!(ThinkingLevel::parse("default"), Some(ThinkingLevel::Auto));
        assert_eq!(ThinkingLevel::parse("off"), None);
    }
}
