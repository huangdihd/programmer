use async_openai::types::responses::{OutputItem, OutputMessageContent, OutputStatus};
use ratatui::prelude::Color;
use ratatui::style::Stylize;
use ratatui_widgets::block::Block;
use ratatui_widgets::paragraph::{Paragraph, Wrap};

pub struct AssistantMessage<'a> {
    output_item: &'a OutputItem
}

impl<'a> AssistantMessage<'a> {
    pub fn new(output_item: &'a OutputItem) -> Self {
        AssistantMessage {
            output_item
        }
    }

    pub fn into_paragraph(self) -> Paragraph<'a> {
        let text = match self.output_item {
            OutputItem::Message(output_message) => output_message.content
                .iter().map(|output_message_content: &OutputMessageContent| match output_message_content {
                OutputMessageContent::OutputText(t) => t.text.clone(),
                OutputMessageContent::Refusal(r) => r.refusal.clone(),
            }).collect::<Vec<_>>().join("\n"),
            OutputItem::Reasoning(reasoning_item) => match reasoning_item.status {
                Some(OutputStatus::InProgress) => "[Thinking...]\n".to_string(),
                _ => "[Thought]\n".to_string()
            }
            _ => "[Unsupported message]\n".to_string()
        };
        let block = Block::default();
        Paragraph::new(text)
            .block(block)
            .fg(Color::White)
            .wrap(Wrap { trim: true })
    }
}