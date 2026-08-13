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
    ActivePhase, CachedLiveSlot, CachedParagraph, CachedToolGroup, ConversationPanel,
    LiveGroupHeader, LiveParagraph, LiveRenderCache, MaterializedLiveCache, ToolGroupLayout,
};
use crate::ui::components::conversation_panel::tool_group::{
    MemberHeader, ToolGroup, ToolGroupMember, build_tool_group_paragraph,
    discover_tool_group_bridge, discover_tool_groups, function_call, is_hidden_runtime_tool,
};
use crate::ui::components::messages::assistant_message::AssistantMessage;
use crate::ui::components::messages::compacting_message::CompactingMessage;
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
use async_openai::types::responses::{
    FunctionCallOutputItemParam, InputItem, InputRole, Item, MessageItem as ApiMessageItem,
    OutputItem, ReasoningItem,
};
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{StatefulWidget, Widget};
use ratatui_widgets::paragraph::Paragraph;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tui_scrollview::{ScrollView, ScrollbarVisibility};

/// Rough height estimate for an item, used to avoid expensive markdown rendering
/// for items far from the viewport. Returns a u16 line count.
fn estimate_item_height(item: &MessageItem, width: u16) -> u16 {
    let w = width.max(40) as usize;
    match item {
        MessageItem::Input(input) if is_hidden_developer_input(input) => 0,
        MessageItem::Input(input) => {
            let text = crate::app::helpers::extract_input_text(input).unwrap_or_default();
            rough_line_count(&text, w).saturating_add(input_image_rows(input))
        }
        MessageItem::Output(output) => match output {
            OutputItem::Reasoning(_) => 3, // rough
            OutputItem::Message(_) => 4,
            OutputItem::FunctionCall(call) if is_hidden_runtime_tool(call) => 0,
            OutputItem::FunctionCall(_) => 2,
            _ => 1,
        },
        MessageItem::ToolOutput { output, .. } => match &output.output {
            async_openai::types::responses::FunctionCallOutput::Text(t) => {
                rough_line_count(t, w).min(20)
            }
            async_openai::types::responses::FunctionCallOutput::Content(content) => {
                crate::ui::image_preview::estimated_rows(content).max(1)
            }
        },
        MessageItem::OpenAIError(_)
        | MessageItem::Error(_)
        | MessageItem::Warning(_)
        | MessageItem::Info(_) => 1,
        MessageItem::Meta { .. } => 1,
        MessageItem::Usage(_, _) => 1,
        // Usually collapsed to its one-line divider (like Reasoning, the
        // estimate ignores the expanded state — the real height comes from the
        // built paragraph).
        MessageItem::Compacted { .. } => 1,
    }
}

fn input_image_rows(input: &InputItem) -> u16 {
    match input {
        InputItem::Item(Item::Message(ApiMessageItem::Input(message))) => {
            crate::ui::image_preview::estimated_rows(&message.content)
        }
        InputItem::EasyMessage(message) => match &message.content {
            async_openai::types::responses::EasyInputContent::ContentList(content) => {
                crate::ui::image_preview::estimated_rows(content)
            }
            async_openai::types::responses::EasyInputContent::Text(_) => 0,
        },
        _ => 0,
    }
}

fn is_hidden_developer_input(input: &InputItem) -> bool {
    matches!(
        input,
        InputItem::Item(Item::Message(ApiMessageItem::Input(message)))
            if message.role == InputRole::Developer
    )
}

/// Rough line count: chars / width, plus explicit newlines.
fn rough_line_count(text: &str, width: usize) -> u16 {
    let lines: u16 = text.lines().count() as u16;
    let wrapped: u16 = text
        .lines()
        .map(|l| (l.chars().count().max(1) / width.max(1)).max(1) as u16)
        .sum();
    lines.max(wrapped).max(1)
}

fn live_cache_matches(
    cache: &LiveRenderCache,
    response_revision: u64,
    content_width: u16,
    conversation_len: usize,
    conversation_mutation_version: u64,
    expanded_items: &HashSet<usize>,
    live_expanded_items: &HashSet<usize>,
) -> bool {
    cache.response_revision == response_revision
        && cache.content_width == content_width
        && cache.conversation_len == conversation_len
        && cache.conversation_mutation_version == conversation_mutation_version
        && cache.expanded_items == *expanded_items
        && cache.live_expanded_items == *live_expanded_items
}

fn materialize_live_cache(
    cache: &LiveRenderCache,
    live_expanded_groups: &HashSet<String>,
    expanded_tool_groups: &HashSet<String>,
) -> MaterializedLiveCache {
    let mut paragraphs = Vec::with_capacity(cache.slots.len());
    let mut headers = Vec::with_capacity(cache.slots.len());
    for slot in &cache.slots {
        let (paragraph, group_header) =
            materialize_live_slot(slot, live_expanded_groups, expanded_tool_groups);
        paragraphs.push(paragraph);
        headers.push(group_header);
    }
    (paragraphs, headers)
}

fn materialize_live_slot(
    slot: &CachedLiveSlot,
    live_expanded_groups: &HashSet<String>,
    expanded_tool_groups: &HashSet<String>,
) -> (LiveParagraph, LiveGroupHeader) {
    match slot {
        CachedLiveSlot::Fixed {
            paragraph,
            group_header,
        } => (paragraph.clone(), group_header.clone()),
        CachedLiveSlot::Explore {
            group_key,
            collapsed,
            collapsed_headers,
            expanded,
            expanded_headers,
        } => {
            let is_expanded = live_expanded_groups.contains(group_key)
                || expanded_tool_groups.contains(group_key);
            let (paragraph, headers) = if is_expanded {
                (expanded, expanded_headers)
            } else {
                (collapsed, collapsed_headers)
            };
            (
                paragraph.clone(),
                Some((group_key.clone(), headers.clone())),
            )
        }
    }
}

fn empty_live_slot() -> CachedLiveSlot {
    CachedLiveSlot::Fixed {
        paragraph: (Arc::new(Paragraph::new("")), 0, Vec::new()),
        group_header: None,
    }
}

