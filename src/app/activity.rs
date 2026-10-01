// Copyright (C) 2026 huangdihd
// SPDX-License-Identifier: GPL-3.0-or-later

//! Adapts durable domain observations into a presentation-only activity view.
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crossterm::event::KeyEvent;

use super::App;
use crate::memory::dream::{DreamHistory, DreamRunStatus};
use crate::peers::graph::{GraphRecord, ObservedState};
use crate::response::message_item::PeerDelegationState;
use crate::session::SessionManager;
use crate::ui::components::activity_panel::{
    ActivityAction, ActivityEntry, ActivityEntryKind, ActivityMode, ActivityPanel,
    GraphEventStatus, GraphStatusTone,
};
use crate::ui::components::conversation_panel::conversation_panel::ConversationPanel;

pub(crate) struct ActivityView {
    mode: ActivityMode,
    session_only: bool,
    center: String,
    last_refresh: Instant,
    session_labels: BTreeMap<String, String>,
    session_details: BTreeMap<String, String>,
}

pub(crate) fn open_dream(app: &mut App<'_>, session_only: bool) {
    open(app, ActivityMode::Dream, session_only);
}

pub(crate) fn open_graph(app: &mut App<'_>) {
    open(app, ActivityMode::SessionGraph, false);
}

fn open(app: &mut App<'_>, mode: ActivityMode, session_only: bool) {
    let title = match mode {
        ActivityMode::Dream if session_only => "Dream history · current session",
        ActivityMode::Dream => "Dream history · current workspace",
        ActivityMode::SessionGraph => "Session graph · direct relationships",
    };
    app.activity_view = Some(ActivityView {
        mode,
        session_only,
        center: app.session.uuid.clone(),
        last_refresh: Instant::now(),
        session_labels: BTreeMap::new(),
        session_details: BTreeMap::new(),
    });
    app.activity_panel = Some(ActivityPanel::new(
        title.into(),
        mode,
        app.session.uuid.clone(),
        Vec::new(),
    ));
    refresh(app);
}

pub(crate) fn tick(app: &mut App<'_>) {
    if app
        .activity_panel
        .as_ref()
        .is_some_and(|panel| !panel.is_confirming())
        && app
            .activity_view
            .as_ref()
            .is_some_and(|view| view.last_refresh.elapsed() >= Duration::from_secs(2))
    {
        refresh(app);
    }
}

fn refresh(app: &mut App<'_>) {
    let Some(view) = app.activity_view.as_mut() else {
        return;
    };
    view.last_refresh = Instant::now();
    let mut kinds = BTreeMap::new();
    let mut record_metadata = BTreeMap::new();
    let mut statuses = BTreeMap::new();
    let result = match view.mode {
        ActivityMode::Dream => {
            crate::memory::MemoryManager::for_current_dir().and_then(|manager| {
                let records = manager.dream_history_list()?;
                Ok(records
                    .into_iter()
                    .filter(|record| {
                        !view.session_only
                            || record
                                .sources
                                .iter()
                                .any(|source| source.session_id == app.session.uuid)
                    })
                    .map(dream_entry)
                    .collect())
            })
        }
        ActivityMode::SessionGraph => load_graph_entries(
            view,
            &app.session.uuid,
            &app.conversation_panel,
            &mut kinds,
            &mut record_metadata,
            &mut statuses,
        ),
    };
    if let Some(panel) = app.activity_panel.as_mut() {
        match result {
            Ok(entries) => {
                panel.replace_entries(entries);
                panel.set_entry_kinds(kinds);
                panel.set_session_labels(view.session_labels.clone());
                panel.set_graph_record_metadata(record_metadata);
                panel.set_graph_statuses(statuses);
            }
            Err(error) => panel.show_notice(format!("Unable to refresh activity: {error}")),
        }
    }
}

