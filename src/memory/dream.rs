// Copyright (C) 2026 huangdihd
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

//! Recoverable background extraction and consolidation for durable memory.
//!
//! Completed sessions are written to a per-project pending queue. An in-process
//! worker consumes that queue once both the queued-session and interval
//! thresholds are met, asks the dedicated memory model for a bounded operation
//! plan, and applies it under a cross-process lock over the memory root.
//!
//! The design is deliberately layered by who authorizes the change:
//!
//! 1. *Automatic* passes run unattended and may only add or extend memories the
//!    model marked as directly stated or demonstrated in a session.
//! 2. *Previewed* passes (`/memory dream preview` then `apply`) may merge,
//!    supersede, and archive, because a human read the plan first.
//!
//! Queue files are only retired after every memory file and the scheduler state
//! have been committed, so a crash, cancellation, or provider outage loses no
//! work. An interrupted commit blocks memory access until explicit
//! `/memory dream recover` rolls its journal forward.

use super::{
    MemoryConfidence, MemoryEntry, MemoryFile, MemoryKind, MemoryManager, MemoryScope,
    MemorySource, MemoryStatus, content_looks_sensitive, normalize_tags, now_secs,
    validate_content,
};
use async_openai::Client;
use async_openai::config::OpenAIConfig;
use async_openai::types::responses::{
    CreateResponse, InputParam, OutputItem, OutputMessageContent,
};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

mod history;
pub(super) use history::validate_entry_ids;
pub(crate) use history::{DreamHistory, DreamRunStatus};

const DREAM_SCHEMA_VERSION: u32 = 1;
const MAX_PENDING_TRANSCRIPT_CHARS: usize = 12_000;
const MAX_PENDING_BATCH: usize = 20;
const MAX_OPERATIONS: usize = 32;
const PREVIEW_FILE: &str = ".dream-preview.json";
const STATE_FILE: &str = ".dream-state.json";
const LOCK_FILE: &str = ".dream.lock";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PendingDream {
    pub(crate) schema_version: u32,
    pub(crate) id: String,
    pub(crate) session_id: String,
    pub(crate) created_at: u64,
    pub(crate) transcript: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct DreamState {
    pub(crate) schema_version: u32,
    pub(crate) last_dream_at: Option<u64>,
    pub(crate) last_processed_session_count: usize,
    pub(crate) last_error: Option<String>,
    pub(crate) last_operation_count: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct DreamConfig {
    pub(crate) enabled: bool,
    pub(crate) min_sessions: usize,
    pub(crate) min_interval_hours: u64,
    pub(crate) timeout_secs: u64,
}

impl Default for DreamConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_sessions: 5,
            min_interval_hours: 24,
            timeout_secs: 60,
        }
    }
}

impl From<&crate::config::programmer_config::MemoryConfig> for DreamConfig {
    fn from(config: &crate::config::programmer_config::MemoryConfig) -> Self {
        Self {
            enabled: config.enabled && config.dream_enabled,
            min_sessions: config.dream_min_sessions,
            min_interval_hours: config.dream_min_interval_hours,
            timeout_secs: config.dream_timeout_secs,
        }
    }
}

/// One-line summary of the consolidation queue for the user-only slash command.
pub(crate) fn render_status(state: &DreamState, pending: usize, preview: bool) -> String {
    render_status_at(state, pending, preview, now_secs())
}

fn render_status_at(state: &DreamState, pending: usize, preview: bool, now: u64) -> String {
    format!(
        "Dream status: queued={pending} transcript(s), preview={}, last_run={}, last_operations={}, last_error={}",
        if preview { "ready" } else { "none" },
        state
            .last_dream_at
            .map(|timestamp| {
                if timestamp > now {
                    return "unknown (clock mismatch)".into();
                }
                let age = now - timestamp;
                match age {
                    0..60 => "just now".into(),
                    60..3600 => format!("{}m ago", age / 60),
                    3600..86400 => format!("{}h {}m ago", age / 3600, age % 3600 / 60),
                    _ => format!("{}d {}h ago", age / 86400, age % 86400 / 3600),
                }
            })
            .unwrap_or_else(|| "never".to_string()),
        state.last_operation_count,
        state.last_error.as_deref().unwrap_or("none")
    )
}

/// The live Dream inputs owned by the front-end. Snapshotting this on every
/// wake means `/model`, provider edits, and `/memory off` take effect without
/// restarting the worker.
#[derive(Clone, Default)]
pub(crate) struct DreamRuntime {
    pub(crate) config: DreamConfig,
    pub(crate) model: Option<DreamModel>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DreamPlan {
    pub(crate) schema_version: u32,
    #[serde(default = "new_run_id")]
    pub(crate) run_id: String,
    pub(crate) generated_at: u64,
    /// How this plan may be applied. An automatic pass is model-only; a
    /// preview is reviewed by a human before it is applied.
    pub(crate) policy: DreamPolicy,
    #[serde(default)]
    pub(crate) source_pending_ids: Vec<String>,
    #[serde(default)]
    pub(crate) operations: Vec<DreamOperation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DreamPolicy {
    /// Generated by the background worker; only high-confidence operations
    /// may be applied without review.
    Automatic,
    /// Generated for the user and applied only via `/memory dream apply`.
    Preview,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DreamOperation {
    pub(crate) action: DreamAction,
    #[serde(default)]
    pub(crate) id: Option<String>,
    #[serde(default)]
    pub(crate) scope: Option<MemoryScope>,
    #[serde(default)]
    pub(crate) kind: Option<MemoryKind>,
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) description: Option<String>,
    #[serde(default)]
    pub(crate) content: Option<String>,
    #[serde(default)]
    pub(crate) tags: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) related_memories: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) confidence: Option<MemoryConfidence>,
    #[serde(default)]
    pub(crate) supersedes: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DreamAction {
    Create,
    Update,
    Supersede,
    Archive,
}

#[derive(Clone)]
pub(crate) struct DreamModel {
    pub(crate) client: Client<OpenAIConfig>,
    pub(crate) model: String,
}

#[derive(Debug)]
pub(crate) struct DreamReport {
    pub(crate) pending: usize,
    pub(crate) operations: usize,
    pub(crate) applied: bool,
    pub(crate) message: String,
}

impl DreamReport {
    fn skipped(pending: usize, message: impl Into<String>) -> Self {
        Self {
            pending,
            operations: 0,
            applied: false,
            message: message.into(),
        }
    }
}

/// Whether one operation may be applied by an unattended pass.
///
/// Automatic Dream never rewrites or retires an existing memory on model
/// judgement alone: it only adds information the model marked as directly
/// stated or demonstrated. Merging, superseding, and archiving stay behind the
/// reviewed `/memory dream preview` + `apply` path.
fn automatic_operation_allowed(operation: &DreamOperation) -> bool {
    match operation.action {
        DreamAction::Create | DreamAction::Update => matches!(
            operation.confidence,
            Some(MemoryConfidence::Explicit | MemoryConfidence::Confirmed)
        ),
        DreamAction::Supersede | DreamAction::Archive => false,
    }
}

impl MemoryManager {
    fn dream_dir(&self) -> PathBuf {
        self.directory(MemoryScope::Project)
    }