#[allow(clippy::too_many_arguments)]
fn build_live_group_slot<'a>(
    group: &ToolGroup,
    conversation_items: &'a [MessageItem],
    receiving_items: &'a [(OutputItem, bool)],
    outputs_by_call: &HashMap<&'a str, (&'a FunctionCallOutputItemParam, bool, Option<&'a str>)>,
    expanded_items: &HashSet<usize>,
    live_expanded_items: &HashSet<usize>,
    live_index_base: usize,
    content_width: u16,
    group_expanded: bool,
) -> CachedLiveSlot {
    let live_outputs: Vec<Option<String>> = group
        .member_indices
        .iter()
        .map(|&member_index| {
            if member_index >= live_index_base {
                return None;
            }
            let call = function_call(&conversation_items[member_index]).unwrap();
            (!outputs_by_call.contains_key(call.call_id.as_str())
                && call.name == crate::tools::command::NAME)
                .then(|| crate::tools::command::live_output(&call.call_id))
                .flatten()
        })
        .collect();
    let members: Vec<ToolGroupMember<'_>> = group
        .member_indices
        .iter()
        .enumerate()
        .map(|(position, &member_index)| {
            if member_index < live_index_base {
                let call = function_call(&conversation_items[member_index]).unwrap();
                return ToolGroupMember {
                    index: member_index,
                    call,
                    output: outputs_by_call.get(call.call_id.as_str()).copied(),
                    live_output: live_outputs[position].as_deref(),
                    expanded: expanded_items.contains(&member_index),
                };
            }
            let live_index = member_index - live_index_base;
            let call = match &receiving_items[live_index].0 {
                OutputItem::FunctionCall(call) => call,
                _ => unreachable!("group members are calls"),
            };
            ToolGroupMember {
                index: member_index,
                call,
                output: None,
                live_output: None,
                expanded: live_expanded_items.contains(&live_index),
            }
        })
        .collect();
    let absorbed: Vec<(usize, &ReasoningItem, bool)> = group
        .absorbed
        .iter()
        .map(|&index| {
            if index < live_index_base {
                let item = match &conversation_items[index] {
                    MessageItem::Output(OutputItem::Reasoning(item)) => item,
                    _ => unreachable!("absorbed items are reasoning"),
                };
                return (index, item, expanded_items.contains(&index));
            }
            let live_index = index - live_index_base;
            let item = match &receiving_items[live_index].0 {
                OutputItem::Reasoning(item) => item,
                _ => unreachable!("absorbed items are reasoning"),
            };
            (index, item, live_expanded_items.contains(&live_index))
        })
        .collect();
    let thought_in_progress = group
        .absorbed
        .iter()
        .chain(group.member_indices.iter())
        .filter(|&&index| index >= live_index_base)
        .any(|&index| receiving_items[index - live_index_base].1);

    if group.is_explore() {
        let (collapsed, collapsed_headers) = build_tool_group_paragraph(
            group,
            &members,
            &absorbed,
            content_width,
            false,
            thought_in_progress,
        );
        let collapsed_height = collapsed.line_count(content_width) as u16;
        let (expanded, expanded_headers) = build_tool_group_paragraph(
            group,
            &members,
            &absorbed,
            content_width,
            true,
            thought_in_progress,
        );
        let expanded_height = expanded.line_count(content_width) as u16;
        CachedLiveSlot::Explore {
            group_key: group.key.clone(),
            collapsed: (Arc::new(collapsed), collapsed_height, Vec::new()),
            collapsed_headers,
            expanded: (Arc::new(expanded), expanded_height, Vec::new()),
            expanded_headers,
        }
    } else {
        let (paragraph, member_headers) = build_tool_group_paragraph(
            group,
            &members,
            &absorbed,
            content_width,
            group_expanded,
            thought_in_progress,
        );
        let height = paragraph.line_count(content_width) as u16;
        CachedLiveSlot::Fixed {
            paragraph: (Arc::new(paragraph), height, Vec::new()),
            group_header: Some((group.key.clone(), member_headers)),
        }
    }
}

