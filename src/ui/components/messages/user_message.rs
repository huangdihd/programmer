use async_openai::types::responses::Item::Message;
use async_openai::types::responses::MessageItem::{Input, Output};
use async_openai::types::responses::{InputContent, InputItem, OutputMessageContent};
use ratatui::prelude::Color;
use ratatui::style::Stylize;
use ratatui_widgets::block::Block;
use ratatui_widgets::paragraph::{Paragraph, Wrap};

pub struct UserMessage<'a> {
    input_item: &'a InputItem
}

impl<'a> UserMessage<'a> {
    pub(crate) fn new(input_item: &'a InputItem) -> Self {
        Self {
            input_item
        }
    }

    pub fn into_paragraph(self) -> Paragraph<'a> {
        let text = match self.input_item {
            InputItem::Item(item) => match item {
                Message(message_item) => match message_item {
                    Input(input_message) => input_message.content
                        .iter().map(|input_content| match input_content {
                        InputContent::InputText(c) => c.text.clone(),
                        _ => "Unsupported message".to_string(),
                    }).collect::<Vec<_>>().join("\n"),
                    Output(output_message) => output_message.content
                        .iter().map(|c| match c {
                        OutputMessageContent::OutputText(t) => t.text.clone(),
                        OutputMessageContent::Refusal(r) => r.refusal.clone(),
                    }).collect::<Vec<_>>().join("\n")
                }
                _ => "[Unsupported message]\n".to_string()
            },
            _ => "[Unsupported message]\n".to_string()
        };

        let block = Block::default();
        Paragraph::new(text)
            .block(block)
            .fg(Color::White)
            .bg(Color::DarkGray)
            .wrap(Wrap { trim: true })
    }
}