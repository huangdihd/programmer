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

use async_openai::types::responses::{FunctionCallOutputItemParam, FunctionToolCall, OutputItem};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Wrap;
use ratatui_widgets::block::{Block, Padding};
use ratatui_widgets::paragraph::Paragraph;

use crate::response::message_item::MessageItem;
use crate::ui::components::messages::assistant::tool_call::ToolCallMessage;
use crate::ui::components::messages::assistant_message::EXPANDED_BG;
use crate::ui::markdown_theme::palette;

/// Small sequences stay easier to scan as ordinary calls.
pub(crate) const MIN_TOOL_GROUP_SIZE: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolGroup {
    /// Stable view-state key: the first call's protocol identifier.
    pub key: String,
    pub member_indices: Vec<usize>,
    kind: ToolGroupKind,
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
/// least [`MIN_TOOL_GROUP_SIZE`] members. Reasoning items are transparent: the
/// Responses API may interleave them between calls from the same assistant
/// batch, but they do not represent a user-visible interaction boundary.
/// `ask_user` and `request_permission` stay standalone because they are real
/// interaction boundaries rather than batch work.
pub(crate) fn discover_tool_groups(items: &[MessageItem]) -> Vec<ToolGroup> {
    let mut groups = Vec::new();
    let mut run = Vec::new();

    let flush = |run: &mut Vec<usize>, groups: &mut Vec<ToolGroup>| {
        if run.len() >= MIN_TOOL_GROUP_SIZE {
            let calls: Vec<&FunctionToolCall> = run
                .iter()
                .filter_map(|&index| function_call(&items[index]))
                .collect();
            groups.push(ToolGroup {
                key: calls[0].call_id.clone(),
                member_indices: std::mem::take(run),
                kind: classify_group(&calls),
            });
        } else {
            run.clear();
        }
    };

    for (index, item) in items.iter().enumerate() {
        match function_call(item) {
            Some(call) if !is_interactive(call) => run.push(index),
            None if matches!(item, MessageItem::Output(OutputItem::Reasoning(_))) => {}
            _ => flush(&mut run, &mut groups),
        }
    }
    flush(&mut run, &mut groups);
    groups
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
    pub(crate) fn title(&self, completed: usize, failed: usize) -> String {
        let total = self.member_indices.len();
        let mut title = if completed < total {
            format!("{}… · {completed}/{total}", self.kind.active_title())
        } else {
            format!("{} · {total} calls", self.kind.completed_title())
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

/// Render a group header and, when open, its individually foldable calls.
pub(crate) fn build_tool_group_paragraph(
    group: &ToolGroup,
    members: &[ToolGroupMember<'_>],
    width: u16,
    expanded: bool,
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
    let mut headers = Vec::new();
    let wrap = members.iter().any(|member| member.expanded);
    let inner_width = width.saturating_sub(2).max(1);
    let mut rendered_rows = text_height(&Text::from(lines.clone()), inner_width, wrap);

    if expanded {
        for member in members {
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

#[cfg(test)]
mod tests {
    use super::*;
    use async_openai::types::responses::ReasoningItem;

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
        assert_eq!(groups[0].title(3, 0), "Explored · 3 calls");
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
        assert_eq!(groups[0].title(3, 0), "Explored · 3 calls");
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
        assert_eq!(group.title(3, 1), "Implemented · 3 calls · 1 failed");
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
            "Used browser tools · 3 calls"
        );
        assert_eq!(
            discover_tool_groups(&mixed_servers)[0].title(0, 0),
            "Using tools… · 0/3"
        );
    }
}
