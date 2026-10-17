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
    AgentSurface, DiagnosticsState, ReviewDecision, RunnerEvent, RunnerPhase, TurnResult,
    TurnRunner,
};
use crate::tools::provider::{AgentToolProvider, LocalToolProvider, ToolProvider};

use crate::runner::assembly::{AgentSpec, PolicySpec, resolve_memory_model};

#[path = "headless_compaction.rs"]
mod compaction;

#[path = "headless_pending.rs"]
mod pending;
use crate::headless_interaction as interaction;

const OUTPUT_SCHEMA_VERSION: u32 = 1;

/// Connect the configured MCP servers for one headless run. Startup failures
/// go to stderr so stdout keeps its text/JSON/JSONL contract; a server that
/// fails is simply absent, matching the TUI. Child processes are killed when
/// the last `Arc` is dropped at the end of the run.
async fn connect_mcp(
    configs: &[crate::mcp::types::McpServerConfig],
) -> Option<Arc<crate::mcp::McpManager>> {
    if configs.is_empty() {
        return None;
    }
    let manager = crate::mcp::McpManager::from_config_with_updates(configs, ".", |_, _| {}).await;
    for error in &manager.startup_errors {
        eprintln!("warning: {error}");
    }
    Some(Arc::new(manager))
}

/// Stable process outcomes; values are an external scripting contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeadlessOutcome {
    Completed,
    Error,
    Usage,
    CheckFailed,
    Timeout,
    MaxSteps,
    #[allow(dead_code)] // Reserved for resumable interactive suspension.
    NeedsInput,
    Interrupted,
}

impl HeadlessOutcome {
    pub(crate) fn exit_code(self) -> i32 {
        match self {
            Self::Completed => 0,
            Self::Error => 1,
            Self::Usage => 2,
            Self::CheckFailed => 3,
            Self::Timeout => 4,
            Self::MaxSteps => 5,
            Self::NeedsInput => 10,
            Self::Interrupted => 130,
        }
    }

    fn stop_reason(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::CheckFailed => "check_failed",
            Self::Timeout => "timeout",
            Self::MaxSteps => "max_steps",
            Self::Interrupted => "interrupted",
            Self::NeedsInput => "input_required",
            Self::Error | Self::Usage => "error",
        }
    }

    pub(crate) fn from_error(error: &color_eyre::Report) -> Self {
        if error
            .downcast_ref::<crate::headless_security::UsageError>()
            .is_some()
        {
            return Self::Usage;
        }
        match error.downcast_ref::<crate::runner::RunnerError>() {
            Some(crate::runner::RunnerError::StepLimit { .. }) => Self::MaxSteps,
            Some(crate::runner::RunnerError::Cancelled) => Self::Interrupted,
            _ => Self::Error,
        }
    }
}

#[derive(Default)]
struct Progress {
    text: String,
    streaming: String,
    usage: (u32, u32, u32),
    compactions: usize,
    completed_steps: usize,
    completed_tool_calls: usize,
}

pub(crate) async fn run(args: RunArgs) -> color_eyre::Result<HeadlessOutcome> {
    let session_options = SessionOptions {
        suspend: args.suspend || args.agent.work_mode == Some(WorkMode::Manual),
        answer: args.answer,
        answer_file: args.answer_file,
        answer_id: args.answer_id,
        resume: args.resume,
        continue_session: args.continue_session,
        fork: args.fork,
        no_save: args.no_save,
        save_session: args.save_session,
    };
    let inputs = RunInputs {
        prompt: args.prompt,
        prompt_file: args.prompt_file,
        append_prompt: args.append_prompt,
        append_prompt_file: args.append_prompt_file,
        images: args.image,
    };
    execute(
        args.agent,
        args.format,
        Some(inputs),
        args.init,
        args.check.then_some(args.fail_on),
        session_options,
    )
    .await
}

pub(crate) async fn init(args: InitArgs) -> color_eyre::Result<HeadlessOutcome> {
    execute(
        args.agent,
        args.format,
        None,
        true,
        None,
        SessionOptions::default(),
    )
    .await
}

#[derive(Default)]
struct SessionOptions {
    suspend: bool,
    answer: Option<String>,
    answer_file: Option<PathBuf>,
    answer_id: Option<String>,
    resume: Option<String>,
    continue_session: bool,
    fork: bool,
    no_save: bool,
    save_session: bool,
}

#[derive(Default)]
struct RunInputs {
    prompt: Option<String>,
    prompt_file: Option<PathBuf>,
    append_prompt: Vec<String>,
    append_prompt_file: Vec<PathBuf>,
    images: Vec<PathBuf>,
}

