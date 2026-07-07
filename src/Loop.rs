use std::collections::VecDeque;
use async_openai::types::responses::{Conversation, MessageContent};

struct Loop {
    conversation_history: Conversation,
    input_queue: VecDeque<MessageContent>
}

impl Loop {
    fn new() {

    }
}