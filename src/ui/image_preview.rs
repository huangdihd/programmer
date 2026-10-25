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
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};
use ratatui_image::picker::cap_parser::{Parser, QueryStdioOptions};
use ratatui_image::picker::{Capability, Picker, ProtocolType};
use ratatui_image::protocol::{Protocol, iterm2::Iterm2};
use ratatui_image::sliced::{SignedPosition, SlicedImage, SlicedProtocol};
use ratatui_image::{Image as TerminalImage, Resize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Write as _;
use std::io::Cursor;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};

const MAX_PREVIEW_COLUMNS: u32 = 64;
const MAX_PREVIEW_ROWS: u32 = 18;
const PREVIEW_INDENT: &str = "      ";

const MARKER_START: u32 = 0xE000;
const MARKER_END: u32 = 0xF8FF;

static PICKER: OnceLock<Picker> = OnceLock::new();
static IMAGES: LazyLock<Mutex<ImageRegistry>> =
    LazyLock::new(|| Mutex::new(ImageRegistry::default()));

type PreviewLineCache = Mutex<HashMap<(blake3::Hash, u16), Vec<Line<'static>>>>;

static PREVIEW_LINE_CACHE: LazyLock<PreviewLineCache> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Default)]
struct ImageRegistry {
    next_marker: u32,
    by_key: HashMap<(blake3::Hash, u16, u16), usize>,
    markers: HashMap<char, (usize, u16)>,
    images: Vec<StoredImage>,
}

struct StoredImage {
    source: Arc<DynamicImage>,
    protocol: RenderedImage,
    size: Size,
    markers: Vec<char>,
}

/// Encoded terminal image. Halfblocks use a local encoder so they can resize
/// directly from the source into terminal cells; the generic protocol path
/// otherwise creates a much larger font-pixel intermediate image first.
enum RenderedImage {
    /// Vertically sliced native protocol used by inline transcript images.
    Native(SlicedProtocol),
    /// Single native image used by the modal viewer's bounded viewport crop.
    ViewerNative(Protocol),
    Halfblocks(HalfblockImage),
}

impl RenderedImage {
    fn new(picker: &Picker, source: &DynamicImage, size: Size, resize: Resize) -> Option<Self> {
        if picker.protocol_type() == ProtocolType::Halfblocks {
            return Some(Self::Halfblocks(HalfblockImage::new(source, size)));
        }
        SlicedProtocol::new_with_resize(picker, source.clone(), size, resize)
            .ok()
            .map(Self::Native)
    }

    fn new_viewer(picker: &Picker, source: &DynamicImage, size: Size) -> Option<Self> {
        if picker.protocol_type() == ProtocolType::Halfblocks {
            return Some(Self::Halfblocks(HalfblockImage::new(source, size)));
        }
        picker
            .new_protocol(source.clone(), size, Resize::Scale(None))
            .ok()
            .map(Self::ViewerNative)
    }

    fn size(&self) -> Size {
        match self {
            Self::Native(protocol) => protocol.size(),
            Self::ViewerNative(protocol) => protocol.size(),
            Self::Halfblocks(image) => image.size,
        }
    }

    fn render(&self, area: Rect, position: SignedPosition, buf: &mut Buffer) {
        match self {
            Self::Native(protocol) => SlicedImage::new(protocol, position).render(area, buf),
            Self::ViewerNative(protocol) => {
                let x = position.x.max(0) as u16;
                let y = position.y.max(0) as u16;
                let render_area = Rect::new(
                    area.x.saturating_add(x),
                    area.y.saturating_add(y),
                    protocol.size().width.min(area.width.saturating_sub(x)),
                    protocol.size().height.min(area.height.saturating_sub(y)),
                );
                TerminalImage::new(protocol)
                    .allow_clipping(true)
                    .render(render_area, buf);
            }
            Self::Halfblocks(image) => image.render(area, position, buf),
        }
    }
}

struct HalfblockImage {
    cells: Vec<HalfblockCell>,
    size: Size,
}

#[derive(Clone, Copy)]
struct HalfblockCell {
    upper: Color,
    lower: Color,
    symbol: char,
}

impl HalfblockImage {
    fn new(source: &DynamicImage, size: Size) -> Self {
        if size == Size::ZERO {
            return Self {
                cells: Vec::new(),
                size,
            };
        }
        let width = u32::from(size.width);
        let height = u32::from(size.height);
        let pixels = source
            .resize_exact(
                width,
                height.saturating_mul(2),
                image::imageops::FilterType::Triangle,
            )
            .to_rgb8();
        let bytes = pixels.as_raw();
        let stride = width as usize * 3;
        let mut cells = Vec::with_capacity(size.width as usize * size.height as usize);
        for row in 0..size.height as usize {
            let upper_row = row * 2 * stride;
            let lower_row = upper_row + stride;
            for column in 0..size.width as usize {
                let upper = rgb(bytes, upper_row + column * 3);
                let lower = rgb(bytes, lower_row + column * 3);
                cells.push(halfblock_cell(upper, lower));
            }
        }
        Self { cells, size }
    }

    fn render(&self, area: Rect, position: SignedPosition, buf: &mut Buffer) {
        let left = i32::from(position.x).max(0);
        let top = i32::from(position.y).max(0);
        let right = (i32::from(position.x) + i32::from(self.size.width)).min(i32::from(area.width));
        let bottom =
            (i32::from(position.y) + i32::from(self.size.height)).min(i32::from(area.height));
        if left >= right || top >= bottom {
            return;
        }
        for y in top..bottom {
            let source_y = (y - i32::from(position.y)) as usize;
            for x in left..right {
                let source_x = (x - i32::from(position.x)) as usize;
                let cell = self.cells[source_y * self.size.width as usize + source_x];
                if let Some(target) = buf.cell_mut((area.x + x as u16, area.y + y as u16)) {
                    target
                        .set_char(cell.symbol)
                        .set_fg(cell.upper)
                        .set_bg(cell.lower);
                }
            }
        }
    }
}

