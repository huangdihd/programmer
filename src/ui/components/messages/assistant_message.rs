use async_openai::types::responses::OutputItem;
use ratatui::prelude::Color;
use ratatui::style::Style;
use ratatui_widgets::block::{Block, Padding};
use ratatui_widgets::paragraph::Paragraph;

use crate::ui::components::messages::assistant::reasoning::ReasoningMessage;
use crate::ui::components::messages::assistant::text::TextMessage;
use crate::ui::components::messages::assistant::tool_call::ToolCallMessage;
use crate::ui::components::messages::assistant::unsupported::UnsupportedMessage;
use crate::ui::markdown_theme::palette;

const PAD_LEFT: u16 = 2;
const PAD_RIGHT: u16 = 2;
/// Gray panel behind expanded reasoning/tool details.
pub(crate) const EXPANDED_BG: Color = palette::SURFACE;

/// Renders one assistant output item. Each item kind has its own renderer (see
/// the `assistant` submodule); this type just dispatches on the variant and
/// wraps the result in the shared padded block.
pub struct AssistantMessage<'a> {
    output_item: &'a OutputItem,
    width: u16,
    /// Whether this item is still being streamed (not yet finalized). Drives the
    /// reasoning label between "Thinking..." and "Thought".
    in_progress: bool,
    /// Whether the user has expanded this item to see its full detail.
    expanded: bool,
}

impl<'a> AssistantMessage<'a> {
    pub fn new(output_item: &'a OutputItem, width: u16) -> Self {
        AssistantMessage {
            output_item,
            width,
            in_progress: false,
            expanded: false,
        }
    }

    pub fn in_progress(mut self, in_progress: bool) -> Self {
        self.in_progress = in_progress;
        self
    }

    pub fn expanded(mut self, expanded: bool) -> Self {
        self.expanded = expanded;
        self
    }

    pub fn into_paragraph(self) -> Paragraph<'static> {
        let text = match self.output_item {
            OutputItem::Message(message) => TextMessage::new(message, self.width).into_text(),
            OutputItem::Reasoning(item) => {
                ReasoningMessage::new(self.in_progress, item)
                    .expanded(self.expanded)
                    .into_text()
            }
            OutputItem::FunctionCall(call) => {
                ToolCallMessage::new(call).expanded(self.expanded).into_text()
            }
            other => UnsupportedMessage::new(other).into_text(),
        };

        let foldable = matches!(
            self.output_item,
            OutputItem::Reasoning(_) | OutputItem::FunctionCall(_)
        );
        let mut block = Block::default().padding(Padding::new(PAD_LEFT, PAD_RIGHT, 0, 1));
        if self.expanded && foldable {
            block = block.style(Style::new().bg(EXPANDED_BG));
        }

        Paragraph::new(text).block(block)
    }
}
