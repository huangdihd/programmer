use async_openai::types::responses::{OutputMessage, OutputMessageContent};
use ratatui::prelude::Color;
use ratatui::style::Stylize;
use ratatui_widgets::block::Block;
use ratatui_widgets::paragraph::{Paragraph, Wrap};

pub struct AssistantMessage<'a> {
    output_message: &'a OutputMessage
}

impl<'a> AssistantMessage<'a> {
    pub fn new(output_message: &'a OutputMessage) -> Self {
        AssistantMessage {
            output_message
        }
    }

    pub fn into_paragraph(self) -> Paragraph<'a> {
        let text = self.output_message.content
            .iter().map(|c| match c {
            OutputMessageContent::OutputText(t) => t.text.clone(),
            OutputMessageContent::Refusal(r) => r.refusal.clone(),
        }).collect::<Vec<_>>().join("\n");
        let block = Block::default();
        Paragraph::new(text)
            .block(block)
            .fg(Color::Cyan)
            .bg(Color::Black)
            .wrap(Wrap { trim: true })
    }
}