fn rgb(bytes: &[u8], offset: usize) -> [u8; 3] {
    [bytes[offset], bytes[offset + 1], bytes[offset + 2]]
}

fn halfblock_cell(mut upper: [u8; 3], mut lower: [u8; 3]) -> HalfblockCell {
    let symbol = if upper == lower {
        ' '
    } else if luminance(lower) > luminance(upper) {
        std::mem::swap(&mut upper, &mut lower);
        '▄'
    } else {
        '▀'
    };
    HalfblockCell {
        upper: Color::Rgb(upper[0], upper[1], upper[2]),
        lower: Color::Rgb(lower[0], lower[1], lower[2]),
        symbol,
    }
}

fn luminance([red, green, blue]: [u8; 3]) -> u32 {
    2126 * u32::from(red) + 7152 * u32::from(green) + 722 * u32::from(blue)
}

/// A rendered inline image that can be clicked in the conversation viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ImagePlacement {
    pub image_id: usize,
    pub area: Rect,
}

/// Modal state for inspecting a decoded conversation image.
pub(crate) struct ImageViewer {
    source: Arc<DynamicImage>,
    zoom_index: usize,
    pan_x: u16,
    pan_y: u16,
    drag_at: Option<(u16, u16)>,
    drag_moved: bool,
    rendered: VecDeque<(ViewerRenderRequest, RenderedImage)>,
    viewport: Size,
    render_requests: Sender<ViewerRenderRequest>,
    render_results: Receiver<(ViewerRenderRequest, Option<RenderedImage>)>,
    requested: Option<ViewerRenderRequest>,
    failed_target: Option<ViewerRenderRequest>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewerRenderQuality {
    DragPreview,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ViewerRenderRequest {
    /// Size of the complete zoomed image in terminal cells.
    canvas: Size,
    /// Size of the modal image viewport in terminal cells.
    viewport: Size,
    pan_x: u16,
    pan_y: u16,
    quality: ViewerRenderQuality,
}

impl ViewerRenderRequest {
    fn visible_size(self) -> Size {
        Size::new(
            self.canvas.width.min(self.viewport.width),
            self.canvas.height.min(self.viewport.height),
        )
    }
}

fn viewer_source_window(
    source: &DynamicImage,
    request: ViewerRenderRequest,
) -> Option<DynamicImage> {
    let visible = request.visible_size();
    let (source_width, source_height) = source.dimensions();
    if request.canvas == Size::ZERO
        || visible == Size::ZERO
        || source_width == 0
        || source_height == 0
    {
        return None;
    }

    let source_boundary = |cell: u16, cells: u16, pixels: u32, round_up: bool| {
        let numerator = u64::from(cell) * u64::from(pixels);
        let denominator = u64::from(cells.max(1));
        let value = if round_up {
            numerator.div_ceil(denominator)
        } else {
            numerator / denominator
        };
        value.min(u64::from(pixels)) as u32
    };
    let left = source_boundary(request.pan_x, request.canvas.width, source_width, false);
    let top = source_boundary(request.pan_y, request.canvas.height, source_height, false);
    let right = source_boundary(
        request.pan_x.saturating_add(visible.width),
        request.canvas.width,
        source_width,
        true,
    );
    let bottom = source_boundary(
        request.pan_y.saturating_add(visible.height),
        request.canvas.height,
        source_height,
        true,
    );
    (right > left && bottom > top).then(|| source.crop_imm(left, top, right - left, bottom - top))
}

fn render_viewer_request(
    picker: &Picker,
    source: &DynamicImage,
    request: ViewerRenderRequest,
) -> Option<RenderedImage> {
    let visible = request.visible_size();
    let mut source = viewer_source_window(source, request)?;
    let render_size = match (request.quality, picker.protocol_type()) {
        (ViewerRenderQuality::DragPreview, ProtocolType::Iterm2) => {
            return iterm_drag_preview(picker, &source, visible);
        }
        (ViewerRenderQuality::DragPreview, ProtocolType::Sixel) => {
            source = pixelated_drag_preview(picker, &source, visible);
            visible
        }
        (ViewerRenderQuality::DragPreview, ProtocolType::Kitty) => {
            Size::new(visible.width.div_ceil(2), visible.height.div_ceil(2))
        }
        _ => visible,
    };
    RenderedImage::new_viewer(picker, &source, render_size)
}

fn iterm_drag_preview(
    picker: &Picker,
    source: &DynamicImage,
    visible: Size,
) -> Option<RenderedImage> {
    let font = picker.font_size();
    let target_width = u32::from(visible.width) * u32::from(font.width.max(1));
    let target_height = u32::from(visible.height) * u32::from(font.height.max(1));
    let preview_width = target_width.div_ceil(DRAG_PREVIEW_PIXEL_DIVISOR).max(1);
    let preview_height = target_height.div_ceil(DRAG_PREVIEW_PIXEL_DIVISOR).max(1);
    let preview = source.resize_exact(
        preview_width,
        preview_height,
        image::imageops::FilterType::Triangle,
    );
    let mut png = Cursor::new(Vec::new());
    preview.write_to(&mut png, image::ImageFormat::Png).ok()?;
    let png = png.into_inner();
    let is_tmux = running_in_tmux();
    let (start, escape, end) = Parser::tmux_start_escape_end(is_tmux);
    let mut data = String::from(start);
    append_clear_area(&mut data, escape, visible);
    write!(
        data,
        "{escape}]1337;File=inline=1;size={};width={}px;height={}px;doNotMoveCursor=1:",
        png.len(),
        target_width,
        target_height,
    )
    .ok()?;
    base64::engine::general_purpose::STANDARD.encode_string(&png, &mut data);
    write!(data, "\x07{end}").ok()?;
    Some(RenderedImage::ViewerNative(Protocol::ITerm2(Iterm2 {
        data,
        size: visible,
        is_tmux,
    })))
}

fn append_clear_area(data: &mut String, escape: &str, size: Size) {
    if size.height == 1 {
        let _ = write!(data, "{escape}[{}X", size.width);
        return;
    }
    for _ in 0..size.height {
        let _ = write!(data, "{escape}[{}X{escape}[1B", size.width);
    }
    let _ = write!(data, "{escape}[{}A", size.height);
}

fn running_in_tmux() -> bool {
    std::env::var("TERM").is_ok_and(|term| term.starts_with("tmux"))
        || std::env::var("TERM_PROGRAM").is_ok_and(|program| program == "tmux")
}

fn pixelated_drag_preview(picker: &Picker, source: &DynamicImage, visible: Size) -> DynamicImage {
    let font = picker.font_size();
    let target_width = u32::from(visible.width) * u32::from(font.width.max(1));
    let target_height = u32::from(visible.height) * u32::from(font.height.max(1));
    let preview_width = target_width.div_ceil(DRAG_PREVIEW_PIXEL_DIVISOR).max(1);
    let preview_height = target_height.div_ceil(DRAG_PREVIEW_PIXEL_DIVISOR).max(1);
    source
        .resize_exact(
            preview_width,
            preview_height,
            image::imageops::FilterType::Triangle,
        )
        .resize_exact(
            target_width,
            target_height,
            image::imageops::FilterType::Nearest,
        )
}

const ZOOM_LEVELS: &[u16] = &[50, 67, 80, 100, 125, 150, 200, 300, 400, 500, 600, 800];
const DEFAULT_ZOOM_INDEX: usize = 3;
const VIEWER_PROTOCOL_CACHE_SIZE: usize = 3;
const DRAG_PREVIEW_PIXEL_DIVISOR: u32 = 5;

impl ImageViewer {
    fn new(source: Arc<DynamicImage>) -> Self {
        let (request_tx, request_rx) = std::sync::mpsc::channel();
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let worker_source = source.clone();
        let worker_picker = picker().clone();
        std::thread::spawn(move || {
            while let Ok(mut target) = request_rx.recv() {
                // Wheel input commonly arrives in bursts. Before starting an
                // expensive resize, keep only the newest requested size.
                while let Ok(newer) = request_rx.try_recv() {
                    target = newer;
                }
                let protocol = render_viewer_request(&worker_picker, &worker_source, target);
                if result_tx.send((target, protocol)).is_err() {
                    break;
                }
            }
        });
        Self {
            source,
            zoom_index: DEFAULT_ZOOM_INDEX,
            pan_x: 0,
            pan_y: 0,
            drag_at: None,
            drag_moved: false,
            rendered: VecDeque::new(),
            viewport: Size::ZERO,
            render_requests: request_tx,
            render_results: result_rx,
            requested: None,
            failed_target: None,
        }
    }

    pub(crate) fn zoom_in(&mut self) {
        self.set_zoom_index((self.zoom_index + 1).min(ZOOM_LEVELS.len() - 1));
    }

    pub(crate) fn zoom_out(&mut self) {
        self.set_zoom_index(self.zoom_index.saturating_sub(1));
    }

    fn set_zoom_index(&mut self, zoom_index: usize) {
        if zoom_index == self.zoom_index {
            return;
        }
        let old_canvas = self.target_size(self.viewport);
        let old_center_x =
            f64::from(self.pan_x) + f64::from(old_canvas.width.min(self.viewport.width)) / 2.0;
        let old_center_y =
            f64::from(self.pan_y) + f64::from(old_canvas.height.min(self.viewport.height)) / 2.0;
        self.zoom_index = zoom_index;
        let new_canvas = self.target_size(self.viewport);
        if old_canvas != Size::ZERO {
            let new_center_x =
                old_center_x * f64::from(new_canvas.width) / f64::from(old_canvas.width.max(1));
            let new_center_y =
                old_center_y * f64::from(new_canvas.height) / f64::from(old_canvas.height.max(1));
            self.pan_x = (new_center_x - f64::from(self.viewport.width) / 2.0)
                .round()
                .clamp(0.0, f64::from(u16::MAX)) as u16;
            self.pan_y = (new_center_y - f64::from(self.viewport.height) / 2.0)
                .round()
                .clamp(0.0, f64::from(u16::MAX)) as u16;
        }
        self.clamp_pan(new_canvas, self.viewport);
    }

    pub(crate) fn reset(&mut self) {
        self.zoom_index = DEFAULT_ZOOM_INDEX;
        self.pan_x = 0;
        self.pan_y = 0;
        self.drag_at = None;
        self.drag_moved = false;
    }

    pub(crate) fn pan_by(&mut self, dx: i16, dy: i16) {
        self.pan_x = self.pan_x.saturating_add_signed(dx);
        self.pan_y = self.pan_y.saturating_add_signed(dy);
        self.clamp_pan_to_target();
    }

    pub(crate) fn begin_drag(&mut self, column: u16, row: u16) {
        self.drag_at = Some((column, row));
        self.drag_moved = false;
    }

    pub(crate) fn drag_to(&mut self, column: u16, row: u16) {
        let Some((last_x, last_y)) = self.drag_at.replace((column, row)) else {
            return;
        };
        // Dragging moves the image itself; the viewed source offset moves in
        // the opposite direction.
        let dx = (i32::from(last_x) - i32::from(column))
            .clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
        let dy = (i32::from(last_y) - i32::from(row))
            .clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
        if dx != 0 || dy != 0 {
            self.drag_moved = true;
        }
        self.pan_by(dx, dy);
    }

    pub(crate) fn end_drag(&mut self) {
        self.drag_at = None;
        self.drag_moved = false;
    }

    fn target_size(&self, viewport: Size) -> Size {
        let (source_width, source_height) = self.source.dimensions();
        if source_width == 0 || source_height == 0 || viewport == Size::ZERO {
            return Size::ZERO;
        }
        let font = picker().font_size();
        let source_columns = source_width as f64 / f64::from(font.width.max(1));
        let source_rows = source_height as f64 / f64::from(font.height.max(1));
        let fit = (f64::from(viewport.width) / source_columns)
            .min(f64::from(viewport.height) / source_rows)
            .min(1.0);
        let zoom = f64::from(ZOOM_LEVELS[self.zoom_index]) / 100.0;
        Size::new(
            (source_columns * fit * zoom)
                .ceil()
                .clamp(1.0, f64::from(u16::MAX)) as u16,
            (source_rows * fit * zoom)
                .ceil()
                .clamp(1.0, f64::from(u16::MAX)) as u16,
        )
    }

    fn clamp_pan(&mut self, image: Size, viewport: Size) {
        self.pan_x = self.pan_x.min(image.width.saturating_sub(viewport.width));
        self.pan_y = self.pan_y.min(image.height.saturating_sub(viewport.height));
    }

    fn clamp_pan_to_target(&mut self) {
        let target = self.target_size(self.viewport);
        self.clamp_pan(target, self.viewport);
    }

    fn zoom_percent(&self, size: Size) -> f64 {
        let (source_width, source_height) = self.source.dimensions();
        if source_width == 0 || source_height == 0 || size == Size::ZERO {
            return 0.0;
        }
        let font = picker().font_size();
        let width_scale =
            f64::from(size.width) * f64::from(font.width.max(1)) / f64::from(source_width);
        let height_scale =
            f64::from(size.height) * f64::from(font.height.max(1)) / f64::from(source_height);
        width_scale.min(height_scale) * 100.0
    }

    fn zoom_label(&self, size: Size) -> String {
        let percent = self.zoom_percent(size);
        if percent < 10.0 && (percent - percent.round()).abs() >= 0.05 {
            format!("{percent:.1}%")
        } else {
            format!("{percent:.0}%")
        }
    }

    fn collect_render_results(&mut self, target: ViewerRenderRequest) {
        while let Ok((request, protocol)) = self.render_results.try_recv() {
            if self.requested == Some(request) {
                self.requested = None;
            }
            let Some(protocol) = protocol else {
                self.failed_target = Some(request);
                continue;
            };
            self.failed_target = None;
            self.cache_render_result(request, protocol, target);
        }
    }

    fn cache_render_result(
        &mut self,
        request: ViewerRenderRequest,
        protocol: RenderedImage,
        target: ViewerRenderRequest,
    ) {
        if let Some(index) = self
            .rendered
            .iter()
            .position(|(cached_request, _)| *cached_request == request)
        {
            self.rendered.remove(index);
        }
        if self.rendered.is_empty()
            || (request.canvas == target.canvas && request.viewport == target.viewport)
        {
            // Native encoding can trail a moving pointer. Present each newest
            // same-zoom crop as it completes instead of freezing the original
            // crop until the worker reaches the exact final pan coordinates.
            self.rendered.push_front((request, protocol));
        } else {
            // An obsolete zoom or viewport must not replace the visible image
            // while its newer request is still being encoded.
            self.rendered.push_back((request, protocol));
        }
    }

    pub(crate) fn render_pending(&self) -> bool {
        self.requested.is_some()
    }

    pub(crate) fn render(&mut self, area: Rect, buf: &mut Buffer) {
        if area.width < 6 || area.height < 4 {
            return;
        }
        let modal = Rect::new(
            area.x.saturating_add(1),
            area.y.saturating_add(1),
            area.width.saturating_sub(2),
            area.height.saturating_sub(2),
        );
        Clear.render(modal, buf);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(crate::ui::theme::role::FOCUS));
        let inner = block.inner(modal);
        if inner.height == 0 {
            block.render(modal, buf);
            return;
        }
        let image_area = Rect::new(
            inner.x,
            inner.y,
            inner.width,
            inner.height.saturating_sub(1),
        );
        let help_area = Rect::new(
            inner.x,
            image_area.bottom(),
            inner.width,
            inner.height.saturating_sub(image_area.height),
        );
        self.viewport = image_area.as_size();
        let canvas = self.target_size(self.viewport);
        self.clamp_pan(canvas, self.viewport);
        let target = ViewerRenderRequest {
            canvas,
            viewport: self.viewport,
            pan_x: self.pan_x,
            pan_y: self.pan_y,
            quality: if self.drag_moved {
                ViewerRenderQuality::DragPreview
            } else {
                ViewerRenderQuality::Full
            },
        };
        self.collect_render_results(target);

        let cached = self
            .rendered
            .iter()
            .position(|(cached_request, _)| *cached_request == target);
        if cached.is_none()
            && canvas != Size::ZERO
            && self.requested != Some(target)
            && self.failed_target != Some(target)
            && self.render_requests.send(target).is_ok()
        {
            self.requested = Some(target);
        }
        if let Some(index) = cached {
            let cached = self.rendered.remove(index).expect("cached protocol exists");
            self.rendered.push_front(cached);
        }
        self.rendered.truncate(VIEWER_PROTOCOL_CACHE_SIZE);

        let title = match self.rendered.front() {
            Some((displayed, _)) if *displayed == target || displayed.canvas == target.canvas => {
                format!(" Image · {} ", self.zoom_label(displayed.canvas))
            }
            Some((displayed, _)) if self.requested == Some(target) => format!(
                " Image · {} · loading {} ",
                self.zoom_label(displayed.canvas),
                self.zoom_label(target.canvas)
            ),
            Some((displayed, _)) if self.failed_target == Some(target) => format!(
                " Image · {} · {} unavailable ",
                self.zoom_label(displayed.canvas),
                self.zoom_label(target.canvas)
            ),
            Some((displayed, _)) => {
                format!(" Image · {} ", self.zoom_label(displayed.canvas))
            }
            None if self.failed_target == Some(target) => {
                format!(" Image · {} unavailable ", self.zoom_label(target.canvas))
            }
            None => format!(" Image · loading {} ", self.zoom_label(target.canvas)),
        };
        block.title(title).render(modal, buf);
        Paragraph::new(Line::from(vec![
            Span::styled(
                " +/- or wheel ",
                Style::default()
                    .fg(crate::ui::theme::role::HEADING)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "zoom  ·  arrows/drag pan  ·  0 reset  ·  Esc close",
                Style::default().fg(crate::ui::markdown_theme::palette::MUTED),
            ),
        ]))
        .render(help_area, buf);

        let Some((_, protocol)) = self.rendered.front() else {
            Paragraph::new("Rendering image…")
                .style(Style::default().fg(crate::ui::markdown_theme::palette::MUTED))
                .render(image_area, buf);
            return;
        };
        let visible = protocol.size();
        let x = ((image_area.width - visible.width) / 2) as i16;
        let y = ((image_area.height - visible.height) / 2) as i16;
        protocol.render(image_area, SignedPosition::from((x, y)), buf);
    }
}

pub(crate) fn viewer(image_id: usize) -> Option<ImageViewer> {
    IMAGES
        .lock()
        .ok()?
        .images
        .get(image_id)
        .map(|image| ImageViewer::new(image.source.clone()))
}

/// Detect the best graphics protocol (Kitty, iTerm2, Sixel, ...). This is
/// intentionally called by terminal setup before crossterm starts consuming
/// input; tests and non-interactive users retain the deterministic fallback.
pub(crate) fn detect_terminal_protocol() {
    // Reuse the existing pre-event-loop query rather than racing a second
    // stdin reader against crossterm. OSC 11 adds no separate round trip.
    let mut picker = Picker::from_query_stdio_with_options(QueryStdioOptions {
        terminal_background_color_osc: true,
        timeout: std::time::Duration::from_millis(500),
        ..QueryStdioOptions::default()
    })
    .unwrap_or_else(|_| Picker::halfblocks());
    let background = picker
        .capabilities()
        .iter()
        .find_map(|capability| match capability {
            Capability::Background(r, g, b) => Some((*r, *g, *b)),
            _ => None,
        });
    crate::ui::theme::set_terminal_background(background);

    let running_in_iterm = std::env::var("TERM_PROGRAM").is_ok_and(|value| value.contains("iTerm"))
        || std::env::var("LC_TERMINAL").is_ok_and(|value| value.contains("iTerm"))
        || std::env::var("ITERM_SESSION_ID").is_ok_and(|value| !value.is_empty());

    let forced_protocol = std::env::var("PROGRAMMER_IMAGE_PROTOCOL").ok();

    match forced_protocol.as_deref() {
        Some("sixel") => {
            picker.set_protocol_type(ProtocolType::Sixel);
        }
        Some("kitty") => {
            picker.set_protocol_type(ProtocolType::Kitty);
        }
        Some("iterm") | Some("iterm2") => {
            picker.set_protocol_type(ProtocolType::Iterm2);
        }
        Some("halfblocks") => {
            picker.set_protocol_type(ProtocolType::Halfblocks);
        }
        _ if running_in_iterm => {
            picker.set_protocol_type(ProtocolType::Iterm2);
        }
        _ => {}
    }

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

    for url in content.iter().filter_map(|part| match part {
        InputContent::InputImage(image) => image.image_url.as_deref(),
        _ => None,
    }) {
        if !lines.is_empty() {
            lines.push(Line::default());
        }

        lines.extend(cached_data_url_lines(url, available_width));
    }

    lines
}

fn cached_data_url_lines(url: &str, available_width: u16) -> Vec<Line<'static>> {
    let key = (blake3::hash(url.as_bytes()), available_width);

