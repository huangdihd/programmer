// Copyright (C) 2026 huangdihd
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Session persistence: save, delete, and config persistence.

use super::App;
use crate::response::message_item::MessageItem;
use crate::session::{SessionManager, SessionSnapshot};
use crate::ui::components::conversation_panel::conversation_panel::ActivePhase;

use super::helpers;

/// Already-loaded startup data. Construction never reads session storage;
/// callers must obtain resumed snapshots while holding the session lock.
pub(crate) enum SessionSeed {
    Fresh { uuid: String },
    Restored(Box<SessionSnapshot>),
}

impl SessionSeed {
    /// The caller owns the session lock; errors must abort startup, not fall
    /// back to an empty session that could overwrite an unreadable archive.
    pub(crate) fn load(
        manager: &SessionManager,
        uuid: String,
    ) -> Result<Self, crate::session::SessionLoadError> {
        match manager.load(&uuid)? {
            Some(snapshot) => Ok(Self::Restored(Box::new(snapshot))),
            None => Ok(Self::Fresh { uuid }),
        }
    }
}

/// Track unsaved changes separately from permission to attempt an idle save.
/// Failed writes wait for a new change or an explicit save, never a timer.
#[derive(Default)]
pub(crate) struct PersistenceState {
    pending_attempt: bool,
    reported_error: Option<String>,
    pub(crate) load_error: Option<String>,
}

impl PersistenceState {
    fn ready(&self) -> bool {
        self.pending_attempt
    }

    fn mark_dirty(&mut self, dirty: &mut bool) {
        *dirty = true;
        self.pending_attempt = true;
    }

    fn failed(&mut self, error: &str) -> bool {
        self.pending_attempt = false;
        if self.reported_error.as_deref() == Some(error) {
            return false;
        }
        self.reported_error = Some(error.to_owned());
        true
    }

    fn committed(&mut self, dirty: &mut bool) {
        *dirty = false;
        self.pending_attempt = false;
        self.reported_error = None;
    }
}

/// Mark the session as needing a save. Cheap: the actual disk write is deferred
/// to the next idle tick (see [`flush_if_dirty`]), so many state changes across
/// a single turn collapse into one save when the turn finishes.
pub(crate) fn mark_dirty(app: &mut App<'_>) {
    app.agent_loop
        .session
        .persistence
        .mark_dirty(&mut app.agent_loop.session.dirty);
}

/// Attempt a pending save once when no turn is in flight. Only a successful
/// write clears dirty; failures wait for a new change or explicit save.
/// Called from the tick handler, this debounces saves to turn boundaries:
/// while a response is streaming or tools are running the app is never idle, so
/// nothing is written until everything settles.
pub(crate) fn flush_if_dirty(app: &mut App<'_>) {
    if app.agent_loop.session.dirty
        && app.agent_loop.session.persistence.ready()
        && app.ui.conversation_panel.receiving_response.is_none()
        && app.agent_loop.phase == ActivePhase::None
    {
        save_session(app);
    }
}

/// Persist the current conversation to the session file.
pub(crate) fn save_session(app: &mut App<'_>) {
    if let Err(e) = persist_session(app)
        && app.agent_loop.session.persistence.failed(&e)
    {
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_error_string(format!("session save: {e}"));
        app.ui.conversation_panel.scroll_to_bottom();
    }
}

/// Persist the current session and report failures to callers that must not
/// proceed without a durable source snapshot (notably conversation forks).
pub(crate) fn save_session_checked(app: &mut App<'_>) -> Result<(), String> {
    if persist_session(app).inspect_err(|error| {
        app.agent_loop.session.persistence.failed(error);
    })? {
        Ok(())
    } else {
        Err("the current session has no persistable user input".to_string())
    }
}

pub(crate) fn persist_session(app: &mut App<'_>) -> Result<bool, String> {
    // Explicit saves may attempt unchanged failed state. Consume the pending
    // attempt even for errors or empty snapshots, but acknowledge only writes.
    app.agent_loop.session.dirty = true;
    app.agent_loop.session.persistence.pending_attempt = false;
    app.sync_todos_from_store();
    let Some(mut snapshot) = build_snapshot(app)? else {
        return Ok(false);
    };
    let manager = app
        .agent_loop
        .session
        .mgr
        .as_ref()
        .ok_or("session persistence unavailable")?;
    commit_snapshot(
        manager,
        &mut snapshot,
        &mut app.agent_loop.session.persistence,
        &mut app.agent_loop.session.dirty,
    )?;
    app.agent_loop.session.did_save = true;
    Ok(true)
}

