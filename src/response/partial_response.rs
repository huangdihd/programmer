use async_openai::types::responses::ResponseStreamEvent::{ResponseCompleted, ResponseCreated, ResponseError, ResponseFailed, ResponseIncomplete, ResponseOutputItemAdded, ResponseOutputItemDone, ResponseOutputTextDelta};
use async_openai::types::responses::{OutputItem, OutputMessageContent, ResponseStreamEvent};
use crate::response::response_finish_reason::ResponseFinishReason;

#[derive(Debug)]
pub struct PartialResponse {
    pub items: Vec<Option<OutputItem>>,
    finish_reason: Option<ResponseFinishReason>
}

impl PartialResponse {
    pub fn new() -> Self {
        PartialResponse {
            items: vec![],
            finish_reason: None,
        }
    }

    fn set_item(&mut self, item: OutputItem, output_index: u32) {
        if self.items.len() <= output_index as usize {
            self.items.resize((output_index + 1) as usize, None);
        }
        self.items[output_index as usize] = Some(item);
    }

    pub fn get_message_items(&self) -> Vec<OutputItem> {
        self.items.iter().filter(|output_item|output_item.is_some()).map(|output_item: &Option<OutputItem>| output_item.clone().unwrap()).collect()
    }

    pub fn handle_response_stream_event(&mut self, response_stream_event: ResponseStreamEvent) {
        match response_stream_event {
            ResponseCreated(_) => {

            }
            ResponseOutputItemAdded(item_added_event) => {
                self.set_item(item_added_event.item, item_added_event.output_index);
            }
            ResponseOutputTextDelta(text_delta_event) => {
                let Some(Some(OutputItem::Message(output_message))) =
                    self.items.get_mut(text_delta_event.output_index as usize)
                else {
                    return;
                };
                let Some(OutputMessageContent::OutputText(part)) =
                    output_message.content.get_mut(text_delta_event.content_index as usize)
                else {
                    return;
                };
                part.text.push_str(&text_delta_event.delta);
            }
            ResponseOutputItemDone(item_done_event) => {
                let Some(Some(output_item)) =
                    self.items.get_mut(item_done_event.output_index as usize)
                else {
                    return;
                };
                *output_item = item_done_event.item;
            }
            ResponseCompleted(response_completed_event) => {
                self.finish_reason = Some(ResponseFinishReason::Completed(response_completed_event.response));
            }
            ResponseFailed(response_failed_event) => {
                self.finish_reason = Some(ResponseFinishReason::Failed(response_failed_event.response));
            }
            ResponseIncomplete(response_incomplete_event) => {
                self.finish_reason = Some(ResponseFinishReason::Incomplete(response_incomplete_event.response));
            }
            ResponseError(response_error_event) => {
                self.finish_reason = Some(ResponseFinishReason::ApiError {
                    code: response_error_event.code,
                    message: response_error_event.message,
                    param: response_error_event.param,
                });
            }
            _ => {}
        }
    }

    pub fn finished(&self) -> bool {
        self.finish_reason.is_some()
    }
    
}