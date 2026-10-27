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

//! Question panel shown at the bottom of the screen when the model calls
//! the `ask_user` tool. Replaces the input panel area with a taller widget
//! showing the question, options, and/or free-text input.
//!
//! Pressing Enter sends the answer back to the blocked tool call via a
//! oneshot channel, then the input panel is restored.

mod delegation;
pub mod ui;

#[cfg(test)]
mod tests;

use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::style::{Modifier, Style};
use ratatui_textarea::TextArea;

use crate::tools::ask_user::{Question, QuestionKind};
use crate::ui::event::AnswerTx;
use crate::ui::markdown_theme::palette;

/// The answer action after handling a key.
#[derive(Debug, PartialEq)]
pub enum AnswerAction {
    /// Still waiting.
    None,
    /// Answer with this text.
    Answer(String),
    /// Switch the session model without answering the pending question.
    SelectModel(String),
}

#[allow(clippy::large_enum_variant)]
pub(crate) enum Mode {
    /// Navigating a list of preset choices.
    Choice { selected: usize },
    /// Typing a free-form answer.
    Text { textarea: TextArea<'static> },
}

pub struct QuestionPanel {
    question: Question,
    mode: Mode,
    answer_tx: Option<AnswerTx>,
    delegation: Option<delegation::Delegation>,
}

fn make_textarea() -> TextArea<'static> {
    let mut ta = TextArea::default();
    ta.set_style(Style::default().fg(palette::TEXT));
    ta.set_cursor_line_style(Style::default());
    ta.set_cursor_style(Style::default().add_modifier(Modifier::REVERSED));
    ta.set_placeholder_text("Type your answer\u{2026}");
    ta.set_placeholder_style(
        Style::default()
            .fg(palette::MUTED)
            .add_modifier(Modifier::ITALIC),
    );
    ta
}

impl QuestionPanel {
    pub fn new(question: Question, answer_tx: AnswerTx) -> Self {
        let mode = match &question.kind {
            QuestionKind::Choice { .. } => Mode::Choice { selected: 0 },
            QuestionKind::Text => Mode::Text {
                textarea: make_textarea(),
            },
        };
        QuestionPanel {
            question,
            mode,
            answer_tx: Some(answer_tx),
            delegation: None,
        }
    }

    pub(crate) fn handle_mouse(
        &mut self,
        area: ratatui::layout::Rect,
        mouse: MouseEvent,
    ) -> AnswerAction {
        if let Some(delegation) = &mut self.delegation {
            return delegation.handle_mouse(area, mouse);
        }
        let pointer_over_choice = self.mouse_option(area, mouse).is_some();
        if pointer_over_choice
            && matches!(
                mouse.kind,
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
            )
            && let Mode::Choice { selected } = &mut self.mode
            && let QuestionKind::Choice { options, .. } = &self.question.kind
        {
            if mouse.kind == MouseEventKind::ScrollUp {
                *selected = selected.saturating_sub(1);
            } else {
                *selected = (*selected + 1).min(options.len().saturating_sub(1));
            }
        }
        AnswerAction::None
    }

    pub(crate) fn interaction_animation_active(&self) -> bool {
        self.delegation
            .as_ref()
            .is_some_and(delegation::Delegation::interaction_animation_active)
    }

    pub(crate) fn clear_pointer_feedback(&mut self) {
        if let Some(delegation) = &mut self.delegation {
            delegation.clear_pointer_feedback();
        }
    }

