// Copyright (C) 2026 huangdihd
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

//! Headless command drivers for one-shot agent and diagnostics runs.

use async_openai::types::responses::{
    FunctionToolCall, InputContent, InputMessage, InputRole, MessageItem as ApiMessageItem,
    OutputStatus,
};
use serde::Serialize;
use serde_json::json;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::AsyncReadExt;

use crate::cancel::CancellationToken;
use crate::classifier::WorkMode;
use crate::cli::{
    AgentArgs, AgentOutputFormat, DiagnosticThreshold, DiagnosticsArgs, DiagnosticsOutputFormat,
    InitArgs, RunArgs,
};
use crate::conversation::Conversation;
use crate::diagnostics::{Diagnostic, Severity, Snapshot};
use crate::runner::{
    AgentSurface, DiagnosticsState, LlmPolicy, ReviewDecision, RunnerEvent, RunnerPhase,
    RunnerPolicy, TurnResult, TurnRunner,
};
use crate::tools::provider::{
    AgentToolProvider, LocalToolProvider, SkillToolProvider, ToolProvider, ToolRegistry,
};

const OUTPUT_SCHEMA_VERSION: u32 = 1;

pub(crate) async fn run(args: RunArgs) -> color_eyre::Result<bool> {
    let RunArgs {
        prompt,
        prompt_file,
        init: run_init,
        check,
        fail_on: threshold,
        format,
        agent: agent_args,
    } = args;
    set_working_directory(agent_args.cwd.as_deref())?;
    let prompt = read_prompt(prompt, prompt_file).await?;
    let cancel = cancellation_token();
    let timeout = agent_args.timeout;

    let operation = async {
        let agent = HeadlessAgent::build(&agent_args, cancel.clone()).await?;
        if !agent_args.no_diagnostics {
            agent.seed_diagnostics_baseline().await;
        }

        if run_init {
            agent
                .run_turn(
                    agent.initialization_prompt.clone(),
                    InputRole::Developer,
                    AgentOutputFormat::Text,
                )
                .await?;
            if !agent_args.no_diagnostics {
                agent.refresh_diagnostics_baseline().await;
            }
        }

        let result = agent.run_turn(prompt, InputRole::User, format).await?;
        let diagnostics = if check {
            Some(collect_report(threshold, &cancel).await)
        } else {
            None
        };
        let passed = diagnostics.as_ref().is_none_or(|report| report.passed);
        emit_agent_result(format, &agent.model, result, diagnostics.as_ref())?;
        Ok(passed)
    };

    let result = run_with_timeout(operation, timeout, &cancel).await;
    crate::diagnostics::shutdown_lsp().await;
    result
}

pub(crate) async fn init(args: InitArgs) -> color_eyre::Result<bool> {
    let InitArgs {
        format,
        agent: agent_args,
    } = args;
    set_working_directory(agent_args.cwd.as_deref())?;
    let cancel = cancellation_token();
    let timeout = agent_args.timeout;

    let operation = async {
        let agent = HeadlessAgent::build(&agent_args, cancel.clone()).await?;
        if !agent_args.no_diagnostics {
            agent.seed_diagnostics_baseline().await;
        }
        let result = agent
            .run_turn(
                agent.initialization_prompt.clone(),
                InputRole::Developer,
                format,
            )
            .await?;
        emit_agent_result(format, &agent.model, result, None)?;
        Ok(true)
    };

    let result = run_with_timeout(operation, timeout, &cancel).await;
    crate::diagnostics::shutdown_lsp().await;
    result
}

pub(crate) async fn diagnostics(args: DiagnosticsArgs) -> color_eyre::Result<bool> {
    set_working_directory(args.cwd.as_deref())?;
    install_security()?;
    let cancel = cancellation_token();
    let report = collect_report(args.fail_on, &cancel).await;

    match args.format {
        DiagnosticsOutputFormat::Text => println!("{}", report.render()),
        DiagnosticsOutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
    }

    crate::diagnostics::shutdown_lsp().await;
    Ok(report.passed)
}

struct HeadlessAgent {
    runner: TurnRunner,
    conversation: Mutex<Conversation>,
    diagnostics_state: Arc<Mutex<DiagnosticsState>>,
    cancel: CancellationToken,
    model: String,
    mode: WorkMode,
    skill_prompt: Option<String>,
    initialization_prompt: String,
    agents: crate::agents::AgentManager,
}

