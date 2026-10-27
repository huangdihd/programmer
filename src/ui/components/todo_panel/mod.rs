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

//! Interactive todo-list panel. Rendered as a pop-up overlay above the
//! input area. Supports navigation, status toggling, deletion, and adding
//! new items via an inline prompt.

pub mod ui;

use crate::todos::TodoList;
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};

/// Interaction intent submitted to the session owner; the panel never mutates its snapshot.
#[derive(Debug, PartialEq)]
pub enum PanelAction {
    None,
    Close,
    Add {
        title: String,
        description: Option<String>,
    },
    Toggle {
        id: String,
    },
    Delete {
        id: String,
    },
}

/// Input mode when adding a new todo.
#[derive(Debug)]
enum AddMode {
    Hidden,
    Title { input: String },
    Description { title: String, input: String },
}

#[derive(Debug)]
pub struct TodoPanel {
    pub list: TodoList,
    /// Index of the highlighted row.
    selected: usize,
    /// Scroll offset for the item list.
    scroll_offset: usize,
    /// Whether we're adding a new todo (progressive: title → description).
    add_mode: AddMode,
}

impl TodoPanel {
    pub fn new(list: TodoList) -> Self {
        TodoPanel {
            list,
            selected: 0,
            scroll_offset: 0,
            add_mode: AddMode::Hidden,
        }
    }

    pub(crate) fn replace_snapshot(&mut self, list: TodoList) {
        self.list = list;
        self.selected = self.selected.min(self.list.todos.len().saturating_sub(1));
        self.scroll_offset = self.scroll_offset.min(self.selected);
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> PanelAction {
        match &mut self.add_mode {
            AddMode::Title { input } => match key.code {
                KeyCode::Esc => {
                    self.add_mode = AddMode::Hidden;
                }
                KeyCode::Enter => {
                    if input.trim().is_empty() {
                        self.add_mode = AddMode::Hidden;
                    } else {
                        let title = std::mem::take(input);
                        self.add_mode = AddMode::Description {
                            title,
                            input: String::new(),
                        };
                    }
                }
                KeyCode::Backspace => {
                    input.pop();
                }
                KeyCode::Char(c) => {
                    input.push(c);
                }
                _ => {}
            },
            AddMode::Description { title, input } => match key.code {
                KeyCode::Esc => {
                    let title = std::mem::take(title);
                    self.selected = self.list.todos.len();
                    self.add_mode = AddMode::Hidden;
                    return PanelAction::Add {
                        title,
                        description: None,
                    };
                }
                KeyCode::Enter => {
                    let title = std::mem::take(title);
                    let desc = if input.trim().is_empty() {
                        None
                    } else {
                        Some(std::mem::take(input))
                    };
                    self.selected = self.list.todos.len();
                    self.add_mode = AddMode::Hidden;
                    return PanelAction::Add {
                        title,
                        description: desc,
                    };
                }
                KeyCode::Backspace => {
                    input.pop();
                }
                KeyCode::Char(c) => {
                    input.push(c);
                }
                _ => {}
            },
            AddMode::Hidden => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    return PanelAction::Close;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    if self.selected > 0 {
                        self.selected -= 1;
                        if self.selected < self.scroll_offset {
                            self.scroll_offset = self.selected;
                        }
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if self.selected + 1 < self.list.todos.len() {
                        self.selected += 1;
                        let visible_end = self.scroll_offset + Self::VISIBLE_ITEMS;
                        if self.selected >= visible_end {
                            self.scroll_offset = self.selected + 1 - Self::VISIBLE_ITEMS;
                        }
                    }
                }
                KeyCode::Enter | KeyCode::Char(' ') => {
                    if let Some(todo) = self.list.todos.get(self.selected) {
                        let id = todo.id.clone();
                        return PanelAction::Toggle { id };
                    }
                }
                KeyCode::Char('d') => {
                    if let Some(todo) = self.list.todos.get(self.selected) {
                        let id = todo.id.clone();
                        if self.selected + 1 >= self.list.todos.len() && self.selected > 0 {
                            self.selected -= 1;
                        }
                        return PanelAction::Delete { id };
                    }
                }
                KeyCode::Char('a') => {
                    self.add_mode = AddMode::Title {
                        input: String::new(),
                    };
                }
                _ => {}
            },
        }
        PanelAction::None
    }

    pub(crate) fn mouse_index(
        &self,
        area: ratatui::layout::Rect,
        mouse: MouseEvent,
    ) -> Option<usize> {
        if !matches!(self.add_mode, AddMode::Hidden) {
            return None;
        }
        let inner = ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .inner(area);
        if mouse.column < inner.x
            || mouse.column >= inner.right()
            || mouse.row < inner.y
            || mouse.row >= inner.bottom().saturating_sub(1)
        {
            return None;
        }
        let index = self.scroll_offset + usize::from(mouse.row - inner.y);
        (index < self.list.todos.len()).then_some(index)
    }

