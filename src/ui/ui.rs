use crate::app::App;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::widgets::Wrap;
use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    style::{Color, Stylize},
    widgets::{Block, Paragraph, Widget},
};

impl Widget for &App<'_> {
    /// Renders the user interface widgets.
    ///
    fn render(self, area: Rect, buf: &mut Buffer) {

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(2),
                Constraint::Length(5),
            ])
            .split(area);

        let block = Block::default()
            .title("programmer")
            .title_alignment(Alignment::Center);

        let paragraph = Paragraph::new(self.response.clone())
            .block(block)
            .fg(Color::Cyan)
            .bg(Color::Black)
            .wrap(Wrap { trim: true });

        paragraph.render(chunks[0], buf);
        self.textarea.render(chunks[1], buf);
    }
}