impl Drop for HeadlessAgent {
    fn drop(&mut self) {
        self.agents.cancel_all();
    }
}

impl HeadlessAgent {
    async fn build(args: &AgentArgs, cancel: CancellationToken) -> color_eyre::Result<Self> {
        let (config, _) = crate::load_config()?;
        let security = crate::install_security_for_current_dir(&config)?;

        let provider_manager = crate::providers::ProviderManager::new(&config).await;
        let model = args
            .model
            .clone()
            .unwrap_or_else(|| provider_manager.default_model());
        let (client, model_name) = provider_manager
            .resolve(&model)
            .map(|(client, name)| (client.clone(), name))
            .ok_or_else(|| color_eyre::eyre::eyre!("unknown provider/model: {model}"))?;

        let todo_store = Arc::new(Mutex::new(Default::default()));
        let security = Arc::new(crate::security::SecurityHandle::new(security));
        let skill_registry = crate::skills::SkillRegistry::load();
        let skill_prompt = skill_registry.catalog_prompt();
        let initialization_prompt = skill_registry
            .prompt(crate::skills::INITIALIZE_PROJECT_SKILL)
            .ok_or_else(|| {
                color_eyre::eyre::eyre!("built-in initialize-project skill is unavailable")
            })?;
        let mut base_providers: Vec<Arc<dyn ToolProvider>> = vec![
            Arc::new(LocalToolProvider::new(todo_store.clone(), security.clone())),
            Arc::new(SkillToolProvider::new(skill_registry.clone())),
        ];

        let (policy, child_policy) = match args.work_mode {
            WorkMode::Yolo => (RunnerPolicy::Yolo, crate::agents::AgentPolicyFactory::Yolo),
            WorkMode::Plan => (
                RunnerPolicy::Sync(args.work_mode.classifier()),
                crate::agents::AgentPolicyFactory::Sync(args.work_mode),
            ),
            WorkMode::Auto => {
                let classifier_model = args
                    .classifier_model
                    .clone()
                    .or_else(|| config.classifier_model.clone())
                    .unwrap_or_else(|| provider_manager.default_classifier_model());
                let (classifier_client, classifier_name) = provider_manager
                    .resolve(&classifier_model)
                    .map(|(client, name)| (client.clone(), name))
                    .ok_or_else(|| {
                        color_eyre::eyre::eyre!(
                            "unknown classifier provider/model: {classifier_model}"
                        )
                    })?;
                let no_logprobs = Arc::new(Mutex::new(HashSet::new()));
                (
                    RunnerPolicy::Llm(Box::new(LlmPolicy {
                        client: classifier_client.clone(),
                        model_name: classifier_name.clone(),
                        no_logprobs: no_logprobs.clone(),
                    })),
                    crate::agents::AgentPolicyFactory::Llm(Box::new(LlmPolicy {
                        client: classifier_client,
                        model_name: classifier_name,
                        no_logprobs,
                    })),
                )
            }
            WorkMode::Manual => {
                return Err(color_eyre::eyre::eyre!(
                    "manual mode requires an interactive approval surface"
                ));
            }
        };

        let agents = crate::agents::AgentManager::default();
        let child_runtime = crate::agents::AgentRuntime {
            provider_manager: Arc::new(provider_manager.clone()),
            client: client.clone(),
            model_name: model_name.clone(),
            model_str: model.clone(),
            todos: todo_store,
            security,
            mcp_manager: None,
            policy: child_policy,
            coauthor: config.git_coauthor.clone(),
            vision_enabled: false,
            thinking_level: args.thinking,
            skill_registry,
            skill_prompt: skill_prompt.clone(),
            approval_label: format!(
                "{} approved by {} mode (headless sub-agent)",
                args.work_mode.icon(),
                args.work_mode.label()
            ),
        };
        base_providers.push(Arc::new(AgentToolProvider::new(
            agents.clone(),
            child_runtime,
        )));
        let tools = Arc::new(ToolRegistry::new(base_providers));

        let diagnostics_state = Arc::new(Mutex::new(DiagnosticsState::default()));
        let hooks: Vec<Arc<dyn crate::runner::hooks::TurnHook>> = if args.no_diagnostics {
            Vec::new()
        } else {
            crate::runner::hooks::standard_hooks(diagnostics_state.clone())
        };

        let runner = TurnRunner {
            client,
            model_name,
            model_str: model.clone(),
            tools,
            policy,
            coauthor: config.git_coauthor,
            vision_enabled: false,
            thinking_level: args.thinking,
            hooks,
            stream_retrying: Arc::new(AtomicBool::new(false)),
            max_steps: args.max_steps,
        };

        Ok(Self {
            runner,
            conversation: Mutex::new(Conversation::new()),
            diagnostics_state,
            cancel,
            model,
            mode: args.work_mode,
            skill_prompt,
            initialization_prompt,
            agents,
        })
    }

