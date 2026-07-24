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
use async_openai::error::OpenAIError;
use async_openai::types::responses::MessageItem as ApiMessageItem;
use async_openai::types::responses::{InputParam, OutputItem, ResponseStreamEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;
use ratatui_widgets::paragraph::Paragraph;
use std::collections::HashSet;
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
    /// `/compact` is summarizing the conversation to shrink the context.
    Compacting,
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
            | MessageItem::Compacted { .. }
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
    /// The conversation's `mutation_version` the entries were built against.
    /// Appends never bump it (index-keyed entries stay valid), but an in-place
    /// mutation — e.g. the runner folding diagnostics into a tool output —
    /// does, and every entry must be dropped.
    pub seen_mutation_version: u64,
}

#[derive(Debug)]
pub struct ConversationPanel {
    /// The UI-free conversation model: history items and turn-usage counter.
    /// The panel adds the view state below on top of it. Shared with the
    /// runner task that drives the turn — the runner appends under brief locks
    /// from its background task while the panel renders it every frame, so
    /// every access here locks briefly and never holds the guard across an
    /// await (there are none in the UI thread) or a render sub-call.
    pub(crate) conversation: std::sync::Arc<std::sync::Mutex<crate::conversation::Conversation>>,
    pub(crate) scroll_view_state: ScrollViewState,
    pub pending_message: Option<String>,
    pub receiving_response: Option<PartialResponse>,
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
    /// Screen area of the "jump to bottom" indicator from the last render, when
    /// the view is scrolled up. `None` when at the bottom (indicator hidden).
    jump_button: Option<Rect>,
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
            conversation: std::sync::Arc::new(std::sync::Mutex::new(
                crate::conversation::Conversation::new(),
            )),
            scroll_view_state: ScrollViewState::new(),
            pending_message: None,
            receiving_response: None,
            phase: ActivePhase::None,
            stick_to_bottom: true,
            expanded_items: HashSet::new(),
            view_area: Rect::ZERO,
            view_offset: 0,
            jump_button: None,
            item_layout: Vec::new(),
            live_item_layout: Vec::new(),
            render_cache: RenderCache::default(),
            frame_count: 0,
            live_expanded_items: HashSet::new(),
            selection: None,
            live_paragraphs: Vec::new(),
            pending_layout: None,
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
            if self
                .conversation
                .lock()
                .unwrap()
                .items
                .get(index)
                .is_some_and(is_foldable)
                && !self.expanded_items.remove(&index)
            {
                self.expanded_items.insert(index);
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

        let welcome = WelcomeMessage;
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
                ActivePhase::ToolRunning | ActivePhase::Classifying | ActivePhase::Compacting
            )
    }

    /// The shared conversation handle, for the runner task that drives a turn.
    pub fn shared_conversation(
        &self,
    ) -> std::sync::Arc<std::sync::Mutex<crate::conversation::Conversation>> {
        self.conversation.clone()
    }

    /// Appends a tool result so it is both rendered and sent back to the model
    /// on the next request. Delegates to [`Conversation::add_tool_output`].
    pub fn add_tool_output(&mut self, output: crate::tools::ToolOutput) {
        self.conversation.lock().unwrap().add_tool_output(output);
    }

    pub fn add_input_message(&mut self, input_message_item: ApiMessageItem) {
        self.conversation.lock().unwrap().add_input_message(input_message_item);
        // A new user message should always bring the view back to the bottom.
        self.stick_to_bottom = true;
    }

    pub fn add_error(&mut self, openai_error: OpenAIError) {
        self.conversation.lock().unwrap().add_error(openai_error);
        self.stick_to_bottom = true;
    }

    pub fn add_error_string(&mut self, message: impl Into<String>) {
        self.conversation.lock().unwrap().add_error_string(message);
        self.stick_to_bottom = true;
    }

    pub fn add_info_string(&mut self, message: impl Into<String>) {
        self.conversation.lock().unwrap().add_info_string(message);
        self.stick_to_bottom = true;
    }

    pub fn add_meta(&mut self, label: impl Into<String>, text: impl Into<String>) {
        self.conversation.lock().unwrap().add_meta(label, text);
        self.stick_to_bottom = true;
    }

    pub fn add_warning_string(&mut self, message: impl Into<String>) {
        self.conversation.lock().unwrap().add_warning_string(message);
        self.stick_to_bottom = true;
    }

    /// Whether there is API-visible history worth compacting: any input/output
    /// item after the last `/compact` boundary.
    pub fn has_compactable_history(&self) -> bool {
        self.conversation.lock().unwrap().has_compactable_history()
    }

    /// Record a finished `/compact`: push the boundary carrying `summary`.
    /// History before it stays visible in the UI but stops being sent to the
    /// API (see [`Conversation::to_input_param`]).
    pub fn apply_compaction(&mut self, summary: String) {
        self.conversation.lock().unwrap().apply_compaction(summary);
        self.stick_to_bottom = true;
    }

    pub fn add_usage(&mut self, input_tokens: u32, output_tokens: u32) {
        self.conversation.lock().unwrap().add_usage(input_tokens, output_tokens);
    }

    /// Flush the accumulated usage as a message and reset the counter.
    pub fn flush_usage(&mut self) {
        if self.conversation.lock().unwrap().flush_usage() {
            self.stick_to_bottom = true;
        }
    }

    /// Reset the accumulated usage counter (on /clear, new session, etc.).
    pub fn reset_accumulated_usage(&mut self) {
        self.conversation.lock().unwrap().reset_accumulated_usage();
    }

    /// Clear all conversation history and pending state.
    pub fn clear_messages(&mut self) {
        self.conversation.lock().unwrap().clear();
        self.pending_message = None;
        self.expanded_items.clear();
        self.live_expanded_items.clear();
        self.selection = None;
        self.stick_to_bottom = true;
    }

    /// Restore a previous session's items into the conversation.
    pub fn restore_items(&mut self, items: Vec<MessageItem>) {
        self.conversation.lock().unwrap().restore_items(items);
        self.stick_to_bottom = true;
    }

    /// A snapshot of the current conversation items (for persistence).
    pub fn items_snapshot(&self) -> Vec<MessageItem> {
        self.conversation.lock().unwrap().items.clone()
    }

    /// The runner committed the streamed response to the shared conversation:
    /// drop the live in-progress view so the same content isn't rendered twice,
    /// transferring live expanded state onto the now-committed items (which sit
    /// at the tail of the conversation).
    pub fn commit_live(&mut self) {
        if let Some(partial) = self.receiving_response.take() {
            let committed = partial.items.iter().flatten().count();
            let base_index = self
                .conversation
                .lock()
                .unwrap()
                .items
                .len()
                .saturating_sub(committed);
            for &live_idx in &self.live_expanded_items {
                self.expanded_items.insert(base_index + live_idx);
            }
            self.live_expanded_items.clear();
        }
    }

    /// Ends the in-flight response (stream error / cancellation), salvaging
    /// whatever was produced so far into the conversation — the runner commits
    /// nothing for a response that errored or was cancelled mid-stream — and
    /// clearing the "receiving" state so the turn is no longer considered busy.
    pub fn abort_receiving(&mut self) {
        if let Some(partial) = self.receiving_response.take() {
            // Transfer live expanded state before items become historical,
            // so reasoning/tool-call items the user expanded during streaming
            // stay expanded instead of auto-collapsing.
            let base_index = self.conversation.lock().unwrap().items.len();
            for &live_idx in &self.live_expanded_items {
                self.expanded_items.insert(base_index + live_idx);
            }
            let cancelled = partial.cancelled.is_cancelled();
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
            let mut conv = self.conversation.lock().unwrap();
            for item in items {
                conv.add_output(item);
            }
        }
    }

    pub fn scroll_to_bottom(&mut self) {
        self.stick_to_bottom = true;
        self.scroll_view_state.scroll_to_bottom();
    }

    pub fn is_at_bottom(&self) -> bool {
        self.scroll_view_state.is_at_bottom()
    }

    /// Record the "jump to bottom" indicator's screen area for this frame.
    pub(crate) fn set_jump_button(&mut self, area: Option<Rect>) {
        self.jump_button = area;
    }

    /// Whether a click at `(column, row)` lands on the jump-to-bottom indicator.
    pub fn jump_button_hit(&self, column: u16, row: u16) -> bool {
        self.jump_button.is_some_and(|b| {
            column >= b.x && column < b.x + b.width && row >= b.y && row < b.y + b.height
        })
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

    /// Fold a streaming chunk into the live view. Rendering-only: the runner
    /// owns the authoritative folding and commits the finished response to the
    /// shared conversation itself — the live copy here just shows tokens as
    /// they arrive, and is dropped on [`ConversationPanel::commit_live`].
    pub fn handle_response_stream_event(&mut self, response_stream_event: ResponseStreamEvent) {
        let receiving_response = self
            .receiving_response
            .as_mut()
            .expect("handle_response_stream_event called with no receiving_response");
        receiving_response.handle_response_stream_event(response_stream_event);
    }

    /// Build the API request input from the conversation history. Delegates to
    /// [`Conversation::to_input_param`] — the history-shaping logic lives on the
    /// model so the headless runner produces byte-identical requests.
    pub fn get_input_param(
        &self,
        current_model: &str,
        skill_prompt: Option<&str>,
        plan_prompt: Option<&str>,
        coauthor: Option<&str>,
    ) -> InputParam {
        self.conversation
            .lock()
            .unwrap()
            .to_input_param(current_model, skill_prompt, plan_prompt, coauthor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cancel::CancellationToken;
    use async_openai::types::responses::{InputContent, InputMessage, InputRole, OutputStatus};
    use ratatui::buffer::Buffer;
    use ratatui::widgets::Widget;

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
    fn abort_receiving_clears_busy_state() {
        let mut panel = ConversationPanel::new();
        panel.receiving_response = Some(PartialResponse::new(CancellationToken::new()));
        assert!(panel.is_busy(), "receiving a response is busy");

        panel.abort_receiving();

        assert!(panel.receiving_response.is_none());
        assert!(!panel.is_busy(), "aborting must clear the busy state");
    }
}