async fn execute(
    mut args: AgentArgs,
    format: AgentOutputFormat,
    prompt: Option<RunInputs>,
    initialize: bool,
    check: Option<DiagnosticThreshold>,
    session_options: SessionOptions,
) -> color_eyre::Result<HeadlessOutcome> {
    let started = std::time::Instant::now();
    set_working_directory(args.cwd.as_deref())?;
    let cancel = cancellation_token();
    let progress = Arc::new(Mutex::new(Progress::default()));
    let mut agent = None;
    let mut report = None;
    let mut metadata = None;
    let mut pending_output: Option<pending::Pending> = None;
    let session = Arc::new(Mutex::new(None::<crate::headless_session::HeadlessSession>));
    let timeout = args.timeout;
    let operation = async {
        let (mut config, _) = crate::load_config_from(args.config.as_deref())?;
        if session_options.suspend
            || session_options.save_session
            || session_options.resume.is_some()
            || session_options.continue_session
        {
            let manager = crate::session::SessionManager::new()
                .ok_or_else(|| color_eyre::eyre::eyre!("session persistence unavailable"))?;
            *session.lock().unwrap() = crate::headless_session::HeadlessSession::open(
                manager,
                session_options.resume.as_deref(),
                session_options.continue_session,
                session_options.fork,
                session_options.no_save,
                session_options.save_session || session_options.suspend,
            )?;
            if let Some(saved) = session.lock().unwrap().as_ref() {
                saved.inherit_args(&mut args, &mut config);
            }
        }
        let stored_pending = session
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|s| s.snapshot.headless_pending.clone())
            .map(serde_json::from_value::<pending::Pending>)
            .transpose()?;
        pending_output = stored_pending.clone();
        let saved_dir = session
            .lock()
            .unwrap()
            .as_ref()
            .filter(|s| s.restored())
            .map(|s| PathBuf::from(&s.snapshot.working_dir));
        if let Some(saved_dir) = saved_dir
            && !same_dir(&saved_dir, &current_dir())
        {
            // Answering re-runs the pending call with its original, possibly
            // relative, arguments, so it must happen in the original workspace.
            if stored_pending.is_some() {
                return Err(crate::headless_security::UsageError(format!(
                    "this session is waiting for input in {}; rerun with --cwd {}",
                    saved_dir.display(),
                    saved_dir.display()
                ))
                .into());
            }
            eprintln!(
                "warning: resuming a session from {} in a different directory ({})",
                saved_dir.display(),
                current_dir().display()
            );
        }
        let answer = match &session_options.answer_file {
            Some(path) => Some(std::fs::read_to_string(path)?.trim_end().to_string()),
            None => session_options.answer.clone(),
        };
        validate_answer(
            stored_pending.as_ref(),
            answer.as_deref(),
            session_options.answer_id.as_deref(),
            config.allow_yolo,
        )?;
        let suspend = session_options.suspend
            || stored_pending.is_some()
            || args.work_mode == Some(WorkMode::Manual);
        if suspend && session_options.no_save {
            return Err(crate::headless_security::UsageError(
                "suspension conflicts with --no-save".into(),
            )
            .into());
        }
        if let (Some(pending), Some(answer)) = (&stored_pending, &answer)
            && pending.kind == "plan_review"
        {
            match interaction::parse_plan_answer(answer, config.allow_yolo)
                .map_err(|e| crate::headless_security::UsageError(e.into()))?
            {
                interaction::PlanAnswer::Manual => args.work_mode = Some(WorkMode::Manual),
                interaction::PlanAnswer::Auto => args.work_mode = Some(WorkMode::Auto),
                interaction::PlanAnswer::Yolo => args.work_mode = Some(WorkMode::Yolo),
                interaction::PlanAnswer::Revise => args.work_mode = Some(WorkMode::Plan),
            }
        }
        if answer.is_none()
            && prompt
                .as_ref()
                .is_some_and(|input| input.prompt.is_none() && input.prompt_file.is_none())
        {
            return Err(crate::headless_security::UsageError(
                "a prompt is required when there is no pending answer".into(),
            )
            .into());
        }
        apply_overrides(&mut config, &args)?;
        let inputs = prepare_inputs(prompt, config.vision_enabled).await?;
        if let Some(saved) = session.lock().unwrap().as_mut()
            && let Some(model) = &args.classifier_model
        {
            saved.snapshot.classifier_model_override =
                crate::session::ModelOverride::Model(model.clone());
        }
        let resolved =
            crate::headless_security::resolve_security(&config, &args.security, &current_dir())?;
        metadata = Some(crate::headless_security::security_metadata(
            &config,
            &args.security,
            &resolved,
        ));
        agent = Some(
            HeadlessAgent::build(
                &args,
                config,
                cancel.clone(),
                progress.clone(),
                format,
                metadata.as_ref(),
                session.clone(),
                suspend,
            )
            .await?,
        );
        let agent = agent.as_ref().expect("agent was built");
        if !args.no_diagnostics {
            agent.seed_diagnostics_baseline().await;
        }
        if let (Some(pending), Some(answer)) = (stored_pending, answer) {
            agent.resume(pending, answer, format).await?;
            pending_output = None;
            if agent.pending.lock().unwrap().is_some() {
                return Ok(HeadlessOutcome::NeedsInput);
            }
        }
        if initialize {
            agent
                .run_turn(
                    agent.initialization_prompt.clone(),
                    InputRole::Developer,
                    if inputs.is_some() {
                        AgentOutputFormat::Text
                    } else {
                        format
                    },
                )
                .await?;
            if inputs.is_some() && !args.no_diagnostics {
                agent.refresh_diagnostics_baseline().await;
            }
        }
        if let Some((instructions, message)) = inputs {
            {
                let mut conversation = agent.conversation.lock().unwrap();
                for instruction in instructions {
                    conversation
                        .add_input_message(input_message(instruction, InputRole::Developer));
                }
            }
            agent.run_message(message, format).await?;
        }
        if agent.pending.lock().unwrap().is_some() {
            return Ok(HeadlessOutcome::NeedsInput);
        }
        if let Some(threshold) = check {
            report = Some(collect_report(threshold, &cancel).await);
        }
        Ok(if report.as_ref().is_none_or(|r| r.passed) {
            HeadlessOutcome::Completed
        } else {
            HeadlessOutcome::CheckFailed
        })
    };
    let mut result = run_with_timeout(operation, timeout, &cancel).await;
    if let Some(agent) = &agent {
        pending_output = agent.pending.lock().unwrap().clone();
        if let Err(error) = agent.save_session() {
            eprintln!("cannot save session: {error}");
            result = Err(error);
        }
        agent.queue_for_dream();
    }
    crate::diagnostics::shutdown_lsp().await;
    if pending_output.is_some()
        && result.as_ref().err().is_some_and(|error| {
            matches!(
                error.downcast_ref::<crate::runner::RunnerError>(),
                Some(crate::runner::RunnerError::InputRequired { .. })
            )
        })
    {
        result = Ok(HeadlessOutcome::NeedsInput);
    }
    if pending_output.is_some() && result.is_ok() {
        result = Ok(HeadlessOutcome::NeedsInput);
    }
    let outcome = match &result {
        Ok(outcome) => *outcome,
        Err(error) => HeadlessOutcome::from_error(error),
    };
    if matches!(
        outcome,
        HeadlessOutcome::Timeout | HeadlessOutcome::Interrupted
    ) {
        eprintln!("headless operation stopped: {}", outcome.stop_reason());
    }
    let model = agent
        .as_ref()
        .map(|a| a.model.as_str())
        .or(args.model.as_deref())
        .unwrap_or("");
    let mut progress = progress.lock().unwrap();
    if let Some(agent) = &agent {
        progress.usage = agent.conversation.lock().unwrap().accumulated_usage;
    }
    let (active_steps, active_tool_calls) = agent
        .as_ref()
        .map(|a| a.runner.metrics.snapshot())
        .unwrap_or_default();
    emit_agent_result(
        format,
        model,
        TurnResult {
            final_text: progress.text.clone(),
            usage: progress.usage,
            steps: progress.completed_steps + active_steps,
            tool_calls: progress.completed_tool_calls + active_tool_calls,
        },
        report.as_ref(),
        outcome,
        args.work_mode.unwrap_or(WorkMode::Auto),
        metadata.as_ref(),
        started.elapsed().as_millis(),
        progress.compactions,
        session
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|s| s.session_id())
            .map(str::to_owned)
            .as_deref(),
        pending_output.as_ref(),
    )?;
    result
}

