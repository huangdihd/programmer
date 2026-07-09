use ratatui::widgets::Widget;
use ratatui_textarea::TextArea;

struct InputPanel<'a> {
    pub text_area: TextArea<'a>,
}

impl Widget for InputPanel<'_> {
    fn render(self, area: ratatui::layout::Rect, buf: &mut ratatui::buffer::Buffer) {
        self.text_area.render(area, buf);
    }
}