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
    agent, ask_user, blob, command, configure_diagnostics, diagnostics, edit_file, fetch, grep,
    load_skill, mcp_bridge, read_file, read_image, request_permission, run_local_tool, task, todo,
    write_file,
};
use crate::mcp::McpManager;
use crate::ui::event::Event;
use async_openai::types::responses::{FunctionCallOutput, FunctionToolCall, Tool};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::UnboundedSender;

/// A provider's verdict on whether a call needs the work-mode classifier — the
/// single "does this go through the classifier?" decision that used to be split
/// between the classifier's read-only fast-path and the MCP per-server policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolApproval {
    /// Safe to run without review — bypass the classifier entirely (read-only
    /// built-ins and MCP tools declaring `readOnlyHint: true`).
    AutoApprove,
    /// Must go through the work-mode classifier (mutating built-ins and MCP
    /// tools without an explicit read-only hint).
    Classify,
}

/// The parent-only provider for the `agent` lifecycle tool. Child runners use
/// the base registry stored in `runtime`, which omits this provider and thereby
/// cannot create grandchildren.
pub(crate) struct AgentToolProvider {
    manager: crate::agents::AgentManager,
    runtime: crate::agents::AgentRuntime,
}

impl AgentToolProvider {
    pub(crate) fn new(
        manager: crate::agents::AgentManager,
        runtime: crate::agents::AgentRuntime,
    ) -> Self {
        Self { manager, runtime }
    }
}

#[async_trait::async_trait]
impl ToolProvider for AgentToolProvider {
    fn tools(&self) -> Vec<Tool> {
        vec![agent::tool()]
    }

    fn approval(&self, name: &str, arguments: &str) -> ToolApproval {
        if name == agent::NAME && agent::is_observational(arguments) {
            ToolApproval::AutoApprove
        } else {
            ToolApproval::Classify
        }
    }

    async fn call(
        &self,
        call: &FunctionToolCall,
        ctx: &ToolCtx<'_>,
    ) -> Result<FunctionCallOutput, String> {
        agent::run(
            &call.arguments,
            &self.manager,
            &self.runtime,
            ctx.sender.clone(),
        )
        .await
        .map(FunctionCallOutput::Text)
    }
}

/// What a provider needs at call time beyond the call itself. Currently just the
/// front-end event channel that interactive tools (`ask_user`) prompt through,
/// the operation id for event tagging, and the cancellation token.
pub(crate) struct ToolCtx<'a> {
    pub sender: &'a UnboundedSender<Event>,
    pub cancel: &'a crate::cancel::CancellationToken,
    pub operation_id: u64,
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
    async fn call(
        &self,
        call: &FunctionToolCall,
        ctx: &ToolCtx<'_>,
    ) -> Result<FunctionCallOutput, String>;
}

/// Read-only provider that loads the body of an enabled skill on demand.
pub(crate) struct SkillToolProvider {
    registry: crate::skills::SkillRegistry,
}

impl SkillToolProvider {
    pub(crate) fn new(registry: crate::skills::SkillRegistry) -> Self {
        Self { registry }
    }
}

#[async_trait::async_trait]
impl ToolProvider for SkillToolProvider {
    fn tools(&self) -> Vec<Tool> {
        vec![load_skill::tool()]
    }

    fn is_read_only(&self, name: &str) -> bool {
        name == load_skill::NAME
    }

    fn approval(&self, _name: &str, _arguments: &str) -> ToolApproval {
        ToolApproval::AutoApprove
    }

    async fn call(
        &self,
        call: &FunctionToolCall,
        _ctx: &ToolCtx<'_>,
    ) -> Result<FunctionCallOutput, String> {
        load_skill::run(&call.arguments, &self.registry).map(FunctionCallOutput::Text)
    }
}

/// The built-in local tools, exposed as one provider — the local analogue of an
/// MCP server.
pub(crate) struct LocalToolProvider {
    todos: Arc<Mutex<crate::todos::TodoList>>,
    security: Arc<crate::security::SecurityHandle>,
    file_scope: u64,
}

impl LocalToolProvider {
    pub(crate) fn new(
        todos: Arc<Mutex<crate::todos::TodoList>>,
        security: Arc<crate::security::SecurityHandle>,
    ) -> Self {
        Self {
            todos,
            security,
            file_scope: 0,
        }
    }