fn commit_snapshot(
    manager: &SessionManager,
    snapshot: &mut SessionSnapshot,
    retry: &mut PersistenceState,
    dirty: &mut bool,
) -> Result<(), String> {
    manager.save(snapshot)?;
    retry.committed(dirty);
    Ok(())
}

/// Build owned persisted data without acknowledging any pending changes.
fn build_snapshot(app: &App<'_>) -> Result<Option<SessionSnapshot>, String> {
    if let Some(error) = &app.agent_loop.session.persistence.load_error {
        return Err(format!(
            "session restore failed; saving is blocked: {error}"
        ));
    }
    let mut items: Vec<MessageItem> = app
        .agent_loop
        .session
        .conversation
        .lock()
        .unwrap()
        .items
        .clone();
    remove_transient_items(&mut items);
    // Don't persist a session with no user input — there's nothing worth
    // resuming, and empty sessions only clutter the picker. `/init` sends a
    // (developer-role) input message, which `first_user_text` picks up, so a
    // session that ran `/init` still counts as having input.
    if helpers::first_user_text(&items).is_none() {
        return Ok(None);
    }
    let Some(mgr) = &app.agent_loop.session.mgr else {
        return Err("session persistence unavailable".to_string());
    };
    let mut session = mgr
        .load(&app.agent_loop.session.uuid)
        .map_err(|error| error.to_string())?
        .unwrap_or_else(|| {
            let mut s = mgr.create();
            s.uuid = app.agent_loop.session.uuid.clone();
            s
        });
    session.title = app.agent_loop.session.title.clone();
    // Capture first user message for the picker preview.
    if session.first_message.is_empty()
        && let Some(text) = helpers::first_user_text(&items)
    {
        session.first_message = crate::session::truncate_first_line(&text, 80);
    }
    session.session_memory = items.iter().rev().find_map(|item| match item {
        MessageItem::Compacted { summary } => crate::memory::SessionMemory::from_summary(summary),
        _ => None,
    });
    SessionManager::set_items(&mut session, items);
    session.history = app.ui.input_panel.history.clone();
    session.input_suggestion = app.ui.input_panel.suggestion().map(str::to_owned);
    session.work_mode = Some(app.agent_loop.session.work_mode);
    session.current_model = Some(app.agent_loop.session.current_model.clone());
    session.last_request_input_tokens = app
        .agent_loop
        .session
        .conversation
        .lock()
        .unwrap()
        .usage_summary()
        .last_request_input_tokens;
    session.vision_enabled = app.agent_loop.session.vision_enabled;
    session.thinking_level = app.agent_loop.session.thinking_level;
    session.classifier_model_override = app.agent_loop.session.classifier_model_override.clone();
    session.compact_model_override = app.agent_loop.session.compact_model_override.clone();
    session.auto_compact_override = app.agent_loop.session.auto_compact_override.clone();
    session.compact_keep_recent_turns_override =
        app.agent_loop.session.compact_keep_recent_turns_override;
    session.todos = app
        .agent_loop
        .session
        .todo_store
        .lock()
        .unwrap()
        .todos
        .clone();
    session.activated_skills = app.agent_loop.skill_registry.activated_names().to_vec();
    session.skill_selection_saved = true;
    session.tasks = app.agent_loop.session.tasks.persist_all();
    session.agents = app.agent_loop.session.agents.persist_all();
    session.file_snapshots = app.security.snapshot().persisted_snapshots();
    Ok(Some(session))
}

fn remove_transient_items(items: &mut Vec<MessageItem>) {
    items.retain(|item| !super::events::is_quit_confirmation_warning(item));
}

/// Write the current config back to `config.toml` atomically.
pub(crate) fn persist_config(app: &mut App<'_>) {
    let Some(config_dir) = dirs::config_dir() else {
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_error_string("cannot locate the config directory");
        app.ui.conversation_panel.scroll_to_bottom();
        return;
    };
    let dir = config_dir.join("programmer");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("config.toml");
    let result = toml::to_string(&app.config)
        .map_err(|e| format!("serialize config: {e}"))
        .and_then(|s| {
            let tmp = path.with_extension("tmp");
            std::fs::write(&tmp, &s).map_err(|e| format!("write {}: {e}", tmp.display()))?;
            std::fs::rename(&tmp, &path).map_err(|e| format!("rename to {}: {e}", path.display()))
        });
    if let Err(e) = result {
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_error_string(format!("failed to save config: {e}"));
        app.ui.conversation_panel.scroll_to_bottom();
    }
}

