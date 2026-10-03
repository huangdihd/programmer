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

//! Keyboard and paste handling: modal panels first (approval, question,
//! plan review, todo/provider/skills/MCP panels), then global shortcuts,
//! then the input panel.

use super::super::{App, commands, session};
use super::update_completions;
use crate::classifier::WorkMode;
use crate::ui::components::provider_panel::PanelAction;
use crate::ui::event::AppEvent;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Handles the key events and updates the state of [`App`].
pub(crate) async fn handle_key_events(
    app: &mut App<'_>,
    key_event: KeyEvent,
) -> color_eyre::Result<()> {
    // ---- Esc while a turn is active: cancel unless an independent overlay
    // owns the key. Approval/question prompts belong to the active turn and
    // intentionally are not listed here, so Esc still cancels that turn.
    if key_event.code == KeyCode::Esc
        && app.agent_loop.cancel.active_id.is_some()
        && app.agent_loop.peer_consent.is_none()
        && !independent_overlay_owns_escape(app)
    {
        app.events.send(AppEvent::Cancel);
        return Ok(());
    }

    // ---- Ctrl+Z while a command tool is running: keep its process alive as
    // a background task. Do this before modal routing so the foreground turn
    // can finish and the promoted task can appear in the sidebar immediately.
    if is_promote_shortcut(key_event)
        && app
            .agent_loop
            .session
            .tasks
            .promote_running_command()
            .is_some()
    {
        return Ok(());
    }

    // ---- interactive terminal panel (fully modal; grabs input) ----
    if app.ui.terminal_pane.is_some() {
        handle_terminal_key(app, key_event);
        return Ok(());
    }

    if app.ui.activity_page.is_some() {
        super::super::activity::handle_key(app, key_event);
        return Ok(());
    }

    if let Some(panel) = app.ui.agent_panel.as_mut() {
        if panel.handle_key(key_event) {
            app.ui.agent_panel = None;
        }
        return Ok(());
    }

    if let Some(panel) = app.ui.rewind_panel.as_mut() {
        use crate::ui::components::rewind_panel::PanelAction as RewindAction;
        let action = panel.handle_key(key_event);
        match action {
            RewindAction::Close => app.ui.rewind_panel = None,
            RewindAction::Fork { checkpoint_id } => {
                app.ui.rewind_panel = None;
                apply_rewind_fork(app, checkpoint_id).await;
            }
            RewindAction::Restore {
                checkpoint_id,
                mode,
            } => {
                app.ui.rewind_panel = None;
                apply_rewind(app, checkpoint_id, mode);
            }
            RewindAction::None => {}
        }
        return Ok(());
    }

    // Questions own the rendered bottom surface. A newly arrived tool review
    // must not consume Enter while the user sees peer consent instead.
    // ---- question panel ----
    super::super::peers::refresh_consent_models(app);
    if let Some(panel) = app.ui.question_panel.as_mut() {
        match panel.handle_key(key_event) {
            crate::ui::components::question_panel::AnswerAction::Answer(text) => {
                panel.answer(text);
                app.ui.question_panel = None;
            }
            crate::ui::components::question_panel::AnswerAction::SelectModel(model) => {
                if let Err(error) =
                    super::super::command_handlers::settings::switch_model(app, &model)
                {
                    app.agent_loop
                        .session
                        .conversation
                        .lock()
                        .unwrap()
                        .add_error_string(error);
                    app.ui.conversation_panel.scroll_to_bottom();
                }
                super::super::peers::refresh_consent_models(app);
            }
            crate::ui::components::question_panel::AnswerAction::None => {}
        }
        return Ok(());
    }

    // ---- tool-call approval (Manual mode) ----
    if app.agent_loop.pending_review.is_some() {
        return handle_approval_key(app, key_event);
    }

    // ---- plan review (Plan mode) ----
    if app.agent_loop.session.work_mode == WorkMode::Plan
        && app.agent_loop.plan_phase == crate::classifier::PlanPhase::Reviewing
    {
        return handle_plan_review_key(app, key_event).await;
    }

    // ---- todo panel ----
    if let Some(panel) = app.ui.todo_panel.as_mut() {
        use crate::ui::components::todo_panel::PanelAction;
        let action = panel.handle_key(key_event);
        match action {
            PanelAction::Close => app.ui.todo_panel = None,
            PanelAction::None => return Ok(()),
            action => {
                let result = {
                    let mut list = app.agent_loop.session.todo_store.lock().unwrap();
                    match action {
                        PanelAction::Add { title, description } => {
                            list.add(title, description);
                            Ok(())
                        }
                        PanelAction::Toggle { id } => list.toggle_status(&id).map(|_| ()),
                        PanelAction::Delete { id } => list.delete(&id).map(|_| ()),
                        PanelAction::None | PanelAction::Close => unreachable!(),
                    }
                };
                if let Err(error) = result {
                    app.agent_loop
                        .session
                        .conversation
                        .lock()
                        .unwrap()
                        .add_error_string(error);
                    app.ui.conversation_panel.scroll_to_bottom();
                }
                app.sync_todos_from_store();
                session::mark_dirty(app);
            }
        }
        return Ok(());
    }

    // ---- sidebar keyboard (when focused) ----
    if app.ui.sidebar.as_ref().is_some_and(|s| s.has_focus) {
        if key_event.code == KeyCode::Esc {
            if let Some(ref mut s) = app.ui.sidebar {
                s.has_focus = false;
            }
            return Ok(());
        }
        if let Some(ref mut s) = app.ui.sidebar {
            let visible_lines = s
                .area()
                .map_or(1, |area| area.height.saturating_sub(1).max(1));
            s.handle_key(key_event, visible_lines);
        }
        return Ok(());
    }

    // ---- Ctrl+B: toggle sidebar ----
    if key_event.code == KeyCode::Char('b') && key_event.modifiers == KeyModifiers::CONTROL {
        if app.ui.sidebar.is_some() {
            app.ui.sidebar = None;
        } else {
            app.ui.sidebar = Some(crate::ui::components::sidebar::Sidebar::new());
        }
        return Ok(());
    }

    // ---- Ctrl+T: cycle work mode ----
    if key_event.code == KeyCode::Char('t') && key_event.modifiers == KeyModifiers::CONTROL {
        app.agent_loop.session.work_mode =
            app.agent_loop.session.work_mode.next(app.config.allow_yolo);
        session::persist_config(app);
        return Ok(());
    }

    // ---- provider management panel (modal) ----
    if let Some(panel) = app.ui.provider_panel.as_mut() {
        if matches!(key_event.code, KeyCode::Char('q' | 'Q' | 'c' | 'C'))
            && key_event.modifiers == KeyModifiers::CONTROL
        {
            app.events.send(AppEvent::Quit);
            return Ok(());
        }
        match panel.handle_key(key_event, &mut app.config, &app.provider_manager) {
            PanelAction::Close => app.ui.provider_panel = None,
            PanelAction::Saved => {
                session::persist_config(app);
                app.events.send(AppEvent::ProvidersChanged);
            }
            PanelAction::RefreshModels => {
                app.events.send(AppEvent::RefreshProviderModels {
                    name: None,
                    notify: true,
                });
            }
            PanelAction::None => {}
        }
        return Ok(());
    }

    // ---- skills management panel (modal) ----
    if let Some(panel) = app.ui.skills_panel.as_mut() {
        use crate::ui::components::skills_panel::PanelAction as SkillsAction;
        match panel.handle_key(key_event, &mut app.agent_loop.skill_registry) {
            SkillsAction::Close => app.ui.skills_panel = None,
            SkillsAction::Saved => session::save_session(app),
            SkillsAction::None => {}
        }
        return Ok(());
    }

    // ---- MCP management panel (modal) ----
    if let Some(panel) = app.ui.mcp_panel.as_mut() {
        use crate::ui::components::mcp_panel::PanelAction as McpAction;
        if matches!(key_event.code, KeyCode::Char('q' | 'Q' | 'c' | 'C'))
            && key_event.modifiers == KeyModifiers::CONTROL
        {
            app.events.send(AppEvent::Quit);
            return Ok(());
        }
        match panel.handle_key(key_event, &mut app.config) {
            McpAction::Close => app.ui.mcp_panel = None,
            McpAction::Saved => {
                session::persist_config(app);
                app.events.send(AppEvent::McpChanged);
            }
            McpAction::None => {}
        }
        return Ok(());
    }

    // ---- diagnostics management panel (modal) ----
    if let Some(panel) = app.ui.diagnostics_panel.as_mut() {
        use crate::ui::components::diagnostics_panel::PanelAction as DiagnosticsAction;
        match panel.handle_key(key_event) {
            DiagnosticsAction::Close => app.ui.diagnostics_panel = None,
            DiagnosticsAction::Saved(profile) => {
                match crate::app::diagnostics::save_profile(&profile) {
                    Ok(()) => {
                        crate::app::diagnostics::reset_diagnostics_state(app);
                        app.agent_loop
                            .session
                            .diagnostics_state
                            .lock()
                            .unwrap()
                            .lsp_configured = crate::app::helpers::lsp_checker_configured();
                        crate::app::diagnostics::start_update(app, false);
                    }
                    Err(error) => {
                        app.agent_loop
                            .session
                            .conversation
                            .lock()
                            .unwrap()
                            .add_error_string(format!(
                                "could not save diagnostics profile: {error}"
                            ));
                        app.ui.conversation_panel.scroll_to_bottom();
                    }
                }
            }
            DiagnosticsAction::None => {}
        }
        return Ok(());
    }

    // ---- security profile management panel (modal) ----
    if app.ui.security_panel.is_some() {
        use crate::ui::components::security_panel::PanelAction as SecurityAction;
        let previous_profile = app.config.active_security_profile.clone();
        let previous_security = app.config.security.clone();
        let action = app
            .ui
            .security_panel
            .as_mut()
            .expect("checked above")
            .handle_key(key_event, &mut app.config);
        match action {
            SecurityAction::Close => app.ui.security_panel = None,
            SecurityAction::Saved => session::persist_config(app),
            SecurityAction::Apply => match app.install_active_security() {
                Ok(()) => session::persist_config(app),
                Err(error) => {
                    app.config.active_security_profile = previous_profile.clone();
                    app.config
                        .security_profiles
                        .insert(previous_profile, previous_security.clone());
                    app.config.security = previous_security;
                    app.agent_loop
                        .session
                        .conversation
                        .lock()
                        .unwrap()
                        .add_error_string(format!("invalid security configuration: {error}"));
                    app.ui.conversation_panel.scroll_to_bottom();
                }
            },
            SecurityAction::None => {}
        }
        return Ok(());
    }

    // ---- Ctrl+V: paste an image directly from the system clipboard ----
    if is_image_paste_shortcut(key_event) {
        paste_clipboard_image(app);
        return Ok(());
    }

    // ---- completion-popup navigation ----
    if app
        .ui
        .input_panel
        .completion
        .as_ref()
        .is_some_and(|c| c.visible)
    {
        match key_event.code {
            KeyCode::Tab => {
                let content = app.ui.input_panel.get_content();
                if let Some(c) = app.ui.input_panel.completion.as_mut() {
                    if content == c.line(c.selected) {
                        if c.candidates.len() == 1 {
                            app.ui.input_panel.completion = None;
                            return Ok(());
                        }
                        c.selected = (c.selected + 1) % c.candidates.len();
                    }
                    let visible = 10usize;
                    if c.selected < c.scroll_offset {
                        c.scroll_offset = c.selected;
                    } else if c.selected >= c.scroll_offset + visible {
                        c.scroll_offset = c.selected - visible + 1;
                    }
                    let text = c.line(c.selected);
                    app.ui.input_panel.set_content(&text);
                }
                return Ok(());
            }
            KeyCode::Up => {
                if let Some(ref mut c) = app.ui.input_panel.completion {
                    let visible = 10usize;
                    if c.selected > 0 {
                        c.selected -= 1;
                    } else {
                        c.selected = c.candidates.len().saturating_sub(1);
                    }
                    if c.selected < c.scroll_offset {
                        c.scroll_offset = c.selected;
                    } else if c.selected >= c.scroll_offset + visible {
                        c.scroll_offset = c.selected - visible + 1;
                    }
                    let text = c.line(c.selected);
                    app.ui.input_panel.set_content(&text);
                }
                return Ok(());
            }
            KeyCode::Down => {
                if let Some(ref mut c) = app.ui.input_panel.completion {
                    c.selected = (c.selected + 1) % c.candidates.len();
                    let visible = 10usize;
                    if c.selected < c.scroll_offset {
                        c.scroll_offset = c.selected;
                    } else if c.selected >= c.scroll_offset + visible {
                        c.scroll_offset = c.selected - visible + 1;
                    }
                    let text = c.line(c.selected);
                    app.ui.input_panel.set_content(&text);
                }
                return Ok(());
            }
            KeyCode::Esc | KeyCode::Char('q') if key_event.modifiers == KeyModifiers::CONTROL => {
                app.ui.input_panel.completion = None;
                return Ok(());
            }
            _ => {}
        }
    }

    // ---- Esc: cancel current stream ----
    if key_event.code == KeyCode::Esc
        && (app.agent_loop.cancel.active_id.is_some()
            || app.agent_loop.auto_compact.active_id.is_some())
    {
        app.events.send(AppEvent::Cancel);
        return Ok(());
    }

    // ---- Ctrl+C / Ctrl+Q: quit ----
    if key_event.modifiers == KeyModifiers::CONTROL
        && matches!(
            key_event.code,
            KeyCode::Char('c') | KeyCode::Char('C') | KeyCode::Char('q') | KeyCode::Char('Q')
        )
    {
        app.events.send(AppEvent::Quit);
        return Ok(());
    }

    // ---- text input ----
    match key_event.code {
        KeyCode::Char(_c) => {
            app.ui.input_panel.input(key_event);
            update_completions(app);
        }
        KeyCode::Backspace => {
            if !app.ui.input_panel.delete_placeholder_backward() {
                app.ui.input_panel.input(key_event);
            }
            update_completions(app);
        }
        KeyCode::Delete => {
            if !app.ui.input_panel.delete_placeholder_forward() {
                app.ui.input_panel.input(key_event);
            }
            update_completions(app);
        }
        KeyCode::Right
            if key_event.modifiers == KeyModifiers::NONE
                && app.ui.input_panel.accept_suggestion() =>
        {
            update_completions(app);
        }
        KeyCode::PageUp => {
            app.ui.conversation_panel.scroll_page_up();
        }
        KeyCode::PageDown => {
            app.ui.conversation_panel.scroll_page_down();
        }
        KeyCode::Up => {
            if app.ui.input_panel.get_content().is_empty() {
                if let Some(pending) = app.agent_loop.pending_request.take() {
                    // Draft recall remains text-only; consume the entire request
                    // so expanded images cannot leak into the next submission.
                    app.ui.input_panel.set_content(&pending.text);
                } else {
                    app.ui.input_panel.history_up();
                }
            } else if app.ui.input_panel.is_navigating_history() {
                app.ui.input_panel.history_up();
            } else {
                app.ui.input_panel.input(key_event);
            }
        }
        KeyCode::Enter if is_newline_modifier(key_event.modifiers) => {
            app.ui.input_panel.insert_newline();
            update_completions(app);
        }
        KeyCode::Enter => {
            let text = app.ui.input_panel.get_content();
            if text.starts_with('/') {
                commands::execute_command(app, &text).await;
            } else if text.starts_with('!') {
                commands::run_bang_command(app, &text);
            } else {
                app.events.send(AppEvent::Start);
            }
        }
        _ => {
            app.ui.input_panel.input(key_event);
            update_completions(app);
        }
    }
    Ok(())
}

