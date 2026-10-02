// Copyright (C) 2026 huangdihd
// SPDX-License-Identifier: GPL-3.0-or-later

//! Presentation-only activity browser. The caller owns loading and rollback;
//! a rollback request is not an authorization until a supplied preview is confirmed.

use std::collections::BTreeMap;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::Style,
    text::Line,
    widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap},
};

use super::panel_search::{PanelSearch, SearchKey};
use crate::ui::theme::role;

mod dream;
mod graph;
use dream::DreamState;
pub use dream::{DreamPresentation, DreamStatusTone};
use graph::GraphState;
pub use graph::{GraphEventStatus, GraphStatusTone};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityMode {
    Dream,
    SessionGraph,
}

/// Authoritative event classification supplied by the caller, never inferred from display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityEntryKind {
    Question,
    Delegation,
}

#[derive(Debug, Clone)]
pub struct ActivityEntry {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub details: String,
    pub from: Option<String>,
    pub to: Option<String>,
    pub rollback_allowed: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ActivityAction {
    None,
    Close,
    Refresh,
    /// Re-center the read-only graph without opening or waking a session.
    FocusSession(String),
    RequestRollback(String),
    ConfirmRollback(String),
}

#[derive(Debug)]
pub struct ActivityPanel {
    title: String,
    mode: ActivityMode,
    center: String,
    entries: Vec<ActivityEntry>,
    entry_kinds: BTreeMap<String, ActivityEntryKind>,
    session_labels: BTreeMap<String, String>,
    kind_filter: Option<ActivityEntryKind>,
    graph: GraphState,
    dream: DreamState,
    notice: Option<String>,
    selected: usize,
    search: PanelSearch,
    details_focused: bool,
    details_scroll: u16,
    details_limit: u16,
    pending_rollback: Option<String>,
    confirmation: Option<(String, String)>,
}

impl ActivityPanel {
    pub fn new(
        title: String,
        mode: ActivityMode,
        center: String,
        entries: Vec<ActivityEntry>,
    ) -> Self {
        Self {
            title,
            mode,
            center,
            entries,
            entry_kinds: BTreeMap::new(),
            session_labels: BTreeMap::new(),
            kind_filter: None,
            graph: GraphState::default(),
            dream: DreamState::default(),
            notice: None,
            selected: 0,
            search: PanelSearch::default(),
            details_focused: false,
            details_scroll: 0,
            details_limit: 0,
            pending_rollback: None,
            confirmation: None,
        }
    }

    fn visible_entries(&self) -> Vec<&ActivityEntry> {
        let peer = (self.mode == ActivityMode::SessionGraph)
            .then(|| self.current_graph_peer())
            .flatten();
        self.entries
            .iter()
            .filter(|entry| {
                if self.mode == ActivityMode::SessionGraph {
                    if let Some(kind) = self.kind_filter
                        && self.entry_kinds.get(&entry.id) != Some(&kind)
                    {
                        return false;
                    }
                    if Some(self.graph_peer_key(entry)) != peer {
                        return false;
                    }
                }
                self.search.matches(&[
                    &entry.id,
                    &entry.title,
                    &entry.summary,
                    &entry.details,
                    entry.from.as_deref().unwrap_or_default(),
                    entry.to.as_deref().unwrap_or_default(),
                ])
            })
            .collect()
    }

    pub fn set_session_labels(&mut self, labels: BTreeMap<String, String>) {
        self.session_labels = labels;
    }

    fn session_label(&self, id: &str) -> String {
        self.session_labels
            .get(id)
            .cloned()
            .unwrap_or_else(|| id.chars().take(8).collect())
    }

    pub fn selected_entry(&self) -> Option<&ActivityEntry> {
        self.visible_entries().get(self.selected).copied()
    }

