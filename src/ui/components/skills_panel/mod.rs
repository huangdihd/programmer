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
//! Opened with `/skill manage` inside the app. Lists every discovered skill
//! and lets the user activate/deactivate them. Skills themselves are read-only
//! (either built into the binary or loaded from `SKILL.md` files on disk), so
//! this panel only toggles the active set. Every change is reported via
//! [`PanelAction::Saved`] so the app can persist `activated_skills` with the
//! session.

use crate::skills::SkillRegistry;
use crate::skills::skill::SkillSource;
use crate::ui::components::panel_search::{PanelSearch, SearchKey};
use crate::ui::interaction::InteractionAnimation;
use crate::ui::markdown_theme::palette;
use crate::ui::theme::role;
use crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Widget};
use std::cell::{Cell, RefCell};
use unicode_width::UnicodeWidthStr;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MouseTarget {
    Skill(usize),
    Search,
    Clear,
    Close,
}

#[derive(Debug, Clone, Copy)]
struct HitRegion {
    area: Rect,
    target: MouseTarget,
}

#[derive(Debug)]
pub struct SkillsPanel {
    selected: usize,
    search: PanelSearch,
    hit_regions: RefCell<Vec<HitRegion>>,
    scroll_area: Cell<Option<Rect>>,
    hovered: Cell<Option<MouseTarget>>,
    pressed: Cell<Option<MouseTarget>>,
    interaction_animation: InteractionAnimation,
}

