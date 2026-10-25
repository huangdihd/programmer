// Copyright (C) 2026 huangdihd
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Widget};

use crate::ui::interaction::InteractionAnimation;
use crate::ui::markdown_theme::palette;
use crate::ui::theme::role;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RestoreMode {
    CodeAndConversation,
    ConversationOnly,
    CodeOnly,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PanelAction {
    None,
    Close,
    Fork {
        checkpoint_id: u64,
    },
    Restore {
        checkpoint_id: u64,
        mode: RestoreMode,
    },
}

#[derive(Debug, Clone)]
struct Entry {
    id: u64,
    prompt: String,
    file_count: usize,
    recovery: bool,
}

#[derive(Debug)]
enum Mode {
    Checkpoints,
    Actions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MouseTarget {
    Checkpoint(usize),
    Action(usize),
    Back,
    Close,
}

#[derive(Debug)]
pub struct RewindPanel {
    entries: Vec<Entry>,
    selected: usize,
    action_selected: usize,
    mode: Mode,
    mouse_hover: Option<MouseTarget>,
    mouse_pressed: Option<MouseTarget>,
    interaction_animation: InteractionAnimation,
}

impl RewindPanel {
    pub(crate) fn new(checkpoints: &[crate::checkpoint::Checkpoint]) -> Self {
        let entries = checkpoints
            .iter()
            .rev()
            .map(|checkpoint| Entry {
                id: checkpoint.id,
                prompt: checkpoint
                    .label
                    .clone()
                    .unwrap_or_else(|| checkpoint.prompt.clone()),
                file_count: checkpoint.files.len(),
                recovery: checkpoint.recovery,
            })
            .collect();
        Self {
            entries,
            selected: 0,
            action_selected: 0,
            mode: Mode::Checkpoints,
            mouse_hover: None,
            mouse_pressed: None,
            interaction_animation: InteractionAnimation::default(),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Handle pointer input with keyboard-equivalent navigation and actions.
    /// A choice activates only when press and release hit the same target.
    pub(crate) fn handle_mouse(&mut self, mouse: MouseEvent, area: Rect) -> PanelAction {
        let target = self.mouse_target(mouse.column, mouse.row, area);
        match mouse.kind {
            MouseEventKind::Moved => self.set_mouse_hover(target),
            MouseEventKind::Down(MouseButton::Left) => {
                self.set_mouse_hover(target);
                self.mouse_pressed = target;
                self.select_mouse_target(target);
                self.interaction_animation.cancel();
            }
            MouseEventKind::Drag(MouseButton::Left) => self.set_mouse_hover(target),
            MouseEventKind::Up(MouseButton::Left) => {
                self.set_mouse_hover(target);
                let activate = self
                    .mouse_pressed
                    .take()
                    .filter(|pressed| Some(*pressed) == target);
                if activate.is_some() {
                    self.interaction_animation.restart();
                }
                return activate.map_or(PanelAction::None, |target| {
                    self.activate_mouse_target(target)
                });
            }
            MouseEventKind::ScrollUp if self.pointer_over_list(mouse.column, mouse.row, area) => {
                return self.handle_key(KeyEvent::new(KeyCode::Up, mouse.modifiers));
            }
            MouseEventKind::ScrollDown
                if self.pointer_over_list(mouse.column, mouse.row, area) =>
            {
                return self.handle_key(KeyEvent::new(KeyCode::Down, mouse.modifiers));
            }
            _ => {}
        }
        PanelAction::None
    }

    pub(crate) fn interaction_animation_active(&self) -> bool {
        self.interaction_animation.active()
    }

    pub(crate) fn cancel_pointer_gesture(&mut self) {
        self.mouse_hover = None;
        self.mouse_pressed = None;
        self.interaction_animation.cancel();
    }

    fn set_mouse_hover(&mut self, target: Option<MouseTarget>) {
        if self.mouse_hover != target {
            self.mouse_hover = target;
            if target.is_some() {
                self.interaction_animation.restart();
            } else {
                self.interaction_animation.cancel();
            }
        }
    }

    fn select_mouse_target(&mut self, target: Option<MouseTarget>) {
        match target {
            Some(MouseTarget::Checkpoint(index)) => self.selected = index,
            Some(MouseTarget::Action(index)) => self.action_selected = index,
            _ => {}
        }
    }

    fn activate_mouse_target(&mut self, target: MouseTarget) -> PanelAction {
        self.select_mouse_target(Some(target));
        match target {
            MouseTarget::Checkpoint(_) | MouseTarget::Action(_) => {
                self.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            }
            MouseTarget::Back | MouseTarget::Close => {
                self.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            }
        }
    }

    fn mouse_target(&self, column: u16, row: u16, area: Rect) -> Option<MouseTarget> {
        if !area.contains((column, row).into()) {
            return None;
        }
        let chunks = panel_chunks(area);
        let content = Block::default().borders(Borders::ALL).inner(chunks[1]);
        if content.contains((column, row).into()) {
            let visible = usize::from(content.height);
            if visible == 0 {
                return None;
            }
            return match self.mode {
                Mode::Checkpoints => {
                    let offset = self.selected.saturating_add(1).saturating_sub(visible);
                    let index = offset + usize::from(row - content.y);
                    (index < self.entries.len()).then_some(MouseTarget::Checkpoint(index))
                }
                Mode::Actions => {
                    let offset = self
                        .action_selected
                        .saturating_add(1)
                        .saturating_sub(visible);
                    let index = offset + usize::from(row - content.y);
                    (index < self.action_count()).then_some(MouseTarget::Action(index))
                }
            };
        }
        if row != chunks[2].y {
            return None;
        }
        match self.mode {
            Mode::Checkpoints => hit_horizontal_target(
                column,
                chunks[2].x,
                &[
                    ("↑↓ navigate  Enter choose restore mode  ", None),
                    ("Esc close", Some(MouseTarget::Close)),
                ],
            ),
            Mode::Actions => hit_horizontal_target(
                column,
                chunks[2].x,
                &[
                    ("↑↓ navigate  Enter confirm  ", None),
                    ("Esc back", Some(MouseTarget::Back)),
                ],
            ),
        }
    }

    fn pointer_over_list(&self, column: u16, row: u16, area: Rect) -> bool {
        Block::default()
            .borders(Borders::ALL)
            .inner(panel_chunks(area)[1])
            .contains((column, row).into())
    }

    fn action_count(&self) -> usize {
        if self
            .entries
            .get(self.selected)
            .is_some_and(|entry| entry.recovery)
        {
            2
        } else {
            5
        }
    }

    fn mouse_style(&self, target: MouseTarget) -> Option<Style> {
        if self.mouse_pressed == Some(target) && self.mouse_hover == Some(target) {
            Some(
                Style::default()
                    .fg(role::SELECTION_TEXT)
                    .bg(role::INTERACTION_PRESSED_BG)
                    .bold(),
            )
        } else if self.mouse_hover == Some(target) {
            Some(
                Style::default()
                    .fg(role::SELECTION_TEXT)
                    .bg(self.interaction_animation.hover_background()),
            )
        } else {
            None
        }
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> PanelAction {
        match self.mode {
            Mode::Checkpoints => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => PanelAction::Close,
                KeyCode::Up | KeyCode::Char('k') => {
                    self.selected = self.selected.saturating_sub(1);
                    PanelAction::None
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.selected = (self.selected + 1).min(self.entries.len().saturating_sub(1));
                    PanelAction::None
                }
                KeyCode::Enter if !self.entries.is_empty() => {
                    self.mode = Mode::Actions;
                    self.action_selected = 0;
                    PanelAction::None
                }
                _ => PanelAction::None,
            },
            Mode::Actions => match key.code {
                KeyCode::Esc => {
                    self.mode = Mode::Checkpoints;
                    PanelAction::None
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.action_selected = self.action_selected.saturating_sub(1);
                    PanelAction::None
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let last = if self.entries[self.selected].recovery {
                        1
                    } else {
                        4
                    };
                    self.action_selected = (self.action_selected + 1).min(last);
                    PanelAction::None
                }
                KeyCode::Enter => {
                    let recovery = self.entries[self.selected].recovery;
                    if (recovery && self.action_selected == 1)
                        || (!recovery && self.action_selected == 4)
                    {
                        return PanelAction::Close;
                    }
                    if !recovery && self.action_selected == 3 {
                        return PanelAction::Fork {
                            checkpoint_id: self.entries[self.selected].id,
                        };
                    }
                    let mode = if recovery {
                        RestoreMode::CodeOnly
                    } else {
                        match self.action_selected {
                            0 => RestoreMode::CodeAndConversation,
                            1 => RestoreMode::ConversationOnly,
                            2 => RestoreMode::CodeOnly,
                            _ => unreachable!(),
                        }
                    };
                    PanelAction::Restore {
                        checkpoint_id: self.entries[self.selected].id,
                        mode,
                    }
                }
                _ => PanelAction::None,
            },
        }
    }

    pub(crate) fn render(&self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        let chunks = panel_chunks(area);
        Paragraph::new(Line::from(vec![
            Span::styled("↶ Rewind", Style::default().fg(role::HEADING).bold()),
            Span::styled(
                "  Built-in write_file/edit_file changes only",
                Style::default().fg(palette::MUTED),
            ),
        ]))
        .render(chunks[0], buf);

        match self.mode {
            Mode::Checkpoints => {
                let items = self.entries.iter().enumerate().map(|(index, entry)| {
                    let prompt = entry.prompt.lines().next().unwrap_or("");
                    let prompt = if prompt.chars().count() > 90 {
                        format!("{}…", prompt.chars().take(90).collect::<String>())
                    } else {
                        prompt.to_string()
                    };
                    let item = ListItem::new(format!(
                        "#{:<4} {}  ({} files)",
                        entry.id, prompt, entry.file_count
                    ));
                    if let Some(style) = self.mouse_style(MouseTarget::Checkpoint(index)) {
                        item.style(style)
                    } else {
                        item
                    }
                });
                let list = List::new(items)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(role::BORDER))
                            .title(" Checkpoints "),
                    )
                    .highlight_style(
                        Style::default()
                            .fg(role::SELECTION_TEXT)
                            .bg(role::SELECTION_BG)
                            .add_modifier(Modifier::BOLD),
                    )
                    .highlight_symbol("❯ ");
                let mut state = ListState::default()
                    .with_selected((!self.entries.is_empty()).then_some(self.selected));
                ratatui::widgets::StatefulWidget::render(list, chunks[1], buf, &mut state);
                Paragraph::new(Line::from(vec![
                    Span::raw("↑↓ navigate  Enter choose restore mode  "),
                    Span::styled(
                        "Esc close",
                        self.mouse_style(MouseTarget::Close).unwrap_or_default(),
                    ),
                ]))
                .render(chunks[2], buf);
            }
            Mode::Actions => {
                let choices: &[&str] = if self.entries[self.selected].recovery {
                    &["Restore code (undo rewind)", "Cancel"]
                } else {
                    &[
                        "Restore code and conversation",
                        "Restore conversation only",
                        "Restore code only",
                        "Fork conversation from here",
                        "Cancel",
                    ]
                };
                let items = choices.iter().enumerate().map(|(index, choice)| {
                    let item = ListItem::new(*choice);
                    if let Some(style) = self.mouse_style(MouseTarget::Action(index)) {
                        item.style(style)
                    } else {
                        item
                    }
                });
                let list = List::new(items)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(role::BORDER))
                            .title(" Restore "),
                    )
                    .highlight_style(
                        Style::default()
                            .fg(role::SELECTION_TEXT)
                            .bg(role::SELECTION_BG)
                            .add_modifier(Modifier::BOLD),
                    )
                    .highlight_symbol("❯ ");
                let mut state = ListState::default().with_selected(Some(self.action_selected));
                ratatui::widgets::StatefulWidget::render(list, chunks[1], buf, &mut state);
                Paragraph::new(Line::from(vec![
                    Span::raw("↑↓ navigate  Enter confirm  "),
                    Span::styled(
                        "Esc back",
                        self.mouse_style(MouseTarget::Back).unwrap_or_default(),
                    ),
                ]))
                .render(chunks[2], buf);
            }
        }
    }
}

