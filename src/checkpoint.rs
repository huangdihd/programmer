// Copyright (C) 2026 huangdihd
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

//! Per-session rewind checkpoints for conversation state and built-in file
//! edits. File bodies are content-addressed so repeated edits do not duplicate
//! data on disk.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct FileChange {
    #[serde(default)]
    pub(crate) sequence: u64,
    pub(crate) path: PathBuf,
    pub(crate) existed: bool,
    pub(crate) before_blob: Option<String>,
    pub(crate) after_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Checkpoint {
    pub(crate) id: u64,
    pub(crate) prompt: String,
    #[serde(default)]
    pub(crate) label: Option<String>,
    pub(crate) conversation_cutoff: usize,
    pub(crate) todos: Vec<crate::todos::Todo>,
    pub(crate) files: Vec<FileChange>,
    #[serde(default)]
    pub(crate) recovery: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Manifest {
    #[serde(default)]
    checkpoints: Vec<Checkpoint>,
}

#[derive(Debug)]
pub(crate) struct RestoreReport {
    pub(crate) restored: usize,
    pub(crate) conflicts: Vec<PathBuf>,
}

#[derive(Debug)]
pub(crate) struct CheckpointStore {
    root: PathBuf,
    manifest: Manifest,
    next_id: u64,
    next_sequence: u64,
}

impl CheckpointStore {
    pub(crate) fn for_session(uuid: &str) -> Option<Self> {
        let root = dirs::config_dir()?
            .join("programmer")
            .join("checkpoints")
            .join(uuid);
        Some(Self::at(root))
    }

    fn at(root: PathBuf) -> Self {
        let manifest: Manifest = std::fs::read(root.join("manifest.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        let next_id = manifest
            .checkpoints
            .iter()
            .map(|checkpoint| checkpoint.id)
            .max()
            .unwrap_or(0)
            .wrapping_add(1);
        let next_sequence = manifest
            .checkpoints
            .iter()
            .flat_map(|checkpoint| checkpoint.files.iter())
            .map(|change| change.sequence)
            .max()
            .unwrap_or(0)
            .wrapping_add(1);
        Self {
            root,
            manifest,
            next_id,
            next_sequence,
        }
    }

    pub(crate) fn checkpoints(&self) -> &[Checkpoint] {
        &self.manifest.checkpoints
    }

    pub(crate) fn checkpoint(&self, id: u64) -> Option<&Checkpoint> {
        self.manifest
            .checkpoints
            .iter()
            .find(|checkpoint| checkpoint.id == id)
    }

    pub(crate) fn has_file_changes_for_restore(&self, target_id: u64) -> bool {
        let target_is_recovery = self
            .checkpoint(target_id)
            .is_some_and(|checkpoint| checkpoint.recovery);
        self.manifest.checkpoints.iter().any(|checkpoint| {
            let selected = if target_is_recovery {
                checkpoint.id == target_id
            } else {
                checkpoint.id >= target_id && !checkpoint.recovery
            };
            selected && !checkpoint.files.is_empty()
        })
    }

    pub(crate) fn begin(
        &mut self,
        prompt: String,
        conversation_cutoff: usize,
        todos: Vec<crate::todos::Todo>,
    ) -> Result<u64, String> {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        self.manifest.checkpoints.push(Checkpoint {
            id,
            prompt,
            label: None,
            conversation_cutoff,
            todos,
            files: Vec::new(),
            recovery: false,
        });
        self.persist()?;
        Ok(id)
    }

    pub(crate) fn record_before(&mut self, id: u64, path: &Path) -> Result<(), String> {
        let path = absolute(path)?;
        let Some(checkpoint_index) = self
            .manifest
            .checkpoints
            .iter()
            .position(|checkpoint| checkpoint.id == id)
        else {
            return Ok(());
        };
        if self.manifest.checkpoints[checkpoint_index]
            .files
            .iter()
            .any(|change| change.path == path)
        {
            return Ok(());
        }
        let before = match std::fs::read(&path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(format!("read {}: {error}", path.display())),
        };
        let before_blob = before
            .as_deref()
            .map(|bytes| self.write_blob(bytes))
            .transpose()?;
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.wrapping_add(1);
        self.manifest.checkpoints[checkpoint_index]
            .files
            .push(FileChange {
                sequence,
                path,
                existed: before.is_some(),
                before_blob,
                after_hash: None,
            });
        self.persist()
    }

    pub(crate) fn record_after(&mut self, id: u64, path: &Path) -> Result<(), String> {
        let path = absolute(path)?;
        let after_hash = std::fs::read(&path).ok().map(|bytes| hash(&bytes));
        let Some(change) = self
            .manifest
            .checkpoints
            .iter_mut()
            .find(|checkpoint| checkpoint.id == id)
            .and_then(|checkpoint| {
                checkpoint
                    .files
                    .iter_mut()
                    .find(|change| change.path == path)
            })
        else {
            return Ok(());
        };
        change.after_hash = after_hash;
        self.persist()
    }

    pub(crate) fn discard_unfinished(&mut self, id: u64, path: &Path) -> Result<(), String> {
        let path = absolute(path)?;
        if let Some(checkpoint) = self
            .manifest
            .checkpoints
            .iter_mut()
            .find(|checkpoint| checkpoint.id == id)
        {
            checkpoint
                .files
                .retain(|change| change.path != path || change.after_hash.is_some());
        }
        self.persist()
    }

    pub(crate) fn restore_files(&mut self, target_id: u64) -> Result<RestoreReport, String> {
        let target_is_recovery = self
            .checkpoint(target_id)
            .is_some_and(|checkpoint| checkpoint.recovery);
        let mut changes = self
            .manifest
            .checkpoints
            .iter()
            .filter(|checkpoint| {
                if target_is_recovery {
                    checkpoint.id == target_id
                } else {
                    checkpoint.id >= target_id && !checkpoint.recovery
                }
            })
            .rev()
            .flat_map(|checkpoint| checkpoint.files.iter().rev().cloned())
            .collect::<Vec<_>>();
        changes.sort_by_key(|change| std::cmp::Reverse(change.sequence));
        let mut report = RestoreReport {
            restored: 0,
            conflicts: Vec::new(),
        };
        let mut seen = std::collections::HashSet::new();
        for change in &changes {
            if !seen.insert(change.path.clone()) {
                continue;
            }
            let current = std::fs::read(&change.path).ok();
            let current_hash = current.as_deref().map(hash);
            if current_hash != change.after_hash {
                report.conflicts.push(change.path.clone());
            }
        }
        if !report.conflicts.is_empty() {
            return Ok(report);
        }
        for change in changes {
            if change.existed {
                let blob = change
                    .before_blob
                    .as_deref()
                    .ok_or_else(|| "checkpoint is missing a before blob".to_string())?;
                let bytes = std::fs::read(self.root.join("blobs").join(blob))
                    .map_err(|error| format!("read checkpoint blob {blob}: {error}"))?;
                if let Some(parent) = change.path.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|error| format!("create {}: {error}", parent.display()))?;
                }
                std::fs::write(&change.path, bytes)
                    .map_err(|error| format!("restore {}: {error}", change.path.display()))?;
            } else if change.path.exists() {
                std::fs::remove_file(&change.path)
                    .map_err(|error| format!("remove {}: {error}", change.path.display()))?;
            }
            report.restored += 1;
        }
        Ok(report)
    }

    pub(crate) fn begin_recovery(
        &mut self,
        target_id: u64,
        conversation_cutoff: usize,
        todos: Vec<crate::todos::Todo>,
    ) -> Result<u64, String> {
        let target_is_recovery = self
            .checkpoint(target_id)
            .is_some_and(|checkpoint| checkpoint.recovery);
        let paths = self
            .manifest
            .checkpoints
            .iter()
            .filter(|checkpoint| {
                if target_is_recovery {
                    checkpoint.id == target_id
                } else {
                    checkpoint.id >= target_id && !checkpoint.recovery
                }
            })
            .flat_map(|checkpoint| checkpoint.files.iter().map(|change| change.path.clone()))
            .collect::<std::collections::HashSet<_>>();
        let id = self.begin(String::new(), conversation_cutoff, todos)?;
        let checkpoint = self
            .manifest
            .checkpoints
            .iter_mut()
            .find(|checkpoint| checkpoint.id == id)
            .expect("new recovery checkpoint exists");
        checkpoint.recovery = true;
        checkpoint.label = Some(format!("Recovery before rewind to #{target_id}"));
        self.persist()?;
        for path in paths {
            self.record_before(id, &path)?;
        }
        Ok(id)
    }

    pub(crate) fn finalize_recovery(&mut self, id: u64) -> Result<(), String> {
        let paths = self
            .checkpoint(id)
            .map(|checkpoint| {
                checkpoint
                    .files
                    .iter()
                    .map(|change| change.path.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for path in paths {
            self.record_after(id, &path)?;
        }
        Ok(())
    }

    pub(crate) fn discard_checkpoint(&mut self, id: u64) -> Result<(), String> {
        self.manifest
            .checkpoints
            .retain(|checkpoint| checkpoint.id != id);
        self.persist()
    }

    /// Keep prompt cutoffs aligned when a compaction marker is inserted into
    /// the middle of the persisted conversation.
    pub(crate) fn record_conversation_insertion(&mut self, at: usize) -> Result<(), String> {
        for checkpoint in &mut self.manifest.checkpoints {
            if checkpoint.conversation_cutoff >= at {
                checkpoint.conversation_cutoff = checkpoint.conversation_cutoff.saturating_add(1);
            }
        }
        self.persist()
    }

    pub(crate) fn truncate_after(
        &mut self,
        target_id: u64,
        preserve_id: Option<u64>,
    ) -> Result<(), String> {
        self.manifest
            .checkpoints
            .retain(|checkpoint| checkpoint.id < target_id || Some(checkpoint.id) == preserve_id);
        self.persist()
    }

    pub(crate) fn delete_all(&mut self) -> Result<(), String> {
        if self.root.exists() {
            std::fs::remove_dir_all(&self.root)
                .map_err(|error| format!("delete checkpoints: {error}"))?;
        }
        self.manifest = Manifest::default();
        Ok(())
    }

    fn write_blob(&self, bytes: &[u8]) -> Result<String, String> {
        let digest = hash(bytes);
        let dir = self.root.join("blobs");
        std::fs::create_dir_all(&dir)
            .map_err(|error| format!("create checkpoint blobs: {error}"))?;
        let path = dir.join(&digest);
        if !path.exists() {
            std::fs::write(&path, bytes)
                .map_err(|error| format!("write checkpoint blob: {error}"))?;
        }
        Ok(digest)
    }

    fn persist(&self) -> Result<(), String> {
        std::fs::create_dir_all(&self.root)
            .map_err(|error| format!("create checkpoint directory: {error}"))?;
        let path = self.root.join("manifest.json");
        let temp = self.root.join("manifest.tmp");
        let json = serde_json::to_vec_pretty(&self.manifest)
            .map_err(|error| format!("serialize checkpoints: {error}"))?;
        std::fs::write(&temp, json)
            .map_err(|error| format!("write checkpoint manifest: {error}"))?;
        std::fs::rename(&temp, &path)
            .map_err(|error| format!("replace checkpoint manifest: {error}"))
    }
}

#[derive(Clone)]
pub(crate) struct CheckpointRecorder {
    pub(crate) store: std::sync::Arc<std::sync::Mutex<CheckpointStore>>,
    pub(crate) checkpoint_id: u64,
}

impl CheckpointRecorder {
    pub(crate) fn before_path(&self, path: &Path) -> Result<(), String> {
        self.store
            .lock()
            .map_err(|_| "checkpoint store lock poisoned".to_string())?
            .record_before(self.checkpoint_id, path)
    }

    pub(crate) fn after_path(&self, path: &Path) -> Result<(), String> {
        self.store
            .lock()
            .map_err(|_| "checkpoint store lock poisoned".to_string())?
            .record_after(self.checkpoint_id, path)
    }

    pub(crate) fn discard_path(&self, path: &Path) -> Result<(), String> {
        self.store
            .lock()
            .map_err(|_| "checkpoint store lock poisoned".to_string())?
            .discard_unfinished(self.checkpoint_id, path)
    }
}

pub(crate) fn path_from_tool_arguments(arguments: &str) -> Option<PathBuf> {
    serde_json::from_str::<serde_json::Value>(arguments)
        .ok()?
        .get("path")?
        .as_str()
        .map(PathBuf::from)
}

fn absolute(path: &Path) -> Result<PathBuf, String> {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| format!("current directory: {error}"))?
            .join(path)
    };
    if joined.exists() {
        return std::fs::canonicalize(&joined)
            .map_err(|error| format!("resolve {}: {error}", joined.display()));
    }
    let parent = joined.parent().unwrap_or_else(|| Path::new("."));
    let resolved_parent = std::fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf());
    Ok(joined
        .file_name()
        .map_or(resolved_parent.clone(), |name| resolved_parent.join(name)))
}

fn hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (PathBuf, CheckpointStore) {
        let root =
            std::env::temp_dir().join(format!("programmer-checkpoint-{}", uuid::Uuid::new_v4()));
        let store = CheckpointStore::at(root.clone());
        (root, store)
    }

    #[test]
    fn restores_multiple_versions_in_reverse_order() {
        let (root, mut store) = temp_store();
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("file.txt");
        std::fs::write(&file, b"a").unwrap();

        let first = store.begin("first".into(), 0, Vec::new()).unwrap();
        store.record_before(first, &file).unwrap();
        std::fs::write(&file, b"b").unwrap();
        store.record_after(first, &file).unwrap();

        let second = store.begin("second".into(), 0, Vec::new()).unwrap();
        store.record_before(second, &file).unwrap();
        std::fs::write(&file, b"c").unwrap();
        store.record_after(second, &file).unwrap();

        let report = store.restore_files(first).unwrap();
        assert!(report.conflicts.is_empty());
        assert_eq!(std::fs::read(&file).unwrap(), b"a");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn external_changes_abort_before_any_file_is_restored() {
        let (root, mut store) = temp_store();
        std::fs::create_dir_all(&root).unwrap();
        let first_file = root.join("first.txt");
        let second_file = root.join("second.txt");
        std::fs::write(&first_file, b"before-1").unwrap();
        std::fs::write(&second_file, b"before-2").unwrap();
        let checkpoint = store.begin("change".into(), 0, Vec::new()).unwrap();
        for (path, after) in [
            (&first_file, b"after-1".as_slice()),
            (&second_file, b"after-2".as_slice()),
        ] {
            store.record_before(checkpoint, path).unwrap();
            std::fs::write(path, after).unwrap();
            store.record_after(checkpoint, path).unwrap();
        }
        std::fs::write(&second_file, b"external").unwrap();

        let report = store.restore_files(checkpoint).unwrap();
        assert_eq!(
            report.conflicts,
            vec![std::fs::canonicalize(&second_file).unwrap()]
        );
        assert_eq!(std::fs::read(&first_file).unwrap(), b"after-1");
        assert_eq!(std::fs::read(&second_file).unwrap(), b"external");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovery_checkpoint_can_undo_a_rewind() {
        let (root, mut store) = temp_store();
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("file.txt");
        std::fs::write(&file, b"before").unwrap();
        let checkpoint = store.begin("change".into(), 0, Vec::new()).unwrap();
        store.record_before(checkpoint, &file).unwrap();
        std::fs::write(&file, b"after").unwrap();
        store.record_after(checkpoint, &file).unwrap();

        let recovery = store.begin_recovery(checkpoint, 0, Vec::new()).unwrap();
        assert!(
            store
                .restore_files(checkpoint)
                .unwrap()
                .conflicts
                .is_empty()
        );
        store.finalize_recovery(recovery).unwrap();
        store.truncate_after(checkpoint, Some(recovery)).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"before");

        assert!(store.restore_files(recovery).unwrap().conflicts.is_empty());
        assert_eq!(std::fs::read(&file).unwrap(), b"after");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn compaction_insertion_keeps_conversation_cutoffs_aligned() {
        let (root, mut store) = temp_store();
        let before = store.begin("before".into(), 2, Vec::new()).unwrap();
        let at = store.begin("at".into(), 5, Vec::new()).unwrap();
        let after = store.begin("after".into(), 8, Vec::new()).unwrap();

        store.record_conversation_insertion(5).unwrap();

        assert_eq!(store.checkpoint(before).unwrap().conversation_cutoff, 2);
        assert_eq!(store.checkpoint(at).unwrap().conversation_cutoff, 6);
        assert_eq!(store.checkpoint(after).unwrap().conversation_cutoff, 9);
        std::fs::remove_dir_all(root).unwrap();
    }
}
