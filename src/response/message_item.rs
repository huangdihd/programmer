use async_openai::error::OpenAIError;
use async_openai::types::responses::{InputItem, OutputItem};

#[derive(Debug)]
pub enum MessageItem {
    Input(InputItem),
    Output(OutputItem),
    OpenAIError(OpenAIError),
    Error(String),
}