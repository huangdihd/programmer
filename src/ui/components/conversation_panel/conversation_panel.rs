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

use crate::response::message_item::MessageItem;
use crate::response::partial_response::PartialResponse;
use crate::prompts::SYSTEM_PROMPT;
use async_openai::error::OpenAIError;
use async_openai::types::responses::MessageItem as ApiMessageItem;
use async_openai::types::responses::{
    FunctionCallOutput, FunctionCallOutputItemParam, InputContent, InputItem, InputMessage,
    InputParam, InputRole, InputTextContent, Item, OutputItem, OutputStatus, ResponseStreamEvent,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;
use ratatui_widgets::paragraph::Paragraph;
use std::collections::{HashMap, HashSet};
use tui_scrollview::ScrollViewState;
use unicode_width::UnicodeWidthStr;

use crate::ui::components::messages::pending_message::PendingMessage;
use crate::ui::components::messages::welcome_message::WelcomeMessage;
use crate::ui::markdown_code_block::CodeCopyButton;

/// The active work phase of a turn. Exactly one is in effect at a time; the
/// old design tracked these as separate booleans that could, in principle,
/// contradict each other. "Thinking" is intentionally absent — it is derived
/// from [`ActivePhase::None`] plus an in-flight `receiving_response`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ActivePhase {
    /// Neither outputting, calling tools, nor classifying. When a response is
    /// still streaming this reads as "Thinking"; otherwise the turn is idle.
    #[default]
    None,
    /// The model is streaming a normal text message.
    Outputting,
    /// The model is streaming tool-call arguments.
    CreatingToolCall,
    /// Tool calls are executing in the background.
    ToolRunning,
    /// The Auto-mode LLM classifier is deciding tool-call approvals.
    Classifying,
    /// Diagnostics checkers are running after an edit.
    Checking,
}

/// Number of rows scrolled per mouse-wheel notch.
const SCROLL_LINES: usize = 3;

/// Renders one region (a message's vertical extent) into an off-screen buffer
/// and copies the selected rows' text into `lines` (indexed relative to
/// `sel_top`). Regions outside the selection are skipped without rendering.
fn extract_region(
    lines: &mut [String],
    sel: &Selection,
    sel_top: u16,
    top: u16,
    height: u16,
    width: u16,
    render: impl FnOnce(&mut Buffer),
) {
    if height == 0 || lines.is_empty() {
        return;
    }
    let bottom = top.saturating_add(height); // exclusive
    let sel_bottom = sel_top + (lines.len() - 1) as u16;
    if bottom <= sel_top || top > sel_bottom {
        return;
    }
    let mut buf = Buffer::empty(Rect::new(0, 0, width, height));
    render(&mut buf);
    for row in top.max(sel_top)..=(bottom - 1).min(sel_bottom) {
        if let Some((from, to)) = sel.row_range(row, width) {
            lines[(row - sel_top) as usize] = extract_row(&buf, row - top, from, to);
        }
    }
}

/// Reads the text of one buffer row between the inclusive columns `from..=to`,
/// skipping the continuation cells of wide characters.
fn extract_row(buf: &Buffer, row: u16, from: u16, to: u16) -> String {
    let mut out = String::new();
    let mut x = from;
    while x <= to {
        let Some(cell) = buf.cell((x, row)) else {
            break;
        };
        let symbol = cell.symbol();
        out.push_str(symbol);
        x = x.saturating_add(symbol.width().max(1) as u16);
    }
    out.trim_end().to_string()
}

/// Finds the copy button (if any) at the given paragraph-relative position and
/// returns its code content.
fn find_copy_button(buttons: &[CodeCopyButton], row: u16, x: u16) -> Option<String> {
    buttons
        .iter()
        .find(|b| b.row == row && x >= b.x_start && x < b.x_end)
        .map(|b| b.content.clone())
}

/// Whether an item has collapsible detail the user can click to expand.
fn is_foldable(item: &MessageItem) -> bool {
    matches!(
        item,
        MessageItem::Output(OutputItem::Reasoning(_))
            | MessageItem::Output(OutputItem::FunctionCall(_))
            | MessageItem::ToolOutput { .. }
    )
}