fn load_graph_entries(
    view: &mut ActivityView,
    current_session_id: &str,
    conversation_panel: &ConversationPanel,
    kinds: &mut BTreeMap<String, ActivityEntryKind>,
    record_metadata: &mut BTreeMap<String, String>,
    statuses: &mut BTreeMap<String, GraphEventStatus>,
) -> Result<Vec<ActivityEntry>, String> {
    let items = if view.center == current_session_id {
        conversation_panel.items_snapshot()
    } else {
        let manager =
            SessionManager::new().ok_or_else(|| "Session store unavailable".to_string())?;
        manager
            .load(&view.center)
            .map_err(|error| error.to_string())?
            .map(SessionManager::into_items)
            .unwrap_or_default()
    };
    let records = crate::peers::graph::query(&view.center, &items)?;
    let mut entries = Vec::with_capacity(records.len());
    for record in records {
        let kind = if record.kind == crate::peers::PeerKind::Delegation {
            ActivityEntryKind::Delegation
        } else {
            ActivityEntryKind::Question
        };
        kinds.insert(record.id.clone(), kind);
        statuses.insert(record.id.clone(), graph_status(&record));
        cache_session_metadata(view, &record.from);
        cache_session_metadata(view, &record.to);
        let from_details = view
            .session_details
            .get(&record.from)
            .cloned()
            .unwrap_or_default();
        let to_details = view
            .session_details
            .get(&record.to)
            .cloned()
            .unwrap_or_default();
        record_metadata.insert(
            record.id.clone(),
            format!(
                "{}\nSource session:\n{from_details}\n\nTarget session:\n{to_details}\n\nPresence is not inferred from activity history. Viewing does not activate either session.\n",
                graph_metadata(&record),
            ),
        );
        entries.push(graph_entry(record));
    }
    Ok(entries)
}

fn cache_session_metadata(view: &mut ActivityView, id: &str) {
    if view.session_labels.contains_key(id) {
        return;
    }
    let short: String = id.chars().take(8).collect();
    let metadata = SessionManager::new()
        .ok_or_else(|| "Session store unavailable".to_string())
        .and_then(|manager| manager.load(id).map_err(|error| error.to_string()));
    let (label, details) = match metadata {
        Ok(Some(session)) => {
            let title = if session.title.is_empty() {
                "Untitled"
            } else {
                &session.title
            };
            (
                format!(
                    "{} · {short}",
                    crate::session::truncate_first_line(title, 20)
                ),
                format!("{title}\nSession: {id}\nWorkspace: {}", session.working_dir),
            )
        }
        Ok(None) => (short, format!("Session {id}: saved metadata unavailable")),
        Err(error) => (
            short,
            format!("Session {id}: metadata unavailable: {error}"),
        ),
    };
    view.session_labels.insert(id.to_string(), label);
    view.session_details.insert(id.to_string(), details);
}

pub(crate) fn handle_key(app: &mut App<'_>, key: KeyEvent) {
    let Some(panel) = app.activity_panel.as_mut() else {
        return;
    };
    let action = panel.handle_key(key);
    handle_action(app, action);
}

pub(crate) fn handle_mouse(app: &mut App<'_>, mouse: crossterm::event::MouseEvent) {
    let Some(panel) = app.activity_panel.as_mut() else {
        return;
    };
    let action = panel.handle_mouse(mouse);
    handle_action(app, action);
}

