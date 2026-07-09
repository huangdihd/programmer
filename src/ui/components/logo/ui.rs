use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::widgets::Widget;
use ratatui_widgets::block::Block;
use crate::ui::components::logo::logo::Logo;

impl Widget for Logo {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .title("Programmer")
            .title_alignment(Alignment::Center);
        block.render(area, buf)
    }
}