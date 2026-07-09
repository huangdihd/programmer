use async_openai::types::responses::{InputContent, InputItem, InputParam, Item, MessageItem, OutputMessageContent};
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use ratatui::prelude::{Color, StatefulWidget};
use ratatui::style::Stylize;
use ratatui::widgets::Widget;
use ratatui_widgets::block::Block;
use ratatui_widgets::paragraph::{Paragraph, Wrap};
use tui_scrollview::{ScrollView, ScrollViewState};

#[derive(Debug)]
pub struct ConversationPanel {
    pub messages: Vec<MessageItem>,
    scroll_view_state: ScrollViewState
}

impl Widget for &mut ConversationPanel {

    fn render(self, area: Rect, buf: &mut Buffer) {

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
                    MessageItem::Input(_) => Color::White,
                    MessageItem::Output(_) => Color::Cyan
                })
                .bg(match &message {
                    MessageItem::Input(_) => Color::DarkGray,
                    MessageItem::Output(_) => Color::Black
                })
                .wrap(Wrap { trim: true });
            let height = paragraph.line_count(content_width) as u16;
            paragraphs.push((paragraph, content_height, height));
            content_height = content_height.saturating_add(height);
        }

        content_height = content_height.max(area.height);

        let mut scroll_view = ScrollView::new(Size::new(content_width, content_height));

        for (paragraph, y, height) in paragraphs {
            scroll_view.render_widget(
                paragraph,
                Rect::new(0, y, content_width, height),
            );
        }
        let mut scroll_view_state = self.scroll_view_state;
        scroll_view.render(area, buf, &mut scroll_view_state);
        self.scroll_view_state = scroll_view_state;
    }
}

impl ConversationPanel {
    pub fn new() -> Self {
        ConversationPanel {
            messages: vec![],
            scroll_view_state: ScrollViewState::new()
        }
    }

    pub fn add_message(&mut self, message: MessageItem) {
        self.messages.push(message);
        self.scroll_view_state.scroll_to_bottom();
    }

    pub fn get_last_message(&self) -> Option<&MessageItem> {
        self.messages.last()
    }

    pub fn get_last_message_mut(&mut self) -> Option<&mut MessageItem> {
        let at_bottom = self.is_at_bottom();
        let res = self.messages.last_mut();
        if at_bottom {
            self.scroll_view_state.scroll_to_bottom();
        }
        res
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll_view_state.scroll_to_bottom();
    }

    pub fn is_at_bottom(&self) -> bool {
        self.scroll_view_state.is_at_bottom()
    }

    pub fn scroll_up(&mut self) {
        self.scroll_view_state.scroll_up();
    }

    pub fn scroll_down(&mut self) {
        self.scroll_view_state.scroll_down();
    }

    pub fn get_input_param(&self) -> InputParam{
        InputParam::Items(self.messages.iter().map(|message_item: &MessageItem| InputItem::Item(Item::Message(message_item.clone()))).collect())
    }
}