/// Independent UI surfaces must get the first chance to close or leave their
/// local mode before Esc is interpreted as cancelling the active model turn.
fn independent_overlay_owns_escape(app: &App<'_>) -> bool {
    app.ui.terminal_pane.is_some()
        || app.ui.agent_panel.is_some()
        || app.ui.activity_page.is_some()
        || app.ui.rewind_panel.is_some()
        || app.ui.todo_panel.is_some()
        || app.ui.provider_panel.is_some()
        || app.ui.skills_panel.is_some()
        || app.ui.mcp_panel.is_some()
        || app.ui.diagnostics_panel.is_some()
        || app.ui.security_panel.is_some()
        || app
            .ui
            .sidebar
            .as_ref()
            .is_some_and(|sidebar| sidebar.has_focus)
        || app
            .ui
            .input_panel
            .completion
            .as_ref()
            .is_some_and(|completion| completion.visible)
}

fn apply_rewind(
    app: &mut App<'_>,
    checkpoint_id: u64,
    mode: crate::ui::components::rewind_panel::RestoreMode,
) {
    use crate::ui::components::rewind_panel::RestoreMode;

    if !app.require_running_session() {
        return;
    }
    let restore_code = matches!(
        mode,
        RestoreMode::CodeAndConversation | RestoreMode::CodeOnly
    );
    let restore_conversation = matches!(
        mode,
        RestoreMode::CodeAndConversation | RestoreMode::ConversationOnly
    );
    if restore_code
        && (app
            .agent_loop
            .session
            .tasks
            .snapshot_all()
            .iter()
            .any(|task| task.status == crate::tasks::TaskStatus::Running)
            || app
                .agent_loop
                .session
                .agents
                .snapshot_all()
                .iter()
                .any(|agent| !agent.status.is_terminal()))
    {
        {
            app.agent_loop
                .session
                .conversation
                .lock()
                .unwrap()
                .add_warning_string(
                    "cannot restore files while a background task or sub-agent is running",
                );
            app.ui.conversation_panel.scroll_to_bottom();
        };
        return;
    }

    let Some(store) = app.agent_loop.session.checkpoint_store.as_ref().cloned() else {
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_error_string("rewind checkpoints are unavailable");
        app.ui.conversation_panel.scroll_to_bottom();
        return;
    };
    let checkpoint = {
        let store = store.lock().unwrap();
        store.checkpoint(checkpoint_id).cloned()
    };
    let Some(checkpoint) = checkpoint else {
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_error_string("the selected rewind checkpoint no longer exists");
        app.ui.conversation_panel.scroll_to_bottom();
        return;
    };

    let mut restored_files = 0;
    let mut recovery_id = None;
    if restore_code {
        let has_file_changes = store
            .lock()
            .unwrap()
            .has_file_changes_for_restore(checkpoint_id);
        if has_file_changes {
            let conversation_cutoff = app
                .agent_loop
                .session
                .conversation
                .lock()
                .unwrap()
                .items
                .len();
            let begin_recovery = store.lock().unwrap().begin_recovery(
                checkpoint_id,
                conversation_cutoff,
                app.ui.todo_list.todos.clone(),
            );
            match begin_recovery {
                Ok(id) => recovery_id = Some(id),
                Err(error) => {
                    {
                        app.agent_loop
                            .session
                            .conversation
                            .lock()
                            .unwrap()
                            .add_error_string(format!(
                                "could not create rewind recovery point: {error}"
                            ));
                        app.ui.conversation_panel.scroll_to_bottom();
                    };
                    return;
                }
            }
        }
        let restore_result = store.lock().unwrap().restore_files(checkpoint_id);
        match restore_result {
            Ok(report) if report.conflicts.is_empty() => restored_files = report.restored,
            Ok(report) => {
                if let Some(recovery_id) = recovery_id {
                    let _ = store.lock().unwrap().discard_checkpoint(recovery_id);
                }
                let paths = report
                    .conflicts
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                app.agent_loop
                    .session
                    .conversation
                    .lock()
                    .unwrap()
                    .add_warning_string(format!(
                        "rewind stopped: files changed outside Programmer: {paths}"
                    ));
                app.ui.conversation_panel.scroll_to_bottom();
                return;
            }
            Err(error) => {
                if let Some(recovery_id) = recovery_id {
                    let _ = store.lock().unwrap().discard_checkpoint(recovery_id);
                }
                app.agent_loop
                    .session
                    .conversation
                    .lock()
                    .unwrap()
                    .add_error_string(format!("rewind failed: {error}"));
                app.ui.conversation_panel.scroll_to_bottom();
                return;
            }
        }
        if let Some(recovery_id) = recovery_id
            && let Err(error) = store.lock().unwrap().finalize_recovery(recovery_id)
        {
            app.agent_loop
                .session
                .conversation
                .lock()
                .unwrap()
                .add_warning_string(format!(
                    "files were restored, but the recovery point could not be finalized: {error}"
                ));
            app.ui.conversation_panel.scroll_to_bottom();
        }
    }

    if restore_conversation {
        commands::invalidate_auto_compaction(app);
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .truncate(checkpoint.conversation_cutoff);
        app.ui.conversation_panel.history_truncated();
        app.ui.input_panel.set_content(&checkpoint.prompt);
        *app.agent_loop.session.todo_store.lock().unwrap() = crate::todos::TodoList {
            todos: checkpoint.todos,
        };
        app.sync_todos_from_store();
        app.agent_loop.pending_request = None;
    }
    if let Err(error) = store
        .lock()
        .unwrap()
        .truncate_after(checkpoint_id, recovery_id)
    {
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_warning_string(format!("could not prune rewind history: {error}"));
        app.ui.conversation_panel.scroll_to_bottom();
    }
    app.agent_loop.current_checkpoint_id = None;
    session::mark_dirty(app);
    app.agent_loop
        .session
        .conversation
        .lock()
        .unwrap()
        .add_info_string(format!(
            "Rewound to prompt #{checkpoint_id}: {} file(s) restored{}",
            restored_files,
            if restore_conversation {
                "; prompt returned to the input"
            } else {
                ""
            }
        ));
    app.ui.conversation_panel.scroll_to_bottom();
}

