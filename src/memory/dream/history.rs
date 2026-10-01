//! Durable audit records and a roll-forward journal. The root-wide writer lock
//! covers ordinary memory edits as well as Dream. An interrupted transaction
//! blocks reads/writes until explicit recovery completes its recorded target.
use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DreamRunStatus {
    Generating,
    Preview,
    ApplyFailed,
    Incomplete,
    GenerationFailed,
    Applied,
    RolledBack,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DreamHistory {
    pub(crate) schema_version: u32,
    pub(crate) run_id: String,
    pub(crate) created_at: u64,
    pub(crate) status: DreamRunStatus,
    pub(crate) sources: Vec<PendingDream>,
    pub(crate) plan: Option<DreamPlan>,
    /// One result per operation, including policy skips and no-ops.
    pub(crate) outcomes: Vec<String>,
    pub(crate) before: Vec<MemoryEntry>,
    pub(crate) after: Vec<MemoryEntry>,
    pub(crate) error: Option<String>,
    /// Independent rollback audit ID; the original snapshots remain immutable.
    #[serde(default)]
    pub(crate) rollback_run_id: Option<String>,
    #[serde(default)]
    pub(crate) original_run_id: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct DreamRollbackPreview {
    pub(crate) run_id: String,
    pub(crate) affected_ids: Vec<String>,
    pub(crate) conflicts: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct Journal {
    schema_version: u32,
    project_path: PathBuf,
    global: MemoryFile,
    project: MemoryFile,
    history: DreamHistory,
    consume_queue: bool,
    #[serde(default)]
    original_history: Option<DreamHistory>,
}

impl MemoryManager {
    fn journal_path(&self) -> PathBuf {
        self.global_path
            .parent()
            .expect("memory root")
            .join(".dream-transaction.json")
    }

    /// Only the store root is trusted. Journal data never selects a write path.
    pub(in crate::memory) fn validate_storage_path(&self, path: &Path) -> Result<(), String> {
        let root = self.global_path.parent().expect("memory root");
        let relative = path
            .strip_prefix(root)
            .map_err(|_| "path outside memory root")?;
        let mut current = root.to_path_buf();
        for component in std::iter::once(None).chain(relative.components().map(Some)) {
            if let Some(component) = component {
                let std::path::Component::Normal(name) = component else {
                    return Err("invalid memory storage path".into());
                };
                current.push(name);
            }
            match std::fs::symlink_metadata(&current) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(format!(
                        "symlink is not allowed in memory storage: {}",
                        current.display()
                    ));
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("inspect {}: {error}", current.display())),
            }
        }
        Ok(())
    }

    fn validate_history(&self, history: &DreamHistory) -> Result<(), String> {
        if history.schema_version != DREAM_SCHEMA_VERSION
            || !safe_id(&history.run_id)
            || history
                .rollback_run_id
                .as_ref()
                .is_some_and(|id| !safe_id(id))
            || history
                .original_run_id
                .as_ref()
                .is_some_and(|id| !safe_id(id))
        {
            return Err("invalid Dream history schema or ID".into());
        }
        for source in &history.sources {
            if source.schema_version != DREAM_SCHEMA_VERSION || !safe_id(&source.id) {
                return Err("invalid Dream history source".into());
            }
        }
        for entries in [&history.before, &history.after] {
            validate_entry_ids(entries)?;
        }
        Ok(())
    }

    fn pending_journal(&self) -> Result<Option<Journal>, String> {
        self.validate_storage_path(&self.journal_path())?;
        if !self.journal_path().exists() {
            return Ok(None);
        }
        let journal: Journal = read_json(&self.journal_path())?;
        self.validate_journal(&journal)?;
        Ok(Some(journal))
    }

    fn validate_journal(&self, journal: &Journal) -> Result<(), String> {
        if journal.schema_version != DREAM_SCHEMA_VERSION
            || journal.project_path != self.project_path
        {
            return Err("Dream transaction belongs to another workspace or schema; run `/memory dream recover` in the originating workspace".into());
        }
        self.validate_storage_path(&self.journal_path())?;
        self.validate_history(&journal.history)?;
        self.validate_storage_path(&self.history_path(&journal.history.run_id)?)?;
        self.validate_memory_file(MemoryScope::Global, &journal.global)?;
        self.validate_memory_file(MemoryScope::Project, &journal.project)?;
        let target = combined(&journal.global, &journal.project);
        validate_entry_ids(&target)?;
        if serde_json::to_value(&target).map_err(|error| error.to_string())?
            != serde_json::to_value(&journal.history.after).map_err(|error| error.to_string())?
        {
            return Err("Dream journal target does not match audit snapshot".into());
        }
        if journal.consume_queue {
            let plan = journal
                .history
                .plan
                .as_ref()
                .ok_or("Dream journal has no plan")?;
            validate_plan(plan)?;
            if journal.history.status != DreamRunStatus::Applied
                || plan.run_id != journal.history.run_id
                || journal.original_history.is_some()
            {
                return Err("invalid Dream apply journal".into());
            }
            self.validate_storage_path(&self.dream_dir().join("processed"))?;
            self.validate_storage_path(&self.pending_dir())?;
            for id in &plan.source_pending_ids {
                self.validate_storage_path(&self.pending_dir().join(format!("{id}.json")))?;
                self.validate_storage_path(
                    &self
                        .dream_dir()
                        .join("processed")
                        .join(format!("{id}.json")),
                )?;
            }
            self.validate_storage_path(&self.dream_dir().join(STATE_FILE))?;
            self.validate_storage_path(&self.dream_dir().join(PREVIEW_FILE))?;
        } else {
            let original = journal
                .original_history
                .as_ref()
                .ok_or("rollback journal has no original audit")?;
            self.validate_history(original)?;
            self.validate_storage_path(&self.history_path(&original.run_id)?)?;
            if journal.history.status != DreamRunStatus::RolledBack
                || original.status != DreamRunStatus::Applied
                || original.rollback_run_id.as_deref() != Some(&journal.history.run_id)
                || journal.history.original_run_id.as_deref() != Some(&original.run_id)
                || original.run_id == journal.history.run_id
            {
                return Err("invalid Dream rollback linkage".into());
            }
        }
        Ok(())
    }

    pub(in crate::memory) fn ensure_no_dream_transaction(&self) -> Result<(), String> {
        if std::fs::symlink_metadata(self.journal_path()).is_ok() {
            return Err(
                "incomplete Dream transaction; run `/memory dream recover` in the originating workspace before accessing memory".into(),
            );
        }
        Ok(())
    }

    pub(in crate::memory) fn acquire_memory_lock(&self) -> Result<File, String> {
        self.acquire_dream_lock()?
            .ok_or_else(|| "memory store is busy; retry after the current writer finishes".into())
    }

    fn history_path(&self, run_id: &str) -> Result<PathBuf, String> {
        if !safe_id(run_id) {
            return Err("invalid Dream run ID".into());
        }
        Ok(self
            .dream_dir()
            .join("history")
            .join(format!("{run_id}.json")))
    }

    fn save_history(&self, history: &DreamHistory) -> Result<(), String> {
        self.validate_history(history)?;
        let path = self.history_path(&history.run_id)?;
        self.validate_storage_path(&path)?;
        atomic_write_json(&path, history)
    }

    pub(crate) fn dream_history_detail(&self, run_id: &str) -> Result<DreamHistory, String> {
        let path = self.history_path(run_id)?;
        if let Some(journal) = self.pending_journal()?
            && journal.history.run_id == run_id
        {
            let mut history = journal.history;
            history.status = DreamRunStatus::Incomplete;
            history.error = Some("Transaction incomplete; run `/memory dream recover`".into());
            return Ok(history);
        }
        self.validate_storage_path(&path)?;
        let history: DreamHistory = read_json(&path)?;
        self.validate_history(&history)?;
        if history.run_id != run_id {
            return Err("unsupported or mismatched Dream history".into());
        }
        Ok(history)
    }

    pub(crate) fn dream_history_list(&self) -> Result<Vec<DreamHistory>, String> {
        let journal = self.pending_journal()?;
        let directory = self.dream_dir().join("history");
        self.validate_storage_path(&directory)?;
        let files = match std::fs::read_dir(directory) {
            Ok(files) => files,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return match journal {
                    Some(journal) => Ok(vec![self.dream_history_detail(&journal.history.run_id)?]),
                    None => Ok(Vec::new()),
                };
            }
            Err(error) => return Err(error.to_string()),
        };
        let mut history = Vec::new();
        for file in files {
            let path = file.map_err(|error| error.to_string())?.path();
            if path
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                let id = path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .ok_or("invalid history filename")?;
                history.push(self.dream_history_detail(id)?);
            }
        }
        if let Some(journal) = journal
            && !history
                .iter()
                .any(|record| record.run_id == journal.history.run_id)
        {
            history.push(self.dream_history_detail(&journal.history.run_id)?);
        }
        history.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then_with(|| right.run_id.cmp(&left.run_id))
        });
        Ok(history)
    }

    pub(super) fn record_generation_start(
        &self,
        pending: &[(PathBuf, PendingDream)],
    ) -> Result<String, String> {
        let run_id = new_run_id();
        self.save_history(&DreamHistory {
            schema_version: DREAM_SCHEMA_VERSION,
            run_id: run_id.clone(),
            created_at: now_secs(),
            status: DreamRunStatus::Generating,
            sources: pending.iter().map(|(_, source)| source.clone()).collect(),
            plan: None,
            outcomes: Vec::new(),
            before: Vec::new(),
            after: Vec::new(),
            error: None,
            rollback_run_id: None,
            original_run_id: None,
        })?;
        Ok(run_id)
    }

    pub(super) fn record_preview(
        &self,
        plan: &DreamPlan,
        pending: &[(PathBuf, PendingDream)],
    ) -> Result<(), String> {
        self.save_history(&DreamHistory {
            schema_version: DREAM_SCHEMA_VERSION,
            run_id: plan.run_id.clone(),
            created_at: plan.generated_at,
            status: DreamRunStatus::Preview,
            sources: pending.iter().map(|(_, source)| source.clone()).collect(),
            plan: Some(plan.clone()),
            outcomes: Vec::new(),
            before: Vec::new(),
            after: Vec::new(),
            error: None,
            rollback_run_id: None,
            original_run_id: None,
        })
    }

    pub(super) fn record_generation_failure(
        &self,
        run_id: &str,
        pending: &[(PathBuf, PendingDream)],
        error: &str,
    ) -> Result<(), String> {
        self.save_history(&DreamHistory {
            schema_version: DREAM_SCHEMA_VERSION,
            run_id: run_id.into(),
            created_at: now_secs(),
            status: DreamRunStatus::GenerationFailed,
            sources: pending.iter().map(|(_, source)| source.clone()).collect(),
            plan: None,
            outcomes: Vec::new(),
            before: Vec::new(),
            after: Vec::new(),
            error: Some(error.into()),
            rollback_run_id: None,
            original_run_id: None,
        })
    }

    pub(super) fn apply_plan_with_history(
        &self,
        plan: &DreamPlan,
        scope: Option<MemoryScope>,
    ) -> Result<usize, String> {
        let result = self.apply_validated_plan(plan, scope);
        if let Err(error) = &result
            && !self.journal_path().exists()
            && safe_id(&plan.run_id)
        {
            let history = DreamHistory {
                schema_version: DREAM_SCHEMA_VERSION,
                run_id: new_run_id(),
                created_at: now_secs(),
                status: DreamRunStatus::ApplyFailed,
                sources: Vec::new(),
                plan: Some(plan.clone()),
                outcomes: Vec::new(),
                before: Vec::new(),
                after: Vec::new(),
                error: Some(error.clone()),
                rollback_run_id: None,
                original_run_id: Some(plan.run_id.clone()),
            };
            self.save_history(&history).map_err(|audit_error| {
                format!("{error}; also failed to record apply failure: {audit_error}")
            })?;
        }
        result
    }

    fn apply_validated_plan(
        &self,
        plan: &DreamPlan,
        scope: Option<MemoryScope>,
    ) -> Result<usize, String> {
        self.ensure_no_dream_transaction()?;
        validate_plan(plan)?;
        let path = self.history_path(&plan.run_id)?;
        let previous = if path.exists() {
            Some(self.dream_history_detail(&plan.run_id)?)
        } else {
            None
        };
        if previous.as_ref().is_some_and(|history| {
            !matches!(
                history.status,
                DreamRunStatus::Preview | DreamRunStatus::Generating
            )
        }) {
            return Err("Dream run has already been applied; it cannot be replayed".into());
        }
        let mut global = self.load(MemoryScope::Global)?;
        let mut project = self.load(MemoryScope::Project)?;
        let before = combined(&global, &project);
        let mut outcomes = Vec::new();
        let mut applied = 0;
        for operation in &plan.operations {
            if plan.policy == DreamPolicy::Automatic && !automatic_operation_allowed(operation) {
                outcomes.push("skipped: automatic policy".into());
                continue;
            }
            if apply_operation(operation, scope, &mut global, &mut project)? {
                applied += 1;
                outcomes.push("applied".into());
            } else {
                outcomes.push("no-op: scope, duplicate, or missing target".into());
            }
        }
        let sources = match previous {
            Some(history) => history.sources,
            None => self
                .load_pending()?
                .into_iter()
                .map(|(_, source)| source)
                .filter(|source| plan.source_pending_ids.contains(&source.id))
                .collect(),
        };
        let history = DreamHistory {
            schema_version: DREAM_SCHEMA_VERSION,
            run_id: plan.run_id.clone(),
            created_at: plan.generated_at,
            status: DreamRunStatus::Applied,
            sources,
            plan: Some(plan.clone()),
            outcomes,
            before,
            after: combined(&global, &project),
            error: None,
            rollback_run_id: None,
            original_run_id: None,
        };
        self.commit_dream_transaction(Journal {
            schema_version: DREAM_SCHEMA_VERSION,
            project_path: self.project_path.clone(),
            global,
            project,
            history,
            consume_queue: true,
            original_history: None,
        })?;
        Ok(applied)
    }

    fn commit_dream_transaction(&self, journal: Journal) -> Result<(), String> {
        self.ensure_no_dream_transaction()?;
        self.validate_journal(&journal)?;
        atomic_write_json(&self.journal_path(), &journal)
            .and_then(|()| self.finish_dream_transaction(&journal))
            .map_err(|error| {
                format!("{error}; transaction incomplete; run `/memory dream recover`")
            })
    }

    fn finish_dream_transaction(&self, journal: &Journal) -> Result<(), String> {
        self.validate_journal(journal)?;
        self.save(MemoryScope::Global, &journal.global)?;
        self.save(MemoryScope::Project, &journal.project)?;
        self.save_history(&journal.history)?;
        if let Some(original) = &journal.original_history {
            self.save_history(original)?;
        }
        if journal.consume_queue {
            let plan = journal.history.plan.as_ref().ok_or("journal has no plan")?;
            let mut state = self.load_dream_state()?;
            let applied = journal
                .history
                .outcomes
                .iter()
                .filter(|outcome| outcome.as_str() == "applied")
                .count();
            finish_applied_plan(self, plan, &mut state, applied)?;
        }
        std::fs::remove_file(self.journal_path()).map_err(|error| error.to_string())?;
        sync_directory(self.journal_path().parent().expect("memory root"))
    }

    /// Explicit roll-forward, never a force rollback. Other projects cannot
    /// recover this project's journal, but all share its fail-closed barrier.
    pub(crate) fn dream_recover(&self) -> Result<(), String> {
        let _lock = self
            .acquire_transaction_lock()?
            .ok_or("memory store is busy")?;
        let journal = self
            .pending_journal()?
            .ok_or("no incomplete Dream transaction")?;
        self.finish_dream_transaction(&journal)
    }

    pub(crate) fn dream_rollback_preview(
        &self,
        run_id: &str,
    ) -> Result<DreamRollbackPreview, String> {
        let _lock = self.acquire_memory_lock()?;
        let history = self.dream_history_detail(run_id)?;
        let current = combined(
            &self.load(MemoryScope::Global)?,
            &self.load(MemoryScope::Project)?,
        );
        rollback_preview(&history, &current)
    }

    pub(crate) fn dream_rollback(&self, run_id: &str) -> Result<DreamRollbackPreview, String> {
        let _lock = self.acquire_memory_lock()?;
        let mut history = self.dream_history_detail(run_id)?;
        let mut global = self.load(MemoryScope::Global)?;
        let mut project = self.load(MemoryScope::Project)?;
        let current = combined(&global, &project);
        let preview = rollback_preview(&history, &current)?;
        if !preview.conflicts.is_empty() {
            return Err(format!(
                "Dream rollback conflicts: {}",
                preview.conflicts.join(", ")
            ));
        }
        for id in &preview.affected_ids {
            let existing = current.iter().find(|entry| &entry.id == id);
            global.entries.retain(|entry| &entry.id != id);
            project.entries.retain(|entry| &entry.id != id);
            if let Some(before) = history.before.iter().find(|entry| &entry.id == id) {
                let mut restored = before.clone();
                if let Some(existing) = existing {
                    restored.use_count = existing.use_count;
                    restored.last_used_at = existing.last_used_at;
                }
                file_for_scope(restored.scope, &mut global, &mut project)
                    .entries
                    .push(restored);
            }
        }
        let rollback_id = new_run_id();
        history.rollback_run_id = Some(rollback_id.clone());
        let rollback = DreamHistory {
            schema_version: DREAM_SCHEMA_VERSION,
            run_id: rollback_id,
            created_at: now_secs(),
            status: DreamRunStatus::RolledBack,
            sources: history.sources.clone(),
            plan: None,
            outcomes: vec![format!("Rolled back Dream run {}", history.run_id)],
            before: current,
            after: combined(&global, &project),
            error: None,
            rollback_run_id: None,
            original_run_id: Some(history.run_id.clone()),
        };
        self.commit_dream_transaction(Journal {
            schema_version: DREAM_SCHEMA_VERSION,
            project_path: self.project_path.clone(),
            global,
            project,
            history: rollback,
            consume_queue: false,
            original_history: Some(history),
        })?;
        Ok(preview)
    }
}