    pub(crate) fn mouse_key(
        &mut self,
        area: ratatui::layout::Rect,
        mouse: MouseEvent,
    ) -> Option<KeyCode> {
        match mouse.kind {
            MouseEventKind::ScrollUp => return Some(KeyCode::Up),
            MouseEventKind::ScrollDown => return Some(KeyCode::Down),
            MouseEventKind::Up(MouseButton::Left) => {}
            _ => return None,
        }
        let index = self.mouse_index(area, mouse)?;
        self.selected = index;
        Some(KeyCode::Enter)
    }

    pub(crate) fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    /// Number of visible items the panel can show (used by UI to compute height).
    const VISIBLE_ITEMS: usize = 8;

    pub fn needed_height(&self) -> u16 {
        match &self.add_mode {
            AddMode::Hidden => {
                let item_count = self.list.todos.len().min(Self::VISIBLE_ITEMS);
                if item_count == 0 {
                    4 // border + empty-state row + hint + border
                } else {
                    3 + item_count as u16 // border + items + hint + border
                }
            }
            _ => 5,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn empty_panel_keeps_its_hint_row_visible() {
        assert_eq!(TodoPanel::new(TodoList::default()).needed_height(), 4);
    }

    #[test]
    fn mouse_click_selects_and_activates_the_visible_todo_row() {
        let mut list = TodoList::default();
        list.add("first".into(), None);
        list.add("second".into(), None);
        let second = list.todos[1].id.clone();
        let mut panel = TodoPanel::new(list);
        let area = ratatui::layout::Rect::new(0, 0, 60, panel.needed_height());
        let code = panel.mouse_key(
            area,
            MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: 4,
                row: 2,
                modifiers: KeyModifiers::NONE,
            },
        );

        assert_eq!(code, Some(KeyCode::Enter));
        assert_eq!(
            panel.handle_key(key(code.unwrap())),
            PanelAction::Toggle { id: second }
        );
    }

    #[test]
    fn keyboard_navigation_keeps_the_selected_item_visible() {
        let mut list = TodoList::default();
        for index in 0..10 {
            list.add(format!("todo {index}"), None);
        }
        let mut panel = TodoPanel::new(list);

        for _ in 0..8 {
            assert_eq!(panel.handle_key(key(KeyCode::Down)), PanelAction::None);
        }

        assert_eq!(panel.selected, 8);
        assert_eq!(panel.scroll_offset, 1);
        assert_eq!(panel.handle_key(key(KeyCode::Up)), PanelAction::None);
        assert_eq!(panel.scroll_offset, 1);
        for _ in 0..7 {
            panel.handle_key(key(KeyCode::Up));
        }
        assert_eq!(panel.selected, 0);
        assert_eq!(panel.scroll_offset, 0);
    }

    #[test]
    fn shrinking_snapshot_keeps_selection_and_deletion_in_bounds() {
        let mut list = TodoList::default();
        for title in ["first", "second", "third"] {
            list.add(title.into(), None);
        }
        let mut panel = TodoPanel::new(list.clone());
        panel.handle_key(key(KeyCode::Down));
        panel.handle_key(key(KeyCode::Down));
        list.todos.truncate(1);
        let id = list.todos[0].id.clone();
        panel.replace_snapshot(list);
        assert_eq!(
            panel.handle_key(key(KeyCode::Char('d'))),
            PanelAction::Delete { id }
        );
        panel.replace_snapshot(TodoList::default());
        assert_eq!(panel.handle_key(key(KeyCode::Char('d'))), PanelAction::None);
    }

    #[test]
    fn editing_emits_intent_without_mutating_the_snapshot() {
        let mut list = TodoList::default();
        list.add("existing".into(), None);
        let id = list.todos[0].id.clone();
        let mut panel = TodoPanel::new(list);
        assert_eq!(
            panel.handle_key(key(KeyCode::Enter)),
            PanelAction::Toggle { id: id.clone() }
        );
        assert_eq!(
            panel.handle_key(key(KeyCode::Char('d'))),
            PanelAction::Delete { id }
        );
        assert_eq!(panel.list.todos.len(), 1);
        panel.handle_key(key(KeyCode::Char('a')));
        panel.handle_key(key(KeyCode::Char('N')));
        panel.handle_key(key(KeyCode::Enter));
        assert_eq!(
            panel.handle_key(key(KeyCode::Esc)),
            PanelAction::Add {
                title: "N".into(),
                description: None
            }
        );
        assert_eq!(panel.list.todos.len(), 1);
    }
}