    pub(crate) fn new_scoped(
        todos: Arc<Mutex<crate::todos::TodoList>>,
        security: Arc<crate::security::SecurityHandle>,
        file_scope: u64,
    ) -> Self {
        Self {
            todos,
            security,
            file_scope,
        }
    }
}

impl Default for LocalToolProvider {
    fn default() -> Self {
        let security = if cfg!(test) {
            crate::security::SecurityManager::standalone()
        } else {
            crate::security::SecurityManager::for_current_dir(Default::default())
        }
        .expect("the current directory should support the default security policy");
        Self::new(
            Arc::new(Mutex::new(crate::todos::TodoList::default())),
            Arc::new(crate::security::SecurityHandle::new(Arc::new(security))),
        )
    }
}

#[async_trait::async_trait]
impl ToolProvider for LocalToolProvider {
    fn tools(&self) -> Vec<Tool> {
        vec![
            command::tool(),
            request_permission::tool(),
            read_file::tool(),
            read_image::tool(),
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
        super::is_read_only_builtin(name)
    }

    fn requires_interaction(&self, name: &str) -> bool {
        matches!(name, ask_user::NAME | request_permission::NAME)
    }

    fn approval(&self, name: &str, arguments: &str) -> ToolApproval {
        if name == request_permission::NAME {
            // The tool itself cannot change anything until the user approves
            // its dedicated prompt, so another classifier/approval round-trip
            // would be redundant.
            return ToolApproval::AutoApprove;
        }
        // Mutating built-ins are classified; read-only ones auto-approve. This is
        // exactly the classifier's old read-only fast-path, now owned here.
        if crate::classifier::needs_review(name, arguments) {
            ToolApproval::Classify
        } else {
            ToolApproval::AutoApprove
        }
    }

    async fn call(
        &self,
        call: &FunctionToolCall,
        ctx: &ToolCtx<'_>,
    ) -> Result<FunctionCallOutput, String> {
        if call.name == ask_user::NAME {
            // ask_user needs the UI channel, so it isn't part of run_local_tool.
            ask_user::run(&call.arguments, ctx.sender, ctx.cancel, ctx.operation_id)
                .await
                .map(FunctionCallOutput::Text)
        } else if call.name == request_permission::NAME {
            request_permission::run(
                &call.arguments,
                ctx.sender,
                ctx.cancel,
                ctx.operation_id,
                &self.security,
            )
            .await
            .map(FunctionCallOutput::Text)
        } else if call.name == command::NAME {
            let security = self.security.snapshot();
            // The command tool streams its output to the live registry (keyed by
            // call id) so the TUI can render it as it runs.
            command::run_with_live_secure(&call.arguments, &call.call_id, ctx.cancel, &security)
                .await
                .map(FunctionCallOutput::Text)
        } else if call.name == todo::NAME {
            todo::run(&call.arguments, &self.todos)
                .await
                .map(FunctionCallOutput::Text)
        } else if call.name == read_image::NAME {
            let security = self.security.snapshot();
            read_image::run_with_security(&call.arguments, &security).await
        } else if call.name == read_file::NAME {
            let security = self.security.snapshot();
            read_file::run_with_security_scope(&call.arguments, &security, self.file_scope)
                .await
                .map(FunctionCallOutput::Text)
        } else if call.name == write_file::NAME {
            let security = self.security.snapshot();
            write_file::run_with_security_scope(&call.arguments, &security, self.file_scope)
                .await
                .map(FunctionCallOutput::Text)
        } else if call.name == edit_file::NAME {
            let security = self.security.snapshot();
            edit_file::run_with_security_scope(&call.arguments, &security, self.file_scope)
                .await
                .map(FunctionCallOutput::Text)
        } else if call.name == task::NAME {
            let security = self.security.snapshot();
            task::run_with_security(&call.arguments, &security)
                .await
                .map(FunctionCallOutput::Text)
        } else {
            let security = self.security.snapshot();
            security.authorize_tool_call(&call.name, &call.arguments)?;
            run_local_tool(&call.name, &call.arguments)
                .await
                .map(FunctionCallOutput::Text)
        }
    }
}

/// All connected MCP servers, exposed as one provider. Advertises the bridged
/// `mcp__<server>__<tool>` tools (plus the synthetic resource/prompt tools) and
/// routes calls back through [`mcp_bridge`].
pub(crate) struct McpToolProvider {
    pub manager: Arc<McpManager>,
    /// Read-only hints captured from the server's current `tools/list` result.
    /// The registry is rebuilt at the start of every turn, so refreshed MCP
    /// metadata is picked up without mutating a runner already in flight.
    declared_tool_read_only: HashMap<String, bool>,
}

impl McpToolProvider {
    pub(crate) fn new(manager: Arc<McpManager>) -> Self {
        let declared_tool_read_only = manager
            .all_tools()
            .into_iter()
            .map(|(name, tool)| (name, tool.is_read_only()))
            .collect();
        Self {
            manager,
            declared_tool_read_only,
        }
    }
}

#[async_trait::async_trait]
impl ToolProvider for McpToolProvider {
    fn tools(&self) -> Vec<Tool> {
        let mut tools = Vec::new();
        mcp_bridge::extend_with_mcp_tools(&mut tools, &self.manager);
        tools
    }