fn validate_plan(plan: &DreamPlan) -> Result<(), String> {
    if plan.schema_version != DREAM_SCHEMA_VERSION
        || !safe_id(&plan.run_id)
        || plan.operations.len() > MAX_OPERATIONS
        || plan.source_pending_ids.iter().any(|id| !safe_id(id))
    {
        return Err("invalid Dream plan schema, IDs, or operation limit".into());
    }
    let unique: std::collections::HashSet<_> = plan.source_pending_ids.iter().collect();
    if unique.len() != plan.source_pending_ids.len() {
        return Err("duplicate Dream source ID".into());
    }
    Ok(())
}

pub(in crate::memory) fn validate_entry_ids(entries: &[MemoryEntry]) -> Result<(), String> {
    let mut ids = std::collections::HashSet::new();
    for entry in entries {
        if !safe_id(&entry.id) || !ids.insert(&entry.id) {
            return Err("invalid or duplicate memory ID".into());
        }
    }
    Ok(())
}

fn combined(global: &MemoryFile, project: &MemoryFile) -> Vec<MemoryEntry> {
    global
        .entries
        .iter()
        .chain(&project.entries)
        .cloned()
        .collect()
}

fn semantic(entry: Option<&MemoryEntry>) -> Option<serde_json::Value> {
    entry.map(|entry| {
        let mut entry = entry.clone();
        entry.use_count = 0;
        entry.last_used_at = None;
        // Timestamps do not represent semantic changes (including edit/revert).
        entry.updated_at = 0;
        serde_json::to_value(entry).expect("MemoryEntry serialization is infallible")
    })
}

