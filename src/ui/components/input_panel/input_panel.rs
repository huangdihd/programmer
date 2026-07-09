use ratatui::style::{Color, Modifier, Style};
use ratatui_textarea::{Input, TextArea};

#[derive(Debug, Clone)]
pub struct InputPanel<'a> {
    pub text_area: TextArea<'a>,
}

impl InputPanel<'_> {
    pub fn new() -> Self {
        let mut text_area = TextArea::default();

        text_area.set_style(Style::default().fg(Color::White));
        text_area.set_cursor_line_style(Style::default());
        text_area.set_cursor_style(Style::default().add_modifier(Modifier::REVERSED));
        text_area.set_placeholder_text("Talk with the programmer…");
        text_area.set_placeholder_style(
            Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
        );

        InputPanel { text_area }
    }

    pub fn get_content(&self) -> String {
        self.text_area.lines().join("\n")
    }

    pub fn input(&mut self, input: impl Into<Input>) -> bool {
        self.text_area.input(input)
    }

    pub fn clear(&mut self) -> bool {
        self.text_area.clear()
    }
}