    fn pending_dir(&self) -> PathBuf {
        self.dream_dir().join("pending")
    }

    pub(crate) fn enqueue_dream(
        &self,
        session_id: &str,
        transcript: &str,
    ) -> Result<PendingDream, String> {
        // Queue inputs are content-addressed and independent of memory commits.
        // A running model must not make session-close enqueue silently fail.
        self.validate_storage_path(&self.pending_dir())?;
        self.validate_storage_path(&self.dream_dir().join("processed"))?;
        let transcript = transcript.trim();
        if transcript.is_empty() {
            return Err("dream transcript must not be empty".to_string());
        }
        if content_looks_sensitive(transcript) {
            return Err("dream transcript appears to contain a credential or secret".to_string());
        }
        let transcript = tail_chars(transcript, MAX_PENDING_TRANSCRIPT_CHARS);
        let digest = blake3::hash(format!("{session_id}\n{transcript}").as_bytes());
        let pending = PendingDream {
            schema_version: DREAM_SCHEMA_VERSION,
            id: format!("dream_{}", &digest.to_hex()[..24]),
            session_id: session_id.to_string(),
            created_at: now_secs(),
            transcript,
        };
        let dir = self.pending_dir();
        std::fs::create_dir_all(&dir)
            .map_err(|error| format!("create {}: {error}", dir.display()))?;
        let path = dir.join(format!("{}.json", pending.id));
        if path.exists()
            || self
                .dream_dir()
                .join("processed")
                .join(path.file_name().unwrap())
                .exists()
        {
            return Ok(pending);
        }
        atomic_write_json(&path, &pending)?;
        Ok(pending)
    }

    pub(crate) fn dream_status(&self) -> Result<(DreamState, usize, bool), String> {
        let state = self.load_dream_state()?;
        let pending = self.pending_count()?;
        let preview = self.dream_dir().join(PREVIEW_FILE).exists();
        Ok((state, pending, preview))
    }

    /// Queue length as reported to the user. A pass itself processes at most
    /// [`MAX_PENDING_BATCH`] sessions, but the queue may legitimately be longer.
    fn pending_count(&self) -> Result<usize, String> {
        let dir = self.pending_dir();
        self.validate_storage_path(&dir)?;
        if !dir.exists() {
            return Ok(0);
        }
        Ok(std::fs::read_dir(&dir)
            .map_err(|error| format!("read {}: {error}", dir.display()))?
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.path().extension().and_then(|value| value.to_str()) == Some("json")
            })
            .count())
    }

    fn load_dream_state(&self) -> Result<DreamState, String> {
        let path = self.dream_dir().join(STATE_FILE);
        self.validate_storage_path(&path)?;
        if !path.exists() {
            return Ok(DreamState {
                schema_version: DREAM_SCHEMA_VERSION,
                ..DreamState::default()
            });
        }
        serde_json::from_slice(
            &std::fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?,
        )
        .map_err(|error| format!("parse {}: {error}", path.display()))
    }

    fn save_dream_state(&self, state: &DreamState) -> Result<(), String> {
        atomic_write_json(&self.dream_dir().join(STATE_FILE), state)
    }

    fn load_pending(&self) -> Result<Vec<(PathBuf, PendingDream)>, String> {
        let dir = self.pending_dir();
        self.validate_storage_path(&dir)?;
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut paths = std::fs::read_dir(&dir)
            .map_err(|error| format!("read {}: {error}", dir.display()))?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
            .collect::<Vec<_>>();
        paths.sort();
        let mut pending = Vec::new();
        for path in paths.into_iter().take(MAX_PENDING_BATCH) {
            self.validate_storage_path(&path)?;
            // A queue file that cannot be read or parsed is skipped rather than
            // failing the whole pass: one corrupt entry must not block every
            // other session from being consolidated.
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let Ok(item) = serde_json::from_slice::<PendingDream>(&bytes) else {
                continue;
            };
            if item.schema_version != DREAM_SCHEMA_VERSION
                || !safe_id(&item.id)
                || path.file_stem().and_then(|value| value.to_str()) != Some(item.id.as_str())
            {
                return Err("invalid Dream queue schema or ID".into());
            }
            pending.push((path, item));
        }
        Ok(pending)
    }

    pub(super) fn acquire_dream_lock(&self) -> Result<Option<File>, String> {
        let lock = self.acquire_transaction_lock()?;
        self.ensure_no_dream_transaction()?;
        Ok(lock)
    }

    fn acquire_transaction_lock(&self) -> Result<Option<File>, String> {
        self.validate_storage_path(&self.dream_dir())?;
        std::fs::create_dir_all(self.dream_dir())
            .map_err(|error| format!("create dream directory: {error}"))?;
        // Lock the whole memory root, not only this project: a Dream pass may
        // update global memories shared by every workspace.
        let global_dir = self.directory(MemoryScope::Global);
        let memory_root = global_dir
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.dream_dir());
        let path = memory_root.join(LOCK_FILE);
        self.validate_storage_path(&path)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|error| format!("open {}: {error}", path.display()))?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(Some(file)),
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || cfg!(windows) && error.raw_os_error() == Some(33) =>
            {
                Ok(None)
            }
            Err(error) => Err(format!("lock {}: {error}", path.display())),
        }
    }

    fn apply_plan(
        &self,
        plan: &DreamPlan,
        scope_filter: Option<MemoryScope>,
    ) -> Result<usize, String> {
        self.apply_plan_with_history(plan, scope_filter)
    }
}