/// Cached render output for a single finished message.
///
/// Historical `items` are append-only and never mutate once added, so their
/// markdown is parsed and laid out exactly once and reused across frames. Only
/// the streaming `receiving_response` is re-rendered every frame.
#[derive(Debug)]
pub(crate) struct CachedParagraph {
    pub paragraph: Paragraph<'static>,
    pub height: u16,
    /// The expand/collapse state this entry was built with, so it can be rebuilt
    /// when the user toggles the item.
    pub expanded: bool,
    /// For tool-call items: whether the matching result was already available
    /// when this entry was built, so it can be rebuilt when the result arrives.
    pub has_output: bool,
    /// Clickable copy hotspots for the item's code blocks, relative to the
    /// paragraph's top-left corner.
    pub copy_buttons: Vec<CodeCopyButton>,
    /// True when this entry's paragraph was skipped (only height estimated)
    /// to avoid expensive markdown rendering of far-offscreen items.
    pub lazy: bool,
}

/// A mouse text selection over the conversation, in scroll-buffer coordinates
/// (`(x, y)` where `y` is a row in the full scrolled content, not the screen).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Selection {
    pub anchor: (u16, u16),
    pub head: (u16, u16),
    /// True once the mouse moved to a different cell while held down; a press
    /// and release on the same cell is a click, not a selection.
    pub dragging: bool,
}

impl Selection {
    /// Selection endpoints ordered by (row, column): (start, end), inclusive.
    fn ordered(&self) -> ((u16, u16), (u16, u16)) {
        let (a, h) = (self.anchor, self.head);
        if (a.1, a.0) <= (h.1, h.0) { (a, h) } else { (h, a) }
    }

    /// The inclusive x range this selection covers on buffer row `row`.
    pub(crate) fn row_range(&self, row: u16, width: u16) -> Option<(u16, u16)> {
        let ((start_x, start_y), (end_x, end_y)) = self.ordered();
        if row < start_y || row > end_y || width == 0 {
            return None;
        }
        let last = width - 1;
        let from = if row == start_y { start_x } else { 0 };
        let to = if row == end_y { end_x } else { last };
        Some((from.min(last), to.min(last)))
    }
}

/// The outcome of releasing the left mouse button over the conversation.
pub enum SelectionEnd {
    /// The press started outside the panel; nothing to do.
    Ignored,
    /// Press and release on the same cell: treat as a click.
    Click,
    /// A drag selection finished; contains the selected text.
    Copied(String),
}

#[derive(Debug, Default)]
pub(crate) struct RenderCache {
    /// The `content_width` the cached paragraphs were laid out for. When the
    /// width changes every entry must be rebuilt.
    pub width: u16,
    /// Parallel to the finished prefix of `items`, indexed identically.
    pub entries: Vec<CachedParagraph>,
}

#[derive(Debug)]
pub struct ConversationPanel {
    pub(crate) items: Vec<MessageItem>,
    pub(crate) scroll_view_state: ScrollViewState,
    pub pending_message: Option<String>,
    pub receiving_response: Option<PartialResponse>,
    /// Accumulated token usage across all responses in the current turn
    /// (a turn may span multiple responses when tool calls are involved).
    pub accumulated_usage: (u32, u32),
    /// The current active work phase of the turn. Replaces the old cluster of
    /// mutually-exclusive `tool_running`/`creating_tool_call`/… booleans:
    /// exactly one phase is active at a time, so a single enum models it
    /// without invalid combinations. "Thinking" is not a phase — it is derived
    /// from [`ActivePhase::None`] while a response is still streaming.
    pub phase: ActivePhase,
    /// When true the view follows new content at the bottom. Scrolling up turns
    /// it off; scrolling back to the bottom turns it on again. This replaces
    /// re-snapping on every chunk, which fought manual scrolling during streaming.
    pub(crate) stick_to_bottom: bool,
    /// Indices into `items` that the user has expanded. Foldable items (reasoning,
    /// tool calls, tool results) render collapsed unless their index is here.
    pub(crate) expanded_items: HashSet<usize>,
    /// The screen area the panel last rendered into, and the scroll offset used,
    /// so a mouse click can be mapped back to the item under the cursor.
    view_area: Rect,
    view_offset: u16,
    /// Per-item vertical extent in scroll-buffer coordinates: `(index, top, bottom)`.
    /// Recorded each render and consulted on click.
    item_layout: Vec<(usize, u16, u16)>,
    /// Per-live-item vertical extent: `(live_index, top, bottom)`. Recorded
    /// alongside `item_layout` so clicks on streaming items can be mapped.
    live_item_layout: Vec<(usize, u16, u16)>,
    pub(crate) render_cache: RenderCache,
    /// Monotonic frame counter, incremented every render. Drives the animated
    /// "Thinking..." dots on the live reasoning indicator during streaming.
    pub(crate) frame_count: u64,
    /// Indices into the live (streaming) items that the user has expanded.
    /// Cleared when a new stream starts.
    pub(crate) live_expanded_items: HashSet<usize>,
    /// The current mouse text selection, if any.
    pub(crate) selection: Option<Selection>,
    /// The streaming items' paragraphs from the last render (paragraph, height,
    /// copy buttons), kept for selection extraction and copy-button clicks.
    pub(crate) live_paragraphs: Vec<(Paragraph<'static>, u16, Vec<CodeCopyButton>)>,
    /// Vertical extent `(top, height)` of the pending-message note in the last
    /// render, for selection extraction.
    pub(crate) pending_layout: Option<(u16, u16)>,
}

impl ConversationPanel {
    pub fn new() -> Self {
        ConversationPanel {
            items: vec![],
            scroll_view_state: ScrollViewState::new(),
            pending_message: None,
            receiving_response: None,
            phase: ActivePhase::None,
            stick_to_bottom: true,
            expanded_items: HashSet::new(),
            view_area: Rect::ZERO,
            view_offset: 0,
            item_layout: Vec::new(),
            live_item_layout: Vec::new(),
            render_cache: RenderCache::default(),
            frame_count: 0,
            live_expanded_items: HashSet::new(),
            selection: None,
            live_paragraphs: Vec::new(),
            pending_layout: None,
            accumulated_usage: (0, 0),
        }
    }

