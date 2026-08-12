// Copyright (C) 2026 huangdihd
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Widget};

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

#[derive(Debug)]
pub struct RewindPanel {
    entries: Vec<Entry>,
    selected: usize,
    action_selected: usize,
    mode: Mode,
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
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
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
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),
                Constraint::Min(3),
                Constraint::Length(2),
            ])
            .split(area);
        Paragraph::new(Line::from(vec![
            Span::styled("↶ Rewind", Style::default().fg(Color::Cyan).bold()),
            Span::styled(
                "  Built-in write_file/edit_file changes only",
                Style::default().fg(Color::DarkGray),
            ),
        ]))
        .render(chunks[0], buf);

        match self.mode {
            Mode::Checkpoints => {
                let items = self.entries.iter().map(|entry| {
                    let prompt = entry.prompt.lines().next().unwrap_or("");
                    let prompt = if prompt.chars().count() > 90 {
                        format!("{}…", prompt.chars().take(90).collect::<String>())
                    } else {
                        prompt.to_string()
                    };
                    ListItem::new(format!(
                        "#{:<4} {}  ({} files)",
                        entry.id, prompt, entry.file_count
                    ))
                });
                let list = List::new(items)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(" Checkpoints "),
                    )
                    .highlight_style(
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    )
                    .highlight_symbol("❯ ");
                let mut state = ListState::default()
                    .with_selected((!self.entries.is_empty()).then_some(self.selected));
                ratatui::widgets::StatefulWidget::render(list, chunks[1], buf, &mut state);
                Paragraph::new("↑↓ navigate  Enter choose restore mode  Esc close")
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
                let items = choices.iter().map(|choice| ListItem::new(*choice));
                let list = List::new(items)
                    .block(Block::default().borders(Borders::ALL).title(" Restore "))
                    .highlight_style(
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    )
                    .highlight_symbol("❯ ");
                let mut state = ListState::default().with_selected(Some(self.action_selected));
                ratatui::widgets::StatefulWidget::render(list, chunks[1], buf, &mut state);
                Paragraph::new("↑↓ navigate  Enter confirm  Esc back").render(chunks[2], buf);
            }
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