pub(crate) async fn diagnostics(args: DiagnosticsArgs) -> color_eyre::Result<HeadlessOutcome> {
    set_working_directory(args.cwd.as_deref())?;
    let (config, _) = crate::load_config_from(args.config.as_deref())?;
    crate::install_security_with_overrides(&config, &args.security)?;
    let cancel = cancellation_token();
    let report = collect_report(args.fail_on, &cancel).await;
    match args.format {
        DiagnosticsOutputFormat::Text => println!("{}", report.render()),
        DiagnosticsOutputFormat::Json => println!("{}", serde_json::to_string_pretty(&report)?),
    }
    crate::diagnostics::shutdown_lsp().await;
    Ok(if cancel.is_cancelled() {
        HeadlessOutcome::Interrupted
    } else if report.passed {
        HeadlessOutcome::Completed
    } else {
        HeadlessOutcome::CheckFailed
    })
}

fn mode_label(mode: WorkMode) -> String {
    mode.label().to_ascii_lowercase()
}

struct HeadlessAgent {
    compactor: Arc<compaction::Compactor>,
    runner: TurnRunner,
    progress: Arc<Mutex<Progress>>,
    conversation: Arc<Mutex<Conversation>>,
    diagnostics_state: Arc<Mutex<DiagnosticsState>>,
    cancel: CancellationToken,
    model: String,
    mode: WorkMode,
    skill_prompt: Option<String>,
    initialization_prompt: String,
    agents: crate::agents::AgentManager,
    /// Identity used when queueing this run for background consolidation.
    dream_session_id: String,
    memory_enabled: bool,
    session: Arc<Mutex<Option<crate::headless_session::HeadlessSession>>>,
    todos: Arc<Mutex<crate::todos::TodoList>>,
    tasks: crate::tasks::TaskManager,
    security: Arc<crate::security::SecurityHandle>,
    activated_skills: Vec<String>,
    suspend: bool,
    pending: Mutex<Option<pending::Pending>>,
}

impl Drop for HeadlessAgent {
    fn drop(&mut self) {
        self.agents.cancel_all();
    }
}

impl HeadlessAgent {
    #[allow(clippy::too_many_arguments)]
    async fn build(
        args: &AgentArgs,
        config: crate::config::programmer_config::ProgrammerConfig,
        cancel: CancellationToken,
        progress: Arc<Mutex<Progress>>,
        format: AgentOutputFormat,
        metadata: Option<&crate::headless_security::SecurityMetadata>,
        session: Arc<Mutex<Option<crate::headless_session::HeadlessSession>>>,
        suspend: bool,
    ) -> color_eyre::Result<Self> {
        let security = crate::install_security_with_overrides(&config, &args.security)?;
        let saved = session.lock().unwrap().as_ref().map(|s| s.snapshot.clone());
        let session_id = session
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|s| s.session_id())
            .map(str::to_owned);
        if let Some(saved) = &saved {
            security.restore_snapshots(&saved.file_snapshots);
        }

        let provider_manager = crate::providers::ProviderManager::new(&config).await;
        let model = args
            .model
            .clone()
            .unwrap_or_else(|| provider_manager.default_model());
        if format == AgentOutputFormat::Jsonl {
            println!(
                "{}",
                json!({"schema_version": OUTPUT_SCHEMA_VERSION, "type": "start",
                "model": model, "work_mode": mode_label(args.work_mode.unwrap_or(WorkMode::Auto)), "security": metadata, "session_id": session_id})
            );
        }
        let (client, model_name) = provider_manager
            .resolve(&model)
            .map(|(client, name)| (client.clone(), name))
            .ok_or_else(|| color_eyre::eyre::eyre!("unknown provider/model: {model}"))?;

        let todo_store = Arc::new(Mutex::new(crate::todos::TodoList {
            todos: saved.as_ref().map(|s| s.todos.clone()).unwrap_or_default(),
        }));
        let security = Arc::new(crate::security::SecurityHandle::new(security));
        let mut skill_registry = crate::skills::SkillRegistry::load();
        if let Some(saved) = &saved
            && (saved.skill_selection_saved || !saved.activated_skills.is_empty())
        {
            skill_registry.set_activated(&saved.activated_skills);
        }
        let activated_skills = skill_registry.activated_names().to_vec();
        let skill_prompt = skill_registry.catalog_prompt();
        let initialization_prompt = skill_registry
            .prompt(crate::skills::INITIALIZE_PROJECT_SKILL)
            .ok_or_else(|| {
                color_eyre::eyre::eyre!("built-in initialize-project skill is unavailable")
            })?;
        let memory_model = resolve_memory_model(
            &provider_manager,
            config.memory.enabled,
            config.memory_model.as_deref(),
            &model,
        );
        let conversation = Arc::new(Mutex::new(Conversation::new()));
        if let Some(saved) = &saved {
            let mut conv = conversation.lock().unwrap();
            conv.restore_items(crate::session::SessionManager::into_items(saved.clone()));
            conv.last_request_input_tokens = saved.last_request_input_tokens;
        }
        let diagnostics_state = Arc::new(Mutex::new(DiagnosticsState::default()));
        let tasks = crate::tasks::TaskManager::default();
        if let Some(saved) = &saved {
            tasks.restore(&saved.tasks);
        }
        let local = LocalToolProvider::new(todo_store.clone(), security.clone())
            .with_diagnostics_state(diagnostics_state.clone())
            .with_tasks(tasks.clone())
            .with_memory_enabled(config.memory.enabled)
            .with_memory_context(Some(conversation.clone()));
        let classifier_model = args
            .classifier_model
            .as_deref()
            .or(config.classifier_model.as_deref())
            .unwrap_or(&model);
        let policy = PolicySpec::resolve(
            args.work_mode.unwrap_or(WorkMode::Auto),
            &provider_manager,
            classifier_model,
            config.classifier_top_logprobs,
            Arc::new(Mutex::new(HashSet::new())),
        )
        .ok_or_else(|| {
            color_eyre::eyre::eyre!("unknown classifier provider/model: {classifier_model}")
        })?;
        // Connect only after the mode and classifier are validated, so a
        // rejected run never spawns MCP server processes.
        let mcp_manager = connect_mcp(&config.mcp_servers).await;
        let mut base_providers: Vec<Arc<dyn ToolProvider>> = Vec::new();

