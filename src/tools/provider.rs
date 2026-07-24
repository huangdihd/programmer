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

//! Tool providers: one interface over every source of tools the agent can call.
//!
//! The built-in local tools used to be a hardcoded `match`, and MCP servers a
//! separate `mcp__`-prefix branch. Both are now [`ToolProvider`]s — the local
//! built-ins are a single [`LocalToolProvider`], and all connected MCP servers a
//! single [`McpToolProvider`]. A [`ToolRegistry`] aggregates any number of
//! providers, builds the advertised tool list, and routes a call to its owning
//! provider by a name→provider table (built once from `tools()`), so dispatch
//! never sniffs prefixes.

use super::{
    ask_user, blob, command, configure_diagnostics, diagnostics, edit_file, fetch, grep,
    mcp_bridge, read_file, run_local_tool, task, todo, write_file,
};
use crate::mcp::McpManager;
use crate::mcp::types::McpPolicy;
use crate::ui::event::Event;
use async_openai::types::responses::{FunctionToolCall, Tool};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;

/// A provider's verdict on whether a call needs the work-mode classifier — the
/// single "does this go through the classifier?" decision that used to be split
/// between the classifier's read-only fast-path and the MCP per-server policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolApproval {
    /// Safe to run without review — bypass the classifier entirely (read-only
    /// built-ins, tools on a trusted MCP server).
    AutoApprove,
    /// Must go through the work-mode classifier (mutating built-ins, tools on an
    /// MCP server marked for review).
    Classify,
}

/// What a provider needs at call time beyond the call itself. Currently just the
/// front-end event channel that interactive tools (`ask_user`) prompt through.
pub(crate) struct ToolCtx<'a> {
    pub sender: &'a UnboundedSender<Event>,
}

/// A source of tools the agent can call. Implemented once for the local
/// built-ins and once for the connected MCP servers; more can be added.
#[async_trait::async_trait]
pub(crate) trait ToolProvider: Send + Sync {
    /// The tool definitions this provider advertises to the model.
    fn tools(&self) -> Vec<Tool>;

    /// Whether `name` is read-only (side-effect-free), so the batch executor may
    /// run it concurrently with other reads. Defaults to serial-only.
    fn is_read_only(&self, _name: &str) -> bool {
        false
    }

    /// Whether `name` needs an interactive front-end (e.g. `ask_user`), so a
    /// headless caller pre-denies it rather than hanging. Defaults to false.
    fn requires_interaction(&self, _name: &str) -> bool {
        false
    }

    /// Whether a call to `name` (with `arguments`) may run without classifier
    /// review, or must be classified. This is the provider's own policy — the
    /// front gate the runner consults before ever invoking the work-mode
    /// classifier. Defaults to the safe choice: classify.
    fn approval(&self, _name: &str, _arguments: &str) -> ToolApproval {
        ToolApproval::Classify
    }

    /// Execute one call this provider owns, returning the raw tool result
    /// (`Ok` = success, `Err` = failure — the caller wraps and truncates it).
    async fn call(&self, call: &FunctionToolCall, ctx: &ToolCtx<'_>) -> Result<String, String>;
}

/// The built-in local tools, exposed as one provider — the local analogue of an
/// MCP server.
pub(crate) struct LocalToolProvider;

#[async_trait::async_trait]
impl ToolProvider for LocalToolProvider {
    fn tools(&self) -> Vec<Tool> {
        vec![
            command::tool(),
            read_file::tool(),
            write_file::tool(),
            edit_file::tool(),
            grep::tool(),
            blob::tool(),
            fetch::tool(),
            ask_user::tool(),
            configure_diagnostics::tool(),
            diagnostics::tool(),
            todo::tool(),
            task::tool(),
        ]
    }

    fn is_read_only(&self, name: &str) -> bool {
        matches!(
            name,
            read_file::NAME | grep::NAME | blob::NAME | fetch::NAME
        )
    }

    fn requires_interaction(&self, name: &str) -> bool {
        name == ask_user::NAME
    }

    fn approval(&self, name: &str, arguments: &str) -> ToolApproval {
        // Mutating built-ins are classified; read-only ones auto-approve. This is
        // exactly the classifier's old read-only fast-path, now owned here.
        if crate::classifier::needs_review(name, arguments) {
            ToolApproval::Classify
        } else {
            ToolApproval::AutoApprove
        }
    }

