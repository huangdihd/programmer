// Copyright (C) 2026 huangdihd
// SPDX-License-Identifier: GPL-3.0-or-later

//! Headless persistence: transient by default, writable on resume or explicit
//! save, and locked even for read-only resumes. Dropping never implicitly saves.

use color_eyre::eyre::{Result, eyre};

use crate::cli::AgentArgs;
use crate::config::programmer_config::ProgrammerConfig;
use crate::session::{
    AutoCompactOverride, ModelOverride, SessionLock, SessionLockError, SessionManager,
    SessionSnapshot, sessions_for_working_dir,
};

pub(crate) struct HeadlessSession {
    pub(crate) snapshot: SessionSnapshot,
    manager: SessionManager,
    _lock: SessionLock,
    uuid: String,
    writable: bool,
    restored: bool,
}

impl HeadlessSession {
    /// `None` means a new transient invocation. Missing resume/continue sources
    /// are errors, never a silent new conversation. Fork reads the last persisted
    /// source without locking it, and always persists via `SessionManager::fork`;
    /// consequently fork + no_save is rejected before touching disk.
    pub(crate) fn open(
        manager: SessionManager,
        resume: Option<&str>,
        continue_session: bool,
        fork: bool,
        no_save: bool,
        save_session: bool,
    ) -> Result<Option<Self>> {
        if resume.is_some() && continue_session {
            return Err(eyre!("--resume conflicts with --continue"));
        }
        if no_save && (fork || save_session) {
            return Err(eyre!("--no-save conflicts with --fork and --save-session"));
        }
        if (fork || no_save) && resume.is_none() && !continue_session {
            return Err(eyre!("--fork and --no-save require --resume or --continue"));
        }
        let cwd = std::env::current_dir()?;
        let source = if let Some(uuid) = resume {
            Some(uuid::Uuid::parse_str(uuid)?.to_string())
        } else if continue_session {
            let sessions = manager.list_all().map_err(|e| eyre!(e))?;
            Some(
                sessions_for_working_dir(&sessions, &cwd)
                    .first()
                    .ok_or_else(|| eyre!("no saved session in {} to continue", cwd.display()))?
                    .uuid
                    .clone(),
            )
        } else {
            None
        };
        let restored = source.is_some();
        let (snapshot, lock) = if let Some(uuid) = source {
            if fork {
                let source = load_required(&manager, &uuid)?;
                let snapshot = manager.fork(&source).map_err(|e| eyre!(e))?;
                let lock = lock(&manager, &snapshot.uuid)?;
                (snapshot, lock)
            } else {
                // Read only after locking: another writer may have saved since
                // listing. Even --no-save must not race a live session owner.
                let lock = lock(&manager, &uuid)?;
                let snapshot = load_required(&manager, &uuid)?;
                (snapshot, lock)
            }
        } else if save_session {
            let snapshot = manager.create();
            let lock = lock(&manager, &snapshot.uuid)?;
            (snapshot, lock)
        } else {
            return Ok(None);
        };
        Ok(Some(Self {
            uuid: snapshot.uuid.clone(),
            snapshot,
            manager,
            _lock: lock,
            writable: !no_save,
            restored,
        }))
    }

    /// Read-only sessions intentionally treat saving as a no-op.
    pub(crate) fn save(&mut self) -> Result<()> {
        if self.writable {
            if self.snapshot.uuid != self.uuid {
                return Err(eyre!("cannot change the UUID of a locked session"));
            }
            self.manager
                .save(&mut self.snapshot)
                .map_err(|e| eyre!(e))?;
        }
        Ok(())
    }

    /// Whether this run continues an existing conversation (resume, continue or fork).
    pub(crate) fn restored(&self) -> bool {
        self.restored
    }

    /// Only writable identities may be used for checkpoint persistence.
    pub(crate) fn session_id(&self) -> Option<&str> {
        self.writable.then_some(self.uuid.as_str())
    }