        let tool_filter = crate::runner::ToolFilter {
            allowed: args.allowed_tools.clone(),
            disallowed: args.disallowed_tools.clone(),
        };
        let agents = crate::agents::AgentManager::default();
        if let Some(saved) = &saved {
            agents.restore(&saved.agents);
        }
        let child_runtime = crate::agents::AgentRuntime {
            tasks: tasks.clone(),
            events: tokio::sync::mpsc::unbounded_channel().0,
            provider_manager: Arc::new(provider_manager.clone()),
            client: client.clone(),
            model_name: model_name.clone(),
            model_str: model.clone(),
            todos: todo_store.clone(),
            security: security.clone(),
            mcp_manager: mcp_manager.clone(),
            policy: policy.clone(),
            soul: config.soul.clone(),
            coauthor: config.git_coauthor.clone(),
            vision_enabled: config.vision_enabled,
            thinking_level: args.thinking.unwrap_or_default(),
            memory_config: config.memory.clone(),
            memory_model: config.memory_model.clone(),
            skill_registry: skill_registry.clone(),
            skill_prompt: skill_prompt.clone(),
            approval_label: format!(
                "{} approved by {} mode (headless sub-agent)",
                args.work_mode.unwrap_or(WorkMode::Auto).icon(),
                args.work_mode.unwrap_or(WorkMode::Auto).label()
            ),
            checkpoint: None,
            conversation_history: None,
            tool_filter: tool_filter.clone(),
        };
        base_providers.push(Arc::new(AgentToolProvider::new(
            agents.clone(),
            child_runtime,
        )));

        let hooks: Vec<Arc<dyn crate::runner::hooks::TurnHook>> = if args.no_diagnostics {
            Vec::new()
        } else {
            crate::runner::hooks::standard_hooks(diagnostics_state.clone())
        };

        let compactor = Arc::new(compaction::Compactor::new(
            &config,
            &model,
            &provider_manager,
            args.thinking.unwrap_or_default(),
        ));
        let mut runner = AgentSpec {
            client,
            model_name,
            model_str: model.clone(),
            local,
            skills: skill_registry,
            mcp: mcp_manager,
            extra_providers: base_providers,
            policy,
            soul: config.soul,
            coauthor: config.git_coauthor,
            vision_enabled: config.vision_enabled,
            thinking_level: args.thinking.unwrap_or_default(),
            memory_model,
            hooks,
            stream_retrying: Arc::new(AtomicBool::new(false)),
            max_steps: args.max_steps,
        }
        .build();
        for pattern in runner.set_tool_filter(tool_filter) {
            eprintln!("warning: tool pattern '{pattern}' matches no available tools");
        }