fn rollback_preview(
    history: &DreamHistory,
    current: &[MemoryEntry],
) -> Result<DreamRollbackPreview, String> {
    if history.status != DreamRunStatus::Applied || history.rollback_run_id.is_some() {
        return Err("only an applied Dream run can be rolled back".into());
    }
    let mut affected_ids = Vec::new();
    let mut conflicts = Vec::new();
    for entry in history.before.iter().chain(&history.after) {
        if affected_ids.contains(&entry.id) {
            continue;
        }
        let before = history
            .before
            .iter()
            .find(|candidate| candidate.id == entry.id);
        let after = history
            .after
            .iter()
            .find(|candidate| candidate.id == entry.id);
        if semantic(before) == semantic(after) {
            continue;
        }
        affected_ids.push(entry.id.clone());
        let actual = current.iter().find(|candidate| candidate.id == entry.id);
        if semantic(actual) != semantic(after) {
            conflicts.push(entry.id.clone());
        }
    }
    Ok(DreamRollbackPreview {
        run_id: history.run_id.clone(),
        affected_ids,
        conflicts,
    })
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("parse {}: {error}", path.display()))
}

pub(super) fn sync_directory(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|error| format!("sync {}: {error}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::tests::{create_operation, manager, plan};
    use super::*;

    #[test]
    fn cross_scope_rollback_retains_audit_sources_and_never_requeues() {
        let manager = manager();
        let source = manager
            .enqueue_dream("session", "Use explicit error handling")
            .unwrap();
        let mut global = create_operation("Prefer explicit error handling");
        global.scope = Some(MemoryScope::Global);
        let mut plan = plan(vec![global, create_operation("Project uses JSON")]);
        plan.source_pending_ids.push(source.id.clone());
        manager.apply_plan(&plan, None).unwrap();
        let restarted = manager.clone();
        let history = restarted.dream_history_detail(&plan.run_id).unwrap();
        assert_eq!(history.sources[0].transcript, source.transcript);
        assert_eq!(history.after.len(), 2);
        assert_eq!(history.outcomes, ["applied", "applied"]);
        assert_eq!(
            restarted
                .dream_rollback_preview(&plan.run_id)
                .unwrap()
                .affected_ids
                .len(),
            2
        );
        restarted.dream_rollback(&plan.run_id).unwrap();
        assert!(restarted.list(None).unwrap().is_empty());
        assert_eq!(restarted.pending_count().unwrap(), 0);
        let original = restarted.dream_history_detail(&plan.run_id).unwrap();
        assert_eq!(original.status, DreamRunStatus::Applied);
        assert_eq!(
            serde_json::to_value(&original.before).unwrap(),
            serde_json::to_value(&history.before).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&original.after).unwrap(),
            serde_json::to_value(&history.after).unwrap()
        );
        let rollback = restarted
            .dream_history_detail(original.rollback_run_id.as_deref().unwrap())
            .unwrap();
        assert_eq!(rollback.status, DreamRunStatus::RolledBack);
        assert_eq!(
            rollback.original_run_id.as_deref(),
            Some(plan.run_id.as_str())
        );
        assert!(rollback.created_at >= original.created_at);
        assert_eq!(restarted.dream_history_list().unwrap().len(), 2);
        assert!(restarted.dream_rollback(&plan.run_id).is_err());
        assert!(restarted.apply_plan(&plan, None).is_err());
    }

    #[test]
    fn rollback_preserves_usage_and_unrelated_entries_but_rejects_semantic_conflicts() {
        let manager = manager();
        let original = manager
            .remember(
                MemoryScope::Project,
                MemoryKind::Convention,
                "Original convention".into(),
                vec![],
            )
            .unwrap();
        let mut update = create_operation("Changed convention");
        update.action = DreamAction::Update;
        update.id = Some(original.id.clone());
        let first = plan(vec![update.clone()]);
        manager.apply_plan(&first, None).unwrap();
        manager.touch(std::slice::from_ref(&original.id)).unwrap();
        let unrelated = manager
            .remember(
                MemoryScope::Global,
                MemoryKind::Convention,
                "Unrelated convention".into(),
                vec![],
            )
            .unwrap();
        manager.dream_rollback(&first.run_id).unwrap();
        let entries = manager.list(None).unwrap();
        let restored = entries
            .iter()
            .find(|entry| entry.id == original.id)
            .unwrap();
        assert_eq!(restored.content, original.content);
        assert_eq!(restored.use_count, 1);
        assert!(restored.last_used_at.is_some());
        assert!(entries.iter().any(|entry| entry.id == unrelated.id));
        let second = plan(vec![update, create_operation("Another Dream fact")]);
        manager.apply_plan(&second, None).unwrap();
        manager
            .update(&original.id, Some("User changed this".into()), None, None)
            .unwrap();
        let before = serde_json::to_value(manager.list(None).unwrap()).unwrap();
        assert_eq!(
            manager
                .dream_rollback_preview(&second.run_id)
                .unwrap()
                .conflicts,
            [original.id]
        );
        assert!(manager.dream_rollback(&second.run_id).is_err());
        assert_eq!(
            serde_json::to_value(manager.list(None).unwrap()).unwrap(),
            before
        );
    }

    #[test]
    fn interrupted_cross_scope_commit_blocks_all_writers_and_recovers() {
        let manager = manager();
        let mut global = create_operation("Global transaction fact");
        global.scope = Some(MemoryScope::Global);
        let plan = plan(vec![global, create_operation("Project transaction fact")]);
        let mut global = manager.load(MemoryScope::Global).unwrap();
        let mut project = manager.load(MemoryScope::Project).unwrap();
        for operation in &plan.operations {
            apply_operation(operation, None, &mut global, &mut project).unwrap();
        }
        let history = DreamHistory {
            schema_version: DREAM_SCHEMA_VERSION,
            run_id: plan.run_id.clone(),
            created_at: now_secs(),
            status: DreamRunStatus::Applied,
            sources: vec![],
            plan: Some(plan.clone()),
            outcomes: vec!["applied".into(), "applied".into()],
            before: vec![],
            after: combined(&global, &project),
            error: None,
            rollback_run_id: None,
            original_run_id: None,
        };
        let index = manager.directory(MemoryScope::Project).join("MEMORY.md");
        std::fs::create_dir_all(&index).unwrap();
        assert!(
            manager
                .commit_dream_transaction(Journal {
                    schema_version: DREAM_SCHEMA_VERSION,
                    project_path: manager.project_path.clone(),
                    global,
                    project,
                    history,
                    consume_queue: true,
                    original_history: None,
                })
                .is_err()
        );
        assert!(manager.journal_path().exists());
        let incomplete = manager.dream_history_list().unwrap();
        assert_eq!(incomplete.len(), 1);
        assert_eq!(incomplete[0].status, DreamRunStatus::Incomplete);
        assert!(
            incomplete[0]
                .error
                .as_deref()
                .unwrap()
                .contains("/memory dream recover")
        );
        assert!(manager.list(None).is_err());
        assert!(
            manager
                .remember(
                    MemoryScope::Global,
                    MemoryKind::Convention,
                    "Blocked".into(),
                    vec![]
                )
                .is_err()
        );
        assert!(manager.update("missing", None, None, None).is_err());
        assert!(manager.touch(&["missing".into()]).is_err());
        assert!(manager.forget("missing").is_err());
        let other = MemoryManager::new(
            manager
                .global_path
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .to_path_buf(),
            manager.workspace_root.join("other-workspace"),
        );
        assert!(other.dream_recover().is_err());
        std::fs::remove_dir(&index).unwrap();
        manager.dream_recover().unwrap();
        assert_eq!(manager.list(None).unwrap().len(), 2);
        assert_eq!(
            manager.dream_history_detail(&plan.run_id).unwrap().status,
            DreamRunStatus::Applied
        );
        assert!(!manager.journal_path().exists());
        manager.dream_rollback(&plan.run_id).unwrap();
        assert!(manager.list(None).unwrap().is_empty());
    }

    #[test]
    fn interrupted_rollback_recovers_both_audits_without_rewriting_original_snapshots() {
        let manager = manager();
        let plan = plan(vec![create_operation("Fact to undo")]);
        manager.apply_plan(&plan, None).unwrap();
        let original = manager.dream_history_detail(&plan.run_id).unwrap();
        // A direct recorded rollback journal models a crash after memory writes.
        let mut original_update = original.clone();
        let rollback_id = new_run_id();
        original_update.rollback_run_id = Some(rollback_id.clone());
        let global = MemoryFile::empty(manager.workspace_path(MemoryScope::Global));
        let project = MemoryFile::empty(manager.workspace_path(MemoryScope::Project));
        let rollback = DreamHistory {
            schema_version: DREAM_SCHEMA_VERSION,
            run_id: rollback_id.clone(),
            created_at: now_secs(),
            status: DreamRunStatus::RolledBack,
            sources: original.sources.clone(),
            plan: None,
            outcomes: vec![],
            before: original.after.clone(),
            after: vec![],
            error: None,
            rollback_run_id: None,
            original_run_id: Some(original.run_id.clone()),
        };
        atomic_write_json(
            &manager.journal_path(),
            &Journal {
                schema_version: DREAM_SCHEMA_VERSION,
                project_path: manager.project_path.clone(),
                global: global.clone(),
                project: project.clone(),
                history: rollback,
                consume_queue: false,
                original_history: Some(original_update),
            },
        )
        .unwrap();
        manager.save(MemoryScope::Global, &global).unwrap();
        manager.save(MemoryScope::Project, &project).unwrap();
        assert_eq!(
            manager.dream_history_detail(&rollback_id).unwrap().status,
            DreamRunStatus::Incomplete
        );
        manager.dream_recover().unwrap();
        assert!(manager.list(None).unwrap().is_empty());
        let restored = manager.dream_history_detail(&plan.run_id).unwrap();
        assert_eq!(restored.status, DreamRunStatus::Applied);
        assert_eq!(
            serde_json::to_value(restored.after).unwrap(),
            serde_json::to_value(original.after).unwrap()
        );
        assert_eq!(
            restored.rollback_run_id.as_deref(),
            Some(rollback_id.as_str())
        );
        assert_eq!(
            manager.dream_history_detail(&rollback_id).unwrap().status,
            DreamRunStatus::RolledBack
        );
        assert_eq!(manager.pending_count().unwrap(), 0);
    }

    #[test]
    fn invalid_plan_is_audited_without_partial_memory_or_queue_changes() {
        let manager = manager();
        let source = manager
            .enqueue_dream("session", "Use safe identifiers")
            .unwrap();
        let mut invalid = create_operation("Rejected operation");
        invalid.content = None;
        let mut plan = plan(vec![create_operation("Must not be committed"), invalid]);
        plan.source_pending_ids.push(source.id.clone());
        assert!(manager.apply_plan(&plan, None).is_err());
        assert!(manager.list(None).unwrap().is_empty());
        assert!(!manager.journal_path().exists());
        assert_eq!(manager.pending_count().unwrap(), 1);
        let history = manager.dream_history_list().unwrap();
        assert_eq!(history[0].status, DreamRunStatus::ApplyFailed);
        assert_eq!(
            history[0].original_run_id.as_deref(),
            Some(plan.run_id.as_str())
        );
        plan.operations.pop();
        plan.source_pending_ids = vec!["../escape".into()];
        assert!(manager.apply_plan(&plan, None).is_err());
        assert!(manager.list(None).unwrap().is_empty());
    }

    #[test]
    fn recovery_rejects_tampered_entry_ids_before_writing_any_scope() {
        let manager = manager();
        let plan = plan(vec![create_operation("Recorded fact")]);
        manager.apply_plan(&plan, None).unwrap();
        let history = manager.dream_history_detail(&plan.run_id).unwrap();
        let mut project = manager.load(MemoryScope::Project).unwrap();
        let original_index =
            std::fs::read(manager.directory(MemoryScope::Project).join("MEMORY.md")).unwrap();
        project.entries[0].id = "../escape".into();
        let global = manager.load(MemoryScope::Global).unwrap();
        let mut journal = Journal {
            schema_version: DREAM_SCHEMA_VERSION,
            project_path: manager.project_path.clone(),
            global,
            project,
            history,
            consume_queue: true,
            original_history: None,
        };
        journal.history.after = combined(&journal.global, &journal.project);
        atomic_write_json(&manager.journal_path(), &journal).unwrap();
        assert!(manager.dream_recover().unwrap_err().contains("memory ID"));
        assert!(manager.journal_path().exists());
        assert_eq!(
            std::fs::read(manager.directory(MemoryScope::Project).join("MEMORY.md")).unwrap(),
            original_index
        );
        journal.project_path = manager.project_path.with_file_name("other.json");
        atomic_write_json(&manager.journal_path(), &journal).unwrap();
        assert!(
            manager
                .dream_recover()
                .unwrap_err()
                .contains("another workspace")
        );
    }

    #[cfg(unix)]
    #[test]
    fn recovery_rejects_symlinked_storage_directories() {
        let manager = manager();
        let plan = plan(vec![create_operation("Recorded fact")]);
        manager.apply_plan(&plan, None).unwrap();
        let journal = Journal {
            schema_version: DREAM_SCHEMA_VERSION,
            project_path: manager.project_path.clone(),
            global: manager.load(MemoryScope::Global).unwrap(),
            project: manager.load(MemoryScope::Project).unwrap(),
            history: manager.dream_history_detail(&plan.run_id).unwrap(),
            consume_queue: true,
            original_history: None,
        };
        atomic_write_json(&manager.journal_path(), &journal).unwrap();
        let directory = manager.directory(MemoryScope::Project);
        let moved = directory.with_extension("moved");
        std::fs::rename(&directory, &moved).unwrap();
        std::os::unix::fs::symlink(&moved, &directory).unwrap();
        assert!(manager.dream_recover().unwrap_err().contains("symlink"));
        assert!(manager.journal_path().exists());
    }

    #[test]
    fn root_lock_coordinates_every_writer_and_read_path() {
        let manager = manager();
        let _lock = manager.acquire_memory_lock().unwrap();
        assert!(
            manager
                .remember(
                    MemoryScope::Project,
                    MemoryKind::Convention,
                    "Blocked".into(),
                    vec![]
                )
                .is_err()
        );
        assert!(manager.update("missing", None, None, None).is_err());
        assert!(manager.touch(&["missing".into()]).is_err());
        assert!(manager.forget("missing").is_err());
        assert!(manager.list(None).is_err());
        assert!(
            manager
                .enqueue_dream("session", "Independent queue writer")
                .is_ok()
        );
    }

    #[test]
    fn preview_and_generation_failure_survive_without_memory_changes() {
        let manager = manager();
        manager.enqueue_dream("session", "Transcript").unwrap();
        let pending = manager.load_pending().unwrap();
        let plan = plan(vec![]);
        manager.record_preview(&plan, &pending).unwrap();
        manager
            .record_generation_failure(&new_run_id(), &pending, "provider failed")
            .unwrap();
        let history = manager.dream_history_list().unwrap();
        assert_eq!(history.len(), 2);
        assert!(
            history
                .iter()
                .any(|run| run.status == DreamRunStatus::GenerationFailed
                    && run.error.as_deref() == Some("provider failed"))
        );
        assert!(manager.dream_rollback(&plan.run_id).is_err());
        assert!(manager.dream_history_detail("../escape").is_err());
        assert_eq!(manager.pending_count().unwrap(), 1);
        assert!(manager.list(None).unwrap().is_empty());
    }
}
