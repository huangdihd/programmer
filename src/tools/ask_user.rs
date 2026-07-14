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

use async_openai::types::responses::Tool;
use serde::Deserialize;
use serde_json::json;

use super::function_tool;
use crate::ui::event::{AnswerTx, AppEvent, Event};
use tokio::sync::mpsc;

pub const NAME: &str = "ask_user";

/// A question to present to the user via the TUI.
#[derive(Debug, Clone)]
pub struct Question {
    pub text: String,
    pub kind: QuestionKind,
}

#[derive(Debug, Clone)]
pub enum QuestionKind {
    /// A list of choices with an "Other…" option at `other_index` that
    /// switches to free-form text input.
    Choice {
        options: Vec<String>,
        other_index: usize,
    },
    /// Free-form text input.
    Text,
}

pub fn tool() -> Tool {
    function_tool(
        NAME,
        "Ask the user a question and wait for their response. Use this when you \
         need clarification before proceeding, or when the user should make a \
         decision. Supports yes/no questions, multiple choice, and free-text input. \
         Every choice list includes an 'Other…' option for free-form answers.",
        json!({
            "question": {
                "type": "string",
                "description": "The question to present to the user."
            },
            "kind": {
                "type": "string",
                "enum": ["yes_no", "multiple_choice", "text"],
                "description": "The type of question: 'yes_no' for a binary choice, 'multiple_choice' for selecting from a list of options, 'text' for free-form text input."
            },
            "options": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Required for 'multiple_choice': the list of options to present."
            }
        }),
        &["question", "kind"],
    )
}

#[derive(Deserialize)]
struct Args {
    question: String,
    kind: String,
    #[serde(default)]
    options: Vec<String>,
}

pub async fn run(arguments: &str, sender: &mpsc::UnboundedSender<Event>) -> Result<String, String> {
    let args: Args = match serde_json::from_str(arguments) {
        Ok(a) => a,
        Err(e) => return Err(format!("error: invalid arguments: {e}")),
    };

    let question = match args.kind.as_str() {
        "yes_no" => Question {
            text: args.question,
            kind: QuestionKind::Choice {
                options: vec![
                    "Yes".to_string(),
                    "No".to_string(),
                    "Other\u{2026}".to_string(),
                ],
                other_index: 2,
            },
        },
        "multiple_choice" => {
            let mut options = args.options;
            if options.is_empty() {
                return Err(
                    "error: 'options' must be a non-empty array for kind='multiple_choice'"
                        .to_string(),
                );
            }
            let other_index = options.len();
            options.push("Other\u{2026}".to_string());
            Question {
                text: args.question,
                kind: QuestionKind::Choice {
                    options,
                    other_index,
                },
            }
        }
        "text" => Question {
            text: args.question,
            kind: QuestionKind::Text,
        },
        other => {
            return Err(format!(
                "error: unknown kind '{other}'; valid kinds: yes_no, multiple_choice, text"
            ))
        }
    };

    let (answer_tx, answer_rx) = tokio::sync::oneshot::channel();
    let _ = sender.send(Event::App(AppEvent::QuestionPrompt {
        question,
        answer_tx: AnswerTx(answer_tx),
    }));
    Ok(answer_rx.await.unwrap_or_else(|_| "(no answer)".to_string()))
}
