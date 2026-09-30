// Copyright (C) 2026 huangdihd
// SPDX-License-Identifier: GPL-3.0-or-later

//! Durable peer inboxes. Delivery acknowledgement and processed-ID deduplication
//! belong to the caller. Paths must not be concurrently replaced by untrusted
//! processes: standard-library path checks do not provide openat-style isolation.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

pub(crate) const MAX_TEXT_BYTES: usize = 32 * 1024;
// JSON escaping can expand each text byte into six bytes.
const MAX_FILE_BYTES: u64 = (MAX_TEXT_BYTES * 2 * 6 + 4096) as u64;

type Result<T> = std::result::Result<T, String>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PeerKind {
    Question,
    Exchange,
    Delegation,
    Status,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PeerEnvelope {
    pub(crate) id: String,
    pub(crate) from: String,
    pub(crate) to: String,
    pub(crate) kind: PeerKind,
    pub(crate) body: String,
    pub(crate) answer: Option<String>,
    pub(crate) created_at: u64,
}

impl PeerEnvelope {
    pub(crate) fn new(
        from: String,
        to: String,
        kind: PeerKind,
        body: String,
        answer: Option<String>,
    ) -> Result<Self> {
        let envelope = Self {
            id: uuid::Uuid::new_v4().to_string(),
            from,
            to,
            kind,
            body,
            answer,
            created_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_secs(),
        };
        envelope.validate()?;
        Ok(envelope)
    }

    fn validate(&self) -> Result<()> {
        validate_uuid(&self.id)?;
        validate_uuid(&self.from)?;
        validate_uuid(&self.to)?;
        if self.body.len() > MAX_TEXT_BYTES
            || self.answer.as_deref().map_or(0, str::len) > MAX_TEXT_BYTES
        {
            return Err("Peer message body or answer exceeds 32 KiB".into());
        }
        Ok(())
    }
}

/// Require the canonical lowercase, hyphenated representation, not merely any
/// string accepted by the UUID parser, so one session has exactly one inbox.
pub(crate) fn validate_uuid(value: &str) -> Result<()> {
    let parsed =
        uuid::Uuid::parse_str(value).map_err(|_| format!("Invalid peer UUID: {value:?}"))?;
    if parsed.to_string() != value {
        return Err(format!("Noncanonical peer UUID: {value:?}"));
    }
    Ok(())
}

pub(crate) fn root() -> Result<PathBuf> {
    dirs::config_dir()
        .map(|path| path.join("programmer").join("peer-inboxes"))
        .ok_or_else(|| "Cannot determine configuration directory for peer inboxes".into())
}

pub(crate) fn enqueue(envelope: &PeerEnvelope) -> Result<()> {
    Store::default_location()?.enqueue(envelope)
}

pub(crate) fn pending(session: &str) -> Result<Vec<PeerEnvelope>> {
    Store::default_location()?.pending(session)
}

pub(crate) fn remove(session: &str, id: &str) -> Result<()> {
    Store::default_location()?.remove(session, id)
}

pub(crate) struct Store {
    root: PathBuf,
}

impl Store {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub(crate) fn default_location() -> Result<Self> {
        Ok(Self::new(root()?))
    }

    fn inbox(&self, session: &str, create: bool) -> Result<PathBuf> {
        validate_uuid(session)?;
        let path = self.root.join(session);
        check_directories(&path, create)?;
        Ok(path)
    }

    pub(crate) fn enqueue(&self, envelope: &PeerEnvelope) -> Result<()> {
        envelope.validate()?;
        let inbox = self.inbox(&envelope.to, true)?;
        let destination = inbox.join(format!("{}.json", envelope.id));
        match fs::symlink_metadata(&destination) {
            Ok(_) => return Err(format!("Peer message already exists: {}", envelope.id)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        let temporary = inbox.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| -> Result<()> {
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
            let bytes = serde_json::to_vec(envelope).map_err(|error| error.to_string())?;
            file.write_all(&bytes).map_err(|error| error.to_string())?;
            file.sync_all().map_err(|error| error.to_string())?;
            fs::rename(&temporary, &destination).map_err(|error| error.to_string())?;
            Ok(())
        })();
        // On failure, remove only the unique temporary file we just allocated.
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    /// Return oldest first, breaking equal timestamps deterministically by ID.
    /// Malformed entries fail explicitly rather than silently losing messages.
    pub(crate) fn pending(&self, session: &str) -> Result<Vec<PeerEnvelope>> {
        let inbox = self.inbox(session, false)?;
        let entries = match fs::read_dir(&inbox) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.to_string()),
        };
        let mut messages = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| error.to_string())?;
            let path = entry.path();
            let read = || -> Result<Option<PeerEnvelope>> {
                let metadata = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
                if !metadata.is_file() {
                    return Err("Expected a regular peer inbox file (no symlinks)".into());
                }
                let name = entry.file_name();
                let name = name.to_str().ok_or("Non-UTF-8 peer inbox filename")?;
                if let Some(id) = name
                    .strip_prefix('.')
                    .and_then(|name| name.strip_suffix(".tmp"))
                {
                    validate_uuid(id)?;
                    return Ok(None); // In-progress or interrupted atomic write.
                }
                let id = name
                    .strip_suffix(".json")
                    .ok_or("Unexpected peer inbox filename")?;
                validate_uuid(id)?;
                if metadata.len() > MAX_FILE_BYTES {
                    return Err("Peer inbox file exceeds size limit".into());
                }
                let mut bytes = Vec::new();
                fs::File::open(&path)
                    .map_err(|error| error.to_string())?
                    .take(MAX_FILE_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|error| error.to_string())?;
                if bytes.len() as u64 > MAX_FILE_BYTES {
                    return Err("Peer inbox file exceeds size limit".into());
                }
                let envelope: PeerEnvelope =
                    serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
                envelope.validate()?;
                if envelope.to != session || envelope.id != id {
                    return Err("Peer message target or ID does not match its inbox path".into());
                }
                Ok(Some(envelope))
            };
            if let Some(envelope) =
                read().map_err(|error| format!("{}: {error}", path.display()))?
            {
                messages.push(envelope);
            }
        }
        messages
            .sort_by(|left, right| (left.created_at, &left.id).cmp(&(right.created_at, &right.id)));
        Ok(messages)
    }

    /// Idempotent acknowledgement; an already removed message is successful.
    pub(crate) fn remove(&self, session: &str, id: &str) -> Result<()> {
        validate_uuid(id)?;
        let path = self.inbox(session, false)?.join(format!("{id}.json"));
        match fs::symlink_metadata(&path) {
            Ok(metadata) if !metadata.is_file() => {
                return Err("Refusing to remove non-regular peer message".into());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.to_string()),
        }
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }
}