    {
        let cache = PREVIEW_LINE_CACHE.lock().unwrap();

        if let Some(lines) = cache.get(&key) {
            return lines.clone();
        }
    }

    let lines = decode_data_url(url)
        .map(|image| render_image_lines(image, available_width))
        .unwrap_or_default();

    PREVIEW_LINE_CACHE
        .lock()
        .unwrap()
        .insert(key, lines.clone());

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
        let source = Arc::new(image);
        let Some(protocol) = RenderedImage::new(picker(), &source, size, Resize::Fit(None)) else {
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
        registry.images.push(StoredImage {
            source,
            protocol,
            size,
            markers,
        });
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
pub(crate) fn render_protocol_images(area: Rect, buf: &mut Buffer) -> Vec<ImagePlacement> {
    let registry = IMAGES.lock().unwrap();
    // Key by visible occurrence, not only by image id: identical image bytes
    // can appear more than once in the transcript.
    let mut placements: HashSet<(usize, u16, i32)> = HashSet::new();
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
                let origin_y = i32::from(y) - i32::from(row);
                placements.insert((image_id, x, origin_y));
            }
        }
    }
    let mut hits = Vec::with_capacity(placements.len());
    for (image_id, x, origin_y) in placements {
        let stored = &registry.images[image_id];
        let relative_top = origin_y - i32::from(area.y);
        let relative_top = relative_top.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
        let position = SignedPosition::from((x.saturating_sub(area.x) as i16, relative_top));
        stored.protocol.render(area, position, buf);

        let full_top = origin_y;
        let visible_top = full_top.max(i32::from(area.y));
        let visible_bottom =
            (full_top + i32::from(stored.size.height)).min(i32::from(area.bottom()));
        let visible_right = x.saturating_add(stored.size.width).min(area.right());
        if visible_top < visible_bottom && x < visible_right {
            hits.push(ImagePlacement {
                image_id,
                area: Rect::new(
                    x,
                    visible_top as u16,
                    visible_right - x,
                    (visible_bottom - visible_top) as u16,
                ),
            });
        }
    }
    hits
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
        let _ = render_protocol_images(area, &mut buffer);
        assert!((0..area.width).all(|x| buffer[(x, 0)].symbol() != marker.to_string()));
    }

    #[test]
    fn identical_visible_images_keep_distinct_hit_placements() {
        let lines = preview_lines(&output(), 80);
        let mut doubled = lines.clone();
        doubled.push(Line::default());
        doubled.extend(lines);
        let area = Rect::new(0, 0, 80, 4);
        let mut buffer = Buffer::empty(area);
        ratatui::widgets::Paragraph::new(doubled).render(area, &mut buffer);

        let mut placements = render_protocol_images(area, &mut buffer);
        placements.sort_unstable_by_key(|placement| placement.area.y);
        assert_eq!(placements.len(), 2);
        assert_eq!(placements[0].image_id, placements[1].image_id);
        assert_ne!(placements[0].area.y, placements[1].area.y);
    }

    #[test]
    fn halfblocks_encode_directly_to_the_requested_cell_grid() {
        let source = DynamicImage::ImageRgb8(ImageBuffer::from_fn(320, 240, |x, y| {
            Rgb([x as u8, y as u8, 128])
        }));
        let requested = Size::new(80, 24);
        let protocol =
            RenderedImage::new(&Picker::halfblocks(), &source, requested, Resize::Fit(None))
                .expect("halfblocks encoding succeeds");
        let RenderedImage::Halfblocks(image) = protocol else {
            panic!("halfblocks picker must use the direct cell-grid encoder");
        };
        assert_eq!(image.size, requested);
        assert_eq!(image.cells.len(), 80 * 24);
    }

    #[test]
    fn viewer_native_zoom_encodes_only_the_visible_window() {
        let source = DynamicImage::ImageRgb8(ImageBuffer::from_fn(2_024, 2_560, |x, y| {
            Rgb([(x % 256) as u8, (y % 256) as u8, 128])
        }));
        let request = ViewerRenderRequest {
            // 2024×2560 at the 10×20 test font keeps the same physical
            // aspect ratio when represented by this enlarged cell canvas.
            canvas: Size::new(640, 405),
            viewport: Size::new(40, 20),
            pan_x: 270,
            pan_y: 190,
            quality: ViewerRenderQuality::Full,
        };
        let cropped = viewer_source_window(&source, request).expect("visible crop exists");
        assert!(cropped.width() < source.width());
        assert!(cropped.height() < source.height());

        let mut picker = Picker::halfblocks();
        picker.set_protocol_type(ProtocolType::Iterm2);
        let protocol =
            render_viewer_request(&picker, &source, request).expect("visible native crop encodes");
        assert!(matches!(protocol, RenderedImage::ViewerNative(_)));
        assert_eq!(protocol.size(), request.viewport);

        let preview_request = ViewerRenderRequest {
            quality: ViewerRenderQuality::DragPreview,
            ..request
        };
        let preview = render_viewer_request(&picker, &source, preview_request)
            .expect("native drag preview encodes");
        assert_eq!(preview.size(), request.viewport);
        let RenderedImage::ViewerNative(Protocol::ITerm2(preview)) = &preview else {
            panic!("iTerm2 drag preview must remain a native protocol image");
        };
        assert!(preview.data.contains("width=400px;height=400px"));
        let payload_start = preview.data.rfind(':').expect("iTerm payload separator") + 1;
        let payload_end = preview.data[payload_start..]
            .find('\x07')
            .map(|offset| payload_start + offset)
            .expect("iTerm payload terminator");
        let png = STANDARD
            .decode(&preview.data[payload_start..payload_end])
            .expect("drag preview payload is base64");
        assert_eq!(
            image::load_from_memory(&png).unwrap().dimensions(),
            (80, 80),
            "the terminal scales a small PNG over the full 40×20-cell viewport"
        );

        picker.set_protocol_type(ProtocolType::Sixel);
        let sixel_preview = render_viewer_request(&picker, &source, preview_request)
            .expect("sixel drag preview encodes");
        assert!(matches!(sixel_preview, RenderedImage::ViewerNative(_)));
        assert_eq!(sixel_preview.size(), request.viewport);

        picker.set_protocol_type(ProtocolType::Kitty);
        let kitty_preview = render_viewer_request(&picker, &source, preview_request)
            .expect("kitty drag preview encodes");
        assert!(matches!(kitty_preview, RenderedImage::ViewerNative(_)));
        assert_eq!(kitty_preview.size(), Size::new(20, 10));
    }

    #[test]
    fn large_portrait_image_continues_zooming_past_fitted_82_percent() {
        let source = Arc::new(DynamicImage::ImageRgb8(ImageBuffer::from_pixel(
            2_024,
            2_560,
            Rgb([255, 255, 255]),
        )));
        let mut viewer = ImageViewer::new(source);
        let viewport = Size::new(166, 105);
        viewer.viewport = viewport;
        let fitted = viewer.target_size(viewport);
        assert_eq!(viewer.zoom_label(fitted), "82%");

        let mut previous = fitted;
        while viewer.zoom_index + 1 < ZOOM_LEVELS.len() {
            viewer.zoom_in();
            let zoomed = viewer.target_size(viewport);
            assert!(zoomed.width > previous.width);
            assert!(zoomed.height > previous.height);
            if viewer.zoom_index == DEFAULT_ZOOM_INDEX + 1 {
                assert_eq!(viewer.zoom_label(zoomed), "103%");
            }
            assert_eq!(
                ViewerRenderRequest {
                    canvas: zoomed,
                    viewport,
                    pan_x: viewer.pan_x,
                    pan_y: viewer.pan_y,
                    quality: ViewerRenderQuality::Full,
                }
                .visible_size(),
                viewport,
                "every native zoom level must stay bounded to the modal viewport"
            );
            previous = zoomed;
        }
    }

    #[test]
    fn viewer_presents_completed_pan_frames_while_pointer_keeps_moving() {
        let source = Arc::new(DynamicImage::ImageRgb8(ImageBuffer::from_pixel(
            400,
            400,
            Rgb([0, 0, 0]),
        )));
        let mut viewer = ImageViewer::new(source);
        let area = Rect::new(0, 0, 42, 14);
        viewer.viewport = Size::new(38, 9);
        while viewer.zoom_index + 1 < ZOOM_LEVELS.len() {
            viewer.zoom_in();
        }
        viewer.pan_x = 30;
        viewer.pan_y = 10;
        let target = ViewerRenderRequest {
            canvas: viewer.target_size(viewer.viewport),
            viewport: viewer.viewport,
            pan_x: viewer.pan_x,
            pan_y: viewer.pan_y,
            quality: ViewerRenderQuality::Full,
        };
        assert!(target.canvas.width > target.viewport.width);
        assert!(target.canvas.height > target.viewport.height);
        // Keep render() from starting a real worker request: this test drives
        // the completion order deterministically through cache_render_result.
        viewer.requested = Some(target);

        let protocol = |color| {
            RenderedImage::Halfblocks(HalfblockImage::new(
                &DynamicImage::ImageRgb8(ImageBuffer::from_pixel(2, 2, color)),
                target.visible_size(),
            ))
        };
        let original = ViewerRenderRequest {
            pan_x: 0,
            pan_y: 0,
            ..target
        };
        viewer.cache_render_result(original, protocol(Rgb([255, 0, 0])), target);
        let mut before = Buffer::empty(area);
        viewer.render(area, &mut before);
        assert_eq!(before[(2, 2)].bg, Color::Rgb(255, 0, 0));

        // The pointer has already reached `target`, but this intermediate crop
        // must still replace the older visible crop as soon as it is ready.
        let intermediate = ViewerRenderRequest {
            pan_x: 15,
            pan_y: 5,
            ..target
        };
        viewer.cache_render_result(intermediate, protocol(Rgb([0, 255, 0])), target);
        let mut advanced = Buffer::empty(area);
        viewer.render(area, &mut advanced);
        assert_eq!(
            viewer.rendered.front().map(|(request, _)| *request),
            Some(intermediate)
        );
        assert_eq!(advanced[(2, 2)].bg, Color::Rgb(0, 255, 0));
        assert_ne!(before[(2, 2)].bg, advanced[(2, 2)].bg);

        // A stale result from another zoom remains cached without replacing
        // the progressively advancing pan frame.
        let obsolete_zoom = ViewerRenderRequest {
            canvas: Size::new(target.canvas.width - 1, target.canvas.height - 1),
            ..target
        };
        viewer.cache_render_result(obsolete_zoom, protocol(Rgb([0, 0, 255])), target);
        assert_eq!(
            viewer.rendered.front().map(|(request, _)| *request),
            Some(intermediate)
        );

        viewer.cache_render_result(target, protocol(Rgb([255, 255, 0])), target);
        let mut finished = Buffer::empty(area);
        viewer.render(area, &mut finished);
        assert_eq!(
            viewer.rendered.front().map(|(request, _)| *request),
            Some(target)
        );
        assert_eq!(finished[(2, 2)].bg, Color::Rgb(255, 255, 0));
    }

    #[test]
    fn viewer_drag_requests_and_displays_the_new_native_crop() {
        let source = Arc::new(DynamicImage::ImageRgb8(ImageBuffer::from_fn(
            400,
            400,
            |x, y| Rgb([(x % 256) as u8, (y % 256) as u8, 128]),
        )));
        let mut viewer = ImageViewer::new(source);
        let area = Rect::new(0, 0, 42, 14);
        let mut buffer = Buffer::empty(area);
        viewer.render(area, &mut buffer);
        let initial = viewer.requested.expect("initial crop is queued");
        wait_for_protocol(&mut viewer, initial);
        viewer.render(area, &mut buffer);

        for _ in 0..6 {
            viewer.zoom_in();
        }
        viewer.render(area, &mut buffer);
        let zoomed = viewer.requested.expect("zoomed crop is queued");
        wait_for_protocol(&mut viewer, zoomed);
        viewer.render(area, &mut buffer);
        let before = (viewer.pan_x, viewer.pan_y);

        viewer.begin_drag(20, 8);
        viewer.drag_to(15, 5);
        assert_eq!((viewer.pan_x, viewer.pan_y), (before.0 + 5, before.1 + 3));

        viewer.render(area, &mut buffer);
        let preview = viewer.requested.expect("drag preview is queued");
        assert_eq!(preview.quality, ViewerRenderQuality::DragPreview);
        assert_eq!((preview.pan_x, preview.pan_y), (viewer.pan_x, viewer.pan_y));
        wait_for_protocol(&mut viewer, preview);
        viewer.render(area, &mut buffer);
        assert_eq!(
            viewer.rendered.front().map(|(request, _)| *request),
            Some(preview)
        );

        viewer.end_drag();
        viewer.render(area, &mut buffer);
        let full = viewer
            .requested
            .expect("full crop is queued after mouse-up");
        assert_eq!(full.quality, ViewerRenderQuality::Full);
        assert_eq!((full.pan_x, full.pan_y), (viewer.pan_x, viewer.pan_y));
        wait_for_protocol(&mut viewer, full);
        viewer.render(area, &mut buffer);
        assert_eq!(
            viewer.rendered.front().map(|(request, _)| *request),
            Some(full)
        );
    }

    #[test]
    fn viewer_zoom_pan_and_reset_are_bounded() {
        let source = Arc::new(DynamicImage::ImageRgb8(ImageBuffer::from_pixel(
            200,
            200,
            Rgb([255, 0, 0]),
        )));
        let mut viewer = ImageViewer::new(source.clone());
        let viewport = Size::new(20, 10);
        let fit = viewer.target_size(viewport);
        viewer.zoom_in();
        assert!(viewer.target_size(viewport).width >= fit.width);
        for _ in 0..ZOOM_LEVELS.len() {
            viewer.zoom_in();
        }
        assert!(ZOOM_LEVELS[viewer.zoom_index] > 400);
        let zoomed = viewer.target_size(viewport);
        viewer.viewport = viewport;

        viewer.pan_by(i16::MAX, i16::MAX);
        let max_x = zoomed.width.saturating_sub(viewport.width);
        let max_y = zoomed.height.saturating_sub(viewport.height);
        assert_eq!((viewer.pan_x, viewer.pan_y), (max_x, max_y));
        viewer.pan_by(0, i16::MAX);
        assert_eq!(
            viewer.pan_y, max_y,
            "panning cannot accumulate below the image"
        );
        viewer.pan_by(0, -1);
        assert_eq!(viewer.pan_y, max_y.saturating_sub(1));

        viewer.reset();
        assert_eq!(viewer.zoom_index, DEFAULT_ZOOM_INDEX);
        assert_eq!((viewer.pan_x, viewer.pan_y), (0, 0));
    }

    #[test]
    fn viewer_label_reports_actual_source_pixel_scale() {
        let source = Arc::new(DynamicImage::ImageRgb8(ImageBuffer::from_pixel(
            1_000,
            1_000,
            Rgb([255, 0, 0]),
        )));
        let viewer = ImageViewer::new(source);
        let fitted = viewer.target_size(Size::new(20, 10));

        assert_eq!(fitted, Size::new(20, 10));
        assert_eq!(viewer.zoom_label(fitted), "20%");
        assert_ne!(
            viewer.zoom_label(fitted),
            format!("{}%", ZOOM_LEVELS[viewer.zoom_index]),
            "the title must describe rendered pixels, not fit-relative controls"
        );
    }

    #[test]
    fn viewer_builds_protocols_off_frame_and_reuses_recent_zoom_sizes() {
        let source = Arc::new(DynamicImage::ImageRgb8(ImageBuffer::from_pixel(
            200,
            200,
            Rgb([255, 0, 0]),
        )));
        let mut viewer = ImageViewer::new(source);
        let area = Rect::new(0, 0, 42, 14);
        let mut buffer = Buffer::empty(area);

        viewer.render(area, &mut buffer);
        let fit = viewer.requested.expect("first render queues a protocol");
        wait_for_protocol(&mut viewer, fit);
        viewer.render(area, &mut buffer);
        assert_eq!(viewer.rendered.front().map(|(size, _)| *size), Some(fit));

        viewer.zoom_in();
        viewer.render(area, &mut buffer);
        let zoomed = viewer.requested.expect("zoom queues a new protocol");
        assert_ne!(zoomed, fit);
        wait_for_protocol(&mut viewer, zoomed);

        viewer.zoom_out();
        viewer.render(area, &mut buffer);
        assert_eq!(viewer.rendered.front().map(|(size, _)| *size), Some(fit));
        assert_ne!(viewer.requested, Some(fit), "cached zoom must be reused");
    }

    fn wait_for_protocol(viewer: &mut ImageViewer, request: ViewerRenderRequest) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !viewer
            .rendered
            .iter()
            .any(|(rendered_request, _)| *rendered_request == request)
        {
            viewer.collect_render_results(request);
            assert!(
                std::time::Instant::now() < deadline,
                "timed out building image protocol for {request:?}"
            );
            std::thread::yield_now();
        }
    }

    #[test]
    fn remote_urls_are_not_synchronously_previewed() {
        let content = [InputContent::InputImage(InputImageContent {
            detail: ImageDetail::Auto,
            file_id: None,
            image_url: Some("https://example.test/image.png".into()),
        })];
        assert!(content_preview_lines(&content, 80).is_empty());
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
        let placements = render_protocol_images(area, &mut buffer);
        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].area.y, area.y);
        assert_eq!(placements[0].area.height, area.height);

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
