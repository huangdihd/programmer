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

//! Render-only grouping for long runs of assistant tool calls.
//!
//! The conversation remains a flat protocol transcript. This module derives
//! groups from that transcript for display, so grouping cannot reorder calls or
//! affect the request sent back to the model.
//!
//! A run that still extends to the end of the transcript is *open*: the model
//! may append more calls to it, so it is grouped as soon as it has a member
//! (once reasoning is involved) rather than waiting for the full run to close.
//! This is what lets a streaming thought be absorbed into its group the moment
//! the first call of the run appears, instead of lingering standalone until
//! enough calls have piled up. Groups stay collapsed by default; while
//! collapsed, a group with absorbed reasoning shows the current thought as a
//! muted summary line under its header, so a streaming thought stays visible
//! without forcing the whole group open. Closed runs keep the
//! [`MIN_TOOL_GROUP_SIZE`] threshold so short, finished sequences stay easy to
//! scan as ordinary calls.

use async_openai::types::responses::{FunctionCallOutputItemParam, FunctionToolCall, OutputItem};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Wrap;
use ratatui_widgets::block::{Block, Padding};
use ratatui_widgets::paragraph::Paragraph;

use crate::response::message_item::MessageItem;
use crate::ui::components::messages::assistant::reasoning::{
    ReasoningMessage, reasoning_summary_title,
};
use crate::ui::components::messages::assistant::tool_call::ToolCallMessage;
use crate::ui::components::messages::assistant_message::EXPANDED_BG;
use crate::ui::markdown_theme::palette;
use async_openai::types::responses::ReasoningItem;

/// Small sequences stay easier to scan as ordinary calls.
pub(crate) const MIN_TOOL_GROUP_SIZE: usize = 3;

/// How many tool names the collapsed summary shows before abbreviating with
/// `…`, keeping the muted detail line short enough to rarely wrap.
const SUMMARY_TOOL_CAP: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolGroup {
    /// Stable view-state key: the first call's protocol identifier.
    pub key: String,
    /// Call indices, in conversation order.
    pub member_indices: Vec<usize>,
    /// Reasoning-item indices absorbed into this group (in conversation
    /// order). They render inside the group's paragraph — interleaved with the
    /// calls — so a folded group hides them and an expanded one shows them
    /// where the model actually emitted them.
    pub absorbed: Vec<usize>,
    /// True when this run still extends to the end of the transcript, so more
    /// calls may be appended. Open groups are grouped from their first member
    /// but render collapsed by default, same as closed ones.
    pub open: bool,
    kind: ToolGroupKind,
}

