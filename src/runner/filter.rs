// Copyright (C) 2026 huangdihd
// SPDX-License-Identifier: GPL-3.0-or-later

//! Per-run capability selection after all providers have been assembled.
use async_openai::types::responses::Tool;

/// Empty allow list means unrestricted; deny patterns take precedence.
/// Patterns are exact names or prefixes ending in `*` (including `*`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ToolFilter {
    pub allowed: Vec<String>,
    pub disallowed: Vec<String>,
}

impl ToolFilter {
    pub(crate) fn permits(&self, name: &str) -> bool {
        (self.allowed.is_empty() || self.allowed.iter().any(|p| matches(p, name)))
            && !self.disallowed.iter().any(|p| matches(p, name))
    }

    /// Check the unfiltered, fully connected registry, not the selected subset.
    pub(crate) fn unmatched_patterns(&self, tools: &[Tool]) -> Vec<String> {
        self.allowed
            .iter()
            .chain(&self.disallowed)
            .filter(|pattern| {
                !tools.iter().any(|tool| match tool {
                    Tool::Function(f) => matches(pattern, &f.name),
                    _ => false,
                })
            })
            .cloned()
            .collect()
    }

    pub(crate) fn select(&self, tools: Vec<Tool>) -> Vec<Tool> {
        tools
            .into_iter()
            .filter(|tool| match tool {
                Tool::Function(f) => self.permits(&f.name),
                _ => self.allowed.is_empty(),
            })
            .collect()
    }
}

fn matches(pattern: &str, name: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => pattern == name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_exact_prefix_and_deny_precedence() {
        assert!(ToolFilter::default().permits("anything"));
        let filter = ToolFilter {
            allowed: vec!["read_*".into(), "mcp__server__*".into()],
            disallowed: vec!["read_image".into()],
        };
        assert!(filter.permits("read_file"));
        assert!(filter.permits("mcp__server__echo"));
        assert!(!filter.permits("read_image"));
        assert!(!filter.permits("write_file"));
        assert!(!matches("read", "read_file"));
        assert!(!matches("r*file", "read_file"));
        assert!(matches("*", "anything"));
    }

    #[test]
    fn warnings_check_unfiltered_registry() {
        let tools = vec![crate::tools::read_file::tool()];
        let filter = ToolFilter {
            allowed: vec!["read_*".into(), "mcp__missing__*".into()],
            disallowed: vec!["read_file".into(), "unknown".into()],
        };
        assert_eq!(
            filter.unmatched_patterns(&tools),
            ["mcp__missing__*", "unknown"]
        );
        assert!(filter.select(tools).is_empty());
    }
}
