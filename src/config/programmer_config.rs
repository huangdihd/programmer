use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct ProgrammerConfig {
    pub model: String,
    pub base_url: String,
    pub api_key: String,
}

impl ProgrammerConfig {
    fn new(model: &str, base_url: &str, api_key: &str) -> ProgrammerConfig {
        ProgrammerConfig {
            model: model.to_string(),
            base_url: base_url.to_string(),
            api_key: api_key.to_string(),
        }
    }
}