    async fn call(&self, call: &FunctionToolCall, ctx: &ToolCtx<'_>) -> Result<String, String> {
        if call.name == ask_user::NAME {
            // ask_user needs the UI channel, so it isn't part of run_local_tool.
            ask_user::run(&call.arguments, ctx.sender).await
        } else {
            run_local_tool(&call.name, &call.arguments).await
        }
    }
}

/// All connected MCP servers, exposed as one provider. Advertises the bridged
/// `mcp__<server>__<tool>` tools (plus the synthetic resource/prompt tools) and
/// routes calls back through [`mcp_bridge`].
pub(crate) struct McpToolProvider {
    pub manager: Arc<McpManager>,
    /// Per-server trust policy (server name → policy). A tool on a `Trusted`
    /// server auto-approves; one on a `Review` server (the default) is classified.
    pub policies: HashMap<String, McpPolicy>,
}

#[async_trait::async_trait]
impl ToolProvider for McpToolProvider {
    fn tools(&self) -> Vec<Tool> {
        let mut tools = Vec::new();
        mcp_bridge::extend_with_mcp_tools(&mut tools, &self.manager);
        tools
    }

    fn approval(&self, name: &str, _arguments: &str) -> ToolApproval {
        // Trusted server → skip the classifier; Review (or unknown) → classify.
        // This is the classifier's old `classify_mcp_policy`, now owned here.
        match crate::classifier::classify_mcp_policy(name, &self.policies) {
            Some(crate::classifier::Verdict::Allow) => ToolApproval::AutoApprove,
            _ => ToolApproval::Classify,
        }
    }

    async fn call(&self, call: &FunctionToolCall, _ctx: &ToolCtx<'_>) -> Result<String, String> {
        mcp_bridge::run_mcp_call(call, Some(self.manager.as_ref())).await
    }
}

/// Aggregates providers into one tool surface: the combined advertised list, the
/// per-tool metadata (read-only, interaction), and call routing.
///
/// The name→provider routes are built once at construction from each provider's
/// `tools()`. The runner rebuilds the registry each turn (like everything else
/// derived from app state), so a dynamic MCP tool list is always fresh at turn
/// start — which is the only point the advertised set matters.
pub(crate) struct ToolRegistry {
    providers: Vec<Arc<dyn ToolProvider>>,
    /// Tool name → index into `providers`. First provider to claim a name wins.
    routes: HashMap<String, usize>,
    /// The aggregated advertised list, precomputed so `tools()` is cheap.
    advertised: Vec<Tool>,
}

impl ToolRegistry {
    pub(crate) fn new(providers: Vec<Arc<dyn ToolProvider>>) -> Self {
        let mut routes: HashMap<String, usize> = HashMap::new();
        let mut advertised: Vec<Tool> = Vec::new();
        for (i, provider) in providers.iter().enumerate() {
            for tool in provider.tools() {
                if let Tool::Function(f) = &tool {
                    routes.entry(f.name.clone()).or_insert(i);
                }
                advertised.push(tool);
            }
        }
        Self {
            providers,
            routes,
            advertised,
        }
    }

    /// The combined advertised tool list, for the request builder.
    pub(crate) fn tools(&self) -> Vec<Tool> {
        self.advertised.clone()
    }

    fn provider_for(&self, name: &str) -> Option<&Arc<dyn ToolProvider>> {
        self.routes.get(name).map(|&i| &self.providers[i])
    }

    /// Whether `name` may run concurrently with other reads.
    pub(crate) fn is_read_only(&self, name: &str) -> bool {
        self.provider_for(name)
            .is_some_and(|p| p.is_read_only(name))
    }

    /// Whether `name` needs an interactive front-end.
    pub(crate) fn requires_interaction(&self, name: &str) -> bool {
        self.provider_for(name)
            .is_some_and(|p| p.requires_interaction(name))
    }

    /// The owning provider's approval policy for a call — the front gate the
    /// runner consults to decide whether to classify it. An unrouted name
    /// defaults to `Classify` (the model asked for a tool that isn't advertised;
    /// it will be denied as unknown at execution, but classifying is the safe
    /// stance).
    pub(crate) fn approval(&self, name: &str, arguments: &str) -> ToolApproval {
        self.provider_for(name)
            .map(|p| p.approval(name, arguments))
            .unwrap_or(ToolApproval::Classify)
    }

