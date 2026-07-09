use async_openai::types::responses::{InputContent, InputMessage};
use ratatui::prelude::Color;
use ratatui::style::Stylize;
use ratatui_widgets::block::Block;
use ratatui_widgets::paragraph::{Paragraph, Wrap};

pub struct UserMessage<'a> {
    input_message: &'a InputMessage
}

impl<'a> UserMessage<'a> {
    pub(crate) fn new(input_message: &'a InputMessage) -> Self {
        Self {
            input_message
        }
    }

    pub fn into_paragraph(self) -> Paragraph<'a> {
        let text = self.input_message.content
            .iter().map(|input_content| match input_content {
            InputContent::InputText(c) => c.text.clone(),
            _ => "Unsupported message".to_string(),
        }).collect::<Vec<_>>().join("\n");
        let block = Block::default();
        Paragraph::new(text)
            .block(block)
            .fg(Color::White)
            .bg(Color::DarkGray)
            .wrap(Wrap { trim: true })
    }
}