impl SkillsPanel {
    pub fn new() -> Self {
        SkillsPanel {
            selected: 0,
            search: PanelSearch::default(),
            hit_regions: RefCell::new(Vec::new()),
            scroll_area: Cell::new(None),
            hovered: Cell::new(None),
            pressed: Cell::new(None),
            interaction_animation: InteractionAnimation::default(),
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
                    && registry.toggle(name).is_some()
                {
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

    /// Handle a pointer event using hit regions from the most recent render.
    pub fn handle_mouse(&mut self, mouse: MouseEvent, registry: &mut SkillRegistry) -> PanelAction {
        let target = self
            .hit_regions
            .borrow()
            .iter()
            .find(|hit| hit.area.contains((mouse.column, mouse.row).into()))
            .map(|hit| hit.target);
        match mouse.kind {
            MouseEventKind::Moved | MouseEventKind::Drag(MouseButton::Left) => {
                self.set_mouse_hover(target);
            }
            MouseEventKind::Down(MouseButton::Left) => {
                self.set_mouse_hover(target);
                self.pressed.set(target);
                self.interaction_animation.cancel();
                if let Some(MouseTarget::Skill(index)) = target {
                    self.selected = index;
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.set_mouse_hover(target);
                let activate = self
                    .pressed
                    .replace(None)
                    .filter(|pressed| Some(*pressed) == target);
                if let Some(target) = activate {
                    self.interaction_animation.restart();
                    return self.activate_mouse_target(target, registry);
                }
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                if self.scroll_area.get().is_some_and(|area| {
                    area.contains((mouse.column, mouse.row).into())
                }) =>
            {
                let code = if mouse.kind == MouseEventKind::ScrollUp {
                    KeyCode::Up
                } else {
                    KeyCode::Down
                };
                return self.handle_key(KeyEvent::new(code, mouse.modifiers), registry);
            }
            _ => {}
        }
        PanelAction::None
    }

    fn activate_mouse_target(
        &mut self,
        target: MouseTarget,
        registry: &mut SkillRegistry,
    ) -> PanelAction {
        match target {
            MouseTarget::Skill(index) => {
                self.selected = index;
                self.search.blur();
                self.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), registry)
            }
            MouseTarget::Search => {
                self.search.focus();
                PanelAction::None
            }
            MouseTarget::Clear => {
                self.search.blur();
                if registry.has_active() {
                    registry.clear();
                    PanelAction::Saved
                } else {
                    PanelAction::None
                }
            }
            MouseTarget::Close => PanelAction::Close,
        }
    }

    /// Whether pointer feedback still needs animation frames.
    pub fn interaction_animation_active(&self) -> bool {
        self.interaction_animation.active()
    }

    pub(crate) fn cancel_pointer_gesture(&mut self) {
        self.hovered.set(None);
        self.pressed.set(None);
        self.interaction_animation.cancel();
    }

    fn set_mouse_hover(&mut self, target: Option<MouseTarget>) {
        if self.hovered.get() != target {
            self.hovered.set(target);
            if target.is_some() {
                self.interaction_animation.restart();
            } else {
                self.interaction_animation.cancel();
            }
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
            Span::styled(
                "🧩  Skills",
                Style::default().fg(role::SECTION_SKILLS).bold(),
            ),
            Span::styled(
                format!("  ({total} installed, {active_count} active)"),
                Style::default().fg(palette::MUTED).italic(),
            ),
        ]))
        .render(chunks[0], buf);

        let mut list_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(role::BORDER));
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
                        Style::default().fg(palette::MUTED),
                    )),
                ]
            } else {
                vec![
                    Line::from(""),
                    Line::from(Span::styled(
                        "  No skills installed.",
                        Style::default().fg(palette::MUTED),
                    )),
                    Line::from(Span::styled(
                        "  Place SKILL.md files in .programmer/skills/<name>/ or \
                         ~/.config/programmer/skills/<name>/.",
                        Style::default().fg(palette::FAINT),
                    )),
                ]
            };
            Paragraph::new(message)
                .block(list_block)
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
                        Style::default().fg(palette::GREEN).bold()
                    } else {
                        Style::default().fg(palette::TEXT)
                    };
                    let mut first = vec![
                        Span::styled(marker, name_style),
                        Span::styled((*name).clone(), name_style),
                    ];
                    if is_active {
                        first.push(Span::styled(
                            "  [active]",
                            Style::default().fg(palette::GREEN),
                        ));
                    }
                    first.push(Span::styled(
                        format!("  · {source}"),
                        Style::default().fg(palette::FAINT),
                    ));
                    let second = Line::from(Span::styled(
                        format!("    {}", truncate(&desc, 90)),
                        Style::default().fg(palette::MUTED),
                    ));
                    ListItem::new(vec![Line::from(first), second])
                })
                .collect();
            let list = List::new(items)
                .block(list_block)
                .highlight_style(
                    Style::default()
                        .fg(role::SELECTION_TEXT)
                        .bg(role::SELECTION_BG)
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol("❯ ");
            let mut list_state = ListState::default();
            list_state.select(Some(self.selected.min(names.len() - 1)));
            ratatui::widgets::StatefulWidget::render(list, chunks[1], buf, &mut list_state);
        }

        // -- Help bar --
        let mut help = vec![
            Span::styled(" ↑↓", Style::default().fg(palette::BLUE).bold()),
            Span::styled(" navigate  ", Style::default().fg(palette::MUTED)),
            Span::styled("Space/Enter", Style::default().fg(palette::BLUE).bold()),
            Span::styled(" toggle  ", Style::default().fg(palette::MUTED)),
        ];
        help.extend(PanelSearch::help_spans());
        help.extend([
            Span::styled("c", Style::default().fg(palette::BLUE).bold()),
            Span::styled(" clear all  ", Style::default().fg(palette::MUTED)),
            Span::styled("q/Esc", Style::default().fg(palette::BLUE).bold()),
            Span::styled(" close", Style::default().fg(palette::MUTED)),
        ]);
        Paragraph::new(Line::from(help)).render(chunks[2], buf);

        let mut hits = Vec::new();
        if !names.is_empty() {
            let inner = Block::default().borders(Borders::ALL).inner(chunks[1]);
            let visible = usize::from(inner.height / 2).max(1);
            let offset = self.selected.saturating_add(1).saturating_sub(visible);
            for (row, index) in (offset..names.len()).take(visible).enumerate() {
                hits.push(HitRegion {
                    area: Rect::new(inner.x, inner.y + row as u16 * 2, inner.width, 2),
                    target: MouseTarget::Skill(index),
                });
            }
        }
        let mut x = chunks[2].x.saturating_add(
            UnicodeWidthStr::width(" ↑↓ navigate  Space/Enter toggle  ") as u16,
        );
        for (label, suffix, target) in [
            ("/", " search  ", MouseTarget::Search),
            ("c", " clear all  ", MouseTarget::Clear),
            ("q/Esc", " close", MouseTarget::Close),
        ] {
            let width = UnicodeWidthStr::width(label) + UnicodeWidthStr::width(suffix);
            hits.push(HitRegion {
                area: Rect::new(x, chunks[2].y, (width as u16).min(chunks[2].right().saturating_sub(x)), 1),
                target,
            });
            x = x.saturating_add(width as u16);
        }
        self.scroll_area
            .set(Some(Block::default().borders(Borders::ALL).inner(chunks[1])));
        paint_mouse_feedback(
            buf,
            &hits,
            self.hovered.get(),
            self.pressed.get(),
            self.interaction_animation.hover_background(),
        );
        *self.hit_regions.borrow_mut() = hits;
    }
}

