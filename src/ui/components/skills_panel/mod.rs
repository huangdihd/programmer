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

//! Full-screen skills management panel.
//!
//! Opened with `/skills manage` inside the app. Lists every discovered skill
//! and lets the user activate/deactivate them. Skills themselves are read-only
//! (loaded from `SKILL.md` files on disk), so this panel only toggles the
//! active set. Every change is reported via [`PanelAction::Saved`] so the app
//! can persist `activated_skills` with the session.

use crate::skills::SkillRegistry;
use crate::skills::skill::SkillSource;
use crate::ui::components::panel_search::{PanelSearch, SearchKey};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Widget};

/// What the app should do after the panel handled a key.
#[derive(Debug, PartialEq)]
pub enum PanelAction {
    /// Nothing to do; the panel only updated its own state.
    None,
    /// Close the panel.
    Close,
    /// The active set changed: persist `activated_skills` with the session.
    Saved,
}

#[derive(Debug)]
pub struct SkillsPanel {
    selected: usize,
    search: PanelSearch,
}

impl SkillsPanel {
    pub fn new() -> Self {
        SkillsPanel {
            selected: 0,
            search: PanelSearch::default(),
        }
    }

    /// Skill names passing the current search filter (name or description).
    fn filtered_names(&self, registry: &SkillRegistry) -> Vec<String> {
        registry
            .names()
            .into_iter()
            .filter(|name| {
                let desc = registry
                    .get(name)
                    .map(|s| s.description.clone())
                    .unwrap_or_default();
                self.search.matches(&[name.as_str(), desc.as_str()])
            })
            .cloned()
            .collect()
    }

