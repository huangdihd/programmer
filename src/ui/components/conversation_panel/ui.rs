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
use crate::ui::components::conversation_panel::conversation_panel::{
    CachedParagraph, ConversationPanel,
};
use crate::ui::components::messages::assistant_message::AssistantMessage;
use crate::ui::components::messages::error_message::ErrorMessage;
use crate::ui::components::messages::info_message::InfoMessage;
use crate::ui::components::messages::pending_message::PendingMessage;
use crate::ui::components::messages::tool_result::ToolResultMessage;
use crate::ui::components::messages::usage_message::UsageMessage;
use crate::ui::components::messages::user_message::UserMessage;
use crate::ui::components::messages::warning_message::WarningMessage;
use crate::ui::components::messages::welcome_message::WelcomeMessage;
use crate::ui::markdown_code_block::CodeCopyButton;
use crate::ui::markdown_theme::palette;
use async_openai::types::responses::{FunctionCallOutputItemParam, OutputItem};
use std::collections::{HashMap, HashSet};
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{StatefulWidget, Widget};
use ratatui_widgets::paragraph::Paragraph;
use tui_scrollview::ScrollView;

/// Rough height estimate for an item, used to avoid expensive markdown rendering
/// for items far from the viewport. Returns a u16 line count.
fn estimate_item_height(item: &MessageItem, width: u16) -> u16 {
    let w = width.max(40) as usize;
    match item {
        MessageItem::Input(input) => {
            let text = crate::app::helpers::extract_input_text(input).unwrap_or_default();
            rough_line_count(&text, w)
        }
        MessageItem::Output(output) => match output {
            OutputItem::Reasoning(_) => 3, // rough
            OutputItem::Message(_) => 4,
            OutputItem::FunctionCall(_) => 2,
            _ => 1,
        },
        MessageItem::ToolOutput { output, .. } => {
            match &output.output {
                async_openai::types::responses::FunctionCallOutput::Text(t) =>
                    rough_line_count(t, w).min(20),
                _ => 1,
            }
        }
        MessageItem::OpenAIError(_) | MessageItem::Error(_)
        | MessageItem::Warning(_) | MessageItem::Info(_) => 1,
        MessageItem::Meta { .. } => 1,
        MessageItem::Usage(_, _) => 1,
        // Usually collapsed to its one-line divider (like Reasoning, the
        // estimate ignores the expanded state — the real height comes from the
        // built paragraph).
        MessageItem::Compacted { .. } => 1,
    }
}

/// Rough line count: chars / width, plus explicit newlines.
fn rough_line_count(text: &str, width: usize) -> u16 {
    let lines: u16 = text.lines().count() as u16;
    let wrapped: u16 = text.lines()
        .map(|l| (l.chars().count().max(1) / width.max(1)).max(1) as u16)
        .sum();
    lines.max(wrapped).max(1)
}

/// Builds the paragraph for a finished history item. Called at most once per
/// item (the result is cached in [`ConversationPanel::render_cache`]).
fn build_item_paragraph(
    item: &MessageItem,
    content_width: u16,
    expanded: bool,
    tool_output: Option<(&FunctionCallOutputItemParam, bool, Option<&str>)>,
) -> (Paragraph<'static>, Vec<CodeCopyButton>) {
    match item {
        MessageItem::ToolOutput { output, failed, .. } => {
            // Only reached for orphan results whose call is missing; results
            // with a call render inside that call's item.
            (
                ToolResultMessage::new(output)
                    .failed(*failed)
                    .expanded(expanded)
                    .into_paragraph(),
                Vec::new(),
            )
        }
        MessageItem::Input(input_item) => (UserMessage::new(input_item).into_paragraph(), Vec::new()),
        MessageItem::Output(output_item) => AssistantMessage::new(output_item, content_width)
            .expanded(expanded)
            .tool_output(tool_output)
            .into_paragraph(),
        MessageItem::OpenAIError(error) => (
            ErrorMessage::new(error.to_string()).into_paragraph(),
            Vec::new(),
        ),
        MessageItem::Error(message) => {
            (ErrorMessage::new(message.clone()).into_paragraph(), Vec::new())
        }
        MessageItem::Info(message) => {
            (InfoMessage::new(message.clone()).into_paragraph(), Vec::new())
        }
        MessageItem::Meta { label, .. } => {
            (InfoMessage::new(format!("\u{25B8} {}", label)).into_paragraph(), Vec::new())
        }
        MessageItem::Warning(message) => {
            (WarningMessage::new(message.clone()).into_paragraph(), Vec::new())
        }
        MessageItem::Usage(input, output) => (
            UsageMessage::new(*input, *output).into_paragraph(),
            Vec::new(),
        ),
        MessageItem::Compacted { summary } => {
            use ratatui::text::{Line, Span, Text};
            use ratatui::widgets::Wrap;
            let arrow = if expanded { "\u{25BE}" } else { "\u{25B8}" };
            let mut lines: Vec<Line<'static>> = vec![Line::from(Span::styled(
                format!("{arrow} \u{2500}\u{2500} context compacted \u{2500}\u{2500}"),
                Style::new().fg(palette::MUTED).add_modifier(Modifier::BOLD),
            ))];
            if expanded {
                for line in summary.lines() {
                    lines.push(Line::from(Span::styled(
                        line.to_string(),
                        Style::new().fg(palette::MUTED),
                    )));
                }
            }
            (
                Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }),
                Vec::new(),
            )
        }
    }
}