/// Delete the session file and start a fresh session with a new UUID.
pub(crate) fn delete_session(app: &mut App<'_>) {
    app.ui.active_suggestion_operation_id = None;
    if let Some(cancel) = app.ui.input_suggestion_cancel.take() {
        cancel.cancel();
    }
    app.ui.input_panel.clear_suggestion();
    app.agent_loop.session.title.clear();
    app.agent_loop.session.title_generation_started = false;
    app.agent_loop.session.title_generation_id =
        app.agent_loop.session.title_generation_id.wrapping_add(1);
    if let Some(store) = &app.agent_loop.session.checkpoint_store {
        let _ = store.lock().unwrap().delete_all();
    }
    if let Some(mgr) = &app.agent_loop.session.mgr {
        let _ = mgr.delete(&app.agent_loop.session.uuid);
        let new_session = mgr.create();
        app.agent_loop.session.uuid = new_session.uuid;
        app.agent_loop.session.persistence = PersistenceState::default();
    }
    app.agent_loop.session.checkpoint_store =
        crate::checkpoint::CheckpointStore::for_session(&app.agent_loop.session.uuid)
            .map(|store| std::sync::Arc::new(std::sync::Mutex::new(store)));
    app.agent_loop.current_checkpoint_id = None;
}

#[cfg(test)]
mod tests {
    use super::remove_transient_items;
    use crate::app::events::QUIT_CONFIRM_WARNING;
    use crate::response::message_item::MessageItem;

    #[tokio::test]
    async fn hydration_restores_one_loaded_snapshot_without_reading_disk_again() {
        use crate::session::{AutoCompactOverride, ModelOverride, SessionManager};
        let directory =
            std::env::temp_dir().join(format!("programmer-hydration-{}", uuid::Uuid::new_v4()));
        let manager = SessionManager::for_test(directory.clone());
        let mut snapshot = manager.create();
        snapshot.title = "loaded title".into();
        snapshot.history = vec!["previous input".into()];
        snapshot.work_mode = Some(crate::classifier::WorkMode::Plan);
        snapshot.vision_enabled = true;
        snapshot.thinking_level = crate::thinking::ThinkingLevel::High;
        snapshot.classifier_model_override = ModelOverride::Current;
        snapshot.compact_model_override = ModelOverride::Model("offline/compact".into());
        snapshot.auto_compact_override = AutoCompactOverride::Tokens(3210);
        snapshot.compact_keep_recent_turns_override = Some(7);
        snapshot.last_request_input_tokens = Some(456);
        snapshot.skill_selection_saved = true;
        snapshot.activated_skills.clear();
        let mut todos = crate::todos::TodoList::default();
        todos.add("restored todo".into(), None);
        snapshot.todos = todos.todos;
        snapshot.tasks.push(crate::tasks::PersistedTask {
            id: 17,
            name: "restored task".into(),
            command: "echo example".into(),
            status: "running".into(),
            exit_code: None,
            elapsed_secs: 5,
            output: "old output".into(),
        });
        manager.save(&mut snapshot).unwrap();
        let lock = manager.try_lock(&snapshot.uuid).unwrap();
        let seed = super::SessionSeed::load(&manager, snapshot.uuid.clone()).unwrap();
        // A constructor re-read would fail or lose every restored setting.
        std::fs::write(manager.session_path(&snapshot.uuid), b"not JSON").unwrap();
        let mut configuration = crate::config::programmer_config::ProgrammerConfig::default();
        configuration.providers.clear();
        configuration.vision_enabled = false;
        configuration.memory.dream_enabled = false;
        let mut app = crate::app::App::new(
            configuration,
            seed,
            Some(manager),
            Vec::new(),
            false,
            "test".into(),
        )
        .await;
        let session = &app.agent_loop.session;
        assert_eq!(session.uuid, snapshot.uuid);
        assert_eq!(session.title, "loaded title");
        assert_eq!(session.work_mode, crate::classifier::WorkMode::Plan);
        assert!(session.vision_enabled);
        assert_eq!(session.thinking_level, crate::thinking::ThinkingLevel::High);
        assert_eq!(session.classifier_model_override, ModelOverride::Current);
        assert_eq!(
            session.compact_model_override,
            ModelOverride::Model("offline/compact".into())
        );
        assert_eq!(
            session.auto_compact_override,
            AutoCompactOverride::Tokens(3210)
        );
        assert_eq!(session.compact_keep_recent_turns_override, Some(7));
        assert_eq!(
            session
                .conversation
                .lock()
                .unwrap()
                .last_request_input_tokens,
            Some(456)
        );
        assert_eq!(
            session.todo_store.lock().unwrap().todos[0].title,
            "restored todo"
        );
        let tasks = session.tasks.persist_all();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, 17);
        assert_eq!(tasks[0].status, "killed");
        assert_eq!(tasks[0].output, "old output");
        assert_eq!(app.ui.input_panel.history, ["previous input"]);
        assert!(app.agent_loop.skill_registry.activated_names().is_empty());
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_input_message(async_openai::types::responses::MessageItem::Input(
                async_openai::types::responses::InputMessage {
                    content: vec![async_openai::types::responses::InputContent::InputText(
                        "new input".into(),
                    )],
                    role: async_openai::types::responses::InputRole::User,
                    status: None,
                },
            ));
        app.ui.conversation_panel.scroll_to_bottom();
        assert!(super::save_session_checked(&mut app).is_err());
        let manager = app.agent_loop.session.mgr.as_ref().unwrap();
        assert_eq!(
            std::fs::read(manager.session_path(&snapshot.uuid)).unwrap(),
            b"not JSON"
        );
        drop(app);
        drop(lock);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn hydration_preserves_legacy_skill_defaults_and_explicit_selection() {
        let directory = std::env::temp_dir().join(format!(
            "programmer-hydration-skills-{}",
            uuid::Uuid::new_v4()
        ));
        let manager = crate::session::SessionManager::for_test(directory);
        let selected = crate::skills::INITIALIZE_PROJECT_SKILL.to_string();
        for selection in [None, Some(vec![selected.clone()])] {
            let mut snapshot = manager.create();
            snapshot.activated_skills = selection.clone().unwrap_or_default();
            // Legacy files with nonempty activation also restore that selection.
            snapshot.skill_selection_saved = false;
            let mut configuration = crate::config::programmer_config::ProgrammerConfig::default();
            configuration.providers.clear();
            configuration.memory.dream_enabled = false;
            let app = crate::app::App::new(
                configuration,
                super::SessionSeed::Restored(Box::new(snapshot)),
                None,
                Vec::new(),
                false,
                "test".into(),
            )
            .await;
            let skills = &app.agent_loop.skill_registry;
            match selection {
                Some(expected) => assert_eq!(skills.activated_names(), expected),
                None => assert_eq!(skills.activated_names().len(), skills.names().len()),
            }
        }
    }

