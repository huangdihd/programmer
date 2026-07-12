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
    /// A list of choices. `other_index` is the position of the "Other…" option, if any.
    Choice {
        options: Vec<String>,
        other_index: Option<usize>,
    },
    /// Free-form text input.
    Text {
        input: String,
    },
}

pub fn tool() -> Tool {
    function_tool(
        NAME,
        "Ask the user a question and wait for their response. Use this when you \
         need clarification before proceeding, or when the user should make a \
         decision. Supports yes/no questions, multiple choice, and free-text input.",
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
            },
            "allow_other": {
                "type": "boolean",
                "description": "For 'yes_no' and 'multiple_choice': when true, adds an 'Other...' option that lets the user type a free-form response instead."
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
    #[serde(default)]
    allow_other: bool,
}

pub async fn run(arguments: &str, sender: &mpsc::UnboundedSender<Event>) -> String {
    let args: Args = match serde_json::from_str(arguments) {
        Ok(a) => a,
        Err(e) => return format!("error: invalid arguments: {e}"),
    };

    let question = match args.kind.as_str() {
        "yes_no" => {
            let mut options = vec!["Yes".to_string(), "No".to_string()];
            let other_index = if args.allow_other {
                options.push("Other…".to_string());
                Some(2usize)
            } else {
                None
            };
            Question {
                text: args.question,
                kind: QuestionKind::Choice {
                    options,
                    other_index,
                },
            }
        }
        "multiple_choice" => {
            let mut options = args.options;
            if options.is_empty() {
                return "error: 'options' must be a non-empty array for kind='multiple_choice'".to_string();
            }
            let other_index = if args.allow_other {
                let idx = options.len();
                options.push("Other…".to_string());
                Some(idx)
            } else {
                None
            };
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
            kind: QuestionKind::Text {
                input: String::new(),
            },
        },
        other => return format!("error: unknown kind '{other}'; valid kinds: yes_no, multiple_choice, text"),
    };

    let (answer_tx, answer_rx) = tokio::sync::oneshot::channel();
    let _ = sender.send(Event::App(AppEvent::QuestionPrompt {
        question,
        answer_tx: AnswerTx(answer_tx),
    }));
    answer_rx.await.unwrap_or_else(|_| "(no answer)".to_string())
}