fn handle_action(app: &mut App<'_>, action: ActivityAction) {
    match action {
        ActivityAction::None => {}
        ActivityAction::Close => {
            app.activity_panel = None;
            app.activity_view = None;
        }
        ActivityAction::Refresh => refresh(app),
        ActivityAction::FocusSession(center) => {
            if let Err(error) = crate::peers::store::validate_uuid(&center) {
                show_error(app, error);
                return;
            }
            app.activity_view = Some(ActivityView {
                mode: ActivityMode::SessionGraph,
                session_only: false,
                center: center.clone(),
                last_refresh: Instant::now(),
                session_labels: BTreeMap::new(),
                session_details: BTreeMap::new(),
            });
            app.activity_panel = Some(ActivityPanel::new(
                "Session graph · re-centered (does not activate session)".into(),
                ActivityMode::SessionGraph,
                center,
                Vec::new(),
            ));
            refresh(app);
        }
        ActivityAction::RequestRollback(id) => {
            let result = crate::memory::MemoryManager::for_current_dir().and_then(|manager| {
                let preview = manager.dream_rollback_preview(&id)?;
                if !preview.conflicts.is_empty() {
                    return Err(format!("Rollback blocked: later changes affect {}", preview.conflicts.join(", ")));
                }
                let record = manager.dream_history_detail(&id)?;
                Ok(format!(
                    "Undo the entire Dream run {}?\nAffected memories: {}\n\n{}\n\nOnly this run's changes are undone. Recall statistics and unrelated memories are preserved. Source transcripts will NOT be requeued. A new audit record is retained.\n\nConfirm with y; cancel with n or Esc.",
                    preview.run_id, preview.affected_ids.len(), memory_changes(&record, true),
                ))
            });
            match result {
                Ok(details) => {
                    if let Some(panel) = app.activity_panel.as_mut() {
                        panel.show_rollback_confirmation(id, details);
                    }
                }
                Err(error) => show_error(app, error),
            }
        }
        ActivityAction::ConfirmRollback(id) => {
            let result = crate::memory::MemoryManager::for_current_dir()
                .and_then(|manager| manager.dream_rollback(&id));
            match result {
                Ok(_) => {
                    app.conversation_panel.add_info_string(format!(
                        "Dream run {id} rolled back; source transcripts were not requeued."
                    ));
                    refresh(app);
                }
                Err(error) => show_error(app, error),
            }
        }
    }
}

fn show_error(app: &mut App<'_>, error: String) {
    app.conversation_panel.add_warning_string(error.clone());
    if let Some(panel) = app.activity_panel.as_mut() {
        panel.show_notice(error);
    }
}

fn time_label(seconds: u64) -> String {
    format!("{} (Unix {seconds})", relative_time_label(seconds))
}

fn relative_time_label(seconds: u64) -> String {
    let age = crate::memory::now_secs().saturating_sub(seconds);
    match age {
        0..60 => "just now".into(),
        60..3600 => format!("{}m ago", age / 60),
        3600..86400 => format!("{}h ago", age / 3600),
        _ => format!("{}d ago", age / 86400),
    }
}

fn dream_entry(record: DreamHistory) -> ActivityEntry {
    let status = if record.rollback_run_id.is_some() {
        "Applied · subsequently rolled back".to_string()
    } else {
        format!("{:?}", record.status)
    };
    let policy = record
        .plan
        .as_ref()
        .map(|plan| format!("{:?}", plan.policy))
        .unwrap_or_else(|| "Unknown".into());
    let mut details = format!(
        "Run: {}\nTime: {}\nStatus: {status}\nPolicy: {policy}\n",
        record.run_id,
        time_label(record.created_at)
    );
    if record.status == DreamRunStatus::Generating {
        details.push_str("Generation was started; no final result is recorded yet. It may still be running or have been interrupted. This is not a liveness indicator.\n");
    }
    if let Some(id) = &record.rollback_run_id {
        details.push_str(&format!("Rollback audit: {id}\n"));
    }
    if let Some(id) = &record.original_run_id {
        details.push_str(&format!("Reverts original run: {id}\n"));
    }
    if let Some(error) = &record.error {
        details.push_str(&format!("\nError: {error}\n"));
    }
    details.push_str("\nOperation results:\n");
    for outcome in &record.outcomes {
        details.push_str(&format!("• {outcome}\n"));
    }
    details.push_str(&format!(
        "\nMemory changes:\n{}",
        memory_changes(&record, false)
    ));
    if let Some(plan) = &record.plan {
        details.push_str("\nRecorded plan:\n");
        match serde_json::to_string_pretty(&plan.operations) {
            Ok(plan) => details.push_str(&plan),
            Err(error) => details.push_str(&format!("Cannot render plan: {error}")),
        }
    }
    details.push_str("\n\nSource excerpts (not complete conversations):\n");
    for source in &record.sources {
        details.push_str(&format!(
            "\nSession {} · {}\n{}\n",
            source.session_id, source.id, source.transcript
        ));
    }
    ActivityEntry {
        id: record.run_id,
        title: format!("{status} · {}", relative_time_label(record.created_at)),
        summary: format!("{policy} · {} source excerpts", record.sources.len()),
        details,
        from: None,
        to: None,
        rollback_allowed: record.status == DreamRunStatus::Applied
            && record.rollback_run_id.is_none(),
    }
}

