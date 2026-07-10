use crate::ui::components::conversation_panel::conversation_panel::ConversationPanel;
use crate::ui::components::messages::assistant_message::AssistantMessage;
use crate::ui::components::messages::user_message::UserMessage;
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use ratatui::widgets::{StatefulWidget, Widget};
use ratatui_widgets::paragraph::Paragraph;
use tui_scrollview::ScrollView;
use crate::response::message_item::MessageItem;
use crate::ui::components::messages::pending_message::PendingMessage;
use crate::ui::components::messages::welcome_message::WelcomeMessage;

impl Widget for &mut ConversationPanel {

    fn render(self, area: Rect, buf: &mut Buffer) {
        let content_width = area.width.saturating_sub(1);
        let welcome_message = WelcomeMessage::default();
        let mut content_height: u16 = welcome_message.line_count(content_width);

        let receiving_items = self
            .receiving_response
            .as_ref()
            .map(|receiving_response| receiving_response.get_message_items())
            .unwrap_or_default();

        let mut paragraphs = vec![];

        for item in &self.items {
            let paragraph = match item {
                MessageItem::Input(input_item) => UserMessage::new(input_item).into_paragraph(),
                MessageItem::Output(output_item) => AssistantMessage::new(output_item).into_paragraph(),
                _ => Paragraph::new("Error"),
            };
            let height = paragraph.line_count(content_width) as u16;
            paragraphs.push((paragraph, content_height, height));
            content_height = content_height.saturating_add(height);
        }

        for output_item in &receiving_items {
            let paragraph = AssistantMessage::new(output_item).into_paragraph();
            let height = paragraph.line_count(content_width) as u16;
            paragraphs.push((paragraph, content_height, height));
            content_height = content_height.saturating_add(height);
        }

        if let Some(text) = &self.pending_message {
            let paragraph = PendingMessage::new(text).into_paragraph();
            let height = paragraph.line_count(content_width) as u16;
            paragraphs.push((paragraph, content_height, height));
            content_height = content_height.saturating_add(height);
        }

        content_height = content_height.max(area.height);

        let mut scroll_view = ScrollView::new(Size::new(content_width, content_height));
        scroll_view.render_widget(
            &welcome_message,
            Rect::new(0, 0, content_width, welcome_message.line_count(content_width)),
        );
        for (paragraph, y, height) in paragraphs {
            scroll_view.render_widget(paragraph, Rect::new(0, y, content_width, height));
        }
        scroll_view.render(area, buf, &mut self.scroll_view_state);
    }
}