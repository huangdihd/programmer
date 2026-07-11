use crate::ui::components::conversation_panel::conversation_panel::{CachedParagraph, ConversationPanel};
use crate::ui::components::messages::assistant_message::AssistantMessage;
use crate::ui::components::messages::user_message::UserMessage;
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use ratatui::widgets::{StatefulWidget, Widget};
use ratatui_widgets::paragraph::Paragraph;
use tui_scrollview::ScrollView;
use crate::response::message_item::MessageItem;
use crate::ui::components::messages::pending_message::PendingMessage;
use crate::ui::components::messages::welcome_message::WelcomeMessage;
use crate::ui::components::messages::error_message::ErrorMessage;
use crate::ui::components::messages::tool_result::ToolResultMessage;
use async_openai::types::responses::{InputItem, Item};

/// Builds the paragraph for a finished history item. Called at most once per
/// item (the result is cached in [`ConversationPanel::render_cache`]).
fn build_item_paragraph(item: &MessageItem, content_width: u16, expanded: bool) -> Paragraph<'static> {
    match item {
        MessageItem::Input(InputItem::Item(Item::FunctionCallOutput(output))) => {
            ToolResultMessage::new(output).expanded(expanded).into_paragraph()
        }
        MessageItem::Input(input_item) => UserMessage::new(input_item).into_paragraph(),
        MessageItem::Output(output_item) => {
            AssistantMessage::new(output_item, content_width)
                .expanded(expanded)
                .into_paragraph()
        }
        MessageItem::OpenAIError(error) => ErrorMessage::new(error.to_string()).into_paragraph(),
        MessageItem::Error(message) => ErrorMessage::new(message.clone()).into_paragraph(),
    }
}

impl Widget for &mut ConversationPanel {

    fn render(self, area: Rect, buf: &mut Buffer) {
        let content_width = area.width.saturating_sub(1);
        let stick_to_bottom = self.stick_to_bottom;
        let welcome_message = WelcomeMessage::default();
        let welcome_height = welcome_message.line_count(content_width);
        let mut content_height: u16 = welcome_height;

        // Refresh the cache of finished messages. `items` is append-only, so an
        // entry is only rebuilt when the width changes (invalidating all) or when
        // the user toggles that specific item's expand/collapse state.
        let cache = &mut self.render_cache;
        if cache.width != content_width {
            cache.width = content_width;
            cache.entries.clear();
        }
        if cache.entries.len() > self.items.len() {
            cache.entries.truncate(self.items.len());
        }
        for index in 0..self.items.len() {
            let expanded = self.expanded_items.contains(&index);
            let needs_build = cache
                .entries
                .get(index)
                .map_or(true, |entry| entry.expanded != expanded);
            if needs_build {
                let paragraph = build_item_paragraph(&self.items[index], content_width, expanded);
                let height = paragraph.line_count(content_width) as u16;
                let entry = CachedParagraph { paragraph, height, expanded };
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
        // so it is the only thing re-rendered here. Live items render collapsed.
        let receiving_items = self
            .receiving_response
            .as_ref()
            .map(|receiving_response| receiving_response.get_message_items())
            .unwrap_or_default();
        let live: Vec<(Paragraph<'static>, u16)> = receiving_items
            .iter()
            .map(|(output_item, in_progress)| {
                let paragraph = AssistantMessage::new(output_item, content_width)
                    .in_progress(*in_progress)
                    .into_paragraph();
                let height = paragraph.line_count(content_width) as u16;
                (paragraph, height)
            })
            .collect();
        for (_, height) in &live {
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
        let visible = |y: u16, height: u16| {
            y < visible_bottom && y.saturating_add(height) > visible_top
        };

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
                scroll_view.render_widget(&entry.paragraph, Rect::new(0, y, content_width, entry.height));
            }
            y = y.saturating_add(entry.height);
        }
        for (paragraph, height) in &live {
            if visible(y, *height) {
                scroll_view.render_widget(paragraph, Rect::new(0, y, content_width, *height));
            }
            y = y.saturating_add(*height);
        }
        if let Some((paragraph, height)) = &pending {
            if visible(y, *height) {
                scroll_view.render_widget(paragraph, Rect::new(0, y, content_width, *height));
            }
        }
        scroll_view.render(area, buf, &mut self.scroll_view_state);

        // The scroll view has now clamped the offset to its real value; store it
        // and the layout for click hit-testing on the next event.
        let offset = self.scroll_view_state.offset().y;
        self.set_layout(area, offset, layout);
    }
}