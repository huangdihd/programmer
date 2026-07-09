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

        

        self.input_panel.render(chunks[1], buf);
    }
}
