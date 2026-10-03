// Copyright (C) 2026 huangdihd
// SPDX-License-Identifier: GPL-3.0-or-later

//! Stable owner of reload state; runners retain a connection snapshot for their turn.

use std::sync::Arc;

use super::{McpConnectionState, McpManager, McpServerStatus, types::McpServerConfig};

#[derive(Default)]
pub(crate) struct McpRuntime {
    connections: Option<Arc<McpManager>>,
    statuses: Vec<McpServerStatus>,
    generation: u64,
}

impl McpRuntime {
    pub(crate) fn begin_reload(&mut self, configuration: &[McpServerConfig]) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        // Existing runners retain their Arc; new turns must not use removed servers.
        self.connections = None;
        self.statuses = configuration
            .iter()
            .map(|server| McpServerStatus::connecting(server.name.clone()))
            .collect();
        self.generation
    }

    pub(crate) fn statuses(&self) -> &[McpServerStatus] {
        &self.statuses
    }

    pub(crate) fn connections(&self) -> Option<&Arc<McpManager>> {
        self.connections.as_ref()
    }

    pub(crate) fn update_status(&mut self, generation: u64, name: &str, state: McpConnectionState) {
        if generation != self.generation {
            return;
        }
        if let Some(server) = self.statuses.iter_mut().find(|server| server.name == name) {
            server.state = state;
        }
    }

    /// Returns accepted startup errors for presentation; stale results have no effects.
    pub(crate) fn finish_reload(
        &mut self,
        generation: u64,
        manager: McpManager,
    ) -> Option<Vec<String>> {
        if generation != self.generation {
            return None;
        }
        let errors = manager.startup_errors.clone();
        self.connections = Some(Arc::new(manager));
        Some(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn empty_connections() -> McpManager {
        McpManager {
            servers: HashMap::new(),
            startup_errors: Vec::new(),
        }
    }

    #[test]
    fn stale_reload_cannot_replace_current_connections() {
        let mut runtime = McpRuntime::default();
        let first = runtime.begin_reload(&[]);
        let second = runtime.begin_reload(&[]);
        assert!(runtime.finish_reload(first, empty_connections()).is_none());
        assert!(runtime.connections().is_none());
        assert_eq!(
            runtime.finish_reload(second, empty_connections()),
            Some(Vec::new())
        );
        let snapshot = runtime.connections().unwrap().clone();
        runtime.begin_reload(&[]);
        assert!(runtime.connections().is_none());
        assert_eq!(Arc::strong_count(&snapshot), 1);
    }

    #[test]
    fn stale_progress_does_not_change_visible_status() {
        let mut runtime = McpRuntime::default();
        let first = runtime.begin_reload(&[]);
        let second = runtime.begin_reload(&[]);
        runtime
            .statuses
            .push(McpServerStatus::connecting("example"));
        runtime.update_status(
            first,
            "example",
            McpConnectionState::Connected { tool_count: 9 },
        );
        assert_eq!(runtime.statuses()[0].state, McpConnectionState::Connecting);
        runtime.update_status(
            second,
            "example",
            McpConnectionState::Connected { tool_count: 2 },
        );
        assert_eq!(
            runtime.statuses()[0].state,
            McpConnectionState::Connected { tool_count: 2 }
        );
    }
}
