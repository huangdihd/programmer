// Copyright (C) 2026 huangdihd
// SPDX-License-Identifier: GPL-3.0-or-later

//! Display-only observations. Reading history never opens a session, acquires a
//! presence lock, answers a question, or grants execution permission.
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use super::store::{self, PeerEnvelope, PeerKind, Store, validate_uuid};
use crate::response::message_item::{MessageItem, PeerDelegationState};

type Result<T> = std::result::Result<T, String>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ObservedState {
    QuestionSubmitted,
    Answered,
    AnswerFailed,
    Delegation(PeerDelegationState),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Observation {
    pub(crate) state: ObservedState,
    /// Unix milliseconds; absent for pre-history session records.
    pub(crate) observed_at: Option<u64>,
}

#[derive(Debug, Clone)]
pub(crate) struct GraphRecord {
    pub(crate) id: String,
    pub(crate) from: String,
    pub(crate) to: String,
    pub(crate) kind: PeerKind,
    pub(crate) created_at: Option<u64>,
    pub(crate) body: Option<String>,
    pub(crate) answer: Option<String>,
    pub(crate) observations: Vec<Observation>,
    pub(crate) incomplete: bool,
    pub(crate) related_delegation_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Event {
    envelope: PeerEnvelope,
    observation: Observation,
    /// Assigned under the record's cross-process lock, independent of wall time.
    /// Zero identifies legacy events whose tie order cannot be recovered.
    #[serde(default)]
    sequence: u64,
}

/// Record only a transition actually witnessed by the caller. Never synthesize
/// completion from an answer, acknowledgement, started turn, or inbox removal.
pub(crate) fn observe(envelope: &PeerEnvelope, state: ObservedState) -> Result<()> {
    Store::default_location()?.observe(envelope, state)
}

pub(crate) fn query(session: &str, saved_items: &[MessageItem]) -> Result<Vec<GraphRecord>> {
    Store::default_location()?.graph(session, saved_items)
}

impl Store {
    pub(crate) fn observe(&self, envelope: &PeerEnvelope, state: ObservedState) -> Result<()> {
        envelope.validate()?;
        let valid = match state {
            ObservedState::Delegation(_) => envelope.kind == PeerKind::Delegation,
            ObservedState::QuestionSubmitted => envelope.kind == PeerKind::Question,
            ObservedState::Answered => {
                envelope.kind == PeerKind::Exchange && envelope.answer.is_some()
            }
            ObservedState::AnswerFailed => {
                matches!(envelope.kind, PeerKind::Question | PeerKind::Status)
            }
        };
        if !valid {
            return Err("Peer observation does not match its envelope kind".into());
        }
        let directory = self.root.join(".history");
        store::check_directories(&directory, true)?;
        let _lock = history_lock(&directory, &envelope.id)?;
        let events = self.history_events()?;
        let previous = events
            .iter()
            .map(|(_, event)| event)
            .filter(|event| event.envelope.id == envelope.id)
            .max_by_key(|event| (event.sequence, event.observation.observed_at));
        // Repeated delivery/observation is idempotent, but Pending after an
        // acceptance is a real restart transition and must not be discarded.
        if previous
            .is_some_and(|event| event.envelope == *envelope && event.observation.state == state)
        {
            return Ok(());
        }
        let sequence = previous
            .map_or(0, |event| event.sequence)
            .checked_add(1)
            .ok_or("Peer history sequence exhausted")?;
        let event = Event {
            sequence,
            envelope: envelope.clone(),
            observation: Observation {
                state,
                observed_at: Some(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_err(|error| error.to_string())?
                        .as_millis() as u64,
                ),
            },
        };
        let identifier = uuid::Uuid::new_v4();
        let temporary = directory.join(format!(".{identifier}.tmp"));
        let destination = directory.join(format!("{identifier}.json"));
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
                .map_err(|error| error.to_string())?;
            let bytes = serde_json::to_vec(&event).map_err(|error| error.to_string())?;
            file.write_all(&bytes).map_err(|error| error.to_string())?;
            file.sync_all().map_err(|error| error.to_string())?;
            fs::rename(&temporary, &destination).map_err(|error| error.to_string())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }

    fn history_events(&self) -> Result<Vec<(String, Event)>> {
        let directory = self.root.join(".history");
        store::check_directories(&directory, false)?;
        let mut events = Vec::new();
        match fs::read_dir(&directory) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry.map_err(|error| error.to_string())?;
                    let path = entry.path();
                    let metadata = match fs::symlink_metadata(&path) {
                        Ok(metadata) => metadata,
                        // A concurrent writer may have just published its temp file.
                        Err(error)
                            if error.kind() == std::io::ErrorKind::NotFound
                                && path.extension().is_some_and(|extension| extension == "tmp") =>
                        {
                            continue;
                        }
                        Err(error) => return Err(error.to_string()),
                    };
                    if !metadata.is_file() {
                        return Err("Peer history entry must be a regular file".into());
                    }
                    let name = entry.file_name();
                    let name = name.to_str().ok_or("Non-UTF-8 peer history filename")?;
                    if let Some(id) = name.strip_suffix(".lock") {
                        validate_uuid(id)?;
                        continue;
                    }
                    if let Some(id) = name
                        .strip_prefix('.')
                        .and_then(|name| name.strip_suffix(".tmp"))
                    {
                        validate_uuid(id)?;
                        continue;
                    }
                    validate_uuid(
                        name.strip_suffix(".json")
                            .ok_or("Unexpected peer history filename")?,
                    )?;
                    let mut bytes = Vec::new();
                    fs::File::open(&path)
                        .map_err(|error| error.to_string())?
                        .take(store::MAX_FILE_BYTES + 1)
                        .read_to_end(&mut bytes)
                        .map_err(|error| error.to_string())?;
                    if bytes.len() as u64 > store::MAX_FILE_BYTES {
                        return Err("Peer history entry exceeds size limit".into());
                    }
                    let event: Event =
                        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
                    event.envelope.validate()?;
                    events.push((name.to_owned(), event));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        Ok(events)
    }

    pub(crate) fn graph(
        &self,
        session: &str,
        saved_items: &[MessageItem],
    ) -> Result<Vec<GraphRecord>> {
        validate_uuid(session)?;
        let mut events = self.history_events()?;
        events.retain(|(_, event)| event.envelope.from == session || event.envelope.to == session);
        events.sort_by(|(left_id, left), (right_id, right)| {
            (
                &left.envelope.id,
                left.sequence,
                left.observation.observed_at,
                left_id,
            )
                .cmp(&(
                    &right.envelope.id,
                    right.sequence,
                    right.observation.observed_at,
                    right_id,
                ))
        });
        let mut records = BTreeMap::new();
        for (_, event) in events {
            let envelope = event.envelope;
            let record = records
                .entry(envelope.id.clone())
                .or_insert_with(|| GraphRecord {
                    id: envelope.id.clone(),
                    from: envelope.from.clone(),
                    to: envelope.to.clone(),
                    kind: if envelope.kind == PeerKind::Delegation {
                        PeerKind::Delegation
                    } else {
                        PeerKind::Question
                    },
                    created_at: Some(envelope.created_at),
                    body: Some(envelope.body.clone()),
                    answer: None,
                    observations: Vec::new(),
                    incomplete: true,
                    related_delegation_id: envelope.related_delegation_id.clone(),
                });
            if envelope.answer.is_some() {
                record.answer = envelope.answer;
            }
            if matches!(
                event.observation.state,
                ObservedState::QuestionSubmitted
                    | ObservedState::Delegation(PeerDelegationState::Pending)
            ) {
                record.incomplete = false;
            }
            record.observations.push(event.observation);
        }
        // Legacy records lack routing/timestamps in some cases. Only recover
        // direction when the old typed record actually establishes it.
        for item in saved_items {
            let (id, from, body, answer, kind, state) = match item {
                MessageItem::PeerExchange {
                    id,
                    from,
                    question,
                    answer,
                } => (
                    id,
                    from,
                    Some(question.clone()),
                    answer.clone(),
                    PeerKind::Question,
                    answer.as_ref().map(|_| ObservedState::Answered),
                ),
                MessageItem::PeerDelegation {
                    id,
                    from,
                    body: Some(body),
                    state,
                } => (
                    id,
                    from,
                    Some(body.clone()),
                    None,
                    PeerKind::Delegation,
                    Some(ObservedState::Delegation(*state)),
                ),
                // Source-side legacy status has no trustworthy original target.
                _ => continue,
            };
            if let Some(record) = records.get_mut(id) {
                merge_saved_answer(record, answer);
                continue;
            }
            // Online UI exchanges use the reversible inbox ID, while durable
            // observations retain the original question ID. They are one event.
            if kind == PeerKind::Question
                && let Ok(question_id) = super::exchange_id(id)
                && let Some(record) = records.get_mut(&question_id)
                && record.from == *from
                && record.to == session
                && record.body == body
            {
                merge_saved_answer(record, answer);
                continue;
            }
            records.insert(
                id.clone(),
                GraphRecord {
                    id: id.clone(),
                    from: from.clone(),
                    to: session.into(),
                    kind,
                    created_at: None,
                    body,
                    answer,
                    observations: state
                        .into_iter()
                        .map(|state| Observation {
                            state,
                            observed_at: None,
                        })
                        .collect(),
                    incomplete: true,
                    related_delegation_id: None,
                },
            );
        }
        // Pending legacy inbox entries remain visible without mutating history.
        for envelope in self.pending(session)? {
            if !matches!(envelope.kind, PeerKind::Question | PeerKind::Delegation) {
                continue;
            }
            records.entry(envelope.id.clone()).or_insert(GraphRecord {
                id: envelope.id,
                from: envelope.from,
                to: envelope.to,
                kind: envelope.kind,
                created_at: Some(envelope.created_at),
                body: Some(envelope.body),
                answer: envelope.answer,
                observations: Vec::new(),
                incomplete: true,
                related_delegation_id: envelope.related_delegation_id,
            });
        }
        let mut records: Vec<_> = records.into_values().collect();
        records
            .sort_by(|left, right| (left.created_at, &left.id).cmp(&(right.created_at, &right.id)));
        Ok(records)
    }
}

/// Keep lock files in place: unlinking a locked inode lets another process
/// acquire a different lock for the same record. Closing releases the OS lock.
fn merge_saved_answer(record: &mut GraphRecord, answer: Option<String>) {
    if record.answer.is_some() || answer.is_none() {
        return;
    }
    record.answer = answer;
    record.incomplete = true;
    // The UI record proves a saved answer exists, but not when it arrived.
    if !record
        .observations
        .iter()
        .any(|event| event.state == ObservedState::Answered)
    {
        record.observations.push(Observation {
            state: ObservedState::Answered,
            observed_at: None,
        });
    }
}

fn history_lock(directory: &std::path::Path, id: &str) -> Result<fs::File> {
    let path = directory.join(format!("{id}.lock"));
    match fs::symlink_metadata(&path) {
        Ok(metadata) if !metadata.is_file() => {
            return Err("Peer history lock must be a regular file".into());
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&path).map_err(|error| error.to_string())?;
    let metadata = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
    if !metadata.is_file() {
        return Err("Peer history lock changed while opening".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let opened = file.metadata().map_err(|error| error.to_string())?;
        if opened.dev() != metadata.dev() || opened.ino() != metadata.ino() || opened.nlink() != 1 {
            return Err("Peer history lock changed or is hard-linked".into());
        }
    }
    file.lock_exclusive().map_err(|error| error.to_string())?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDirectory(std::path::PathBuf);
    impl TestDirectory {
        fn new() -> Self {
            Self(
                std::env::temp_dir()
                    .canonicalize()
                    .unwrap()
                    .join(format!("programmer-peer-graph-{}", uuid::Uuid::new_v4())),
            )
        }
    }
    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn delegation() -> PeerEnvelope {
        PeerEnvelope::new(
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
            PeerKind::Delegation,
            "task".into(),
            None,
        )
        .unwrap()
    }

    #[test]
    fn online_ui_exchange_is_not_a_second_graph_question() {
        let directory = TestDirectory::new();
        let store = Store::new(directory.0.clone());
        let mut question = delegation();
        question.kind = PeerKind::Question;
        store
            .observe(&question, ObservedState::QuestionSubmitted)
            .unwrap();
        let mut answered = question.clone();
        answered.kind = PeerKind::Exchange;
        answered.answer = Some("Answer".into());
        store.observe(&answered, ObservedState::Answered).unwrap();
        let ui_item = MessageItem::PeerExchange {
            id: super::super::exchange_id(&question.id).unwrap(),
            from: question.from.clone(),
            question: question.body.clone(),
            answer: answered.answer,
        };
        let records = store.graph(&question.to, &[ui_item]).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id, question.id);
        assert_eq!(records[0].observations.len(), 2);
    }

    #[test]
    fn saved_online_answer_supplements_a_failed_answer_audit() {
        let directory = TestDirectory::new();
        let store = Store::new(directory.0.clone());
        let mut question = delegation();
        question.kind = PeerKind::Question;
        store
            .observe(&question, ObservedState::QuestionSubmitted)
            .unwrap();
        let item = MessageItem::PeerExchange {
            id: super::super::exchange_id(&question.id).unwrap(),
            from: question.from.clone(),
            question: question.body.clone(),
            answer: Some("Saved answer".into()),
        };
        let records = store.graph(&question.to, &[item]).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].answer.as_deref(), Some("Saved answer"));
        assert!(records[0].incomplete);
        assert_eq!(records[0].observations.last().unwrap().observed_at, None);
        assert_eq!(
            records[0].observations.last().unwrap().state,
            ObservedState::Answered
        );
    }

    #[test]
    fn sequence_preserves_restart_chronology_despite_clock_ties_and_regression() {
        let directory = TestDirectory::new();
        let root = directory.0.join("inboxes");
        let envelope = delegation();
        let states = [
            PeerDelegationState::Pending,
            PeerDelegationState::AcceptedQueued,
            PeerDelegationState::Pending,
            PeerDelegationState::AcceptedQueued,
            PeerDelegationState::Started,
        ];
        for state in states {
            // Reopen rather than relying on any in-memory sequence counter.
            Store::new(root.clone())
                .observe(&envelope, ObservedState::Delegation(state))
                .unwrap();
        }
        let store = Store::new(root.clone());
        for (name, mut event) in store.history_events().unwrap() {
            event.observation.observed_at = Some(if event.sequence <= 2 { 500 } else { 100 });
            fs::write(
                root.join(".history").join(name),
                serde_json::to_vec(&event).unwrap(),
            )
            .unwrap();
        }
        let graph = store.graph(&envelope.to, &[]).unwrap();
        assert_eq!(
            graph[0]
                .observations
                .iter()
                .map(|observation| observation.state)
                .collect::<Vec<_>>(),
            states.map(ObservedState::Delegation)
        );
        assert_eq!(
            graph[0]
                .observations
                .iter()
                .map(|observation| observation.observed_at.unwrap())
                .collect::<Vec<_>>(),
            [500, 500, 100, 100, 100]
        );
    }

    #[test]
    fn concurrent_duplicate_observations_are_deduplicated_under_record_lock() {
        let directory = TestDirectory::new();
        let root = directory.0.join("inboxes");
        let envelope = delegation();
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let root = &root;
                let envelope = &envelope;
                scope.spawn(move || {
                    Store::new(root.clone())
                        .observe(
                            envelope,
                            ObservedState::Delegation(PeerDelegationState::Pending),
                        )
                        .unwrap();
                });
            }
        });
        let store = Store::new(root);
        let events = store.history_events().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].1.sequence, 1);
        let saved = [MessageItem::PeerDelegation {
            id: envelope.id.clone(),
            from: envelope.from.clone(),
            body: Some(envelope.body.clone()),
            state: PeerDelegationState::AcceptedQueued,
        }];
        store.enqueue(&envelope).unwrap();
        let graph = store.graph(&envelope.to, &saved).unwrap();
        assert_eq!(graph.len(), 1);
        assert_eq!(graph[0].observations.len(), 1);
        assert_eq!(
            graph[0].observations[0].state,
            ObservedState::Delegation(PeerDelegationState::Pending)
        );
        assert_eq!(store.pending(&envelope.to).unwrap(), [envelope]);
    }

    #[test]
    fn concurrent_distinct_observations_receive_unique_contiguous_sequences() {
        let directory = TestDirectory::new();
        let root = directory.0.join("inboxes");
        let envelope = delegation();
        std::thread::scope(|scope| {
            for index in 0..8 {
                let root = &root;
                let mut envelope = envelope.clone();
                envelope.body = format!("observation {index}");
                scope.spawn(move || {
                    Store::new(root.clone())
                        .observe(
                            &envelope,
                            ObservedState::Delegation(PeerDelegationState::Pending),
                        )
                        .unwrap();
                });
            }
        });
        let mut sequences: Vec<_> = Store::new(root)
            .history_events()
            .unwrap()
            .into_iter()
            .map(|(_, event)| event.sequence)
            .collect();
        sequences.sort();
        assert_eq!(sequences, (1..=8).collect::<Vec<_>>());
    }

    #[test]
    fn legacy_event_without_sequence_precedes_new_observations() {
        let directory = TestDirectory::new();
        let store = Store::new(directory.0.join("inboxes"));
        let envelope = delegation();
        store
            .observe(
                &envelope,
                ObservedState::Delegation(PeerDelegationState::AcceptedQueued),
            )
            .unwrap();
        let (name, event) = store.history_events().unwrap().pop().unwrap();
        let mut value = serde_json::to_value(event).unwrap();
        value.as_object_mut().unwrap().remove("sequence");
        value["observation"]["observed_at"] = serde_json::json!(u64::MAX);
        fs::write(
            store.root.join(".history").join(name),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        store
            .observe(
                &envelope,
                ObservedState::Delegation(PeerDelegationState::Pending),
            )
            .unwrap();
        let graph = store.graph(&envelope.to, &[]).unwrap();
        assert_eq!(
            graph[0].observations.last().unwrap().state,
            ObservedState::Delegation(PeerDelegationState::Pending)
        );
    }

    #[test]
    fn linked_answer_never_changes_delegation_state() {
        let directory = TestDirectory::new();
        let store = Store::new(directory.0.join("inboxes"));
        let delegation = delegation();
        store
            .observe(
                &delegation,
                ObservedState::Delegation(PeerDelegationState::Started),
            )
            .unwrap();
        let mut question = PeerEnvelope::new(
            delegation.to.clone(),
            delegation.from.clone(),
            PeerKind::Question,
            "done".into(),
            None,
        )
        .unwrap();
        question.related_delegation_id = Some(delegation.id.clone());
        store
            .observe(&question, ObservedState::QuestionSubmitted)
            .unwrap();
        question.kind = PeerKind::Exchange;
        question.answer = Some("acknowledged".into());
        store.observe(&question, ObservedState::Answered).unwrap();
        let graph = store.graph(&delegation.from, &[]).unwrap();
        assert_eq!(graph.len(), 2);
        let record = graph
            .iter()
            .find(|record| record.id == delegation.id)
            .unwrap();
        assert_eq!(record.observations.len(), 1);
        assert_eq!(
            record.observations[0].state,
            ObservedState::Delegation(PeerDelegationState::Started)
        );
        assert!(record.answer.is_none());
        let record = graph
            .iter()
            .find(|record| record.id == question.id)
            .unwrap();
        assert_eq!(
            record.related_delegation_id.as_deref(),
            Some(delegation.id.as_str())
        );
        assert_eq!(record.answer.as_deref(), Some("acknowledged"));
    }

    #[test]
    fn malformed_history_fails_explicitly_without_modifying_inbox() {
        let directory = TestDirectory::new();
        let store = Store::new(directory.0.join("inboxes"));
        let envelope = delegation();
        store.enqueue(&envelope).unwrap();
        fs::create_dir(store.root.join(".history")).unwrap();
        fs::write(
            store
                .root
                .join(".history")
                .join(format!("{}.json", uuid::Uuid::new_v4())),
            b"not json",
        )
        .unwrap();
        assert!(store.graph(&envelope.to, &[]).is_err());
        assert!(
            store
                .observe(
                    &envelope,
                    ObservedState::Delegation(PeerDelegationState::Pending)
                )
                .is_err()
        );
        assert_eq!(store.pending(&envelope.to).unwrap(), [envelope]);
    }

    #[test]
    fn history_survives_consumption_and_is_visible_at_both_endpoints() {
        let directory = TestDirectory::new();
        let store = Store::new(directory.0.clone().join("inboxes"));
        let envelope = PeerEnvelope::new(
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
            PeerKind::Delegation,
            "task".into(),
            None,
        )
        .unwrap();
        store.enqueue(&envelope).unwrap();
        store
            .observe(
                &envelope,
                ObservedState::Delegation(PeerDelegationState::Pending),
            )
            .unwrap();
        store
            .observe(
                &envelope,
                ObservedState::Delegation(PeerDelegationState::AcceptedQueued),
            )
            .unwrap();
        store.remove(&envelope.to, &envelope.id).unwrap();
        for session in [&envelope.from, &envelope.to] {
            let graph = store.graph(session, &[]).unwrap();
            assert_eq!(graph.len(), 1);
            assert!(!graph[0].incomplete);
            assert_eq!(graph[0].observations.len(), 2);
            assert!(
                graph[0]
                    .observations
                    .iter()
                    .all(|observation| observation.observed_at.is_some())
            );
            assert!(
                !graph[0]
                    .observations
                    .iter()
                    .any(|observation| observation.state
                        == ObservedState::Delegation(PeerDelegationState::Started))
            );
        }
    }

    #[test]
    fn legacy_envelopes_default_to_no_association_and_invalid_states_fail() {
        let directory = TestDirectory::new();
        let store = Store::new(directory.0.clone());
        let envelope = PeerEnvelope::new(
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
            PeerKind::Question,
            "question".into(),
            None,
        )
        .unwrap();
        let value = serde_json::to_value(&envelope).unwrap();
        assert!(value.get("related_delegation_id").is_none());
        let restored: PeerEnvelope = serde_json::from_value(value).unwrap();
        assert!(restored.related_delegation_id.is_none());
        assert!(
            store
                .observe(
                    &envelope,
                    ObservedState::Delegation(PeerDelegationState::Started)
                )
                .is_err()
        );
        assert!(!directory.0.exists());
        let mut linked = envelope;
        linked.related_delegation_id = Some(uuid::Uuid::new_v4().to_string());
        store
            .observe(&linked, ObservedState::QuestionSubmitted)
            .unwrap();
        let graph = store.graph(&linked.from, &[]).unwrap();
        assert_eq!(graph[0].related_delegation_id, linked.related_delegation_id);
        assert_eq!(graph[0].observations.len(), 1);
    }

    #[test]
    fn empty_query_is_read_only_and_legacy_is_incomplete() {
        let directory = TestDirectory::new();
        let root = directory.0.clone().join("absent");
        let store = Store::new(root.clone());
        let session = uuid::Uuid::new_v4().to_string();
        assert!(store.graph(&session, &[]).unwrap().is_empty());
        assert!(!root.exists());
        let items = [MessageItem::PeerExchange {
            id: uuid::Uuid::new_v4().to_string(),
            from: uuid::Uuid::new_v4().to_string(),
            question: "question".into(),
            answer: Some("answer".into()),
        }];
        let graph = store.graph(&session, &items).unwrap();
        assert!(graph[0].incomplete);
        assert_eq!(graph[0].created_at, None);
        assert_eq!(graph[0].observations[0].observed_at, None);
        assert!(!root.exists());
    }
}