    /// Handle a key event, possibly toggling skills in `registry`.
    pub fn handle_key(&mut self, key: KeyEvent, registry: &mut SkillRegistry) -> PanelAction {
        if let SearchKey::Consumed { changed } = self.search.handle_key(key) {
            if changed {
                self.selected = 0;
            }
            return PanelAction::None;
        }
        let names = self.filtered_names(registry);
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => PanelAction::Close,
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
                PanelAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.selected + 1 < names.len() {
                    self.selected += 1;
                }
                PanelAction::None
            }
            KeyCode::Char(' ') | KeyCode::Enter => {
                if let Some(name) = names.get(self.selected)
                    && registry.toggle(name).is_some() {
                        return PanelAction::Saved;
                    }
                PanelAction::None
            }
            // Deactivate every skill at once.
            KeyCode::Char('c') => {
                if registry.has_active() {
                    registry.clear();
                    return PanelAction::Saved;
                }
                PanelAction::None
            }
            _ => PanelAction::None,
        }
    }

    pub fn render(&self, registry: &SkillRegistry, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),
                Constraint::Min(3),
                Constraint::Length(2),
            ])
            .split(area);

        let total = registry.names().len();
        let active_count = registry
            .names()
            .iter()
            .filter(|n| registry.is_active(n))
            .count();
        let names = self.filtered_names(registry);

        // -- Title --
        Paragraph::new(Line::from(vec![
            Span::styled("🧩  Skills", Style::default().fg(Color::Cyan).bold()),
            Span::styled(
                format!("  ({total} installed, {active_count} active)"),
                Style::default().fg(Color::Gray).italic(),
            ),
        ]))
        .render(chunks[0], buf);

        let mut list_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));
        if let Some(title) = self.search.block_title(names.len(), total) {
            list_block = list_block.title(title);
        }

        // -- Skill list --
        if names.is_empty() {
            let message = if total > 0 {
                vec![
                    Line::from(""),
                    Line::from(Span::styled(
                        "  No skills match the search.",
                        Style::default().fg(Color::Gray),
                    )),
                ]
            } else {
                vec![
                    Line::from(""),
                    Line::from(Span::styled(
                        "  No skills installed.",
                        Style::default().fg(Color::Gray),
                    )),
                    Line::from(Span::styled(
                        "  Place SKILL.md files in .programmer/skills/<name>/ or \
                         ~/.config/programmer/skills/<name>/.",
                        Style::default().fg(Color::DarkGray),
                    )),
                ]
            };
            Paragraph::new(message).block(list_block).render(chunks[1], buf);
        } else {
            let items: Vec<ListItem> = names
                .iter()
                .map(|name| {
                    let is_active = registry.is_active(name);
                    let (desc, source) = registry
                        .get(name)
                        .map(|s| (s.description.clone(), source_label(&s.source)))
                        .unwrap_or_default();

                    let marker = if is_active { "◉ " } else { "○ " };
                    let name_style = if is_active {
                        Style::default().fg(Color::Green).bold()
                    } else {
                        Style::default().fg(Color::White)
                    };
                    let mut first = vec![
                        Span::styled(marker, name_style),
                        Span::styled((*name).clone(), name_style),
                    ];
                    if is_active {
                        first.push(Span::styled(
                            "  [active]",
                            Style::default().fg(Color::Green),
                        ));
                    }
                    first.push(Span::styled(
                        format!("  · {source}"),
                        Style::default().fg(Color::DarkGray),
                    ));
                    let second = Line::from(Span::styled(
                        format!("    {}", truncate(&desc, 90)),
                        Style::default().fg(Color::Gray),
                    ));
                    ListItem::new(vec![Line::from(first), second])
                })
                .collect();
            let list = List::new(items)
                .block(list_block)
                .highlight_style(
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol("❯ ");
            let mut list_state = ListState::default();
            list_state.select(Some(self.selected.min(names.len() - 1)));
            ratatui::widgets::StatefulWidget::render(list, chunks[1], buf, &mut list_state);
        }

        // -- Help bar --
        let mut help = vec![
            Span::styled(" ↑↓", Style::default().fg(Color::Cyan).bold()),
            Span::styled(" navigate  ", Style::default().fg(Color::Gray)),
            Span::styled("Space/Enter", Style::default().fg(Color::Cyan).bold()),
            Span::styled(" toggle  ", Style::default().fg(Color::Gray)),
        ];
        help.extend(PanelSearch::help_spans());
        help.extend([
            Span::styled("c", Style::default().fg(Color::Cyan).bold()),
            Span::styled(" clear all  ", Style::default().fg(Color::Gray)),
            Span::styled("q/Esc", Style::default().fg(Color::Cyan).bold()),
            Span::styled(" close", Style::default().fg(Color::Gray)),
        ]);
        Paragraph::new(Line::from(help)).render(chunks[2], buf);
    }
}

/// A short label for where a skill was loaded from.
fn source_label(source: &SkillSource) -> String {
    match source {
        SkillSource::Project => "project".to_string(),
        SkillSource::Global => "global".to_string(),
    }
}

/// Flatten whitespace and truncate a description for single-line display.
fn truncate(s: &str, max: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    crate::ui::text::truncate_to_width(&flat, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
    }

    #[test]
    fn navigation_clamps_to_list() {
        let mut panel = SkillsPanel::new();
        let mut reg = SkillRegistry::default();
        // With no skills, down does nothing and selection stays at 0.
        assert_eq!(panel.handle_key(key(KeyCode::Down), &mut reg), PanelAction::None);
        assert_eq!(panel.selected, 0);
    }

    #[test]
    fn esc_closes() {
        let mut panel = SkillsPanel::new();
        let mut reg = SkillRegistry::default();
        assert_eq!(panel.handle_key(key(KeyCode::Esc), &mut reg), PanelAction::Close);
    }

    #[test]
    fn toggle_empty_registry_is_noop() {
        let mut panel = SkillsPanel::new();
        let mut reg = SkillRegistry::default();
        assert_eq!(panel.handle_key(key(KeyCode::Char(' ')), &mut reg), PanelAction::None);
    }
}