impl DreamModel {
    /// Automatic consolidation pass. Thresholds, generation, the automatic
    /// confidence policy, application, and queue/state commit all happen under
    /// the cross-process Dream lock so two Programmer instances cannot both
    /// rewrite memory from the same pending sessions.
    pub(crate) async fn run_auto(
        &self,
        manager: &MemoryManager,
        config: &DreamConfig,
    ) -> Result<DreamReport, String> {
        if !config.enabled {
            return Ok(DreamReport::skipped(0, "automatic Dream is disabled"));
        }
        let Some(_lock) = manager.acquire_dream_lock()? else {
            return Ok(DreamReport::skipped(
                manager.pending_count()?,
                "another Programmer process is already running Dream",
            ));
        };

        let pending = manager.load_pending()?;
        let queued = manager.pending_count()?;
        let mut state = manager.load_dream_state()?;
        if pending.is_empty() {
            return Ok(DreamReport::skipped(0, "no pending sessions"));
        }
        let interval = config.min_interval_hours.saturating_mul(3600);
        let interval_due = state
            .last_dream_at
            .is_none_or(|last| now_secs().saturating_sub(last) >= interval);
        // Count distinct sessions, not queue files: one session may be queued
        // more than once when it continues after an earlier pass, and that must
        // not look like several finished sessions. The batch cap is the highest
        // threshold that can ever be satisfied in one pass.
        let threshold = config.min_sessions.clamp(1, MAX_PENDING_BATCH);
        let finished_sessions = pending
            .iter()
            .map(|(_, item)| item.session_id.as_str())
            .collect::<std::collections::HashSet<_>>()
            .len();
        // Sessions are only one of the two gates: a burst of sessions must not
        // trigger back-to-back passes inside the same interval.
        if finished_sessions < threshold || !interval_due {
            return Ok(DreamReport::skipped(
                queued,
                "Dream thresholds have not been reached",
            ));
        }

        let memories = manager.list_unlocked(None)?;
        let run_id = manager.record_generation_start(&pending)?;
        let mut plan = match self
            .generate_plan(&pending, &memories, config.timeout_secs)
            .await
        {
            Ok(plan) => plan,
            Err(error) => {
                // Keep the queue: the sessions are still unprocessed, and the
                // failure is visible in `dream status`.
                manager.record_generation_failure(&run_id, &pending, &error)?;
                state.last_error = Some(error.clone());
                manager.save_dream_state(&state).map_err(|state_error| {
                    format!("{error}; also failed to record Dream state: {state_error}")
                })?;
                return Err(error);
            }
        };
        plan.run_id = run_id;
        plan.policy = DreamPolicy::Automatic;
        let applied = manager.apply_plan(&plan, None)?;
        Ok(DreamReport {
            pending: manager.pending_count()?,
            operations: applied,
            applied: true,
            message: format!(
                "Dream consolidated {applied} of {} operation(s) from {} session(s)",
                plan.operations.len(),
                plan.source_pending_ids.len()
            ),
        })
    }

    /// Generate and persist an auditable plan without touching memory. The plan
    /// stays on disk until the user applies it, so a preview never requires a
    /// second model call.
    pub(crate) async fn preview(
        &self,
        manager: &MemoryManager,
        config: &DreamConfig,
        scope: Option<MemoryScope>,
        cancellation: crate::cancel::CancellationToken,
    ) -> Result<DreamReport, String> {
        if cancellation.is_cancelled() {
            return Err("Dream preview cancelled".into());
        }
        let Some(_lock) = manager.acquire_dream_lock()? else {
            return Ok(DreamReport::skipped(
                manager.pending_count()?,
                "another Programmer process is already running Dream",
            ));
        };
        let pending = manager.load_pending()?;
        let mut state = manager.load_dream_state()?;
        if pending.is_empty() {
            return Ok(DreamReport::skipped(0, "no pending sessions"));
        }
        let memories = manager.list_unlocked(None)?;
        let run_id = manager.record_generation_start(&pending)?;
        let plan = match cancellation
            .wait_or(self.generate_plan(&pending, &memories, config.timeout_secs))
            .await
            .unwrap_or_else(|| Err("Dream preview cancelled".into()))
        {
            Ok(plan) => plan,
            Err(error) => {
                manager.record_generation_failure(&run_id, &pending, &error)?;
                state.last_error = Some(error.clone());
                manager.save_dream_state(&state).map_err(|state_error| {
                    format!("{error}; also failed to record Dream state: {state_error}")
                })?;
                return Err(error);
            }
        };
        let mut plan = plan;
        plan.run_id = run_id;
        plan.policy = DreamPolicy::Preview;
        if let Some(scope) = scope {
            plan.operations
                .retain(|operation| operation.scope.is_none_or(|value| value == scope));
        }
        manager.record_preview(&plan, &pending)?;
        atomic_write_json(&manager.dream_dir().join(PREVIEW_FILE), &plan)?;
        Ok(DreamReport {
            pending: manager.pending_count()?,
            operations: plan.operations.len(),
            applied: false,
            message: format!(
                "Dream preview has {} operation(s) from {} session(s); review {} and run `/memory dream apply`",
                plan.operations.len(),
                plan.source_pending_ids.len(),
                manager.dream_dir().join(PREVIEW_FILE).display()
            ),
        })
    }