fn memory_changes(record: &DreamHistory, reverse: bool) -> String {
    let (before, after) = if reverse {
        (&record.after, &record.before)
    } else {
        (&record.before, &record.after)
    };
    let mut identifiers: Vec<_> = before
        .iter()
        .chain(after)
        .map(|entry| entry.id.as_str())
        .collect();
    identifiers.sort_unstable();
    identifiers.dedup();
    let mut output = String::new();
    for id in identifiers {
        let old = before.iter().find(|entry| entry.id == id);
        let new = after.iter().find(|entry| entry.id == id);
        if serde_json::to_value(old).expect("serializable memory")
            == serde_json::to_value(new).expect("serializable memory")
        {
            continue;
        }
        output.push_str(&format!("\n{id}\n"));
        for (prefix, entry) in [("-", old), ("+", new)] {
            match entry {
                Some(entry) => {
                    output.push_str(&format!(
                        "{prefix} [{:?}/{:?}] {}\n{prefix} {}\n",
                        entry.scope, entry.status, entry.name, entry.content
                    ));
                    output.push_str(&format!("{prefix} kind={:?} confidence={:?} description={}\n{prefix} tags={:?} related={:?} supersedes={:?}\n",
                        entry.kind, entry.confidence, entry.description, entry.tags, entry.related_memories, entry.supersedes));
                }
                None => output.push_str(&format!("{prefix} (absent)\n")),
            }
        }
    }
    if output.is_empty() {
        output.push_str("No recorded memory changes.\n");
    }
    output
}

fn state_label(state: ObservedState) -> &'static str {
    match state {
        ObservedState::QuestionSubmitted => "Question submitted",
        ObservedState::Answered => "Answered (not task completion)",
        ObservedState::AnswerFailed => "Answer failed",
        ObservedState::Delegation(state) => match state {
            PeerDelegationState::Pending => "Awaiting acceptance",
            PeerDelegationState::AcceptedQueued => "Accepted · queued",
            PeerDelegationState::Started => "Started · completion unknown",
            PeerDelegationState::Rejected => "Rejected",
            PeerDelegationState::Cancelled => "Cancelled before start",
        },
    }
}

fn graph_status(record: &GraphRecord) -> GraphEventStatus {
    let state = record.observations.last().map(|event| event.state);
    let tone = match state {
        Some(ObservedState::Answered) => GraphStatusTone::Answered,
        Some(
            ObservedState::AnswerFailed | ObservedState::Delegation(PeerDelegationState::Rejected),
        ) => GraphStatusTone::Failed,
        Some(
            ObservedState::QuestionSubmitted
            | ObservedState::Delegation(
                PeerDelegationState::Pending
                | PeerDelegationState::AcceptedQueued
                | PeerDelegationState::Started,
            ),
        ) => GraphStatusTone::Pending,
        Some(ObservedState::Delegation(PeerDelegationState::Cancelled)) | None => {
            GraphStatusTone::Unknown
        }
    };
    GraphEventStatus {
        label: state.map(state_label).unwrap_or("Unknown").into(),
        tone,
    }
}

