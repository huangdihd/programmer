// Copyright (C) 2026 huangdihd
// SPDX-License-Identifier: GPL-3.0-or-later

//! Presence means a user-opened TUI, never an offline inquiry worker.
use fs2::FileExt;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

pub(crate) struct Presence(File);

fn root() -> Result<PathBuf, String> {
    Ok(super::store::root()?.with_file_name("peer-online"))
}

fn open(root: &Path, session: &str) -> Result<File, String> {
    super::store::validate_uuid(session)?;
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(root).map_err(|e| e.to_string())?;
    if !std::fs::symlink_metadata(root)
        .map_err(|e| e.to_string())?
        .file_type()
        .is_dir()
    {
        return Err("Online presence root must be a directory, not a symlink".into());
    }
    let path = root.join(format!("{session}.lock"));
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err("Online presence lock must be a regular file, not a symlink".into());
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&path).map_err(|e| e.to_string())?;
    let metadata = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
    if !metadata.file_type().is_file() {
        return Err("Online presence lock changed while opening".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let opened = file.metadata().map_err(|e| e.to_string())?;
        if opened.dev() != metadata.dev() || opened.ino() != metadata.ino() || opened.nlink() != 1 {
            return Err("Online presence lock changed or is hard-linked".into());
        }
    }
    Ok(file)
}

impl Presence {
    pub(crate) fn register(session: &str) -> Result<Self, String> {
        Self::register_at(&root()?, session)
    }

    fn register_at(root: &Path, session: &str) -> Result<Self, String> {
        let file = open(root, session)?;
        file.try_lock_exclusive()
            .map_err(|e| format!("Cannot register online session: {e}"))?;
        Ok(Self(file))
    }
}

impl Drop for Presence {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

pub(crate) fn is_online(session: &str) -> Result<bool, String> {
    is_online_at(&root()?, session)
}

fn is_online_at(root: &Path, session: &str) -> Result<bool, String> {
    let file = open(root, session)?;
    match file.try_lock_exclusive() {
        Ok(()) => {
            let _ = FileExt::unlock(&file);
            Ok(false)
        }
        Err(error)
            if error.kind() == std::io::ErrorKind::WouldBlock
                || cfg!(windows) && error.raw_os_error() == Some(33) =>
        {
            Ok(true)
        }
        Err(error) => Err(format!("Cannot check online session: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!("peer-online-test-{}", uuid::Uuid::new_v4())))
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn presence_tracks_lock_lifetime_not_file_existence() {
        let root = TempRoot::new();
        let session = uuid::Uuid::new_v4().to_string();
        assert!(!is_online_at(&root.0, &session).unwrap());
        let presence = Presence::register_at(&root.0, &session).unwrap();
        assert!(is_online_at(&root.0, &session).unwrap());
        assert!(Presence::register_at(&root.0, &session).is_err());
        assert!(!is_online_at(&root.0, &uuid::Uuid::new_v4().to_string()).unwrap());
        drop(presence);
        assert!(root.0.join(format!("{session}.lock")).is_file());
        assert!(!is_online_at(&root.0, &session).unwrap());
        assert!(Presence::register_at(&root.0, &session).is_ok());
        assert!(is_online_at(&root.0, "../invalid").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_lock_is_rejected_without_touching_target() {
        let root = TempRoot::new();
        std::fs::create_dir_all(&root.0).unwrap();
        let session = uuid::Uuid::new_v4().to_string();
        let target = root.0.join("target");
        std::fs::write(&target, "unchanged").unwrap();
        std::os::unix::fs::symlink(&target, root.0.join(format!("{session}.lock"))).unwrap();
        assert!(Presence::register_at(&root.0, &session).is_err());
        assert!(is_online_at(&root.0, &session).is_err());
        assert_eq!(std::fs::read_to_string(target).unwrap(), "unchanged");
    }
}