/// Builds the paragraph for a finished history item. Called at most once per
/// item (the result is cached in [`ConversationPanel::render_cache`]).
fn build_item_paragraph(
    item: &MessageItem,
    content_width: u16,
    expanded: bool,
    tool_output: Option<(&FunctionCallOutputItemParam, bool, Option<&str>)>,
    live_output: Option<&str>,
) -> (Paragraph<'static>, Vec<CodeCopyButton>) {
    match item {
        MessageItem::ToolOutput { output, failed, .. } => {
            // Only reached for orphan results whose call is missing; results
            // with a call render inside that call's item.
            (
                ToolResultMessage::new(output, content_width)
                    .failed(*failed)
                    .expanded(expanded)
                    .into_paragraph(),
                Vec::new(),
            )
        }
        MessageItem::Input(input_item) => (
            UserMessage::new(input_item, content_width).into_paragraph(),
            Vec::new(),
        ),
        MessageItem::Output(output_item) => AssistantMessage::new(output_item, content_width)
            .expanded(expanded)
            .tool_output(tool_output)
            .live_output(live_output)
            .into_paragraph(),
        MessageItem::OpenAIError(error) => (
            ErrorMessage::new(error.to_string()).into_paragraph(),
            Vec::new(),
        ),
        MessageItem::Error(message) => (
            ErrorMessage::new(message.clone()).into_paragraph(),
            Vec::new(),
        ),
        MessageItem::Info(message) => (
            InfoMessage::new(message.clone()).into_paragraph(),
            Vec::new(),
        ),
        MessageItem::Meta { label, .. } => (
            InfoMessage::new(format!("\u{25B8} {}", label)).into_paragraph(),
            Vec::new(),
        ),
        MessageItem::Warning(message) => (
            WarningMessage::new(message.clone()).into_paragraph(),
            Vec::new(),
        ),
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
        // Layout: one blank column of margin on the left and one on the right
        // (the right column doubles as the overlay scrollbar's track), so the
        // scrollbar never overlaps content.
        let left_margin = u16::from(area.width >= 3);
        let content_width = area.width.saturating_sub(left_margin + 1);
        let content_area = Rect {
            x: area.x + left_margin,
            width: content_width,
            ..area
        };
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

        let live_response_revision = self
            .receiving_response
            .as_ref()
            .map(|response| response.render_revision())
            .unwrap_or_default();
        let live_cache_hit = self.live_render_cache.as_ref().is_some_and(|cache| {
            live_cache_matches(
                cache,
                live_response_revision,
                content_width,
                conv.items.len(),
                conv.mutation_version,
                &self.expanded_items,
                &self.live_expanded_items,
            )
        });
        let (receiving_items, live_message_items, bridge_group, bridged_committed_items) =
            if live_cache_hit {
                (
                    Vec::new(),
                    Vec::new(),
                    None,
                    self.live_render_cache
                        .as_ref()
                        .unwrap()
                        .bridged_committed_items
                        .clone(),
                )
            } else {
                let receiving_items = self
                    .receiving_response
                    .as_ref()
                    .map(|receiving_response| receiving_response.get_message_items())
                    .unwrap_or_default();
                let live_message_items: Vec<MessageItem> = receiving_items
                    .iter()
                    .map(|(output_item, _)| MessageItem::Output(output_item.clone()))
                    .collect();
                let bridge_group = discover_tool_group_bridge(&conv.items, &live_message_items);
                let bridged_committed_items: HashSet<usize> = bridge_group
                    .iter()
                    .flat_map(|group| group.member_indices.iter().chain(group.absorbed.iter()))
                    .copied()
                    .filter(|&index| index < conv.items.len())
                    .collect();
                (
                    receiving_items,
                    live_message_items,
                    bridge_group,
                    bridged_committed_items,
                )
            };

        // Tool calls render together with their result as one message: map each
        // call_id to its result, and collect the call ids so the standalone
        // result items can be hidden (they draw inside their call's entry).
        let mut outputs_by_call: HashMap<&str, (&FunctionCallOutputItemParam, bool, Option<&str>)> =
            HashMap::new();
        for item in &conv.items {
            if let MessageItem::ToolOutput {
                output,
                failed,
                approval_label,
            } = item
            {
                outputs_by_call.insert(
                    output.call_id.as_str(),
                    (output, *failed, approval_label.as_deref()),
                );
            }
        }
        let call_ids: HashSet<&str> = conv
            .items
            .iter()
            .filter_map(|item| match item {
                MessageItem::Output(OutputItem::FunctionCall(call)) => Some(call.call_id.as_str()),
                _ => None,
            })
            .collect();
        let tool_groups = discover_tool_groups(&conv.items);
        let mut group_by_item = vec![None; conv.items.len()];
        for (group_index, group) in tool_groups.iter().enumerate() {
            for &item_index in group.member_indices.iter().chain(group.absorbed.iter()) {
                group_by_item[item_index] = Some(group_index);
            }
        }

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
        // An in-place mutation (the runner folding diagnostics into a tool
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
        for (index, &group_index) in group_by_item.iter().enumerate() {
            let mut h = if bridged_committed_items.contains(&index) {
                0
            } else if let Some(group_index) = group_index {
                let group = &tool_groups[group_index];
                if group.member_indices[0] != index {
                    0
                } else if self.expanded_tool_groups.contains(&group.key) {
                    1 + group
                        .member_indices
                        .iter()
                        .chain(group.absorbed.iter())
                        .map(|&member| estimate_item_height(&conv.items[member], content_width))
                        .sum::<u16>()
                } else {
                    2
                }
            } else if matches!(&conv.items[index], MessageItem::ToolOutput { output, .. }
                if call_ids.contains(output.call_id.as_str()))
            {
                0 // hidden inside its call's entry
            } else {
                estimate_item_height(&conv.items[index], content_width)
            };
            if let MessageItem::Output(OutputItem::FunctionCall(call)) = &conv.items[index]
                && let Some((output, _, _)) = outputs_by_call.get(call.call_id.as_str())
                && let async_openai::types::responses::FunctionCallOutput::Content(content) =
                    &output.output
            {
                h = h.saturating_add(crate::ui::image_preview::estimated_rows(content));
            }
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

        for (index, &group_index) in group_by_item.iter().enumerate() {
            let expanded = self.expanded_items.contains(&index);
            let group = group_index.map(|group_index| &tool_groups[group_index]);
            let is_group_start = group.is_some_and(|group| group.member_indices[0] == index);
            let hidden_group_member = group.is_some() && !is_group_start;
            let bridged_group_member = bridged_committed_items.contains(&index);
            let (hidden, tool_output) = match &conv.items[index] {
                MessageItem::ToolOutput { output, .. } => {
                    (call_ids.contains(output.call_id.as_str()), None)
                }
                MessageItem::Output(OutputItem::FunctionCall(call)) => (
                    is_hidden_runtime_tool(call),
                    outputs_by_call.get(call.call_id.as_str()).copied(),
                ),
                MessageItem::Input(input) => (is_hidden_developer_input(input), None),
                _ => (false, None),
            };
            let hidden = hidden || hidden_group_member || bridged_group_member;
            let has_output = tool_output.is_some();
            let effective_has_output = group
                .filter(|_| is_group_start)
                .map(|group| {
                    group.member_indices.iter().all(|&member_index| {
                        let call = function_call(&conv.items[member_index]).unwrap();
                        outputs_by_call.contains_key(call.call_id.as_str())
                    })
                })
                .unwrap_or(has_output);
            // A still-running `command` call streams its output live: fetch the
            // in-flight buffer and force a rebuild each frame while it grows, so
            // the panel shows output as it arrives rather than only the final
            // committed result.
            let live_output = match &conv.items[index] {
                MessageItem::Output(OutputItem::FunctionCall(call))
                    if !has_output && call.name == crate::tools::command::NAME =>
                {
                    crate::tools::command::live_output(&call.call_id)
                }
                _ => None,
            };
            let in_viewport = index >= build_from || in_window[index];
            let group_render_key = group.filter(|_| is_group_start).map(|group| {
                // Open runs stay collapsed by default, same as closed ones;
                // only the user's explicit expansion opens a group.
                let group_expanded = self.expanded_tool_groups.contains(&group.key);
                let member_states = group.member_indices.iter().map(|&member_index| {
                    let call = function_call(&conv.items[member_index]).unwrap();
                    let output = outputs_by_call.get(call.call_id.as_str());
                    format!(
                        "{}{}",
                        usize::from(self.expanded_items.contains(&member_index)),
                        output.map_or('p', |(_, failed, _)| if *failed { 'f' } else { 's' })
                    )
                });
                let absorbed_states = group
                    .absorbed
                    .iter()
                    .map(|&index| usize::from(self.expanded_items.contains(&index)).to_string());
                format!(
                    "{}:{}:{}",
                    usize::from(group_expanded),
                    member_states.collect::<String>(),
                    absorbed_states.collect::<String>()
                )
            });
            let group_has_live_output = group.filter(|_| is_group_start).is_some_and(|group| {
                self.expanded_tool_groups.contains(&group.key)
                    && group.member_indices.iter().any(|&member_index| {
                        let call = function_call(&conv.items[member_index]).unwrap();
                        !outputs_by_call.contains_key(call.call_id.as_str())
                            && call.name == crate::tools::command::NAME
                            && crate::tools::command::live_output(&call.call_id).is_some()
                    })
            });
            let needs_build = live_output.is_some()
                || group_has_live_output
                || cache.entries.get(index).is_none_or(|entry| {
                    let cached_group_key = entry
                        .tool_group
                        .as_ref()
                        .map(|group| group.render_key.as_str());
                    entry.expanded != expanded
                        || entry.has_output != effective_has_output
                        || cached_group_key != group_render_key.as_deref()
                        || entry.hidden != hidden
                        || (entry.lazy && in_viewport)
                });
            if needs_build {
                let entry = if hidden {
                    CachedParagraph {
                        paragraph: Paragraph::new(""),
                        height: 0,
                        hidden: true,
                        expanded,
                        has_output: effective_has_output,
                        copy_buttons: Vec::new(),
                        lazy: false,
                        tool_group: None,
                    }
                } else if !in_viewport {
                    CachedParagraph {
                        paragraph: Paragraph::new(""),
                        height: est_heights[index],
                        hidden: false,
                        expanded,
                        has_output: effective_has_output,
                        copy_buttons: Vec::new(),
                        lazy: true,
                        tool_group: group
                            .filter(|_| is_group_start)
                            .map(|group| CachedToolGroup {
                                key: group.key.clone(),
                                render_key: group_render_key.clone().unwrap(),
                                member_headers: Vec::new(),
                            }),
                    }
                } else if let Some(group) = group.filter(|_| is_group_start) {
                    let group_expanded = self.expanded_tool_groups.contains(&group.key);
                    let live_outputs: Vec<Option<String>> = group
                        .member_indices
                        .iter()
                        .map(|&member_index| {
                            let call = function_call(&conv.items[member_index]).unwrap();
                            (!outputs_by_call.contains_key(call.call_id.as_str())
                                && call.name == crate::tools::command::NAME)
                                .then(|| crate::tools::command::live_output(&call.call_id))
                                .flatten()
                        })
                        .collect();
                    let members: Vec<ToolGroupMember<'_>> = group
                        .member_indices
                        .iter()
                        .enumerate()
                        .map(|(position, &member_index)| {
                            let call = function_call(&conv.items[member_index]).unwrap();
                            ToolGroupMember {
                                index: member_index,
                                call,
                                output: outputs_by_call.get(call.call_id.as_str()).copied(),
                                live_output: live_outputs[position].as_deref(),
                                expanded: self.expanded_items.contains(&member_index),
                            }
                        })
                        .collect();
                    let absorbed: Vec<(usize, &ReasoningItem, bool)> = group
                        .absorbed
                        .iter()
                        .map(|&index| {
                            let item = match &conv.items[index] {
                                MessageItem::Output(OutputItem::Reasoning(item)) => item,
                                _ => unreachable!("absorbed items are reasoning"),
                            };
                            (index, item, self.expanded_items.contains(&index))
                        })
                        .collect();
                    let (paragraph, member_headers) = build_tool_group_paragraph(
                        group,
                        &members,
                        &absorbed,
                        content_width,
                        group_expanded,
                        false,
                    );
                    let height = paragraph.line_count(content_width) as u16;
                    CachedParagraph {
                        paragraph,
                        height,
                        hidden: false,
                        expanded,
                        has_output: effective_has_output,
                        copy_buttons: Vec::new(),
                        lazy: false,
                        tool_group: Some(CachedToolGroup {
                            key: group.key.clone(),
                            render_key: group_render_key.clone().unwrap(),
                            member_headers,
                        }),
                    }
                } else {
                    let (paragraph, copy_buttons) = build_item_paragraph(
                        &conv.items[index],
                        content_width,
                        expanded,
                        tool_output,
                        live_output.as_deref(),
                    );
                    let height = paragraph.line_count(content_width) as u16;
                    CachedParagraph {
                        paragraph,
                        height,
                        hidden: false,
                        expanded,
                        has_output: effective_has_output,
                        copy_buttons,
                        lazy: false,
                        tool_group: None,
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

        let live_index_base = conv.items.len();
        if live_cache_hit {
            #[cfg(test)]
            {
                self.live_explore_cache_hits = self.live_explore_cache_hits.wrapping_add(1);
            }
            let (paragraphs, headers) = materialize_live_cache(
                self.live_render_cache.as_ref().unwrap(),
                &self.live_expanded_groups,
                &self.expanded_tool_groups,
            );
            self.live_paragraphs = paragraphs;
            self.live_group_headers = headers;
        } else {
            // Generate a new live cache only when stream content or nested view
            // state changes. Explore groups pre-render both outer fold states.
            let mut live_groups = discover_tool_groups(&live_message_items);
            for group in &mut live_groups {
                group.offset_indices(live_index_base);
            }
            if let Some(bridge_group) = bridge_group {
                live_groups.retain(|group| {
                    !group
                        .member_indices
                        .iter()
                        .chain(group.absorbed.iter())
                        .any(|&index| {
                            bridge_group
                                .member_indices
                                .iter()
                                .chain(bridge_group.absorbed.iter())
                                .any(|&bridge_index| bridge_index == index)
                        })
                });
                live_groups.push(bridge_group);
                live_groups.sort_by_key(|group| {
                    group
                        .member_indices
                        .iter()
                        .chain(group.absorbed.iter())
                        .filter(|&&index| index >= live_index_base)
                        .copied()
                        .min()
                        .unwrap_or(usize::MAX)
                });
            }
            let mut live_group_by_item = vec![None; receiving_items.len()];
            for (group_index, group) in live_groups.iter().enumerate() {
                for &item_index in group.member_indices.iter().chain(group.absorbed.iter()) {
                    if item_index >= live_index_base {
                        live_group_by_item[item_index - live_index_base] = Some(group_index);
                    }
                }
            }

            let mut live_group_headers = Vec::with_capacity(receiving_items.len());
            let mut live_paragraphs = Vec::with_capacity(receiving_items.len());
            let mut cache_slots = Vec::with_capacity(receiving_items.len());
            let mut cacheable = !receiving_items.is_empty();
            let mut saw_explore = false;
            for i in 0..receiving_items.len() {
                let (output_item, in_progress) = &receiving_items[i];
                let expanded = self.live_expanded_items.contains(&i);
                if let Some(group_index) = live_group_by_item[i] {
                    let group = &live_groups[group_index];
                    let first_live_index = group
                        .member_indices
                        .iter()
                        .chain(group.absorbed.iter())
                        .filter(|&&index| index >= live_index_base)
                        .copied()
                        .min()
                        .expect("live group has a streaming item")
                        - live_index_base;
                    if first_live_index != i {
                        let slot = empty_live_slot();
                        let (paragraph, group_header) = materialize_live_slot(
                            &slot,
                            &self.live_expanded_groups,
                            &self.expanded_tool_groups,
                        );
                        live_paragraphs.push(paragraph);
                        live_group_headers.push(group_header);
                        cache_slots.push(slot);
                        continue;
                    }
                    if group.is_explore() {
                        saw_explore = true;
                        #[cfg(test)]
                        {
                            self.live_explore_builds = self.live_explore_builds.wrapping_add(1);
                        }
                    } else {
                        cacheable = false;
                    }
                    let group_expanded = self.live_expanded_groups.contains(&group.key)
                        || self.expanded_tool_groups.contains(&group.key);
                    let slot = build_live_group_slot(
                        group,
                        &conv.items,
                        &receiving_items,
                        &outputs_by_call,
                        &self.expanded_items,
                        &self.live_expanded_items,
                        live_index_base,
                        content_width,
                        group_expanded,
                    );
                    let (paragraph, group_header) = materialize_live_slot(
                        &slot,
                        &self.live_expanded_groups,
                        &self.expanded_tool_groups,
                    );
                    live_paragraphs.push(paragraph);
                    live_group_headers.push(group_header);
                    cache_slots.push(slot);
                } else {
                    if matches!(output_item, OutputItem::FunctionCall(call) if is_hidden_runtime_tool(call))
                    {
                        let slot = empty_live_slot();
                        let (paragraph, group_header) = materialize_live_slot(
                            &slot,
                            &self.live_expanded_groups,
                            &self.expanded_tool_groups,
                        );
                        live_paragraphs.push(paragraph);
                        live_group_headers.push(group_header);
                        cache_slots.push(slot);
                        continue;
                    }
                    cacheable = false;
                    let (paragraph, copy_buttons) =
                        AssistantMessage::new(output_item, content_width)
                            .in_progress(*in_progress)
                            .expanded(expanded)
                            .frame_count(self.frame_count)
                            .into_paragraph();
                    let height = paragraph.line_count(content_width) as u16;
                    let paragraph = (Arc::new(paragraph), height, copy_buttons);
                    live_paragraphs.push(paragraph.clone());
                    live_group_headers.push(None);
                    cache_slots.push(CachedLiveSlot::Fixed {
                        paragraph,
                        group_header: None,
                    });
                }
            }
            self.live_paragraphs = live_paragraphs;
            self.live_group_headers = live_group_headers;
            self.live_render_cache = (cacheable && saw_explore).then(|| LiveRenderCache {
                response_revision: live_response_revision,
                content_width,
                conversation_len: conv.items.len(),
                conversation_mutation_version: conv.mutation_version,
                expanded_items: self.expanded_items.clone(),
                live_expanded_items: self.live_expanded_items.clone(),
                bridged_committed_items: bridged_committed_items.clone(),
                slots: cache_slots,
            });
        }
        for (_, height, _) in &self.live_paragraphs {
            content_height = content_height.saturating_add(*height);
        }

        let compacting = (self.phase == ActivePhase::Compacting).then(|| {
            let paragraph = CompactingMessage::into_paragraph();
            let height = paragraph.line_count(content_width) as u16;
            (paragraph, height)
        });
        if let Some((_, height)) = &compacting {
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

        // New content arriving while the user is scrolled up shifts their view
        // relative to the content — count that as scroll activity so the
        // scrollbar appears as a "new messages" cue. (Field-level mutation:
        // `cache` below still holds a mutable borrow of `self`.)
        if !stick_to_bottom && content_height > self.last_content_height {
            crate::ui::components::conversation_panel::conversation_panel::note_scroll_activity_field(
                &mut self.last_scroll_activity_at,
            );
        }
        self.last_content_height = content_height;

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

        let mut scroll_view = ScrollView::new(Size::new(content_width, content_height))
            .scrollbars_visibility(ScrollbarVisibility::Never);
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
        let mut tool_group_layout = Vec::new();
        for (index, entry) in cache.entries.iter().enumerate() {
            layout.push((index, y, y.saturating_add(entry.height)));
            if let Some(group) = &entry.tool_group {
                tool_group_layout.push(ToolGroupLayout {
                    key: group.key.clone(),
                    top: y,
                    bottom: y.saturating_add(entry.height),
                    live_index_base: None,
                    member_headers: group
                        .member_headers
                        .iter()
                        .map(|header| {
                            crate::ui::components::conversation_panel::tool_group::MemberHeader {
                                index: header.index,
                                top: y.saturating_add(header.top),
                                bottom: y.saturating_add(header.bottom),
                            }
                        })
                        .collect(),
                });
            }
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
        let mut live_tool_group_layout = Vec::new();
        for (i, (paragraph, height, _)) in self.live_paragraphs.iter().enumerate() {
            live_layout.push((i, y, y.saturating_add(*height)));
            if let Some((key, headers)) = &self.live_group_headers[i] {
                live_tool_group_layout.push(ToolGroupLayout {
                    key: key.clone(),
                    top: y,
                    bottom: y.saturating_add(*height),
                    live_index_base: Some(live_index_base),
                    member_headers: headers
                        .iter()
                        .map(|header| MemberHeader {
                            index: header.index,
                            top: y.saturating_add(header.top),
                            bottom: y.saturating_add(header.bottom),
                        })
                        .collect(),
                });
            }
            if visible(y, *height) {
                scroll_view
                    .render_widget(paragraph.as_ref(), Rect::new(0, y, content_width, *height));
            }
            y = y.saturating_add(*height);
        }
        if let Some((paragraph, height)) = &compacting {
            if visible(y, *height) {
                scroll_view.render_widget(paragraph, Rect::new(0, y, content_width, *height));
            }
            y = y.saturating_add(*height);
        }
        self.pending_layout = pending.as_ref().map(|(_, height)| (y, *height));
        if let Some((paragraph, height)) = &pending
            && visible(y, *height)
        {
            scroll_view.render_widget(paragraph, Rect::new(0, y, content_width, *height));
        }
        scroll_view.render(content_area, buf, &mut self.scroll_view_state);
        crate::ui::image_preview::render_protocol_images(content_area, buf);

        // The scroll view has now clamped the offset to its real value; store it
        // and the layout for click hit-testing on the next event.
        let offset = self.scroll_view_state.offset().y;

        // Draw a minimal custom scrollbar: no background, thin gray thumb. It
        // lives in the right margin column (never overlapping content) and only
        // appears for a short window after the user scrolls.
        if area.height > 0 && content_height > area.height && self.scrollbar_recently_active() {
            let scrollbar_x = content_area.right();
            let viewport_h = area.height as f64;
            let content_h = content_height as f64;
            let thumb_h = ((viewport_h / content_h) * viewport_h).max(1.0) as u16;
            let max_scroll = (content_height.saturating_sub(area.height)) as f64;
            let ratio = if max_scroll > 0.0 {
                offset as f64 / max_scroll
            } else {
                0.0
            };
            let thumb_top = ((area.height.saturating_sub(thumb_h)) as f64 * ratio) as u16;
            let thumb_bottom = thumb_top.saturating_add(thumb_h).min(area.height);
            for row in thumb_top..thumb_bottom {
                if let Some(cell) = buf.cell_mut((scrollbar_x, area.y + row)) {
                    cell.set_symbol("█")
                        .set_style(Style::new().fg(palette::MUTED));
                }
            }
        }

        // Paint the mouse selection as reversed cells on top of the rendered
        // content (screen coordinates, visible rows only).
        if let Some(sel) = self.selection {
            for screen_row in 0..area.height {
                let buffer_row = offset.saturating_add(screen_row);
                if let Some((from, to)) = sel.row_range(buffer_row, content_width) {
                    for x in from..=to {
                        if let Some(cell) = buf.cell_mut((content_area.x + x, area.y + screen_row))
                        {
                            cell.set_style(Style::new().add_modifier(Modifier::REVERSED));
                        }
                    }
                }
            }
        }

        // "Jump to bottom" indicator: shown only while scrolled up, at the
        // bottom-right of the content area. Clickable (see the mouse handler).
        if area.height > 0 && !self.scroll_view_state.is_at_bottom() {
            let label = " \u{2193} latest ";
            let w = label.chars().count() as u16;
            if content_area.width >= w {
                let x = content_area.right().saturating_sub(w);
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
                self.set_jump_button(Some(Rect {
                    x,
                    y,
                    width: w,
                    height: 1,
                }));
            } else {
                self.set_jump_button(None);
            }
        } else {
            self.set_jump_button(None);
        }

        self.set_layout(
            content_area,
            offset,
            layout,
            live_layout,
            tool_group_layout,
            live_tool_group_layout,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::is_hidden_developer_input;
    use async_openai::types::responses::{
        FunctionCallOutput, FunctionCallOutputItemParam, FunctionToolCall, InputContent,
        InputMessage, InputRole, InputTextContent, Item, MessageItem as ApiMessageItem, OutputItem,
        OutputStatus,
    };
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::widgets::Widget;

    use crate::tools::ToolOutput;
    use crate::ui::components::conversation_panel::conversation_panel::{
        ActivePhase, ConversationPanel,
    };

    #[test]
    fn developer_runtime_inputs_are_hidden_from_chat_rendering() {
        let input = Item::Message(ApiMessageItem::Input(InputMessage {
            content: vec![InputContent::InputText(InputTextContent {
                text: "runtime notification".to_string(),
            })],
            role: InputRole::Developer,
            status: Some(OutputStatus::Completed),
        }))
        .into();
        assert!(is_hidden_developer_input(&input));
    }

    #[test]
    fn compacting_phase_renders_a_transient_conversation_message() {
        let mut panel = ConversationPanel::new();
        panel.phase = ActivePhase::Compacting;

        let active = render_text(&mut panel, Rect::new(0, 0, 80, 8));
        assert!(active.contains("⧉  Compacting context…"), "{active}");

        panel.phase = ActivePhase::None;
        let finished = render_text(&mut panel, Rect::new(0, 0, 80, 8));
        assert!(!finished.contains("Compacting context"), "{finished}");
    }

    #[test]
    fn load_skill_call_and_instructions_are_hidden_from_chat_rendering() {
        let mut panel = ConversationPanel::new();
        panel
            .conversation
            .lock()
            .unwrap()
            .add_output(OutputItem::FunctionCall(tool_call(
                0,
                crate::tools::load_skill::NAME,
            )));
        panel.add_tool_output(ToolOutput {
            param: FunctionCallOutputItemParam {
                call_id: "call-0".into(),
                output: FunctionCallOutput::Text("very large private skill instructions".into()),
                id: None,
                status: None,
            },
            failed: false,
            approval_label: None,
        });

        let rendered = render_text(&mut panel, Rect::new(0, 0, 80, 24));
        assert!(!rendered.contains("load_skill"), "{rendered}");
        assert!(
            !rendered.contains("private skill instructions"),
            "{rendered}"
        );
        assert!(!rendered.contains("Used tools"), "{rendered}");
    }

    #[test]
    fn streaming_load_skill_call_is_hidden_immediately() {
        use crate::cancel::CancellationToken;
        use async_openai::types::responses::{ResponseOutputItemAddedEvent, ResponseStreamEvent};

        let mut panel = ConversationPanel::new();
        panel.receiving_response = Some(crate::response::partial_response::PartialResponse::new(
            CancellationToken::new(),
        ));
        panel.handle_response_stream_event(ResponseStreamEvent::ResponseOutputItemAdded(
            ResponseOutputItemAddedEvent {
                sequence_number: 0,
                output_index: 0,
                item: OutputItem::FunctionCall(tool_call(0, crate::tools::load_skill::NAME)),
            },
        ));

        let rendered = render_text(&mut panel, Rect::new(0, 0, 80, 24));
        assert!(!rendered.contains("load_skill"), "{rendered}");
        assert!(!rendered.contains("Used tools"), "{rendered}");
    }

    #[test]
    fn long_tool_run_collapses_and_refreshes_as_results_arrive() {
        let mut panel = ConversationPanel::new();
        for (index, name) in ["grep", "blob", "read_file"].into_iter().enumerate() {
            panel
                .conversation
                .lock()
                .unwrap()
                .add_output(OutputItem::FunctionCall(tool_call(index, name)));
        }
        let area = Rect::new(0, 0, 80, 24);

        let pending = render_text(&mut panel, area);
        assert!(pending.contains("Exploring… · 0/3"));
        assert!(!pending.contains("grep  {}"));

        let group_row = pending
            .lines()
            .position(|line| line.contains("Exploring"))
            .unwrap() as u16;
        panel.handle_click(2, group_row);
        assert!(panel.expanded_tool_groups.contains("call-0"));
        let expanded = render_text(&mut panel, area);
        assert!(expanded.contains("grep  {}"), "{expanded}");
        assert!(expanded.contains("blob  {}"), "{expanded}");
        assert!(expanded.contains("read_file  {}"), "{expanded}");

        let first_call_row = expanded
            .lines()
            .position(|line| line.contains("grep  {}"))
            .unwrap() as u16;
        panel.handle_click(2, first_call_row);
        assert!(panel.expanded_items.contains(&0));

        for index in 0..3 {
            panel.add_tool_output(ToolOutput {
                param: FunctionCallOutputItemParam {
                    call_id: format!("call-{index}"),
                    output: FunctionCallOutput::Text("done".into()),
                    id: None,
                    status: None,
                },
                failed: index == 1,
                approval_label: None,
            });
        }
        let completed = render_text(&mut panel, area);
        assert!(completed.contains("Explored · 1 failed"));
    }

    fn tool_call(index: usize, name: &str) -> FunctionToolCall {
        FunctionToolCall {
            arguments: "{}".into(),
            call_id: format!("call-{index}"),
            namespace: None,
            name: name.into(),
            id: None,
            status: None,
        }
    }

    fn render_text(panel: &mut ConversationPanel, area: Rect) -> String {
        let mut buffer = Buffer::empty(area);
        panel.render(area, &mut buffer);
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .filter_map(|x| buffer.cell((x, y)))
                    .map(|cell| cell.symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn streaming_thought_is_absorbed_into_the_tool_group() {
        // A thought plus a single call arrive in the live stream. Before this
        // fix the thought rendered standalone until the whole response was
        // committed; now the open run groups immediately and the thought is
        // absorbed into the group's muted summary line. The group stays
        // collapsed by default.
        use crate::cancel::CancellationToken;
        use async_openai::types::responses::{
            ReasoningItem, ResponseOutputItemAddedEvent, ResponseOutputItemDoneEvent,
            ResponseStreamEvent,
        };
        let mut panel = ConversationPanel::new();
        panel.receiving_response = Some(crate::response::partial_response::PartialResponse::new(
            CancellationToken::new(),
        ));
        let reasoning = ReasoningItem {
            id: None,
            summary: Vec::new(),
            content: None,
            encrypted_content: None,
            status: None,
        };
        panel.handle_response_stream_event(ResponseStreamEvent::ResponseOutputItemAdded(
            ResponseOutputItemAddedEvent {
                sequence_number: 0,
                output_index: 0,
                item: OutputItem::Reasoning(reasoning.clone()),
            },
        ));
        // Real Responses streams finish the reasoning item before starting the
        // function call. The old test omitted this event, so it never exercised
        // the state transition that made Thinking jump out of the group.
        panel.handle_response_stream_event(ResponseStreamEvent::ResponseOutputItemDone(
            ResponseOutputItemDoneEvent {
                sequence_number: 1,
                output_index: 0,
                item: OutputItem::Reasoning(reasoning),
            },
        ));
        let call = tool_call(0, "grep");
        panel.handle_response_stream_event(ResponseStreamEvent::ResponseOutputItemAdded(
            ResponseOutputItemAddedEvent {
                sequence_number: 2,
                output_index: 1,
                item: OutputItem::FunctionCall(call.clone()),
            },
        ));

        let area = Rect::new(0, 0, 80, 24);
        let rendered = render_text(&mut panel, area);
        // The group header is there and the group is collapsed: the call's
        // detail text is hidden behind the folded header.
        assert!(rendered.contains("Exploring… · 0/1"), "{rendered}");
        assert!(!rendered.contains("grep  {}"), "{rendered}");
        // The thought was absorbed into the group: it appears on the muted
        // summary line right under the header, still streaming, and its
        // status string is the last piece of that line.
        let header_row = rendered
            .lines()
            .position(|l| l.contains("Exploring"))
            .unwrap();
        let summary_row = rendered.lines().position(|l| l.contains("✻")).unwrap();
        assert_eq!(summary_row, header_row + 1, "{rendered}");
        let summary = rendered.lines().nth(summary_row).unwrap();
        assert!(summary.contains("grep"), "{summary}");
        let grep_pos = summary.find("grep").unwrap();
        let thought_pos = summary.find("✻").unwrap();
        assert!(thought_pos > grep_pos, "{summary}");
        assert!(summary.contains("✻ Thinking"), "{summary}");

        // Once the call itself is done, the absorbed state settles to Thought.
        panel.handle_response_stream_event(ResponseStreamEvent::ResponseOutputItemDone(
            ResponseOutputItemDoneEvent {
                sequence_number: 3,
                output_index: 1,
                item: OutputItem::FunctionCall(call),
            },
        ));
        let completed = render_text(&mut panel, area);
        assert!(completed.contains("✻ Thought"), "{completed}");
        assert!(!completed.contains("✻ Thinking"), "{completed}");

        // Clicking the header expands the live group, showing the call.
        panel.handle_click(2, header_row as u16);
        let expanded = render_text(&mut panel, area);
        assert!(expanded.contains("grep  {}"), "{expanded}");
    }

    #[test]
    fn live_explore_group_reuses_layout_until_content_or_view_changes() {
        use crate::cancel::CancellationToken;
        use async_openai::types::responses::{
            ReasoningItem, ResponseOutputItemAddedEvent, ResponseReasoningSummaryTextDeltaEvent,
            ResponseStreamEvent, SummaryPart, SummaryTextContent,
        };

        let mut panel = ConversationPanel::new();
        panel.receiving_response = Some(crate::response::partial_response::PartialResponse::new(
            CancellationToken::new(),
        ));
        panel.handle_response_stream_event(ResponseStreamEvent::ResponseOutputItemAdded(
            ResponseOutputItemAddedEvent {
                sequence_number: 0,
                output_index: 0,
                item: OutputItem::Reasoning(ReasoningItem {
                    id: Some("reasoning-0".into()),
                    summary: vec![SummaryPart::SummaryText(SummaryTextContent {
                        text: "cached reasoning".into(),
                    })],
                    content: None,
                    encrypted_content: None,
                    status: None,
                }),
            },
        ));
        panel.handle_response_stream_event(ResponseStreamEvent::ResponseOutputItemAdded(
            ResponseOutputItemAddedEvent {
                sequence_number: 1,
                output_index: 1,
                item: OutputItem::FunctionCall(tool_call(0, "grep")),
            },
        ));

        let area = Rect::new(0, 0, 80, 24);
        let collapsed = render_text(&mut panel, area);
        assert_eq!(panel.live_explore_builds, 1);
        let header_row = collapsed
            .lines()
            .position(|line| line.contains("Exploring"))
            .unwrap() as u16;

        panel.handle_click(2, header_row);
        let expanded = render_text(&mut panel, area);
        assert!(expanded.contains("cached reasoning"), "{expanded}");
        assert_eq!(
            panel.live_explore_builds, 1,
            "the expanded form must be generated with the message"
        );
        assert_eq!(panel.live_explore_cache_hits, 1);

        let unchanged = render_text(&mut panel, area);
        assert!(unchanged.contains("cached reasoning"), "{unchanged}");
        assert_eq!(
            panel.live_explore_builds, 1,
            "an unchanged animation/timer frame must reuse the live paragraph"
        );
        assert_eq!(panel.live_explore_cache_hits, 2);

        panel.handle_response_stream_event(ResponseStreamEvent::ResponseReasoningSummaryTextDelta(
            ResponseReasoningSummaryTextDeltaEvent {
                sequence_number: 2,
                item_id: "reasoning-0".into(),
                output_index: 0,
                summary_index: 0,
                delta: " updated".into(),
            },
        ));
        let updated = render_text(&mut panel, area);
        assert!(updated.contains("cached reasoning updated"), "{updated}");
        assert_eq!(panel.live_explore_builds, 2, "new content must invalidate");
    }

    #[test]
    fn streaming_tools_extend_the_existing_committed_group_without_splitting() {
        use crate::cancel::CancellationToken;
        use async_openai::types::responses::{
            ReasoningItem, ResponseOutputItemAddedEvent, ResponseOutputItemDoneEvent,
            ResponseStreamEvent,
        };

        let reasoning = || ReasoningItem {
            id: None,
            summary: Vec::new(),
            content: None,
            encrypted_content: None,
            status: None,
        };
        let mut panel = ConversationPanel::new();
        panel
            .conversation
            .lock()
            .unwrap()
            .add_output(OutputItem::Reasoning(reasoning()));
        panel
            .conversation
            .lock()
            .unwrap()
            .add_output(OutputItem::FunctionCall(tool_call(0, "grep")));
        panel.add_tool_output(ToolOutput {
            param: FunctionCallOutputItemParam {
                call_id: "call-0".into(),
                output: FunctionCallOutput::Text("done".into()),
                id: None,
                status: None,
            },
            failed: false,
            approval_label: None,
        });

        let area = Rect::new(0, 0, 80, 24);
        let committed = render_text(&mut panel, area);
        assert!(committed.contains("Explored"), "{committed}");

        panel.receiving_response = Some(crate::response::partial_response::PartialResponse::new(
            CancellationToken::new(),
        ));
        panel.handle_response_stream_event(ResponseStreamEvent::ResponseOutputItemAdded(
            ResponseOutputItemAddedEvent {
                sequence_number: 0,
                output_index: 0,
                item: OutputItem::Reasoning(reasoning()),
            },
        ));
        panel.handle_response_stream_event(ResponseStreamEvent::ResponseOutputItemDone(
            ResponseOutputItemDoneEvent {
                sequence_number: 1,
                output_index: 0,
                item: OutputItem::Reasoning(reasoning()),
            },
        ));
        for (output_index, name) in [(1usize, "blob"), (2usize, "read_file")] {
            panel.handle_response_stream_event(ResponseStreamEvent::ResponseOutputItemAdded(
                ResponseOutputItemAddedEvent {
                    sequence_number: output_index as u64 + 1,
                    output_index: output_index as u32,
                    item: OutputItem::FunctionCall(tool_call(output_index, name)),
                },
            ));
        }

        let streaming = render_text(&mut panel, area);
        assert_eq!(
            streaming
                .lines()
                .filter(|line| line.contains("Explor"))
                .count(),
            1,
            "the committed and live portions must render as one block:\n{streaming}"
        );
        assert!(streaming.contains("Exploring… · 1/3"), "{streaming}");
        assert!(streaming.contains("grep, blob, read_file"), "{streaming}");
        assert!(!streaming.contains("Explored · 1/1"), "{streaming}");

        // The block keeps the same shape when the runner commits the streamed
        // response; it must not briefly split or move during that hand-off.
        let newly_committed = panel
            .receiving_response
            .as_ref()
            .unwrap()
            .get_message_items();
        for (item, _) in newly_committed {
            panel.conversation.lock().unwrap().add_output(item);
        }
        panel.commit_live();
        let committed_again = render_text(&mut panel, area);
        assert_eq!(
            committed_again
                .lines()
                .filter(|line| line.contains("Explor"))
                .count(),
            1,
            "the commit hand-off must keep one block:\n{committed_again}"
        );
        assert!(
            committed_again.contains("Exploring… · 1/3"),
            "{committed_again}"
        );
    }

    #[test]
    fn cancelling_a_bridged_stream_restores_ungrouped_committed_calls() {
        use crate::cancel::CancellationToken;
        use async_openai::types::responses::{ResponseOutputItemAddedEvent, ResponseStreamEvent};

        let mut panel = ConversationPanel::new();
        for (index, name) in ["grep", "blob"].into_iter().enumerate() {
            panel
                .conversation
                .lock()
                .unwrap()
                .add_output(OutputItem::FunctionCall(tool_call(index, name)));
        }
        let area = Rect::new(0, 0, 80, 24);
        let before = render_text(&mut panel, area);
        assert!(before.contains("grep  {}"), "{before}");
        assert!(before.contains("blob  {}"), "{before}");

        let cancel = CancellationToken::new();
        panel.receiving_response = Some(crate::response::partial_response::PartialResponse::new(
            cancel.clone(),
        ));
        panel.handle_response_stream_event(ResponseStreamEvent::ResponseOutputItemAdded(
            ResponseOutputItemAddedEvent {
                sequence_number: 0,
                output_index: 0,
                item: OutputItem::FunctionCall(tool_call(2, "read_file")),
            },
        ));
        let bridged = render_text(&mut panel, area);
        assert!(bridged.contains("Exploring… · 0/3"), "{bridged}");
        assert!(!bridged.contains("grep  {}"), "{bridged}");

        cancel.cancel();
        panel.abort_receiving();
        let restored = render_text(&mut panel, area);
        assert!(!restored.contains("Exploring"), "{restored}");
        assert!(restored.contains("grep  {}"), "{restored}");
        assert!(restored.contains("blob  {}"), "{restored}");
    }
}
