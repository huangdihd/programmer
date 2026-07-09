use ratatui::prelude::Color;
use ratatui::style::Stylize;
use ratatui_widgets::block::Block;
use ratatui_widgets::paragraph::{Paragraph, Wrap};

pub struct PendingMessage<'a> {
    text: &'a str,
}

impl<'a> PendingMessage<'a> {
    pub fn new(text: &'a str) -> Self {
        Self { text }
    }

    pub fn into_paragraph(self) -> Paragraph<'a> {
        let block = Block::default()
            .title("Pending Message")
            .title_style(Color::Green);
        Paragraph::new(self.text)
            .block(block)
            .fg(Color::LightBlue)
            .bg(Color::DarkGray)
            .wrap(Wrap { trim: true })
    }
}