impl ToolGroup {
    pub(crate) fn offset_indices(&mut self, offset: usize) {
        self.member_indices
            .iter_mut()
            .chain(self.absorbed.iter_mut())
            .for_each(|index| *index += offset);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ToolGroupKind {
    Explore,
    Research,
    Edit,
    Verify,
    Implement,
    Run,
    Plan,
    Configure,
    Mcp(String),
    General,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalToolKind {
    Explore,
    Research,
    Edit,
    Verify,
    Run,
    Plan,
    Configure,
    Other,
}

/// One member passed to the group renderer, including its current result and
/// nested expansion state.
pub(crate) struct ToolGroupMember<'a> {
    pub index: usize,
    pub call: &'a FunctionToolCall,
    pub output: Option<(&'a FunctionCallOutputItemParam, bool, Option<&'a str>)>,
    pub live_output: Option<&'a str>,
    pub expanded: bool,
}

/// The rows occupied by a member's clickable header, relative to the group's
/// paragraph. Details are intentionally not clickable, so selecting output
/// text never unexpectedly folds it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct MemberHeader {
    pub index: usize,
    pub top: u16,
    pub bottom: u16,
}

/// Find maximal runs of non-interactive function calls and retain runs with at
/// least [`MIN_TOOL_GROUP_SIZE`] members. Reasoning and tool outputs are
/// transparent: the Responses API may interleave reasoning between calls from
/// the same assistant batch, and a tool result is the machine-generated echo of
/// a call in the same run (rendered inside that call), so neither is a
/// user-visible interaction boundary. `ask_user` and `request_permission` stay
/// standalone because they are real interaction boundaries rather than batch
/// work.
///
/// The final run (the one still extending to the end of the transcript) is
/// *open* — more calls may still be appended. An open run is grouped as soon
/// as it has any member once reasoning is involved, so a thought is absorbed
/// the moment its first call appears instead of waiting for a full-size run.
pub(crate) fn discover_tool_groups(items: &[MessageItem]) -> Vec<ToolGroup> {
    let mut groups = Vec::new();
    let mut run = Vec::new();
    let mut absorbed = Vec::new();

    let flush = |run: &mut Vec<usize>,
                 absorbed: &mut Vec<usize>,
                 open: bool,
                 groups: &mut Vec<ToolGroup>| {
        let min = if !absorbed.is_empty() {
            1
        } else {
            MIN_TOOL_GROUP_SIZE
        };
        if run.len() >= min {
            let calls: Vec<&FunctionToolCall> = run
                .iter()
                .filter_map(|&index| function_call(&items[index]))
                .collect();
            groups.push(ToolGroup {
                key: calls[0].call_id.clone(),
                member_indices: std::mem::take(run),
                absorbed: std::mem::take(absorbed),
                open,
                kind: classify_group(&calls),
            });
        } else {
            run.clear();
            absorbed.clear();
        }
    };

    for (index, item) in items.iter().enumerate() {
        match function_call(item) {
            Some(call) if is_hidden_runtime_tool(call) => {}
            Some(call) if !is_interactive(call) => run.push(index),
            None if matches!(item, MessageItem::Output(OutputItem::Reasoning(_))) => {
                absorbed.push(index)
            }
            None if matches!(item, MessageItem::ToolOutput { .. }) => {}
            _ => flush(&mut run, &mut absorbed, false, &mut groups),
        }
    }
    flush(&mut run, &mut absorbed, true, &mut groups);
    groups
}

/// Find the open tool run that crosses from committed conversation history
/// into the currently streaming response. The renderer stores those two parts
/// separately, but they are one logical run and must therefore render as one
/// stable block while the response is still arriving.
pub(crate) fn discover_tool_group_bridge(
    committed: &[MessageItem],
    live: &[MessageItem],
) -> Option<ToolGroup> {
    if live.is_empty() {
        return None;
    }

    // Only the trailing run can continue into a new streamed response. Clone
    // that small suffix instead of the whole transcript on every frame.
    let tail_start = committed
        .iter()
        .rposition(|item| !continues_tool_run(item))
        .map_or(0, |index| index + 1);
    let committed_tail_len = committed.len().saturating_sub(tail_start);
    if committed_tail_len == 0 {
        return None;
    }

    let mut joined = committed[tail_start..].to_vec();
    joined.extend_from_slice(live);
    let mut group = discover_tool_groups(&joined).into_iter().find(|group| {
        let has_committed_call = group
            .member_indices
            .iter()
            .any(|&index| index < committed_tail_len);
        let has_live_item = group
            .member_indices
            .iter()
            .chain(group.absorbed.iter())
            .any(|&index| index >= committed_tail_len);
        has_committed_call && has_live_item
    })?;

    for index in group
        .member_indices
        .iter_mut()
        .chain(group.absorbed.iter_mut())
    {
        *index = if *index < committed_tail_len {
            tail_start + *index
        } else {
            committed.len() + (*index - committed_tail_len)
        };
    }
    Some(group)
}

fn continues_tool_run(item: &MessageItem) -> bool {
    match function_call(item) {
        Some(call) if is_hidden_runtime_tool(call) => true,
        Some(call) => !is_interactive(call),
        None => matches!(
            item,
            MessageItem::Output(OutputItem::Reasoning(_)) | MessageItem::ToolOutput { .. }
        ),
    }
}

/// Runtime plumbing that must remain in the protocol transcript but should not
/// appear as user-visible work or influence tool-run grouping.
pub(crate) fn is_hidden_runtime_tool(call: &FunctionToolCall) -> bool {
    call.name == crate::tools::load_skill::NAME
}

pub(crate) fn function_call(item: &MessageItem) -> Option<&FunctionToolCall> {
    match item {
        MessageItem::Output(OutputItem::FunctionCall(call)) => Some(call),
        _ => None,
    }
}

fn is_interactive(call: &FunctionToolCall) -> bool {
    matches!(
        call.name.as_str(),
        crate::tools::ask_user::NAME | crate::tools::request_permission::NAME
    )
}

fn classify_group(calls: &[&FunctionToolCall]) -> ToolGroupKind {
    let mcp_servers: Option<Vec<&str>> = calls.iter().map(|call| mcp_server(&call.name)).collect();
    if let Some(servers) = mcp_servers {
        if let Some(first) = servers.first()
            && servers.iter().all(|server| server == first)
        {
            return ToolGroupKind::Mcp((*first).to_string());
        }
        // MCP annotations describe safety, not semantic intent. Do not infer a
        // purpose from arbitrary external tool names or descriptions.
        return ToolGroupKind::General;
    }
    if calls.iter().any(|call| mcp_server(&call.name).is_some()) {
        return ToolGroupKind::General;
    }

    let kinds: Vec<LocalToolKind> = calls
        .iter()
        .map(|call| classify_local_tool(&call.name))
        .collect();
    let all = |kind| kinds.iter().all(|candidate| *candidate == kind);
    let has = |kind| kinds.contains(&kind);

    if all(LocalToolKind::Explore) {
        ToolGroupKind::Explore
    } else if all(LocalToolKind::Research) {
        ToolGroupKind::Research
    } else if all(LocalToolKind::Edit) {
        ToolGroupKind::Edit
    } else if all(LocalToolKind::Verify) {
        ToolGroupKind::Verify
    } else if has(LocalToolKind::Edit)
        && has(LocalToolKind::Verify)
        && kinds.iter().all(|kind| {
            matches!(
                kind,
                LocalToolKind::Explore | LocalToolKind::Edit | LocalToolKind::Verify
            )
        })
    {
        ToolGroupKind::Implement
    } else if all(LocalToolKind::Run) {
        ToolGroupKind::Run
    } else if all(LocalToolKind::Plan) {
        ToolGroupKind::Plan
    } else if all(LocalToolKind::Configure) {
        ToolGroupKind::Configure
    } else {
        ToolGroupKind::General
    }
}

fn classify_local_tool(name: &str) -> LocalToolKind {
    match name {
        crate::tools::grep::NAME
        | crate::tools::blob::NAME
        | crate::tools::read_file::NAME
        | crate::tools::read_image::NAME => LocalToolKind::Explore,
        crate::tools::fetch::NAME => LocalToolKind::Research,
        crate::tools::edit_file::NAME | crate::tools::write_file::NAME => LocalToolKind::Edit,
        crate::tools::diagnostics::NAME => LocalToolKind::Verify,
        crate::tools::command::NAME | crate::tools::task::NAME => LocalToolKind::Run,
        crate::tools::todo::NAME => LocalToolKind::Plan,
        crate::tools::configure_diagnostics::NAME => LocalToolKind::Configure,
        _ => LocalToolKind::Other,
    }
}

fn mcp_server(name: &str) -> Option<&str> {
    name.strip_prefix("mcp__")?
        .split_once("__")
        .map(|(server, _)| server)
}

impl ToolGroup {
    /// The shared MCP server when this group runs tools of a single server.
    fn mcp_server(&self) -> Option<&str> {
        match &self.kind {
            ToolGroupKind::Mcp(server) => Some(server),
            _ => None,
        }
    }

