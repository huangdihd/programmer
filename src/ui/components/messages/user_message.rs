// Copyright (C) 2026 huangdihd
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

use async_openai::types::responses::Item::Message;
use async_openai::types::responses::MessageItem::{Input, Output};
use async_openai::types::responses::{InputContent, InputItem, OutputMessageContent};
use ratatui::style::Style;
use ratatui::text::{Line, Span, Text};
use ratatui_widgets::block::{Block, Padding};
use ratatui_widgets::paragraph::{Paragraph, Wrap};
use regex::Regex;
use std::sync::LazyLock;

use crate::ui::markdown_theme::palette;

static PASTED_IMAGE_PLACEHOLDER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\[Pasted image #\d+ \d+x\d+\]").expect("valid pasted-image placeholder regex")
});

pub struct UserMessage<'a> {
    input_item: &'a InputItem,
    width: u16,
}

impl<'a> UserMessage<'a> {
    pub(crate) fn new(input_item: &'a InputItem, width: u16) -> Self {
        Self { input_item, width }
    }

    pub fn into_paragraph(self) -> Paragraph<'static> {
        let accent = palette::BLUE;
        let text_fg = palette::TEXT;
        let bar_bg = palette::SURFACE;

        let lines = display_lines(self.input_item, self.width, accent);

        Paragraph::new(Text::from(lines))
            .style(Style::new().fg(text_fg).bg(bar_bg))
            .block(Block::default().padding(Padding::new(1, 1, 0, 0)))
            .wrap(Wrap { trim: false })
    }
}

fn display_lines(
    input_item: &InputItem,
    width: u16,
    accent: ratatui::style::Color,
) -> Vec<Line<'static>> {
    let InputItem::Item(Message(Input(message))) = input_item else {
        return text_lines(&display_text(input_item), accent, true);
    };

    let mut lines = Vec::new();
    let mut prompt_rendered = false;
    for content in &message.content {
        match content {
            InputContent::InputText(text) => {
                let text = strip_pasted_image_placeholders(&text.text);
                let text_lines = text_lines(&text, accent, !prompt_rendered);
                prompt_rendered |= !text_lines.is_empty();
                lines.extend(text_lines);
            }
            InputContent::InputImage(_) => {
                lines.extend(crate::ui::image_preview::content_preview_lines(
                    std::slice::from_ref(content),
                    width,
                ));
            }
            InputContent::InputFile(_) => {
                lines.extend(text_lines("📎 File attachment", accent, !prompt_rendered));
                prompt_rendered = true;
            }
        }
    }
    lines
}

fn text_lines(text: &str, accent: ratatui::style::Color, show_prompt: bool) -> Vec<Line<'static>> {
    text.lines()
        .enumerate()
        .map(|(i, line)| {
            let prefix = if i == 0 && show_prompt { "❯ " } else { "  " };
            Line::from(vec![
                Span::styled(prefix.to_string(), Style::new().fg(accent)),
                Span::raw(line.to_string()),
            ])
        })
        .collect()
}

fn display_text(input_item: &InputItem) -> String {
    match input_item {
        InputItem::Item(Message(Input(input_message))) => input_message
            .content
            .iter()
            .filter_map(|input_content| match input_content {
                InputContent::InputText(c) => Some(strip_pasted_image_placeholders(&c.text)),
                // The image itself is rendered as a colored preview below.
                InputContent::InputImage(_) => None,
                InputContent::InputFile(_) => Some("📎 File attachment".to_string()),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        InputItem::Item(Message(Output(output_message))) => output_message
            .content
            .iter()
            .map(|c| match c {
                OutputMessageContent::OutputText(t) => t.text.clone(),
                OutputMessageContent::Refusal(r) => r.refusal.clone(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        InputItem::Item(_) | InputItem::ItemReference(_) | InputItem::EasyMessage(_) => {
            "[Unsupported message]\n".to_string()
        }
    }
}

fn strip_pasted_image_placeholders(text: &str) -> String {
    PASTED_IMAGE_PLACEHOLDER
        .replace_all(text, "")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_openai::types::responses::{
        ImageDetail, InputImageContent, InputMessage, InputTextContent, Item, MessageItem,
        OutputStatus,
    };
    use base64::Engine;
    use image::{ImageBuffer, ImageFormat, Rgb};
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::widgets::Widget;
    use std::io::Cursor;

    fn png_data_url() -> String {
        let image = ImageBuffer::from_pixel(2, 2, Rgb([255u8, 0, 0]));
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, ImageFormat::Png).unwrap();
        format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes.into_inner())
        )
    }

    fn pasted_image_message(image_url: String) -> InputItem {
        InputItem::from(Item::Message(MessageItem::Input(InputMessage {
            content: vec![
                InputContent::InputImage(InputImageContent {
                    detail: ImageDetail::Auto,
                    file_id: None,
                    image_url: Some(image_url),
                }),
                InputContent::InputText(InputTextContent {
                    text: "Can you see this?".to_string(),
                }),
            ],
            role: async_openai::types::responses::InputRole::User,
            status: Some(OutputStatus::Completed),
        })))
    }

    #[test]
    fn sent_image_placeholder_is_replaced_by_the_preview() {
        let input = pasted_image_message("data:image/png;base64,AAAA".to_string());

        assert_eq!(display_text(&input), "Can you see this?");
    }

    #[test]
    fn sent_image_renders_as_colored_halfblocks() {
        let input = pasted_image_message(png_data_url());
        let area = Rect::new(0, 0, 40, 8);
        let mut buffer = Buffer::empty(area);

        UserMessage::new(&input, area.width)
            .into_paragraph()
            .render(area, &mut buffer);

        let rendered = (0..area.height)
            .flat_map(|y| (0..area.width).map(move |x| (x, y)))
            .filter_map(|position| buffer.cell(position))
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains('▀'));
        assert!(!rendered.contains("Pasted image"));
    }

    #[test]
    fn sent_image_renders_before_following_text() {
        let input = pasted_image_message(png_data_url());
        let area = Rect::new(0, 0, 40, 8);
        let mut buffer = Buffer::empty(area);

        UserMessage::new(&input, area.width)
            .into_paragraph()
            .render(area, &mut buffer);

        let image_row = (0..area.height)
            .find(|&y| (0..area.width).any(|x| buffer[(x, y)].symbol() == "▀"))
            .expect("image preview row");
        let text_row = (0..area.height)
            .find(|&y| (0..area.width).any(|x| buffer[(x, y)].symbol() == "C"))
            .expect("text row");
        assert!(image_row < text_row);
    }
}
