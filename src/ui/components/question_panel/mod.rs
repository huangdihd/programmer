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

pub mod ui;

use crossterm::event::{KeyCode, KeyEvent};

use crate::tools::ask_user::{Question, QuestionKind};
use crate::ui::event::AnswerTx;

/// The answer action after handling a key.
#[derive(Debug, PartialEq)]
pub enum AnswerAction {
    /// Still waiting.
    None,
    /// Answer with this text.
    Answer(String),
}

#[derive(Debug)]
pub(crate) enum Mode {
    /// Navigating a list of preset choices.
    Choice { selected: usize },
    /// Typing a free-form answer.
    Text { input: String },
}

#[derive(Debug)]
pub struct QuestionPanel {
    question: Question,
    mode: Mode,
    answer_tx: Option<AnswerTx>,
}

impl QuestionPanel {
    pub fn new(question: Question, answer_tx: AnswerTx) -> Self {
        let mode = match &question.kind {
            QuestionKind::Choice { .. } => Mode::Choice { selected: 0 },
            QuestionKind::Text { .. } => Mode::Text {
                input: String::new(),
            },
        };
        QuestionPanel {
            question,
            mode,
            answer_tx: Some(answer_tx),
        }
    }

    /// Handle a key event. Returns the answer if the user submitted one.
    pub fn handle_key(&mut self, key: KeyEvent) -> AnswerAction {
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
                    KeyCode::Enter => {
                        if Some(*selected) == *other_index {
                            // Switch to text input mode for "Other…".
                            self.mode = Mode::Text {
                                input: String::new(),
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
            Mode::Text { input } => match key.code {
                KeyCode::Esc => {
                    // Go back to choice mode if we're in an "Other…" text input.
                    if let QuestionKind::Choice { .. } = &self.question.kind {
                        self.mode = Mode::Choice { selected: 0 };
                    }
                    AnswerAction::None
                }
                KeyCode::Enter => {
                    AnswerAction::Answer(input.clone())
                }
                KeyCode::Backspace => {
                    input.pop();
                    AnswerAction::None
                }
                KeyCode::Char(c) => {
                    input.push(c);
                    AnswerAction::None
                }
                _ => AnswerAction::None,
            },
        }
    }

    /// Consume the stored answer_tx and send the answer.
    pub fn answer(&mut self, text: String) {
        if let Some(tx) = self.answer_tx.take() {
            tx.send(text);
        }
    }

    /// Append pasted text in text-input mode.
    pub fn handle_paste(&mut self, data: &str) {
        if let Mode::Text { input } = &mut self.mode {
            let clean: String = data.chars().filter(|c| *c != '\n' && *c != '\r').collect();
            input.push_str(&clean);
        }
    }
}

impl Drop for QuestionPanel {
    fn drop(&mut self) {
        // If the panel is dropped without answering (shouldn't normally happen),
        // at least unblock the tool call.
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
