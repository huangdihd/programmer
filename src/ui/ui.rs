use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    style::{Color, Stylize},
    widgets::{Block, BorderType, Paragraph, Widget},
};
use ratatui::widgets::Wrap;
use crate::app::App;

impl Widget for &App {
    /// Renders the user interface widgets.
    ///
    // This is where you add new widgets.
    // See the following resources:
    // - https://docs.rs/ratatui/latest/ratatui/widgets/index.html
    // - https://github.com/ratatui/ratatui/tree/master/examples
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block = Block::bordered()
            .title("programmer")
            .title_alignment(Alignment::Center)
            .border_type(BorderType::Rounded);

        let mut text = match self.response.try_lock() {
            Ok(guard) => guard.clone(),
            Err(_) => String::from("Press s to call the LLM..."),
        };

        if text == "" {
            text = "Press s to call the LLM...".to_string();
        }

        let paragraph = Paragraph::new(text)
            .block(block)
            .fg(Color::Cyan)
            .bg(Color::Black)
            .wrap(Wrap { trim: true });

        paragraph.render(area, buf);
    }
}