    /// Route `call` to its owning provider and execute it. An unrecognised name
    /// (the model asked for a tool that isn't advertised) is a failed result.
    pub(crate) async fn call(
        &self,
        call: &FunctionToolCall,
        ctx: &ToolCtx<'_>,
    ) -> Result<String, String> {
        match self.provider_for(&call.name) {
            Some(provider) => provider.call(call, ctx).await,
            None => Err(format!("error: unknown tool '{}'", call.name)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, args: &str) -> FunctionToolCall {
        FunctionToolCall {
            arguments: args.into(),
            call_id: format!("c_{name}"),
            namespace: None,
            name: name.into(),
            id: None,
            status: None,
        }
    }

    #[test]
    fn local_provider_advertises_builtins_with_metadata() {
        let p = LocalToolProvider;
        let names: Vec<String> = p
            .tools()
            .iter()
            .filter_map(|t| match t {
                Tool::Function(f) => Some(f.name.clone()),
                _ => None,
            })
            .collect();
        assert!(names.contains(&read_file::NAME.to_string()));
        assert!(names.contains(&ask_user::NAME.to_string()));
        // Read-only classification drives concurrent execution.
        assert!(p.is_read_only(read_file::NAME));
        assert!(!p.is_read_only(write_file::NAME));
        // Interaction classification drives the headless pre-deny.
        assert!(p.requires_interaction(ask_user::NAME));
        assert!(!p.requires_interaction(read_file::NAME));
    }

    #[test]
    fn registry_aggregates_and_routes_metadata() {
        let reg = ToolRegistry::new(vec![Arc::new(LocalToolProvider)]);
        // The advertised list carries the built-ins.
        assert!(reg.tools().iter().any(|t| matches!(
            t, Tool::Function(f) if f.name == command::NAME
        )));
        // Metadata is routed to the owning provider.
        assert!(reg.is_read_only(grep::NAME));
        assert!(!reg.is_read_only(edit_file::NAME));
        assert!(reg.requires_interaction(ask_user::NAME));
        // An unknown tool has no route: not read-only, not interactive.
        assert!(!reg.is_read_only("nope"));
        assert!(!reg.requires_interaction("nope"));
    }

    #[test]
    fn local_provider_approval_gates_mutating_only() {
        let reg = ToolRegistry::new(vec![Arc::new(LocalToolProvider)]);
        // Read-only built-ins auto-approve (bypass the classifier)...
        assert_eq!(reg.approval(read_file::NAME, "{}"), ToolApproval::AutoApprove);
        assert_eq!(reg.approval(grep::NAME, "{}"), ToolApproval::AutoApprove);
        // ...mutating ones are classified.
        assert_eq!(reg.approval(write_file::NAME, "{}"), ToolApproval::Classify);
        assert_eq!(reg.approval(command::NAME, "{}"), ToolApproval::Classify);
        // The task tool is action-dependent: observe auto-approves, create classifies.
        assert_eq!(
            reg.approval(task::NAME, r#"{"action":"list"}"#),
            ToolApproval::AutoApprove
        );
        assert_eq!(
            reg.approval(task::NAME, r#"{"action":"create","command":"x"}"#),
            ToolApproval::Classify
        );
        // Unknown tool: classify (safe default).
        assert_eq!(reg.approval("nope", "{}"), ToolApproval::Classify);
    }

    // (McpToolProvider::approval is a thin wrapper over the classifier's
    // `classify_mcp_policy`, which is covered by its own Trusted/Review tests.)

    #[tokio::test]
    async fn registry_dispatches_a_local_call_and_rejects_unknown() {
        let reg = ToolRegistry::new(vec![Arc::new(LocalToolProvider)]);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = ToolCtx { sender: &tx };

        // A real local dispatch: write a temp file, then read it back.
        let tmp = std::env::temp_dir().join(format!("registry_dispatch_{}", std::process::id()));
        let path = serde_json::to_string(&tmp.to_string_lossy()).unwrap();
        let _ = std::fs::remove_file(&tmp);
        let w = reg
            .call(
                &call("write_file", &format!("{{\"path\":{path},\"content\":\"hello\"}}")),
                &ctx,
            )
            .await;
        assert!(w.is_ok(), "write dispatched: {w:?}");
        let r = reg
            .call(&call("read_file", &format!("{{\"path\":{path}}}")), &ctx)
            .await;
        assert_eq!(r.as_deref(), Ok("hello"));
        let _ = std::fs::remove_file(&tmp);

        // An unadvertised name is a failed result, not a panic.
        let unknown = reg.call(&call("does_not_exist", "{}"), &ctx).await;
        assert!(unknown.is_err());
        assert!(unknown.unwrap_err().contains("unknown tool"));
    }
}