    #[test]
    fn hydration_load_error_preserves_archive_and_missing_session_is_fresh() {
        let directory = std::env::temp_dir().join(format!(
            "programmer-hydration-error-{}",
            uuid::Uuid::new_v4()
        ));
        let manager = crate::session::SessionManager::for_test(directory.clone());
        let uuid = uuid::Uuid::new_v4().to_string();
        assert!(matches!(
            super::SessionSeed::load(&manager, uuid.clone()).unwrap(),
            super::SessionSeed::Fresh { .. }
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = manager.session_path(&uuid);
        std::fs::write(&path, b"broken archive").unwrap();
        let lock = manager.try_lock(&uuid).unwrap();
        assert!(super::SessionSeed::load(&manager, uuid).is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"broken archive");
        drop(lock);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn persistence_empty_session_without_manager_is_a_silent_noop() {
        let mut configuration = crate::config::programmer_config::ProgrammerConfig::default();
        configuration.providers.clear();
        let mut app = crate::app::App::new(
            configuration,
            crate::app::session::SessionSeed::Fresh {
                uuid: "empty-save-test".to_string(),
            },
            None,
            Vec::new(),
            false,
            "test".to_string(),
        )
        .await;
        assert!(app.agent_loop.session.mgr.is_none());
        app.agent_loop.session.conversation.lock().unwrap().clear();
        app.ui.conversation_panel.clear_view();
        super::mark_dirty(&mut app);
        super::save_session(&mut app);
        assert!(app.ui.conversation_panel.items_snapshot().is_empty());
        assert!(app.agent_loop.session.persistence.reported_error.is_none());
        assert!(app.agent_loop.session.dirty);
        assert!(!app.agent_loop.session.did_save);
        assert!(!app.agent_loop.session.persistence.ready());
        assert_eq!(
            super::save_session_checked(&mut app).unwrap_err(),
            "the current session has no persistable user input"
        );
        assert!(app.agent_loop.session.dirty);
        assert!(!app.agent_loop.session.persistence.ready());
    }

    #[test]
    fn persistence_commit_failure_keeps_dirty_until_real_write_succeeds() {
        let directory = std::env::temp_dir().join(format!(
            "programmer-commit-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let manager = crate::session::SessionManager::for_test(directory.clone());
        let mut snapshot = manager.create();
        let mut retry = super::PersistenceState::default();
        let mut dirty = true;
        std::fs::write(&directory, b"not a directory").unwrap();
        assert!(super::commit_snapshot(&manager, &mut snapshot, &mut retry, &mut dirty).is_err());
        assert!(dirty);
        std::fs::remove_file(&directory).unwrap();
        super::commit_snapshot(&manager, &mut snapshot, &mut retry, &mut dirty).unwrap();
        assert!(!dirty);
        assert!(manager.load(&snapshot.uuid).unwrap().is_some());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn persistence_failed_save_stays_dirty_and_waits_for_new_change() {
        let mut retry = super::PersistenceState::default();
        let mut dirty = false;
        assert!(!retry.ready());
        retry.mark_dirty(&mut dirty);
        assert!(retry.ready());
        assert!(retry.failed("disk full"));
        assert!(dirty);
        assert!(!retry.ready());
        assert!(!retry.failed("disk full"));
        assert!(retry.failed("permission denied"));
        retry.mark_dirty(&mut dirty);
        assert!(retry.ready());
        assert_eq!(retry.reported_error.as_deref(), Some("permission denied"));
        retry.committed(&mut dirty);
        assert!(!dirty);
        assert!(!retry.ready());
        assert!(retry.reported_error.is_none());
        assert!(retry.failed("disk full"));
    }

    #[tokio::test]
    async fn persistence_idle_ticks_do_not_retry_but_new_changes_and_explicit_saves_do() {
        use async_openai::types::responses::{
            InputMessage, InputRole, MessageItem as ApiMessageItem,
        };

        let mut configuration = crate::config::programmer_config::ProgrammerConfig::default();
        configuration.providers.clear();
        let mut app = crate::app::App::new(
            configuration,
            crate::app::session::SessionSeed::Fresh {
                uuid: "save-attempt-test".to_string(),
            },
            None,
            Vec::new(),
            false,
            "test".to_string(),
        )
        .await;
        let directory =
            std::env::temp_dir().join(format!("programmer-save-attempt-{}", uuid::Uuid::new_v4()));
        app.agent_loop.session.mgr =
            Some(crate::session::SessionManager::for_test(directory.clone()));
        app.agent_loop.session.persistence = super::PersistenceState::default();
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_input_message(ApiMessageItem::Input(InputMessage {
                content: vec![async_openai::types::responses::InputContent::InputText(
                    async_openai::types::responses::InputTextContent {
                        text: "persist me".to_string(),
                    },
                )],
                role: InputRole::User,
                status: None,
            }));
        app.ui.conversation_panel.scroll_to_bottom();
        std::fs::write(&directory, b"not a directory").unwrap();
        super::mark_dirty(&mut app);
        super::flush_if_dirty(&mut app);
        assert!(app.agent_loop.session.dirty);
        assert!(app.agent_loop.session.persistence.reported_error.is_some());
        let item_count = app.ui.conversation_panel.items_snapshot().len();
        std::fs::remove_file(&directory).unwrap();

        for _ in 0..10 {
            super::flush_if_dirty(&mut app);
        }
        assert!(
            !directory.exists(),
            "idle ticks must not attempt another write"
        );
        assert!(app.agent_loop.session.dirty);
        assert!(app.agent_loop.session.persistence.reported_error.is_some());
        assert_eq!(app.ui.conversation_panel.items_snapshot().len(), item_count);

        super::mark_dirty(&mut app);
        super::flush_if_dirty(&mut app);
        assert!(!app.agent_loop.session.dirty);
        assert!(app.agent_loop.session.persistence.reported_error.is_none());
        assert!(app.agent_loop.session.did_save);

        // A checked save must also consume the pending attempt on failure.
        app.agent_loop.session.persistence.load_error = Some("restore failed".to_string());
        super::mark_dirty(&mut app);
        assert!(super::save_session_checked(&mut app).is_err());
        assert!(app.agent_loop.session.dirty);
        assert!(!app.agent_loop.session.persistence.ready());
        assert!(app.agent_loop.session.persistence.reported_error.is_some());
        app.agent_loop.session.persistence.load_error = None;
        super::save_session_checked(&mut app).unwrap();
        assert!(!app.agent_loop.session.dirty);
        assert!(app.agent_loop.session.persistence.reported_error.is_none());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn quit_confirmation_warning_is_not_persisted() {
        let mut items = vec![
            MessageItem::Warning(QUIT_CONFIRM_WARNING.to_string()),
            MessageItem::Warning("keep this warning".to_string()),
        ];

        remove_transient_items(&mut items);

        assert_eq!(items.len(), 1);
        assert!(matches!(
            &items[0],
            MessageItem::Warning(text) if text == "keep this warning"
        ));
    }
}
