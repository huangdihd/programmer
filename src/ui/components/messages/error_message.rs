use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui_widgets::block::{Block, Padding};
use ratatui_widgets::paragraph::{Paragraph, Wrap};

use crate::ui::markdown_theme::palette;

const PAD_LEFT: u16 = 2;
const PAD_RIGHT: u16 = 2;

/// Renders an error (an API error or an internal message) as a distinct,
/// red-accented block so failures stand out from normal conversation.
pub struct ErrorMessage {
    message: String,
}

impl ErrorMessage {
    pub fn new(message: String) -> Self {
        Self { message }
    }

    pub fn into_paragraph(self) -> Paragraph<'static> {
        let red = palette::RED;
        let body = palette::RED_MUTED;

        let mut lines: Vec<Line<'static>> = vec![Line::from(Span::styled(
            "✕ Error",
            Style::new().fg(red).add_modifier(Modifier::BOLD),
        ))];

        for line in self.message.lines() {
            lines.push(Line::from(Span::styled(
                line.to_string(),
                Style::new().fg(body),
            )));
        }

        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .block(Block::default().padding(Padding::new(PAD_LEFT, PAD_RIGHT, 0, 1)))
    }
}
