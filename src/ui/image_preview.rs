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
use image::{DynamicImage, GenericImageView, ImageEncoder};
use ratatui::buffer::{Buffer, CellDiffOption};
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
use std::num::NonZeroU16;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

const MAX_PREVIEW_COLUMNS: u32 = 64;
const MAX_PREVIEW_ROWS: u32 = 18;
const PREVIEW_INDENT: &str = "      ";

const MARKER_START: u32 = 0xE000;
const MARKER_END: u32 = 0xF8FF;
const MAX_CACHED_IMAGE_SOURCES: usize = 256;
const MAX_CACHED_IMAGE_BYTES: usize = 256 * 1024 * 1024;
const MAX_RENDERED_IMAGES: usize = 256;
const MAX_RENDERED_IMAGE_BYTES: usize = 64 * 1024 * 1024;
const CACHE_LIMIT_MESSAGE: &str = "      [Image preview unavailable: cache limit reached]";
const PREVIEW_UNAVAILABLE_MESSAGE: &str = "      [Image preview unavailable]";
const KITTY_PLACEMENT_ID: u32 = 1;
const KITTY_PAYLOAD_BYTES: usize = 3_072;
const KITTY_UPLOAD_CHUNKS_PER_FRAME: usize = 16;
const MAX_KITTY_UPLOAD_SOURCE_BYTES: usize = 64 * 1024 * 1024;
const MAX_KITTY_UPLOAD_PNG_BYTES: usize = 65 * 1024 * 1024;
static PICKER: OnceLock<Picker> = OnceLock::new();
static KITTY_TERMINAL_STATE: OnceLock<Mutex<KittyTerminalState>> = OnceLock::new();
static KITTY_ENCODER: OnceLock<KittyEncoder> = OnceLock::new();

/// Images decoded and rendered for one conversation panel. Keeping this state
/// with the panel gives previews the same lifetime as their cached paragraphs
/// instead of leaking image data across sessions and sub-agent views.
#[derive(Default)]
pub(crate) struct ImagePreviewStore {
    registry: Mutex<ImageRegistry>,
}

impl std::fmt::Debug for ImagePreviewStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let registry = self.registry.lock().unwrap();
        formatter
            .debug_struct("ImagePreviewStore")
            .field("sources", &registry.sources.len())
            .field("images", &registry.images.len())
            .field("decoded_bytes", &registry.decoded_bytes)
            .field("rendered_bytes", &registry.rendered_bytes)
            .finish()
    }
}

#[derive(Default)]
struct ImageRegistry {
    next_marker: u32,
    sources: HashMap<blake3::Hash, Arc<DynamicImage>>,
    decoded_bytes: usize,
    rendered_bytes: usize,
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

fn rendered_image_bytes(size: Size) -> Option<usize> {
    let font = picker().font_size();
    let pixel_count = usize::from(size.width)
        .checked_mul(usize::from(font.width.max(1)))?
        .checked_mul(usize::from(size.height))?
        .checked_mul(usize::from(font.height.max(1)))?;
    // Native terminal protocols retain encoded strings derived from the target
    // pixel grid. This intentionally over-reserves compared with RGBA/base64
    // so protocol-specific framing and allocator overhead remain inside the
    // aggregate budget.
    pixel_count.checked_mul(64)
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
    obsolete_request: Option<ViewerRenderRequest>,
    failed_target: Option<ViewerRenderRequest>,
    kitty: Option<KittyViewerImage>,
    renderable: bool,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ViewerSourceWindow {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

struct KittyViewerImage {
    image_id: u32,
    source_size: (u32, u32),
    is_tmux: bool,
    upload: Receiver<KittyUploadEvent>,
    cancelled: Arc<AtomicBool>,
    upload_started: bool,
    uploaded: bool,
    failed: bool,
    release_upload_on_next_render: bool,
}

#[derive(Default)]
struct KittyTerminalState {
    active_upload: Option<u32>,
    cleanup: Vec<String>,
}

struct KittyEncodeTask {
    source: Arc<DynamicImage>,
    image_id: u32,
    is_tmux: bool,
    cancelled: Arc<AtomicBool>,
    result: SyncSender<KittyUploadEvent>,
}

struct KittyEncoder {
    state: Arc<(Mutex<KittyEncoderState>, Condvar)>,
}

#[derive(Default)]
struct KittyEncoderState {
    active: Option<Arc<AtomicBool>>,
    pending: Option<KittyEncodeTask>,
}

impl KittyEncoderState {
    fn replace_pending(&mut self, task: KittyEncodeTask) {
        if let Some(active) = &self.active {
            active.store(true, Ordering::Release);
        }
        if let Some(obsolete) = self.pending.replace(task) {
            obsolete.cancelled.store(true, Ordering::Release);
        }
    }
}

impl KittyEncoder {
    fn new() -> Self {
        let state = Arc::new((Mutex::new(KittyEncoderState::default()), Condvar::new()));
        let worker_state = state.clone();
        std::thread::Builder::new()
            .name("kitty-image-encoder".into())
            .spawn(move || {
                loop {
                    let task = {
                        let (state, ready) = &*worker_state;
                        let mut state = state.lock().unwrap();
                        while state.pending.is_none() {
                            state = ready.wait(state).unwrap();
                        }
                        let task = state.pending.take().expect("pending Kitty encoder task");
                        state.active = Some(task.cancelled.clone());
                        task
                    };
                    if !task.cancelled.load(Ordering::Acquire)
                        && encode_kitty_upload(
                            &task.source,
                            task.image_id,
                            task.is_tmux,
                            &task.cancelled,
                            task.result.clone(),
                        )
                        .is_none()
                        && !task.cancelled.load(Ordering::Acquire)
                    {
                        let _ = task.result.try_send(KittyUploadEvent::Failed);
                    }
                    let (state, _) = &*worker_state;
                    let mut state = state.lock().unwrap();
                    if state
                        .active
                        .as_ref()
                        .is_some_and(|active| Arc::ptr_eq(active, &task.cancelled))
                    {
                        state.active = None;
                    }
                }
            })
            .expect("failed to start Kitty image encoder");
        Self { state }
    }

    fn submit(
        &self,
        source: Arc<DynamicImage>,
        image_id: u32,
        is_tmux: bool,
    ) -> (Receiver<KittyUploadEvent>, Arc<AtomicBool>) {
        // Back-pressure limits buffered base64 to about one megabyte even if
        // the UI is temporarily unable to draw.
        let (result, upload) = std::sync::mpsc::sync_channel(256);
        let cancelled = Arc::new(AtomicBool::new(false));
        let task = KittyEncodeTask {
            source,
            image_id,
            is_tmux,
            cancelled: cancelled.clone(),
            result,
        };
        let (state, ready) = &*self.state;
        let mut state = state.lock().unwrap();
        state.replace_pending(task);
        ready.notify_one();
        drop(state);
        (upload, cancelled)
    }
}

enum KittyUploadEvent {
    Chunk(String),
    Complete,
    Failed,
}

struct KittyPngWriter<'a> {
    image_id: u32,
    is_tmux: bool,
    cancelled: &'a AtomicBool,
    result: SyncSender<KittyUploadEvent>,
    pending: Vec<u8>,
    first_chunk: bool,
    png_bytes: usize,
}

impl<'a> KittyPngWriter<'a> {
    fn new(
        image_id: u32,
        is_tmux: bool,
        cancelled: &'a AtomicBool,
        result: SyncSender<KittyUploadEvent>,
    ) -> Self {
        Self {
            image_id,
            is_tmux,
            cancelled,
            result,
            pending: Vec::with_capacity(KITTY_PAYLOAD_BYTES),
            first_chunk: true,
            png_bytes: 0,
        }
    }

    fn finish(mut self) -> std::io::Result<()> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "Kitty image encoding cancelled",
            ));
        }
        if self.pending.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "PNG encoder produced no data",
            ));
        }
        self.emit_pending(false)?;
        self.send_event(KittyUploadEvent::Complete)
    }

    fn send_event(&self, mut event: KittyUploadEvent) -> std::io::Result<()> {
        loop {
            if self.cancelled.load(Ordering::Acquire) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "Kitty image encoding cancelled",
                ));
            }
            match self.result.try_send(event) {
                Ok(()) => return Ok(()),
                Err(TrySendError::Full(returned)) => {
                    event = returned;
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                Err(TrySendError::Disconnected(_)) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "viewer closed",
                    ));
                }
            }
        }
    }

    fn emit_pending(&mut self, more: bool) -> std::io::Result<()> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "Kitty image encoding cancelled",
            ));
        }
        let (start, escape, end) = Parser::tmux_start_escape_end(self.is_tmux);
        let mut sequence = String::with_capacity(4_192);
        sequence.push_str(start);
        if self.first_chunk {
            write!(
                sequence,
                "{escape}_Ga=t,q=2,i={},f=100,t=d,m={};",
                self.image_id,
                u8::from(more)
            )
            .map_err(std::io::Error::other)?;
        } else {
            write!(sequence, "{escape}_Gq=2,m={};", u8::from(more))
                .map_err(std::io::Error::other)?;
        }
        base64::engine::general_purpose::STANDARD.encode_string(&self.pending, &mut sequence);
        write!(sequence, "{escape}\\{end}").map_err(std::io::Error::other)?;
        self.send_event(KittyUploadEvent::Chunk(sequence))?;
        self.first_chunk = false;
        self.pending.clear();
        Ok(())
    }
}

