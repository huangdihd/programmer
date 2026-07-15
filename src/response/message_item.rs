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

use async_openai::error::OpenAIError;
use async_openai::types::responses::{FunctionCallOutputItemParam, InputItem, OutputItem};

#[derive(Debug)]
pub enum MessageItem {
    Input(InputItem),
    Output(OutputItem),
    /// A tool result with a pre-computed failure flag — set once in
    /// `add_tool_output` so renderers and the classifier never parse text.
    ToolOutput {
        output: FunctionCallOutputItemParam,
        failed: bool,
        /// Human-readable label explaining why this tool call was approved or
        /// denied (e.g. "approved by Auto mode", "denied in Manual mode by user").
        approval_label: Option<String>,
    },
    OpenAIError(OpenAIError),
    Error(String),
    Warning(String),
    Info(String),
    Meta { label: String, text: String },
    Usage(u32, u32), // (input_tokens, output_tokens)
}

impl Clone for MessageItem {
    fn clone(&self) -> Self {
        match self {
            MessageItem::Input(i) => MessageItem::Input(i.clone()),
            MessageItem::Output(o) => MessageItem::Output(o.clone()),
            MessageItem::ToolOutput { output, failed, approval_label } => MessageItem::ToolOutput {
                output: output.clone(),
                failed: *failed,
                approval_label: approval_label.clone(),
            },
            MessageItem::OpenAIError(e) => {
                MessageItem::Error(format!("(cloned error) {e}"))
            }
            MessageItem::Error(s) => MessageItem::Error(s.clone()),
            MessageItem::Warning(s) => MessageItem::Warning(s.clone()),
            MessageItem::Info(s) => MessageItem::Info(s.clone()),
            MessageItem::Meta { label, text } => MessageItem::Meta {
                label: label.clone(),
                text: text.clone(),
            },
            MessageItem::Usage(i, o) => MessageItem::Usage(*i, *o),
        }
    }
}