    async fn generate_plan(
        &self,
        pending: &[(PathBuf, PendingDream)],
        memories: &[MemoryEntry],
        timeout_secs: u64,
    ) -> Result<DreamPlan, String> {
        let transcripts = pending
            .iter()
            .map(|(_, item)| {
                serde_json::json!({
                    "id": item.id,
                    "session_id": item.session_id,
                    "created_at": item.created_at,
                    "transcript": item.transcript,
                })
            })
            .collect::<Vec<_>>();
        let manifest = memories
            .iter()
            .map(|entry| {
                serde_json::json!({
                    "id": entry.id,
                    "scope": entry.scope,
                    "kind": entry.kind,
                    "name": entry.name,
                    "description": entry.description,
                    "content": entry.content,
                    "tags": entry.tags,
                    "updated_at": entry.updated_at,
                    "use_count": entry.use_count,
                })
            })
            .collect::<Vec<_>>();
        let prompt = format!(
            "Pending completed sessions:\n{}\n\nActive memory manifest:\n{}\n\nChoose the narrowest correct scope for each operation.\nReturn one JSON object with an `operations` array. Each operation has action create|update|supersede|archive. Create requires scope, kind, name, description, content, confidence and may include tags/related_memories/supersedes. Update requires id and only changed fields. Supersede requires id and may create a replacement as a separate create operation whose supersedes names the old id. Archive requires id. Preserve explicit user preferences and corrections. Record only stable, reusable, verified information that cannot simply be read from the repository. Never record credentials, temporary progress, guesses, tool output, or assistant proposals the user did not confirm. Merge semantic duplicates; resolve contradictions in favor of newer explicit user statements. Use confidence explicit only for direct user statements, confirmed for decisions demonstrated or accepted in the session, and inferred for anything you would want a human to review. Return JSON only.",
            serde_json::to_string(&transcripts).map_err(|error| error.to_string())?,
            serde_json::to_string(&manifest).map_err(|error| error.to_string())?,
        );
        let request = CreateResponse {
            model: Some(self.model.clone()),
            input: InputParam::Text(prompt),
            instructions: Some(
                "You are Programmer's Dream memory curator. Produce a conservative, auditable long-term-memory consolidation plan."
                    .to_string(),
            ),
            temperature: Some(0.0),
            max_output_tokens: Some(4096),
            store: Some(false),
            ..Default::default()
        };
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs.max(1)),
            self.client.responses().create(request),
        )
        .await
        .map_err(|_| format!("Dream timed out after {}s", timeout_secs.max(1)))?
        .map_err(format_planning_error)?;
        let text = response_text(&response.output)?;
        let json = strip_json_fence(&text);
        #[derive(Deserialize)]
        struct Operations {
            #[serde(default)]
            operations: Vec<DreamOperation>,
        }
        let parsed: Operations =
            serde_json::from_str(json).map_err(|error| format!("invalid Dream plan: {error}"))?;
        let operations = parsed.operations.into_iter().take(MAX_OPERATIONS).collect();
        Ok(DreamPlan {
            schema_version: DREAM_SCHEMA_VERSION,
            run_id: new_run_id(),
            generated_at: now_secs(),
            // Overwritten by the caller: generation never decides how its own
            // plan may be applied.
            policy: DreamPolicy::Preview,
            source_pending_ids: pending.iter().map(|(_, item)| item.id.clone()).collect(),
            operations,
        })
    }
}

fn format_planning_error(error: async_openai::error::OpenAIError) -> String {
    match error {
        async_openai::error::OpenAIError::Reqwest(error) => {
            let category = if error.is_timeout() {
                "HTTP client timeout"
            } else if error.is_connect() {
                "HTTP connection failed"
            } else if error.is_body() || error.is_decode() {
                "HTTP response body failed"
            } else {
                "HTTP request failed"
            };
            // Request URLs may carry credentials in userinfo or query parameters.
            // Display/source preserves causes without dumping headers or bodies.
            let error = error.without_url();
            format!(
                "Dream planning: {category}: {}",
                planning_error_chain(&error)
            )
        }
        error => format!("Dream planning: {}", planning_error_chain(&error)),
    }
}

fn planning_error_chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    // Bound traversal even if a provider error exposes a cyclic source chain.
    for _ in 0..8 {
        let Some(cause) = source else {
            return message;
        };
        message.push_str("; caused by: ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    if source.is_some() {
        message.push_str("; further causes omitted");
    }
    message
}

/// Apply the persisted preview. This performs no model call and takes the same
/// cross-process lock as an automatic pass, so an explicit apply can never
/// interleave with a background consolidation.
pub(crate) fn apply_saved_preview(
    manager: &MemoryManager,
    scope: Option<MemoryScope>,
) -> Result<DreamReport, String> {
    let path = manager.dream_dir().join(PREVIEW_FILE);
    if !path.exists() {
        return Err("no Dream preview is available; run `/memory dream preview` first".to_string());
    }
    let Some(_lock) = manager.acquire_dream_lock()? else {
        return Ok(DreamReport::skipped(
            manager.pending_count()?,
            "another Programmer process is already running Dream",
        ));
    };
    let plan: DreamPlan = serde_json::from_slice(
        &std::fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?,
    )
    .map_err(|error| format!("parse {}: {error}", path.display()))?;
    let applied = manager.apply_plan(&plan, scope)?;
    Ok(DreamReport {
        pending: manager.pending_count()?,
        operations: applied,
        applied: true,
        message: format!("Dream applied {applied} previewed operation(s)"),
    })
}

/// Whether a queue id is safe to use as a file name: Dream ids are generated
/// locally, but a preview applied from disk is untrusted input.
fn new_run_id() -> String {
    format!("dream_{}", uuid::Uuid::new_v4().simple())
}

pub(super) fn safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '-'
        })
}

fn finish_applied_plan(
    manager: &MemoryManager,
    plan: &DreamPlan,
    state: &mut DreamState,
    applied: usize,
) -> Result<(), String> {
    let processed_dir = manager.dream_dir().join("processed");
    std::fs::create_dir_all(&processed_dir)
        .map_err(|error| format!("create {}: {error}", processed_dir.display()))?;
    for id in &plan.source_pending_ids {
        // The preview file is user-writable, so an id is only ever used as a
        // file name after it is confirmed to be one.
        if !safe_id(id) {
            return Err("invalid Dream source ID".into());
        }
        let source = manager.pending_dir().join(format!("{id}.json"));
        if source.exists() {
            let target = processed_dir.join(format!("{id}.json"));
            std::fs::rename(&source, &target).map_err(|error| {
                format!("move {} to {}: {error}", source.display(), target.display())
            })?;
        }
    }
    history::sync_directory(&processed_dir)?;
    if manager.pending_dir().exists() {
        history::sync_directory(&manager.pending_dir())?;
    }
    state.schema_version = DREAM_SCHEMA_VERSION;
    state.last_dream_at = Some(now_secs());
    state.last_processed_session_count = plan.source_pending_ids.len();
    state.last_operation_count = applied;
    state.last_error = None;
    manager.save_dream_state(state)?;
    let preview = manager.dream_dir().join(PREVIEW_FILE);
    if preview.exists() {
        std::fs::remove_file(&preview)
            .map_err(|error| format!("remove {}: {error}", preview.display()))?;
    }
    history::sync_directory(&manager.dream_dir())
}

/// How often the background worker re-examines the pending queue while the
/// application is running. The session and interval thresholds still gate every
/// pass, so this only bounds how quickly a newly crossed threshold is noticed.
const WORKER_INTERVAL: std::time::Duration = std::time::Duration::from_secs(600);