    /// Records the layout from the last render so clicks can be mapped to items.
    pub(crate) fn set_layout(
        &mut self,
        area: Rect,
        offset: u16,
        item_layout: Vec<(usize, u16, u16)>,
        live_item_layout: Vec<(usize, u16, u16)>,
    ) {
        self.view_area = area;
        self.view_offset = offset;
        self.item_layout = item_layout;
        self.live_item_layout = live_item_layout;
    }

    /// Handles a left click at the given screen coordinates: if it lands on a
    /// foldable item (finished or live), toggle that item's expanded state.
    pub fn handle_click(&mut self, column: u16, row: u16) {
        let area = self.view_area;
        let inside = area.width > 0
            && area.height > 0
            && column >= area.x
            && column < area.x + area.width
            && row >= area.y
            && row < area.y + area.height;
        if !inside {
            return;
        }

        let buffer_y = (row - area.y).saturating_add(self.view_offset);
        let x_rel = column - area.x;

        // Live items sit after finished items in the scroll buffer; check them
        // first so they take priority when layout ranges overlap (they shouldn't,
        // but this is the safer ordering).
        if let Some(&(live_idx, top, _)) = self
            .live_item_layout
            .iter()
            .find(|&&(_, top, bottom)| buffer_y >= top && buffer_y < bottom)
        {
            let hit = self
                .live_paragraphs
                .get(live_idx)
                .and_then(|(_, _, buttons)| find_copy_button(buttons, buffer_y - top, x_rel));
            if let Some(content) = hit {
                self.copy_code_block(&content);
                return;
            }
            if !self.live_expanded_items.remove(&live_idx) {
                self.live_expanded_items.insert(live_idx);
            }
            return;
        }

        if let Some(&(index, top, _)) = self
            .item_layout
            .iter()
            .find(|&&(_, top, bottom)| buffer_y >= top && buffer_y < bottom)
        {
            let hit = self
                .render_cache
                .entries
                .get(index)
                .and_then(|entry| find_copy_button(&entry.copy_buttons, buffer_y - top, x_rel));
            if let Some(content) = hit {
                self.copy_code_block(&content);
                return;
            }
            if self.items.get(index).is_some_and(is_foldable) {
                if !self.expanded_items.remove(&index) {
                    self.expanded_items.insert(index);
                }
            }
        }
    }

    /// Copies a code block's content to the clipboard, reporting failure.
    fn copy_code_block(&mut self, content: &str) {
        if !crate::clipboard::copy(content) {
            self.add_error_string("failed to copy code block to clipboard");
        }
    }