impl std::io::Write for KittyPngWriter<'_> {
    fn write(&mut self, mut bytes: &[u8]) -> std::io::Result<usize> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "Kitty image encoding cancelled",
            ));
        }
        let written = bytes.len();
        self.png_bytes = self
            .png_bytes
            .checked_add(written)
            .filter(|total| *total <= MAX_KITTY_UPLOAD_PNG_BYTES)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::FileTooLarge,
                    "Kitty PNG upload exceeds the memory limit",
                )
            })?;
        while !bytes.is_empty() {
            if self.pending.len() == KITTY_PAYLOAD_BYTES {
                self.emit_pending(true)?;
            }
            let remaining = KITTY_PAYLOAD_BYTES - self.pending.len();
            let take = remaining.min(bytes.len());
            self.pending.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
        }
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn kitty_terminal_state() -> &'static Mutex<KittyTerminalState> {
    KITTY_TERMINAL_STATE.get_or_init(|| Mutex::new(KittyTerminalState::default()))
}

fn claim_kitty_upload(image_id: u32) -> bool {
    let mut state = kitty_terminal_state().lock().unwrap();
    match state.active_upload {
        Some(active_image_id) => active_image_id == image_id,
        None if state.cleanup.is_empty() => {
            state.active_upload = Some(image_id);
            true
        }
        None => false,
    }
}

fn release_kitty_upload(image_id: u32) {
    let mut state = kitty_terminal_state().lock().unwrap();
    if state.active_upload == Some(image_id) {
        state.active_upload = None;
    }
}

impl KittyViewerImage {
    fn new(source: Arc<DynamicImage>) -> Self {
        let image_id = kitty_image_id();
        let source_size = source.dimensions();
        let is_tmux = running_in_tmux();
        let (upload, cancelled) = KITTY_ENCODER
            .get_or_init(KittyEncoder::new)
            .submit(source, image_id, is_tmux);
        Self {
            image_id,
            source_size,
            is_tmux,
            upload,
            cancelled,
            upload_started: false,
            uploaded: false,
            failed: false,
            release_upload_on_next_render: false,
        }
    }

    fn poll_upload(&mut self) -> String {
        if self.uploaded || self.failed {
            return String::new();
        }
        let mut commands = String::new();
        for _ in 0..KITTY_UPLOAD_CHUNKS_PER_FRAME {
            match self.upload.try_recv() {
                Ok(KittyUploadEvent::Chunk(command)) => commands.push_str(&command),
                Ok(KittyUploadEvent::Complete) => {
                    self.uploaded = true;
                    self.release_upload_on_next_render = true;
                    break;
                }
                Ok(KittyUploadEvent::Failed) | Err(TryRecvError::Disconnected) => {
                    self.failed = true;
                    if self.upload_started || !commands.is_empty() {
                        commands.push_str(&kitty_abort_upload(self.is_tmux));
                        self.upload_started = false;
                    }
                    self.release_upload_on_next_render = true;
                    break;
                }
                Err(TryRecvError::Empty) => break,
            }
        }
        commands
    }

    fn pending(&self) -> bool {
        (!self.uploaded && !self.failed) || self.release_upload_on_next_render
    }

    fn release_completed_upload(&mut self) {
        if self.release_upload_on_next_render {
            release_kitty_upload(self.image_id);
            self.release_upload_on_next_render = false;
        }
    }

    fn render(&mut self, area: Rect, request: ViewerRenderRequest, buf: &mut Buffer) -> bool {
        self.release_completed_upload();
        let Some(window) = viewer_source_bounds(self.source_size, request) else {
            return false;
        };
        let cells = request.visible_size();
        if !self.uploaded && !self.failed && !claim_kitty_upload(self.image_id) {
            return false;
        }
        let upload = self.poll_upload();
        let Some(command) = kitty_placement(
            self.image_id,
            KITTY_PLACEMENT_ID,
            self.is_tmux,
            window,
            cells,
        ) else {
            return false;
        };
        if !self.uploaded {
            if upload.is_empty() {
                return false;
            }
            if !self.failed {
                self.upload_started = true;
            }
            render_kitty_upload(&upload, area, buf);
            return true;
        }
        render_kitty_placement(&upload, &command, area, buf);
        true
    }
}

impl Drop for KittyViewerImage {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        let mut state = kitty_terminal_state().lock().unwrap();
        if state.active_upload == Some(self.image_id) {
            state.active_upload = None;
        }
        if !self.uploaded && !self.upload_started {
            return;
        }
        let mut command = String::new();
        if !self.uploaded {
            command.push_str(&kitty_abort_upload(self.is_tmux));
        }
        command.push_str(&kitty_delete_image(self.image_id, self.is_tmux));
        state.cleanup.push(command);
    }
}

