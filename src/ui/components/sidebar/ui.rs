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

use super::{ClickTarget, Sidebar, SidebarSection};
use crate::agents::{AgentSnapshot, AgentStatus};
use crate::diagnostics::{Diagnostic, Severity};
use crate::mcp::{McpConnectionState, McpServerStatus};
use crate::providers::{ProviderModelState, ProviderModelStatus};
use crate::tasks::{SidebarTaskSnapshot, TaskStatus};
use crate::todos::{TodoList, TodoStatus};
use crate::ui::text::{format_duration_secs, truncate_to_width, wrap_to_width};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

/// Max visible content lines per expanded MCP section before truncation.
const VISIBLE_PER_SECTION: usize = 6;

/// Continuation-line indent in spaces.
const CONT_INDENT: &str = "    ";

#[derive(Clone, Copy)]
struct SidebarData<'a> {
    diagnostics: &'a [Diagnostic],
    lsp_configured: bool,
    mcp_servers: &'a [McpServerStatus],
    provider_models: &'a [ProviderModelStatus],
    active_provider: &'a str,
    todo_list: &'a TodoList,
    tasks: &'a [SidebarTaskSnapshot],
    agents: &'a [AgentSnapshot],
}

impl Sidebar {
    /// Render the sidebar into `area`. Populates `self.click_map` so the
    /// caller can resolve mouse clicks back to section titles or items.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        diagnostics: &[Diagnostic],
        lsp_configured: bool,
        mcp_servers: &[McpServerStatus],
        provider_models: &[ProviderModelStatus],
        active_provider: &str,
        todo_list: &TodoList,
        tasks: &[SidebarTaskSnapshot],
        agents: &[AgentSnapshot],
    ) {
        let block = Block::default()
            .borders(Borders::LEFT)
            .border_style(Style::default().fg(Color::DarkGray));
        let inner = block.inner(area);
        block.render(area, buf);

        if inner.width == 0 || inner.height == 0 {
            self.click_map.clear();
            return;
        }

        // Build all lines + click targets before scrolling.
        let data = SidebarData {
            diagnostics,
            lsp_configured,
            mcp_servers,
            provider_models,
            active_provider,
            todo_list,
            tasks,
            agents,
        };
        let (all_lines, click_targets) = self.build_lines(inner.width, data);

        // Clamp scroll.
        let visible_height = inner.height as usize;
        let total_lines = all_lines.len();
        let max_scroll = total_lines
            .saturating_sub(visible_height)
            .min(usize::from(u16::MAX)) as u16;
        self.clamp_scroll(total_lines, visible_height);
        let offset = self.scroll_offset;

        // Build the click map for visible lines (skipping scroll offset).
        self.click_map.clear();
        for target in click_targets
            .iter()
            .skip(offset as usize)
            .take(visible_height)
        {
            self.click_map.push(target.clone());
        }

        // Render visible slice.
        for (i, line) in all_lines
            .iter()
            .skip(offset as usize)
            .take(visible_height)
            .enumerate()
        {
            let y = inner.y + i as u16;
            if y < inner.y + inner.height {
                Paragraph::new(line.clone()).render(
                    Rect {
                        x: inner.x,
                        y,
                        width: inner.width,
                        height: 1,
                    },
                    buf,
                );
            }
        }

        // Scroll indicator at bottom-right if content overflows.
        if total_lines > visible_height {
            let pct = offset as f64 / max_scroll.max(1) as f64 * 100.0;
            let indicator = format!("{}/{total_lines} ({pct:.0}%)", offset + 1);
            let indicator_len = indicator.len() as u16;
            let x = inner
                .x
                .saturating_add(inner.width.saturating_sub(indicator_len));
            let indicator_line = Line::from(Span::styled(
                indicator,
                Style::default().fg(Color::DarkGray),
            ));
            Paragraph::new(indicator_line).render(
                Rect {
                    x,
                    y: inner.y + inner.height.saturating_sub(1),
                    width: indicator_len,
                    height: 1,
                },
                buf,
            );
        }
    }

    /// Build a flat list of all renderable lines + click targets.
    fn build_lines(
        &self,
        width: u16,
        data: SidebarData<'_>,
    ) -> (Vec<Line<'static>>, Vec<ClickTarget>) {
        let mut lines: Vec<Line> = Vec::new();
        let mut targets: Vec<ClickTarget> = Vec::new();

        let mut rendered_any = false;
        for section in self.sections.iter() {
            // Skip sections with nothing to show (e.g. no todos, no tasks, no
            // MCP servers, no diagnostics configured) so the sidebar only lists
            // what's actually present.
            if !self.section_has_content(section.key, data) {
                continue;
            }

            // Separator before every visible section except the first.
            if rendered_any {
                lines.push(Line::from(Span::styled(
                    "─".repeat(width as usize),
                    Style::default().fg(Color::DarkGray),
                )));
                targets.push(ClickTarget::None);
            }
            rendered_any = true;

            // Title line.
            let title = self.section_title(section, data);
            let title_line = self.make_title_line(&title, section.key, section.collapsed);
            lines.push(title_line);
            targets.push(ClickTarget::Section(section.key));

            if !section.collapsed {
                match section.key {
                    SidebarSection::Diagnostics => {
                        self.render_diagnostics(
                            &mut lines,
                            &mut targets,
                            width,
                            data.diagnostics,
                            data.lsp_configured,
                        );
                    }
                    SidebarSection::Mcp => {
                        self.render_mcp(&mut lines, &mut targets, width, data.mcp_servers);
                    }
                    SidebarSection::Providers => {
                        self.render_providers(
                            &mut lines,
                            &mut targets,
                            data.provider_models,
                            data.active_provider,
                        );
                    }
                    SidebarSection::Todos => {
                        self.render_todos(&mut lines, &mut targets, width, data.todo_list);
                    }
                    SidebarSection::Tasks => {
                        self.render_tasks(&mut lines, &mut targets, width, data.tasks);
                    }
                    SidebarSection::Agents => {
                        self.render_agents(&mut lines, &mut targets, width, data.agents);
                    }
                }
            }
        }

        if !rendered_any {
            lines.push(Line::from(Span::styled(
                "  nothing to show yet",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            )));
            targets.push(ClickTarget::None);
        }

        (lines, targets)
    }

    /// Whether a section has anything worth showing. Empty sections are hidden
    /// entirely (title included) to keep the sidebar uncluttered.
    fn section_has_content(&self, key: SidebarSection, data: SidebarData<'_>) -> bool {
        match key {
            // Show when diagnostics exist or a live LSP is configured.
            SidebarSection::Diagnostics => data.lsp_configured || !data.diagnostics.is_empty(),
            SidebarSection::Mcp => !data.mcp_servers.is_empty(),
            SidebarSection::Providers => !data.provider_models.is_empty(),
            SidebarSection::Todos => !data.todo_list.todos.is_empty(),
            SidebarSection::Tasks => !data.tasks.is_empty(),
            SidebarSection::Agents => !data.agents.is_empty(),
        }
    }

    // -- section title helpers --

    fn section_title(&self, section: &super::SectionState, data: SidebarData<'_>) -> String {
        match section.key {
            SidebarSection::Agents => {
                let running = data
                    .agents
                    .iter()
                    .filter(|agent| agent.status == AgentStatus::Running)
                    .count();
                format!("Agents ({running} running, {} total)", data.agents.len())
            }
            SidebarSection::Tasks => {
                if data.tasks.is_empty() {
                    "Tasks".to_string()
                } else {
                    let running = data
                        .tasks
                        .iter()
                        .filter(|t| t.status == TaskStatus::Running)
                        .count();
                    format!("Tasks ({running} running, {} total)", data.tasks.len())
                }
            }
            SidebarSection::Diagnostics => {
                if !data.lsp_configured && data.diagnostics.is_empty() {
                    "Diagnostics".to_string()
                } else {
                    // Compact counts (only non-zero) so the title fits the
                    // narrow sidebar: e.g. "Diagnostics (2E 5W 12L)".
                    let count = |s| data.diagnostics.iter().filter(|d| d.severity == s).count();
                    let mut parts = Vec::new();
                    for (n, letter) in [
                        (count(Severity::Error), 'E'),
                        (count(Severity::Warning), 'W'),
                        (count(Severity::Lint), 'L'),
                    ] {
                        if n > 0 {
                            parts.push(format!("{n}{letter}"));
                        }
                    }
                    if parts.is_empty() {
                        "Diagnostics".to_string()
                    } else {
                        format!("Diagnostics ({})", parts.join(" "))
                    }
                }
            }
            SidebarSection::Mcp => {
                let mut parts = Vec::new();
                let ready = data
                    .mcp_servers
                    .iter()
                    .filter(|server| matches!(server.state, McpConnectionState::Connected { .. }))
                    .count();
                let connecting = data
                    .mcp_servers
                    .iter()
                    .filter(|server| matches!(server.state, McpConnectionState::Connecting))
                    .count();
                let failed = data
                    .mcp_servers
                    .iter()
                    .filter(|server| matches!(server.state, McpConnectionState::Failed { .. }))
                    .count();
                for (amount, label) in [
                    (ready, "ready"),
                    (connecting, "connecting"),
                    (failed, "failed"),
                ] {
                    if amount > 0 {
                        parts.push(format!("{amount} {label}"));
                    }
                }
                format!("MCP ({})", parts.join(", "))
            }
            SidebarSection::Providers => {
                let ready = data
                    .provider_models
                    .iter()
                    .filter(|provider| matches!(provider.state, ProviderModelState::Ready { .. }))
                    .count();
                let refreshing = data
                    .provider_models
                    .iter()
                    .filter(|provider| provider.state == ProviderModelState::Refreshing)
                    .count();
                let failed = data
                    .provider_models
                    .iter()
                    .filter(|provider| provider.state == ProviderModelState::Failed)
                    .count();
                let mut parts = Vec::new();
                for (amount, label) in [
                    (ready, "ready"),
                    (refreshing, "refreshing"),
                    (failed, "failed"),
                ] {
                    if amount > 0 {
                        parts.push(format!("{amount} {label}"));
                    }
                }
                format!("Providers ({})", parts.join(", "))
            }
            SidebarSection::Todos => {
                let pending = data
                    .todo_list
                    .todos
                    .iter()
                    .filter(|t| t.status == TodoStatus::Pending)
                    .count();
                let done = data
                    .todo_list
                    .todos
                    .iter()
                    .filter(|t| t.status == TodoStatus::Completed)
                    .count();
                if data.todo_list.todos.is_empty() {
                    "Todos".to_string()
                } else {
                    format!("Todos ({pending} pending, {done} done)")
                }
            }
        }
    }

    fn make_title_line(
        &self,
        title: &str,
        section: SidebarSection,
        collapsed: bool,
    ) -> Line<'static> {
        let arrow = if collapsed { "▶" } else { "▼" };
        let text = format!(" {arrow} {title}");

        let color = match section {
            SidebarSection::Diagnostics => Color::Red,
            SidebarSection::Mcp => Color::Magenta,
            SidebarSection::Providers => Color::Green,
            SidebarSection::Todos => Color::Yellow,
            SidebarSection::Tasks => Color::Cyan,
            SidebarSection::Agents => Color::Blue,
        };

        let style = Style::default().fg(color).add_modifier(Modifier::BOLD);

        Line::from(Span::styled(text, style))
    }

    // -- per-section content renderers --

    fn render_providers(
        &self,
        lines: &mut Vec<Line<'static>>,
        targets: &mut Vec<ClickTarget>,
        providers: &[ProviderModelStatus],
        active_provider: &str,
    ) {
        for provider in providers {
            let (dot_color, label, label_color) = match provider.state {
                ProviderModelState::Refreshing => {
                    (Color::Yellow, "refreshing…".to_string(), Color::Yellow)
                }
                ProviderModelState::Ready { model_count } => (
                    Color::Green,
                    format!("{model_count} models"),
                    Color::DarkGray,
                ),
                ProviderModelState::Failed => {
                    (Color::Red, "refresh failed".to_string(), Color::Red)
                }
            };
            let active_marker = if provider.name == active_provider {
                " [current]"
            } else {
                ""
            };
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled("●", Style::default().fg(dot_color)),
                Span::raw(" "),
                Span::styled(provider.name.clone(), Style::default().fg(Color::White)),
                Span::styled(active_marker, Style::default().fg(Color::Yellow)),
                Span::styled(format!(" ({label})"), Style::default().fg(label_color)),
            ]));
            targets.push(ClickTarget::None);
        }
    }

    fn render_diagnostics(
        &self,
        lines: &mut Vec<Line<'static>>,
        targets: &mut Vec<ClickTarget>,
        width: u16,
        diagnostics: &[Diagnostic],
        lsp_configured: bool,
    ) {
        if !lsp_configured && diagnostics.is_empty() {
            lines.push(Line::from(Span::styled(
                "  No diagnostics configured",
                Style::default().fg(Color::DarkGray),
            )));
            targets.push(ClickTarget::None);
            return;
        }
        if diagnostics.is_empty() {
            lines.push(Line::from(Span::styled(
                "  No issues detected",
                Style::default().fg(Color::Green),
            )));
            targets.push(ClickTarget::None);
            return;
        }

        let mut sorted: Vec<&Diagnostic> = diagnostics.iter().collect();
        sorted.sort_by_key(|d| d.severity);
        let msg_max = (width.saturating_sub(4)) as usize; // 4-char indent

        for (i, d) in sorted.iter().enumerate() {
            let (severity_icon, severity_color) = match d.severity {
                Severity::Error => ("E", Color::Red),
                Severity::Warning => ("W", Color::Yellow),
                Severity::Lint => ("L", Color::DarkGray),
                Severity::Info => ("I", Color::Blue),
            };

            // Show just basename:line so it fits.
            let file = std::path::Path::new(&d.file)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| d.file.clone());
            let loc = if d.line > 0 {
                format!("{file}:{}", d.line)
            } else {
                file
            };

            // Line 1: "  E basename:line" (no message on this line)
            let header = Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    severity_icon,
                    Style::default()
                        .fg(severity_color)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled(loc, Style::default().fg(Color::Gray)),
            ]);
            lines.push(header);
            targets.push(ClickTarget::Diagnostic(i));

            // Line 2+: "    message text" wrapped
            let msg_chunks = wrap_to_width(&d.message, msg_max);
            for chunk in &msg_chunks {
                lines.push(Line::from(vec![
                    Span::raw("    "),
                    Span::styled(chunk.clone(), Style::default().fg(Color::White)),
                ]));
                targets.push(ClickTarget::None);
            }
        }
    }

    fn render_mcp(
        &self,
        lines: &mut Vec<Line<'static>>,
        targets: &mut Vec<ClickTarget>,
        width: u16,
        mcp_servers: &[McpServerStatus],
    ) {
        for server in mcp_servers.iter().take(VISIBLE_PER_SECTION) {
            let (dot_color, label, label_color) = match &server.state {
                McpConnectionState::Connecting => {
                    (Color::Yellow, " (connecting...)".to_string(), Color::Yellow)
                }
                McpConnectionState::Connected { tool_count } => (
                    Color::Green,
                    format!(" ({tool_count} tools)"),
                    Color::DarkGray,
                ),
                McpConnectionState::Failed { .. } => {
                    (Color::Red, " (failed)".to_string(), Color::Red)
                }
            };
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled("●", Style::default().fg(dot_color)),
                Span::raw(" "),
                Span::styled(server.name.clone(), Style::default().fg(Color::White)),
                Span::styled(label, Style::default().fg(label_color)),
            ]));
            targets.push(ClickTarget::None);

            let McpConnectionState::Failed { error } = &server.state else {
                continue;
            };
            let msg_max = (width.saturating_sub(4)) as usize;
            for chunk in wrap_to_width(error, msg_max) {
                lines.push(Line::from(vec![
                    Span::raw("    "),
                    Span::styled(chunk, Style::default().fg(Color::Yellow)),
                ]));
                targets.push(ClickTarget::None);
            }
        }

        if mcp_servers.len() > VISIBLE_PER_SECTION {
            let remaining = mcp_servers.len() - VISIBLE_PER_SECTION;
            lines.push(Line::from(Span::styled(
                format!("    … {remaining} more"),
                Style::default().fg(Color::DarkGray),
            )));
            targets.push(ClickTarget::None);
        }
    }

    fn render_todos(
        &self,
        lines: &mut Vec<Line<'static>>,
        targets: &mut Vec<ClickTarget>,
        width: u16,
        todo_list: &TodoList,
    ) {
        if todo_list.todos.is_empty() {
            lines.push(Line::from(Span::styled(
                "  No todos",
                Style::default().fg(Color::DarkGray),
            )));
            targets.push(ClickTarget::None);
            return;
        }

        let mut sorted: Vec<&crate::todos::Todo> = todo_list.todos.iter().collect();
        sorted.sort_by_key(|t| todo_status_order(&t.status));

        for (i, todo) in sorted.iter().enumerate() {
            let (icon, color) = match todo.status {
                TodoStatus::Pending => (TodoStatus::Pending.icon(), Color::DarkGray),
                TodoStatus::InProgress => (TodoStatus::InProgress.icon(), Color::Yellow),
                TodoStatus::Completed => (TodoStatus::Completed.icon(), Color::Green),
                TodoStatus::Cancelled => (TodoStatus::Cancelled.icon(), Color::Red),
            };

            let title_style = Style::default().fg(Color::White);
            let cont_style = Style::default().fg(Color::DarkGray);

            let prefix_spans: Vec<Span<'static>> = vec![
                Span::raw("  "),
                Span::styled(
                    format!("[{icon}]"),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
            ];
            let prefix_width = spans_width(&prefix_spans);

            let line_count_before = lines.len();
            wrapped_item(
                lines,
                prefix_spans,
                &todo.title,
                width,
                prefix_width,
                CONT_INDENT,
                title_style,
                cont_style,
            );
            targets.push(ClickTarget::TodoItem(i));
            for _ in 1..(lines.len() - line_count_before) {
                targets.push(ClickTarget::None);
            }
        }
    }

    fn render_tasks(
        &self,
        lines: &mut Vec<Line<'static>>,
        targets: &mut Vec<ClickTarget>,
        width: u16,
        tasks: &[SidebarTaskSnapshot],
    ) {
        if tasks.is_empty() {
            lines.push(Line::from(Span::styled(
                "  No background tasks",
                Style::default().fg(Color::DarkGray),
            )));
            targets.push(ClickTarget::None);
            return;
        }

        for task in tasks {
            let (icon, color) = match task.status {
                TaskStatus::Running => ("▶", Color::Yellow),
                TaskStatus::Completed => ("✓", Color::Green),
                TaskStatus::Failed => ("✗", Color::Red),
                TaskStatus::Killed => ("⊘", Color::DarkGray),
            };

            let expanded = self.task_expanded(task.id);
            let arrow = if expanded { "▾" } else { "▸" };
            let elapsed = format_duration_secs(task.elapsed);
            let title_style = Style::default().fg(Color::White);
            let cont_style = Style::default().fg(Color::DarkGray);

            let prefix_spans: Vec<Span<'static>> = vec![
                Span::raw("  "),
                Span::styled(
                    icon.to_string(),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" {arrow} #{} ", task.id),
                    Style::default().fg(Color::DarkGray),
                ),
            ];
            let prefix_width = spans_width(&prefix_spans);

            // Header line(s): clickable to toggle the output view.
            let line_count_before = lines.len();
            wrapped_item(
                lines,
                prefix_spans,
                &format!("{} ({elapsed})", task.name),
                width,
                prefix_width,
                CONT_INDENT,
                title_style,
                cont_style,
            );
            for _ in 0..(lines.len() - line_count_before) {
                targets.push(ClickTarget::Task(task.id));
            }

            if expanded {
                self.render_task_output(lines, targets, width, task);
            }
        }
    }

    fn render_agents(
        &self,
        lines: &mut Vec<Line<'static>>,
        targets: &mut Vec<ClickTarget>,
        width: u16,
        agents: &[AgentSnapshot],
    ) {
        for agent in agents {
            let (icon, color) = match agent.status {
                AgentStatus::Running => ("▶", Color::Yellow),
                AgentStatus::Completed => ("✓", Color::Green),
                AgentStatus::Failed => ("✗", Color::Red),
                AgentStatus::Cancelled => ("⊘", Color::DarkGray),
            };
            let prefix_spans = vec![
                Span::raw("  "),
                Span::styled(
                    icon.to_string(),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" #{} ", agent.id),
                    Style::default().fg(Color::DarkGray),
                ),
            ];
            let prefix_width = spans_width(&prefix_spans);
            let before = lines.len();
            wrapped_item(
                lines,
                prefix_spans,
                &format!("{} ({})", agent.name, format_duration_secs(agent.elapsed)),
                width,
                prefix_width,
                CONT_INDENT,
                Style::default().fg(Color::White),
                Style::default().fg(Color::DarkGray),
            );
            for _ in 0..(lines.len() - before) {
                targets.push(ClickTarget::Agent(agent.id));
            }
        }
    }

    /// The expanded output view under a task header: exit code (when
    /// finished) and the last few output lines.
    fn render_task_output(
        &self,
        lines: &mut Vec<Line<'static>>,
        targets: &mut Vec<ClickTarget>,
        width: u16,
        task: &SidebarTaskSnapshot,
    ) {
        let dim = Style::default().fg(Color::DarkGray);
        let text_style = Style::default().fg(Color::Gray);
        let budget = (width.saturating_sub(6) as usize).max(8);

        if let Some(code) = task.exit_code {
            lines.push(Line::from(Span::styled(format!("     exit {code}"), dim)));
            targets.push(ClickTarget::None);
        }

        let Some(output) = &task.output else {
            return;
        };
        if output.lines.is_empty() {
            lines.push(Line::from(Span::styled("     (no output)", dim)));
            targets.push(ClickTarget::None);
            return;
        }
        if output.omitted_lines > 0 {
            lines.push(Line::from(Span::styled(
                format!("     … {} earlier lines", output.omitted_lines),
                dim,
            )));
            targets.push(ClickTarget::None);
        }
        for out_line in &output.lines {
            lines.push(Line::from(vec![
                Span::raw("     "),
                Span::styled(truncate_to_width(out_line, budget), text_style),
            ]));
            targets.push(ClickTarget::None);
        }
    }
}