async fn apply_rewind_fork(app: &mut App<'_>, checkpoint_id: u64) {
    if !app.require_running_session() {
        return;
    }
    let Some(source_store) = app.agent_loop.session.checkpoint_store.as_ref().cloned() else {
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_error_string("rewind checkpoints are unavailable");
        app.ui.conversation_panel.scroll_to_bottom();
        return;
    };
    let checkpoint = {
        let store = source_store.lock().unwrap();
        store.checkpoint(checkpoint_id).cloned()
    };
    let Some(checkpoint) = checkpoint.filter(|checkpoint| !checkpoint.recovery) else {
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_error_string("the selected rewind checkpoint cannot be forked");
        app.ui.conversation_panel.scroll_to_bottom();
        return;
    };
    if app.agent_loop.session.mgr.is_none() {
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_error_string("cannot fork without session persistence");
        app.ui.conversation_panel.scroll_to_bottom();
        return;
    }
    if let Err(error) = app.close_session().await {
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_error_string(format!(
                "could not close the source session before forking: {error}"
            ));
        app.ui.conversation_panel.scroll_to_bottom();
        return;
    }
    if let Err(error) = session::save_session_checked(app) {
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_error_string(format!(
                "could not save the source session before forking: {error}"
            ));
        app.ui.conversation_panel.scroll_to_bottom();
        return;
    }
    let manager = app
        .agent_loop
        .session
        .mgr
        .as_ref()
        .expect("session manager checked above");
    let source_uuid = app.agent_loop.session.uuid.clone();
    let fork_uuid = manager.create().uuid;
    let Some(mut fork_store) = crate::checkpoint::CheckpointStore::for_session(&fork_uuid) else {
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_error_string("could not create rewind history for the fork");
        app.ui.conversation_panel.scroll_to_bottom();
        return;
    };
    if let Err(error) =
        fork_store.copy_conversation_history_before(&source_store.lock().unwrap(), checkpoint_id)
    {
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_error_string(format!(
                "could not create rewind history for the fork: {error}"
            ));
        app.ui.conversation_panel.scroll_to_bottom();
        return;
    }

    commands::invalidate_auto_compaction(app);
    // Detach all mutable resources: retained source handles must never write into the fork.
    let conversation = std::sync::Arc::new(std::sync::Mutex::new(
        app.agent_loop.session.conversation.lock().unwrap().clone(),
    ));
    app.agent_loop.session.conversation = conversation.clone();
    app.ui.conversation_panel.conversation = conversation;
    app.agent_loop.session.todo_store = Default::default();
    app.agent_loop.session.tasks = crate::tasks::TaskManager::default();
    app.connect_task_events();
    app.agent_loop.task_notifications.clear();
    app.agent_loop.session.agents = crate::agents::AgentManager::default();
    app.agent_loop.agent_notifications = crate::app::AgentNotificationState::new();
    app.agent_loop.session.persistence = session::PersistenceState::default();
    app.agent_loop.session.title_generation_id =
        app.agent_loop.session.title_generation_id.wrapping_add(1);
    app.agent_loop.session.title_generation_started = !app.agent_loop.session.title.is_empty();
    crate::app::diagnostics::reset_diagnostics_state(app);
    app.agent_loop.session.uuid = fork_uuid.clone();
    app.agent_loop.session.did_save = false;
    app.agent_loop
        .session
        .conversation
        .lock()
        .unwrap()
        .truncate(checkpoint.conversation_cutoff);
    app.ui.conversation_panel.history_truncated();
    app.ui.input_panel.set_content(&checkpoint.prompt);
    *app.agent_loop.session.todo_store.lock().unwrap() = crate::todos::TodoList {
        todos: checkpoint.todos,
    };
    app.sync_todos_from_store();
    app.agent_loop.pending_request = None;
    app.agent_loop.session.checkpoint_store =
        Some(std::sync::Arc::new(std::sync::Mutex::new(fork_store)));
    app.agent_loop.current_checkpoint_id = None;
    session::mark_dirty(app);
    app.agent_loop.session.conversation.lock().unwrap().add_info_string(format!(
    "Forked prompt #{checkpoint_id} into session {fork_uuid}. Source session {source_uuid} was preserved; prompt returned to the input."
    ));
    app.ui.conversation_panel.scroll_to_bottom();
    if let Err(error) = session::persist_session(app) {
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_error_string(format!(
                "could not save the fork; session remains stopped: {error}"
            ));
        app.ui.conversation_panel.scroll_to_bottom();
        return;
    }
    app.agent_loop.session.lifecycle = crate::app::SessionLifecycle::Running;
}