fn kitty_image_id() -> u32 {
    let bytes = *uuid::Uuid::new_v4().as_bytes();
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]).max(1)
}

fn encode_kitty_upload(
    source: &DynamicImage,
    image_id: u32,
    is_tmux: bool,
    cancelled: &AtomicBool,
    result: SyncSender<KittyUploadEvent>,
) -> Option<()> {
    if source.width() == 0
        || source.height() == 0
        || source.as_bytes().len() > MAX_KITTY_UPLOAD_SOURCE_BYTES
        || cancelled.load(Ordering::Acquire)
    {
        return None;
    }
    let mut writer = KittyPngWriter::new(image_id, is_tmux, cancelled, result);
    image::codecs::png::PngEncoder::new(&mut writer)
        .write_image(
            source.as_bytes(),
            source.width(),
            source.height(),
            source.color().into(),
        )
        .ok()?;
    writer.finish().ok()
}

fn kitty_placement(
    image_id: u32,
    placement_id: u32,
    is_tmux: bool,
    source: ViewerSourceWindow,
    cells: Size,
) -> Option<String> {
    if cells == Size::ZERO {
        return None;
    }
    let (start, escape, end) = Parser::tmux_start_escape_end(is_tmux);
    Some(format!(
        "{start}{escape}_Ga=p,q=2,i={image_id},p={placement_id},x={},y={},w={},h={},c={},r={},C=1{escape}\\{end}",
        source.x, source.y, source.width, source.height, cells.width, cells.height
    ))
}

fn kitty_abort_upload(is_tmux: bool) -> String {
    let (start, escape, end) = Parser::tmux_start_escape_end(is_tmux);
    format!("{start}{escape}_Gq=2,m=0;{escape}\\{end}")
}

fn kitty_delete_image(image_id: u32, is_tmux: bool) -> String {
    let (start, escape, end) = Parser::tmux_start_escape_end(is_tmux);
    format!("{start}{escape}_Ga=d,d=I,i={image_id},q=2{escape}\\{end}")
}

fn take_kitty_cleanup() -> String {
    let mut state = kitty_terminal_state().lock().unwrap();
    if state.active_upload.is_some() {
        return String::new();
    }
    std::mem::take(&mut state.cleanup).concat()
}

pub(crate) fn render_kitty_cleanup(buf: &mut Buffer) {
    let area = buf.area;
    if area.is_empty() {
        return;
    }
    let cleanup = take_kitty_cleanup();
    if cleanup.is_empty() {
        return;
    }
    let Some(cell) = buf.cell_mut((area.x, area.y)) else {
        return;
    };
    let symbol = format!("{cleanup}{}", cell.symbol());
    cell.set_symbol(&symbol)
        .set_diff_option(CellDiffOption::ForcedWidth(
            NonZeroU16::new(1).expect("one is nonzero"),
        ));
}

pub(crate) fn write_kitty_cleanup(writer: &mut impl std::io::Write) -> std::io::Result<()> {
    let cleanup = take_kitty_cleanup();
    if cleanup.is_empty() {
        return Ok(());
    }
    writer.write_all(cleanup.as_bytes())?;
    writer.flush()
}

fn render_kitty_upload(upload: &str, area: Rect, buf: &mut Buffer) {
    let Some(cell) = buf.cell_mut((area.x, area.y)) else {
        return;
    };
    // Graphics commands do not move the cursor. Keep the terminal's physical
    // cursor aligned with the one-cell width reported to Ratatui.
    let symbol = format!("{upload}\x1b[1C");
    cell.set_symbol(&symbol)
        .set_diff_option(CellDiffOption::ForcedWidth(
            NonZeroU16::new(1).expect("one is nonzero"),
        ));
}

fn render_kitty_placement(upload: &str, placement: &str, area: Rect, buf: &mut Buffer) {
    let Some(cell) = buf.cell_mut((area.x, area.y)) else {
        return;
    };
    // C=1 keeps the placement command from moving the cursor. Advance one
    // physical cell to match the width Ratatui associates with this symbol.
    let symbol = format!("{upload}{placement}\x1b[1C");
    cell.set_symbol(&symbol)
        .set_diff_option(CellDiffOption::ForcedWidth(
            NonZeroU16::new(1).expect("one is nonzero"),
        ));
}

fn viewer_source_bounds(
    source_size: (u32, u32),
    request: ViewerRenderRequest,
) -> Option<ViewerSourceWindow> {
    let visible = request.visible_size();
    let (source_width, source_height) = source_size;
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
    (right > left && bottom > top).then_some(ViewerSourceWindow {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    })
}

fn viewer_source_window(
    source: &DynamicImage,
    request: ViewerRenderRequest,
) -> Option<DynamicImage> {
    let window = viewer_source_bounds(source.dimensions(), request)?;
    Some(source.crop_imm(window.x, window.y, window.width, window.height))
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
    running_in_tmux_environment(
        std::env::var("TERM").ok().as_deref(),
        std::env::var("TERM_PROGRAM").ok().as_deref(),
        std::env::var("TMUX").ok().as_deref(),
    )
}

