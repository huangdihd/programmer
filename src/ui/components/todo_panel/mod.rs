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
use crossterm::event::{KeyCode, KeyEvent};

/// What the panel should do after handling a key event.
#[derive(Debug, PartialEq)]
pub enum PanelAction {
    None,
    Close,
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
                    self.list.add(title, None);
                    let _ = self.list.save_to_file();
                    self.selected = self.list.todos.len().saturating_sub(1);
                    self.add_mode = AddMode::Hidden;
                }
                KeyCode::Enter => {
                    let title = std::mem::take(title);
                    let desc = if input.trim().is_empty() {
                        None
                    } else {
                        Some(std::mem::take(input))
                    };
                    self.list.add(title, desc);
                    let _ = self.list.save_to_file();
                    self.selected = self.list.todos.len().saturating_sub(1);
                    self.add_mode = AddMode::Hidden;
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
                    }
                }
                KeyCode::Enter | KeyCode::Char(' ') => {
                    if let Some(todo) = self.list.todos.get(self.selected) {
                        let id = todo.id.clone();
                        let _ = self.list.toggle_status(&id);
                        let _ = self.list.save_to_file();
                    }
                }
                KeyCode::Char('d') => {
                    if !self.list.todos.is_empty() {
                        let id = self.list.todos[self.selected].id.clone();
                        let _ = self.list.delete(&id);
                        let _ = self.list.save_to_file();
                        if self.selected >= self.list.todos.len() && self.selected > 0 {
                            self.selected -= 1;
                        }
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

    /// Number of visible items the panel can show (used by UI to compute height).
    const VISIBLE_ITEMS: usize = 8;

    pub fn needed_height(&self) -> u16 {
        match &self.add_mode {
            AddMode::Hidden => {
                let item_count = self.list.todos.len().min(Self::VISIBLE_ITEMS);
                (3 + item_count as u16).max(3)
            }
            _ => 5,
        }
    }
}