    /// Preserve the selected identity across refresh/reordering, not its old index.
    /// Any outstanding rollback preview is invalidated by new backend data.
    pub fn replace_entries(&mut self, entries: Vec<ActivityEntry>) {
        let selected_id = self.selected_entry().map(|entry| entry.id.clone());
        if self.mode == ActivityMode::SessionGraph {
            self.remember_graph_event();
            self.graph.peer = self.current_graph_peer();
        }
        self.entries = entries;
        let visible = self.visible_entries();
        let retained_selection =
            selected_id.and_then(|id| visible.iter().position(|entry| entry.id == id));
        self.selected = retained_selection
            .unwrap_or_else(|| self.selected.min(visible.len().saturating_sub(1)));
        // A confirmation has its own scroll position, not the selected entry's.
        if retained_selection.is_none() || self.confirmation.is_some() {
            self.reset_details();
        }
        self.pending_rollback = None;
        self.confirmation = None;
        self.notice = None;
        if self.mode == ActivityMode::SessionGraph {
            self.graph.peer = self.current_graph_peer();
            self.remember_graph_event();
        }
    }

    /// Pause automatic refresh during both preview loading and confirmation.
    /// Explicit replacement still invalidates a preview, even for an unchanged ID.
    pub fn is_confirming(&self) -> bool {
        self.pending_rollback.is_some() || self.confirmation.is_some()
    }

    /// Display a loading/action error without discarding the current data or scroll.
    pub fn show_notice(&mut self, message: String) {
        self.notice = Some(message);
        self.pending_rollback = None;
    }

    /// Replace authoritative kinds after loading entries. Unclassified events appear
    /// only under All; this keeps existing ActivityEntry constructors compatible.
    pub fn set_entry_kinds(&mut self, kinds: BTreeMap<String, ActivityEntryKind>) {
        let selected_id = self.selected_entry().map(|entry| entry.id.clone());
        self.entry_kinds = kinds;
        let retained_selection = selected_id.and_then(|id| {
            self.visible_entries()
                .iter()
                .position(|entry| entry.id == id)
        });
        self.selected = retained_selection.unwrap_or(0);
        if retained_selection.is_none() {
            self.reset_details();
        }
    }

