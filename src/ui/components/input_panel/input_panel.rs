use ratatui_textarea::{Input, TextArea};

#[derive(Debug, Clone)]
pub struct InputPanel<'a> {
    pub text_area: TextArea<'a>,
}

impl InputPanel<'_> {
    pub fn new() -> Self {
        InputPanel {
            text_area: TextArea::default()
        }
    }

    pub fn get_content(&self) -> String{
        self.text_area.lines().join("\n")
    }

    pub fn input(&mut self, input: impl Into<Input>) -> bool {
        self.text_area.input(input)
    }

    pub fn clear(&mut self) -> bool {
        self.text_area.clear()
    }
}