    async fn run_turn(
        &self,
        text: String,
        role: InputRole,
        format: AgentOutputFormat,
    ) -> color_eyre::Result<TurnResult> {
        self.conversation
            .lock()
            .unwrap()
            .add_input_message(input_message(text, role));
        let surface = CliSurface {
            jsonl: format == AgentOutputFormat::Jsonl,
            plan: self.mode == WorkMode::Plan,
            skill_prompt: self.skill_prompt.clone(),
            approval_label: format!(
                "{} approved by {} mode (headless)",
                self.mode.icon(),
                self.mode.label()
            ),
        };
        self.runner
            .run_turn(&self.conversation, &self.cancel, &surface)
            .await
            .map_err(Into::into)
    }

    async fn seed_diagnostics_baseline(&self) {
        if self.diagnostics_state.lock().unwrap().baseline.is_some() {
            return;
        }
        self.refresh_diagnostics_baseline().await;
    }

    async fn refresh_diagnostics_baseline(&self) {
        let cwd = current_dir();
        let baseline = crate::diagnostics::collect(&cwd, &self.cancel)
            .await
            .map(|snapshot| snapshot.diagnostics);
        self.diagnostics_state.lock().unwrap().baseline = baseline;
    }
}

struct CliSurface {
    jsonl: bool,
    plan: bool,
    skill_prompt: Option<String>,
    approval_label: String,
}

#[async_trait::async_trait]
impl AgentSurface for CliSurface {
    fn on_event(&self, event: RunnerEvent<'_>) {
        if !self.jsonl {
            return;
        }
        let event = match event {
            RunnerEvent::StreamChunk(_) => return,
            RunnerEvent::ResponseCommitted => json!({
                "schema_version": OUTPUT_SCHEMA_VERSION,
                "type": "response_committed",
            }),
            RunnerEvent::Assistant(text) => json!({
                "schema_version": OUTPUT_SCHEMA_VERSION,
                "type": "assistant",
                "text": text,
            }),
            RunnerEvent::ToolCall { name } => json!({
                "schema_version": OUTPUT_SCHEMA_VERSION,
                "type": "tool_call",
                "name": name,
            }),
            RunnerEvent::Phase(phase) => json!({
                "schema_version": OUTPUT_SCHEMA_VERSION,
                "type": "phase",
                "phase": phase_label(phase),
            }),
        };
        println!("{event}");
    }

    async fn review(
        &self,
        call: &FunctionToolCall,
        reason: &str,
        _position: (usize, usize),
    ) -> ReviewDecision {
        ReviewDecision::Deny {
            output: crate::runner::classify::classifier_denied_output(call, reason),
        }
    }

    fn plan_prompt(&self) -> Option<&str> {
        self.plan.then_some(crate::prompts::PLAN_PLANNING_PROMPT)
    }

    fn skill_prompt(&self) -> Option<String> {
        self.skill_prompt.clone()
    }

    fn approval_label(&self) -> String {
        self.approval_label.clone()
    }
}

fn phase_label(phase: RunnerPhase) -> &'static str {
    match phase {
        RunnerPhase::Streaming => "streaming",
        RunnerPhase::Classifying => "classifying",
        RunnerPhase::RunningTools => "running_tools",
        RunnerPhase::Checking => "checking",
    }
}

fn input_message(text: String, role: InputRole) -> ApiMessageItem {
    ApiMessageItem::Input(InputMessage {
        content: vec![InputContent::InputText(text.into())],
        role,
        status: Some(OutputStatus::Completed),
    })
}