    fn other_endpoint<'a>(&self, entry: &'a ActivityEntry) -> Option<&'a str> {
        let center = Some(self.center.as_str());
        let peer = if entry.from.as_deref() == center {
            entry.to.as_deref()
        } else if entry.to.as_deref() == center {
            entry.from.as_deref()
        } else {
            None
        };
        peer.filter(|peer| !peer.is_empty() && *peer != self.center)
    }

    /// Supply the backend's explicit preview for the outstanding request.
    /// Returns false for stale IDs, read-only mode, or ineligible entries.
    pub fn show_rollback_confirmation(&mut self, id: String, details: String) -> bool {
        if self.mode != ActivityMode::Dream
            || self.pending_rollback.as_ref() != Some(&id)
            || !self
                .entries
                .iter()
                .any(|entry| entry.id == id && entry.rollback_allowed)
        {
            return false;
        }
        self.pending_rollback = None;
        self.confirmation = Some((id, details));
        self.reset_details();
        true
    }

    fn reset_details(&mut self) {
        self.details_scroll = 0;
        self.details_limit = 0;
    }

    fn scroll_details(&mut self, code: KeyCode) {
        self.details_scroll = match code {
            KeyCode::Up | KeyCode::Char('k') => self.details_scroll.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.details_scroll.saturating_add(1),
            KeyCode::PageUp => self.details_scroll.saturating_sub(10),
            KeyCode::PageDown => self.details_scroll.saturating_add(10),
            KeyCode::Home => 0,
            KeyCode::End => self.details_limit,
            _ => self.details_scroll,
        }
        .min(self.details_limit);
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> ActivityAction {
        if key.kind == KeyEventKind::Release {
            return ActivityAction::None;
        }
        if self.mode == ActivityMode::SessionGraph {
            return self.handle_graph_key(key);
        }
        if self.confirmation.is_some() {
            match key.code {
                KeyCode::Char('y') if key.kind == KeyEventKind::Press => {
                    let (id, _) = self.confirmation.take().expect("confirmation exists");
                    self.reset_details();
                    return ActivityAction::ConfirmRollback(id);
                }
                KeyCode::Esc | KeyCode::Char('n') => {
                    self.confirmation = None;
                    self.reset_details();
                }
                _ => self.scroll_details(key.code),
            }
            return ActivityAction::None;
        }
        // Applied searches must not intercept Escape when ascending from details.
        if self.details_focused && key.code == KeyCode::Esc {
            self.details_focused = false;
            return ActivityAction::None;
        }
        if let SearchKey::Consumed { changed } = self.search.handle_key(key) {
            self.details_focused = false;
            if changed {
                self.selected = 0;
                self.reset_details();
                self.pending_rollback = None;
            }
            return ActivityAction::None;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return ActivityAction::Close,
            KeyCode::Char('R') | KeyCode::F(5) => {
                self.pending_rollback = None;
                return ActivityAction::Refresh;
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.search
                    .handle_key(KeyEvent::new(KeyCode::Enter, key.modifiers));
                self.details_focused = !self.details_focused;
            }
            KeyCode::Enter if self.selected_entry().is_some() => self.details_focused = true,
            KeyCode::Char('m') => self.dream.metadata_expanded = !self.dream.metadata_expanded,
            KeyCode::Char('s') => self.dream.sources_expanded = !self.dream.sources_expanded,
            KeyCode::Char('r')
                if self.mode == ActivityMode::Dream && key.kind == KeyEventKind::Press =>
            {
                if let Some(entry) = self.selected_entry().filter(|entry| entry.rollback_allowed) {
                    let id = entry.id.clone();
                    self.pending_rollback = Some(id.clone());
                    return ActivityAction::RequestRollback(id);
                }
            }
            code if self.details_focused => self.scroll_details(code),
            code => {
                let count = self.visible_entries().len();
                let selected = match code {
                    KeyCode::Up | KeyCode::Char('k') => self.selected.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') => self.selected.saturating_add(1),
                    KeyCode::PageUp => self.selected.saturating_sub(10),
                    KeyCode::PageDown => self.selected.saturating_add(10),
                    KeyCode::Home => 0,
                    KeyCode::End => count.saturating_sub(1),
                    _ => self.selected,
                }
                .min(count.saturating_sub(1));
                if selected != self.selected {
                    self.selected = selected;
                    self.pending_rollback = None;
                    self.reset_details();
                }
            }
        }
        ActivityAction::None
    }

    pub fn render(&mut self, area: Rect, buffer: &mut Buffer) {
        match self.mode {
            ActivityMode::SessionGraph => self.render_session_graph(area, buffer),
            ActivityMode::Dream => self.render_dream(area, buffer),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    pub(super) fn entry(id: &str) -> ActivityEntry {
        ActivityEntry {
            id: id.into(),
            title: id.into(),
            summary: "summary".into(),
            details: (0..80).map(|line| format!("line {line}\n")).collect(),
            from: Some("peer".into()),
            to: Some("current".into()),
            rollback_allowed: true,
        }
    }
    pub(super) fn panel(mode: ActivityMode) -> ActivityPanel {
        ActivityPanel::new(
            "Activity".into(),
            mode,
            "current".into(),
            vec![entry("alpha"), entry("beta")],
        )
    }
    pub(super) fn press(panel: &mut ActivityPanel, code: KeyCode) -> ActivityAction {
        panel.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
    }
    pub(super) fn render(panel: &mut ActivityPanel, width: u16, height: u16) -> String {
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        panel.render(area, &mut buffer);
        buffer
            .content()
            .chunks(usize::from(width.max(1)))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn selection_survives_reordering_and_removal() {
        let mut panel = panel(ActivityMode::Dream);
        press(&mut panel, KeyCode::Down);
        panel.replace_entries(vec![entry("beta"), entry("alpha")]);
        assert_eq!(panel.selected_entry().unwrap().id, "beta");
        panel.replace_entries(vec![entry("alpha")]);
        assert_eq!(panel.selected_entry().unwrap().id, "alpha");
        panel.replace_entries(vec![]);
        assert!(panel.selected_entry().is_none());
        assert_eq!(press(&mut panel, KeyCode::Char('r')), ActivityAction::None);
    }

    #[test]
    fn refresh_preserves_details_scroll_until_selection_disappears() {
        let mut panel = panel(ActivityMode::Dream);
        press(&mut panel, KeyCode::Down);
        press(&mut panel, KeyCode::Tab);
        render(&mut panel, 80, 20);
        press(&mut panel, KeyCode::PageDown);
        let scroll = panel.details_scroll;
        assert!(scroll > 0);
        panel.replace_entries(vec![entry("beta"), entry("alpha")]);
        assert_eq!(panel.selected_entry().unwrap().id, "beta");
        assert_eq!(panel.details_scroll, scroll);
        assert!(panel.details_focused);
        panel.show_notice("refresh failed".into());
        assert_eq!(panel.details_scroll, scroll);
        assert_eq!(panel.selected_entry().unwrap().id, "beta");
        render(&mut panel, 80, 20);
        assert_eq!(panel.details_scroll, scroll);
        let mut shortened = entry("beta");
        shortened.details = "short".into();
        panel.replace_entries(vec![shortened]);
        render(&mut panel, 80, 20);
        assert_eq!(panel.details_scroll, 0);
        panel.replace_entries(vec![entry("alpha")]);
        assert_eq!(panel.details_scroll, 0);
    }

    #[test]
    fn confirmation_blocks_refresh_and_explicit_replacement_invalidates_it() {
        let mut panel = panel(ActivityMode::Dream);
        assert!(!panel.is_confirming());
        press(&mut panel, KeyCode::Char('r'));
        assert!(panel.is_confirming());
        assert!(panel.show_rollback_confirmation("alpha".into(), "preview".into()));
        assert!(panel.is_confirming());
        panel.replace_entries(vec![entry("alpha")]);
        assert!(!panel.is_confirming());
        assert_eq!(press(&mut panel, KeyCode::Char('y')), ActivityAction::None);
        press(&mut panel, KeyCode::Char('r'));
        panel.show_notice("preview failed".into());
        assert!(!panel.is_confirming());
        assert_eq!(panel.selected_entry().unwrap().id, "alpha");
        assert!(render(&mut panel, 80, 20).contains("Notice: preview failed"));
    }

    #[test]
    fn graph_filters_use_typed_kinds_and_recenter_only_other_endpoints() {
        let mut panel = panel(ActivityMode::SessionGraph);
        panel.entries[0].title = "delegation-looking title".into();
        let mut other = entry("other-event");
        other.from = Some("other".into());
        panel.entries.push(other);
        panel.set_entry_kinds(BTreeMap::from([
            ("alpha".into(), ActivityEntryKind::Question),
            ("beta".into(), ActivityEntryKind::Delegation),
        ]));
        press(&mut panel, KeyCode::Enter);
        assert_eq!(panel.visible_entries().len(), 2);
        assert_eq!(
            press(&mut panel, KeyCode::Char('e')),
            ActivityAction::FocusSession("peer".into())
        );
        assert!(render(&mut panel, 120, 28).contains("Events · peer"));
        press(&mut panel, KeyCode::Char('f'));
        assert_eq!(panel.selected_entry().unwrap().id, "alpha");
        press(&mut panel, KeyCode::Char('f'));
        assert_eq!(panel.selected_entry().unwrap().id, "beta");
        press(&mut panel, KeyCode::Char('f'));
        assert_eq!(panel.visible_entries().len(), 2);
        press(&mut panel, KeyCode::Backspace);
        assert_eq!(
            panel.visible_entries().len(),
            2,
            "reset remains peer-scoped"
        );
        panel.replace_entries(vec![ActivityEntry {
            from: Some("current".into()),
            ..entry("self-route")
        }]);
        assert_eq!(press(&mut panel, KeyCode::Char('e')), ActivityAction::None);
        assert_eq!(
            panel.visible_entries().len(),
            1,
            "unknown routes stay accessible"
        );
    }

    #[test]
    fn graph_kind_metadata_refresh_keeps_selection_and_unclassified_events_out() {
        let mut panel = panel(ActivityMode::SessionGraph);
        panel.set_entry_kinds(BTreeMap::from([(
            "beta".into(),
            ActivityEntryKind::Question,
        )]));
        press(&mut panel, KeyCode::Char('f'));
        assert_eq!(panel.visible_entries().len(), 1);
        assert_eq!(panel.selected_entry().unwrap().id, "beta");
        press(&mut panel, KeyCode::Tab);
        render(&mut panel, 100, 28);
        press(&mut panel, KeyCode::PageDown);
        let scroll = panel.details_scroll;
        panel.replace_entries(vec![entry("beta"), entry("alpha")]);
        panel.set_entry_kinds(BTreeMap::from([(
            "beta".into(),
            ActivityEntryKind::Question,
        )]));
        assert_eq!(panel.selected_entry().unwrap().id, "beta");
        assert_eq!(panel.details_scroll, scroll);
        press(&mut panel, KeyCode::Char('f'));
        assert!(panel.selected_entry().is_none());
        assert_eq!(
            press(&mut panel, KeyCode::Char('e')),
            ActivityAction::FocusSession("peer".into())
        );
        press(&mut panel, KeyCode::Backspace);
        assert_eq!(panel.visible_entries().len(), 2);
    }

    #[test]
    fn selected_overflow_peer_is_visible_and_uses_selection_colors() {
        let mut panel = panel(ActivityMode::SessionGraph);
        panel.entries = (0..9)
            .map(|index| {
                let mut entry = entry(&format!("event-{index}"));
                entry.from = Some(format!("peer-{index}"));
                entry
            })
            .collect();
        press(&mut panel, KeyCode::End);
        let area = Rect::new(0, 0, 120, 28);
        let mut buffer = Buffer::empty(area);
        panel.render(area, &mut buffer);
        let rows: Vec<String> = (0..area.height)
            .map(|row| {
                (0..area.width)
                    .map(|column| buffer[(column, row)].symbol())
                    .collect()
            })
            .collect();
        let peer_row = rows
            .iter()
            .position(|row| row.contains("peer-8 ×1"))
            .unwrap();
        assert!(
            peer_row < 13,
            "selected peer must be visible inside peer pane"
        );
        assert!(rows.iter().any(|row| row.contains("Peers (9)")));
        let column = rows[peer_row]
            .chars()
            .position(|character| character == 'p')
            .unwrap();
        assert_eq!(
            buffer[(column as u16, peer_row as u16)].fg,
            crate::ui::markdown_theme::palette::TEXT
        );
        assert_eq!(
            buffer[(column as u16, peer_row as u16)].bg,
            crate::ui::markdown_theme::palette::SURFACE
        );
        assert!(rows.iter().any(|row| row.contains("Enter events")));
    }

    #[test]
    fn search_edits_and_escape_clears_before_closing() {
        let mut panel = panel(ActivityMode::Dream);
        press(&mut panel, KeyCode::Char('/'));
        for character in "BEx".chars() {
            press(&mut panel, KeyCode::Char(character));
        }
        assert!(panel.selected_entry().is_none());
        press(&mut panel, KeyCode::Backspace);
        assert_eq!(panel.selected_entry().unwrap().id, "beta");
        assert!(render(&mut panel, 80, 20).contains("/BE"));
        press(&mut panel, KeyCode::Enter);
        assert_eq!(press(&mut panel, KeyCode::Esc), ActivityAction::None);
        assert_eq!(press(&mut panel, KeyCode::Esc), ActivityAction::Close);
    }

    #[test]
    fn rollback_requires_requested_preview_and_explicit_confirmation() {
        let mut panel = panel(ActivityMode::Dream);
        assert!(!panel.show_rollback_confirmation("alpha".into(), "preview".into()));
        assert_eq!(
            press(&mut panel, KeyCode::Char('r')),
            ActivityAction::RequestRollback("alpha".into())
        );
        assert_eq!(press(&mut panel, KeyCode::Char('y')), ActivityAction::None);
        assert!(!panel.show_rollback_confirmation("beta".into(), "preview".into()));
        assert!(panel.show_rollback_confirmation("alpha".into(), "Exact rollback preview".into()));
        assert!(render(&mut panel, 80, 20).contains("Exact rollback preview"));
        assert_eq!(press(&mut panel, KeyCode::Esc), ActivityAction::None);
        assert_eq!(press(&mut panel, KeyCode::Char('y')), ActivityAction::None);
        press(&mut panel, KeyCode::Char('r'));
        panel.show_rollback_confirmation("alpha".into(), "preview".into());
        assert_eq!(
            press(&mut panel, KeyCode::Char('y')),
            ActivityAction::ConfirmRollback("alpha".into())
        );
        assert_eq!(press(&mut panel, KeyCode::Char('y')), ActivityAction::None);
        press(&mut panel, KeyCode::Char('r'));
        panel.replace_entries(vec![entry("alpha")]);
        assert!(!panel.show_rollback_confirmation("alpha".into(), "stale".into()));
    }

    #[test]
    fn graph_aggregates_edges_and_never_mutates() {
        let mut panel = panel(ActivityMode::SessionGraph);
        let mut outgoing = entry("outgoing");
        outgoing.from = Some("current".into());
        outgoing.to = Some("child".into());
        panel.entries.push(outgoing);
        let screen = render(&mut panel, 100, 28);
        for text in ["current", "peer ×2", "child ×1", "Events", "Details"] {
            assert!(screen.contains(text), "missing {text}");
        }
        assert_eq!(panel.visible_entries().len(), 2);
        assert_eq!(press(&mut panel, KeyCode::Char('r')), ActivityAction::None);
        assert!(!panel.show_rollback_confirmation("alpha".into(), "preview".into()));
    }

    #[test]
    fn confirmation_cancels_and_ignores_release_events() {
        let mut panel = panel(ActivityMode::Dream);
        press(&mut panel, KeyCode::Char('r'));
        panel.show_rollback_confirmation("alpha".into(), "preview".into());
        for (width, height) in [(0, 0), (1, 1), (12, 4), (40, 12)] {
            render(&mut panel, width, height);
        }
        let release = KeyEvent::new_with_kind(
            KeyCode::Char('y'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        );
        assert_eq!(panel.handle_key(release), ActivityAction::None);
        assert!(panel.confirmation.is_some());
        assert_eq!(press(&mut panel, KeyCode::Char('n')), ActivityAction::None);
        assert!(panel.confirmation.is_none());
        panel.entries[0].rollback_allowed = false;
        assert_eq!(press(&mut panel, KeyCode::Char('r')), ActivityAction::None);
    }

    #[test]
    fn small_sizes_and_details_scrolling_are_bounded() {
        for mode in [ActivityMode::Dream, ActivityMode::SessionGraph] {
            let mut panel = panel(mode);
            for (width, height) in [(0, 0), (1, 1), (2, 2), (12, 4), (40, 12), (100, 28)] {
                render(&mut panel, width, height);
                press(&mut panel, KeyCode::Tab);
                render(&mut panel, width, height);
                press(&mut panel, KeyCode::Tab);
            }
            press(&mut panel, KeyCode::Tab);
            if mode == ActivityMode::SessionGraph {
                press(&mut panel, KeyCode::Tab);
            }
            render(&mut panel, 40, 12);
            press(&mut panel, KeyCode::End);
            assert!(panel.details_scroll > 0);
            assert!(render(&mut panel, 40, 12).contains("line 79"));
            press(&mut panel, KeyCode::Home);
            assert_eq!(panel.details_scroll, 0);
        }
    }
    #[test]
    fn named_session_graph_and_dream_preview_render() {
        let mut graph = panel(ActivityMode::SessionGraph);
        graph.set_session_labels(BTreeMap::from([
            ("current".into(), "Current work · ab123456".into()),
            ("peer".into(), "Review · cd123456".into()),
        ]));
        let screen = render(&mut graph, 120, 30);
        assert!(screen.contains("Review · cd123456"));
        assert!(screen.contains("Current work · ab123456"));
        if std::env::var_os("PROGRAMMER_ACTIVITY_PREVIEW").is_some() {
            println!("SESSION GRAPH\n{screen}");
            let mut dream = panel(ActivityMode::Dream);
            println!("DREAM HISTORY\n{}", render(&mut dream, 120, 30));
            press(&mut dream, KeyCode::Char('r'));
            assert!(dream.show_rollback_confirmation(
                "alpha".into(),
                "Undo one run. Later changes block rollback. Sources are not requeued.".into()
            ));
            println!("ROLLBACK CONFIRMATION\n{}", render(&mut dream, 90, 14));
        }
    }
}
