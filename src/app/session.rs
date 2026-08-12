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
use crate::session::SessionManager;
use crate::ui::components::conversation_panel::conversation_panel::ActivePhase;

use super::helpers;

/// Mark the session as needing a save. Cheap: the actual disk write is deferred
/// to the next idle tick (see [`flush_if_dirty`]), so many state changes across
/// a single turn collapse into one save when the turn finishes.
pub(crate) fn mark_dirty(app: &mut App<'_>) {
    app.session.dirty = true;
}

/// If the session is dirty and no turn is in flight, persist it and clear the
/// flag. Called from the tick handler, this debounces saves to turn boundaries:
/// while a response is streaming or tools are running the app is never idle, so
/// nothing is written until everything settles.
pub(crate) fn flush_if_dirty(app: &mut App<'_>) {
    if app.session.dirty
        && app.conversation_panel.receiving_response.is_none()
        && app.conversation_panel.phase == ActivePhase::None
    {
        save_session(app);
    }
}

/// Persist the current conversation to the session file.
pub(crate) fn save_session(app: &mut App<'_>) {
    if let Err(e) = persist_session(app) {
        app.conversation_panel
            .add_error_string(format!("session save: {e}"));
    }
}

/// Persist the current session and report failures to callers that must not
/// proceed without a durable source snapshot (notably conversation forks).
pub(crate) fn save_session_checked(app: &mut App<'_>) -> Result<(), String> {
    if persist_session(app)? {
        Ok(())
    } else {
        Err("the current session has no persistable user input".to_string())
    }
}

fn persist_session(app: &mut App<'_>) -> Result<bool, String> {
    app.session.dirty = false;
    app.sync_todos_from_store();
    let Some(mgr) = &app.session.mgr else {
        return Ok(false);
    };
    let mut items: Vec<MessageItem> = app.conversation_panel.items_snapshot();
    remove_transient_items(&mut items);
    // Don't persist a session with no user input — there's nothing worth
    // resuming, and empty sessions only clutter the picker. `/init` sends a
    // (developer-role) input message, which `first_user_text` picks up, so a
    // session that ran `/init` still counts as having input.
    if helpers::first_user_text(&items).is_none() {
        return Ok(false);
    }
    let mut session = mgr.load(&app.session.uuid).unwrap_or_else(|| {
        let mut s = mgr.create();
        s.uuid = app.session.uuid.clone();
        s
    });
    // Capture first user message for the picker preview.
    if session.first_message.is_empty()
        && let Some(text) = helpers::first_user_text(&items)
    {
        session.first_message = crate::session::truncate_first_line(&text, 80);
    }
    SessionManager::set_items(&mut session, items);
    session.history = app.input_panel.history.clone();
    session.work_mode = Some(app.work_mode);
    session.current_model = Some(app.current_model.clone());
    session.vision_enabled = app.vision_enabled;
    session.thinking_level = app.thinking_level;
    session.classifier_model_override = app.session.classifier_model_override.clone();
    session.classifier_top_logprobs_override = app.session.classifier_top_logprobs_override;
    session.compact_model_override = app.session.compact_model_override.clone();
    session.auto_compact_override = app.session.auto_compact_override.clone();
    session.compact_keep_recent_turns_override = app.session.compact_keep_recent_turns_override;
    session.todos = app.todo_list.todos.clone();
    session.activated_skills = app.skill_registry.activated_names().to_vec();
    session.skill_selection_saved = true;
    session.tasks = crate::tasks::persist_all();
    mgr.save(&mut session)?;
    app.session.did_save = true;
    Ok(true)
}

fn remove_transient_items(items: &mut Vec<MessageItem>) {
    items.retain(|item| !super::events::is_quit_confirmation_warning(item));
}

/// Write the current config back to `config.toml` atomically.
pub(crate) fn persist_config(app: &mut App<'_>) {
    let Some(config_dir) = dirs::config_dir() else {
        app.conversation_panel
            .add_error_string("cannot locate the config directory");
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
        app.conversation_panel
            .add_error_string(format!("failed to save config: {e}"));
    }
}

/// Delete the session file and start a fresh session with a new UUID.
pub(crate) fn delete_session(app: &mut App<'_>) {
    if let Some(store) = &app.checkpoint_store {
        let _ = store.lock().unwrap().delete_all();
    }
    if let Some(mgr) = &app.session.mgr {
        let _ = mgr.delete(&app.session.uuid);
        let new_session = mgr.create();
        app.session.uuid = new_session.uuid;
    }
    app.checkpoint_store = crate::checkpoint::CheckpointStore::for_session(&app.session.uuid)
        .map(|store| std::sync::Arc::new(std::sync::Mutex::new(store)));
    app.current_checkpoint_id = None;
}

#[cfg(test)]
mod tests {
    use super::remove_transient_items;
    use crate::app::events::QUIT_CONFIRM_WARNING;
    use crate::response::message_item::MessageItem;

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