    /// Maps screen coordinates into scroll-buffer coordinates, clamping into
    /// the panel area. Returns `None` when the point is outside the panel.
    fn to_buffer_pos(&self, column: u16, row: u16, clamp: bool) -> Option<(u16, u16)> {
        let area = self.view_area;
        if area.width == 0 || area.height == 0 {
            return None;
        }
        let (column, row) = if clamp {
            (
                column.clamp(area.x, area.right() - 1),
                row.clamp(area.y, area.bottom() - 1),
            )
        } else {
            if column < area.x || column >= area.right() || row < area.y || row >= area.bottom() {
                return None;
            }
            (column, row)
        };
        Some((column - area.x, (row - area.y).saturating_add(self.view_offset)))
    }

    /// Left button pressed: start a potential selection at this point.
    pub fn selection_begin(&mut self, column: u16, row: u16) {
        self.selection = self.to_buffer_pos(column, row, false).map(|pos| Selection {
            anchor: pos,
            head: pos,
            dragging: false,
        });
    }

    /// Mouse dragged with the left button held: extend the selection.
    pub fn selection_drag(&mut self, column: u16, row: u16) {
        // Auto-scroll when dragging against the top/bottom edge so the
        // selection can extend past the visible window. The terminal clamps the
        // mouse row to its bounds, so a drag beyond the edge keeps arriving at
        // the edge row — treat that as "scroll and keep selecting". Selection
        // coordinates are absolute (buffer) positions, so scrolling moves the
        // window while the anchor stays put and the head reaches new content.
        let area = self.view_area;
        if self.selection.is_some() && area.height > 0 {
            if row >= area.bottom().saturating_sub(1) {
                self.scroll_down();
            } else if row <= area.y {
                self.scroll_up();
            }
        }
        let Some(pos) = self.to_buffer_pos(column, row, true) else {
            return;
        };
        if let Some(sel) = self.selection.as_mut() {
            if pos != sel.anchor {
                sel.dragging = true;
            }
            sel.head = pos;
        }
    }

    /// Left button released: either finish a drag selection (returning the
    /// selected text) or report a plain click.
    pub fn selection_end(&mut self, column: u16, row: u16) -> SelectionEnd {
        self.selection_drag(column, row);
        match self.selection {
            None => SelectionEnd::Ignored,
            Some(sel) if !sel.dragging || sel.anchor == sel.head => {
                self.selection = None;
                SelectionEnd::Click
            }
            // Keep the selection so the highlight stays visible.
            Some(sel) => SelectionEnd::Copied(self.extract_selection_text(sel)),
        }
    }

    /// Reads the selected text back out of the rendered content by re-rendering
    /// the regions the selection covers into an off-screen buffer.
    fn extract_selection_text(&self, sel: Selection) -> String {
        let width = self.render_cache.width;
        if width == 0 {
            return String::new();
        }
        let ((_, sel_top), (_, sel_bottom)) = sel.ordered();
        let mut lines = vec![String::new(); (sel_bottom - sel_top) as usize + 1];

        let welcome = WelcomeMessage::default();
        let welcome_height = welcome.line_count(width);
        extract_region(&mut lines, &sel, sel_top, 0, welcome_height, width, |b| {
            (&welcome).render(b.area, b)
        });

        for &(index, top, bottom) in &self.item_layout {
            if let Some(entry) = self.render_cache.entries.get(index) {
                extract_region(
                    &mut lines,
                    &sel,
                    sel_top,
                    top,
                    bottom.saturating_sub(top),
                    width,
                    |b| (&entry.paragraph).render(b.area, b),
                );
            }
        }
        for &(live_index, top, bottom) in &self.live_item_layout {
            if let Some((paragraph, _, _)) = self.live_paragraphs.get(live_index) {
                extract_region(
                    &mut lines,
                    &sel,
                    sel_top,
                    top,
                    bottom.saturating_sub(top),
                    width,
                    |b| paragraph.render(b.area, b),
                );
            }
        }
        if let (Some(text), Some((top, height))) =
            (self.pending_message.as_ref(), self.pending_layout)
        {
            let paragraph = PendingMessage::new(text).into_paragraph();
            extract_region(&mut lines, &sel, sel_top, top, height, width, |b| {
                (&paragraph).render(b.area, b)
            });
        }

        lines.join("\n")
    }

    /// Whether a turn is in flight (streaming a response or running tools), in
    /// which case new user input is queued rather than starting a new request.
    pub fn is_busy(&self) -> bool {
        self.receiving_response.is_some()
            || matches!(
                self.phase,
                ActivePhase::ToolRunning | ActivePhase::Classifying
            )
    }