impl Widget for &mut ConversationPanel {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.frame_count = self.frame_count.wrapping_add(1);
        let content_width = area.width.saturating_sub(1);
        let stick_to_bottom = self.stick_to_bottom;
        let welcome_message = WelcomeMessage;
        let welcome_height = welcome_message.line_count(content_width);
        let mut content_height: u16 = welcome_height;

        // Snapshot the shared conversation for this frame. The guard borrows
        // from a local Arc clone (not `self`), so the render cache below can be
        // borrowed mutably alongside; it is dropped as soon as the cached
        // paragraphs are built, before any rendering.
        let conv_arc = self.conversation.clone();
        let conv = conv_arc.lock().unwrap();

        // Tool calls render together with their result as one message: map each
        // call_id to its result, and collect the call ids so the standalone
        // result items can be hidden (they draw inside their call's entry).
        let mut outputs_by_call: HashMap<&str, (&FunctionCallOutputItemParam, bool, Option<&str>)> =
            HashMap::new();
        for item in &conv.items {
            if let MessageItem::ToolOutput { output, failed, approval_label } = item {
                outputs_by_call.insert(
                    output.call_id.as_str(),
                    (output, *failed, approval_label.as_deref()),
                );
            }
        }
        let call_ids: HashSet<&str> = self
            .conversation
            .items
            .iter()
            .filter_map(|item| match item {
                MessageItem::Output(OutputItem::FunctionCall(call)) => {
                    Some(call.call_id.as_str())
                }
                _ => None,
            })
            .collect();

        // Refresh the cache of finished messages. `items` is append-only, so an
        // entry is only rebuilt when the width changes (invalidating all), when
        // the user toggles that specific item's expand/collapse state, or when a
        // tool call's result arrives.
        let cache = &mut self.render_cache;
        if cache.width != content_width {
            cache.width = content_width;
            cache.entries.clear();
            self.selection = None;
        }
        // An in-place mutation (the engine folding diagnostics into a tool
        // output) invalidates by version; appends never bump it.
        if cache.seen_mutation_version != conv.mutation_version {
            cache.seen_mutation_version = conv.mutation_version;
            cache.entries.clear();
        }
        if cache.entries.len() > conv.items.len() {
            cache.entries.truncate(conv.items.len());
        }

        // First pass: compute estimated heights for every item without
        // fully rendering them, so we can figure out which ones are visible.
        let mut est_heights: Vec<u16> = Vec::with_capacity(conv.items.len());
        for index in 0..conv.items.len() {
            let h = if matches!(&conv.items[index], MessageItem::ToolOutput { output, .. }
                if call_ids.contains(output.call_id.as_str()))
            {
                0 // hidden inside its call's entry
            } else {
                estimate_item_height(&conv.items[index], content_width)
            };
            est_heights.push(h);
        }

        // With stick_to_bottom, the viewport covers roughly `area.height`
        // lines above the bottom. Build items within ~4 screenfuls; the
        // rest stay as cheap estimates until the user scrolls near them.
        let viewport_lines = area.height.max(20) as u32 * 4;
        let mut accum: u32 = 0;
        let mut build_from = conv.items.len();
        for i in (0..conv.items.len()).rev() {
            accum += est_heights[i] as u32;
            build_from = i;
            if accum >= viewport_lines {
                break;
            }
        }