fn hit_horizontal_target(
    column: u16,
    start: u16,
    spans: &[(&str, Option<MouseTarget>)],
) -> Option<MouseTarget> {
    let x = column.checked_sub(start)?;
    let mut offset = 0_u16;
    for (text, target) in spans {
        let width = Line::from(*text).width().min(usize::from(u16::MAX)) as u16;
        if x >= offset && x < offset.saturating_add(width) {
            return *target;
        }
        offset = offset.saturating_add(width);
    }
    None
}

fn panel_chunks(area: Rect) -> std::rc::Rc<[Rect]> {
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(3),
            Constraint::Length(2),
        ])
        .split(area)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn checkpoint(recovery: bool) -> crate::checkpoint::Checkpoint {
        crate::checkpoint::Checkpoint {
            id: 7,
            prompt: "try another approach".to_string(),
            label: recovery.then(|| "recovery".to_string()),
            conversation_cutoff: 3,
            todos: Vec::new(),
            files: Vec::new(),
            recovery,
        }
    }

    #[test]
    fn mouse_requires_matching_release_and_scopes_wheel_to_list() {
        let area = Rect::new(0, 0, 80, 10);
        let chunks = panel_chunks(area);
        let list = Block::default().borders(Borders::ALL).inner(chunks[1]);
        let mut first = checkpoint(false);
        first.id = 1;
        let mut second = checkpoint(false);
        second.id = 2;
        let mut panel = RewindPanel::new(&[first, second]);

        assert_eq!(
            panel.handle_mouse(mouse(MouseEventKind::ScrollDown, 0, 0), area),
            PanelAction::None
        );
        assert_eq!(panel.selected, 0);
        assert_eq!(
            panel.handle_mouse(
                mouse(MouseEventKind::ScrollDown, list.x, list.y),
                area
            ),
            PanelAction::None
        );
        assert_eq!(panel.selected, 1);

        panel.handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), list.x, list.y),
            area,
        );
        assert_eq!(
            panel.handle_mouse(
                mouse(MouseEventKind::Up(MouseButton::Left), list.x, list.y + 1),
                area
            ),
            PanelAction::None
        );
        assert!(matches!(panel.mode, Mode::Checkpoints));

        panel.handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), list.x, list.y),
            area,
        );
        assert_eq!(
            panel.handle_mouse(
                mouse(MouseEventKind::Up(MouseButton::Left), list.x, list.y),
                area
            ),
            PanelAction::None
        );
        assert!(matches!(panel.mode, Mode::Actions));
    }

    #[test]
    fn mouse_restore_only_activates_on_matching_release() {
        let area = Rect::new(0, 0, 80, 10);
        let list = Block::default()
            .borders(Borders::ALL)
            .inner(panel_chunks(area)[1]);
        let mut panel = RewindPanel::new(&[checkpoint(false)]);
        assert_eq!(panel.handle_key(key(KeyCode::Enter)), PanelAction::None);

        panel.handle_mouse(
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                list.x,
                list.y + 2,
            ),
            area,
        );
        assert_eq!(
            panel.handle_mouse(
                mouse(
                    MouseEventKind::Up(MouseButton::Left),
                    list.x,
                    list.y + 1
                ),
                area
            ),
            PanelAction::None
        );
        assert!(matches!(panel.mode, Mode::Actions));

        panel.handle_mouse(
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                list.x,
                list.y + 2,
            ),
            area,
        );
        assert_eq!(
            panel.handle_mouse(
                mouse(
                    MouseEventKind::Up(MouseButton::Left),
                    list.x,
                    list.y + 2
                ),
                area
            ),
            PanelAction::Restore {
                checkpoint_id: 7,
                mode: RestoreMode::CodeOnly,
            }
        );
    }

    #[test]
    fn fork_is_available_for_prompt_checkpoints() {
        let mut panel = RewindPanel::new(&[checkpoint(false)]);
        assert_eq!(panel.handle_key(key(KeyCode::Enter)), PanelAction::None);
        for _ in 0..3 {
            assert_eq!(panel.handle_key(key(KeyCode::Down)), PanelAction::None);
        }
        assert_eq!(
            panel.handle_key(key(KeyCode::Enter)),
            PanelAction::Fork { checkpoint_id: 7 }
        );
    }

    #[test]
    fn recovery_checkpoints_do_not_offer_fork() {
        let mut panel = RewindPanel::new(&[checkpoint(true)]);
        assert_eq!(panel.handle_key(key(KeyCode::Enter)), PanelAction::None);
        assert_eq!(panel.handle_key(key(KeyCode::Down)), PanelAction::None);
        assert_eq!(panel.handle_key(key(KeyCode::Down)), PanelAction::None);
        assert_eq!(panel.handle_key(key(KeyCode::Enter)), PanelAction::Close);
    }
}