fn graph_entry(record: GraphRecord) -> ActivityEntry {
    let status = record
        .observations
        .last()
        .map(|event| state_label(event.state))
        .unwrap_or("Unknown");
    let kind = format!("{:?}", record.kind);
    let title = record
        .body
        .as_deref()
        .and_then(|body| body.lines().find(|line| !line.trim().is_empty()))
        .map(|line| crate::session::truncate_first_line(line.trim(), 100))
        .unwrap_or_else(|| "Original text unavailable".into());
    let mut details = String::new();
    if record.incomplete {
        details.push_str("Historical information is incomplete; missing transitions and times are not inferred.\n");
    }
    details.push_str(&format!(
        "\nQuestion / task:\n{}\n",
        record
            .body
            .as_deref()
            .unwrap_or("Original text unavailable")
    ));
    if let Some(answer) = &record.answer {
        details.push_str(&format!("\nAnswer:\n{answer}\n"));
    }
    details.push_str("\nObserved timeline:\n");
    for event in record.observations {
        let time = event
            .observed_at
            .map(|milliseconds| time_label(milliseconds / 1000))
            .unwrap_or_else(|| "Time unavailable".into());
        details.push_str(&format!("{time} · {}\n", state_label(event.state)));
    }
    ActivityEntry {
        id: record.id,
        title,
        summary: format!(
            "{kind} · {status} · {}",
            record
                .created_at
                .map(relative_time_label)
                .unwrap_or_else(|| "Time unavailable".into())
        ),
        details,
        from: Some(record.from),
        to: Some(record.to),
        rollback_allowed: false,
    }
}