    /// Handle a key event. Returns the answer if the user submitted one.
    pub fn handle_key(&mut self, key: KeyEvent) -> AnswerAction {
        if let Some(delegation) = &mut self.delegation {
            return delegation.handle_key(key);
        }
        match &mut self.mode {
            Mode::Choice { selected } => {
                let QuestionKind::Choice {
                    options,
                    other_index,
                } = &self.question.kind
                else {
                    unreachable!()
                };

                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        *selected = selected.saturating_sub(1);
                        AnswerAction::None
                    }
                    KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                        if *selected + 1 < options.len() {
                            *selected += 1;
                        }
                        AnswerAction::None
                    }
                    KeyCode::Esc => AnswerAction::Answer("(cancelled)".to_string()),
                    KeyCode::Enter => {
                        if *selected == *other_index {
                            self.mode = Mode::Text {
                                textarea: make_textarea(),
                            };
                            AnswerAction::None
                        } else {
                            let answer = options[*selected].clone();
                            AnswerAction::Answer(answer)
                        }
                    }
                    _ => AnswerAction::None,
                }
            }
            Mode::Text { textarea } => match key.code {
                KeyCode::Esc => AnswerAction::Answer("(cancelled)".to_string()),
                KeyCode::Enter => {
                    let text = textarea.lines().join("\n");
                    AnswerAction::Answer(text)
                }
                _ => {
                    textarea.input(ratatui_textarea::Input::from(key));
                    AnswerAction::None
                }
            },
        }
    }

    pub(crate) fn mouse_option(
        &self,
        area: ratatui::layout::Rect,
        mouse: MouseEvent,
    ) -> Option<usize> {
        if self.delegation.is_some() {
            return None;
        }
        let Mode::Choice { .. } = &self.mode else {
            return None;
        };
        let QuestionKind::Choice { options, .. } = &self.question.kind else {
            return None;
        };
        let inner = ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::TOP)
            .inner(area);
        let chunks = ratatui::layout::Layout::default()
            .direction(ratatui::layout::Direction::Vertical)
            .constraints([
                ratatui::layout::Constraint::Min(1),
                ratatui::layout::Constraint::Length(options.len() as u16),
                ratatui::layout::Constraint::Length(1),
            ])
            .split(inner);
        if mouse.column < chunks[1].x
            || mouse.column >= chunks[1].right()
            || mouse.row < chunks[1].y
            || mouse.row >= chunks[1].bottom()
        {
            return None;
        }
        Some(usize::from(mouse.row - chunks[1].y).min(options.len().saturating_sub(1)))
    }

    /// Handle a click in the panel's last rendered area. Choice rows activate
    /// immediately; text questions retain keyboard editing while owning the click.
    pub(crate) fn select_mouse_option(
        &mut self,
        area: ratatui::layout::Rect,
        mouse: MouseEvent,
    ) -> bool {
        if mouse.kind != MouseEventKind::Up(MouseButton::Left) {
            return false;
        }
        let Some(index) = self.mouse_option(area, mouse) else {
            return false;
        };
        let Mode::Choice { selected } = &mut self.mode else {
            return false;
        };
        *selected = index;
        true
    }

    /// Consume the stored answer_tx and send the answer.
    pub fn answer(&mut self, text: String) {
        if let Some(tx) = self.answer_tx.take() {
            tx.send(text);
        }
    }

    /// Append pasted text in text-input mode.
    pub fn handle_paste(&mut self, data: &str) {
        if let Some(delegation) = &mut self.delegation {
            delegation.paste(data);
            return;
        }
        if let Mode::Text { textarea } = &mut self.mode {
            let clean: String = data.chars().filter(|c| *c != '\n' && *c != '\r').collect();
            textarea.insert_str(&clean);
        }
    }
}

impl Drop for QuestionPanel {
    fn drop(&mut self) {
        if let Some(tx) = self.answer_tx.take() {
            tx.send("(cancelled)".to_string());
        }
    }
}

impl AnswerTx {
    pub fn send(self, text: String) {
        let _ = self.0.send(text);
    }
}

// TextArea doesn't implement Debug; provide a custom impl.
impl std::fmt::Debug for QuestionPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mode_str = match &self.mode {
            Mode::Choice { selected } => format!("Choice({selected})"),
            Mode::Text { .. } => "Text".to_string(),
        };
        f.debug_struct("QuestionPanel")
            .field("question", &self.question)
            .field("mode", &mode_str)
            .finish()
    }
}
