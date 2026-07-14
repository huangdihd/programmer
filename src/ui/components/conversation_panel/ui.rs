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
use async_openai::types::responses::{FunctionCallOutputItemParam, OutputItem};
use std::collections::{HashMap, HashSet};
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{StatefulWidget, Widget};
use ratatui_widgets::paragraph::Paragraph;
use tui_scrollview::ScrollView;

/// Builds the paragraph for a finished history item. Called at most once per
/// item (the result is cached in [`ConversationPanel::render_cache`]).
fn build_item_paragraph(
    item: &MessageItem,
    content_width: u16,
    expanded: bool,
    tool_output: Option<(&FunctionCallOutputItemParam, bool)>,
) -> (Paragraph<'static>, Vec<CodeCopyButton>) {
    match item {
        MessageItem::ToolOutput { output, failed } => {
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
        )
    }
}

impl Widget for &mut ConversationPanel {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.frame_count = self.frame_count.wrapping_add(1);
        let content_width = area.width.saturating_sub(1);
        let stick_to_bottom = self.stick_to_bottom;
        let welcome_message = WelcomeMessage::default();
        let welcome_height = welcome_message.line_count(content_width);
        let mut content_height: u16 = welcome_height;

        // Tool calls render together with their result as one message: map each
        // call_id to its result, and collect the call ids so the standalone
        // result items can be hidden (they draw inside their call's entry).
        let mut outputs_by_call: HashMap<&str, (&FunctionCallOutputItemParam, bool)> =
            HashMap::new();
        for item in &self.items {
            if let MessageItem::ToolOutput { output, failed } = item {
                outputs_by_call.insert(output.call_id.as_str(), (output, *failed));
            }
        }
        let call_ids: HashSet<&str> = self
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
            // Selection coordinates are relative to the old layout.
            self.selection = None;
        }
        if cache.entries.len() > self.items.len() {
            cache.entries.truncate(self.items.len());
        }
        for index in 0..self.items.len() {
            let expanded = self.expanded_items.contains(&index);
            let (hidden, tool_output) = match &self.items[index] {
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
            let needs_build = cache
                .entries
                .get(index)
                .map_or(true, |entry| {
                    entry.expanded != expanded || entry.has_output != has_output
                });
            if needs_build {
                let entry = if hidden {
                    // The result renders inside its call's entry; keep a
                    // zero-height placeholder so indices stay aligned.
                    CachedParagraph {
                        paragraph: Paragraph::new(""),
                        height: 0,
                        expanded,
                        has_output,
                        copy_buttons: Vec::new(),
                    }
                } else {
                    let (paragraph, copy_buttons) = build_item_paragraph(
                        &self.items[index],
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
        if let Some((paragraph, height)) = &pending {
            if visible(y, *height) {
                scroll_view.render_widget(paragraph, Rect::new(0, y, content_width, *height));
            }
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

        self.set_layout(area, offset, layout, live_layout);
    }
}
