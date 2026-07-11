use async_openai::types::responses::{FunctionCallOutput, FunctionCallOutputItemParam};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui_widgets::block::{Block, Padding};
use ratatui_widgets::paragraph::{Paragraph, Wrap};

use crate::ui::components::messages::assistant_message::EXPANDED_BG;
use crate::ui::markdown_theme::palette;

const PAD_LEFT: u16 = 2;
const PAD_RIGHT: u16 = 2;

/// Renders the result of a tool call (a `function_call_output`). Collapsed it is
/// a single line showing the first line of output; expanded it shows the full
/// output. A caret hints the line can be clicked to toggle.
pub struct ToolResultMessage<'a> {
    output: &'a FunctionCallOutputItemParam,
    expanded: bool,
}

impl<'a> ToolResultMessage<'a> {
    pub fn new(output: &'a FunctionCallOutputItemParam) -> Self {
        Self {
            output,
            expanded: false,
        }
    }

    pub fn expanded(mut self, expanded: bool) -> Self {
        self.expanded = expanded;
        self
    }

    pub fn into_paragraph(self) -> Paragraph<'static> {
        let dim = Style::new().fg(palette::MUTED).add_modifier(Modifier::DIM);

        let text = match &self.output.output {
            FunctionCallOutput::Text(text) => text.clone(),
            FunctionCallOutput::Content(_) => "[non-text output]".to_string(),
        };

        let all: Vec<&str> = text.lines().collect();
        let multiline = all.len() > 1;
        let block = Block::default().padding(Padding::new(PAD_LEFT, PAD_RIGHT, 0, 1));

        if !self.expanded {
            // Collapsed: a single (unwrapped) line — the first line of output.
            let first = all.first().copied().unwrap_or("[no output]");
            let caret = if multiline { " ▸" } else { "" };
            let line = Line::from(Span::styled(format!("⎿ {first}{caret}"), dim));
            return Paragraph::new(Text::from(line)).block(block);
        }

        // Expanded: the full output.
        let mut lines: Vec<Line<'static>> = all
            .iter()
            .enumerate()
            .map(|(index, line)| {
                let prefix = if index == 0 { "⎿ " } else { "  " };
                let suffix = if index == 0 && multiline { " ▾" } else { "" };
                Line::from(Span::styled(format!("{prefix}{line}{suffix}"), dim))
            })
            .collect();
        if lines.is_empty() {
            lines.push(Line::from(Span::styled("⎿ [no output]", dim)));
        }

        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .block(block.style(Style::new().bg(EXPANDED_BG)))
    }
}
