// Copyright (C) 2026 huangdihd
// SPDX-License-Identifier: GPL-3.0-or-later

//! Read-only, peer-scoped graph presentation. Unknown routes are a real group,
//! not a sentinel session ID that could collide with an actual peer.
//! Peers → Events → Details is the Enter/Escape hierarchy; Tab cycles all three.
//! Search and kind filters affect only the selected peer's events. `m` toggles
//! separately supplied record metadata; no graph action accepts or starts work.
//! Use neutral palette tokens here: global SUBTLE/BORDER roles intentionally
//! resolve to blue in other panels. Only graph focus uses their blue accent.
use super::*;
use crate::ui::markdown_theme::palette;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    layout::Position,
    style::Modifier,
    text::{Span, Text},
};

// Each item reserves one blank row above and below its content, even unfocused.
const PEER_ITEM_HEIGHT: u16 = 3;
const EVENT_ITEM_HEIGHT: u16 = 4;

#[derive(Debug, Clone, Copy)]
pub enum GraphStatusTone {
    Pending,
    Answered,
    Failed,
    Unknown,
}

/// Supplied from typed observations, never guessed from task or answer text.
#[derive(Debug, Clone)]
pub struct GraphEventStatus {
    pub label: String,
    pub tone: GraphStatusTone,
}

