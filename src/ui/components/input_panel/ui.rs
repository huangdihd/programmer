use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};
use crate::ui::components::input_panel::input_panel::InputPanel;

const ACCENT: Color = Color::LightBlue;

impl Widget for &InputPanel<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .title(" Input ")
            .title_style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))
            .borders(Borders::TOP | Borders::BOTTOM)
            .border_style(Style::default().fg(ACCENT));

        let inner = block.inner(area);
        block.render(area, buf);

        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(2), Constraint::Min(1)])
            .split(inner);

        Paragraph::new("❯ ")
            .style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))
            .render(chunks[0], buf);

        self.text_area.render(chunks[1], buf);
    }
}