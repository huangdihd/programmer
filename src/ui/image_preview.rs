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

use async_openai::types::responses::{FunctionCallOutput, InputContent};
use base64::Engine;
use image::{DynamicImage, GenericImageView, Rgba, imageops::FilterType};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use crate::ui::markdown_theme::palette;

const MAX_PREVIEW_COLUMNS: u32 = 64;
const MAX_PREVIEW_ROWS: u32 = 18;
const PREVIEW_INDENT: &str = "      ";

pub(crate) fn estimated_rows(content: &[InputContent]) -> u16 {
    let image_count = content
        .iter()
        .filter(|part| matches!(part, InputContent::InputImage(_)))
        .count();
    u16::try_from(image_count)
        .unwrap_or(u16::MAX)
        .saturating_mul(MAX_PREVIEW_ROWS as u16 + 1)
}

pub(crate) fn output_text(output: &FunctionCallOutput) -> String {
    match output {
        FunctionCallOutput::Text(text) => text.clone(),
        FunctionCallOutput::Content(content) => {
            let text = content
                .iter()
                .filter_map(|part| match part {
                    InputContent::InputText(text) => Some(text.text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            if text.is_empty() {
                "[image output]".to_string()
            } else {
                text
            }
        }
    }
}

pub(crate) fn preview_lines(
    output: &FunctionCallOutput,
    available_width: u16,
) -> Vec<Line<'static>> {
    let FunctionCallOutput::Content(content) = output else {
        return Vec::new();
    };
    content_preview_lines(content, available_width)
}

pub(crate) fn content_preview_lines(
    content: &[InputContent],
    available_width: u16,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for image in content.iter().filter_map(|part| match part {
        InputContent::InputImage(image) => image.image_url.as_deref().and_then(decode_data_url),
        _ => None,
    }) {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        lines.extend(render_halfblocks(&image, available_width));
    }
    lines
}

fn decode_data_url(url: &str) -> Option<DynamicImage> {
    let (header, encoded) = url.split_once(',')?;
    if !header.starts_with("data:image/") || !header.ends_with(";base64") {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    image::load_from_memory(&bytes).ok()
}

fn render_halfblocks(image: &DynamicImage, available_width: u16) -> Vec<Line<'static>> {
    let max_width = u32::from(available_width.saturating_sub(PREVIEW_INDENT.len() as u16))
        .clamp(1, MAX_PREVIEW_COLUMNS);
    let (source_width, source_height) = image.dimensions();
    if source_width == 0 || source_height == 0 {
        return Vec::new();
    }

    let width_scale = max_width as f64 / source_width as f64;
    let height_scale = (MAX_PREVIEW_ROWS * 2) as f64 / source_height as f64;
    let scale = width_scale.min(height_scale).min(1.0);
    let width = (source_width as f64 * scale).round().max(1.0) as u32;
    let height = (source_height as f64 * scale).round().max(1.0) as u32;
    let pixels = image
        .resize_exact(width, height, FilterType::Triangle)
        .to_rgba8();

    (0..height.div_ceil(2))
        .map(|row| {
            let mut spans = Vec::with_capacity(width as usize + 1);
            spans.push(Span::raw(PREVIEW_INDENT));
            for column in 0..width {
                let upper = composite(pixels.get_pixel(column, row * 2));
                let lower = if row * 2 + 1 < height {
                    composite(pixels.get_pixel(column, row * 2 + 1))
                } else {
                    surface_rgb()
                };
                spans.push(Span::styled(
                    "▀",
                    Style::new()
                        .fg(Color::Rgb(upper.0, upper.1, upper.2))
                        .bg(Color::Rgb(lower.0, lower.1, lower.2)),
                ));
            }
            Line::from(spans)
        })
        .collect()
}

fn composite(pixel: &Rgba<u8>) -> (u8, u8, u8) {
    let (background_red, background_green, background_blue) = surface_rgb();
    let alpha = u16::from(pixel[3]);
    let blend = |foreground: u8, background: u8| {
        ((u16::from(foreground) * alpha + u16::from(background) * (255 - alpha)) / 255) as u8
    };
    (
        blend(pixel[0], background_red),
        blend(pixel[1], background_green),
        blend(pixel[2], background_blue),
    )
}

fn surface_rgb() -> (u8, u8, u8) {
    match palette::SURFACE {
        Color::Rgb(red, green, blue) => (red, green, blue),
        _ => (0, 0, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_openai::types::responses::{ImageDetail, InputImageContent};
    use base64::engine::general_purpose::STANDARD;
    use image::{ImageBuffer, ImageFormat, Rgb};
    use std::io::Cursor;

    fn output() -> FunctionCallOutput {
        let image = ImageBuffer::from_fn(2, 2, |x, y| {
            if (x, y) == (0, 0) {
                Rgb([255u8, 0, 0])
            } else {
                Rgb([0u8, 0, 255])
            }
        });
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, ImageFormat::Png).unwrap();
        FunctionCallOutput::Content(vec![
            InputContent::InputText("Read image test.png (2x2).".into()),
            InputContent::InputImage(InputImageContent {
                detail: ImageDetail::Auto,
                file_id: None,
                image_url: Some(format!(
                    "data:image/png;base64,{}",
                    STANDARD.encode(bytes.into_inner())
                )),
            }),
        ])
    }

    #[test]
    fn multimodal_output_has_a_text_summary() {
        assert_eq!(output_text(&output()), "Read image test.png (2x2).");
    }

    #[test]
    fn preview_uses_colored_halfblocks() {
        let lines = preview_lines(&output(), 80);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans[1].content.as_ref(), "▀");
        assert!(matches!(
            lines[0].spans[1].style.fg,
            Some(Color::Rgb(255, 0, 0))
        ));
        assert!(matches!(
            lines[0].spans[1].style.bg,
            Some(Color::Rgb(0, 0, 255))
        ));
    }
}