fn is_promote_shortcut(key: KeyEvent) -> bool {
    key.code == KeyCode::Char('z') && key.modifiers == KeyModifiers::CONTROL
}

fn is_image_paste_shortcut(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('v' | 'V')) && key.modifiers == KeyModifiers::CONTROL
}

fn paste_clipboard_image(app: &mut App<'_>) {
    let image = match crate::clipboard::read_image() {
        Ok(Some(image)) => image,
        Ok(None) => {
            app.agent_loop
                .session
                .conversation
                .lock()
                .unwrap()
                .add_warning_string("clipboard does not contain an image");
            app.ui.conversation_panel.scroll_to_bottom();
            return;
        }
        Err(error) => {
            app.agent_loop
                .session
                .conversation
                .lock()
                .unwrap()
                .add_error_string(error);
            app.ui.conversation_panel.scroll_to_bottom();
            return;
        }
    };
    let attachment = match crate::commands::image_content_from_bytes(&image.png) {
        Ok(attachment) => attachment,
        Err(error) => {
            app.agent_loop
                .session
                .conversation
                .lock()
                .unwrap()
                .add_warning_string(error);
            app.ui.conversation_panel.scroll_to_bottom();
            return;
        }
    };
    if !app
        .ui
        .input_panel
        .add_image(attachment, image.width, image.height)
    {
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_warning_string(format!(
                "cannot paste more than {} images into one message",
                crate::commands::MAX_IMAGES_PER_MESSAGE
            ));
        app.ui.conversation_panel.scroll_to_bottom();
    }
    update_completions(app);
}