    pub(crate) fn title(&self, completed: usize, failed: usize) -> String {
        let total = self.member_indices.len();
        let mut title = if completed < total {
            format!("{}… · {completed}/{total}", self.kind.active_title())
        } else {
            self.kind.completed_title().to_string()
        };
        if failed > 0 {
            title.push_str(&format!(" · {failed} failed"));
        }
        title
    }
}

impl ToolGroupKind {
    fn active_title(&self) -> String {
        match self {
            Self::Explore => "Exploring".into(),
            Self::Research => "Researching".into(),
            Self::Edit => "Editing".into(),
            Self::Verify => "Verifying".into(),
            Self::Implement => "Implementing".into(),
            Self::Run => "Running tools".into(),
            Self::Plan => "Planning".into(),
            Self::Configure => "Configuring".into(),
            Self::Mcp(server) => format!("Using {server} tools"),
            Self::General => "Using tools".into(),
        }
    }

    fn completed_title(&self) -> String {
        match self {
            Self::Explore => "Explored".into(),
            Self::Research => "Researched".into(),
            Self::Edit => "Edited".into(),
            Self::Verify => "Verified".into(),
            Self::Implement => "Implemented".into(),
            Self::Run => "Ran tools".into(),
            Self::Plan => "Planned".into(),
            Self::Configure => "Configured".into(),
            Self::Mcp(server) => format!("Used {server} tools"),
            Self::General => "Used tools".into(),
        }
    }
}

/// Render a group header and, when open, its individually foldable calls and
/// absorbed reasoning items (in conversation order).
///
/// Collapsed, the header is followed by a muted summary line describing the
/// run — the tool names, the MCP server when the group is uniform, and the
/// current thought state last (`✻ Thinking…` while streaming, `✻ Thought` once
/// done) — so absorbed reasoning stays visible without expanding the group.
/// `thought_in_progress` reflects the live streaming state of the absorbed
/// reasoning (committed groups pass `false`).
pub(crate) fn build_tool_group_paragraph<'a>(
    group: &ToolGroup,
    members: &[ToolGroupMember<'a>],
    absorbed: &[(usize, &'a ReasoningItem, bool)],
    width: u16,
    expanded: bool,
    thought_in_progress: bool,
) -> (Paragraph<'static>, Vec<MemberHeader>) {
    let completed = members
        .iter()
        .filter(|member| member.output.is_some())
        .count();
    let failed = members
        .iter()
        .filter(|member| member.output.is_some_and(|(_, failed, _)| failed))
        .count();
    let color = if failed > 0 {
        palette::RED
    } else if completed == members.len() {
        palette::GREEN
    } else {
        palette::YELLOW
    };
    let arrow = if expanded { "\u{25BE}" } else { "\u{25B8}" };
    let muted = Style::new().fg(palette::MUTED);
    let mut lines = vec![Line::from(vec![
        Span::styled(format!("{arrow} "), muted),
        Span::styled(
            format!("\u{1F6E0} {}", group.title(completed, failed)),
            Style::new().fg(color).add_modifier(Modifier::BOLD),
        ),
    ])];
    // Collapsed groups get a muted summary line: which tools, the MCP server
    // when the group is uniform, and the thought state last.
    if !expanded {
        lines.push(Line::from(Span::styled(
            collapsed_summary(group, members, absorbed, thought_in_progress),
            muted,
        )));
    }
    let mut headers = Vec::new();
    let wrap = members.iter().any(|member| member.expanded)
        || absorbed.iter().any(|(_, _, expanded)| *expanded);
    let inner_width = width.saturating_sub(2).max(1);
    let mut rendered_rows = text_height(&Text::from(lines.clone()), inner_width, wrap);

    if expanded {
        // Calls and reasoning interleave in the transcript; render them in
        // conversation order so each thought sits next to the calls around it.
        let mut entries: Vec<(usize, &ToolGroupMember<'a>)> = Vec::with_capacity(members.len());
        entries.extend(members.iter().map(|member| (member.index, member)));
        let mut reasoning: Vec<(usize, &'a ReasoningItem, bool)> = absorbed.to_vec();
        entries.sort_by_key(|(index, _)| *index);
        reasoning.sort_by_key(|(index, _, _)| *index);
        let mut calls = entries.into_iter().peekable();
        let mut thoughts = reasoning.into_iter().peekable();
        while calls.peek().is_some() || thoughts.peek().is_some() {
            let next_call = calls.peek().map(|(index, _)| *index);
            let next_thought = thoughts.peek().map(|(index, _, _)| *index);
            if next_call.is_some_and(|call| next_thought.is_none_or(|thought| call < thought)) {
                let (_, member) = calls.next().unwrap();
                let text = ToolCallMessage::new(member.call, width)
                    .output(member.output.map(|(output, _, _)| output))
                    .failed(member.output.map(|(_, failed, _)| failed).unwrap_or(false))
                    .approval_label(member.output.and_then(|(_, _, label)| label))
                    .live_output(member.live_output)
                    .expanded(member.expanded)
                    .into_text();
                let height = text_height(&text, inner_width, wrap);
                headers.push(MemberHeader {
                    index: member.index,
                    top: rendered_rows,
                    bottom: rendered_rows.saturating_add(1),
                });
                rendered_rows = rendered_rows.saturating_add(height);
                lines.extend(text.lines);
            } else {
                let (index, item, thought_expanded) = thoughts.next().unwrap();
                let (text, _) = ReasoningMessage::new(false, item, width)
                    .expanded(thought_expanded)
                    .into_parts();
                let height = text_height(&text, inner_width, wrap);
                headers.push(MemberHeader {
                    index,
                    top: rendered_rows,
                    bottom: rendered_rows.saturating_add(1),
                });
                rendered_rows = rendered_rows.saturating_add(height);
                lines.extend(text.lines);
            }
        }
    }

    let block = Block::default()
        .padding(Padding::new(1, 1, 0, 1))
        .style(if expanded {
            Style::new().bg(EXPANDED_BG)
        } else {
            Style::new()
        });
    let mut paragraph = Paragraph::new(Text::from(lines)).block(block);
    if wrap {
        paragraph = paragraph.wrap(Wrap { trim: false });
    }
    (paragraph, headers)
}

fn text_height(text: &Text<'static>, width: u16, wrap: bool) -> u16 {
    let paragraph = Paragraph::new(text.clone());
    let paragraph = if wrap {
        paragraph.wrap(Wrap { trim: false })
    } else {
        paragraph
    };
    paragraph.line_count(width) as u16
}

/// The muted detail line under a collapsed group header: the tool names, the
/// MCP server when the group is uniform, and the thought state last.
fn collapsed_summary(
    group: &ToolGroup,
    members: &[ToolGroupMember<'_>],
    absorbed: &[(usize, &ReasoningItem, bool)],
    thought_in_progress: bool,
) -> String {
    let mut parts = Vec::with_capacity(4);
    let names: Vec<String> = members
        .iter()
        .map(|member| display_tool_name(&member.call.name))
        .collect();
    if names.len() > SUMMARY_TOOL_CAP {
        parts.push(format!("{}, …", names[..SUMMARY_TOOL_CAP].join(", ")));
    } else {
        parts.push(names.join(", "));
    }
    if let Some(server) = group.mcp_server() {
        parts.push(format!("via {server}"));
    }
    if !group.absorbed.is_empty() {
        let mut thought: String = if thought_in_progress {
            "✻ Thinking…".into()
        } else {
            "✻ Thought".into()
        };
        if let Some(summary) = absorbed
            .iter()
            .rev()
            .find_map(|(_, item, _)| reasoning_summary_title(item))
        {
            thought.push_str(" · ");
            thought.push_str(&summary);
        }
        parts.push(thought);
    }
    parts.join(" · ")
}

/// The tool name as shown in the collapsed summary: MCP calls drop their
/// `mcp__<server>__` prefix, local calls keep their name.
fn display_tool_name(name: &str) -> String {
    name.strip_prefix("mcp__")
        .and_then(|rest| rest.split_once("__"))
        .map(|(_, tool)| tool.to_string())
        .unwrap_or_else(|| name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_openai::types::responses::{
        FunctionCallOutput, ReasoningItem, SummaryPart, SummaryTextContent,
    };
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::widgets::Widget;

    fn call(index: usize, name: &str) -> MessageItem {
        MessageItem::Output(OutputItem::FunctionCall(FunctionToolCall {
            arguments: "{}".into(),
            call_id: format!("call-{index}"),
            namespace: None,
            name: name.into(),
            id: None,
            status: None,
        }))
    }

    #[test]
    fn groups_three_contiguous_calls_but_not_two() {
        let items = vec![
            call(0, "grep"),
            call(1, "blob"),
            call(2, "read_file"),
            MessageItem::Info("boundary".into()),
            call(3, "grep"),
            call(4, "read_file"),
        ];

        let groups = discover_tool_groups(&items);

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].key, "call-0");
        assert_eq!(groups[0].member_indices, [0, 1, 2]);
        assert_eq!(groups[0].title(3, 0), "Explored");
    }

    #[test]
    fn open_run_with_reasoning_groups_from_the_first_call() {
        // The transcript ends mid-run: only the thought and its first call have
        // arrived. The run is still open (more calls may follow), so the
        // thought must already be absorbed instead of lingering standalone.
        let reasoning = || {
            MessageItem::Output(OutputItem::Reasoning(ReasoningItem {
                id: None,
                summary: Vec::new(),
                content: None,
                encrypted_content: None,
                status: None,
            }))
        };
        let items = vec![reasoning(), call(0, "grep")];

        let groups = discover_tool_groups(&items);

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].member_indices, [1]);
        assert_eq!(groups[0].absorbed, [0]);
        assert!(groups[0].open);
    }

    #[test]
    fn open_run_without_reasoning_still_needs_three_calls() {
        // Bare calls (no interleaved reasoning) keep the size threshold even
        // while the run is open, so a lone call never gets a group header.
        let two_calls = vec![call(0, "grep"), call(1, "blob")];
        assert!(discover_tool_groups(&two_calls).is_empty());

        let three_calls = vec![call(0, "grep"), call(1, "blob"), call(2, "read_file")];
        let groups = discover_tool_groups(&three_calls);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].member_indices, [0, 1, 2]);
        assert!(groups[0].open);
    }

    #[test]
    fn closed_run_with_reasoning_stays_absorbed_from_the_first_call() {
        // Once a thought has been associated with a tool run, closing the run
        // must not dissolve the group and make the thought jump back out as a
        // standalone item.
        let reasoning = || {
            MessageItem::Output(OutputItem::Reasoning(ReasoningItem {
                id: None,
                summary: Vec::new(),
                content: None,
                encrypted_content: None,
                status: None,
            }))
        };
        let items = vec![
            reasoning(),
            call(0, "grep"),
            MessageItem::Info("boundary".into()),
        ];

        let groups = discover_tool_groups(&items);

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].member_indices, [1]);
        assert_eq!(groups[0].absorbed, [0]);
        assert!(!groups[0].open);
    }

    #[test]
    fn open_run_with_reasoning_remains_grouped_after_a_boundary() {
        let reasoning = || {
            MessageItem::Output(OutputItem::Reasoning(ReasoningItem {
                id: None,
                summary: Vec::new(),
                content: None,
                encrypted_content: None,
                status: None,
            }))
        };
        let items = vec![reasoning(), call(0, "grep"), call(1, "blob")];
        let groups = discover_tool_groups(&items);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].member_indices, [1, 2]);
        assert_eq!(groups[0].absorbed, [0]);
        assert!(groups[0].open);

        // The same run stays grouped after a boundary closes it.
        let mut closed = items;
        closed.push(MessageItem::Info("boundary".into()));
        let groups = discover_tool_groups(&closed);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].member_indices, [1, 2]);
        assert_eq!(groups[0].absorbed, [0]);
        assert!(!groups[0].open);
    }

    #[test]
    fn interactive_calls_break_groups() {
        let items = vec![
            call(0, "grep"),
            call(1, "blob"),
            call(2, "ask_user"),
            call(3, "grep"),
            call(4, "blob"),
            call(5, "read_file"),
        ];

        let groups = discover_tool_groups(&items);

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].member_indices, [3, 4, 5]);
    }

    #[test]
    fn load_skill_is_not_a_used_tools_group_member() {
        let items = vec![
            call(0, "grep"),
            call(1, crate::tools::load_skill::NAME),
            call(2, "blob"),
            call(3, "read_file"),
        ];

        let groups = discover_tool_groups(&items);

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].member_indices, [0, 2, 3]);
        assert_eq!(groups[0].title(3, 0), "Explored");
    }

    #[test]
    fn reasoning_does_not_break_a_tool_group() {
        let reasoning = || {
            MessageItem::Output(OutputItem::Reasoning(ReasoningItem {
                id: None,
                summary: Vec::new(),
                content: None,
                encrypted_content: None,
                status: None,
            }))
        };
        let items = vec![
            call(0, "grep"),
            reasoning(),
            call(1, "blob"),
            reasoning(),
            call(2, "read_file"),
        ];

        let groups = discover_tool_groups(&items);

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].member_indices, [0, 2, 4]);
        assert_eq!(groups[0].absorbed, [1, 3]);
        assert_eq!(groups[0].title(3, 0), "Explored");
    }

    #[test]
    fn serial_tool_outputs_do_not_break_a_tool_group() {
        // Real interleaved turn: each call streams back, is approved, runs, and
        // its result is committed before the next call arrives. The outputs are
        // machine-generated echoes of the same run, so they must not split it.
        let output = |index: usize| MessageItem::ToolOutput {
            output: FunctionCallOutputItemParam {
                call_id: format!("call-{index}"),
                output: FunctionCallOutput::Text("ok".into()),
                id: None,
                status: None,
            },
            failed: false,
            approval_label: Some("approved by Auto mode".into()),
        };
        let items = vec![
            call(0, "grep"),
            output(0),
            call(1, "blob"),
            output(1),
            call(2, "read_file"),
            output(2),
        ];

        let groups = discover_tool_groups(&items);

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].member_indices, [0, 2, 4]);
        assert_eq!(groups[0].absorbed, Vec::<usize>::new());
        assert_eq!(groups[0].title(3, 0), "Explored");
    }

    #[test]
    fn titles_track_lifecycle_and_failures() {
        let items = vec![
            call(0, "edit_file"),
            call(1, "diagnostics"),
            call(2, "read_file"),
        ];
        let group = discover_tool_groups(&items).pop().unwrap();

        assert_eq!(group.title(1, 0), "Implementing… · 1/3");
        assert_eq!(group.title(3, 1), "Implemented · 1 failed");
    }

    #[test]
    fn expanded_group_renders_absorbed_reasoning_in_conversation_order() {
        let reasoning = || {
            MessageItem::Output(OutputItem::Reasoning(ReasoningItem {
                id: None,
                summary: Vec::new(),
                content: None,
                encrypted_content: None,
                status: None,
            }))
        };
        let items = vec![
            call(0, "grep"),
            reasoning(),
            call(1, "blob"),
            reasoning(),
            call(2, "read_file"),
        ];
        let group = discover_tool_groups(&items).pop().unwrap();
        let members: Vec<ToolGroupMember<'_>> = group
            .member_indices
            .iter()
            .map(|&index| {
                let call = function_call(&items[index]).unwrap();
                ToolGroupMember {
                    index,
                    call,
                    output: None,
                    live_output: None,
                    expanded: false,
                }
            })
            .collect();
        let absorbed: Vec<(usize, &ReasoningItem, bool)> = group
            .absorbed
            .iter()
            .map(|&index| match &items[index] {
                MessageItem::Output(OutputItem::Reasoning(item)) => (index, item, false),
                _ => unreachable!(),
            })
            .collect();

        let (paragraph, _) =
            build_tool_group_paragraph(&group, &members, &absorbed, 80, true, false);
        let rendered = render_paragraph(&paragraph, 80, 40);
        let grep_row = rendered.lines().position(|l| l.contains("grep")).unwrap();
        let thought_rows = rendered
            .lines()
            .enumerate()
            .filter(|(_, l)| l.contains("✻ Thought"))
            .map(|(i, _)| i)
            .collect::<Vec<_>>();
        let blob_row = rendered.lines().position(|l| l.contains("blob")).unwrap();
        assert_eq!(thought_rows.len(), 2);
        // Interleaving: each thought sits between its surrounding calls.
        assert!(thought_rows[0] > grep_row && thought_rows[0] < blob_row);
        assert!(thought_rows[1] > blob_row);

        // Folded: the calls and thoughts disappear into the header, which now
        // carries a muted summary line: tool names, then the thought state.
        let (paragraph, _) =
            build_tool_group_paragraph(&group, &members, &absorbed, 80, false, false);
        let rendered = render_paragraph(&paragraph, 80, 40);
        assert!(!rendered.contains("grep  {}"), "{rendered}");
        assert!(rendered.contains("Exploring… · 0/3"), "{rendered}");
        let summary = rendered
            .lines()
            .find(|line| line.contains("✻ Thought"))
            .unwrap();
        assert!(summary.contains("grep, blob, read_file"), "{summary}");
        // The thought state is the last piece of the summary.
        let grep_pos = summary.find("read_file").unwrap();
        let thought_pos = summary.find("✻").unwrap();
        assert!(thought_pos > grep_pos, "{summary}");
    }

    #[test]
    fn collapsed_group_shows_reasoning_summary_while_thinking_and_when_done() {
        let items = vec![
            MessageItem::Output(OutputItem::Reasoning(ReasoningItem {
                id: None,
                summary: vec![SummaryPart::SummaryText(SummaryTextContent {
                    text: "Inspecting parser state".into(),
                })],
                content: None,
                encrypted_content: None,
                status: None,
            })),
            call(0, "grep"),
        ];
        let group = discover_tool_groups(&items).pop().unwrap();
        let call = function_call(&items[1]).unwrap();
        let members = [ToolGroupMember {
            index: 1,
            call,
            output: None,
            live_output: None,
            expanded: false,
        }];
        let reasoning = match &items[0] {
            MessageItem::Output(OutputItem::Reasoning(item)) => item,
            _ => unreachable!(),
        };
        let absorbed = [(0, reasoning, false)];

        let (thinking, _) =
            build_tool_group_paragraph(&group, &members, &absorbed, 80, false, true);
        let thinking = render_paragraph(&thinking, 80, 10);
        assert!(
            thinking.contains("✻ Thinking… · Inspecting parser state"),
            "{thinking}"
        );

        let (thought, _) =
            build_tool_group_paragraph(&group, &members, &absorbed, 80, false, false);
        let thought = render_paragraph(&thought, 80, 10);
        assert!(
            thought.contains("✻ Thought · Inspecting parser state"),
            "{thought}"
        );
    }

    fn render_paragraph(paragraph: &Paragraph<'static>, width: u16, height: u16) -> String {
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        paragraph.clone().render(area, &mut buffer);
        (0..height)
            .map(|y| {
                (0..width)
                    .filter_map(|x| buffer.cell((x, y)))
                    .map(|cell| cell.symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn mcp_titles_use_only_the_explicit_server_identity() {
        let same_server = vec![
            call(0, "mcp__browser__search"),
            call(1, "mcp__browser__open"),
            call(2, "mcp__browser__click"),
        ];
        let mixed_servers = vec![
            call(0, "mcp__browser__search"),
            call(1, "mcp__github__read"),
            call(2, "mcp__browser__open"),
        ];

        assert_eq!(
            discover_tool_groups(&same_server)[0].title(3, 0),
            "Used browser tools"
        );
        assert_eq!(
            discover_tool_groups(&mixed_servers)[0].title(0, 0),
            "Using tools… · 0/3"
        );
    }
}