    /// Appends a tool result so it is both rendered and sent back to the model
    /// on the next request. The `failed` flag is authoritative (reported by the
    /// tool via [`crate::tools::run_tool_call`]), stored alongside the output so
    /// renderers and the classifier never re-parse the text for an `error:`
    /// prefix.
    pub fn add_tool_output(&mut self, output: crate::tools::ToolOutput) {
        self.items.push(MessageItem::ToolOutput {
            output: output.param,
            failed: output.failed,
            approval_label: output.approval_label,
        });
    }

    /// Append text to the stored output of the tool call identified by
    /// `call_id`, so post-edit feedback (diagnostics) renders inside that call's
    /// result — visible when the user expands it — and is sent to the model as
    /// part of the tool result. Returns whether a matching output was found.
    ///
    /// This is the one place `items` is mutated rather than appended, so the
    /// affected cache entry is dropped to force a re-render.
    pub fn append_to_tool_output(&mut self, call_id: &str, extra: &str) -> bool {
        for item in self.items.iter_mut() {
            if let MessageItem::ToolOutput { output, .. } = item {
                if output.call_id == call_id {
                    match &mut output.output {
                        FunctionCallOutput::Text(text) => text.push_str(extra),
                        other => *other = FunctionCallOutput::Text(extra.trim_start().to_string()),
                    }
                    // The result renders inside its call's entry, which isn't
                    // necessarily adjacent (a batch interleaves several calls and
                    // outputs). Drop the whole cache so it rebuilds cleanly.
                    self.render_cache.entries.clear();
                    return true;
                }
            }
        }
        false
    }

    pub fn add_input_message(&mut self, input_message_item: ApiMessageItem) {
        self.items
            .push(MessageItem::Input(InputItem::Item(Item::from(
                input_message_item,
            ))));
        // A new user message should always bring the view back to the bottom.
        self.stick_to_bottom = true;
    }

    pub fn add_error(&mut self, openai_error: OpenAIError) {
        // Non-conforming providers send error payloads the stream parser can't
        // deserialize; surface the embedded API error message instead of the
        // raw "missing field" noise when the payload is recognizable.
        if let OpenAIError::JSONDeserialize(_, content) = &openai_error {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(content) {
                if let Some(message) = value
                    .get("message")
                    .or_else(|| value.get("error").and_then(|e| e.get("message")))
                    .and_then(|m| m.as_str())
                {
                    let code = value
                        .get("code")
                        .or_else(|| value.get("error").and_then(|e| e.get("code")))
                        .and_then(|c| c.as_str())
                        .unwrap_or("api error");
                    self.add_error_string(format!("{code}: {message}"));
                    return;
                }
            }
        }
        self.items.push(MessageItem::OpenAIError(openai_error));
        self.stick_to_bottom = true;
    }

    pub fn add_error_string(&mut self, message: impl Into<String>) {
        self.items.push(MessageItem::Error(message.into()));
        self.stick_to_bottom = true;
    }

    pub fn add_info_string(&mut self, message: impl Into<String>) {
        self.items.push(MessageItem::Info(message.into()));
        self.stick_to_bottom = true;
    }

    pub fn add_meta(&mut self, label: impl Into<String>, text: impl Into<String>) {
        self.items.push(MessageItem::Meta {
            label: label.into(),
            text: text.into(),
        });
        self.stick_to_bottom = true;
    }

    pub fn add_warning_string(&mut self, message: impl Into<String>) {
        self.items.push(MessageItem::Warning(message.into()));
        self.stick_to_bottom = true;
    }

    pub fn add_usage(&mut self, input_tokens: u32, output_tokens: u32) {
        self.accumulated_usage.0 += input_tokens;
        self.accumulated_usage.1 += output_tokens;
    }

    /// Flush the accumulated usage as a message and reset the counter.
    pub fn flush_usage(&mut self) {
        let (input, output) = self.accumulated_usage;
        if input > 0 || output > 0 {
            self.items.push(MessageItem::Usage(input, output));
            self.accumulated_usage = (0, 0);
            self.stick_to_bottom = true;
        }
    }

    /// Reset the accumulated usage counter (on /clear, new session, etc.).
    pub fn reset_accumulated_usage(&mut self) {
        self.accumulated_usage = (0, 0);
    }

