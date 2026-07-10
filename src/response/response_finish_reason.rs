use async_openai::error::OpenAIError;
use async_openai::types::responses::{Response};

#[derive(Debug)]
pub enum ResponseFinishReason {
    Completed(Response),
    Failed(Response),
    Incomplete(Response),
    ApiError {
        code: Option<String>,
        message: String,
        param: Option<String>,
    },
    StreamError(OpenAIError),
}