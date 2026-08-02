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
use image::{DynamicImage, GenericImageView};
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
use ratatui_image::picker::Picker;
use ratatui_image::sliced::{SignedPosition, SlicedImage, SlicedProtocol};
use ratatui_image::Resize;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, OnceLock};

const MAX_PREVIEW_COLUMNS: u32 = 64;
const MAX_PREVIEW_ROWS: u32 = 18;
const PREVIEW_INDENT: &str = "      ";

const MARKER_START: u32 = 0xE000;
const MARKER_END: u32 = 0xF8FF;

static PICKER: OnceLock<Picker> = OnceLock::new();
static IMAGES: LazyLock<Mutex<ImageRegistry>> =
    LazyLock::new(|| Mutex::new(ImageRegistry::default()));

#[derive(Default)]
struct ImageRegistry {
    next_marker: u32,
    by_key: HashMap<(blake3::Hash, u16, u16), usize>,
    markers: HashMap<char, (usize, u16)>,
    images: Vec<StoredImage>,
}

struct StoredImage {
    protocol: SlicedProtocol,
    markers: Vec<char>,
}

/// Detect the best graphics protocol (Kitty, iTerm2, Sixel, ...). This is
/// intentionally called by terminal setup before crossterm starts consuming
/// input; tests and non-interactive users retain the deterministic fallback.
pub(crate) fn detect_terminal_protocol() {
    let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
    let _ = PICKER.set(picker);
}

fn picker() -> &'static Picker {
    PICKER.get_or_init(Picker::halfblocks)
}

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
        lines.extend(render_image_lines(image, available_width));
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

fn preview_size(image: &DynamicImage, available_width: u16) -> Size {
    let max_width = u32::from(available_width.saturating_sub(PREVIEW_INDENT.len() as u16))
        .clamp(1, MAX_PREVIEW_COLUMNS);
    let (source_width, source_height) = image.dimensions();
    if source_width == 0 || source_height == 0 {
        return Size::ZERO;
    }

    let font = picker().font_size();
    let source_columns = source_width as f64 / f64::from(font.width.max(1));
    let source_rows = source_height as f64 / f64::from(font.height.max(1));
    let width_scale = max_width as f64 / source_columns;
    let height_scale = MAX_PREVIEW_ROWS as f64 / source_rows;
    let scale = width_scale.min(height_scale).min(1.0);
    Size::new(
        (source_columns * scale).ceil().max(1.0) as u16,
        (source_rows * scale).ceil().max(1.0) as u16,
    )
}

fn render_image_lines(image: DynamicImage, available_width: u16) -> Vec<Line<'static>> {
    let size = preview_size(&image, available_width);
    if size == Size::ZERO {
        return Vec::new();
    }
    let encoded = image.to_rgba8();
    let key = (blake3::hash(encoded.as_raw()), size.width, size.height);
    let mut registry = IMAGES.lock().unwrap();
    let image_id = if let Some(image_id) = registry.by_key.get(&key) {
        *image_id
    } else {
        if MARKER_START + registry.next_marker + u32::from(size.height) > MARKER_END + 1 {
            return Vec::new();
        }
        let Ok(protocol) = SlicedProtocol::new_with_resize(
            picker(),
            image,
            size,
            Resize::Fit(None),
        ) else {
            return Vec::new();
        };
        let image_id = registry.images.len();
        let markers = (0..size.height)
            .map(|row| {
                let marker = char::from_u32(MARKER_START + registry.next_marker + u32::from(row))
                    .expect("private-use marker is valid");
                registry.markers.insert(marker, (image_id, row));
                marker
            })
            .collect();
        registry.next_marker += u32::from(size.height);
        registry.images.push(StoredImage { protocol, markers });
        registry.by_key.insert(key, image_id);
        image_id
    };
    let markers = &registry.images[image_id].markers;

    (0..size.height)
        .map(|row| {
            Line::from(vec![
                Span::raw(PREVIEW_INDENT),
                Span::raw(markers[usize::from(row)].to_string()),
            ])
        })
        .collect()
}

/// Replace visible private-use placeholders with protocol-native, vertically
/// sliced images. Slicing is essential inside the scrolling transcript: iTerm2
/// cannot clip a whole inline image after it has been emitted.
pub(crate) fn render_protocol_images(area: Rect, buf: &mut Buffer) {
    let registry = IMAGES.lock().unwrap();
    let mut placements: HashMap<usize, (u16, u16, u16)> = HashMap::new();
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            let Some(cell) = buf.cell((x, y)) else {
                continue;
            };
            let mut chars = cell.symbol().chars();
            let Some(marker) = chars.next() else { continue };
            if chars.next().is_none()
                && let Some(&(image_id, row)) = registry.markers.get(&marker)
            {
                placements.entry(image_id).or_insert((x, y, row));
            }
        }
    }
    for (image_id, (x, y, first_visible_row)) in placements {
        let position = SignedPosition::from((
            x.saturating_sub(area.x) as i16,
            y.saturating_sub(area.y) as i16 - first_visible_row as i16,
        ));
        SlicedImage::new(&registry.images[image_id].protocol, position).render(area, buf);
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
    fn preview_reserves_rows_for_a_protocol_image() {
        let lines = preview_lines(&output(), 80);
        assert_eq!(lines.len(), 1);
        let marker = lines[0].spans[1].content.chars().next().unwrap();
        assert!((MARKER_START..=MARKER_END).contains(&(marker as u32)));

        let area = Rect::new(0, 0, 80, 4);
        let mut buffer = Buffer::empty(area);
        ratatui::widgets::Paragraph::new(lines).render(area, &mut buffer);
        render_protocol_images(area, &mut buffer);
        assert!((0..area.width).all(|x| buffer[(x, 0)].symbol() != marker.to_string()));
    }

    #[test]
    fn partially_scrolled_image_still_renders() {
        let image = DynamicImage::ImageRgb8(ImageBuffer::from_pixel(40, 160, Rgb([255, 0, 0])));
        let lines = render_image_lines(image, 40);
        assert!(lines.len() > 2);

        // Simulate ScrollView clipping the first two image rows: the remaining
        // row markers must be enough to reconstruct the correct slice.
        let visible = lines.into_iter().skip(2).collect::<Vec<_>>();
        let area = Rect::new(0, 0, 40, visible.len() as u16);
        let mut buffer = Buffer::empty(area);
        ratatui::widgets::Paragraph::new(visible).render(area, &mut buffer);
        render_protocol_images(area, &mut buffer);

        assert!((0..area.height).all(|y| {
            (0..area.width).all(|x| {
                !buffer[(x, y)]
                    .symbol()
                    .chars()
                    .any(|c| (MARKER_START..=MARKER_END).contains(&(c as u32)))
            })
        }));
    }
}