async fn read_prompt(
    prompt: Option<String>,
    prompt_file: Option<PathBuf>,
) -> color_eyre::Result<String> {
    if let Some(path) = prompt_file {
        return std::fs::read_to_string(&path)
            .map_err(|error| color_eyre::eyre::eyre!("reading {}: {error}", path.display()));
    }
    let prompt = prompt.expect("clap requires one prompt source");
    if prompt != "-" {
        return Ok(prompt);
    }

    let mut text = String::new();
    tokio::io::stdin().read_to_string(&mut text).await?;
    if text.is_empty() {
        return Err(color_eyre::eyre::eyre!("stdin contained no prompt"));
    }
    Ok(text)
}

async fn run_with_timeout<F>(
    operation: F,
    timeout_seconds: Option<u64>,
    cancel: &CancellationToken,
) -> color_eyre::Result<bool>
where
    F: std::future::Future<Output = color_eyre::Result<bool>>,
{
    let Some(seconds) = timeout_seconds else {
        return operation.await;
    };
    match tokio::time::timeout(Duration::from_secs(seconds), operation).await {
        Ok(result) => result,
        Err(_) => {
            cancel.cancel();
            Err(color_eyre::eyre::eyre!(
                "headless operation timed out after {seconds} second(s)"
            ))
        }
    }
}

fn cancellation_token() -> CancellationToken {
    let cancel = CancellationToken::new();
    let signal = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            signal.cancel();
        }
    });
    cancel
}

pub(crate) fn set_working_directory(path: Option<&Path>) -> color_eyre::Result<()> {
    let Some(path) = path else {
        return Ok(());
    };
    std::env::set_current_dir(path).map_err(|error| {
        color_eyre::eyre::eyre!("changing directory to {}: {error}", path.display())
    })
}

fn current_dir() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn install_security() -> color_eyre::Result<()> {
    let (config, _) = crate::load_config()?;
    crate::install_security_for_current_dir(&config).map(|_| ())
}

#[derive(Debug, Serialize)]
struct AgentResult<'a> {
    schema_version: u32,
    response: &'a str,
    model: &'a str,
    usage: Usage,
    #[serde(skip_serializing_if = "Option::is_none")]
    check: Option<&'a DiagnosticsReport>,
    passed: bool,
}

#[derive(Debug, Serialize)]
struct Usage {
    input_tokens: u32,
    output_tokens: u32,
}

