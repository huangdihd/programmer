use crate::ui::markdown_theme::AppTheme;
use async_openai::types::responses::{OutputItem, OutputMessageContent};
use ratatui::prelude::Color;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui_markdown::markdown::MarkdownRenderer;
use ratatui_widgets::block::{Block, Padding};
use ratatui_widgets::paragraph::Paragraph;

const PAD_LEFT: u16 = 2;
const PAD_RIGHT: u16 = 2;

pub struct AssistantMessage<'a> {
    output_item: &'a OutputItem,
    width: u16,
    /// Whether this item is still being streamed (not yet finalized). Drives the
    /// reasoning label between "Thinking..." and "Thought".
    in_progress: bool,
}

impl<'a> AssistantMessage<'a> {
    pub fn new(output_item: &'a OutputItem, width: u16) -> Self {
        AssistantMessage {
            output_item,
            width,
            in_progress: false,
        }
    }

    pub fn in_progress(mut self, in_progress: bool) -> Self {
        self.in_progress = in_progress;
        self
    }

    pub fn into_paragraph(self) -> Paragraph<'static> {
        let muted = Style::new()
            .fg(Color::Rgb(0x80, 0x88, 0x90))
            .add_modifier(Modifier::DIM | Modifier::ITALIC);

        let text = match self.output_item {
            OutputItem::Message(output_message) => {
                let md = output_message.content
                    .iter()
                    .map(|c: &OutputMessageContent| match c {
                        OutputMessageContent::OutputText(t) => t.text.clone(),
                        OutputMessageContent::Refusal(r) => r.refusal.clone(),
                    })
                    .collect::<Vec<_>>()
                    .join("\n");

                let render_width = self.width
                    .saturating_sub(PAD_LEFT + PAD_RIGHT)
                    .min(100);
                let renderer = MarkdownRenderer::new(render_width as usize);
                let blocks = renderer.parse(&md);
                Text::from(renderer.render(&blocks, &AppTheme))
            }
            OutputItem::Reasoning(_) => {
                let label = if self.in_progress {
                    "✻ Thinking..."
                } else {
                    "✻ Thought"
                };
                Text::from(Line::from(Span::styled(label, muted)))
            }
            _ => {
                let ty = serde_json::to_value(self.output_item)
                    .ok()
                    .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(String::from))
                    .unwrap_or_else(|| "unknown".to_string());
                Text::from(Line::from(Span::styled(
                    format!("[Unsupported message: {ty}]"),
                    muted,
                )))
            },
        };

        Paragraph::new(text)
            .block(Block::default().padding(Padding::new(PAD_LEFT, PAD_RIGHT, 0, 1)))
    }
}