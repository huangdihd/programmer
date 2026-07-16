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

//! Right-hand sidebar panel showing diagnostics, MCP status, and todos.
//!
//! The sidebar is toggled with `Ctrl+B`. Each section can be
//! collapsed/expanded by clicking its title. Todo items can be toggled by
//! clicking them. Scroll is handled with the mouse wheel.

pub mod ui;

use crossterm::event::{KeyCode, KeyEvent};

/// Identifies one collapsible section within the sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarSection {
    Diagnostics,
    Mcp,
    Todos,
    Tasks,
}

/// Per-section UI state.
#[derive(Debug)]
pub(crate) struct SectionState {
    key: SidebarSection,
    collapsed: bool,
}

/// What occupies a given line in the rendered sidebar.
#[derive(Debug, Clone)]
pub enum ClickTarget {
    /// Nothing actionable.
    None,
    /// Section title; click toggles collapse.
    Section(SidebarSection),
    /// A todo item; the usize is the index into the sorted todo slice.
    TodoItem(usize),
    /// A diagnostic entry; the usize is the index into the sorted diagnostics
    /// slice.
    Diagnostic(usize),
}

/// The sidebar panel itself.
#[derive(Debug)]
pub struct Sidebar {
    /// Ordered list of sections (top → bottom).
    sections: Vec<SectionState>,
    /// Vertical scroll offset in lines (for the entire sidebar content).
    scroll_offset: u16,
    /// Whether keyboard input is routed to the sidebar.
    pub has_focus: bool,
    /// Click map built by the last render: for each rendered line (after
    /// scrolling), what is clickable there.
    pub click_map: Vec<ClickTarget>,
}

impl Sidebar {
    pub fn new() -> Self {
        let sections = vec![
            SectionState {
                key: SidebarSection::Mcp,
                collapsed: false,
            },
            SectionState {
                key: SidebarSection::Todos,
                collapsed: false,
            },
            SectionState {
                key: SidebarSection::Tasks,
                collapsed: false,
            },
            SectionState {
                key: SidebarSection::Diagnostics,
                collapsed: false,
            },
        ];
        Sidebar {
            sections,
            scroll_offset: 0,
            has_focus: false,
            click_map: Vec::new(),
        }
    }

    /// Fixed width of the sidebar in columns.
    pub fn needed_width() -> u16 {
        32
    }

    /// Current vertical scroll offset.
    pub fn scroll_offset(&self) -> u16 {
        self.scroll_offset
    }

    /// Toggle the collapse state of a section.
    pub fn toggle_section(&mut self, section: SidebarSection) {
        if let Some(s) = self.sections.iter_mut().find(|s| s.key == section) {
            s.collapsed = !s.collapsed;
        }
    }

    // -- scrolling --

    pub fn scroll_down(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_add(1);
    }

    pub fn scroll_up(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_sub(1);
    }

    /// Clamp scroll_offset so it doesn't exceed available content.
    pub fn clamp_scroll(&mut self, total_lines: u16, visible_lines: u16) {
        let max_scroll = total_lines.saturating_sub(visible_lines);
        if self.scroll_offset > max_scroll {
            self.scroll_offset = max_scroll;
        }
    }

    // -- keyboard (minimal) --

    pub fn handle_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.scroll_offset = self.scroll_offset.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.scroll_offset = self.scroll_offset.saturating_add(1);
            }
            _ => {}
        }
    }
}