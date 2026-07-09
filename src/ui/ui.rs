use crate::app::App;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    widgets::Widget,
};
use crate::ui::components::logo::Logo;

impl Widget for &mut App<'_> {

    fn render(self, area: Rect, buf: &mut Buffer) {

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(2),
                Constraint::Length(3),
            ])
            .split(area);
        let logo = Logo::new();
        logo.render(chunks[0], buf);
        self.conversation_panel.render(chunks[1], buf);
        self.input_panel.render(chunks[2], buf);
    }
}
