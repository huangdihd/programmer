use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::prelude::Widget;
use ratatui_widgets::block::Block;

pub struct Logo{

}

impl Logo{
    pub fn new() -> Self{
        Logo {
        }
    }
}

impl Widget for Logo {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .title("Programmer")
            .title_alignment(Alignment::Center);
        block.render(area, buf)
    }
}