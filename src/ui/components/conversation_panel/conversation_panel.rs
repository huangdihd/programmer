use async_openai::types::responses::{InputItem, InputParam, Item, MessageItem};
use tui_scrollview::ScrollViewState;

#[derive(Debug)]
pub struct ConversationPanel {
    pub messages: Vec<MessageItem>,
    pub(crate)scroll_view_state: ScrollViewState
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