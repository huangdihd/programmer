use ratatui::widgets::Widget;
use crate::ui::components::input_panel::input_panel::InputPanel;

impl Widget for &InputPanel<'_> {
    fn render(self, area: ratatui::layout::Rect, buf: &mut ratatui::buffer::Buffer) {
        self.text_area.render(area, buf);
    }
}