/// An in-process consolidation worker.
///
/// Deliberately not a cron job, launchd agent, or Task Scheduler entry: it is
/// started with the application, stops with it, and needs no per-platform
/// installation or elevated permission. Persistence lives in the pending queue
/// and `.dream-state.json`, so work survives a crash without needing a daemon.
pub(crate) struct DreamWorker {
    cancel: crate::cancel::CancellationToken,
    handle: tokio::task::JoinHandle<()>,
    /// The same flag the worker raises while a pass runs, kept here so
    /// `shutdown` can always lower it — aborting the task mid-pass would
    /// otherwise leave the title-bar indicator on.
    active: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl DreamWorker {
    /// `active` is the UI's "a pass is running" flag: it is raised only around
    /// the pass itself so the title bar can show the indicator, and lowered
    /// again for the long sleep in between.
    pub(crate) fn start(
        manager: MemoryManager,
        runtime: std::sync::Arc<std::sync::Mutex<DreamRuntime>>,
        active: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        let cancel = crate::cancel::CancellationToken::new();
        let token = cancel.clone();
        let worker_flag = active.clone();
        let handle = tokio::spawn(async move {
            loop {
                // Snapshot then drop the lock: the front-end updates this on
                // every turn, so the guard must never be held across the
                // network call below.
                let snapshot = runtime
                    .lock()
                    .map(|runtime| runtime.clone())
                    .unwrap_or_default();
                if snapshot.config.enabled
                    && let Some(model) = &snapshot.model
                {
                    let pass = model.run_auto(&manager, &snapshot.config);
                    // Cancelling drops the pass at its next await point; the
                    // queue files survive, so nothing is silently lost.
                    active.store(true, std::sync::atomic::Ordering::Relaxed);
                    let _ = token.wait_or(pass).await;
                    // Also runs when the pass was cancelled: `wait_or` returns
                    // rather than unwinding, and a hard abort happens only while
                    // the process is on its way out.
                    active.store(false, std::sync::atomic::Ordering::Relaxed);
                }
                tokio::select! {
                    () = token.wait() => break,
                    () = tokio::time::sleep(WORKER_INTERVAL) => {}
                }
            }
        });
        Self {
            cancel,
            handle,
            active: worker_flag,
        }
    }
}

/// Stop the worker. An in-flight pass is dropped at its next await point, so a
/// cancelled model call leaves its sources queued. Writes are synchronous and
/// journaled; an I/O failure or process crash requires explicit roll-forward
/// recovery rather than silently reapplying a partially committed plan.
pub(crate) fn shutdown(worker: &mut Option<DreamWorker>) {
    if let Some(worker) = worker.take() {
        // Lower the indicator first: the abort below can land before the pass
        // gets a chance to clear it on its way out.
        worker
            .active
            .store(false, std::sync::atomic::Ordering::Relaxed);
        worker.cancel.cancel();
        worker.handle.abort();
    }
}

fn apply_operation(
    operation: &DreamOperation,
    scope_filter: Option<MemoryScope>,
    global: &mut MemoryFile,
    project: &mut MemoryFile,
) -> Result<bool, String> {
    match operation.action {
        DreamAction::Create => {
            let scope = operation
                .scope
                .ok_or_else(|| "Dream create requires scope".to_string())?;
            if scope_filter.is_some_and(|filter| filter != scope) {
                return Ok(false);
            }
            let kind = operation
                .kind
                .ok_or_else(|| "Dream create requires kind".to_string())?;
            let content = required_text(&operation.content, "content")?;
            validate_content(&content)?;
            let name = validate_name(required_text(&operation.name, "name")?)?;
            let description =
                validate_description(required_text(&operation.description, "description")?)?;
            let file = file_for_scope(scope, global, project);
            if file.entries.iter().any(|entry| {
                entry.status == MemoryStatus::Active
                    && (entry.name.eq_ignore_ascii_case(&name)
                        || entry.content.eq_ignore_ascii_case(&content))
            }) {
                return Ok(false);
            }
            let now = now_secs();
            file.entries.push(MemoryEntry {
                id: format!("mem_{}", uuid::Uuid::new_v4().simple()),
                scope,
                kind,
                name,
                description,
                content,
                tags: normalize_tags(operation.tags.clone().unwrap_or_default()),
                related_memories: operation.related_memories.clone().unwrap_or_default(),
                created_at: now,
                updated_at: now,
                last_used_at: None,
                use_count: 0,
                source: MemorySource::Dream,
                confidence: operation.confidence.unwrap_or(MemoryConfidence::Inferred),
                status: MemoryStatus::Active,
                supersedes: operation.supersedes.clone(),
            });
            Ok(true)
        }
        DreamAction::Update => {
            let id = required_text(&operation.id, "id")?;
            let Some(entry) = find_entry_mut(&id, global, project) else {
                return Ok(false);
            };
            if scope_filter.is_some_and(|filter| filter != entry.scope) {
                return Ok(false);
            }
            if let Some(content) = &operation.content {
                validate_content(content)?;
                entry.content = content.trim().to_string();
            }
            if let Some(kind) = operation.kind {
                entry.kind = kind;
            }
            if let Some(name) = &operation.name {
                entry.name = validate_name(name.clone())?;
            }
            if let Some(description) = &operation.description {
                entry.description = validate_description(description.clone())?;
            }
            if let Some(tags) = &operation.tags {
                entry.tags = normalize_tags(tags.clone());
            }
            if let Some(related) = &operation.related_memories {
                entry.related_memories = related.clone();
            }
            if let Some(confidence) = operation.confidence {
                entry.confidence = confidence;
            }
            entry.updated_at = now_secs();
            Ok(true)
        }
        DreamAction::Supersede | DreamAction::Archive => {
            let id = required_text(&operation.id, "id")?;
            let Some(entry) = find_entry_mut(&id, global, project) else {
                return Ok(false);
            };
            if scope_filter.is_some_and(|filter| filter != entry.scope) {
                return Ok(false);
            }
            entry.status = match operation.action {
                DreamAction::Supersede => MemoryStatus::Superseded,
                DreamAction::Archive => MemoryStatus::Archived,
                _ => unreachable!(),
            };
            entry.updated_at = now_secs();
            Ok(true)
        }
    }
}

fn file_for_scope<'a>(
    scope: MemoryScope,
    global: &'a mut MemoryFile,
    project: &'a mut MemoryFile,
) -> &'a mut MemoryFile {
    match scope {
        MemoryScope::Global => global,
        MemoryScope::Project => project,
    }
}

fn find_entry_mut<'a>(
    id: &str,
    global: &'a mut MemoryFile,
    project: &'a mut MemoryFile,
) -> Option<&'a mut MemoryEntry> {
    global
        .entries
        .iter_mut()
        .chain(project.entries.iter_mut())
        .find(|entry| entry.id == id)
}