    /// Restore session settings without overriding explicit invocation flags.
    /// Fresh sessions leave config defaults untouched. Call before constructing
    /// the runtime and applying CLI overrides to the configuration.
    pub(crate) fn inherit_args(&self, args: &mut AgentArgs, config: &mut ProgrammerConfig) {
        if !self.restored {
            return;
        }
        if args.model.is_none() {
            args.model = self.snapshot.current_model.clone();
        }
        args.thinking.get_or_insert(self.snapshot.thinking_level);
        if args.work_mode.is_none() {
            args.work_mode = self.snapshot.work_mode;
        }
        if !args.vision && !args.no_vision {
            config.vision_enabled = self.snapshot.vision_enabled;
        }
        if args.classifier_model.is_none() {
            inherit_model(
                &self.snapshot.classifier_model_override,
                &mut config.classifier_model,
            );
        }
        inherit_model(
            &self.snapshot.compact_model_override,
            &mut config.compact_model,
        );
        match self.snapshot.auto_compact_override {
            AutoCompactOverride::Inherit => {}
            AutoCompactOverride::Disabled => config.auto_compact_tokens = 0,
            AutoCompactOverride::Tokens(tokens) => config.auto_compact_tokens = tokens,
        }
        if let Some(turns) = self.snapshot.compact_keep_recent_turns_override {
            config.compact_keep_recent_turns = turns;
        }
    }
}

fn inherit_model(saved: &ModelOverride, target: &mut Option<String>) {
    match saved {
        ModelOverride::Inherit => {}
        ModelOverride::Current => *target = None,
        ModelOverride::Model(model) => *target = Some(model.clone()),
    }
}

fn load_required(manager: &SessionManager, uuid: &str) -> Result<SessionSnapshot> {
    let snapshot = manager
        .load(uuid)
        .map_err(|e| eyre!("{e}"))?
        .ok_or_else(|| eyre!("session {uuid} was not found"))?;
    if snapshot.uuid != uuid {
        return Err(eyre!("session {uuid} contains a mismatched UUID"));
    }
    Ok(snapshot)
}