        Ok(Self {
            suspend,
            pending: Mutex::new(
                saved
                    .as_ref()
                    .and_then(|s| s.headless_pending.clone())
                    .map(serde_json::from_value)
                    .transpose()?,
            ),
            compactor,
            progress,
            runner,
            memory_enabled: config.memory.enabled,
            dream_session_id: session_id
                .unwrap_or_else(|| format!("headless-{}", uuid::Uuid::new_v4().simple())),
            session,
            todos: todo_store,
            tasks,
            security,
            activated_skills,
            conversation,
            diagnostics_state,
            cancel,
            model,
            mode: args.work_mode.unwrap_or(WorkMode::Auto),
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
        self.run_message(input_message(text, role), format).await
    }

    async fn run_message(
        &self,
        message: ApiMessageItem,
        format: AgentOutputFormat,
    ) -> color_eyre::Result<TurnResult> {
        // Preserve earlier initialization-turn counts before the runner resets
        // its current-turn counters; the active counters remain readable on cancellation.
        let (steps, tool_calls) = self.runner.metrics.snapshot();
        {
            let mut progress = self.progress.lock().unwrap();
            progress.completed_steps += steps;
            progress.completed_tool_calls += tool_calls;
        }
        if let ApiMessageItem::Input(input) = &message
            && let Some(text) = input.content.iter().find_map(|content| match content {
                InputContent::InputText(text) => Some(text.text.clone()),
                _ => None,
            })
            && let Some(saved) = self.session.lock().unwrap().as_mut()
        {
            if saved.snapshot.title.is_empty() {
                saved.snapshot.title = crate::session::truncate_first_line(&text, 80);
                saved.snapshot.first_message = saved.snapshot.title.clone();
            }
            saved.snapshot.history.push(text);
        }
        self.conversation.lock().unwrap().add_input_message(message);
        self.drive(None, format).await
    }

    async fn drive(
        &self,
        resume: Option<(crate::runner::RunnerCheckpoint, String)>,
        format: AgentOutputFormat,
    ) -> color_eyre::Result<TurnResult> {
        let surface = CliSurface {
            suspend: self.suspend,
            compaction: Some((
                self.compactor.clone(),
                self.conversation.clone(),
                self.cancel.clone(),
            )),
            progress: self.progress.clone(),
            jsonl: format == AgentOutputFormat::Jsonl,
            plan: self.mode == WorkMode::Plan,
            skill_prompt: self.skill_prompt.clone(),
            approval_label: format!(
                "{} approved by {} mode (headless)",
                self.mode.icon(),
                self.mode.label()
            ),
        };
        let result = if let Some((checkpoint, answer)) = resume {
            self.runner
                .run_turn_from(
                    &self.conversation,
                    &self.cancel,
                    &surface,
                    checkpoint,
                    answer,
                )
                .await
        } else {
            self.runner
                .run_turn(&self.conversation, &self.cancel, &surface)
                .await
        };
        if let Err(crate::runner::RunnerError::InputRequired { checkpoint }) = &result {
            *self.pending.lock().unwrap() = Some(pending::Pending::tool(
                (**checkpoint).clone(),
                self.dream_session_id.clone(),
            ));
        } else if let Ok(result) = &result
            && self.suspend
            && self.mode == WorkMode::Plan
        {
            *self.pending.lock().unwrap() = Some(pending::Pending::plan(
                result.final_text.clone(),
                self.dream_session_id.clone(),
            ));
        }
        self.progress.lock().unwrap().usage = self.conversation.lock().unwrap().accumulated_usage;
        if let Ok(result) = &result {
            self.progress.lock().unwrap().text = result.final_text.clone();
        }
        self.save_session()?;
        result.map_err(Into::into)
    }

    fn save_session(&self) -> color_eyre::Result<()> {
        let mut session = self.session.lock().unwrap();
        let Some(session) = session.as_mut() else {
            return Ok(());
        };
        if session.session_id().is_none() {
            return Ok(());
        }
        let pending = self.pending.lock().unwrap();
        session.snapshot.headless_pending =
            pending.as_ref().map(serde_json::to_value).transpose()?;
        if pending.is_none() {
            crate::runner::ensure_tool_output_pairing(&self.conversation);
        }
        let conversation = self.conversation.lock().unwrap();
        let items: Vec<_> = conversation.items().cloned().collect();
        let snapshot = &mut session.snapshot;
        if snapshot.first_message.is_empty()
            && let Some(text) = crate::app::helpers::first_user_text(&items)
        {
            snapshot.first_message = crate::session::truncate_first_line(&text, 80);
            snapshot.title = snapshot.first_message.clone();
        }
        snapshot.last_request_input_tokens = conversation.usage_summary().last_request_input_tokens;
        crate::session::SessionManager::set_items(snapshot, items);
        snapshot.current_model = Some(self.model.clone());
        snapshot.work_mode = Some(self.mode);
        snapshot.thinking_level = self.runner.thinking_level;
        snapshot.vision_enabled = self.runner.vision_enabled;
        snapshot.todos = self.todos.lock().unwrap().todos.clone();
        snapshot.tasks = self.tasks.persist_all();
        snapshot.agents = self.agents.persist_all();
        snapshot.activated_skills = self.activated_skills.clone();
        snapshot.skill_selection_saved = true;
        snapshot.file_snapshots = self.security.snapshot().persisted_snapshots();
        session.save()
    }

    /// Queue this headless run's user/assistant prose for background
    /// consolidation. A run is a complete session, so it is queued exactly like
    /// an interactive one; the same content-hash dedup makes a repeated
    /// identical run a no-op.
    fn queue_for_dream(&self) {
        if !self.memory_enabled {
            return;
        }
        let items = self
            .conversation
            .lock()
            .unwrap()
            .items()
            .cloned()
            .collect::<Vec<_>>();
        let transcript = crate::app::dream_transcript(&items);
        if transcript.trim().is_empty() {
            return;
        }
        if let Ok(manager) = crate::memory::MemoryManager::for_current_dir() {
            let _ = manager.enqueue_dream(&self.dream_session_id, &transcript);
        }
    }

    async fn resume(
        &self,
        pending: pending::Pending,
        answer: String,
        format: AgentOutputFormat,
    ) -> color_eyre::Result<TurnResult> {
        *self.pending.lock().unwrap() = None;
        if let Some(checkpoint) = pending.checkpoint {
            let answer = normalize_answer(&checkpoint.pending, &answer);
            self.drive(Some((checkpoint, answer)), format).await
        } else {
            if let Some(feedback) = answer.strip_prefix("revise:") {
                return self
                    .run_turn(feedback.trim().to_string(), InputRole::User, format)
                    .await;
            }
            let instruction = {
                match self.mode {
                    WorkMode::Manual => crate::prompts::PLAN_APPROVED_MANUAL_PROMPT,
                    WorkMode::Yolo => crate::prompts::PLAN_APPROVED_YOLO_PROMPT,
                    _ => crate::prompts::PLAN_APPROVED_AUTO_PROMPT,
                }
                .to_string()
            };
            self.run_turn(instruction, InputRole::Developer, format)
                .await
        }
    }

    async fn seed_diagnostics_baseline(&self) {
        if self.diagnostics_state.lock().unwrap().baseline.is_some() {
            return;
        }
        self.refresh_diagnostics_baseline().await;
    }

    async fn refresh_diagnostics_baseline(&self) {
        let cwd = current_dir();
        let generation = self.diagnostics_state.lock().unwrap().begin_update();
        let snapshot = self
            .cancel
            .wait_or(crate::diagnostics::collect(&cwd, &self.cancel))
            .await
            .flatten();
        self.diagnostics_state
            .lock()
            .unwrap()
            .publish(generation, snapshot.as_ref());
    }
}

struct CliSurface {
    suspend: bool,
    compaction: Option<(
        Arc<compaction::Compactor>,
        Arc<Mutex<Conversation>>,
        CancellationToken,
    )>,
    progress: Arc<Mutex<Progress>>,
    jsonl: bool,
    plan: bool,
    skill_prompt: Option<String>,
    approval_label: String,
}

#[async_trait::async_trait]
impl AgentSurface for CliSurface {
    fn suspend_enabled(&self) -> bool {
        self.suspend
    }
    fn on_event(&self, event: RunnerEvent<'_>) {
        {
            let mut progress = self.progress.lock().unwrap();
            match &event {
                RunnerEvent::Phase(RunnerPhase::Streaming) => progress.streaming.clear(),
                RunnerEvent::StreamChunk(chunk) => {
                    if let async_openai::types::responses::ResponseStreamEvent::ResponseOutputTextDelta(delta) = chunk.as_ref() {
                        progress.streaming.push_str(&delta.delta);
                        progress.text = progress.streaming.clone();
                    }
                }
                RunnerEvent::Assistant(text) => progress.text = text.to_string(),
                _ => {}
            }
        }
        if !self.jsonl {
            return;
        }
        let event = match event {
            RunnerEvent::StreamChunk(_) | RunnerEvent::Activity(_) => return,
            RunnerEvent::ResponseAborted { .. } => return,
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
            RunnerEvent::Notice(text) => json!({
                "schema_version": OUTPUT_SCHEMA_VERSION,
                "type": "notice",
                "text": text,
            }),
            RunnerEvent::UsageSafePoint { .. } | RunnerEvent::WaitingSubagents(_) => return,
        };
        println!("{event}");
    }