    /// Clear all conversation history and pending state.
    pub fn clear_messages(&mut self) {
        self.items.clear();
        self.pending_message = None;
        self.expanded_items.clear();
        self.live_expanded_items.clear();
        self.selection = None;
        self.stick_to_bottom = true;
        self.accumulated_usage = (0, 0);
    }

    /// Restore a previous session's items into the conversation.
    pub fn restore_items(&mut self, items: Vec<MessageItem>) {
        self.items = items;
        self.stick_to_bottom = true;
    }

    /// Iterate over the current conversation items (for persistence).
    pub fn items(&self) -> impl Iterator<Item = &MessageItem> {
        self.items.iter()
    }

    /// Ends the in-flight response (e.g. after a stream error), keeping whatever
    /// was produced so far and clearing the "receiving" state so the turn is no
    /// longer considered busy.
    pub fn abort_receiving(&mut self) {
        if let Some(partial) = self.receiving_response.take() {
            // Transfer live expanded state before items become historical,
            // so reasoning/tool-call items the user expanded during streaming
            // stay expanded instead of auto-collapsing.
            let base_index = self.items.len();
            for &live_idx in &self.live_expanded_items {
                self.expanded_items.insert(base_index + live_idx);
            }
            let cancelled = partial.cancelled.load(std::sync::atomic::Ordering::Relaxed);
            let items: Vec<OutputItem> = if cancelled {
                // When the user cancelled, drop all function calls so they
                // aren't shown and won't execute.
                partial
                    .items
                    .into_iter()
                    .flatten()
                    .filter(|item| !matches!(item, OutputItem::FunctionCall(_)))
                    .collect()
            } else {
                partial.into_aborted_items()
            };
            self.items
                .extend(items.into_iter().map(MessageItem::Output));
        }
    }

    pub fn scroll_to_bottom(&mut self) {
        self.stick_to_bottom = true;
        self.scroll_view_state.scroll_to_bottom();
    }

    pub fn is_at_bottom(&self) -> bool {
        self.scroll_view_state.is_at_bottom()
    }

    pub fn scroll_up(&mut self) {
        // Stop following the bottom so incoming content doesn't yank the view back.
        self.stick_to_bottom = false;
        for _ in 0..SCROLL_LINES {
            self.scroll_view_state.scroll_up();
        }
    }

    pub fn scroll_down(&mut self) {
        for _ in 0..SCROLL_LINES {
            self.scroll_view_state.scroll_down();
        }
        // Reaching the bottom again re-enables auto-follow.
        if self.scroll_view_state.is_at_bottom() {
            self.stick_to_bottom = true;
        }
    }

    pub fn handle_response_stream_event(
        &mut self,
        response_stream_event: ResponseStreamEvent,
    ) -> Option<PartialResponse> {
        let receiving_response = self
            .receiving_response
            .as_mut()
            .expect("handle_response_stream_event called with no receiving_response");
        receiving_response.handle_response_stream_event(response_stream_event);

        if receiving_response.finished() {
            self.receiving_response.take()
        } else {
            None
        }
    }