/// Handle a key while the task panel is open. Pipe tasks are strictly read-only
/// and use navigation keys for captured output. Interactive tasks retain the
/// input-grab behavior.
fn handle_terminal_key(app: &mut App<'_>, key_event: KeyEvent) {
    use crate::ui::components::terminal_panel::key_event_to_bytes;

    let Some(pane) = app.ui.terminal_pane.as_mut() else {
        return;
    };

    if !pane.accepts_input() {
        let page = pane
            .grid
            .map(|grid| grid.height.max(1) as i32)
            .unwrap_or(10);
        match key_event.code {
            KeyCode::Esc | KeyCode::Char('q') => app.ui.terminal_pane = None,
            KeyCode::Up | KeyCode::Char('k') => pane.scroll_read_only(1),
            KeyCode::Down | KeyCode::Char('j') => pane.scroll_read_only(-1),
            KeyCode::PageUp => pane.scroll_read_only(page),
            KeyCode::PageDown => pane.scroll_read_only(-page),
            KeyCode::Home => pane.scroll_read_only_to_start(),
            KeyCode::End => pane.scroll_read_only_to_end(),
            _ => {}
        }
        return;
    }

    // Ctrl+O toggles whether subsequent input belongs to the child.
    if key_event.code == KeyCode::Char('o') && key_event.modifiers.contains(KeyModifiers::CONTROL) {
        pane.grabbed = !pane.grabbed;
        return;
    }

    if pane.grabbed {
        // Cursor keys need the child's DECCKM mode to pick CSI vs SS3.
        let app_cursor = app
            .agent_loop
            .session
            .tasks
            .with_screen(pane.task_id, |s| s.application_cursor())
            .unwrap_or(false);
        if let Some(bytes) = key_event_to_bytes(key_event, app_cursor) {
            let _ = app
                .agent_loop
                .session
                .tasks
                .write_bytes(pane.task_id, &bytes);
            // Typing snaps the view back to live output.
            app.agent_loop
                .session
                .tasks
                .scroll_screen(pane.task_id, i32::MIN);
        }
        return;
    }

    // Released: the panel owns Esc/q and uses them to close.
    if matches!(key_event.code, KeyCode::Esc | KeyCode::Char('q')) {
        app.ui.terminal_pane = None;
    }
}