fn lock(manager: &SessionManager, uuid: &str) -> Result<SessionLock> {
    manager.try_lock_as(uuid, "headless").map_err(|error| match error {
        SessionLockError::InUse { pid } => {
            let owner = manager.lock_owner(uuid);
            eyre!(
                "session {uuid} is in use (pid: {}, process type: {}); use --fork to continue separately",
                pid.map_or_else(|| "unknown".into(), |pid| pid.to_string()),
                owner.as_ref().and_then(|owner| owner.process_type.as_deref()).unwrap_or("unknown")
            )
        }
        SessionLockError::Io(error) => eyre!(error),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Store(std::path::PathBuf);
    impl Store {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!(
                "programmer-headless-session-{}",
                uuid::Uuid::new_v4()
            )))
        }
        fn manager(&self) -> SessionManager {
            SessionManager::for_test(self.0.clone())
        }
        fn open(
            &self,
            resume: Option<&str>,
            continue_session: bool,
            no_save: bool,
            save_session: bool,
        ) -> Result<Option<HeadlessSession>> {
            HeadlessSession::open(
                self.manager(),
                resume,
                continue_session,
                false,
                no_save,
                save_session,
            )
        }
    }
    impl Drop for Store {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn roundtrip_readonly_and_lock_lifetime() {
        let store = Store::new();
        assert!(store.open(None, false, false, false).unwrap().is_none());
        assert!(!store.0.exists());
        let mut session = store.open(None, false, false, true).unwrap().unwrap();
        let id = session.session_id().unwrap().to_owned();
        session.snapshot.history.push("saved".into());
        session.save().unwrap();
        assert!(store.open(Some(&id), false, true, false).is_err());
        #[cfg(not(windows))]
        assert_eq!(
            store
                .manager()
                .lock_owner(&id)
                .unwrap()
                .process_type
                .as_deref(),
            Some("headless")
        );
        drop(session);
        let mut session = store.open(Some(&id), false, true, false).unwrap().unwrap();
        assert!(session.session_id().is_none());
        assert_eq!(session.snapshot.history, ["saved"]);
        session.snapshot.history.clear();
        session.save().unwrap();
        assert!(store.manager().try_lock(&id).is_err());
        drop(session);
        let session = store.open(Some(&id), false, false, false).unwrap().unwrap();
        assert_eq!(session.snapshot.history, ["saved"]);
    }

    #[test]
    fn continue_uses_latest_only_in_current_directory() {
        let store = Store::new();
        assert!(store.open(None, true, false, false).is_err());
        let manager = store.manager();
        let mut older = manager.create();
        manager.save(&mut older).unwrap();
        let mut latest = manager.create();
        manager.save(&mut latest).unwrap();
        latest.updated_at += 10;
        std::fs::write(
            manager.session_path(&latest.uuid),
            serde_json::to_vec(&latest).unwrap(),
        )
        .unwrap();
        let mut other = manager.create();
        other.working_dir = store.0.join("other-project").display().to_string();
        manager.save(&mut other).unwrap();
        other.updated_at += 20;
        std::fs::write(
            manager.session_path(&other.uuid),
            serde_json::to_vec(&other).unwrap(),
        )
        .unwrap();
        let session = store.open(None, true, false, false).unwrap().unwrap();
        assert_eq!(session.session_id(), Some(latest.uuid.as_str()));
        assert!(store.open(None, true, false, false).is_err());
    }

    #[test]
    fn fork_allows_locked_source_and_rejects_no_save() {
        let store = Store::new();
        let mut source = store.open(None, false, false, true).unwrap().unwrap();
        source.save().unwrap();
        let id = source.session_id().unwrap();
        assert!(
            HeadlessSession::open(store.manager(), Some(id), false, true, true, false).is_err()
        );
        let fork = HeadlessSession::open(store.manager(), Some(id), false, true, false, false)
            .unwrap()
            .unwrap();
        assert_ne!(fork.session_id(), source.session_id());
        assert!(
            store
                .manager()
                .load(fork.session_id().unwrap())
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn inherit_saved_settings_but_keep_explicit_flags() {
        let store = Store::new();
        let mut session = store.open(None, false, false, true).unwrap().unwrap();
        let mut config = ProgrammerConfig::default();
        #[derive(clap::Parser)]
        struct TestCli {
            #[command(flatten)]
            args: AgentArgs,
        }
        let mut args = <TestCli as clap::Parser>::parse_from(["test"]).args;
        session.inherit_args(&mut args, &mut config);
        assert!(args.thinking.is_none());
        session.restored = true;
        session.snapshot.current_model = Some("provider/saved".into());
        session.snapshot.thinking_level = crate::thinking::ThinkingLevel::High;
        session.snapshot.work_mode = Some(crate::classifier::WorkMode::Auto);
        session.snapshot.vision_enabled = false;
        session.snapshot.auto_compact_override = AutoCompactOverride::Disabled;
        session.snapshot.classifier_model_override = ModelOverride::Current;
        config.classifier_model = Some("provider/default".into());
        session.inherit_args(&mut args, &mut config);
        assert_eq!(args.model.as_deref(), Some("provider/saved"));
        assert_eq!(args.thinking, Some(crate::thinking::ThinkingLevel::High));
        assert_eq!(args.work_mode, Some(crate::classifier::WorkMode::Auto));
        assert!(!config.vision_enabled);
        assert!(config.classifier_model.is_none());
        assert_eq!(config.auto_compact_tokens, 0);
        args.model = Some("provider/explicit".into());
        args.thinking = Some(crate::thinking::ThinkingLevel::None);
        args.vision = true;
        config.vision_enabled = true;
        session.inherit_args(&mut args, &mut config);
        assert_eq!(args.model.as_deref(), Some("provider/explicit"));
        assert_eq!(args.thinking, Some(crate::thinking::ThinkingLevel::None));
        assert!(config.vision_enabled);
    }
}
