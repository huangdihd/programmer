use crate::app::App;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    widgets::Widget,
};

impl Widget for &mut App<'_> {

    fn render(self, area: Rect, buf: &mut Buffer) {

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(2),
                Constraint::Length(5),
            ])
            .split(area);

        self.input_panel.render(chunks[1], buf);
        self.conversation_panel.render(chunks[0], buf);
    }
}