fn running_in_tmux_environment(
    term: Option<&str>,
    term_program: Option<&str>,
    tmux: Option<&str>,
) -> bool {
    tmux.is_some_and(|value| !value.is_empty())
        || term.is_some_and(|value| value.starts_with("tmux"))
        || term_program == Some("tmux")
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
        let kitty = (picker().protocol_type() == ProtocolType::Kitty
            && source.as_bytes().len() <= MAX_KITTY_UPLOAD_SOURCE_BYTES)
            .then(|| KittyViewerImage::new(source.clone()));
        if kitty.is_none() {
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
        }
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
            obsolete_request: None,
            failed_target: None,
            kitty,
            renderable: false,
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
            if !self.drag_moved
                && self
                    .requested
                    .is_some_and(|request| request.quality == ViewerRenderQuality::Full)
            {
                // The encoder cannot cancel an in-flight resize, but this frame
                // predates the drag and must never be transmitted afterward.
                self.obsolete_request = self.requested;
            }
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
            if self.obsolete_request == Some(request) {
                self.obsolete_request = None;
                continue;
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
        if target.quality == ViewerRenderQuality::DragPreview
            && request.quality == ViewerRenderQuality::Full
        {
            // A full-quality native frame can contain megabytes of pixel data.
            // If dragging started while it was being encoded, transmitting the
            // now-obsolete frame would stall the terminal before the cheap
            // preview can appear. Drop it and submit the latest crop instead.
            return;
        }
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
        self.renderable
            && (self.kitty.as_ref().is_some_and(KittyViewerImage::pending)
                || self.requested.is_some())
    }

    pub(crate) fn requires_exclusive_render(&self) -> bool {
        self.kitty.as_ref().is_some_and(KittyViewerImage::pending)
    }

    pub(crate) fn render(&mut self, area: Rect, buf: &mut Buffer) {
        if let Some(kitty) = self.kitty.as_mut() {
            // Release after the frame containing the final chunk, even if the
            // terminal became too small to render the modal in this frame.
            kitty.release_completed_upload();
        }
        self.renderable = area.width >= 6 && area.height >= 4;
        if !self.renderable {
            self.viewport = Size::ZERO;
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
            self.renderable = false;
            self.viewport = Size::ZERO;
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
        self.renderable = canvas != Size::ZERO;
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
        let zoom_label = self.zoom_label(canvas);
        if let Some(kitty) = self.kitty.as_mut() {
            let title = format!(" Image · {zoom_label} ");
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
            let visible = target.visible_size();
            let render_area = Rect::new(
                image_area.x + image_area.width.saturating_sub(visible.width) / 2,
                image_area.y + image_area.height.saturating_sub(visible.height) / 2,
                visible.width,
                visible.height,
            );
            if !kitty.render(render_area, target, buf) {
                Paragraph::new(if kitty.failed {
                    "Image unavailable"
                } else {
                    "Rendering image…"
                })
                .style(Style::default().fg(crate::ui::markdown_theme::palette::MUTED))
                .render(image_area, buf);
            }
            return;
        }

        self.collect_render_results(target);

        let cached = self
            .rendered
            .iter()
            .position(|(cached_request, _)| *cached_request == target);
        if cached.is_none()
            && canvas != Size::ZERO
            && self.requested.is_none()
            && self.failed_target != Some(target)
            && self.render_requests.send(target).is_ok()
        {
            // Keep at most one expensive encoder job in flight. Pointer events
            // may outpace native image encoding by hundreds of frames; once this
            // job completes, the next render submits the latest target instead
            // of replaying every obsolete crop in between.
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
        // A completed protocol can belong to the previous terminal size while
        // the replacement is still rendering. Never subtract the stale size
        // directly from the smaller viewport: u16 underflow would panic in
        // debug builds and wrap in release builds.
        let x = (image_area.width.saturating_sub(visible.width) / 2) as i16;
        let y = (image_area.height.saturating_sub(visible.height) / 2) as i16;
        protocol.render(image_area, SignedPosition::from((x, y)), buf);
    }
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

impl ImagePreviewStore {
    pub(crate) fn clear(&self) {
        *self.registry.lock().unwrap() = ImageRegistry::default();
    }

    pub(crate) fn viewer(&self, image_id: usize) -> Option<ImageViewer> {
        self.registry
            .lock()
            .ok()?
            .images
            .get(image_id)
            .map(|image| ImageViewer::new(image.source.clone()))
    }

    pub(crate) fn preview_lines(
        &self,
        output: &FunctionCallOutput,
        available_width: u16,
    ) -> Vec<Line<'static>> {
        let FunctionCallOutput::Content(content) = output else {
            return Vec::new();
        };
        self.content_preview_lines(content, available_width)
    }

    pub(crate) fn content_preview_lines(
        &self,
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
            lines.extend(self.data_url_lines(url, available_width));
        }

        lines
    }

    fn data_url_lines(&self, url: &str, available_width: u16) -> Vec<Line<'static>> {
        if !url.starts_with("data:image/") {
            return Vec::new();
        }
        let source_key = blake3::hash(url.as_bytes());
        let mut registry = self.registry.lock().unwrap();
        let source = if let Some(source) = registry.sources.get(&source_key) {
            source.clone()
        } else {
            if registry.sources.len() >= MAX_CACHED_IMAGE_SOURCES
                || registry.decoded_bytes >= MAX_CACHED_IMAGE_BYTES
            {
                return preview_message(CACHE_LIMIT_MESSAGE);
            }
            // Keep admission and decoding under the store lock. Rendering is
            // single-threaded today, but the store is shareable; serializing
            // this step prevents concurrent decoders from each claiming the
            // same remaining budget and creating a transient memory spike.
            let decode_budget = MAX_CACHED_IMAGE_BYTES - registry.decoded_bytes;
            let image = match decode_data_url(url, decode_budget) {
                Ok(image) => image,
                Err(DecodeDataUrlError::Limit) => return preview_message(CACHE_LIMIT_MESSAGE),
                Err(DecodeDataUrlError::Invalid) => {
                    return preview_message(PREVIEW_UNAVAILABLE_MESSAGE);
                }
            };
            let decoded_bytes = image.as_bytes().len();
            if registry
                .decoded_bytes
                .checked_add(decoded_bytes)
                .is_none_or(|total| total > MAX_CACHED_IMAGE_BYTES)
            {
                return preview_message(CACHE_LIMIT_MESSAGE);
            }
            let source = Arc::new(image);
            registry.decoded_bytes += decoded_bytes;
            registry.sources.insert(source_key, source.clone());
            source
        };
        drop(registry);
        self.render_image_lines(source_key, source, available_width)
    }
}