        // When the user has scrolled away from the bottom, also build the
        // items overlapping the scroll window (plus a margin in both
        // directions), so scrolling up never lands on a blank placeholder.
        // Positions come from the cached heights (real for built entries,
        // estimates for lazy ones) — good enough to pick candidates, and
        // self-correcting as entries get built on subsequent frames.
        let mut in_window = vec![false; conv.items.len()];
        if !stick_to_bottom {
            let offset_y = self.scroll_view_state.offset().y as u32;
            let win_top = offset_y.saturating_sub(viewport_lines);
            let win_bottom = offset_y + area.height as u32 + viewport_lines;
            let mut y = welcome_height as u32;
            for i in 0..conv.items.len() {
                let h = cache
                    .entries
                    .get(i)
                    .filter(|e| !e.lazy)
                    .map(|e| e.height)
                    .unwrap_or(est_heights[i]) as u32;
                if y < win_bottom && y + h > win_top {
                    in_window[i] = true;
                }
                y += h;
            }
        }

        for index in 0..conv.items.len() {
            let expanded = self.expanded_items.contains(&index);
            let (hidden, tool_output) = match &conv.items[index] {
                MessageItem::ToolOutput { output, .. } => {
                    (call_ids.contains(output.call_id.as_str()), None)
                }
                MessageItem::Output(OutputItem::FunctionCall(call)) => (
                    false,
                    outputs_by_call.get(call.call_id.as_str()).copied(),
                ),
                _ => (false, None),
            };
            let has_output = tool_output.is_some();
            let in_viewport = index >= build_from || in_window[index];
            let needs_build = cache
                .entries
                .get(index)
                .is_none_or(|entry| {
                    entry.expanded != expanded
                        || entry.has_output != has_output
                        || (entry.lazy && in_viewport)
                });
            if needs_build {
                let entry = if hidden {
                    CachedParagraph {
                        paragraph: Paragraph::new(""),
                        height: 0,
                        expanded,
                        has_output,
                        copy_buttons: Vec::new(),
                        lazy: false,
                    }
                } else if !in_viewport {
                    CachedParagraph {
                        paragraph: Paragraph::new(""),
                        height: est_heights[index],
                        expanded,
                        has_output,
                        copy_buttons: Vec::new(),
                        lazy: true,
                    }
                } else {
                    let (paragraph, copy_buttons) = build_item_paragraph(
                        &conv.items[index],
                        content_width,
                        expanded,
                        tool_output,
                    );
                    let height = paragraph.line_count(content_width) as u16;
                    CachedParagraph {
                        paragraph,
                        height,
                        expanded,
                        has_output,
                        copy_buttons,
                        lazy: false,
                    }
                };
                if index < cache.entries.len() {
                    cache.entries[index] = entry;
                } else {
                    cache.entries.push(entry);
                }
            }
        }
        for entry in &cache.entries {
            content_height = content_height.saturating_add(entry.height);
        }

        // The streaming response is the only content that changes between frames,
        // so it is the only thing re-rendered here.
        let receiving_items = self
            .receiving_response
            .as_ref()
            .map(|receiving_response| receiving_response.get_message_items())
            .unwrap_or_default();
        self.live_paragraphs = receiving_items
            .iter()
            .enumerate()
            .map(|(i, (output_item, in_progress))| {
                let (paragraph, copy_buttons) = AssistantMessage::new(output_item, content_width)
                    .in_progress(*in_progress)
                    .expanded(self.live_expanded_items.contains(&i))
                    .frame_count(self.frame_count)
                    .into_paragraph();
                let height = paragraph.line_count(content_width) as u16;
                (paragraph, height, copy_buttons)
            })
            .collect();
        for (_, height, _) in &self.live_paragraphs {
            content_height = content_height.saturating_add(*height);
        }

        let pending = self.pending_message.as_ref().map(|text| {
            let paragraph = PendingMessage::new(text).into_paragraph();
            let height = paragraph.line_count(content_width) as u16;
            (paragraph, height)
        });
        if let Some((_, height)) = &pending {
            content_height = content_height.saturating_add(*height);
        }

        content_height = content_height.max(area.height);

