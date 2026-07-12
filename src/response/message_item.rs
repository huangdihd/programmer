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
use async_openai::types::responses::{InputItem, OutputItem};

#[derive(Debug)]
pub enum MessageItem {
    Input(InputItem),
    Output(OutputItem),
    OpenAIError(OpenAIError),
    Error(String),
    Warning(String),
    Info(String),
    Usage(u32, u32), // (input_tokens, output_tokens)
}

impl Clone for MessageItem {
    fn clone(&self) -> Self {
        match self {
            MessageItem::Input(i) => MessageItem::Input(i.clone()),
            MessageItem::Output(o) => MessageItem::Output(o.clone()),
            MessageItem::OpenAIError(e) => {
                MessageItem::Error(format!("(cloned error) {e}"))
            }
            MessageItem::Error(s) => MessageItem::Error(s.clone()),
            MessageItem::Warning(s) => MessageItem::Warning(s.clone()),
            MessageItem::Info(s) => MessageItem::Info(s.clone()),
            MessageItem::Usage(i, o) => MessageItem::Usage(*i, *o),
        }
    }
}