fn preview_message(message: &'static str) -> Vec<Line<'static>> {
    vec![Line::raw(message)]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DecodeDataUrlError {
    Invalid,
    Limit,
}

fn decode_data_url(
    url: &str,
    max_decoded_bytes: usize,
) -> Result<DynamicImage, DecodeDataUrlError> {
    let (header, encoded) = url.split_once(',').ok_or(DecodeDataUrlError::Invalid)?;
    if !header.starts_with("data:image/") || !header.ends_with(";base64") {
        return Err(DecodeDataUrlError::Invalid);
    }
    let max_encoded_length = (crate::commands::MAX_IMAGE_BYTES as usize).div_ceil(3) * 4;
    if encoded.len() > max_encoded_length {
        return Err(DecodeDataUrlError::Limit);
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| DecodeDataUrlError::Invalid)?;
    if bytes.len() as u64 > crate::commands::MAX_IMAGE_BYTES {
        return Err(DecodeDataUrlError::Limit);
    }

    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| DecodeDataUrlError::Invalid)?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(max_decoded_bytes as u64);
    reader.limits(limits);
    reader.decode().map_err(|error| match error {
        image::ImageError::Limits(_) => DecodeDataUrlError::Limit,
        _ => DecodeDataUrlError::Invalid,
    })
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

impl ImagePreviewStore {
    fn render_image_lines(
        &self,
        source_key: blake3::Hash,
        source: Arc<DynamicImage>,
        available_width: u16,
    ) -> Vec<Line<'static>> {
        let size = preview_size(&source, available_width);
        if size == Size::ZERO {
            return preview_message(PREVIEW_UNAVAILABLE_MESSAGE);
        }
        let key = (source_key, size.width, size.height);
        let mut registry = self.registry.lock().unwrap();
        let image_id = if let Some(image_id) = registry.by_key.get(&key) {
            *image_id
        } else {
            let marker_end = MARKER_START + registry.next_marker + u32::from(size.height);
            let Some(rendered_bytes) = rendered_image_bytes(size) else {
                return preview_message(CACHE_LIMIT_MESSAGE);
            };
            if registry.images.len() >= MAX_RENDERED_IMAGES
                || marker_end > MARKER_END + 1
                || registry
                    .rendered_bytes
                    .checked_add(rendered_bytes)
                    .is_none_or(|total| total > MAX_RENDERED_IMAGE_BYTES)
            {
                return preview_message(CACHE_LIMIT_MESSAGE);
            }
            let Some(protocol) = RenderedImage::new(picker(), &source, size, Resize::Fit(None))
            else {
                return preview_message(PREVIEW_UNAVAILABLE_MESSAGE);
            };
            let image_id = registry.images.len();
            let markers = (0..size.height)
                .map(|row| {
                    let marker =
                        char::from_u32(MARKER_START + registry.next_marker + u32::from(row))
                            .expect("private-use marker is valid");
                    registry.markers.insert(marker, (image_id, row));
                    marker
                })
                .collect();
            registry.next_marker += u32::from(size.height);
            registry.rendered_bytes += rendered_bytes;
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
    pub(crate) fn render_protocol_images(
        &self,
        area: Rect,
        buf: &mut Buffer,
    ) -> Vec<ImagePlacement> {
        let registry = self.registry.lock().unwrap();
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_openai::types::responses::{ImageDetail, InputImageContent};
    use base64::engine::general_purpose::STANDARD;
    use image::{ImageBuffer, ImageFormat, Rgb};
    use std::io::Cursor;

    static KITTY_TERMINAL_TEST_LOCK: Mutex<()> = Mutex::new(());

    struct KittyTerminalTestGuard {
        _serial: std::sync::MutexGuard<'static, ()>,
    }

    impl Drop for KittyTerminalTestGuard {
        fn drop(&mut self) {
            *kitty_terminal_state().lock().unwrap() = KittyTerminalState::default();
        }
    }

    fn isolated_kitty_terminal_state() -> KittyTerminalTestGuard {
        let serial = KITTY_TERMINAL_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *kitty_terminal_state().lock().unwrap() = KittyTerminalState::default();
        KittyTerminalTestGuard { _serial: serial }
    }

    fn png_with_declared_dimensions(width: u32, height: u32) -> Vec<u8> {
        let image = ImageBuffer::from_pixel(1, 1, Rgb([0u8, 0, 0]));
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, ImageFormat::Png).unwrap();
        let mut bytes = bytes.into_inner();
        bytes[16..20].copy_from_slice(&width.to_be_bytes());
        bytes[20..24].copy_from_slice(&height.to_be_bytes());
        let checksum = png_crc32(&bytes[12..29]);
        bytes[29..33].copy_from_slice(&checksum.to_be_bytes());
        bytes
    }

    fn png_crc32(bytes: &[u8]) -> u32 {
        let mut checksum = u32::MAX;
        for &byte in bytes {
            checksum ^= u32::from(byte);
            for _ in 0..8 {
                let mask = 0_u32.wrapping_sub(checksum & 1);
                checksum = (checksum >> 1) ^ (0xEDB8_8320 & mask);
            }
        }
        !checksum
    }

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
        let image_previews = ImagePreviewStore::default();
        let lines = image_previews.preview_lines(&output(), 80);
        assert_eq!(lines.len(), 1);
        let marker = lines[0].spans[1].content.chars().next().unwrap();
        assert!((MARKER_START..=MARKER_END).contains(&(marker as u32)));

        let area = Rect::new(0, 0, 80, 4);
        let mut buffer = Buffer::empty(area);
        ratatui::widgets::Paragraph::new(lines).render(area, &mut buffer);
        let _ = image_previews.render_protocol_images(area, &mut buffer);
        assert!((0..area.width).all(|x| buffer[(x, 0)].symbol() != marker.to_string()));
    }

    #[test]
    fn identical_visible_images_keep_distinct_hit_placements() {
        let image_previews = ImagePreviewStore::default();
        let lines = image_previews.preview_lines(&output(), 80);
        let mut doubled = lines.clone();
        doubled.push(Line::default());
        doubled.extend(lines);
        let area = Rect::new(0, 0, 80, 4);
        let mut buffer = Buffer::empty(area);
        ratatui::widgets::Paragraph::new(doubled).render(area, &mut buffer);

        let mut placements = image_previews.render_protocol_images(area, &mut buffer);
        placements.sort_unstable_by_key(|placement| placement.area.y);
        assert_eq!(placements.len(), 2);
        assert_eq!(placements[0].image_id, placements[1].image_id);
        assert_ne!(placements[0].area.y, placements[1].area.y);
    }

    #[test]
    fn preview_stores_are_isolated_and_clear_releases_all_registry_state() {
        let first = ImagePreviewStore::default();
        let second = ImagePreviewStore::default();

        assert!(!first.preview_lines(&output(), 80).is_empty());
        {
            let first_registry = first.registry.lock().unwrap();
            assert_eq!(first_registry.sources.len(), 1);
            assert_eq!(first_registry.images.len(), 1);
            assert!(!first_registry.markers.is_empty());
            assert!(first_registry.decoded_bytes > 0);
        }
        assert!(second.registry.lock().unwrap().sources.is_empty());

        assert!(!second.preview_lines(&output(), 80).is_empty());
        first.clear();

        let first_registry = first.registry.lock().unwrap();
        assert!(first_registry.sources.is_empty());
        assert!(first_registry.images.is_empty());
        assert!(first_registry.by_key.is_empty());
        assert!(first_registry.markers.is_empty());
        assert_eq!(first_registry.next_marker, 0);
        assert_eq!(first_registry.decoded_bytes, 0);
        assert_eq!(first_registry.rendered_bytes, 0);
        drop(first_registry);

        assert!(first.viewer(0).is_none());
        assert!(second.viewer(0).is_some());
    }

    #[test]
    fn declared_dimensions_are_limited_before_the_full_image_is_decoded() {
        let bytes = png_with_declared_dimensions(100_000, 100_000);
        assert!(
            bytes.len() < 1_000,
            "test fixture must stay highly compressed"
        );
        let url = format!("data:image/png;base64,{}", STANDARD.encode(bytes));

        assert!(matches!(
            decode_data_url(&url, 1024 * 1024),
            Err(DecodeDataUrlError::Limit)
        ));
    }

    #[test]
    fn source_and_decoded_byte_limits_report_an_explicit_placeholder() {
        let FunctionCallOutput::Content(content) = output() else {
            unreachable!("test output is multimodal");
        };
        let url = content
            .iter()
            .find_map(|part| match part {
                InputContent::InputImage(image) => image.image_url.as_deref(),
                _ => None,
            })
            .expect("test image has a data URL");
        let source_key = blake3::hash(url.as_bytes());

        let source_limited = ImagePreviewStore::default();
        {
            let placeholder = Arc::new(DynamicImage::ImageRgb8(ImageBuffer::from_pixel(
                1,
                1,
                Rgb([0, 0, 0]),
            )));
            let mut registry = source_limited.registry.lock().unwrap();
            let mut index = 0_u64;
            while registry.sources.len() < MAX_CACHED_IMAGE_SOURCES {
                let key = blake3::hash(&index.to_le_bytes());
                if key != source_key {
                    registry.sources.insert(key, placeholder.clone());
                }
                index += 1;
            }
            registry.decoded_bytes = registry.sources.len() * placeholder.as_bytes().len();
        }
        let lines = source_limited.data_url_lines(url, 80);
        assert_eq!(lines[0].spans[0].content, CACHE_LIMIT_MESSAGE);

        let byte_limited = ImagePreviewStore::default();
        byte_limited.registry.lock().unwrap().decoded_bytes = MAX_CACHED_IMAGE_BYTES;
        let lines = byte_limited.data_url_lines(url, 80);
        assert_eq!(lines[0].spans[0].content, CACHE_LIMIT_MESSAGE);
    }

    #[test]
    fn rendered_image_and_marker_limits_report_an_explicit_placeholder() {
        let source = Arc::new(DynamicImage::ImageRgb8(ImageBuffer::from_pixel(
            1,
            1,
            Rgb([0, 0, 0]),
        )));
        let render_limited = ImagePreviewStore::default();
        for index in 0..MAX_RENDERED_IMAGES {
            let lines = render_limited.render_image_lines(
                blake3::hash(&index.to_le_bytes()),
                source.clone(),
                80,
            );
            assert_ne!(lines[0].spans[0].content, CACHE_LIMIT_MESSAGE);
        }
        let lines = render_limited.render_image_lines(
            blake3::hash(&MAX_RENDERED_IMAGES.to_le_bytes()),
            source.clone(),
            80,
        );
        assert_eq!(lines[0].spans[0].content, CACHE_LIMIT_MESSAGE);

        let byte_limited = ImagePreviewStore::default();
        byte_limited.registry.lock().unwrap().rendered_bytes = MAX_RENDERED_IMAGE_BYTES;
        let lines = byte_limited.render_image_lines(
            blake3::hash(b"rendered-byte-limit"),
            source.clone(),
            80,
        );
        assert_eq!(lines[0].spans[0].content, CACHE_LIMIT_MESSAGE);

        let marker_limited = ImagePreviewStore::default();
        marker_limited.registry.lock().unwrap().next_marker = MARKER_END - MARKER_START + 1;
        let lines = marker_limited.render_image_lines(blake3::hash(b"marker-limit"), source, 80);
        assert_eq!(lines[0].spans[0].content, CACHE_LIMIT_MESSAGE);
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
    }

    fn encoded_kitty_upload(source: &DynamicImage, image_id: u32) -> Vec<KittyUploadEvent> {
        let cancelled = AtomicBool::new(false);
        let (sender, receiver) = std::sync::mpsc::sync_channel(256);
        encode_kitty_upload(source, image_id, false, &cancelled, sender)
            .expect("Kitty upload encodes");
        receiver.try_iter().collect()
    }

    #[test]
    fn kitty_upload_streams_one_chunked_png_transmission() {
        let source = DynamicImage::ImageRgb8(ImageBuffer::from_fn(64, 48, |x, y| {
            Rgb([x as u8, y as u8, (x + y) as u8])
        }));
        let events = encoded_kitty_upload(&source, 42);
        assert!(matches!(events.last(), Some(KittyUploadEvent::Complete)));
        let commands = events
            .iter()
            .filter_map(|event| match event {
                KittyUploadEvent::Chunk(command) => Some(command.as_str()),
                KittyUploadEvent::Complete | KittyUploadEvent::Failed => None,
            })
            .collect::<Vec<_>>();
        let sequence = commands.concat();

        assert_eq!(sequence.matches("a=t").count(), 1);
        assert_eq!(sequence.matches("f=100").count(), 1);
        assert!(!sequence.contains("a=T"));

        let mut png = Vec::new();
        for command in commands {
            let (_, payload) = command.split_once(';').expect("Kitty command has payload");
            let payload = payload.strip_suffix("\x1b\\").unwrap();
            assert!(payload.len() <= 4_096);
            png.extend(STANDARD.decode(payload).expect("payload is base64"));
        }
        assert_eq!(
            image::load_from_memory(&png).unwrap().dimensions(),
            (64, 48)
        );
    }

    #[test]
    fn cancelled_kitty_upload_produces_no_chunks() {
        let source = DynamicImage::ImageRgb8(ImageBuffer::from_pixel(2, 2, Rgb([0, 0, 0])));
        let cancelled = AtomicBool::new(true);
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);

        assert!(encode_kitty_upload(&source, 42, false, &cancelled, sender).is_none());
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn full_kitty_upload_channel_still_observes_cancellation() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        sender.send(KittyUploadEvent::Failed).unwrap();
        let thread_cancelled = cancelled.clone();
        let worker = std::thread::spawn(move || {
            let writer = KittyPngWriter::new(42, false, &thread_cancelled, sender);
            writer
                .send_event(KittyUploadEvent::Complete)
                .expect_err("cancellation interrupts a full upload channel")
                .kind()
        });

        std::thread::sleep(std::time::Duration::from_millis(10));
        assert!(!worker.is_finished());
        cancelled.store(true, Ordering::Release);

        assert_eq!(worker.join().unwrap(), std::io::ErrorKind::Interrupted);
        drop(receiver);
    }

    #[test]
    fn kitty_encoder_cancels_active_and_replaces_pending_task() {
        fn task(image_id: u32) -> (KittyEncodeTask, Arc<AtomicBool>) {
            let (result, _upload) = std::sync::mpsc::sync_channel(1);
            let cancelled = Arc::new(AtomicBool::new(false));
            (
                KittyEncodeTask {
                    source: Arc::new(DynamicImage::new_rgb8(1, 1)),
                    image_id,
                    is_tmux: false,
                    cancelled: cancelled.clone(),
                    result,
                },
                cancelled,
            )
        }

        let active = Arc::new(AtomicBool::new(false));
        let (obsolete, obsolete_cancelled) = task(1);
        let (latest, latest_cancelled) = task(2);
        let mut state = KittyEncoderState {
            active: Some(active.clone()),
            pending: Some(obsolete),
        };

        state.replace_pending(latest);

        assert!(active.load(Ordering::Acquire));
        assert!(obsolete_cancelled.load(Ordering::Acquire));
        assert!(!latest_cancelled.load(Ordering::Acquire));
        assert_eq!(state.pending.as_ref().unwrap().image_id, 2);
    }

    #[test]
    fn kitty_upload_polling_is_limited_to_sixteen_chunks_per_frame() {
        let _terminal_state = isolated_kitty_terminal_state();
        let (upload_sender, upload) = std::sync::mpsc::sync_channel(17);
        for index in 0..17 {
            upload_sender
                .send(KittyUploadEvent::Chunk(format!("[{index}]")))
                .unwrap();
        }
        let mut image = KittyViewerImage {
            image_id: 42,
            source_size: (1, 1),
            is_tmux: false,
            upload,
            cancelled: Arc::new(AtomicBool::new(false)),
            upload_started: false,
            uploaded: false,
            failed: false,
            release_upload_on_next_render: false,
        };

        let first_frame = image.poll_upload();
        let expected_first_frame = (0..16)
            .map(|index| format!("[{index}]"))
            .collect::<String>();
        assert_eq!(first_frame, expected_first_frame);
        assert_eq!(image.poll_upload(), "[16]");
        assert!(image.poll_upload().is_empty());
    }

    #[test]
    fn kitty_pan_replaces_one_native_placement_without_reuploading() {
        let _terminal_state = isolated_kitty_terminal_state();
        let source = DynamicImage::ImageRgb8(ImageBuffer::from_pixel(400, 200, Rgb([255, 0, 0])));
        let image_id = 0x1234_5678;
        let (upload_sender, upload) = std::sync::mpsc::sync_channel(256);
        let cancelled = Arc::new(AtomicBool::new(false));
        encode_kitty_upload(&source, image_id, false, &cancelled, upload_sender)
            .expect("Kitty upload encodes");
        let mut image = KittyViewerImage {
            image_id,
            source_size: source.dimensions(),
            is_tmux: false,
            upload,
            cancelled,
            upload_started: false,
            uploaded: false,
            failed: false,
            release_upload_on_next_render: false,
        };
        let area = Rect::new(0, 0, 20, 10);
        let mut first_buffer = Buffer::empty(area);
        let first = ViewerRenderRequest {
            canvas: Size::new(40, 20),
            viewport: area.as_size(),
            pan_x: 0,
            pan_y: 0,
            quality: ViewerRenderQuality::Full,
        };

        assert!(image.render(area, first, &mut first_buffer));
        let first_frame = first_buffer[(0, 0)].symbol().to_string();
        assert!(first_frame.contains("a=t"));
        assert!(first_frame.contains("f=100"));
        assert!(first_frame.contains("a=p"));
        assert!(first_frame.contains("p=1"));
        assert!(!first_frame.contains("U=1"));
        assert!(first_frame.contains("x=0,y=0,w=200,h=100,c=20,r=10"));
        assert_eq!(
            first_buffer[(0, 0)].diff_option,
            CellDiffOption::ForcedWidth(NonZeroU16::new(1).unwrap())
        );
        assert_eq!(first_buffer[(1, 0)].diff_option, CellDiffOption::None);
        assert!(
            image.pending(),
            "the completion frame schedules one exclusive release frame"
        );

        let second = ViewerRenderRequest { pan_x: 10, ..first };
        let mut second_buffer = Buffer::empty(area);
        assert!(image.render(area, second, &mut second_buffer));
        let second_frame = second_buffer[(0, 0)].symbol();
        assert!(!second_frame.contains("a=t"));
        assert!(!second_frame.contains("f=100"));
        assert!(second_frame.contains("a=p"));
        assert!(second_frame.contains("p=1"));
        assert!(second_frame.contains("x=100,y=0,w=200,h=100,c=20,r=10"));
        assert_eq!(first_buffer.diff(&second_buffer).len(), 1);
        assert!(!image.pending());

        image.uploaded = false;
    }

    #[test]
    fn kitty_cleanup_deletes_only_the_viewers_image() {
        assert_eq!(
            kitty_delete_image(42, false),
            "\x1b_Ga=d,d=I,i=42,q=2\x1b\\"
        );
    }

    #[test]
    fn zero_sized_buffer_does_not_discard_kitty_cleanup() {
        let _terminal_state = isolated_kitty_terminal_state();
        kitty_terminal_state()
            .lock()
            .unwrap()
            .cleanup
            .push("cleanup".into());
        render_kitty_cleanup(&mut Buffer::empty(Rect::ZERO));

        let mut buffer = Buffer::empty(Rect::new(0, 0, 1, 1));
        render_kitty_cleanup(&mut buffer);

        assert!(buffer[(0, 0)].symbol().starts_with("cleanup"));
    }

    #[test]
    fn active_kitty_upload_defers_cleanup_and_other_uploads() {
        let _terminal_state = isolated_kitty_terminal_state();
        let mut state = kitty_terminal_state().lock().unwrap();
        *state = KittyTerminalState::default();
        state.active_upload = Some(42);
        state.cleanup.push("cleanup".into());
        drop(state);

        assert!(claim_kitty_upload(42));
        assert!(!claim_kitty_upload(43));
        let mut buffer = Buffer::empty(Rect::new(0, 0, 1, 1));
        render_kitty_cleanup(&mut buffer);
        assert!(!buffer[(0, 0)].symbol().contains("cleanup"));

        release_kitty_upload(42);
        assert!(
            !claim_kitty_upload(43),
            "queued cleanup runs before a new upload"
        );
        render_kitty_cleanup(&mut buffer);
        assert!(buffer[(0, 0)].symbol().starts_with("cleanup"));
        assert!(claim_kitty_upload(43));
        release_kitty_upload(43);
    }

    #[test]
    fn kitty_placement_keeps_cursor_accounting_local() {
        let area = Rect::new(2, 1, 3, 2);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 6, 4));
        render_kitty_placement("upload", "placement", area, &mut buffer);

        let cell = &buffer[(area.x, area.y)];
        assert_eq!(cell.symbol(), "uploadplacement\x1b[1C");
        assert_eq!(
            cell.diff_option,
            CellDiffOption::ForcedWidth(NonZeroU16::new(1).unwrap())
        );
        assert_eq!(
            buffer[(area.x + 1, area.y)].diff_option,
            CellDiffOption::None
        );
    }

    #[test]
    fn tiny_viewport_does_not_remain_render_pending() {
        let source = Arc::new(DynamicImage::ImageRgb8(ImageBuffer::from_pixel(
            2,
            2,
            Rgb([0, 0, 0]),
        )));
        let mut viewer = ImageViewer::new(source);
        viewer.requested = Some(ViewerRenderRequest {
            canvas: Size::new(1, 1),
            viewport: Size::new(1, 1),
            pan_x: 0,
            pan_y: 0,
            quality: ViewerRenderQuality::Full,
        });
        viewer.renderable = true;
        assert!(viewer.render_pending());

        let mut buffer = Buffer::empty(Rect::new(0, 0, 5, 3));
        viewer.render(buffer.area, &mut buffer);

        assert!(!viewer.render_pending());
        assert_eq!(viewer.viewport, Size::ZERO);
    }

    #[test]
    fn tmux_environment_variable_enables_passthrough() {
        assert!(running_in_tmux_environment(
            Some("screen-256color"),
            None,
            Some("/tmp/tmux-501/default,1,0"),
        ));
        assert!(!running_in_tmux_environment(
            Some("screen-256color"),
            None,
            None,
        ));
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
    fn viewer_discards_an_obsolete_full_frame_after_dragging_starts() {
        let source = Arc::new(DynamicImage::ImageRgb8(ImageBuffer::from_pixel(
            200,
            200,
            Rgb([255, 0, 0]),
        )));
        let mut viewer = ImageViewer::new(source);
        let full_request = ViewerRenderRequest {
            canvas: Size::new(40, 20),
            viewport: Size::new(20, 10),
            pan_x: 0,
            pan_y: 0,
            quality: ViewerRenderQuality::Full,
        };
        let drag_target = ViewerRenderRequest {
            pan_x: 10,
            quality: ViewerRenderQuality::DragPreview,
            ..full_request
        };
        let full_protocol = RenderedImage::Halfblocks(HalfblockImage::new(
            &DynamicImage::ImageRgb8(ImageBuffer::from_pixel(2, 2, Rgb([255, 0, 0]))),
            full_request.visible_size(),
        ));

        viewer.cache_render_result(full_request, full_protocol, drag_target);

        assert!(viewer.rendered.is_empty());
    }

    #[test]
    fn viewer_discards_a_pre_drag_full_frame_even_after_mouse_up() {
        let source = Arc::new(DynamicImage::ImageRgb8(ImageBuffer::from_pixel(
            400,
            400,
            Rgb([255, 0, 0]),
        )));
        let (render_requests, _queued_requests) = std::sync::mpsc::channel();
        let (result_sender, render_results) = std::sync::mpsc::channel();
        let active_request = ViewerRenderRequest {
            canvas: Size::new(40, 20),
            viewport: Size::new(20, 10),
            pan_x: 0,
            pan_y: 0,
            quality: ViewerRenderQuality::Full,
        };
        let mut viewer = ImageViewer {
            source,
            zoom_index: DEFAULT_ZOOM_INDEX + 1,
            pan_x: 0,
            pan_y: 0,
            drag_at: None,
            drag_moved: false,
            rendered: VecDeque::new(),
            viewport: active_request.viewport,
            render_requests,
            render_results,
            requested: Some(active_request),
            obsolete_request: None,
            failed_target: None,
            kitty: None,
            renderable: true,
        };
        viewer.begin_drag(10, 5);
        viewer.drag_to(8, 5);
        assert_eq!(viewer.obsolete_request, Some(active_request));
        viewer.end_drag();
        let settled_target = ViewerRenderRequest {
            pan_x: viewer.pan_x,
            pan_y: viewer.pan_y,
            ..active_request
        };
        let obsolete_protocol = RenderedImage::Halfblocks(HalfblockImage::new(
            &DynamicImage::ImageRgb8(ImageBuffer::from_pixel(2, 2, Rgb([255, 0, 0]))),
            active_request.visible_size(),
        ));
        result_sender
            .send((active_request, Some(obsolete_protocol)))
            .unwrap();

        viewer.collect_render_results(settled_target);

        assert!(viewer.rendered.is_empty());
        assert_eq!(viewer.requested, None);
        assert_eq!(viewer.obsolete_request, None);
    }

    #[test]
    fn viewer_queues_only_the_latest_target_after_active_encoding_finishes() {
        let source = Arc::new(DynamicImage::ImageRgb8(ImageBuffer::from_pixel(
            200,
            200,
            Rgb([255, 0, 0]),
        )));
        let (render_requests, queued_requests) = std::sync::mpsc::channel();
        let (result_sender, render_results) = std::sync::mpsc::channel();
        let active_request = ViewerRenderRequest {
            canvas: Size::new(20, 10),
            viewport: Size::new(20, 10),
            pan_x: 0,
            pan_y: 0,
            quality: ViewerRenderQuality::Full,
        };
        let mut viewer = ImageViewer {
            source,
            zoom_index: DEFAULT_ZOOM_INDEX + 1,
            pan_x: 0,
            pan_y: 0,
            drag_at: None,
            drag_moved: false,
            rendered: VecDeque::new(),
            viewport: Size::ZERO,
            render_requests,
            render_results,
            requested: Some(active_request),
            obsolete_request: None,
            failed_target: None,
            kitty: None,
            renderable: true,
        };
        let mut buffer = Buffer::empty(Rect::new(0, 0, 42, 14));

        viewer.render(buffer.area, &mut buffer);

        assert_eq!(viewer.requested, Some(active_request));
        assert!(queued_requests.try_recv().is_err());

        let completed = RenderedImage::Halfblocks(HalfblockImage::new(
            &DynamicImage::ImageRgb8(ImageBuffer::from_pixel(2, 2, Rgb([255, 0, 0]))),
            active_request.visible_size(),
        ));
        result_sender
            .send((active_request, Some(completed)))
            .unwrap();
        viewer.render(buffer.area, &mut buffer);

        let latest = queued_requests.try_recv().expect("latest target queued");
        assert_eq!(viewer.requested, Some(latest));
        assert_ne!(latest, active_request);
        assert!(queued_requests.try_recv().is_err());
    }

    #[test]
    fn viewer_clips_a_cached_protocol_from_a_larger_terminal() {
        let source = Arc::new(DynamicImage::ImageRgb8(ImageBuffer::from_pixel(
            200,
            200,
            Rgb([255, 0, 0]),
        )));
        let mut viewer = ImageViewer::new(source);
        let stale_request = ViewerRenderRequest {
            canvas: Size::new(20, 10),
            viewport: Size::new(20, 10),
            pan_x: 0,
            pan_y: 0,
            quality: ViewerRenderQuality::Full,
        };
        let stale_protocol = RenderedImage::Halfblocks(HalfblockImage::new(
            &DynamicImage::ImageRgb8(ImageBuffer::from_pixel(2, 2, Rgb([255, 0, 0]))),
            stale_request.viewport,
        ));
        viewer.rendered.push_front((stale_request, stale_protocol));

        let area = Rect::new(0, 0, 12, 8);
        let viewport = Size::new(8, 3);
        let target = ViewerRenderRequest {
            canvas: viewer.target_size(viewport),
            viewport,
            pan_x: 0,
            pan_y: 0,
            quality: ViewerRenderQuality::Full,
        };
        viewer.requested = Some(target);
        let mut buffer = Buffer::empty(area);

        viewer.render(area, &mut buffer);

        assert_eq!(buffer[(2, 2)].bg, Color::Rgb(255, 0, 0));
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
        let image_previews = ImagePreviewStore::default();
        assert!(
            image_previews
                .content_preview_lines(&content, 80)
                .is_empty()
        );
    }

    #[test]
    fn partially_scrolled_image_still_renders() {
        let image_previews = ImagePreviewStore::default();
        let source = Arc::new(DynamicImage::ImageRgb8(ImageBuffer::from_pixel(
            40,
            160,
            Rgb([255, 0, 0]),
        )));
        let source_key = blake3::hash(source.as_bytes());
        let lines = image_previews.render_image_lines(source_key, source, 40);
        assert!(lines.len() > 2);

        // Simulate ScrollView clipping the first two image rows: the remaining
        // row markers must be enough to reconstruct the correct slice.
        let visible = lines.into_iter().skip(2).collect::<Vec<_>>();
        let area = Rect::new(0, 0, 40, visible.len() as u16);
        let mut buffer = Buffer::empty(area);
        ratatui::widgets::Paragraph::new(visible).render(area, &mut buffer);
        let placements = image_previews.render_protocol_images(area, &mut buffer);
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
