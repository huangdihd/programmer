use async_openai::types::responses::Item::Message;
use async_openai::types::responses::MessageItem::{Input, Output};
use async_openai::types::responses::{InputContent, InputItem, OutputMessageContent};
use ratatui::style::Style;
use ratatui::text::{Line, Span, Text};
use ratatui_widgets::block::{Block, Padding};
use ratatui_widgets::paragraph::{Paragraph, Wrap};

use crate::ui::markdown_theme::palette;

pub struct UserMessage<'a> {
    input_item: &'a InputItem
}

impl<'a> UserMessage<'a> {
    pub(crate) fn new(input_item: &'a InputItem) -> Self {
        Self {
            input_item
        }
    }

    pub fn into_paragraph(self) -> Paragraph<'static> {
        let raw = match self.input_item {
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

        let accent = palette::BLUE;
        let text_fg = palette::TEXT;
        let bar_bg = palette::SURFACE;

        let lines: Vec<Line<'static>> = raw
            .lines()
            .enumerate()
            .map(|(i, l)| {
                let prefix = if i == 0 { "❯ " } else { "  " };
                Line::from(vec![
                    Span::styled(prefix.to_string(), Style::new().fg(accent)),
                    Span::raw(l.to_string()),
                ])
            })
            .collect();

        Paragraph::new(Text::from(lines))
            .style(Style::new().fg(text_fg).bg(bar_bg))
            .block(Block::default().padding(Padding::new(1, 1, 0, 0)))
            .wrap(Wrap { trim: false })
    }
}