        // Follow the bottom while the user hasn't scrolled up. Doing this here
        // (rather than re-snapping on every incoming chunk) is what lets manual
        // scrolling stick during streaming.
        if stick_to_bottom {
            self.scroll_view_state.scroll_to_bottom();
        }

        // Only the rows in the current scroll window are visible, so skip
        // rendering paragraphs that fall entirely outside it. `render_widget`
        // (re)wraps and writes every cell of a paragraph, so culling off-screen
        // ones turns each frame from O(whole conversation) into O(viewport).
        //
        // The offset is clamped exactly as `ScrollView::render` does below, which
        // also resolves the `u16::MAX` sentinel that `scroll_to_bottom` leaves in
        // the state (used every frame while auto-following a streaming reply).
        let max_y_offset = content_height.saturating_sub(area.height);
        let visible_top = self.scroll_view_state.offset().y.min(max_y_offset);
        let visible_bottom = visible_top.saturating_add(area.height);
        let visible =
            |y: u16, height: u16| y < visible_bottom && y.saturating_add(height) > visible_top;

        let mut scroll_view = ScrollView::new(Size::new(content_width, content_height));
        let mut y = 0u16;
        if visible(y, welcome_height) {
            scroll_view.render_widget(
                &welcome_message,
                Rect::new(0, y, content_width, welcome_height),
            );
        }
        y = y.saturating_add(welcome_height);

        // Record each item's vertical extent (in buffer coordinates) so a click
        // can be mapped back to the item under the cursor.
        let mut layout: Vec<(usize, u16, u16)> = Vec::with_capacity(cache.entries.len());
        for (index, entry) in cache.entries.iter().enumerate() {
            layout.push((index, y, y.saturating_add(entry.height)));
            if visible(y, entry.height) {
                scroll_view.render_widget(
                    &entry.paragraph,
                    Rect::new(0, y, content_width, entry.height),
                );
            }
            y = y.saturating_add(entry.height);
        }
        let mut live_layout: Vec<(usize, u16, u16)> =
            Vec::with_capacity(self.live_paragraphs.len());
        for (i, (paragraph, height, _)) in self.live_paragraphs.iter().enumerate() {
            live_layout.push((i, y, y.saturating_add(*height)));
            if visible(y, *height) {
                scroll_view.render_widget(paragraph, Rect::new(0, y, content_width, *height));
            }
            y = y.saturating_add(*height);
        }
        self.pending_layout = pending.as_ref().map(|(_, height)| (y, *height));
        if let Some((paragraph, height)) = &pending
            && visible(y, *height) {
                scroll_view.render_widget(paragraph, Rect::new(0, y, content_width, *height));
            }
        scroll_view.render(area, buf, &mut self.scroll_view_state);

        // The scroll view has now clamped the offset to its real value; store it
        // and the layout for click hit-testing on the next event.
        let offset = self.scroll_view_state.offset().y;

        // Paint the mouse selection as reversed cells on top of the rendered
        // content (screen coordinates, visible rows only).
        if let Some(sel) = self.selection {
            for screen_row in 0..area.height {
                let buffer_row = offset.saturating_add(screen_row);
                if let Some((from, to)) = sel.row_range(buffer_row, content_width) {
                    for x in from..=to {
                        if let Some(cell) = buf.cell_mut((area.x + x, area.y + screen_row)) {
                            cell.set_style(Style::new().add_modifier(Modifier::REVERSED));
                        }
                    }
                }
            }
        }

        // "Jump to bottom" indicator: shown only while scrolled up, at the
        // bottom-right of the view. Clickable (see the mouse handler).
        if area.height > 0 && !self.scroll_view_state.is_at_bottom() {
            let label = " \u{2193} latest ";
            let w = label.chars().count() as u16;
            if area.width >= w {
                let x = area.x + area.width - w;
                let y = area.y + area.height - 1;
                buf.set_string(
                    x,
                    y,
                    label,
                    Style::new()
                        .fg(palette::TEXT)
                        .bg(palette::SURFACE)
                        .add_modifier(Modifier::BOLD),
                );
                self.set_jump_button(Some(Rect { x, y, width: w, height: 1 }));
            } else {
                self.set_jump_button(None);
            }
        } else {
            self.set_jump_button(None);
        }

        self.set_layout(area, offset, layout, live_layout);
    }
}