fn required_text(value: &Option<String>, field: &str) -> Result<String, String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("Dream operation requires {field}"))
}

fn validate_name(value: String) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 96 {
        return Err("Dream memory name must contain 1..=96 characters".to_string());
    }
    Ok(value.to_string())
}

fn validate_description(value: String) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 500 {
        return Err("Dream memory description must contain 1..=500 characters".to_string());
    }
    validate_content(value)?;
    Ok(value.to_string())
}

fn response_text(output: &[OutputItem]) -> Result<String, String> {
    for item in output {
        if let OutputItem::Message(message) = item {
            let text = message
                .content
                .iter()
                .filter_map(|content| match content {
                    OutputMessageContent::OutputText(text) => Some(text.text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            if !text.trim().is_empty() {
                return Ok(text);
            }
        }
    }
    Err("Dream model returned no text".to_string())
}

fn strip_json_fence(text: &str) -> &str {
    let text = text.trim();
    text.strip_prefix("```json")
        .or_else(|| text.strip_prefix("```"))
        .unwrap_or(text)
        .strip_suffix("```")
        .unwrap_or(text)
        .trim()
}

fn tail_chars(value: &str, max: usize) -> String {
    let count = value.chars().count();
    if count <= max {
        value.to_string()
    } else {
        value.chars().skip(count - max).collect()
    }
}

fn atomic_write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("create {}: {error}", parent.display()))?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("dream"),
        uuid::Uuid::new_v4().simple()
    ));
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .map_err(|error| format!("create {}: {error}", temporary.display()))?;
        file.write_all(&bytes)
            .map_err(|error| format!("write {}: {error}", temporary.display()))?;
        file.sync_all()
            .map_err(|error| format!("sync {}: {error}", temporary.display()))?;
        std::fs::rename(&temporary, path).map_err(|error| {
            format!(
                "replace {} with {}: {error}",
                path.display(),
                temporary.display()
            )
        })?;
        #[cfg(unix)]
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("sync {}: {error}", parent.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn manager() -> MemoryManager {
        let root =
            std::env::temp_dir().join(format!("programmer-dream-test-{}", uuid::Uuid::new_v4()));
        MemoryManager::new(root.join("config"), root.join("workspace"))
    }

    pub(super) fn create_operation(content: &str) -> DreamOperation {
        DreamOperation {
            action: DreamAction::Create,
            id: None,
            scope: Some(MemoryScope::Project),
            kind: Some(MemoryKind::Decision),
            name: Some("chosen-storage-format".into()),
            description: Some("The project stores sessions as JSON.".into()),
            content: Some(content.into()),
            tags: Some(vec!["session".into()]),
            related_memories: None,
            confidence: Some(MemoryConfidence::Confirmed),
            supersedes: None,
        }
    }

    pub(super) fn plan(operations: Vec<DreamOperation>) -> DreamPlan {
        DreamPlan {
            schema_version: DREAM_SCHEMA_VERSION,
            run_id: new_run_id(),
            generated_at: now_secs(),
            policy: DreamPolicy::Preview,
            source_pending_ids: Vec::new(),
            operations,
        }
    }

    fn status_operation(id: &str, action: DreamAction) -> DreamOperation {
        DreamOperation {
            action,
            id: Some(id.to_string()),
            scope: None,
            kind: None,
            name: None,
            description: None,
            content: None,
            tags: None,
            related_memories: None,
            confidence: None,
            supersedes: None,
        }
    }

    #[test]
    fn enqueue_remains_available_while_dream_holds_the_memory_lock() {
        let manager = manager();
        let _lock = manager.acquire_dream_lock().unwrap().unwrap();
        let pending = manager
            .enqueue_dream("another-session", "Keep regression tests")
            .unwrap();
        assert_eq!(manager.load_pending().unwrap()[0].1.id, pending.id);
    }

    #[test]
    fn pending_dreams_are_deduped_by_content() {
        let manager = manager();
        let first = manager
            .enqueue_dream("session-1", "User: Always run cargo test")
            .unwrap();
        let again = manager
            .enqueue_dream("session-1", "User: Always run cargo test")
            .unwrap();
        assert_eq!(first.id, again.id);
        assert_eq!(manager.load_pending().unwrap().len(), 1);

        manager
            .enqueue_dream("session-1", "User: Always run cargo fmt")
            .unwrap();
        assert_eq!(manager.load_pending().unwrap().len(), 2);
    }

    #[test]
    fn pending_dreams_reject_credentials_and_empty_transcripts() {
        let manager = manager();
        assert!(manager.enqueue_dream("session-1", "   ").is_err());
        assert!(
            manager
                .enqueue_dream("session-1", "api_key=super-secret-value")
                .is_err()
        );
    }

    #[test]
    fn applying_plan_creates_and_supersedes_memory() {
        let manager = manager();
        assert_eq!(
            manager
                .apply_plan(
                    &plan(vec![create_operation("Use JSON for session persistence")]),
                    None
                )
                .unwrap(),
            1
        );
        let entry = manager.list(Some(MemoryScope::Project)).unwrap().remove(0);
        assert_eq!(entry.name, "chosen-storage-format");
        assert_eq!(entry.description, "The project stores sessions as JSON.");
        assert_eq!(entry.source, MemorySource::Dream);

        assert_eq!(
            manager
                .apply_plan(
                    &plan(vec![status_operation(&entry.id, DreamAction::Supersede)]),
                    None
                )
                .unwrap(),
            1
        );
        // Superseded memories leave the active set but stay on disk.
        assert!(manager.list(Some(MemoryScope::Project)).unwrap().is_empty());
        assert_eq!(manager.load(MemoryScope::Project).unwrap().entries.len(), 1);
        assert_eq!(
            manager.load(MemoryScope::Project).unwrap().entries[0].status,
            MemoryStatus::Superseded
        );
    }

    #[test]
    fn duplicate_create_is_idempotent() {
        let manager = manager();
        let plan = plan(vec![create_operation("Use JSON for session persistence")]);
        assert_eq!(manager.apply_plan(&plan, None).unwrap(), 1);
        assert!(
            manager
                .apply_plan(&plan, None)
                .unwrap_err()
                .contains("already been applied")
        );
        let mut duplicate = plan.clone();
        duplicate.run_id = new_run_id();
        assert_eq!(manager.apply_plan(&duplicate, None).unwrap(), 0);
    }

    #[test]
    fn scope_filter_rejects_other_scope() {
        let manager = manager();
        let plan = plan(vec![create_operation("Use JSON for session persistence")]);
        assert_eq!(
            manager
                .apply_plan(&plan, Some(MemoryScope::Global))
                .unwrap(),
            0
        );
    }

    #[test]
    fn operation_cap_and_content_validation_are_enforced() {
        let manager = manager();
        let too_many = plan(
            (0..MAX_OPERATIONS + 1)
                .map(|index| create_operation(&format!("Fact number {index}")))
                .collect(),
        );
        assert!(manager.apply_plan(&too_many, None).is_err());

        let mut credential = create_operation("Use JSON for session persistence");
        credential.content = Some("api_key=super-secret-value".into());
        assert!(manager.apply_plan(&plan(vec![credential]), None).is_err());

        let mut empty_name = create_operation("Use JSON for session persistence");
        empty_name.name = Some("  ".into());
        assert!(manager.apply_plan(&plan(vec![empty_name]), None).is_err());
    }

    #[test]
    fn automatic_policy_never_retires_memory_without_review() {
        // A create the model only inferred is not automatic material...
        let mut inferred = create_operation("Use JSON for session persistence");
        inferred.confidence = Some(MemoryConfidence::Inferred);
        assert!(!automatic_operation_allowed(&inferred));
        // ...and retiring an existing memory always requires a human preview.
        assert!(!automatic_operation_allowed(&status_operation(
            "mem_x",
            DreamAction::Archive
        )));
        assert!(!automatic_operation_allowed(&status_operation(
            "mem_x",
            DreamAction::Supersede
        )));
        assert!(automatic_operation_allowed(&create_operation(
            "explicit fact"
        )));
    }

    #[test]
    fn automatic_plan_cannot_archive_through_a_tampered_preview() {
        let manager = manager();
        manager
            .remember(
                MemoryScope::Project,
                MemoryKind::Decision,
                "Use JSON for session persistence".into(),
                Vec::new(),
            )
            .unwrap();
        let id = manager.list(None).unwrap()[0].id.clone();
        let mut automatic = plan(vec![status_operation(&id, DreamAction::Archive)]);
        automatic.policy = DreamPolicy::Automatic;
        assert_eq!(manager.apply_plan(&automatic, None).unwrap(), 0);
        assert_eq!(manager.list(None).unwrap().len(), 1);
    }

    #[test]
    fn apply_without_a_preview_explains_itself() {
        let manager = manager();
        let error = apply_saved_preview(&manager, None).unwrap_err();
        assert!(error.contains("no Dream preview"));
    }

    #[test]
    fn applying_a_preview_consumes_the_queue_and_clears_the_preview() {
        let manager = manager();
        manager
            .enqueue_dream("session-1", "User: always run cargo test")
            .unwrap();
        let pending = manager.load_pending().unwrap();
        let mut preview = plan(vec![create_operation("Use JSON for session persistence")]);
        preview.source_pending_ids = vec![pending[0].1.id.clone()];
        atomic_write_json(&manager.dream_dir().join(PREVIEW_FILE), &preview).unwrap();

        let report = apply_saved_preview(&manager, None).unwrap();
        assert_eq!(report.operations, 1);
        assert!(report.applied);
        assert!(manager.load_pending().unwrap().is_empty());
        assert!(!manager.dream_dir().join(PREVIEW_FILE).exists());
        assert_eq!(manager.load_dream_state().unwrap().last_operation_count, 1);
        assert!(manager.dream_dir().join("processed").exists());
    }

    #[test]
    fn dream_status_uses_readable_time_without_changing_other_fields() {
        let now = 1_790_847_206;
        for (timestamp, expected) in [
            (None, "never"),
            (Some(now), "just now"),
            (Some(now - 59), "just now"),
            (Some(now - 60), "1m ago"),
            (Some(now - 3599), "59m ago"),
            (Some(now - 3600), "1h 0m ago"),
            (Some(now - 12000), "3h 20m ago"),
            (Some(now - 86400), "1d 0h ago"),
            (Some(now - 176400), "2d 1h ago"),
            (Some(now + 1), "unknown (clock mismatch)"),
            (Some(u64::MAX), "unknown (clock mismatch)"),
        ] {
            let state = DreamState {
                last_dream_at: timestamp,
                last_operation_count: 7,
                last_error: Some("Dream timed out after 60s".into()),
                ..Default::default()
            };
            assert_eq!(
                render_status_at(&state, 17, false, now),
                format!(
                    "Dream status: queued=17 transcript(s), preview=none, last_run={expected}, last_operations=7, last_error=Dream timed out after 60s"
                )
            );
            assert_eq!(state.last_dream_at, timestamp);
        }
        assert!(
            render_status_at(&DreamState::default(), 0, true, now)
                .contains("preview=ready, last_run=never, last_operations=0, last_error=none")
        );
    }

    #[test]
    fn dream_state_survives_a_restart() {
        let manager = manager();
        let state = DreamState {
            schema_version: DREAM_SCHEMA_VERSION,
            last_dream_at: Some(now_secs()),
            last_processed_session_count: 3,
            last_error: Some("provider offline".into()),
            last_operation_count: 2,
        };
        manager.save_dream_state(&state).unwrap();
        let reloaded = manager.load_dream_state().unwrap();
        assert_eq!(reloaded.last_processed_session_count, 3);
        assert_eq!(reloaded.last_operation_count, 2);
        assert_eq!(reloaded.last_error.as_deref(), Some("provider offline"));

        let (_, pending, preview) = manager.dream_status().unwrap();
        assert_eq!(pending, 0);
        assert!(!preview);
    }

    #[tokio::test]
    async fn the_worker_picks_up_queued_sessions_and_keeps_them_when_the_provider_fails() {
        let manager = manager();
        manager
            .enqueue_dream("session-1", "User: always run cargo test")
            .unwrap();

        let config = DreamConfig {
            enabled: true,
            min_sessions: 1,
            min_interval_hours: 0,
            // A refused connection must fail fast so the test stays quick; the
            // behaviour under test is what happens to the queue afterwards.
            timeout_secs: 1,
        };
        let runtime = std::sync::Arc::new(std::sync::Mutex::new(DreamRuntime {
            config,
            model: Some(DreamModel {
                client: Client::with_config(
                    OpenAIConfig::new()
                        .with_api_base("http://127.0.0.1:1/v1")
                        .with_api_key("test"),
                ),
                model: "test-model".to_string(),
            }),
        }));
        let active = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut worker = Some(DreamWorker::start(manager.clone(), runtime, active.clone()));

        // The worker runs the first pass immediately; wait for it to record its
        // failure rather than sleeping a fixed interval.
        let mut state = manager.load_dream_state().unwrap();
        for _ in 0..100 {
            state = manager.load_dream_state().unwrap();
            if state.last_error.is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        shutdown(&mut worker);

        // A failed pass reports why and keeps every queued session.
        assert!(
            state.last_error.is_some(),
            "the provider failure must be recorded in the Dream state"
        );
        assert_eq!(manager.load_pending().unwrap().len(), 1);
        assert_eq!(manager.list(None).unwrap().len(), 0);
        // The UI indicator must be down once the worker is stopped, even though
        // the pass failed.
        assert!(!active.load(std::sync::atomic::Ordering::Relaxed));
    }

    #[tokio::test]
    async fn planning_connection_error_preserves_causes_in_audit_and_status() {
        let manager = manager();
        manager
            .enqueue_dream("session-1", "User: run cargo test")
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let model = DreamModel {
            client: Client::with_config(
                OpenAIConfig::new()
                    .with_api_base(format!("http://{address}/v1"))
                    .with_api_key("test-secret-key"),
            )
            .with_http_client(reqwest::Client::builder().no_proxy().build().unwrap()),
            model: "test-model".into(),
        };
        let configuration = DreamConfig {
            min_sessions: 1,
            min_interval_hours: 0,
            // Windows connection refusal and provider retries can exceed three
            // seconds. Let the transport error win, not Dream's outer timeout.
            timeout_secs: 30,
            ..Default::default()
        };
        let error = model.run_auto(&manager, &configuration).await.unwrap_err();
        assert!(error.contains("HTTP connection failed"), "{error}");
        assert!(error.contains("caused by:"), "{error}");
        assert!(!error.contains("test-secret-key"));
        let history = manager.dream_history_list().unwrap();
        assert_eq!(history[0].error.as_deref(), Some(error.as_str()));
        let state = manager.load_dream_state().unwrap();
        assert_eq!(state.last_error.as_deref(), Some(error.as_str()));
        assert!(render_status(&state, 1, false).contains(&error));
        assert_eq!(manager.load_pending().unwrap().len(), 1);
        assert!(manager.list(None).unwrap().is_empty());
    }

    #[tokio::test]
    async fn planning_client_timeout_is_distinct_and_omits_request_url_secrets() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (_connection, _) = listener.accept().await.unwrap();
            std::future::pending::<()>().await;
        });
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_millis(100))
            .build()
            .unwrap();
        let error = client
            .get(format!("http://{address}/?api_key=private-query-value"))
            .send()
            .await
            .unwrap_err();
        server.abort();
        let message = format_planning_error(async_openai::error::OpenAIError::Reqwest(error));
        assert!(message.contains("HTTP client timeout"), "{message}");
        assert!(message.contains("caused by:"), "{message}");
        assert!(!message.contains("private-query-value"), "{message}");
        assert!(!message.contains("http://"), "{message}");
    }

    #[tokio::test]
    async fn manual_preview_cancels_a_stalled_provider_and_releases_the_store() {
        let manager = manager();
        manager
            .enqueue_dream("session-1", "User: always run cargo test")
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let (connected, connection) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.unwrap();
            let _ = connected.send(());
            std::future::pending::<()>().await;
        });
        let model = DreamModel {
            client: Client::with_config(
                OpenAIConfig::new().with_api_base(base).with_api_key("test"),
            ),
            model: "test-model".into(),
        };
        let configuration = DreamConfig {
            enabled: true,
            min_sessions: 1,
            min_interval_hours: 0,
            timeout_secs: 60,
        };
        let cancellation = crate::cancel::CancellationToken::new();
        let task_manager = manager.clone();
        let task_cancellation = cancellation.clone();
        let task = tokio::spawn(async move {
            model
                .preview(&task_manager, &configuration, None, task_cancellation)
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(3), connection)
            .await
            .unwrap()
            .unwrap();
        assert!(manager.list(None).unwrap_err().contains("busy"));
        cancellation.cancel();
        let result = tokio::time::timeout(std::time::Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap();
        assert!(result.unwrap_err().contains("cancelled"));
        assert!(manager.list(None).unwrap().is_empty());
        assert_eq!(manager.load_pending().unwrap().len(), 1);
        assert!(!manager.dream_dir().join(PREVIEW_FILE).exists());
        let history = manager.dream_history_list().unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].status, DreamRunStatus::GenerationFailed);
        assert!(history[0].error.as_ref().unwrap().contains("cancelled"));
        server.abort();
    }

    #[tokio::test]
    async fn the_worker_raises_the_indicator_while_a_pass_is_in_flight() {
        let manager = manager();
        manager
            .enqueue_dream("session-1", "User: always run cargo test")
            .unwrap();

        // A provider that accepts the connection and then says nothing, so the
        // pass stays in flight long enough to observe the indicator.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((stream, _)) = listener.accept().await {
                held.push(stream);
            }
        });

        let runtime = std::sync::Arc::new(std::sync::Mutex::new(DreamRuntime {
            config: DreamConfig {
                enabled: true,
                min_sessions: 1,
                min_interval_hours: 0,
                timeout_secs: 60,
            },
            model: Some(DreamModel {
                client: Client::with_config(
                    OpenAIConfig::new().with_api_base(base).with_api_key("test"),
                ),
                model: "test-model".to_string(),
            }),
        }));
        let active = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut worker = Some(DreamWorker::start(manager.clone(), runtime, active.clone()));

        let mut raised = false;
        for _ in 0..100 {
            if active.load(std::sync::atomic::Ordering::Relaxed) {
                raised = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(raised, "the indicator must be up during a pass");
        // The in-flight model call owns the store lock; reads fail closed.
        assert!(manager.list(None).unwrap_err().contains("busy"));
        assert!(
            manager
                .dream_history_list()
                .unwrap()
                .iter()
                .any(|history| history.status == DreamRunStatus::Generating)
        );

        shutdown(&mut worker);
        assert!(!active.load(std::sync::atomic::Ordering::Relaxed));
        server.abort();
    }

    #[test]
    fn a_held_lock_prevents_a_second_pass() {
        let manager = manager();
        let held = manager.acquire_dream_lock().unwrap().expect("first lock");
        // A second consolidation must not start while the first still runs; the
        // pending queue stays untouched for the next attempt.
        assert!(manager.acquire_dream_lock().unwrap().is_none());
        drop(held);
        assert!(manager.acquire_dream_lock().unwrap().is_some());
    }
}