    async fn usage_safe_point(&self, input_tokens: u32) -> Result<(), crate::runner::RunnerError> {
        if let Some((compactor, conversation, cancel)) = &self.compaction
            && compactor
                .run(input_tokens, conversation, cancel, self.jsonl)
                .await?
        {
            self.progress.lock().unwrap().compactions += 1;
        }
        Ok(())
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
        RunnerPhase::Associating => "associating_memories",
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

fn apply_overrides(
    config: &mut crate::config::programmer_config::ProgrammerConfig,
    args: &AgentArgs,
) -> color_eyre::Result<()> {
    if args.no_compact {
        config.auto_compact_tokens = 0;
        config.mandatory_compact_tokens = 0;
    } else {
        if let Some(tokens) = args.compact_tokens {
            config.auto_compact_tokens = tokens;
        }
        if let Some(tokens) = args.mandatory_compact_tokens {
            config.mandatory_compact_tokens = tokens;
        }
    }
    if args.vision || args.no_vision {
        config.vision_enabled = args.vision;
    }
    if args.no_memory {
        config.memory.enabled = false;
    }
    for name in &args.mcp {
        if !config.mcp_servers.iter().any(|server| &server.name == name) {
            return Err(crate::headless_security::UsageError(format!(
                "unknown MCP server '{name}'; available servers: {}",
                config
                    .mcp_servers
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
            .into());
        }
    }
    if args.no_mcp {
        config.mcp_servers.clear();
    } else if !args.mcp.is_empty() {
        config
            .mcp_servers
            .retain(|server| args.mcp.contains(&server.name));
    }
    if args.no_coauthor {
        config.git_coauthor = None;
    } else if let Some(coauthor) = &args.coauthor {
        config.git_coauthor = Some(coauthor.clone());
    }
    if let Some(path) = &args.soul_file {
        config.soul = Some(std::fs::read_to_string(path)?);
    }
    Ok(())
}

async fn prepare_inputs(
    inputs: Option<RunInputs>,
    vision_enabled: bool,
) -> color_eyre::Result<Option<(Vec<String>, ApiMessageItem)>> {
    let Some(inputs) = inputs else {
        return Ok(None);
    };
    if inputs.prompt.is_none() && inputs.prompt_file.is_none() {
        return Ok(None);
    }
    if !vision_enabled && !inputs.images.is_empty() {
        return Err(crate::headless_security::UsageError(
            "--image requires vision; enable it with --vision".into(),
        )
        .into());
    }
    let mut instructions = inputs.append_prompt;
    for path in inputs.append_prompt_file {
        instructions.push(std::fs::read_to_string(path)?);
    }
    let mut images = Vec::new();
    for path in inputs.images {
        let metadata = std::fs::metadata(&path)?;
        if metadata.len() > crate::commands::MAX_IMAGE_BYTES {
            return Err(color_eyre::eyre::eyre!(
                "image {} exceeds size limit",
                path.display()
            ));
        }
        let bytes = std::fs::read(&path)?;
        images.push(
            crate::commands::image_content_from_bytes(&bytes)
                .map_err(color_eyre::eyre::Report::msg)?,
        );
    }
    let dropped = crate::commands::limit_image_attachments(&mut images);
    if dropped > 0 {
        eprintln!("warning: dropped {dropped} image attachments exceeding per-message limits");
    }
    let text = read_prompt(inputs.prompt, inputs.prompt_file).await?;
    let mut content = vec![InputContent::InputText(text.into())];
    content.extend(images.into_iter().map(InputContent::InputImage));
    Ok(Some((
        instructions,
        ApiMessageItem::Input(InputMessage {
            content,
            role: InputRole::User,
            status: Some(OutputStatus::Completed),
        }),
    )))
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
) -> color_eyre::Result<HeadlessOutcome>
where
    F: std::future::Future<Output = color_eyre::Result<HeadlessOutcome>>,
{
    let deadline = async {
        match timeout_seconds {
            Some(seconds) => tokio::time::sleep(Duration::from_secs(seconds)).await,
            None => std::future::pending::<()>().await,
        }
    };
    tokio::select! {
        biased;
        _ = cancel.wait() => Ok(HeadlessOutcome::Interrupted),
        _ = deadline => {
            cancel.cancel();
            Ok(HeadlessOutcome::Timeout)
        }
        result = operation => result,
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

fn same_dir(a: &Path, b: &Path) -> bool {
    let canonical = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    canonical(a) == canonical(b)
}

fn current_dir() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

#[derive(Debug, Serialize)]
struct AgentResult<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pending: Option<&'a pending::Pending>,
    session_id: Option<&'a str>,
    schema_version: u32,
    steps: usize,
    tool_calls: usize,
    duration_ms: u128,
    compactions: usize,
    response: &'a str,
    model: &'a str,
    usage: Usage,
    #[serde(skip_serializing_if = "Option::is_none")]
    check: Option<&'a DiagnosticsReport>,
    passed: bool,
    stop_reason: &'a str,
    work_mode: String,
    security: Option<&'a crate::headless_security::SecurityMetadata>,
}

#[derive(Debug, Serialize)]
struct Usage {
    input_tokens: u32,
    output_tokens: u32,
    cached_input_tokens: u32,
}

#[allow(clippy::too_many_arguments)]
fn emit_agent_result(
    format: AgentOutputFormat,
    model: &str,
    result: TurnResult,
    check: Option<&DiagnosticsReport>,
    outcome: HeadlessOutcome,
    mode: WorkMode,
    security: Option<&crate::headless_security::SecurityMetadata>,
    duration_ms: u128,
    compactions: usize,
    session_id: Option<&str>,
    pending: Option<&pending::Pending>,
) -> color_eyre::Result<()> {
    // The runner checkpoint is private session state, not part of the output contract.
    let public_pending = pending.map(|pending| pending::Pending {
        checkpoint: None,
        ..pending.clone()
    });
    let pending = public_pending.as_ref();
    let output = AgentResult {
        pending,
        schema_version: OUTPUT_SCHEMA_VERSION,
        session_id,
        steps: result.steps,
        tool_calls: result.tool_calls,
        duration_ms,
        compactions,
        response: &result.final_text,
        model,
        usage: Usage {
            input_tokens: result.usage.0,
            output_tokens: result.usage.1,
            cached_input_tokens: result.usage.2,
        },
        check,
        passed: outcome == HeadlessOutcome::Completed,
        stop_reason: outcome.stop_reason(),
        work_mode: mode_label(mode),
        security,
    };

    if format == AgentOutputFormat::Jsonl
        && let Some(pending) = pending
    {
        println!(
            "{}",
            json!({"schema_version": OUTPUT_SCHEMA_VERSION, "type": "input_required", "pending": pending})
        );
    }
    match format {
        AgentOutputFormat::Text => {
            println!(
                "{}",
                pending.map_or(result.final_text.as_str(), |p| p.prompt.as_str())
            );
            if let Some(pending) = pending {
                eprintln!("Continue: {}", pending.resume_command);
            }
            if let Some(id) = session_id {
                eprintln!("session: {id}");
            }
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

fn validate_answer(
    pending: Option<&pending::Pending>,
    answer: Option<&str>,
    id: Option<&str>,
    allow_yolo: bool,
) -> color_eyre::Result<()> {
    let fail = |message: &str| crate::headless_security::UsageError(message.into());
    let Some(pending) = pending else {
        return if answer.is_some() {
            Err(fail("session has no pending interaction").into())
        } else {
            Ok(())
        };
    };
    let answer =
        answer.ok_or_else(|| fail("pending interaction requires --answer or --answer-file"))?;
    if id.is_some_and(|id| id != pending.id) {
        return Err(fail("--answer-id does not match pending interaction").into());
    }
    let valid = match pending.kind.as_str() {
        "tool_approval" => {
            answer == "approve"
                || answer == "deny"
                || answer
                    .strip_prefix("deny:")
                    .is_some_and(|s| !s.trim().is_empty())
        }
        "permission" => {
            answer == "allow"
                || answer == "deny"
                || answer
                    .strip_prefix("deny:")
                    .is_some_and(|s| !s.trim().is_empty())
        }
        "question" => true,
        "plan_review" => interaction::parse_plan_answer(answer, allow_yolo).is_ok(),
        _ => false,
    };
    if !valid {
        return Err(fail("invalid --answer for pending interaction").into());
    }
    Ok(())
}

fn normalize_answer(pending: &crate::runner::Pending, answer: &str) -> String {
    match pending.kind {
        crate::runner::PendingKind::Permission if answer == "allow" => {
            "Approve for this session".into()
        }
        crate::runner::PendingKind::Question => answer
            .parse::<usize>()
            .ok()
            .and_then(|index| index.checked_sub(1))
            .and_then(|index| pending.options.get(index))
            .cloned()
            .unwrap_or_else(|| answer.into()),
        _ => answer.into(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn pending_answers_are_validated_without_mutation() {
        let mut p =
            super::pending::Pending::plan("Review".into(), uuid::Uuid::new_v4().to_string());
        assert!(super::validate_answer(Some(&p), None, None, false).is_err());
        assert!(super::validate_answer(Some(&p), Some("auto"), Some("wrong"), false).is_err());
        assert!(super::validate_answer(Some(&p), Some("yolo"), None, false).is_err());
        assert!(super::validate_answer(Some(&p), Some("revise: add tests"), None, false).is_ok());
        for (kind, valid, invalid) in [
            ("tool_approval", "approve", "allow"),
            ("permission", "allow", "approve"),
        ] {
            p.kind = kind.into();
            for answer in [valid, "deny", "deny: not needed"] {
                assert!(super::validate_answer(Some(&p), Some(answer), Some(&p.id), false).is_ok());
            }
            assert!(super::validate_answer(Some(&p), Some(invalid), None, false).is_err());
        }
        assert!(super::validate_answer(None, Some("approve"), None, false).is_err());
        let mut snapshot = crate::session::SessionManager::for_test(std::env::temp_dir()).create();
        snapshot.headless_pending = Some(serde_json::to_value(&p).unwrap());
        let restored: crate::session::SessionSnapshot =
            serde_json::from_value(serde_json::to_value(&snapshot).unwrap()).unwrap();
        assert_eq!(snapshot.headless_pending, restored.headless_pending);
    }
    use super::*;
    use crate::tools::provider::{McpToolProvider, ToolRegistry};

    fn http_server(name: &str, url: String) -> crate::mcp::types::McpServerConfig {
        crate::mcp::types::McpServerConfig {
            name: name.to_string(),
            command: String::new(),
            args: Vec::new(),
            env: std::collections::HashMap::new(),
            url: Some(url),
        }
    }

    #[tokio::test]
    async fn headless_without_mcp_config_starts_no_servers() {
        assert!(connect_mcp(&[]).await.is_none());
    }

    #[tokio::test]
    async fn headless_advertises_connected_mcp_tools_and_skips_failed_servers() {
        let (url, server) = crate::mcp::tests::spawn_mock_http_server().await;
        let configs = [
            http_server("mock", url),
            // Nothing listens on port 9: startup fails without aborting the run.
            http_server("dead", "http://127.0.0.1:9/mcp".to_string()),
        ];
        let manager = tokio::time::timeout(Duration::from_secs(30), connect_mcp(&configs))
            .await
            .expect("MCP startup must finish")
            .expect("configured servers yield a manager");
        server.abort();

        assert!(manager.has_server("mock"));
        assert!(!manager.has_server("dead"));
        assert_eq!(manager.startup_errors.len(), 1);

        let registry = ToolRegistry::new(vec![Arc::new(McpToolProvider::new(manager))]);
        let names: Vec<String> = registry
            .tools()
            .into_iter()
            .filter_map(|tool| match tool {
                async_openai::types::responses::Tool::Function(function) => Some(function.name),
                _ => None,
            })
            .collect();
        assert!(
            names.iter().any(|name| name == "mcp__mock__echo"),
            "advertised tools: {names:?}"
        );
    }

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

    fn agent_args(flags: &[&str]) -> AgentArgs {
        use clap::Parser;
        let mut argv = vec!["programmer", "init"];
        argv.extend_from_slice(flags);
        let parsed = crate::cli::Args::try_parse_from(argv).unwrap();
        let Some(crate::cli::Command::Init(args)) = parsed.command else {
            panic!("init")
        };
        args.agent
    }

    #[test]
    fn invocation_overrides_are_local_and_select_servers() {
        let original = crate::config::programmer_config::ProgrammerConfig::default();
        let mut config = original.clone();
        config.mcp_servers = vec![
            http_server("one", "unused".into()),
            http_server("two", "unused".into()),
        ];
        apply_overrides(
            &mut config,
            &agent_args(&[
                "--vision",
                "--no-memory",
                "--mcp",
                "two",
                "--coauthor",
                "Name <mail@example.com>",
            ]),
        )
        .unwrap();
        assert!(config.vision_enabled);
        assert!(!config.memory.enabled);
        assert_eq!(config.mcp_servers.len(), 1);
        assert_eq!(config.mcp_servers[0].name, "two");
        assert_eq!(
            config.git_coauthor.as_deref(),
            Some("Name <mail@example.com>")
        );
        assert!(original.memory.enabled);
        apply_overrides(
            &mut config,
            &agent_args(&["--no-vision", "--no-mcp", "--no-coauthor"]),
        )
        .unwrap();
        assert!(!config.vision_enabled);
        assert!(config.mcp_servers.is_empty());
        assert!(config.git_coauthor.is_none());
    }

    #[tokio::test]
    async fn appended_instructions_and_images_use_shared_encoding() {
        use image::{ImageBuffer, ImageFormat, Rgb};
        let temp = std::env::temp_dir().join(format!("programmer-inputs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&temp).unwrap();
        let instruction = temp.join("instructions.txt");
        std::fs::write(&instruction, "file instruction").unwrap();
        let soul = temp.join("soul.txt");
        std::fs::write(&soul, "temporary soul").unwrap();
        let mut config = crate::config::programmer_config::ProgrammerConfig::default();
        apply_overrides(
            &mut config,
            &agent_args(&["--soul-file", soul.to_str().unwrap()]),
        )
        .unwrap();
        assert_eq!(config.soul.as_deref(), Some("temporary soul"));
        let path = temp.join("image.png");
        ImageBuffer::from_pixel(2, 1, Rgb([10u8, 20, 30]))
            .save_with_format(&path, ImageFormat::Png)
            .unwrap();
        let inputs = RunInputs {
            prompt: Some("user text".into()),
            append_prompt: vec!["first".into(), "second".into()],
            append_prompt_file: vec![instruction],
            images: vec![path],
            ..Default::default()
        };
        let (instructions, message) = prepare_inputs(Some(inputs), true).await.unwrap().unwrap();
        assert_eq!(instructions, ["first", "second", "file instruction"]);
        let ApiMessageItem::Input(message) = message else {
            panic!("input")
        };
        assert_eq!(message.role, InputRole::User);
        assert!(
            matches!(&message.content[0], InputContent::InputText(text) if text.text == "user text")
        );
        assert!(
            matches!(&message.content[1], InputContent::InputImage(image) if image.image_url.as_deref().unwrap().starts_with("data:image/png;base64,"))
        );
        for name in ["instructions.txt", "soul.txt", "image.png"] {
            std::fs::remove_file(temp.join(name)).unwrap();
        }
        std::fs::remove_dir(temp).unwrap();
    }

    #[tokio::test]
    async fn images_require_vision_before_reading_files() {
        let error = prepare_inputs(
            Some(RunInputs {
                prompt: Some("text".into()),
                images: vec![PathBuf::from("missing.png")],
                ..Default::default()
            }),
            false,
        )
        .await
        .unwrap_err();
        assert_eq!(HeadlessOutcome::from_error(&error), HeadlessOutcome::Usage);
    }

    #[test]
    fn exit_codes_are_stable() {
        for (outcome, code) in [
            (HeadlessOutcome::Completed, 0),
            (HeadlessOutcome::Error, 1),
            (HeadlessOutcome::Usage, 2),
            (HeadlessOutcome::CheckFailed, 3),
            (HeadlessOutcome::Timeout, 4),
            (HeadlessOutcome::MaxSteps, 5),
            (HeadlessOutcome::NeedsInput, 10),
            (HeadlessOutcome::Interrupted, 130),
        ] {
            assert_eq!(outcome.exit_code(), code);
        }
        let limit = color_eyre::Report::new(crate::runner::RunnerError::StepLimit { limit: 1 });
        assert_eq!(
            HeadlessOutcome::from_error(&limit),
            HeadlessOutcome::MaxSteps
        );
        let cancelled = color_eyre::Report::new(crate::runner::RunnerError::Cancelled);
        assert_eq!(
            HeadlessOutcome::from_error(&cancelled),
            HeadlessOutcome::Interrupted
        );
        assert_eq!(
            HeadlessOutcome::from_error(&color_eyre::eyre::eyre!("IO")),
            HeadlessOutcome::Error
        );
    }

    #[tokio::test]
    async fn deadline_and_cancellation_have_distinct_outcomes() {
        let cancel = CancellationToken::new();
        let result = run_with_timeout(std::future::pending(), Some(0), &cancel)
            .await
            .unwrap();
        assert_eq!(result, HeadlessOutcome::Timeout);
        assert!(cancel.is_cancelled());
        assert_eq!(
            run_with_timeout(std::future::pending(), None, &cancel)
                .await
                .unwrap(),
            HeadlessOutcome::Interrupted
        );
        let cancel = CancellationToken::new();
        assert_eq!(
            run_with_timeout(async { Ok(HeadlessOutcome::Completed) }, None, &cancel)
                .await
                .unwrap(),
            HeadlessOutcome::Completed
        );
    }

    #[test]
    fn surface_keeps_last_assistant_text_without_jsonl() {
        let progress = Arc::new(Mutex::new(Progress::default()));
        let surface = CliSurface {
            suspend: false,
            compaction: None,
            progress: progress.clone(),
            jsonl: false,
            plan: false,
            skill_prompt: None,
            approval_label: String::new(),
        };
        surface.on_event(RunnerEvent::Assistant("partial answer"));
        assert_eq!(progress.lock().unwrap().text, "partial answer");
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
            suspend: false,
            compaction: None,
            progress: Arc::default(),
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