/// Forward a mouse event to the terminal panel's PTY when input is grabbed and
/// the program has enabled mouse reporting. Swallowed otherwise.
pub(crate) fn handle_terminal_mouse(app: &mut App<'_>, mouse: crossterm::event::MouseEvent) {
    use crate::ui::components::terminal_panel::mouse_event_to_bytes;
    use crossterm::event::MouseEventKind;

    let Some(pane) = app.ui.terminal_pane.as_mut() else {
        return;
    };

    if !pane.accepts_input() {
        match mouse.kind {
            MouseEventKind::ScrollUp => pane.scroll_read_only(3),
            MouseEventKind::ScrollDown => pane.scroll_read_only(-3),
            _ => {}
        }
        return;
    }

    let mode = app
        .agent_loop
        .session
        .tasks
        .with_screen(pane.task_id, |s| s.mouse_protocol_mode())
        .unwrap_or(vt100::MouseProtocolMode::None);

    // The wheel scrolls the local scrollback unless a grabbed program is
    // consuming the mouse itself (then the wheel is forwarded below).
    let program_wants_mouse = pane.grabbed && mode != vt100::MouseProtocolMode::None;
    match mouse.kind {
        MouseEventKind::ScrollUp if !program_wants_mouse => {
            app.agent_loop.session.tasks.scroll_screen(pane.task_id, 3);
            return;
        }
        MouseEventKind::ScrollDown if !program_wants_mouse => {
            app.agent_loop.session.tasks.scroll_screen(pane.task_id, -3);
            return;
        }
        _ => {}
    }

    // Everything else is only forwarded while grabbed.
    if !pane.grabbed {
        return;
    }
    let Some(grid) = pane.grid else {
        return;
    };
    if let Some(bytes) = mouse_event_to_bytes(mouse, grid, mode) {
        let _ = app
            .agent_loop
            .session
            .tasks
            .write_bytes(pane.task_id, &bytes);
    }
}

/// Enter combined with any of these modifiers inserts a newline instead of sending.
pub(crate) fn is_newline_modifier(modifiers: KeyModifiers) -> bool {
    modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT)
}

/// Handles text pasted into the terminal (bracketed paste).
pub(crate) fn handle_paste(app: &mut App<'_>, data: String) {
    // While the terminal panel has input grabbed, a paste goes to the PTY.
    if let Some(pane) = app.ui.terminal_pane.as_ref() {
        if pane.grabbed {
            let _ = app
                .agent_loop
                .session
                .tasks
                .write_bytes(pane.task_id, data.as_bytes());
        }
        return;
    }
    let data = data.replace("\r\n", "\n").replace('\r', "\n");
    if app.ui.rewind_panel.is_some() || app.ui.activity_page.is_some() {
        return;
    }
    if let Some(panel) = app.ui.question_panel.as_mut() {
        panel.handle_paste(&data);
        return;
    }
    if let Some(panel) = app.ui.provider_panel.as_mut() {
        panel.handle_paste(&data);
        return;
    }
    if let Some(panel) = app.ui.mcp_panel.as_mut() {
        panel.handle_paste(&data);
        return;
    }
    if let Some(panel) = app.ui.diagnostics_panel.as_mut() {
        panel.handle_paste(&data);
        return;
    }
    if let Some(panel) = app.ui.security_panel.as_mut() {
        panel.handle_paste(&data);
        return;
    }
    if !data.contains('\n') && data.chars().count() <= 200 {
        app.ui.input_panel.insert_str(&data);
    } else {
        app.ui.input_panel.add_paste(data);
    }
    update_completions(app);
}

