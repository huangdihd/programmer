use async_openai::types::responses::{InputContent, MessageItem, OutputMessageContent};
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use ratatui::prelude::{Color, StatefulWidget};
use ratatui::style::Stylize;
use ratatui_widgets::block::Block;
use ratatui_widgets::paragraph::{Paragraph, Wrap};
use tui_scrollview::{ScrollView, ScrollViewState};

pub struct ResponsePanel {
    pub messages: Vec<MessageItem>
}

impl StatefulWidget for &ResponsePanel {

    type State = ScrollViewState;
    fn render(self, area: Rect, buf: &mut Buffer, state: &mut ScrollViewState) {


        let content_width = area.width.saturating_sub(1);
        let mut content_height: u16 = 0;

        let mut paragraphs = vec![];

        for message in &self.messages {
            let block = Block::default();
            let text = match &message {
                MessageItem::Input(input_message) => input_message.content
                    .iter().map(|input_content| match input_content {
                    InputContent::InputText(c) => c.text.clone(),
                    _ => "Unsupported message".to_string(),
                }).collect::<Vec<_>>().join("\n"),
                MessageItem::Output(output_item) => output_item.content
                    .iter().map(|c| match c {
                    OutputMessageContent::OutputText(t) => t.text.clone(),
                    OutputMessageContent::Refusal(r) => r.refusal.clone(),
                }).collect::<Vec<_>>().join("\n"),
            };
            let paragraph = Paragraph::new(text)
                .block(block)
                .fg(match &message {
                    MessageItem::Input(_) => Color::default(),
                    MessageItem::Output(_) => Color::Cyan
                })
                .bg(match &message {
                    MessageItem::Input(_) => Color::Gray,
                    MessageItem::Output(_) => Color::Black
                })
                .wrap(Wrap { trim: true });
            let height = paragraph.line_count(content_width) as u16;
            paragraphs.push((paragraph, content_height, height));
            content_height.saturating_add(height);
        }

        content_height = content_height.max(area.height);

        let mut scroll_view = ScrollView::new(Size::new(content_width, content_height));

        for (paragraph, y, height) in paragraphs {
            scroll_view.render_widget(
                paragraph,
                Rect::new(0, y, content_width, height),
            );
        }

        scroll_view.render(area, buf, state);
    }
}

impl ResponsePanel {
    pub fn new() -> Self {
        ResponsePanel {
            messages: vec![]
        }
    }

    pub fn add_message(&mut self, message: MessageItem) {
        self.messages.push(message);
    }
}