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

//! Building the streaming API request from a [`Conversation`]. Shared by the
//! TUI's `spawn_stream` and the headless runner so both produce byte-identical
//! requests for the same history and system context.

use crate::conversation::Conversation;
use async_openai::types::responses::{CreateResponse, Tool};

/// The pieces of system context that shape the developer message at the head of
/// a request, beyond the conversation history itself.
pub(crate) struct SystemContext<'a> {
    /// The active `provider/model`, named in the system prompt banner.
    pub current_model: &'a str,
    /// Combined instructions from active skills, if any.
    pub skill_prompt: Option<&'a str>,
    /// Plan-mode instructions, computed by the TUI; the runner passes `None`.
    pub plan_prompt: Option<&'a str>,
    /// Git commit co-author trailer to request, if configured.
    pub coauthor: Option<&'a str>,
}

/// Assemble a streaming [`CreateResponse`] for `conversation` under `ctx`,
/// targeting `model_name` with the given `tools`.
pub(crate) fn build_request(
    conversation: &Conversation,
    ctx: &SystemContext<'_>,
    model_name: String,
    tools: Vec<Tool>,
) -> CreateResponse {
    CreateResponse {
        stream: Some(true),
        input: conversation.to_input_param(
            ctx.current_model,
            ctx.skill_prompt,
            ctx.plan_prompt,
            ctx.coauthor,
        ),
        model: Some(model_name),
        tools: Some(tools),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_openai::types::responses::{
        InputContent, InputItem, InputMessage, InputParam, InputRole, Item,
        MessageItem as ApiMessageItem, OutputStatus,
    };

    #[test]
    fn build_request_is_streaming_and_carries_history_and_tools() {
        let mut conv = Conversation::new();
        conv.add_input_message(ApiMessageItem::Input(InputMessage {
            content: vec![InputContent::InputText("hello".into())],
            role: InputRole::User,
            status: Some(OutputStatus::Completed),
        }));

        let ctx = SystemContext {
            current_model: "prov/model-x",
            skill_prompt: Some("SKILL-PROMPT-MARKER"),
            plan_prompt: None,
            coauthor: Some("Ada <ada@example.com>"),
        };
        let req = build_request(&conv, &ctx, "model-x".to_string(), {
            use crate::tools::provider::ToolProvider;
            crate::tools::provider::LocalToolProvider.tools()
        });

        assert_eq!(req.stream, Some(true));
        assert_eq!(req.model.as_deref(), Some("model-x"));
        assert!(req.tools.as_ref().is_some_and(|t| !t.is_empty()), "tool list present");

        let InputParam::Items(items) = req.input else {
            panic!("expected an item list");
        };
        // First item is the developer system message carrying the model banner,
        // the skill prompt, and the coauthor trailer.
        let InputItem::Item(Item::Message(ApiMessageItem::Input(dev))) = &items[0] else {
            panic!("first item should be the developer message");
        };
        assert_eq!(dev.role, InputRole::Developer);
        let dev_text = match &dev.content[0] {
            InputContent::InputText(t) => t.text.clone(),
            _ => panic!("developer message should be text"),
        };
        assert!(dev_text.contains("prov/model-x"), "model banner");
        assert!(dev_text.contains("SKILL-PROMPT-MARKER"), "skill prompt appended");
        assert!(dev_text.contains("Ada <ada@example.com>"), "coauthor trailer");
        // The user message follows.
        assert!(matches!(
            &items[1],
            InputItem::Item(Item::Message(ApiMessageItem::Input(m)))
                if m.role == InputRole::User
        ));
    }
}