    fn is_read_only(&self, name: &str) -> bool {
        self.declared_tool_read_only
            .get(name)
            .copied()
            .unwrap_or_else(|| mcp_bridge::is_synthetic_read_only(name, &self.manager))
    }

    fn approval(&self, name: &str, _arguments: &str) -> ToolApproval {
        if self.is_read_only(name) {
            ToolApproval::AutoApprove
        } else {
            ToolApproval::Classify
        }
    }

    async fn call(
        &self,
        call: &FunctionToolCall,
        _ctx: &ToolCtx<'_>,
    ) -> Result<FunctionCallOutput, String> {
        mcp_bridge::run_mcp_call(call, Some(self.manager.as_ref()))
            .await
            .map(FunctionCallOutput::Text)
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
    /// Tool name → compiled parameter schema. Calls are checked against this
    /// before either the approval policy or classifier sees them.
    validators: HashMap<String, Result<jsonschema::Validator, String>>,
    /// The aggregated advertised list, precomputed so `tools()` is cheap.
    advertised: Vec<Tool>,
}

impl ToolRegistry {
    pub(crate) fn new(providers: Vec<Arc<dyn ToolProvider>>) -> Self {
        let mut routes: HashMap<String, usize> = HashMap::new();
        let mut validators = HashMap::new();
        let mut advertised: Vec<Tool> = Vec::new();
        for (i, provider) in providers.iter().enumerate() {
            for tool in provider.tools() {
                if let Tool::Function(f) = &tool
                    && let std::collections::hash_map::Entry::Vacant(route) =
                        routes.entry(f.name.clone())
                {
                    route.insert(i);
                    let schema = f
                        .parameters
                        .clone()
                        .unwrap_or_else(|| serde_json::json!({"type": "object"}));
                    validators.insert(
                        f.name.clone(),
                        jsonschema::validator_for(&schema).map_err(|error| error.to_string()),
                    );
                }
                advertised.push(tool);
            }
        }
        Self {
            providers,
            routes,
            validators,
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

    /// Validate a model-produced call before approval or classification. This
    /// keeps malformed or unknown calls out of the policy model and returns a
    /// deterministic error to the conversation instead.
    pub(crate) fn validate(&self, call: &FunctionToolCall) -> Result<(), String> {
        let validator = self
            .validators
            .get(&call.name)
            .ok_or_else(|| format!("unknown tool '{}'", call.name))?
            .as_ref()
            .map_err(|error| {
                format!(
                    "tool '{}' has an invalid parameter schema: {error}",
                    call.name
                )
            })?;
        let arguments: serde_json::Value = serde_json::from_str(&call.arguments)
            .map_err(|error| format!("arguments are not valid JSON: {error}"))?;
        validator
            .validate(&arguments)
            .map_err(|error| format!("arguments do not match the schema: {error}"))
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
    ) -> Result<FunctionCallOutput, String> {
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
        let p = LocalToolProvider::default();
        let names: Vec<String> = p
            .tools()
            .iter()
            .filter_map(|t| match t {
                Tool::Function(f) => Some(f.name.clone()),
                _ => None,
            })
            .collect();
        assert!(names.contains(&read_file::NAME.to_string()));
        assert!(names.contains(&read_image::NAME.to_string()));
        assert!(names.contains(&ask_user::NAME.to_string()));
        assert!(names.contains(&request_permission::NAME.to_string()));
        // Read-only classification drives concurrent execution.
        assert!(p.is_read_only(read_file::NAME));
        assert!(p.is_read_only(read_image::NAME));
        assert!(!p.is_read_only(write_file::NAME));
        // Interaction classification drives the headless pre-deny.
        assert!(p.requires_interaction(ask_user::NAME));
        assert!(p.requires_interaction(request_permission::NAME));
        assert!(!p.requires_interaction(read_file::NAME));
    }

    #[test]
    fn registry_aggregates_and_routes_metadata() {
        let reg = ToolRegistry::new(vec![Arc::new(LocalToolProvider::default())]);
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
    fn skill_provider_advertises_a_read_only_loader() {
        let provider = SkillToolProvider::new(crate::skills::SkillRegistry::load());

        assert!(provider.tools().iter().any(|tool| matches!(
            tool,
            Tool::Function(function) if function.name == load_skill::NAME
        )));
        assert!(provider.is_read_only(load_skill::NAME));
        assert_eq!(
            provider.approval(load_skill::NAME, r#"{"name":"programmer-guide"}"#),
            ToolApproval::AutoApprove
        );
    }

    #[test]
    fn registry_validates_calls_before_policy_routing() {
        let reg = ToolRegistry::new(vec![Arc::new(LocalToolProvider::default())]);
        let valid = call(read_file::NAME, r#"{"path":"src/main.rs"}"#);
        assert!(reg.validate(&valid).is_ok());

        let sandbox_permission = call(
            request_permission::NAME,
            r#"{"kind":"sandbox","mode":"network","operation":null,"path":null,"reason":"download dependencies"}"#,
        );
        assert!(reg.validate(&sandbox_permission).is_ok());
        let filesystem_permission = call(
            request_permission::NAME,
            r#"{"kind":"filesystem","mode":null,"operation":"write","path":"/tmp/output","reason":"write the requested output"}"#,
        );
        assert!(reg.validate(&filesystem_permission).is_ok());

        let missing_required = call(read_file::NAME, "{}");
        assert!(
            reg.validate(&missing_required)
                .unwrap_err()
                .contains("required")
        );

        let malformed = call(read_file::NAME, "{");
        assert!(
            reg.validate(&malformed)
                .unwrap_err()
                .contains("not valid JSON")
        );

        let unknown = call("not_advertised", "{}");
        assert!(reg.validate(&unknown).unwrap_err().contains("unknown tool"));
    }

    #[test]
    fn local_provider_approval_gates_mutating_only() {
        let reg = ToolRegistry::new(vec![Arc::new(LocalToolProvider::default())]);
        // Read-only built-ins auto-approve (bypass the classifier)...
        assert_eq!(
            reg.approval(read_file::NAME, "{}"),
            ToolApproval::AutoApprove
        );
        assert_eq!(reg.approval(grep::NAME, "{}"), ToolApproval::AutoApprove);
        // ...mutating ones are classified.
        assert_eq!(reg.approval(write_file::NAME, "{}"), ToolApproval::Classify);
        assert_eq!(reg.approval(command::NAME, "{}"), ToolApproval::Classify);
        assert_eq!(
            reg.approval(request_permission::NAME, "{}"),
            ToolApproval::AutoApprove
        );
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

    #[tokio::test]
    async fn registry_dispatches_a_local_call_and_rejects_unknown() {
        let reg = ToolRegistry::new(vec![Arc::new(LocalToolProvider::default())]);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let cancel = crate::cancel::CancellationToken::new();
        let ctx = ToolCtx {
            sender: &tx,
            cancel: &cancel,
            operation_id: 0,
        };

        // A real local dispatch: write a temp file, then read it back.
        let tmp = std::env::temp_dir().join(format!("registry_dispatch_{}", std::process::id()));
        let path = serde_json::to_string(&tmp.to_string_lossy()).unwrap();
        let _ = std::fs::remove_file(&tmp);
        let w = reg
            .call(
                &call(
                    "write_file",
                    &format!("{{\"path\":{path},\"content\":\"hello\"}}"),
                ),
                &ctx,
            )
            .await;
        assert!(w.is_ok(), "write dispatched: {w:?}");
        let r = reg
            .call(&call("read_file", &format!("{{\"path\":{path}}}")), &ctx)
            .await;
        assert_eq!(r, Ok(FunctionCallOutput::Text("hello".to_string())));
        let _ = std::fs::remove_file(&tmp);

        // An unadvertised name is a failed result, not a panic.
        let unknown = reg.call(&call("does_not_exist", "{}"), &ctx).await;
        assert!(unknown.is_err());
        assert!(unknown.unwrap_err().contains("unknown tool"));
    }
}
