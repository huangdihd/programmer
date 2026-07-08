use crate::app::App;
use ratatui::layout::{Constraint, Direction, Layout, Size};
use ratatui::widgets::{StatefulWidget, Wrap};
use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    style::{Color, Stylize},
    widgets::{Block, Paragraph, Widget},
};
use tui_scrollview::{ScrollView, ScrollViewState};

impl StatefulWidget for &App<'_> {
    type State = ScrollViewState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut ScrollViewState) {

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
            .wrap(Wrap { trim: true });

        let content_width = chunks[0].width.saturating_sub(1);
        let content_height = (paragraph.line_count(content_width) as u16).max(chunks[0].height);

        let mut scroll_view = ScrollView::new(Size::new(content_width, content_height));

        scroll_view.render_widget(
            paragraph,
            Rect::new(0, 0, content_width, content_height),
        );

        scroll_view.render(chunks[0], buf, state);
        self.textarea.render(chunks[1], buf);
    }
}
