use ratatui::style::{Color, Style};
use ratatui_textarea::{Input, TextArea};
use ratatui_widgets::block::Block;
use ratatui_widgets::borders::{BorderType, Borders};

#[derive(Debug, Clone)]
pub struct InputPanel<'a> {
    pub text_area: TextArea<'a>,
}

impl InputPanel<'_> {
    pub fn new() -> Self {
        let mut text_area = TextArea::default();

        text_area.set_block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::LightBlue))
        );

        InputPanel {
            text_area
        }
    }

    pub fn get_content(&self) -> String{
        self.text_area.lines().join("\n")
    }

    pub fn input(&mut self, input: impl Into<Input>) -> bool {
        self.text_area.input(input)
    }

    pub fn clear(&mut self) -> bool {
        self.text_area.clear()
    }
}