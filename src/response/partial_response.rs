use async_openai::types::responses::OutputItem;

struct PartialResponse {
    pub items: Vec<OutputItem>
}

impl PartialResponse {
    pub fn new() -> Self {
        PartialResponse {
            items: vec![]
        }
    }

    pub fn add_item(&mut self, item: OutputItem) {
        self.items.push(item);
    }

    
}