fn emit_agent_result(
    format: AgentOutputFormat,
    model: &str,
    result: TurnResult,
    check: Option<&DiagnosticsReport>,
) -> color_eyre::Result<()> {
    let output = AgentResult {
        schema_version: OUTPUT_SCHEMA_VERSION,
        response: &result.final_text,
        model,
        usage: Usage {
            input_tokens: result.usage.0,
            output_tokens: result.usage.1,
        },
        check,
        passed: check.is_none_or(|report| report.passed),
    };

    match format {
        AgentOutputFormat::Text => {
            println!("{}", result.final_text);
            if let Some(report) = check {
                eprintln!("\n--- Final diagnostics ---\n{}", report.render());
            }
        }
        AgentOutputFormat::Json => println!("{}", serde_json::to_string_pretty(&output)?),
        AgentOutputFormat::Jsonl => println!(
            "{}",
            serde_json::to_string(&json!({
                "schema_version": OUTPUT_SCHEMA_VERSION,
                "type": "result",
                "result": output,
            }))?
        ),
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct DiagnosticsReport {
    schema_version: u32,
    cwd: String,
    profile: String,
    configured: bool,
    passed: bool,
    threshold: &'static str,
    summary: DiagnosticSummary,
    diagnostics: Vec<Diagnostic>,
    checker_errors: Vec<String>,
}

#[derive(Debug, Default, Serialize)]
struct DiagnosticSummary {
    errors: usize,
    warnings: usize,
    lints: usize,
    info: usize,
}

impl DiagnosticsReport {
    fn from_snapshot(threshold: DiagnosticThreshold, snapshot: Option<Snapshot>) -> Self {
        let configured = snapshot.is_some();
        let Snapshot {
            mut diagnostics,
            mut errors,
        } = snapshot.unwrap_or_else(|| Snapshot {
            diagnostics: Vec::new(),
            errors: vec![format!(
                "no diagnostics profile at {}",
                crate::diagnostics::PROFILE_PATH
            )],
        });
        diagnostics.sort_by(|a, b| {
            a.severity
                .cmp(&b.severity)
                .then_with(|| a.file.cmp(&b.file))
                .then_with(|| a.line.cmp(&b.line))
                .then_with(|| a.col.cmp(&b.col))
                .then_with(|| a.message.cmp(&b.message))
        });
        errors.sort();

        let summary = DiagnosticSummary {
            errors: count_severity(&diagnostics, Severity::Error),
            warnings: count_severity(&diagnostics, Severity::Warning),
            lints: count_severity(&diagnostics, Severity::Lint),
            info: count_severity(&diagnostics, Severity::Info),
        };
        let passed = configured
            && errors.is_empty()
            && !diagnostics
                .iter()
                .any(|diagnostic| threshold.matches(diagnostic.severity));

        Self {
            schema_version: OUTPUT_SCHEMA_VERSION,
            cwd: current_dir().to_string_lossy().into_owned(),
            profile: crate::diagnostics::PROFILE_PATH.to_string(),
            configured,
            passed,
            threshold: threshold.label(),
            summary,
            diagnostics,
            checker_errors: errors,
        }
    }

    fn render(&self) -> String {
        if !self.configured {
            return format!("No diagnostics profile found at {} (failed).", self.profile);
        }
        let snapshot = Snapshot {
            diagnostics: self.diagnostics.clone(),
            errors: self.checker_errors.clone(),
        };
        format!(
            "{}\nResult: {} (fail on {}).",
            snapshot.render(),
            if self.passed { "passed" } else { "failed" },
            self.threshold
        )
    }
}

async fn collect_report(
    threshold: DiagnosticThreshold,
    cancel: &CancellationToken,
) -> DiagnosticsReport {
    let snapshot = crate::diagnostics::collect(&current_dir(), cancel).await;
    DiagnosticsReport::from_snapshot(threshold, snapshot)
}

fn count_severity(diagnostics: &[Diagnostic], severity: Severity) -> usize {
    diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == severity)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diagnostic(severity: Severity) -> Diagnostic {
        Diagnostic {
            file: "src/main.rs".to_string(),
            line: 7,
            col: Some(3),
            severity,
            code: Some("E1".to_string()),
            message: "problem".to_string(),
        }
    }

    #[test]
    fn thresholds_include_more_severe_findings() {
        assert!(DiagnosticThreshold::Error.matches(Severity::Error));
        assert!(!DiagnosticThreshold::Error.matches(Severity::Warning));
        assert!(DiagnosticThreshold::Warning.matches(Severity::Error));
        assert!(DiagnosticThreshold::Warning.matches(Severity::Warning));
        assert!(!DiagnosticThreshold::Warning.matches(Severity::Lint));
        assert!(DiagnosticThreshold::Lint.matches(Severity::Lint));
        assert!(!DiagnosticThreshold::Lint.matches(Severity::Info));
    }

    #[test]
    fn report_fails_on_threshold_and_checker_errors() {
        let warning = DiagnosticsReport::from_snapshot(
            DiagnosticThreshold::Error,
            Some(Snapshot {
                diagnostics: vec![diagnostic(Severity::Warning)],
                errors: Vec::new(),
            }),
        );
        assert!(warning.passed);

        let checker_error = DiagnosticsReport::from_snapshot(
            DiagnosticThreshold::Error,
            Some(Snapshot {
                diagnostics: Vec::new(),
                errors: vec!["checker failed".to_string()],
            }),
        );
        assert!(!checker_error.passed);
    }

    #[test]
    fn missing_profile_is_not_clean() {
        let report = DiagnosticsReport::from_snapshot(DiagnosticThreshold::Error, None);
        assert!(!report.configured);
        assert!(!report.passed);
    }

    #[test]
    fn cli_surface_exposes_the_loaded_skill_prompt() {
        let surface = CliSurface {
            jsonl: false,
            plan: false,
            skill_prompt: Some("skill instructions".to_string()),
            approval_label: "test".to_string(),
        };

        assert_eq!(
            surface.skill_prompt().as_deref(),
            Some("skill instructions")
        );
    }

    #[test]
    fn report_json_has_a_versioned_stable_shape() {
        let report = DiagnosticsReport::from_snapshot(
            DiagnosticThreshold::Warning,
            Some(Snapshot {
                diagnostics: vec![diagnostic(Severity::Error)],
                errors: Vec::new(),
            }),
        );
        let value = serde_json::to_value(report).unwrap();
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["threshold"], "warning");
        assert_eq!(value["summary"]["errors"], 1);
        assert_eq!(value["diagnostics"][0]["severity"], "error");
    }
}
