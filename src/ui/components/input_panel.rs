use crossterm::event::KeyEvent;
use ratatui::macros::text;
use ratatui::widgets::Widget;
use ratatui_textarea::{Input, TextArea};

#[derive(Debug, Clone)]
pub struct InputPanel<'a> {
    pub text_area: TextArea<'a>,
}

impl Widget for &InputPanel<'_> {
    fn render(self, area: ratatui::layout::Rect, buf: &mut ratatui::buffer::Buffer) {
        self.text_area.render(area, buf);
    }
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
    
    pub(crate) fn input(&mut self, input: impl Into<Input>) -> bool {
        self.text_area.input(input)
    }
    
    pub(crate) fn clear(&mut self) -> bool {
        self.text_area.clear()
    }
}