// -- helpers --

/// Wraps `message` text to fit within `max_width`. The first line gets
/// `prefix_spans` prepended; continuation lines use `indent` and
/// `cont_style`.
#[allow(clippy::too_many_arguments)]
fn wrapped_item(
    lines: &mut Vec<Line<'static>>,
    prefix_spans: Vec<Span<'static>>,
    message: &str,
    max_width: u16,
    prefix_width: u16,
    indent: &str,
    first_style: Style,
    cont_style: Style,
) {
    if max_width <= prefix_width {
        lines.push(Line::from(prefix_spans));
        return;
    }

    let msg_max = (max_width - prefix_width) as usize;
    let chunks = wrap_to_width(message, msg_max);

    for (i, chunk) in chunks.iter().enumerate() {
        if i == 0 {
            let mut spans = prefix_spans.clone();
            spans.push(Span::styled(chunk.clone(), first_style));
            lines.push(Line::from(spans));
        } else {
            lines.push(Line::from(vec![
                Span::raw(indent.to_string()),
                Span::styled(chunk.clone(), cont_style),
            ]));
        }
    }
}

/// Total display width of a slice of spans.
fn spans_width(spans: &[Span<'_>]) -> u16 {
    spans.iter().map(|s| s.width() as u16).sum()
}

/// Ordering key for TodoStatus (Pending < InProgress < Completed < Cancelled).
pub(crate) fn todo_status_order(s: &TodoStatus) -> u8 {
    match s {
        TodoStatus::Pending => 0,
        TodoStatus::InProgress => 1,
        TodoStatus::Completed => 2,
        TodoStatus::Cancelled => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::{SidebarTaskOutput, SidebarTaskSnapshot};
    use std::time::Duration;

    fn buffer_text(buf: &Buffer) -> String {
        let mut text = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                text.push_str(buf[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }

    fn render_mcp_statuses(statuses: &[McpServerStatus]) -> String {
        let mut sidebar = Sidebar::new();
        let area = Rect::new(0, 0, 40, 20);
        let mut buf = Buffer::empty(area);
        sidebar.render(
            area,
            &mut buf,
            &[],
            false,
            statuses,
            &[],
            "",
            &crate::todos::TodoList::default(),
            &[],
            &[],
        );
        buffer_text(&buf)
    }

    fn render_provider_statuses(statuses: &[ProviderModelStatus], active: &str) -> String {
        let mut sidebar = Sidebar::new();
        let area = Rect::new(0, 0, 48, 20);
        let mut buf = Buffer::empty(area);
        sidebar.render(
            area,
            &mut buf,
            &[],
            false,
            &[],
            statuses,
            active,
            &crate::todos::TodoList::default(),
            &[],
            &[],
        );
        buffer_text(&buf)
    }

    #[test]
    fn mcp_connecting_server_is_visible() {
        let text = render_mcp_statuses(&[McpServerStatus::connecting("codegraph")]);

        assert!(text.contains("MCP (1 connecting)"), "got:\n{text}");
        assert!(text.contains("codegraph (connecting...)"), "got:\n{text}");
    }

    #[test]
    fn mcp_connected_server_with_no_tools_is_visible() {
        let text = render_mcp_statuses(&[McpServerStatus {
            name: "empty-server".to_string(),
            state: McpConnectionState::Connected { tool_count: 0 },
        }]);

        assert!(text.contains("MCP (1 ready)"), "got:\n{text}");
        assert!(text.contains("empty-server (0 tools)"), "got:\n{text}");
    }

    #[test]
    fn mcp_failed_server_shows_its_error() {
        let text = render_mcp_statuses(&[McpServerStatus {
            name: "broken".to_string(),
            state: McpConnectionState::Failed {
                error: "tools/list returned invalid JSON".to_string(),
            },
        }]);

        assert!(text.contains("MCP (1 failed)"), "got:\n{text}");
        assert!(text.contains("broken (failed)"), "got:\n{text}");
        assert!(
            text.contains("tools/list returned invalid JSON"),
            "got:\n{text}"
        );
    }

    #[test]
    fn provider_model_states_are_visible_in_the_sidebar() {
        let text = render_provider_statuses(
            &[
                ProviderModelStatus {
                    name: "openai".to_string(),
                    state: ProviderModelState::Ready { model_count: 12 },
                },
                ProviderModelStatus {
                    name: "deepseek".to_string(),
                    state: ProviderModelState::Refreshing,
                },
                ProviderModelStatus {
                    name: "offline".to_string(),
                    state: ProviderModelState::Failed,
                },
            ],
            "deepseek",
        );

        assert!(
            text.contains("Providers (1 ready, 1 refreshing, 1 failed)"),
            "got:\n{text}"
        );
        assert!(text.contains("openai (12 models)"), "got:\n{text}");
        assert!(
            text.contains("deepseek [current] (refreshing…)"),
            "got:\n{text}"
        );
        assert!(!text.contains("openai [current]"), "got:\n{text}");
        assert!(text.contains("offline (refresh failed)"), "got:\n{text}");
    }

    #[test]
    fn all_provider_model_states_are_visible_in_the_sidebar() {
        let statuses: Vec<_> = (1..=10)
            .map(|index| ProviderModelStatus {
                name: format!("provider-{index}"),
                state: ProviderModelState::Ready { model_count: index },
            })
            .collect();

        let text = render_provider_statuses(&statuses, "provider-10");

        assert!(
            text.contains("provider-10 [current] (10 models)"),
            "got:\n{text}"
        );
        assert!(!text.contains(" more"), "got:\n{text}");
    }

    #[test]
    fn tasks_section_renders_above_diagnostics() {
        let mut sidebar = Sidebar::new();
        let area = Rect::new(0, 0, 32, 40);
        let mut buf = Buffer::empty(area);
        let tasks = vec![SidebarTaskSnapshot {
            id: 7,
            name: "cargo watch build monitor".to_string(),
            status: crate::tasks::TaskStatus::Running,
            exit_code: None,
            elapsed: Duration::from_secs(75),
            output: None,
        }];

        // `lsp_configured: true` keeps the (otherwise empty) diagnostics
        // section visible so the ordering assertion below has both sections.
        sidebar.render(
            area,
            &mut buf,
            &[],
            true,
            &[],
            &[],
            "",
            &crate::todos::TodoList::default(),
            &tasks,
            &[],
        );

        let text = buffer_text(&buf);
        assert!(text.contains("Tasks (1 running, 1 total)"), "got:\n{text}");
        assert!(text.contains("#7"), "got:\n{text}");
        assert!(text.contains("1m15s"), "got:\n{text}");
        let tasks_pos = text.find("Tasks").expect("tasks section");
        let diag_pos = text.find("Diagnostics").expect("diagnostics section");
        assert!(tasks_pos < diag_pos, "Tasks must render above Diagnostics");
    }

    #[test]
    fn expanded_task_shows_output_tail_and_exit_code() {
        let mut sidebar = Sidebar::new();
        sidebar.toggle_task(3);
        let area = Rect::new(0, 0, 32, 40);
        let mut buf = Buffer::empty(area);
        let tasks = vec![SidebarTaskSnapshot {
            id: 3,
            name: "build".to_string(),
            status: crate::tasks::TaskStatus::Failed,
            exit_code: Some(101),
            elapsed: Duration::from_secs(9),
            output: Some(SidebarTaskOutput {
                lines: (4..=13).map(|i| format!("line {i}")).collect(),
                omitted_lines: 3,
            }),
        }];

        sidebar.render(
            area,
            &mut buf,
            &[],
            false,
            &[],
            &[],
            "",
            &crate::todos::TodoList::default(),
            &tasks,
            &[],
        );

        let text = buffer_text(&buf);
        assert!(text.contains("exit 101"), "got:\n{text}");
        // Only the last 10 lines show, with an earlier-lines marker.
        assert!(text.contains("3 earlier lines"), "got:\n{text}");
        assert!(text.contains("line 13"), "got:\n{text}");
        assert!(!text.contains("line 2 "), "got:\n{text}");

        // Collapsing hides the output again.
        sidebar.toggle_task(3);
        let mut buf2 = Buffer::empty(area);
        sidebar.render(
            area,
            &mut buf2,
            &[],
            false,
            &[],
            &[],
            "",
            &crate::todos::TodoList::default(),
            &tasks,
            &[],
        );
        assert!(!buffer_text(&buf2).contains("exit 101"));
    }

    #[test]
    fn agents_section_shows_live_status_and_label() {
        let mut sidebar = Sidebar::new();
        let area = Rect::new(0, 0, 40, 20);
        let mut buf = Buffer::empty(area);
        let agents = vec![crate::agents::AgentSnapshot {
            id: 2,
            name: "Review diagnostics".to_string(),
            prompt: "review diagnostics".to_string(),
            status: crate::agents::AgentStatus::Running,
            elapsed: Duration::from_secs(3),
            result: None,
        }];

        sidebar.render(
            area,
            &mut buf,
            &[],
            false,
            &[],
            &[],
            "",
            &crate::todos::TodoList::default(),
            &[],
            &agents,
        );

        let text = buffer_text(&buf);
        assert!(text.contains("Agents (1 running, 1 total)"), "got:\n{text}");
        assert!(text.contains("#2 Review diagnostics"), "got:\n{text}");
    }
}