/// Check each component rather than following a symlink hidden in an ancestor.
fn check_directories(path: &Path, create: bool) -> Result<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        if matches!(component, Component::ParentDir) {
            return Err("Parent traversal is not allowed in peer inbox paths".into());
        }
        current.push(component);
        if matches!(component, Component::Prefix(_)) {
            if !path.has_root() {
                return Err("Drive-relative peer inbox paths are not allowed".into());
            }
            // Windows prefixes are not directories until joined to their root separator.
            // The next iteration checks the root; every ancestor is still checked.
            continue;
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if !metadata.is_dir() => {
                return Err(format!(
                    "Peer inbox path is not a directory (symlinks forbidden): {}",
                    current.display()
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {
                match fs::create_dir(&current) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error.to_string()),
                }
                let metadata = fs::symlink_metadata(&current).map_err(|error| error.to_string())?;
                if !metadata.is_dir() {
                    return Err(format!(
                        "Unsafe peer inbox directory: {}",
                        current.display()
                    ));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestStore(Store);

    impl TestStore {
        fn new() -> Self {
            let root = std::env::temp_dir()
                .canonicalize()
                .unwrap()
                .join(format!("programmer-peer-test-{}", uuid::Uuid::new_v4()));
            Self(Store::new(root))
        }
    }

    impl Drop for TestStore {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0.root);
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_absolute_and_verbatim_directories_are_checked() {
        let temporary = std::env::temp_dir();
        let canonical = temporary.canonicalize().unwrap();
        assert!(matches!(
            canonical.components().next(),
            Some(Component::Prefix(prefix)) if prefix.kind().is_verbatim()
        ));
        for root in [temporary, canonical] {
            check_directories(&root, false).unwrap();
            let store = TestStore(Store::new(
                root.join(format!("programmer-peer-test-{}", uuid::Uuid::new_v4())),
            ));
            check_directories(&store.0.root, false).unwrap();
            assert!(!store.0.root.exists());
            check_directories(&store.0.root, true).unwrap();
            assert!(store.0.root.is_dir());
            let file = store.0.root.join("not-a-directory");
            fs::write(&file, "test").unwrap();
            assert!(check_directories(&file.join("child"), false).is_err());
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_drive_relative_directories_are_rejected() {
        for path in [r"C:", r"C:inbox"] {
            assert!(
                check_directories(Path::new(path), false)
                    .unwrap_err()
                    .contains("Drive-relative")
            );
        }
    }

    fn message() -> PeerEnvelope {
        PeerEnvelope::new(
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
            PeerKind::Question,
            "question".into(),
            None,
        )
        .unwrap()
    }

    #[test]
    fn roundtrip_and_stable_ordering() {
        let store = TestStore::new();
        let mut first = message();
        first.created_at = 1;
        first.id = "00000000-0000-4000-8000-000000000001".into();
        let mut second = first.clone();
        second.id = "00000000-0000-4000-8000-000000000002".into();
        let mut third = first.clone();
        third.id = "00000000-0000-4000-8000-000000000003".into();
        third.created_at = 2;
        third.kind = PeerKind::Exchange;
        third.answer = Some("answer".into());
        assert!(store.0.pending(&first.to).unwrap().is_empty());
        for envelope in [&third, &second, &first] {
            store.0.enqueue(envelope).unwrap();
        }
        assert!(store.0.enqueue(&first).is_err());
        assert_eq!(
            store.0.pending(&first.to).unwrap(),
            vec![first.clone(), second, third]
        );
        store.0.remove(&first.to, &first.id).unwrap();
        store.0.remove(&first.to, &first.id).unwrap();
        assert_eq!(store.0.pending(&first.to).unwrap().len(), 2);
    }

    #[test]
    fn invalid_ids_are_rejected() {
        let store = TestStore::new();
        for id in ["../escape", "", "/tmp", "00000000000040008000000000000001"] {
            assert!(validate_uuid(id).is_err());
            assert!(store.0.pending(id).is_err());
            let mut envelope = message();
            envelope.from = id.into();
            assert!(store.0.enqueue(&envelope).is_err());
            envelope = message();
            envelope.to = id.into();
            assert!(store.0.enqueue(&envelope).is_err());
            assert!(store.0.remove(&message().to, id).is_err());
        }
    }

    #[test]
    fn malformed_and_mismatched_entries_fail_explicitly() {
        let store = TestStore::new();
        let envelope = message();
        store.0.enqueue(&envelope).unwrap();
        let path = store
            .0
            .root
            .join(&envelope.to)
            .join(format!("{}.json", envelope.id));
        fs::write(&path, "{broken").unwrap();
        assert!(
            store
                .0
                .pending(&envelope.to)
                .unwrap_err()
                .contains(&envelope.id)
        );
        let mut wrong = envelope.clone();
        wrong.to = uuid::Uuid::new_v4().to_string();
        fs::write(&path, serde_json::to_vec(&wrong).unwrap()).unwrap();
        assert!(store.0.pending(&envelope.to).is_err());
    }

    #[test]
    fn question_and_answer_have_independent_caps() {
        let store = TestStore::new();
        let mut envelope = message();
        envelope.body = "\0".repeat(MAX_TEXT_BYTES);
        store.0.enqueue(&envelope).unwrap();
        assert_eq!(
            store.0.pending(&envelope.to).unwrap(),
            vec![envelope.clone()]
        );
        envelope.answer = Some("\0".repeat(MAX_TEXT_BYTES));
        assert!(envelope.validate().is_ok());
        envelope.answer.as_mut().unwrap().push('x');
        assert!(envelope.validate().is_err());
        let path = store
            .0
            .root
            .join(&envelope.to)
            .join(format!("{}.json", envelope.id));
        fs::write(path, serde_json::to_vec(&envelope).unwrap()).unwrap();
        assert!(store.0.pending(&envelope.to).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_rejected() {
        use std::os::unix::fs::symlink;
        let store = TestStore::new();
        let envelope = message();
        store.0.enqueue(&envelope).unwrap();
        let path = store
            .0
            .root
            .join(&envelope.to)
            .join(format!("{}.json", envelope.id));
        fs::remove_file(&path).unwrap();
        symlink("missing", &path).unwrap();
        assert!(store.0.pending(&envelope.to).is_err());
        assert!(store.0.remove(&envelope.to, &envelope.id).is_err());
        assert!(store.0.enqueue(&envelope).is_err());
        let other = uuid::Uuid::new_v4().to_string();
        symlink(&envelope.to, store.0.root.join(&other)).unwrap();
        assert!(store.0.pending(&other).is_err());
    }
}
