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

//! Incremental search shared by the full-screen management panels
//! (providers, skills, MCP servers).
//!
//! `/` opens the search bar in a panel's list mode; typed characters narrow
//! the list as they are entered. Enter keeps the filter applied and hands
//! keys back to the panel's normal hotkeys; Esc clears the filter (while
//! typing, or later with a filter applied — only an Esc with no filter
//! reaches the panel and closes it). Navigation keys pass through, so ↑/↓
//! work mid-search.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

/// How [`PanelSearch::handle_key`] treated a key.
#[derive(Debug, PartialEq, Eq)]
pub enum SearchKey {
    /// The key was consumed by the search bar. When `changed` is true the
    /// query text changed and the caller should reset its selection to 0.
    Consumed { changed: bool },
    /// Not a search key — the panel should handle it normally.
    Ignored,
}

/// Search-bar state: the query plus whether keystrokes are being captured.
#[derive(Debug, Default)]
pub struct PanelSearch {
    /// Current query. Stays applied after Enter until cleared with Esc.
    query: String,
    /// Whether the search bar has key focus.
    active: bool,
}

impl PanelSearch {
    /// Handle a key while the owning panel is in its list mode.
    pub fn handle_key(&mut self, key: KeyEvent) -> SearchKey {
        if self.active {
            let changed = match key.code {
                KeyCode::Esc => {
                    let had_query = !self.query.is_empty();
                    self.query.clear();
                    self.active = false;
                    had_query
                }
                KeyCode::Enter => {
                    self.active = false;
                    false
                }
                KeyCode::Backspace => self.query.pop().is_some(),
                KeyCode::Char(c) => {
                    self.query.push(c);
                    true
                }
                // Everything else (↑/↓, PageUp…) passes through so the list
                // can be navigated while the query is being typed.
                _ => return SearchKey::Ignored,
            };
            return SearchKey::Consumed { changed };
        }
        match key.code {
            KeyCode::Char('/') => {
                self.active = true;
                SearchKey::Consumed { changed: false }
            }
            // With a filter applied, Esc clears it; only a filter-less Esc
            // falls through to the panel (which closes on it).
            KeyCode::Esc if !self.query.is_empty() => {
                self.query.clear();
                SearchKey::Consumed { changed: true }
            }
            _ => SearchKey::Ignored,
        }
    }

    /// Whether a filter is currently narrowing the list.
    pub fn is_filtering(&self) -> bool {
        !self.query.is_empty()
    }

    /// Case-insensitive substring match against any of the given fields.
    pub fn matches(&self, fields: &[&str]) -> bool {
        if self.query.is_empty() {
            return true;
        }
        let q = self.query.to_lowercase();
        fields.iter().any(|f| f.to_lowercase().contains(&q))
    }

    /// Title line for the list's border showing the query and match count;
    /// `None` when the search bar is closed and no filter is applied.
    pub fn block_title(&self, shown: usize, total: usize) -> Option<Line<'static>> {
        if !self.active && self.query.is_empty() {
            return None;
        }
        let cursor = if self.active { "▌" } else { "" };
        Some(Line::from(vec![
            Span::styled(
                format!(" /{}{cursor} ", self.query),
                Style::default().fg(Color::Cyan),
            ),
            Span::styled(
                format!("({shown}/{total}) "),
                Style::default().fg(Color::DarkGray),
            ),
        ]))
    }

    /// Help-bar spans advertising the search key.
    pub fn help_spans() -> [Span<'static>; 2] {
        [
            Span::styled("/", Style::default().fg(Color::Cyan).bold()),
            Span::styled(" search  ", Style::default().fg(Color::Gray)),
        ]
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
    fn slash_opens_and_typing_filters() {
        let mut s = PanelSearch::default();
        assert_eq!(s.handle_key(key(KeyCode::Char('/'))), SearchKey::Consumed { changed: false });
        assert_eq!(s.handle_key(key(KeyCode::Char('n'))), SearchKey::Consumed { changed: true });
        assert_eq!(s.handle_key(key(KeyCode::Char('P'))), SearchKey::Consumed { changed: true });
        assert!(s.matches(&["npm-server"]));
        assert!(s.matches(&["other", "runs NPm stuff"]));
        assert!(!s.matches(&["unrelated"]));
    }

    #[test]
    fn enter_applies_and_esc_clears_in_two_stages() {
        let mut s = PanelSearch::default();
        s.handle_key(key(KeyCode::Char('/')));
        s.handle_key(key(KeyCode::Char('x')));
        // Enter: filter stays applied, keys return to the panel.
        assert_eq!(s.handle_key(key(KeyCode::Enter)), SearchKey::Consumed { changed: false });
        assert!(s.is_filtering());
        assert_eq!(s.handle_key(key(KeyCode::Char('a'))), SearchKey::Ignored);
        // Esc with a filter applied clears it instead of reaching the panel.
        assert_eq!(s.handle_key(key(KeyCode::Esc)), SearchKey::Consumed { changed: true });
        assert!(!s.is_filtering());
        // A second Esc is the panel's to handle (close).
        assert_eq!(s.handle_key(key(KeyCode::Esc)), SearchKey::Ignored);
    }

    #[test]
    fn navigation_passes_through_while_typing() {
        let mut s = PanelSearch::default();
        s.handle_key(key(KeyCode::Char('/')));
        assert_eq!(s.handle_key(key(KeyCode::Down)), SearchKey::Ignored);
        assert_eq!(s.handle_key(key(KeyCode::Up)), SearchKey::Ignored);
    }

    #[test]
    fn empty_query_matches_everything() {
        let s = PanelSearch::default();
        assert!(s.matches(&["anything"]));
        assert!(s.block_title(1, 1).is_none());
    }
}
