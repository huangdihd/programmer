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
}

impl SkillsPanel {
    pub fn new() -> Self {
        SkillsPanel { selected: 0 }
    }

    /// Handle a key event, possibly toggling skills in `registry`.
    pub fn handle_key(&mut self, key: KeyEvent, registry: &mut SkillRegistry) -> PanelAction {
        let names: Vec<String> = registry.names().into_iter().cloned().collect();
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
                if let Some(name) = names.get(self.selected) {
                    if registry.toggle(name).is_some() {
                        return PanelAction::Saved;
                    }
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

        let names = registry.names();
        let active_count = names.iter().filter(|n| registry.is_active(n)).count();

        // -- Title --
        Paragraph::new(Line::from(vec![
            Span::styled("🧩  Skills", Style::default().fg(Color::Cyan).bold()),
            Span::styled(
                format!("  ({} installed, {} active)", names.len(), active_count),
                Style::default().fg(Color::Gray).italic(),
            ),
        ]))
        .render(chunks[0], buf);

        // -- Skill list --
        if names.is_empty() {
            Paragraph::new(vec![
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
            ])
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::DarkGray)),
            )
            .render(chunks[1], buf);
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
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::DarkGray)),
                )
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
        let help = Line::from(vec![
            Span::styled(" ↑↓", Style::default().fg(Color::Cyan).bold()),
            Span::styled(" navigate  ", Style::default().fg(Color::Gray)),
            Span::styled("Space/Enter", Style::default().fg(Color::Cyan).bold()),
            Span::styled(" toggle  ", Style::default().fg(Color::Gray)),
            Span::styled("c", Style::default().fg(Color::Cyan).bold()),
            Span::styled(" clear all  ", Style::default().fg(Color::Gray)),
            Span::styled("q/Esc", Style::default().fg(Color::Cyan).bold()),
            Span::styled(" close", Style::default().fg(Color::Gray)),
        ]);
        Paragraph::new(help).render(chunks[2], buf);
    }
}

/// A short label for where a skill was loaded from.
fn source_label(source: &SkillSource) -> String {
    match source {
        SkillSource::Project(_) => "project".to_string(),
        SkillSource::Global(_) => "global".to_string(),
    }
}

/// Truncate a description to `max` chars for single-line display.
fn truncate(s: &str, max: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        format!("{}…", flat.chars().take(max - 1).collect::<String>())
    }
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