// ---------------------------------------------------------------------------
// Tool-call approval (Manual mode)
// ---------------------------------------------------------------------------

/// Handle approval keys when tool calls are queued.
fn handle_approval_key(app: &mut App<'_>, key_event: KeyEvent) -> color_eyre::Result<()> {
    match key_event.code {
        KeyCode::Up | KeyCode::Char('k') => {
            if let Some(ref mut review) = app.agent_loop.pending_review {
                review.selected = review.selected.saturating_sub(1);
            }
        }
        KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
            if let Some(ref mut review) = app.agent_loop.pending_review
                && review.selected + 1 < 2
            {
                review.selected += 1;
            }
        }
        KeyCode::Enter => {
            if let Some(review) = app.agent_loop.pending_review.take() {
                use crate::runner::ReviewDecision;
                use async_openai::types::responses::{
                    FunctionCallOutput, FunctionCallOutputItemParam,
                };
                let decision = match review.selected {
                    0 => ReviewDecision::Approve,
                    _ => ReviewDecision::Deny {
                        output: crate::tools::ToolOutput {
                            param: FunctionCallOutputItemParam {
                                call_id: review.call.call_id.clone(),
                                output: FunctionCallOutput::Text(if review.agent_id.is_some() {
                                    format!(
                                        "error: sub-agent tool call denied by user — {}",
                                        review.reason
                                    )
                                } else {
                                    format!(
                                        "error: tool call denied by user in Manual mode — {}",
                                        review.reason,
                                    )
                                }),
                                id: None,
                                status: None,
                            },
                            failed: true,
                            approval_label: Some(if review.agent_id.is_some() {
                                "✖ denied by user (sub-agent)".to_string()
                            } else {
                                format!("{} denied in Manual mode by user", WorkMode::Manual.icon())
                            }),
                        },
                    },
                };
                let _ = review.reply.0.send(decision);
                app.agent_loop.pending_review = app.agent_loop.review_queue.pop_front();
            }
        }
        _ => {}
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Plan review (Plan mode Reviewing phase)
// ---------------------------------------------------------------------------

/// Handle keyboard input in the plan review bar (Plan mode Reviewing phase).
async fn handle_plan_review_key(app: &mut App<'_>, key_event: KeyEvent) -> color_eyre::Result<()> {
    let option_count = if app.config.allow_yolo { 4 } else { 3 };
    match key_event.code {
        KeyCode::Up | KeyCode::Char('k') => {
            app.ui.plan_review_selected = app.ui.plan_review_selected.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
            if app.ui.plan_review_selected + 1 < option_count {
                app.ui.plan_review_selected += 1;
            }
        }
        KeyCode::Esc => {
            // Cancel review — go back to Planning for revision.
            app.agent_loop.plan_phase = crate::classifier::PlanPhase::Planning;
            app.ui.plan_review_selected = 0;
            app.agent_loop
                .session
                .conversation
                .lock()
                .unwrap()
                .add_info_string("Plan review cancelled — you can revise the plan.");
            app.ui.conversation_panel.scroll_to_bottom();
            session::save_session(app);
        }
        KeyCode::Enter => match app.ui.plan_review_selected {
            0 => {
                approve_plan(
                        app,
                        WorkMode::Manual,
                        "Plan approved — executing with Manual mode.",
                        "The plan was approved by the user. Execute it now using the identified steps. Ask for approval before running commands or destructive edits.",
                    )
                    .await;
            }
            1 => {
                approve_plan(
                    app,
                    WorkMode::Auto,
                    "Plan approved — executing with Auto mode.",
                    "The plan was approved by the user. Execute it now using the identified steps.",
                )
                .await;
            }
            2 => {
                if app.config.allow_yolo {
                    approve_plan(
                            app,
                            WorkMode::Yolo,
                            "Plan approved — executing with YOLO mode.",
                            "The plan was approved by the user. Execute it now using the identified steps. You have full autonomy.",
                        )
                        .await;
                } else {
                    propose_plan_changes(app);
                }
            }
            3 => {
                propose_plan_changes(app);
            }
            _ => {}
        },
        _ => {
            // Any other key: pass through to input panel for feedback text
            app.ui.input_panel.input(key_event);
        }
    }
    Ok(())
}

/// Exit Plan mode into `mode` and kick off execution with a hidden
/// developer-role instruction.
async fn approve_plan(app: &mut App<'_>, mode: WorkMode, info: &str, hidden: &str) {
    app.agent_loop.session.work_mode = mode;
    app.agent_loop.plan_phase = crate::classifier::PlanPhase::default();
    app.ui.plan_review_selected = 0;
    app.agent_loop
        .session
        .conversation
        .lock()
        .unwrap()
        .add_info_string(info);
    app.ui.conversation_panel.scroll_to_bottom();
    session::save_session(app);
    commands::start_request_as(
        app,
        hidden.to_string(),
        async_openai::types::responses::InputRole::Developer,
    )
    .await;
}

/// Return to Planning phase so the user can type feedback on the plan.
fn propose_plan_changes(app: &mut App<'_>) {
    app.agent_loop.plan_phase = crate::classifier::PlanPhase::Planning;
    app.ui.plan_review_selected = 0;
    app.agent_loop
        .session
        .conversation
        .lock()
        .unwrap()
        .add_info_string("Enter your feedback in the input panel.");
    app.ui.conversation_panel.scroll_to_bottom();
    session::save_session(app);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rewind_fork_closes_source_and_isolates_runtime_resources() {
        use async_openai::types::responses::{InputContent, InputMessage, InputRole, MessageItem};
        let mut app = crate::app::commands::tests::command_test_app().await;
        let directory = std::env::current_dir()
            .unwrap()
            .join(".programmer")
            .join(format!("fork-test-{}", uuid::Uuid::new_v4()));
        app.agent_loop.session.mgr = Some(crate::session::SessionManager::for_test(directory));
        app.agent_loop
            .session
            .conversation
            .lock()
            .unwrap()
            .add_input_message(MessageItem::Input(InputMessage {
                content: vec![InputContent::InputText("source prompt".into())],
                role: InputRole::User,
                status: None,
            }));
        app.ui.conversation_panel.scroll_to_bottom();
        let source_uuid = app.agent_loop.session.uuid.clone();
        let source = app.agent_loop.session.conversation.clone();
        let todos = app.agent_loop.session.todo_store.clone();
        let tasks = app.agent_loop.session.tasks.clone();
        let generation = app.task_event_generation;
        let checkpoint = app
            .agent_loop
            .session
            .checkpoint_store
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .begin("fork prompt".into(), 0, Vec::new())
            .unwrap();
        tasks.spawn("sleep 30", None, None).unwrap();
        apply_rewind_fork(&mut app, checkpoint).await;
        assert!(app.session_accepts_work());
        assert_ne!(app.agent_loop.session.uuid, source_uuid);
        assert!(tasks.spawn("true", None, None).is_err());
        assert!(
            tasks
                .snapshot_all()
                .iter()
                .all(|task| task.status != crate::tasks::TaskStatus::Running)
        );
        assert!(app.agent_loop.session.tasks.snapshot_all().is_empty());
        assert!(app.task_event_generation > generation);
        assert!(!std::sync::Arc::ptr_eq(
            &source,
            &app.agent_loop.session.conversation
        ));
        assert!(!std::sync::Arc::ptr_eq(
            &todos,
            &app.agent_loop.session.todo_store
        ));
        assert!(
            source
                .lock()
                .unwrap()
                .items
                .iter()
                .any(|item| matches!(item, crate::response::message_item::MessageItem::Input(_)))
        );
        assert_eq!(app.ui.input_panel.get_content(), "fork prompt");
        let saved = app
            .agent_loop
            .session
            .mgr
            .as_ref()
            .unwrap()
            .load(&source_uuid)
            .unwrap()
            .unwrap();
        assert!(saved.tasks.iter().all(|task| task.status != "running"));
        app.close_session().await.unwrap();
    }

    #[test]
    fn ctrl_z_is_the_command_promotion_shortcut() {
        assert!(is_promote_shortcut(KeyEvent::new(
            KeyCode::Char('z'),
            KeyModifiers::CONTROL
        )));
        assert!(!is_promote_shortcut(KeyEvent::new(
            KeyCode::Char('z'),
            KeyModifiers::NONE
        )));
    }

    #[test]
    fn ctrl_v_is_the_image_paste_shortcut() {
        assert!(is_image_paste_shortcut(KeyEvent::new(
            KeyCode::Char('v'),
            KeyModifiers::CONTROL
        )));
        assert!(!is_image_paste_shortcut(KeyEvent::new(
            KeyCode::Char('v'),
            KeyModifiers::NONE
        )));
    }

    #[tokio::test]
    async fn esc_closes_mcp_panel_without_cancelling_the_active_turn() {
        let mut config = crate::config::programmer_config::ProgrammerConfig::default();
        config.providers.clear();
        let mut app = crate::app::App::new(
            config,
            crate::app::session::SessionSeed::Fresh {
                uuid: "mcp-escape-routing-test".to_string(),
            },
            None,
            Vec::new(),
            false,
            "test".to_string(),
        )
        .await;
        app.agent_loop.cancel.active_id = Some(crate::cancel::OperationId(42));
        app.ui.mcp_panel = Some(crate::ui::components::mcp_panel::McpPanel::new());

        handle_key_events(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .await
            .unwrap();

        assert!(app.ui.mcp_panel.is_none());
        assert_eq!(
            app.agent_loop.cancel.active_id,
            Some(crate::cancel::OperationId(42))
        );
        assert!(!app.agent_loop.cancel.active.is_cancelled());
    }
    #[tokio::test]
    async fn activity_page_owns_escape_and_paste_without_cancelling_active_turn() {
        let mut config = crate::config::programmer_config::ProgrammerConfig::default();
        config.providers.clear();
        config.memory.dream_enabled = false;
        let mut app = crate::app::App::new(
            config,
            crate::app::session::SessionSeed::Fresh {
                uuid: "activity-routing-test".into(),
            },
            None,
            Vec::new(),
            false,
            "test".into(),
        )
        .await;
        app.agent_loop.cancel.active_id = Some(crate::cancel::OperationId(42));
        app.ui.activity_page = Some(crate::app::activity::ActivityPage::new(
            crate::ui::components::activity_panel::ActivityMode::Dream,
            false,
            app.agent_loop.session.uuid.clone(),
            crate::ui::components::activity_panel::ActivityPanel::new(
                "History".into(),
                crate::ui::components::activity_panel::ActivityMode::Dream,
                app.agent_loop.session.uuid.clone(),
                Vec::new(),
            ),
        ));
        assert!(super::super::has_blocking_surface(&app));
        let original = app.ui.input_panel.get_content();
        super::handle_paste(&mut app, "must not reach input".into());
        assert_eq!(app.ui.input_panel.get_content(), original);
        handle_key_events(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .await
            .unwrap();
        assert!(app.ui.activity_page.is_none());
        assert_eq!(
            app.agent_loop.cancel.active_id,
            Some(crate::cancel::OperationId(42))
        );
        assert!(!app.agent_loop.cancel.active.is_cancelled());
    }
}