    pub fn get_input_param(&self, current_model: &str, skill_prompt: Option<&str>, plan_prompt: Option<&str>) -> InputParam {
        let mut system_prompt = format!(
            "{SYSTEM_PROMPT}\n\nYou are running as model: {current_model}\n\n{}",
            crate::tools::environment_info()
        );
        if let Some(prompt) = skill_prompt {
            system_prompt.push_str("\n\n");
            system_prompt.push_str(prompt);
        }
        if let Some(prompt) = plan_prompt {
            system_prompt.push_str("\n\n");
            system_prompt.push_str(prompt);
        }
        let developer_message =
            InputItem::from(Item::Message(ApiMessageItem::Input(InputMessage {
                content: vec![InputContent::InputText(system_prompt.into())],
                role: InputRole::Developer,
                status: Some(OutputStatus::Completed),
            })));

        // Recorded function_call_output items, keyed by call id.
        let mut recorded_outputs: HashMap<&str, &FunctionCallOutputItemParam> = HashMap::new();
        for item in &self.items {
            if let MessageItem::ToolOutput { output, .. } = item {
                recorded_outputs.entry(output.call_id.as_str()).or_insert(output);
            }
        }

        // Outputs must stay grouped after the whole assistant output block, in
        // call order (`reasoning, call_1, call_2, output_1, output_2`), matching
        // how OpenAI documents multi-turn tool use. Interleaving them between
        // the calls makes chat-completions-backed providers split the block
        // into several assistant messages, and thinking models (e.g. DeepSeek)
        // then reject the later ones for missing reasoning content.
        let mut input_items = vec![developer_message];
        let mut pending_outputs: Vec<InputItem> = Vec::new();
        for message_item in &self.items {
            match message_item {
                // A stored output marks the boundary after an assistant block:
                // flush that block's outputs here, in call order.
                MessageItem::ToolOutput { .. } => {
                    input_items.append(&mut pending_outputs);
                }
                MessageItem::Input(input_item) => {
                    input_items.append(&mut pending_outputs);
                    input_items.push(input_item.clone());
                }
                MessageItem::Meta { text, .. } => {
                    input_items.append(&mut pending_outputs);
                    input_items.push(InputItem::from(Item::Message(ApiMessageItem::Input(InputMessage {
                        content: vec![InputContent::InputText(InputTextContent { text: text.clone() })],
                        role: InputRole::User,
                        status: Some(OutputStatus::Completed),
                    }))));
                }
                MessageItem::Output(output_item) => {
                    // A non-call output (an assistant message or reasoning the
                    // model emitted *after* its tool calls) closes the tool-call
                    // block: flush the pending outputs first so every
                    // `function_call` stays immediately followed by its
                    // `function_call_output`. Otherwise a trailing message wedges
                    // between a call and its output, and chat-completions-backed
                    // providers reject the assistant tool_calls message for not
                    // being followed by tool results.
                    if !matches!(output_item, OutputItem::FunctionCall(_)) {
                        input_items.append(&mut pending_outputs);
                    }
                    input_items.push(output_item.clone().into());
                    if let OutputItem::FunctionCall(call) = output_item {
                        let output = match recorded_outputs.remove(call.call_id.as_str()) {
                            Some(output) => output.clone(),
                            // A call with no recorded output (e.g. the user
                            // cancelled while the tool was running) would make
                            // the API reject the whole history; answer it
                            // synthetically.
                            None => FunctionCallOutputItemParam {
                                call_id: call.call_id.clone(),
                                output: FunctionCallOutput::Text(
                                    "error: tool execution was cancelled before it completed"
                                        .to_string(),
                                ),
                                id: None,
                                status: None,
                            },
                        };
                        pending_outputs.push(InputItem::from(Item::FunctionCallOutput(output)));
                    }
                }
                _ => {}
            }
        }
        input_items.append(&mut pending_outputs);

        InputParam::Items(input_items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::widgets::Widget;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    fn user_message(text: &str) -> ApiMessageItem {
        ApiMessageItem::Input(InputMessage {
            content: vec![InputContent::InputText(text.into())],
            role: InputRole::User,
            status: Some(OutputStatus::Completed),
        })
    }

    #[test]
    fn render_does_not_panic_and_scroll_up_moves_the_view() {
        let mut panel = ConversationPanel::new();
        for i in 0..40 {
            panel.add_input_message(user_message(&format!("message number {i}")));
        }

        let area = Rect::new(0, 0, 40, 10);

        // Initial render sticks to the bottom.
        let mut buf = Buffer::empty(area);
        (&mut panel).render(area, &mut buf);
        let bottom = panel.scroll_view_state.offset().y;
        assert!(bottom > 0, "content taller than viewport should scroll");

        // Scrolling up should move the view up and stay there across renders.
        panel.scroll_up();
        let mut buf2 = Buffer::empty(area);
        (&mut panel).render(area, &mut buf2);
        let after = panel.scroll_view_state.offset().y;
        assert!(
            after < bottom,
            "offset should decrease: {bottom} -> {after}"
        );
        assert!(!panel.stick_to_bottom, "scrolling up disables auto-follow");
    }

    #[test]
    fn function_call_outputs_stay_grouped_after_the_assistant_block() {
        use async_openai::types::responses::{
            AssistantRole, FunctionToolCall, OutputMessage, OutputMessageContent,
            OutputTextContent,
        };

        let call = |call_id: &str| {
            OutputItem::FunctionCall(FunctionToolCall {
                arguments: "{}".into(),
                call_id: call_id.into(),
                namespace: None,
                name: "command".into(),
                id: None,
                status: None,
            })
        };
        let output = |call_id: &str| crate::tools::ToolOutput {
            param: FunctionCallOutputItemParam {
                call_id: call_id.into(),
                output: FunctionCallOutput::Text("ok".into()),
                id: None,
                status: None,
            },
            failed: false,
            approval_label: None,
        };
        let assistant_text = |text: &str| {
            OutputItem::Message(OutputMessage {
                content: vec![OutputMessageContent::OutputText(OutputTextContent {
                    annotations: vec![],
                    logprobs: None,
                    text: text.into(),
                })],
                id: "msg_1".into(),
                role: AssistantRole::Assistant,
                phase: None,
                status: OutputStatus::Completed,
            })
        };

        let mut panel = ConversationPanel::new();
        panel.add_input_message(user_message("hi"));
        // The model emitted text *after* the call within the same response, so
        // the recorded output ended up separated from its call by that text.
        panel.items.push(MessageItem::Output(call("call_1")));
        panel
            .items
            .push(MessageItem::Output(assistant_text("trailing text")));
        panel.add_tool_output(output("call_1"));
        // An orphaned call with no recorded output (cancelled mid-run).
        panel.items.push(MessageItem::Output(call("call_2")));

        let InputParam::Items(items) = panel.get_input_param("test/model", None, None) else {
            panic!("expected an item list");
        };
        // Every call must be answered (missing ones synthesized), with all
        // outputs grouped after the assistant output block in call order —
        // never interleaved between the calls.
        let kind = |item: &InputItem| match item {
            InputItem::Item(Item::FunctionCall(c)) => format!("call:{}", c.call_id),
            InputItem::Item(Item::FunctionCallOutput(o)) => format!("output:{}", o.call_id),
            InputItem::Item(Item::Message(_)) => "message".to_string(),
            _ => "other".to_string(),
        };
        let kinds: Vec<String> = items.iter().map(|i| kind(i)).collect();
        // Each call must be immediately followed by its output; a message the
        // model emitted after the call is pushed out to *after* that output, so
        // the assistant tool_calls block is always answered by tool results
        // before any other message — what chat-completions providers require.
        assert_eq!(
            kinds,
            vec![
                "message", // developer
                "message", // user
                "call:call_1",
                "output:call_1",
                "message", // trailing assistant text, moved after the output
                "call:call_2",
                "output:call_2", // synthesized for the orphaned call
            ]
        );
    }

    #[test]
    fn parallel_call_outputs_are_not_interleaved_between_calls() {
        use async_openai::types::responses::FunctionToolCall;

        let call = |call_id: &str| {
            OutputItem::FunctionCall(FunctionToolCall {
                arguments: "{}".into(),
                call_id: call_id.into(),
                namespace: None,
                name: "command".into(),
                id: None,
                status: None,
            })
        };
        let output = |call_id: &str| crate::tools::ToolOutput {
            param: FunctionCallOutputItemParam {
                call_id: call_id.into(),
                output: FunctionCallOutput::Text("ok".into()),
                id: None,
                status: None,
            },
            failed: false,
            approval_label: None,
        };

        let mut panel = ConversationPanel::new();
        panel.add_input_message(user_message("hi"));
        // One response with two parallel calls; outputs recorded afterwards.
        panel.items.push(MessageItem::Output(call("call_1")));
        panel.items.push(MessageItem::Output(call("call_2")));
        panel.add_tool_output(output("call_1"));
        panel.add_tool_output(output("call_2"));

        let InputParam::Items(items) = panel.get_input_param("test/model", None, None) else {
            panic!("expected an item list");
        };
        let order: Vec<String> = items
            .iter()
            .filter_map(|item| match item {
                InputItem::Item(Item::FunctionCall(c)) => Some(format!("call:{}", c.call_id)),
                InputItem::Item(Item::FunctionCallOutput(o)) => {
                    Some(format!("output:{}", o.call_id))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            order,
            vec!["call:call_1", "call:call_2", "output:call_1", "output:call_2"],
            "outputs must come after both calls, never between them"
        );
    }

    #[test]
    fn abort_receiving_clears_busy_state() {
        let mut panel = ConversationPanel::new();
        panel.receiving_response = Some(PartialResponse::new(Arc::new(AtomicBool::new(false))));
        assert!(panel.is_busy(), "receiving a response is busy");

        panel.abort_receiving();

        assert!(panel.receiving_response.is_none());
        assert!(!panel.is_busy(), "aborting must clear the busy state");
    }
}
