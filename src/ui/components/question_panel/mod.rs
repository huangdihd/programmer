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
use ratatui::style::{Color, Modifier, Style};
use ratatui_textarea::TextArea;

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
}

fn make_textarea() -> TextArea<'static> {
    let mut ta = TextArea::default();
    ta.set_style(Style::default().fg(Color::White));
    ta.set_cursor_line_style(Style::default());
    ta.set_cursor_style(Style::default().add_modifier(Modifier::REVERSED));
    ta.set_placeholder_text("Type your answer\u{2026}");
    ta.set_placeholder_style(
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC),
    );
    ta
}

impl QuestionPanel {
    pub fn new(question: Question, answer_tx: AnswerTx) -> Self {
        let mode = match &question.kind {
            QuestionKind::Choice { .. } => Mode::Choice { selected: 0 },
            QuestionKind::Text { .. } => Mode::Text {
                textarea: make_textarea(),
            },
        };
        QuestionPanel {
            question,
            mode,
            answer_tx: Some(answer_tx),
        }
    }

    /// The question text the model asked.
    pub fn question_text(&self) -> &str {
        &self.question.text
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

    /// Consume the stored answer_tx and send the answer.
    pub fn answer(&mut self, text: String) {
        if let Some(tx) = self.answer_tx.take() {
            tx.send(text);
        }
    }

    /// Append pasted text in text-input mode.
    pub fn handle_paste(&mut self, data: &str) {
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