impl GraphStatusTone {
    fn color(self) -> ratatui::style::Color {
        match self {
            Self::Pending => palette::YELLOW,
            Self::Answered => palette::GREEN,
            Self::Failed => palette::RED,
            Self::Unknown => palette::MUTED,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum PeerKey {
    Session(String),
    Unknown,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Focus {
    #[default]
    Peers,
    Events,
    Details,
}

impl Focus {
    fn next(self) -> Self {
        match self {
            Self::Peers => Self::Events,
            Self::Events => Self::Details,
            Self::Details => Self::Peers,
        }
    }

    fn previous(self) -> Self {
        match self {
            Self::Peers => Self::Details,
            Self::Events => Self::Peers,
            Self::Details => Self::Events,
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct GraphState {
    pub(super) peer: Option<PeerKey>,
    focus: Focus,
    remembered_events: BTreeMap<PeerKey, String>,
    peer_offset: usize,
    event_offset: usize,
    search_editing: bool,
    metadata: BTreeMap<String, String>,
    metadata_expanded: bool,
    statuses: BTreeMap<String, GraphEventStatus>,
    hits: HitRegions,
}

#[derive(Debug, Default)]
struct HitRegions {
    peers: Rect,
    peer_rows: Rect,
    events: Rect,
    event_rows: Rect,
    details: Rect,
    metadata: Rect,
}

#[derive(Debug)]
struct Peer {
    key: PeerKey,
    total: usize,
    questions: usize,
    delegations: usize,
    incoming: usize,
    outgoing: usize,
}

impl ActivityPanel {
    /// Optional record identifiers/provenance, separated from the task body.
    /// Keys are ActivityEntry IDs. Replacing this map does not reset navigation.
    pub fn set_graph_record_metadata(&mut self, metadata: BTreeMap<String, String>) {
        self.graph.metadata = metadata;
    }

    pub fn set_graph_statuses(&mut self, statuses: BTreeMap<String, GraphEventStatus>) {
        self.graph.statuses = statuses;
    }

    fn graph_summary_spans(&self, entry: &ActivityEntry) -> Vec<Span<'static>> {
        let muted = Style::default().fg(palette::MUTED);
        let Some(status) = self.graph.statuses.get(&entry.id) else {
            return vec![Span::styled(entry.summary.clone(), muted)];
        };
        // The adapter owns both strings; matching its exact label only locates
        // the styled span. The color always comes from the typed observation.
        let Some((prefix, suffix)) = entry.summary.split_once(&status.label) else {
            return vec![Span::styled(entry.summary.clone(), muted)];
        };
        vec![
            Span::styled(prefix.to_owned(), muted),
            Span::styled(
                status.label.clone(),
                Style::default().fg(status.tone.color()),
            ),
            Span::styled(suffix.to_owned(), muted),
        ]
    }

    pub(super) fn graph_peer_key(&self, entry: &ActivityEntry) -> PeerKey {
        self.other_endpoint(entry)
            .map(|peer| PeerKey::Session(peer.to_owned()))
            .unwrap_or(PeerKey::Unknown)
    }

    fn graph_peers(&self) -> Vec<Peer> {
        // First-seen order follows the adapter's record order; identity is always
        // the full ID, never a truncated label or a direction-specific edge.
        let mut positions = BTreeMap::new();
        let mut peers: Vec<Peer> = Vec::new();
        for entry in &self.entries {
            let key = self.graph_peer_key(entry);
            let index = *positions.entry(key.clone()).or_insert_with(|| {
                peers.push(Peer {
                    key,
                    total: 0,
                    questions: 0,
                    delegations: 0,
                    incoming: 0,
                    outgoing: 0,
                });
                peers.len() - 1
            });
            let peer = &mut peers[index];
            peer.total += 1;
            match self.entry_kinds.get(&entry.id) {
                Some(ActivityEntryKind::Question) => peer.questions += 1,
                Some(ActivityEntryKind::Delegation) => peer.delegations += 1,
                None => {}
            }
            if peer.key != PeerKey::Unknown {
                peer.incoming += usize::from(entry.to.as_deref() == Some(self.center.as_str()));
                peer.outgoing += usize::from(entry.from.as_deref() == Some(self.center.as_str()));
            }
        }
        peers
    }

    pub(super) fn current_graph_peer(&self) -> Option<PeerKey> {
        self.graph
            .peer
            .as_ref()
            .filter(|selected| {
                self.entries
                    .iter()
                    .any(|entry| self.graph_peer_key(entry) == **selected)
            })
            .cloned()
            .or_else(|| self.entries.first().map(|entry| self.graph_peer_key(entry)))
    }

    fn graph_peer_label(&self, key: &PeerKey) -> String {
        match key {
            PeerKey::Session(id) => self.session_label(id),
            PeerKey::Unknown => "Unknown peer · route unavailable".into(),
        }
    }

    pub(super) fn remember_graph_event(&mut self) {
        if let Some(peer) = self.current_graph_peer()
            && let Some(id) = self.selected_entry().map(|entry| entry.id.clone())
        {
            self.graph.remembered_events.insert(peer, id);
        }
    }

    fn select_graph_peer(&mut self, key: PeerKey) {
        if self.current_graph_peer().as_ref() == Some(&key) {
            return;
        }
        self.remember_graph_event();
        self.graph.peer = Some(key.clone());
        self.selected = self
            .graph
            .remembered_events
            .get(&key)
            .and_then(|id| {
                self.visible_entries()
                    .iter()
                    .position(|entry| &entry.id == id)
            })
            .unwrap_or(0);
        self.graph.event_offset = 0;
        self.reset_details();
    }

    fn select_graph_event(&mut self, selected: usize) {
        let selected = selected.min(self.visible_entries().len().saturating_sub(1));
        if self.selected != selected {
            self.selected = selected;
            self.reset_details();
        }
        self.remember_graph_event();
    }

    fn reset_graph_filter_selection(&mut self) {
        self.selected = 0;
        self.graph.event_offset = 0;
        self.reset_details();
    }

    pub(super) fn handle_graph_key(&mut self, key: KeyEvent) -> ActivityAction {
        // Search owns text keys only while editing. An applied query must not
        // intercept Escape: Escape always follows the region hierarchy.
        if self.graph.search_editing && matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
            self.search
                .handle_key(KeyEvent::new(KeyCode::Enter, key.modifiers));
            self.graph.search_editing = false;
        }
        if self.graph.search_editing
            && let SearchKey::Consumed { changed } = self.search.handle_key(key)
        {
            if matches!(key.code, KeyCode::Enter | KeyCode::Esc) {
                self.graph.search_editing = false;
            }
            if changed {
                self.reset_graph_filter_selection();
            }
            return ActivityAction::None;
        }
        match key.code {
            KeyCode::Char('q') => return ActivityAction::Close,
            KeyCode::Esc => {
                self.graph.focus = match self.graph.focus {
                    Focus::Peers => return ActivityAction::Close,
                    Focus::Events => Focus::Peers,
                    Focus::Details => Focus::Events,
                };
            }
            KeyCode::Tab => self.graph.focus = self.graph.focus.next(),
            KeyCode::BackTab => self.graph.focus = self.graph.focus.previous(),
            KeyCode::Enter => match self.graph.focus {
                Focus::Peers if !self.visible_entries().is_empty() => {
                    self.graph.focus = Focus::Events
                }
                Focus::Events if self.selected_entry().is_some() => {
                    self.graph.focus = Focus::Details
                }
                _ => {}
            },
            KeyCode::Char('R') | KeyCode::F(5) => return ActivityAction::Refresh,
            KeyCode::Char('e') => {
                if let Some(PeerKey::Session(peer)) = self.current_graph_peer() {
                    return ActivityAction::FocusSession(peer);
                }
            }
            KeyCode::Char('/') => {
                self.graph.focus = Focus::Events;
                self.graph.search_editing = true;
                self.search.handle_key(key);
            }
            KeyCode::Backspace => {
                self.search = PanelSearch::default();
                self.kind_filter = None;
                self.reset_graph_filter_selection();
            }
            KeyCode::Char('f') => {
                self.kind_filter = match self.kind_filter {
                    None => Some(ActivityEntryKind::Question),
                    Some(ActivityEntryKind::Question) => Some(ActivityEntryKind::Delegation),
                    Some(ActivityEntryKind::Delegation) => None,
                };
                self.graph.focus = Focus::Events;
                self.reset_graph_filter_selection();
            }
            KeyCode::Char('m') => self.graph.metadata_expanded = !self.graph.metadata_expanded,
            code => self.navigate_graph(code),
        }
        ActivityAction::None
    }

    fn navigate_graph(&mut self, code: KeyCode) {
        match self.graph.focus {
            Focus::Details => self.scroll_details(match code {
                KeyCode::Left => KeyCode::Up,
                KeyCode::Right => KeyCode::Down,
                _ => code,
            }),
            Focus::Peers => {
                let peers = self.graph_peers();
                let selected = peers
                    .iter()
                    .position(|peer| Some(&peer.key) == self.current_graph_peer().as_ref())
                    .unwrap_or(0);
                let index = navigate(
                    code,
                    selected,
                    peers.len(),
                    usize::from(self.graph.hits.peer_rows.height / PEER_ITEM_HEIGHT),
                );
                if let Some(peer) = peers.get(index) {
                    self.select_graph_peer(peer.key.clone());
                }
            }
            Focus::Events => {
                let selected = navigate(
                    code,
                    self.selected,
                    self.visible_entries().len(),
                    usize::from(self.graph.hits.event_rows.height / EVENT_ITEM_HEIGHT),
                );
                self.select_graph_event(selected);
            }
        }
    }

    /// Mouse targets come only from the most recent render, including viewport
    /// offsets. Hidden narrow-layout panes and stale pre-resize rows are cleared.
    pub fn handle_mouse(&mut self, event: MouseEvent) -> ActivityAction {
        if self.mode != ActivityMode::SessionGraph {
            return ActivityAction::None;
        }
        let position = Position::new(event.column, event.row);
        let hits = &self.graph.hits;
        let focus = if hits.peers.contains(position) {
            Focus::Peers
        } else if hits.events.contains(position) {
            Focus::Events
        } else if hits.details.contains(position) {
            Focus::Details
        } else {
            return ActivityAction::None;
        };
        if matches!(
            event.kind,
            MouseEventKind::Down(MouseButton::Left)
                | MouseEventKind::ScrollUp
                | MouseEventKind::ScrollDown
        ) && self.graph.search_editing
        {
            self.search
                .handle_key(KeyEvent::new(KeyCode::Enter, event.modifiers));
            self.graph.search_editing = false;
        }
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.graph.focus = focus;
                if hits.peer_rows.contains(position) {
                    let index = self.graph.peer_offset
                        + usize::from((event.row - hits.peer_rows.y) / PEER_ITEM_HEIGHT);
                    if let Some(peer) = self.graph_peers().get(index) {
                        self.select_graph_peer(peer.key.clone());
                    }
                } else if hits.event_rows.contains(position) {
                    let index = self.graph.event_offset
                        + usize::from((event.row - hits.event_rows.y) / EVENT_ITEM_HEIGHT);
                    if index < self.visible_entries().len() {
                        self.select_graph_event(index);
                    }
                } else if hits.metadata.contains(position) {
                    self.graph.metadata_expanded = !self.graph.metadata_expanded;
                }
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                self.graph.focus = focus;
                self.navigate_graph(if event.kind == MouseEventKind::ScrollUp {
                    KeyCode::Up
                } else {
                    KeyCode::Down
                });
            }
            _ => {}
        }
        ActivityAction::None
    }

    pub(super) fn render_session_graph(&mut self, area: Rect, buffer: &mut Buffer) {
        self.graph.hits = HitRegions::default();
        Clear.render(area, buffer);
        if area.is_empty() {
            return;
        }
        let sections = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(u16::from(self.notice.is_some())),
            Constraint::Min(0),
            Constraint::Length(if area.height >= 8 { 2 } else { 1 }),
        ])
        .split(area);
        Paragraph::new(format!(
            " {} · {} · read-only",
            self.title,
            self.session_label(&self.center)
        ))
        .style(Style::default().fg(role::FOCUS))
        .render(sections[0], buffer);
        if let Some(notice) = &self.notice {
            Paragraph::new(format!("Notice: {notice}"))
                .style(Style::default().fg(role::TOOL_PENDING))
                .render(sections[1], buffer);
        }
        let content = sections[2];
        if content.width < 80 && self.graph.focus == Focus::Details {
            self.render_graph_details(content, buffer);
        } else {
            let (overview, details) = if content.width >= 80 {
                let columns =
                    Layout::horizontal([Constraint::Percentage(54), Constraint::Percentage(46)])
                        .split(content);
                (columns[0], Some(columns[1]))
            } else {
                (content, None)
            };
            let peer_count = self.graph_peers().len();
            let peer_height = (peer_count
                .max(1)
                .saturating_mul(usize::from(PEER_ITEM_HEIGHT))
                .saturating_add(1)
                .min(usize::from(u16::MAX)) as u16)
                .min((overview.height / 2).max(u16::from(overview.height > 0)));
            let rows = Layout::vertical([Constraint::Length(peer_height), Constraint::Min(0)])
                .split(overview);
            self.render_graph_peers(rows[0], buffer);
            self.render_graph_events(rows[1], buffer);
            if let Some(details) = details {
                self.render_graph_details(details, buffer);
            }
        }
        let navigation = match self.graph.focus {
            Focus::Peers => "Peers · ↑↓/jk select session · Enter events · Esc close",
            Focus::Events => "Events · ↑↓/jk select event · Enter details · Esc peers",
            Focus::Details => "Details · ↑↓/jk scroll · Esc events · m metadata",
        };
        Paragraph::new(format!("{navigation}\nTab/⇧Tab focus · / search · f kind · Backspace reset · e re-center · R refresh · q close"))
            .style(Style::default().fg(palette::MUTED)).render(sections[3], buffer);
    }

    fn render_graph_peers(&mut self, area: Rect, buffer: &mut Buffer) {
        self.graph.hits.peers = area;
        let peers = self.graph_peers();
        let selected_peer = self.current_graph_peer();
        let selected = peers
            .iter()
            .position(|peer| Some(&peer.key) == selected_peer.as_ref())
            .unwrap_or(0);
        let body = region(
            area,
            format!("1 Peers ({})", peers.len()),
            self.graph.focus == Focus::Peers,
            false,
            buffer,
        );
        let capacity = usize::from((body.height / PEER_ITEM_HEIGHT).max(1));
        self.graph.hits.peer_rows = Rect {
            height: body.height.min(capacity as u16 * PEER_ITEM_HEIGHT),
            ..body
        };
        self.graph.peer_offset = viewport(self.graph.peer_offset, selected, capacity, peers.len());
        let lines: Vec<_> = peers
            .iter()
            .enumerate()
            .skip(self.graph.peer_offset)
            .take(capacity)
            .flat_map(|(index, peer)| {
                let direction = match (peer.incoming > 0, peer.outgoing > 0) {
                    (true, true) => "↔",
                    (true, false) => "←",
                    (false, true) => "→",
                    _ => "?",
                };
                [
                    Line::default(),
                    Line::from(format!(
                        "{} {} {} ×{} · Q{} D{}",
                        if index == selected { "›" } else { " " },
                        direction,
                        self.graph_peer_label(&peer.key),
                        peer.total,
                        peer.questions,
                        peer.delegations
                    ))
                    .style(row_style(
                        index == selected,
                        self.graph.focus == Focus::Peers,
                    )),
                    Line::default(),
                ]
            })
            .collect();
        if peers.is_empty() {
            Paragraph::new("No related sessions")
                .style(Style::default().fg(palette::MUTED))
                .render(body, buffer);
        } else {
            Paragraph::new(lines).render(body, buffer);
            if self.graph.focus == Focus::Peers {
                fill_selected_row(
                    body,
                    selected.saturating_sub(self.graph.peer_offset),
                    PEER_ITEM_HEIGHT,
                    buffer,
                );
            }
        }
    }

    fn render_graph_events(&mut self, area: Rect, buffer: &mut Buffer) {
        self.graph.hits.events = area;
        let peer = self.current_graph_peer();
        let label = peer
            .as_ref()
            .map(|peer| self.graph_peer_label(peer))
            .unwrap_or_else(|| "No peer".into());
        let body = region(
            area,
            format!("2 Events · {label}"),
            self.graph.focus == Focus::Events,
            false,
            buffer,
        );
        let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(body);
        let kind = match self.kind_filter {
            None => "All",
            Some(ActivityEntryKind::Question) => "Questions",
            Some(ActivityEntryKind::Delegation) => "Delegations",
        };
        let visible = self.visible_entries();
        let total = self
            .entries
            .iter()
            .filter(|entry| Some(self.graph_peer_key(entry)) == peer)
            .count();
        let title = self
            .search
            .block_title(visible.len(), total)
            .unwrap_or_else(|| Line::from(format!("{kind} · {}/{} events", visible.len(), total)));
        Paragraph::new(title)
            .style(Style::default().fg(palette::MUTED))
            .render(rows[0], buffer);
        let count = visible.len();
        let capacity = usize::from((rows[1].height / EVENT_ITEM_HEIGHT).max(1));
        let offset = viewport(self.graph.event_offset, self.selected, capacity, count);
        let lines: Vec<_> = visible
            .iter()
            .enumerate()
            .skip(offset)
            .take(capacity)
            .flat_map(|(index, entry)| {
                let style = row_style(index == self.selected, self.graph.focus == Focus::Events);
                let direction = if self.other_endpoint(entry).is_none() {
                    "? route unavailable"
                } else if entry.from.as_deref() == Some(self.center.as_str()) {
                    "→ sent"
                } else {
                    "← received"
                };
                [
                    Line::default(),
                    Line::from(format!(
                        "{} {}",
                        if index == self.selected { "›" } else { " " },
                        entry.title
                    ))
                    .style(style),
                    Line::from({
                        let mut spans = vec![Span::styled(
                            format!("  {direction} · "),
                            Style::default().fg(palette::MUTED),
                        )];
                        spans.extend(self.graph_summary_spans(entry));
                        spans
                    })
                    .style(style),
                    Line::default(),
                ]
            })
            .collect();
        self.graph.event_offset = offset;
        self.graph.hits.event_rows = Rect {
            height: rows[1].height.min(capacity as u16 * EVENT_ITEM_HEIGHT),
            ..rows[1]
        };
        if count == 0 {
            Paragraph::new("No matching events for this peer")
                .style(Style::default().fg(palette::MUTED))
                .render(rows[1], buffer);
        } else {
            Paragraph::new(lines).render(rows[1], buffer);
            if self.graph.focus == Focus::Events {
                fill_selected_row(
                    rows[1],
                    self.selected.saturating_sub(offset),
                    EVENT_ITEM_HEIGHT,
                    buffer,
                );
            }
        }
    }

    fn render_graph_details(&mut self, area: Rect, buffer: &mut Buffer) {
        self.graph.hits.details = area;
        let body = region(
            area,
            "3 Details".into(),
            self.graph.focus == Focus::Details,
            area.width < 80 && area.x > 0,
            buffer,
        );
        let entry = self.selected_entry();
        let metadata = entry.and_then(|entry| self.graph.metadata.get(&entry.id));
        let text = entry
            .map(|entry| {
                let route = match (&entry.from, &entry.to) {
                    (Some(from), Some(to)) if self.other_endpoint(entry).is_some() => {
                        format!("{} → {}", self.session_label(from), self.session_label(to))
                    }
                    _ => "Unknown peer · route unavailable".into(),
                };
                let mut text = Text::from(vec![
                    Line::styled(
                        entry.title.clone(),
                        Style::default()
                            .fg(palette::TEXT)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Line::from(self.graph_summary_spans(entry)),
                    Line::styled(route, Style::default().fg(palette::MUTED)),
                    Line::default(),
                ]);
                text.extend(Text::from(entry.details.clone()));
                if self.graph.metadata_expanded
                    && let Some(metadata) = metadata
                {
                    text.extend(Text::from(format!(
                        "\n\nRecord metadata / provenance\n{metadata}"
                    )));
                }
                text
            })
            .unwrap_or_else(|| "No event selected".into());
        let rows = Layout::vertical([
            Constraint::Min(0),
            Constraint::Length(u16::from(metadata.is_some())),
        ])
        .split(body);
        if metadata.is_some() {
            self.graph.hits.metadata = rows[1];
            Paragraph::new(if self.graph.metadata_expanded {
                "▾ Record metadata / provenance [m]"
            } else {
                "▸ Record metadata / provenance [m]"
            })
            .style(Style::default().fg(palette::MUTED))
            .render(rows[1], buffer);
        }
        if rows[0].is_empty() {
            return;
        }
        let paragraph = Paragraph::new(text)
            .style(Style::default().fg(palette::TEXT))
            .wrap(Wrap { trim: false });
        self.details_limit = paragraph
            .line_count(rows[0].width)
            .saturating_sub(usize::from(rows[0].height))
            .min(usize::from(u16::MAX)) as u16;
        self.details_scroll = self.details_scroll.min(self.details_limit);
        paragraph
            .scroll((self.details_scroll, 0))
            .render(rows[0], buffer);
    }
}

fn navigate(code: KeyCode, selected: usize, count: usize, page: usize) -> usize {
    match code {
        KeyCode::Up | KeyCode::Left | KeyCode::Char('k') => selected.saturating_sub(1),
        KeyCode::Down | KeyCode::Right | KeyCode::Char('j') => selected.saturating_add(1),
        KeyCode::PageUp => selected.saturating_sub(page.max(1)),
        KeyCode::PageDown => selected.saturating_add(page.max(1)),
        KeyCode::Home => 0,
        KeyCode::End => count.saturating_sub(1),
        _ => selected,
    }
    .min(count.saturating_sub(1))
}

fn viewport(offset: usize, selected: usize, capacity: usize, count: usize) -> usize {
    let offset = offset.min(count.saturating_sub(capacity));
    if selected < offset {
        selected
    } else if selected >= offset.saturating_add(capacity) {
        selected.saturating_sub(capacity.saturating_sub(1))
    } else {
        offset
    }
}

/// Line styles cover glyphs only. Paint the selected item's entire visible
/// rectangle without overwriting status foregrounds or adjacent regions.
fn fill_selected_row(area: Rect, visible_index: usize, height: u16, buffer: &mut Buffer) {
    let offset = visible_index.saturating_mul(usize::from(height));
    if offset >= usize::from(area.height) {
        return;
    }
    let top = area.y + offset as u16;
    for y in top..top.saturating_add(height).min(area.bottom()) {
        for x in area.x..area.right() {
            buffer[(x, y)].set_bg(palette::SURFACE);
        }
    }
}

fn row_style(selected: bool, focused: bool) -> Style {
    match (selected, focused) {
        (true, true) => Style::default()
            .fg(palette::TEXT)
            .bg(palette::SURFACE)
            .add_modifier(Modifier::BOLD),
        (true, false) => Style::default()
            .fg(palette::TEXT)
            .add_modifier(Modifier::BOLD),
        _ => Style::default().fg(palette::TEXT),
    }
}

fn region(
    area: Rect,
    title: String,
    focused: bool,
    left_border: bool,
    buffer: &mut Buffer,
) -> Rect {
    let block = Block::default()
        .borders(if left_border {
            Borders::TOP | Borders::LEFT
        } else {
            Borders::TOP
        })
        .border_style(Style::default().fg(palette::BORDER))
        .title(
            Line::from(format!(" {}{title} ", if focused { "● " } else { "" }))
                .style(Style::default().fg(if focused { role::FOCUS } else { palette::MUTED })),
        );
    let body = block.inner(area);
    block.render(area, buffer);
    body
}

#[cfg(test)]
mod tests {
    use super::super::tests::{entry, panel, press, render};
    use super::*;
    use crossterm::event::KeyModifiers;

    #[test]
    fn every_list_item_has_vertical_padding_without_outer_padding() {
        for focus in [Focus::Peers, Focus::Events] {
            let mut panel = panel(ActivityMode::SessionGraph);
            let mut other = entry("other");
            other.from = Some("other-peer".into());
            panel.entries.push(other);
            panel.graph.focus = focus;
            let area = Rect::new(3, 2, 120, 32);
            let mut buffer = Buffer::empty(area);
            panel.render(area, &mut buffer);
            assert_eq!(buffer[(area.x + 1, area.y)].symbol(), "A");
            assert_eq!(buffer[(area.x, area.bottom() - 1)].symbol(), "T");
            for (rows, height) in [
                (panel.graph.hits.peer_rows, 3),
                (panel.graph.hits.event_rows, 4),
            ] {
                for index in 0..2 {
                    let top = rows.y + index * height;
                    for y in [top, top + height - 1] {
                        for x in rows.x..rows.right() {
                            assert_eq!(buffer[(x, y)].symbol(), " ", "padding at {x},{y}");
                        }
                    }
                    assert_ne!(buffer[(rows.x + 2, top + 1)].symbol(), " ");
                    for y in top..top + height {
                        mouse(
                            &mut panel,
                            Rect::new(rows.x, y, 1, 1),
                            MouseEventKind::Down(MouseButton::Left),
                        );
                        if height == 3 {
                            assert_eq!(
                                panel.current_graph_peer(),
                                Some(PeerKey::Session(
                                    if index == 0 { "peer" } else { "other-peer" }.into()
                                ))
                            );
                        } else {
                            assert_eq!(panel.selected, usize::from(index));
                        }
                    }
                    // Restore the original peer before testing the event rectangles.
                    panel.select_graph_peer(PeerKey::Session("peer".into()));
                }
            }
        }
    }

    #[test]
    fn selected_background_covers_blank_cells_across_the_whole_row() {
        use crate::ui::theme::{self, Theme};
        for theme in [Theme::Dark, Theme::Light] {
            for focus in [Focus::Peers, Focus::Events] {
                let mut panel = panel(ActivityMode::SessionGraph);
                panel.graph.focus = focus;
                let area = Rect::new(0, 0, 140, 25);
                let mut buffer = Buffer::empty(area);
                panel.render(area, &mut buffer);
                let rows = if focus == Focus::Peers {
                    panel.graph.hits.peer_rows
                } else {
                    panel.graph.hits.event_rows
                };
                theme::apply(theme, area, &mut buffer);
                let background = buffer[(rows.x, rows.y)].bg;
                let height = if focus == Focus::Peers { 3 } else { 4 };
                for y in rows.y..rows.y + height {
                    for x in rows.x..rows.right() {
                        assert_eq!(
                            buffer[(x, y)].bg,
                            background,
                            "selection gap at {x},{y}: {theme:?} {focus:?}"
                        );
                    }
                }
                assert_ne!(
                    buffer[(rows.right(), rows.y)].bg,
                    background,
                    "selection must not bleed into details"
                );
            }
        }
    }

    #[test]
    fn resolved_graph_secondary_text_and_dividers_are_not_focus_blue() {
        use crate::ui::{
            markdown_theme::palette,
            theme::{self, Theme},
        };
        for theme in [Theme::Dark, Theme::Light] {
            let mut panel = panel(ActivityMode::SessionGraph);
            let area = Rect::new(0, 0, 140, 26);
            let mut buffer = Buffer::empty(area);
            panel.render(area, &mut buffer);
            theme::apply(theme, area, &mut buffer);
            let mut expected = Buffer::empty(Rect::new(0, 0, 3, 1));
            expected[(0, 0)].set_fg(role::FOCUS);
            expected[(1, 0)].set_fg(palette::MUTED);
            expected[(2, 0)].set_fg(palette::BORDER);
            theme::apply(theme, expected.area, &mut expected);
            assert_eq!(
                buffer[(0, 25)].fg,
                expected[(1, 0)].fg,
                "footer must be neutral in {theme:?}"
            );
            let divider = buffer
                .content
                .iter()
                .find(|cell| cell.symbol() == "─")
                .unwrap();
            assert_eq!(
                divider.fg,
                expected[(2, 0)].fg,
                "divider must be neutral in {theme:?}"
            );
            assert_ne!(row_style(true, false).fg, Some(role::FOCUS));
            assert_ne!(row_style(true, true).bg, Some(role::SELECTION_BG));
        }
    }

    #[test]
    fn status_colors_survive_selection_and_theme_resolution() {
        use crate::ui::theme::{self, Theme};
        for theme in [Theme::Dark, Theme::Light] {
            for (tone, label, color) in [
                (
                    GraphStatusTone::Pending,
                    "Awaiting acceptance",
                    palette::YELLOW,
                ),
                (GraphStatusTone::Answered, "Answered", palette::GREEN),
                (GraphStatusTone::Failed, "Answer failed", palette::RED),
                (GraphStatusTone::Unknown, "Unknown", palette::MUTED),
            ] {
                let mut panel = panel(ActivityMode::SessionGraph);
                panel.entries[0].summary = format!("Question · {label} · just now");
                panel.set_graph_statuses(BTreeMap::from([(
                    "alpha".into(),
                    GraphEventStatus {
                        label: label.into(),
                        tone,
                    },
                )]));
                press(&mut panel, KeyCode::Enter);
                let area = Rect::new(0, 0, 160, 30);
                let mut buffer = Buffer::empty(area);
                panel.render(area, &mut buffer);
                let event_rows = panel.graph.hits.event_rows;
                let status_row = event_rows.y + 2;
                let x = event_rows.x + "  ← received · Question · ".chars().count() as u16;
                let mut expected = Buffer::empty(Rect::new(0, 0, 2, 1));
                expected[(0, 0)].set_fg(color);
                expected[(1, 0)]
                    .set_fg(palette::TEXT)
                    .set_bg(palette::SURFACE);
                theme::apply(theme, expected.area, &mut expected);
                theme::apply(theme, area, &mut buffer);
                assert_eq!(buffer[(x, status_row)].symbol(), &label[..1]);
                assert_eq!(buffer[(x, status_row)].fg, expected[(0, 0)].fg);
                assert_eq!(buffer[(x, status_row)].bg, expected[(1, 0)].bg);
                assert_eq!(
                    buffer[(event_rows.x + 2, event_rows.y + 1)].fg,
                    expected[(1, 0)].fg
                );
                let details = panel.graph.hits.details;
                let detail_status_x = details.x + 1 + "Question · ".len() as u16;
                assert_eq!(
                    buffer[(detail_status_x, details.y + 2)].fg,
                    expected[(0, 0)].fg
                );
            }
        }
    }

    #[test]
    fn task_first_layout_keeps_peer_counts_events_and_details_separate() {
        let mut panel = panel(ActivityMode::SessionGraph);
        panel.entries[0].title = "Review rollback conflict protection".into();
        panel.entries[0].summary = "Delegation · Accepted · queued · 2m ago".into();
        panel.entries[0].details = "Check both memory scopes.\nAccepted does not mean started.\n\nObserved timeline:\n2m ago · Accepted · queued".into();
        panel.entries[1].title = "Does rollback preserve recall statistics?".into();
        panel.entries[1].summary = "Question · Answered (not task completion) · 6m ago".into();
        let mut other = entry("legacy");
        other.from = Some("history-peer".into());
        panel.entries.push(other);
        panel.set_entry_kinds(BTreeMap::from([
            ("alpha".into(), ActivityEntryKind::Delegation),
            ("beta".into(), ActivityEntryKind::Question),
            ("legacy".into(), ActivityEntryKind::Delegation),
        ]));
        panel.set_session_labels(BTreeMap::from([
            ("peer".into(), "Rollback review".into()),
            ("history-peer".into(), "History compatibility".into()),
        ]));
        press(&mut panel, KeyCode::Enter);
        let screen = render(&mut panel, 140, 26);
        assert!(screen.contains("Rollback review ×2 · Q1 D1"));
        assert!(screen.contains("Review rollback conflict protection"));
        assert!(screen.contains("received · Delegation · Accepted"));
        assert!(!screen.contains("Delegation · Delegation"));
        assert!(screen.contains("2 Events · Rollback review"));
        assert!(screen.contains("3 Details"));
        println!("\n{screen}");
    }

    #[test]
    fn three_regions_descend_ascend_cycle_and_empty_enter() {
        let mut panel = panel(ActivityMode::SessionGraph);
        assert_eq!(panel.graph.focus, Focus::Peers);
        press(&mut panel, KeyCode::Enter);
        assert_eq!(panel.graph.focus, Focus::Events);
        press(&mut panel, KeyCode::Enter);
        assert_eq!(panel.graph.focus, Focus::Details);
        assert_eq!(press(&mut panel, KeyCode::Esc), ActivityAction::None);
        assert_eq!(panel.graph.focus, Focus::Events);
        assert_eq!(press(&mut panel, KeyCode::Esc), ActivityAction::None);
        assert_eq!(press(&mut panel, KeyCode::Esc), ActivityAction::Close);
        press(&mut panel, KeyCode::BackTab);
        assert_eq!(panel.graph.focus, Focus::Details);
        press(&mut panel, KeyCode::Tab);
        assert_eq!(panel.graph.focus, Focus::Peers);
        panel.replace_entries(vec![]);
        press(&mut panel, KeyCode::Enter);
        assert_eq!(panel.graph.focus, Focus::Peers);
        press(&mut panel, KeyCode::Tab);
        press(&mut panel, KeyCode::Enter);
        assert_eq!(panel.graph.focus, Focus::Events);
        assert_eq!(press(&mut panel, KeyCode::Char('q')), ActivityAction::Close);
    }

    #[test]
    fn peer_aggregation_is_bidirectional_typed_full_identity_and_unknown_safe() {
        let mut panel = panel(ActivityMode::SessionGraph);
        panel.entries[0].from = Some("same-prefix-A".into());
        panel.entries[1].from = Some("current".into());
        panel.entries[1].to = Some("same-prefix-A".into());
        let mut different = entry("different");
        different.from = Some("same-prefix-B".into());
        let mut missing = entry("missing");
        missing.from = None;
        let mut unrelated = entry("unrelated");
        unrelated.to = Some("not-center".into());
        panel.entries.extend([different, missing, unrelated]);
        panel.set_entry_kinds(BTreeMap::from([
            ("alpha".into(), ActivityEntryKind::Question),
            ("beta".into(), ActivityEntryKind::Delegation),
        ]));
        let peers = panel.graph_peers();
        assert_eq!(peers.len(), 3);
        assert_eq!(
            (peers[0].total, peers[0].incoming, peers[0].outgoing),
            (2, 1, 1)
        );
        assert_eq!((peers[0].questions, peers[0].delegations), (1, 1));
        assert_ne!(peers[0].key, peers[1].key);
        press(&mut panel, KeyCode::End);
        assert_eq!(panel.current_graph_peer(), Some(PeerKey::Unknown));
        assert_eq!(panel.visible_entries().len(), 2);
        assert_eq!(press(&mut panel, KeyCode::Char('e')), ActivityAction::None);
        assert!(render(&mut panel, 120, 30).contains("Unknown peer"));
    }

    #[test]
    fn switching_peers_remembers_events_refresh_retains_identity_focus_and_scroll() {
        let mut panel = panel(ActivityMode::SessionGraph);
        let mut other = entry("other-event");
        other.from = Some("other".into());
        panel.entries.push(other.clone());
        press(&mut panel, KeyCode::Enter);
        press(&mut panel, KeyCode::Down);
        assert_eq!(panel.selected_entry().unwrap().id, "beta");
        press(&mut panel, KeyCode::Esc);
        press(&mut panel, KeyCode::Down);
        assert_eq!(panel.selected_entry().unwrap().id, "other-event");
        press(&mut panel, KeyCode::Up);
        assert_eq!(panel.selected_entry().unwrap().id, "beta");
        press(&mut panel, KeyCode::Enter);
        press(&mut panel, KeyCode::Enter);
        render(&mut panel, 100, 25);
        press(&mut panel, KeyCode::PageDown);
        let scroll = panel.details_scroll;
        assert!(scroll > 0);
        assert_eq!(
            press(&mut panel, KeyCode::Char('R')),
            ActivityAction::Refresh
        );
        panel.replace_entries(vec![other, entry("beta"), entry("alpha")]);
        assert_eq!(panel.selected_entry().unwrap().id, "beta");
        assert_eq!(
            panel.current_graph_peer(),
            Some(PeerKey::Session("peer".into()))
        );
        assert_eq!(panel.graph.focus, Focus::Details);
        assert_eq!(panel.details_scroll, scroll);
        render(&mut panel, 100, 25);
        assert_eq!(panel.details_scroll, scroll);
        panel.replace_entries(vec![entry("alpha")]);
        assert_eq!(panel.details_scroll, 0);
        assert_eq!(panel.graph.focus, Focus::Details);
    }

    #[test]
    fn search_is_peer_scoped_escape_ascends_and_backspace_resets_filters() {
        let mut panel = panel(ActivityMode::SessionGraph);
        let mut other = entry("outside-beta");
        other.from = Some("outside".into());
        panel.entries.push(other);
        press(&mut panel, KeyCode::Char('/'));
        for character in "beta".chars() {
            press(&mut panel, KeyCode::Char(character));
        }
        assert_eq!(panel.graph.focus, Focus::Events);
        assert_eq!(panel.visible_entries().len(), 1);
        assert_eq!(panel.graph_peers().len(), 2);
        press(&mut panel, KeyCode::Enter);
        assert!(panel.search.is_filtering());
        press(&mut panel, KeyCode::Esc);
        assert_eq!(panel.graph.focus, Focus::Peers);
        assert!(panel.search.is_filtering());
        press(&mut panel, KeyCode::Backspace);
        assert_eq!(panel.visible_entries().len(), 2);
        assert!(!panel.search.is_filtering());
        assert_eq!(
            panel.current_graph_peer(),
            Some(PeerKey::Session("peer".into()))
        );
        press(&mut panel, KeyCode::Char('f'));
        assert!(panel.visible_entries().is_empty());
        press(&mut panel, KeyCode::Esc);
        press(&mut panel, KeyCode::Enter);
        assert_eq!(
            panel.graph.focus,
            Focus::Peers,
            "empty events cannot be descended into"
        );
    }

    fn mouse(panel: &mut ActivityPanel, area: Rect, kind: MouseEventKind) {
        assert_eq!(
            panel.handle_mouse(MouseEvent {
                kind,
                column: area.x,
                row: area.y,
                modifiers: KeyModifiers::NONE,
            }),
            ActivityAction::None
        );
    }

    #[test]
    fn mouse_uses_rendered_offsets_focus_scroll_and_narrow_visibility() {
        let mut panel = panel(ActivityMode::SessionGraph);
        panel.entries = (0..20)
            .map(|index| {
                let mut entry = entry(&format!("event-{index}"));
                entry.from = Some(format!("peer-{index}"));
                entry
            })
            .collect();
        press(&mut panel, KeyCode::End);
        render(&mut panel, 120, 25);
        let peer_rows = panel.graph.hits.peer_rows;
        let offset = panel.graph.peer_offset;
        assert!(offset > 0);
        mouse(
            &mut panel,
            peer_rows,
            MouseEventKind::Down(MouseButton::Left),
        );
        assert_eq!(
            panel.current_graph_peer(),
            Some(PeerKey::Session(format!("peer-{offset}")))
        );
        render(&mut panel, 120, 25);
        let event_rows = panel.graph.hits.event_rows;
        mouse(
            &mut panel,
            event_rows,
            MouseEventKind::Down(MouseButton::Left),
        );
        assert_eq!(panel.graph.focus, Focus::Events);
        let details = panel.graph.hits.details;
        mouse(&mut panel, details, MouseEventKind::ScrollDown);
        assert_eq!(panel.graph.focus, Focus::Details);
        assert_eq!(panel.details_scroll, 1);
        render(&mut panel, 40, 20);
        assert!(panel.graph.hits.peers.is_empty());
        assert!(panel.graph.hits.events.is_empty());
        assert!(!panel.graph.hits.details.is_empty());
        press(&mut panel, KeyCode::Esc);
        render(&mut panel, 40, 20);
        assert!(panel.graph.hits.details.is_empty());
        assert!(!panel.graph.hits.events.is_empty());
        render(&mut panel, 0, 0);
        assert!(panel.graph.hits.peers.is_empty());
        assert!(panel.graph.hits.events.is_empty());
    }

    #[test]
    fn search_escape_clears_editing_without_leaving_events_and_footer_tracks_focus() {
        let mut panel = panel(ActivityMode::SessionGraph);
        assert!(render(&mut panel, 140, 25).contains("Peers · ↑↓/jk select session"));
        press(&mut panel, KeyCode::Char('/'));
        press(&mut panel, KeyCode::Char('z'));
        assert!(panel.visible_entries().is_empty());
        press(&mut panel, KeyCode::Esc);
        assert!(!panel.search.is_filtering());
        assert!(!panel.graph.search_editing);
        assert_eq!(panel.graph.focus, Focus::Events);
        assert!(render(&mut panel, 140, 25).contains("Events · ↑↓/jk select event"));
        press(&mut panel, KeyCode::Enter);
        assert!(render(&mut panel, 140, 25).contains("Details · ↑↓/jk scroll"));
    }

    #[test]
    fn event_mouse_rows_respect_scroll_offset_and_refresh_retains_viewport() {
        let mut panel = panel(ActivityMode::SessionGraph);
        panel.entries = (0..30)
            .map(|index| entry(&format!("event-{index}")))
            .collect();
        press(&mut panel, KeyCode::Enter);
        press(&mut panel, KeyCode::End);
        render(&mut panel, 120, 20);
        let offset = panel.graph.event_offset;
        assert!(offset > 0);
        let event_rows = panel.graph.hits.event_rows;
        mouse(
            &mut panel,
            event_rows,
            MouseEventKind::Down(MouseButton::Left),
        );
        assert_eq!(
            panel.selected_entry().unwrap().id,
            format!("event-{offset}")
        );
        let second_line = Rect::new(event_rows.x, event_rows.y + 1, 1, 1);
        mouse(
            &mut panel,
            second_line,
            MouseEventKind::Down(MouseButton::Left),
        );
        assert_eq!(
            panel.selected_entry().unwrap().id,
            format!("event-{offset}")
        );
        panel.replace_entries(panel.entries.clone());
        render(&mut panel, 120, 20);
        assert_eq!(panel.graph.event_offset, offset);
        assert_eq!(panel.graph.focus, Focus::Events);
        let event_rows = panel.graph.hits.event_rows;
        mouse(&mut panel, event_rows, MouseEventKind::ScrollUp);
        assert_eq!(
            panel.selected_entry().unwrap().id,
            format!("event-{}", offset - 1)
        );
    }

    #[test]
    fn tiny_sizes_are_safe_in_every_focus_and_release_does_not_navigate() {
        let mut panel = panel(ActivityMode::SessionGraph);
        for focus in [Focus::Peers, Focus::Events, Focus::Details] {
            panel.graph.focus = focus;
            for width in 0..8 {
                for height in 0..8 {
                    render(&mut panel, width, height);
                    assert!(panel.details_scroll <= panel.details_limit);
                }
            }
        }
        panel.graph.focus = Focus::Peers;
        let release =
            KeyEvent::new_with_kind(KeyCode::Tab, KeyModifiers::NONE, KeyEventKind::Release);
        assert_eq!(panel.handle_key(release), ActivityAction::None);
        assert_eq!(panel.graph.focus, Focus::Peers);
        assert_eq!(row_style(true, true).bg, Some(palette::SURFACE));
        assert_ne!(row_style(true, false).bg, Some(role::SELECTION_BG));
    }

    #[test]
    fn metadata_is_opt_in_and_read_only_and_inactive_selection_is_distinct() {
        let mut panel = panel(ActivityMode::SessionGraph);
        panel.entries[0].details = "Task body".into();
        panel.set_graph_record_metadata(BTreeMap::from([(
            "alpha".into(),
            "source-record-secret-marker".into(),
        )]));
        let screen = render(&mut panel, 120, 25);
        assert!(screen.contains("Task body"));
        assert!(!screen.contains("source-record-secret-marker"));
        assert_eq!(
            panel.graph.hits.peers.height, 4,
            "one peer uses only its padded row and heading"
        );
        press(&mut panel, KeyCode::Char('m'));
        assert!(render(&mut panel, 120, 25).contains("source-record-secret-marker"));
        let toggle = panel.graph.hits.metadata;
        mouse(&mut panel, toggle, MouseEventKind::Down(MouseButton::Left));
        assert!(!panel.graph.metadata_expanded);
        assert_eq!(press(&mut panel, KeyCode::Char('r')), ActivityAction::None);
        assert!(!panel.show_rollback_confirmation("alpha".into(), "preview".into()));
        assert_ne!(row_style(true, true), row_style(true, false));
    }
}