fn paint_mouse_feedback(
    buf: &mut Buffer,
    hits: &[HitRegion],
    hovered: Option<MouseTarget>,
    pressed: Option<MouseTarget>,
    hover_background: ratatui::style::Color,
) {
    let Some(target) = hovered else { return };
    let Some(hit) = hits.iter().find(|hit| hit.target == target) else {
        return;
    };
    for y in hit.area.y..hit.area.bottom() {
        for x in hit.area.x..hit.area.right() {
            if let Some(cell) = buf.cell_mut((x, y)) {
                if pressed == Some(target) {
                    cell.set_bg(role::INTERACTION_PRESSED_BG);
                    cell.set_fg(role::SELECTION_TEXT);
                } else {
                    cell.set_bg(hover_background);
                }
            }
        }
    }
}

/// A short label for where a skill was loaded from.
fn source_label(source: &SkillSource) -> &'static str {
    match source {
        SkillSource::BuiltIn => "built-in",
        SkillSource::Shared => "shared",
        SkillSource::Global => "global",
        SkillSource::Project => "project",
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

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: crossterm::event::KeyModifiers::NONE,
        }
    }

    #[test]
    fn navigation_clamps_to_list() {
        let mut panel = SkillsPanel::new();
        let mut reg = SkillRegistry::default();
        // With no skills, down does nothing and selection stays at 0.
        assert_eq!(
            panel.handle_key(key(KeyCode::Down), &mut reg),
            PanelAction::None
        );
        assert_eq!(panel.selected, 0);
    }

    #[test]
    fn esc_closes() {
        let mut panel = SkillsPanel::new();
        let mut reg = SkillRegistry::default();
        assert_eq!(
            panel.handle_key(key(KeyCode::Esc), &mut reg),
            PanelAction::Close
        );
    }

    #[test]
    fn toggle_empty_registry_is_noop() {
        let mut panel = SkillsPanel::new();
        let mut reg = SkillRegistry::default();
        assert_eq!(
            panel.handle_key(key(KeyCode::Char(' ')), &mut reg),
            PanelAction::None
        );
    }

    #[test]
    fn mouse_requires_matching_release_to_toggle() {
        let mut panel = SkillsPanel::new();
        let mut registry = SkillRegistry::load();
        let area = Rect::new(0, 0, 100, 14);
        panel.render(&registry, area, &mut Buffer::empty(area));
        let hits = panel.hit_regions.borrow();
        let first = hits.first().unwrap().area;
        let second = hits.get(1).unwrap().area;
        let first_name = panel.filtered_names(&registry)[0].clone();
        let initially_active = registry.is_active(&first_name);
        drop(hits);

        assert_eq!(
            panel.handle_mouse(
                mouse(MouseEventKind::Down(MouseButton::Left), first.x, first.y),
                &mut registry,
            ),
            PanelAction::None
        );
        assert_eq!(registry.is_active(&first_name), initially_active);
        assert_eq!(
            panel.handle_mouse(
                mouse(MouseEventKind::Up(MouseButton::Left), second.x, second.y),
                &mut registry,
            ),
            PanelAction::None
        );
        assert_eq!(registry.is_active(&first_name), initially_active);

        panel.handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), first.x, first.y),
            &mut registry,
        );
        assert_eq!(
            panel.handle_mouse(
                mouse(MouseEventKind::Up(MouseButton::Left), first.x, first.y),
                &mut registry,
            ),
            PanelAction::Saved
        );
        assert_eq!(registry.is_active(&first_name), !initially_active);
    }

    #[test]
    fn mouse_wheel_navigates_skill_list() {
        let mut panel = SkillsPanel::new();
        let mut registry = SkillRegistry::load();
        assert!(registry.names().len() >= 2);

        let area = Rect::new(0, 0, 100, 14);
        panel.render(&registry, area, &mut Buffer::empty(area));
        assert_eq!(
            panel.handle_mouse(mouse(MouseEventKind::ScrollDown, 0, 0), &mut registry),
            PanelAction::None
        );
        assert_eq!(panel.selected, 0);
        let list = panel.scroll_area.get().expect("skill list area");
        assert_eq!(
            panel.handle_mouse(
                mouse(MouseEventKind::ScrollDown, list.x, list.y),
                &mut registry,
            ),
            PanelAction::None
        );
        assert_eq!(panel.selected, 1);
        assert_eq!(
            panel.handle_mouse(
                mouse(MouseEventKind::ScrollUp, list.x, list.y),
                &mut registry,
            ),
            PanelAction::None
        );
        assert_eq!(panel.selected, 0);
    }
}
