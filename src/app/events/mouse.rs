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

//! Mouse handling: routing scrolls and clicks between the conversation
//! panel and the sidebar, plus text selection.

use super::super::{session, App};
use crate::ui::components::conversation_panel::conversation_panel::SelectionEnd;
use crate::ui::components::sidebar::ClickTarget;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

/// Route a mouse event to the sidebar or the conversation panel.
pub(crate) fn handle_mouse(app: &mut App<'_>, mouse: MouseEvent) {
    match mouse.kind {
        MouseEventKind::ScrollDown => {
            if app.sidebar_area.is_some_and(|a| mouse.column >= a.x) {
                if let Some(ref mut s) = app.sidebar { s.scroll_down(); }
            } else {
                app.conversation_panel.scroll_down();
            }
        }
        MouseEventKind::ScrollUp => {
            if app.sidebar_area.is_some_and(|a| mouse.column >= a.x) {
                if let Some(ref mut s) = app.sidebar { s.scroll_up(); }
            } else {
                app.conversation_panel.scroll_up();
            }
        }
        MouseEventKind::Down(MouseButton::Left) => {
            // If click is in the sidebar, track it and don't start selection.
            app.sidebar_click_active = app.sidebar.is_some()
                && app.sidebar_area.as_ref().is_some_and(|area| {
                    mouse.column >= area.x
                        && mouse.column < area.x + area.width
                        && mouse.row >= area.y
                        && mouse.row < area.y + area.height
                });
            if app.sidebar_click_active {
                return;
            }
            // Clicking the "jump to bottom" indicator snaps to the latest.
            if app.conversation_panel.jump_button_hit(mouse.column, mouse.row) {
                app.conversation_panel.scroll_to_bottom();
                return;
            }
            app.conversation_panel
                .selection_begin(mouse.column, mouse.row)
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if app.sidebar_click_active {
                return;
            }
            app.conversation_panel
                .selection_drag(mouse.column, mouse.row)
        }
        MouseEventKind::Up(MouseButton::Left) => {
            if app.sidebar_click_active {
                app.sidebar_click_active = false;
                // Only act if the release is still in the sidebar.
                if let Some(ref sidebar) = app.sidebar
                    && let Some(ref area) = app.sidebar_area
                        && mouse.column > area.x
                            && mouse.column < area.x + area.width
                            && mouse.row >= area.y
                            && mouse.row < area.y + area.height
                        {
                            let line_idx = (mouse.row - area.y) as usize;
                            if line_idx < sidebar.click_map.len() {
                                let target = sidebar.click_map[line_idx].clone();
                                handle_sidebar_click(app, &target);
                            }
                        }
                return;
            }
            match app
                .conversation_panel
                .selection_end(mouse.column, mouse.row)
            {
                SelectionEnd::Click => app
                    .conversation_panel
                    .handle_click(mouse.column, mouse.row),
                SelectionEnd::Copied(text) => {
                    if !crate::clipboard::copy(&text) {
                        app.conversation_panel
                            .add_error_string("failed to copy selection to clipboard");
                        session::save_session(app);
                    }
                }
                SelectionEnd::Ignored => {}
            }
        }
        _ => {}
    }
}

fn handle_sidebar_click(app: &mut App<'_>, target: &ClickTarget) {
    match target {
        ClickTarget::Section(key) => {
            if let Some(ref mut s) = app.sidebar {
                s.toggle_section(*key);
            }
        }
        ClickTarget::TodoItem(idx) => {
            let mut sorted: Vec<&crate::todos::Todo> =
                app.todo_list.todos.iter().collect();
            sorted.sort_by_key(|t| {
                crate::ui::components::sidebar::ui::todo_status_order(&t.status)
            });
            if let Some(todo) = sorted.get(*idx) {
                let id = todo.id.clone();
                let _ = app.todo_list.toggle_status(&id);
                let _ = app.todo_list.save_to_file();
            }
        }
        ClickTarget::Task(id) => {
            if let Some(ref mut s) = app.sidebar {
                s.toggle_task(*id);
            }
        }
        ClickTarget::Diagnostic(_idx) => {
            // Could jump to file location in the future.
        }
        ClickTarget::None => {}
    }
}
