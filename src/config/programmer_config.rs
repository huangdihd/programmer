use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct ProgrammerConfig {
    pub model: String,
    pub base_url: String,
    pub api_key: String,
}

impl Default for ProgrammerConfig {
    fn default() -> Self {
        ProgrammerConfig {
            model: "Type your model here".to_string(),
            base_url: "Type your base_url here".to_string(),
            api_key: "Type your api_key here".to_string(),
        }
    }
}

impl ProgrammerConfig {
    fn new(model: &str, base_url: &str, api_key: &str) -> Self {
        ProgrammerConfig {
            model: model.to_string(),
            base_url: base_url.to_string(),
            api_key: api_key.to_string(),
        }
    }
}