fn graph_metadata(record: &GraphRecord) -> String {
    let created = record
        .created_at
        .map(time_label)
        .unwrap_or_else(|| "Time unavailable".into());
    let mut metadata = format!(
        "Event ID: {}\nType: {:?}\nCreated: {created}\n",
        record.id, record.kind
    );
    if let Some(related) = &record.related_delegation_id {
        metadata.push_str(&format!("Explicitly related delegation: {related}\n"));
    }
    metadata
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{
        MemoryConfidence, MemoryEntry, MemoryKind, MemoryScope, MemorySource, MemoryStatus,
    };
    use crate::peers::PeerKind;
    use crate::peers::graph::Observation;

    fn dream_record() -> DreamHistory {
        DreamHistory {
            schema_version: 1,
            run_id: "dream-run".into(),
            created_at: 123456,
            status: DreamRunStatus::Applied,
            sources: Vec::new(),
            plan: None,
            outcomes: Vec::new(),
            before: Vec::new(),
            after: Vec::new(),
            error: None,
            rollback_run_id: None,
            original_run_id: None,
        }
    }

    fn graph_record(observations: Vec<Observation>) -> GraphRecord {
        GraphRecord {
            id: "delegation".into(),
            from: "source".into(),
            to: "target".into(),
            kind: PeerKind::Delegation,
            created_at: Some(123456),
            body: Some("Review the change".into()),
            answer: None,
            observations,
            incomplete: false,
            related_delegation_id: None,
        }
    }

    #[test]
    fn graph_state_labels_describe_observations_not_completion() {
        for (state, expected) in [
            (ObservedState::QuestionSubmitted, "Question submitted"),
            (ObservedState::Answered, "Answered (not task completion)"),
            (ObservedState::AnswerFailed, "Answer failed"),
            (
                ObservedState::Delegation(PeerDelegationState::Pending),
                "Awaiting acceptance",
            ),
            (
                ObservedState::Delegation(PeerDelegationState::AcceptedQueued),
                "Accepted · queued",
            ),
            (
                ObservedState::Delegation(PeerDelegationState::Started),
                "Started · completion unknown",
            ),
            (
                ObservedState::Delegation(PeerDelegationState::Rejected),
                "Rejected",
            ),
            (
                ObservedState::Delegation(PeerDelegationState::Cancelled),
                "Cancelled before start",
            ),
        ] {
            assert_eq!(state_label(state), expected);
            let entry = graph_entry(graph_record(vec![Observation {
                state,
                observed_at: Some(123456000),
            }]));
            assert!(
                entry
                    .summary
                    .starts_with(&format!("Delegation · {expected} · "))
            );
            assert!(entry.details.contains(expected));
        }
    }

    #[test]
    fn graph_status_color_uses_observations_not_answer_prose() {
        let mut record = graph_record(Vec::new());
        record.answer = Some("Everything completed successfully".into());
        assert!(matches!(
            graph_status(&record).tone,
            GraphStatusTone::Unknown
        ));
        for (state, expected_color) in [
            (ObservedState::Answered, "answered"),
            (ObservedState::AnswerFailed, "failed"),
            (
                ObservedState::Delegation(PeerDelegationState::Pending),
                "pending",
            ),
            (
                ObservedState::Delegation(PeerDelegationState::AcceptedQueued),
                "pending",
            ),
            (
                ObservedState::Delegation(PeerDelegationState::Started),
                "pending",
            ),
            (
                ObservedState::Delegation(PeerDelegationState::Rejected),
                "failed",
            ),
            (
                ObservedState::Delegation(PeerDelegationState::Cancelled),
                "unknown",
            ),
        ] {
            record.observations = vec![Observation {
                state,
                observed_at: None,
            }];
            let status = graph_status(&record);
            let color = match status.tone {
                GraphStatusTone::Pending => "pending",
                GraphStatusTone::Answered => "answered",
                GraphStatusTone::Failed => "failed",
                GraphStatusTone::Unknown => "unknown",
            };
            assert_eq!(color, expected_color);
            assert_eq!(status.label, state_label(state));
        }
    }

    #[test]
    fn graph_answer_without_observation_keeps_state_unknown() {
        let mut record = graph_record(Vec::new());
        record.answer = Some("Acknowledged".into());
        record.incomplete = true;
        let entry = graph_entry(record);
        assert!(entry.summary.starts_with("Delegation · Unknown · "));
        assert!(
            entry
                .details
                .contains("missing transitions and times are not inferred")
        );
        assert!(entry.details.contains("Answer:\nAcknowledged"));
        assert!(!entry.rollback_allowed);
    }

    #[test]
    fn graph_task_preview_is_bounded_without_losing_body_or_provenance() {
        let mut record = graph_record(Vec::new());
        let body = format!("\n  {}\nsecond line", "审查".repeat(100));
        record.body = Some(body.clone());
        record.related_delegation_id = Some("related-task".into());
        let metadata = graph_metadata(&record);
        let entry = graph_entry(record);
        assert!(entry.title.chars().count() <= 101);
        assert!(!entry.title.contains('\n'));
        assert!(entry.details.contains(&body));
        assert!(!entry.details.contains("Event ID:"));
        assert!(metadata.contains("Event ID: delegation"));
        assert!(metadata.contains("Explicitly related delegation: related-task"));
        assert!(metadata.contains("(Unix 123456)"));
    }

    #[test]
    fn graph_missing_body_and_time_are_explicit_without_inventing_state() {
        let mut record = graph_record(Vec::new());
        record.body = None;
        record.created_at = None;
        assert!(graph_metadata(&record).contains("Time unavailable"));
        let entry = graph_entry(record);
        assert_eq!(entry.title, "Original text unavailable");
        assert!(entry.summary.contains("Unknown · Time unavailable"));
        assert!(entry.details.contains("Original text unavailable"));
    }

    #[test]
    fn rollback_is_disabled_after_original_run_was_rolled_back() {
        let mut record = dream_record();
        assert!(dream_entry(record.clone()).rollback_allowed);
        record.rollback_run_id = Some("rollback-audit".into());
        let entry = dream_entry(record);
        assert!(!entry.rollback_allowed);
        assert!(
            entry
                .title
                .starts_with("Applied · subsequently rolled back")
        );
        assert!(entry.details.contains("Rollback audit: rollback-audit"));
    }

    #[test]
    fn rollback_is_disabled_for_non_applied_runs() {
        for status in [
            DreamRunStatus::Generating,
            DreamRunStatus::Preview,
            DreamRunStatus::ApplyFailed,
            DreamRunStatus::Incomplete,
            DreamRunStatus::GenerationFailed,
            DreamRunStatus::RolledBack,
        ] {
            let mut record = dream_record();
            record.status = status;
            assert!(!dream_entry(record).rollback_allowed, "{status:?}");
        }
    }

    #[test]
    fn graph_titles_prioritize_task_while_summaries_keep_observed_status() {
        let dream = dream_entry(dream_record());
        let graph = graph_entry(graph_record(vec![Observation {
            state: ObservedState::Delegation(PeerDelegationState::Started),
            observed_at: Some(123456000),
        }]));
        assert!(dream.title.starts_with("Applied · "));
        assert_eq!(graph.title, "Review the change");
        assert!(graph.summary.starts_with("Delegation · Started"));
        for entry in [dream, graph] {
            assert!(!entry.title.contains("Unix"));
            assert!(!entry.title.contains("123456"));
            assert!(entry.details.contains("(Unix 123456)"));
        }
        let now = crate::memory::now_secs();
        assert_eq!(relative_time_label(now + 60), "just now");
        assert_eq!(relative_time_label(now - 120), "2m ago");
        assert_eq!(relative_time_label(now - 7200), "2h ago");
        assert_eq!(relative_time_label(now - 172800), "2d ago");
    }

    #[test]
    fn change_preview_includes_semantic_metadata_and_reverses_it_for_rollback() {
        let before = MemoryEntry {
            id: "memory".into(),
            scope: MemoryScope::Project,
            kind: MemoryKind::ProjectFact,
            name: "Original name".into(),
            description: "Original description".into(),
            content: "Unchanged content".into(),
            tags: vec!["old-tag".into()],
            related_memories: vec!["old-related".into()],
            created_at: 1,
            updated_at: 1,
            last_used_at: None,
            use_count: 0,
            source: MemorySource::Dream,
            confidence: MemoryConfidence::Inferred,
            status: MemoryStatus::Active,
            supersedes: None,
        };
        let mut after = before.clone();
        after.scope = MemoryScope::Global;
        after.kind = MemoryKind::Decision;
        after.name = "Updated name".into();
        after.description = "Updated description".into();
        after.tags = vec!["new-tag".into()];
        after.related_memories = vec!["new-related".into()];
        after.confidence = MemoryConfidence::Confirmed;
        after.status = MemoryStatus::Archived;
        after.supersedes = Some("older-memory".into());
        let mut record = dream_record();
        record.before = vec![before];
        record.after = vec![after];
        for (reverse, old_prefix, new_prefix) in [(false, "-", "+"), (true, "+", "-")] {
            let preview = memory_changes(&record, reverse);
            for (prefix, lines) in [
                (
                    old_prefix,
                    [
                        "[Project/Active] Original name",
                        "kind=ProjectFact confidence=Inferred description=Original description",
                        "tags=[\"old-tag\"] related=[\"old-related\"] supersedes=None",
                    ],
                ),
                (
                    new_prefix,
                    [
                        "[Global/Archived] Updated name",
                        "kind=Decision confidence=Confirmed description=Updated description",
                        "tags=[\"new-tag\"] related=[\"new-related\"] supersedes=Some(\"older-memory\")",
                    ],
                ),
            ] {
                for line in lines {
                    assert!(preview.contains(&format!("{prefix} {line}\n")), "{preview}");
                }
            }
        }
    }
}
