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
    app.session.dirty = false;
    let Some(mgr) = &app.session.mgr else { return };
    let items: Vec<MessageItem> = app.conversation_panel.items().cloned().collect();
    let mut session = mgr.load(&app.session.uuid).unwrap_or_else(|| {
        let mut s = mgr.create();
        s.uuid = app.session.uuid.clone();
        s
    });
    // Capture first user message for the picker preview.
    if session.first_message.is_empty() {
        if let Some(text) = helpers::first_user_text(&items) {
            session.first_message =
                crate::session::truncate_first_line(&text, 80);
        }
    }
    SessionManager::set_items(&mut session, items);
    session.history = app.input_panel.history.clone();
    session.work_mode = Some(app.work_mode);
    session.current_model = Some(app.current_model.clone());
    session.classifier_model = app.config.classifier_model.clone();
    session.todos = app.todo_list.todos.clone();
    session.activated_skills = app.skill_registry.activated_names().to_vec();
    session.tasks = crate::tasks::persist_all();
    if let Err(e) = mgr.save(&mut session) {
        app.conversation_panel
            .add_error_string(format!("session save: {e}"));
    }
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
            std::fs::write(&tmp, &s)
                .map_err(|e| format!("write {}: {e}", tmp.display()))?;
            std::fs::rename(&tmp, &path)
                .map_err(|e| format!("rename to {}: {e}", path.display()))
        });
    if let Err(e) = result {
        app.conversation_panel
            .add_error_string(format!("failed to save config: {e}"));
    }
}

/// Delete the session file and start a fresh session with a new UUID.
pub(crate) fn delete_session(app: &mut App<'_>) {
    if let Some(mgr) = &app.session.mgr {
        let _ = mgr.delete(&app.session.uuid);
        let new_session = mgr.create();
        app.session.uuid = new_session.uuid;
    }
}
