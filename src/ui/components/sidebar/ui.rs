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
use crate::diagnostics::{Diagnostic, Severity};
use crate::mcp::McpManager;
use crate::tasks::{TaskSnapshot, TaskStatus};
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

impl Sidebar {
    /// Render the sidebar into `area`. Populates `self.click_map` so the
    /// caller can resolve mouse clicks back to section titles or items.
    pub fn render(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        diagnostics: &[Diagnostic],
        lsp_configured: bool,
        mcp_manager: Option<&McpManager>,
        todo_list: &TodoList,
        tasks: &[TaskSnapshot],
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
        let (all_lines, click_targets) = self.build_lines(
            inner.width,
            diagnostics,
            lsp_configured,
            mcp_manager,
            todo_list,
            tasks,
        );

        // Clamp scroll.
        let visible_height = inner.height as usize;
        let total_lines = all_lines.len();
        let max_scroll = total_lines.saturating_sub(visible_height) as u16;
        let offset = self.scroll_offset.min(max_scroll);

        // Build the click map for visible lines (skipping scroll offset).
        self.click_map.clear();
        for target in click_targets.iter().skip(offset as usize).take(visible_height) {
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
        diagnostics: &[Diagnostic],
        lsp_configured: bool,
        mcp_manager: Option<&McpManager>,
        todo_list: &TodoList,
        tasks: &[TaskSnapshot],
    ) -> (Vec<Line<'static>>, Vec<ClickTarget>) {
        let mut lines: Vec<Line> = Vec::new();
        let mut targets: Vec<ClickTarget> = Vec::new();

        for (idx, section) in self.sections.iter().enumerate() {
            // Title line.
            let title = self.section_title(section, diagnostics, lsp_configured, mcp_manager, todo_list, tasks);
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
                            diagnostics,
                            lsp_configured,
                        );
                    }
                    SidebarSection::Mcp => {
                        self.render_mcp(&mut lines, &mut targets, width, mcp_manager);
                    }
                    SidebarSection::Todos => {
                        self.render_todos(&mut lines, &mut targets, width, todo_list);
                    }
                    SidebarSection::Tasks => {
                        self.render_tasks(&mut lines, &mut targets, width, tasks);
                    }
                }
            }

            // Separator between sections (not after the last one).
            if idx + 1 < self.sections.len() {
                lines.push(Line::from(Span::styled(
                    "─".repeat(width as usize),
                    Style::default().fg(Color::DarkGray),
                )));
                targets.push(ClickTarget::None);
            }
        }

        (lines, targets)
    }

    // -- section title helpers --

    fn section_title(
        &self,
        section: &super::SectionState,
        diagnostics: &[Diagnostic],
        lsp_configured: bool,
        mcp_manager: Option<&McpManager>,
        todo_list: &TodoList,
        tasks: &[TaskSnapshot],
    ) -> String {
        match section.key {
            SidebarSection::Tasks => {
                if tasks.is_empty() {
                    "Tasks".to_string()
                } else {
                    let running = tasks
                        .iter()
                        .filter(|t| t.status == TaskStatus::Running)
                        .count();
                    format!("Tasks ({running} running, {} total)", tasks.len())
                }
            }
            SidebarSection::Diagnostics => {
                if !lsp_configured && diagnostics.is_empty() {
                    "Diagnostics".to_string()
                } else {
                    let errs = diagnostics.iter().filter(|d| d.severity == Severity::Error).count();
                    let warns = diagnostics
                        .iter()
                        .filter(|d| d.severity == Severity::Warning)
                        .count();
                    format!("Diagnostics ({errs} err, {warns} warn)")
                }
            }
            SidebarSection::Mcp => {
                if let Some(mgr) = mcp_manager {
                    let servers = mgr.server_count();
                    let tools = mgr.all_tools().len();
                    format!("MCP ({servers} servers, {tools} tools)")
                } else {
                    "MCP".to_string()
                }
            }
            SidebarSection::Todos => {
                let pending = todo_list
                    .todos
                    .iter()
                    .filter(|t| t.status == TodoStatus::Pending)
                    .count();
                let done = todo_list
                    .todos
                    .iter()
                    .filter(|t| t.status == TodoStatus::Completed)
                    .count();
                if todo_list.todos.is_empty() {
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
            SidebarSection::Todos => Color::Yellow,
            SidebarSection::Tasks => Color::Cyan,
        };

        let style = Style::default()
            .fg(color)
            .add_modifier(Modifier::BOLD);

        Line::from(Span::styled(text, style))
    }

    // -- per-section content renderers --

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
        mcp_manager: Option<&McpManager>,
    ) {
        let Some(mgr) = mcp_manager else {
            lines.push(Line::from(Span::styled(
                "  No MCP servers configured",
                Style::default().fg(Color::DarkGray),
            )));
            targets.push(ClickTarget::None);
            return;
        };

        let tools = mgr.all_tools();
        let mut tool_counts: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for (key, _) in &tools {
            if let Some(rest) = key.strip_prefix("mcp__") {
                if let Some((server, _)) = rest.split_once("__") {
                    *tool_counts.entry(server.to_string()).or_default() += 1;
                }
            }
        }

        let entries: Vec<(String, usize)> = tool_counts.into_iter().collect();
        let entries_empty = entries.is_empty();

        let mut count = 0usize;
        for (server, tool_n) in entries.into_iter() {
            if count >= VISIBLE_PER_SECTION {
                break;
            }
            let line = Line::from(vec![
                Span::raw("  "),
                Span::styled("●", Style::default().fg(Color::Green)),
                Span::raw(" "),
                Span::styled(server, Style::default().fg(Color::White)),
                Span::styled(
                    format!(" ({tool_n} tools)"),
                    Style::default().fg(Color::DarkGray),
                ),
            ]);
            lines.push(line);
            targets.push(ClickTarget::None);
            count += 1;
        }

        // Wrap each startup error across as many lines as it needs so the full
        // message is readable, rather than clipping it to a single line.
        let msg_max = (width.saturating_sub(4)) as usize; // "  ✗ " / "    " indent
        for err in &mgr.startup_errors {
            if count >= VISIBLE_PER_SECTION {
                break;
            }
            let chunks = wrap_to_width(err, msg_max);
            for (i, chunk) in chunks.iter().enumerate() {
                let line = if i == 0 {
                    Line::from(vec![
                        Span::raw("  "),
                        Span::styled("✗", Style::default().fg(Color::Red)),
                        Span::raw(" "),
                        Span::styled(chunk.clone(), Style::default().fg(Color::Yellow)),
                    ])
                } else {
                    Line::from(vec![
                        Span::raw("    "),
                        Span::styled(chunk.clone(), Style::default().fg(Color::Yellow)),
                    ])
                };
                lines.push(line);
                targets.push(ClickTarget::None);
            }
            count += 1;
        }

        if entries_empty && mgr.startup_errors.is_empty() {
            lines.push(Line::from(Span::styled(
                "  No tools discovered",
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
                Span::styled(format!("[{icon}]"), Style::default().fg(color).add_modifier(Modifier::BOLD)),
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
        tasks: &[TaskSnapshot],
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

    /// The expanded output view under a task header: exit code (when
    /// finished) and the last few output lines.
    fn render_task_output(
        &self,
        lines: &mut Vec<Line<'static>>,
        targets: &mut Vec<ClickTarget>,
        width: u16,
        task: &TaskSnapshot,
    ) {
        const OUTPUT_TAIL_LINES: usize = 10;
        let dim = Style::default().fg(Color::DarkGray);
        let text_style = Style::default().fg(Color::Gray);
        let budget = (width.saturating_sub(6) as usize).max(8);

        if let Some(code) = task.exit_code {
            lines.push(Line::from(Span::styled(
                format!("     exit {code}"),
                dim,
            )));
            targets.push(ClickTarget::None);
        }

        let tail: Vec<&str> = task
            .output
            .lines()
            .rev()
            .take(OUTPUT_TAIL_LINES)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if tail.is_empty() {
            lines.push(Line::from(Span::styled("     (no output)", dim)));
            targets.push(ClickTarget::None);
            return;
        }
        let total = task.output.lines().count();
        if total > tail.len() {
            lines.push(Line::from(Span::styled(
                format!("     … {} earlier lines", total - tail.len()),
                dim,
            )));
            targets.push(ClickTarget::None);
        }
        for out_line in tail {
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
    use crate::tasks::TaskSnapshot;
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

    #[test]
    fn tasks_section_renders_above_diagnostics() {
        let mut sidebar = Sidebar::new();
        let area = Rect::new(0, 0, 32, 40);
        let mut buf = Buffer::empty(area);
        let tasks = vec![TaskSnapshot {
            id: 7,
            name: "cargo watch 构建监听".to_string(),
            command: "cargo watch".to_string(),
            status: crate::tasks::TaskStatus::Running,
            exit_code: None,
            elapsed: Duration::from_secs(75),
            output: String::new(),
        }];

        sidebar.render(
            area,
            &mut buf,
            &[],
            false,
            None,
            &crate::todos::TodoList::default(),
            &tasks,
        );

        let text = buffer_text(&buf);
        assert!(text.contains("Tasks (1 running, 1 total)"), "got:\n{text}");
        assert!(text.contains("#7"), "got:\n{text}");
        assert!(text.contains("1m15s"), "got:\n{text}");
        let tasks_pos = text.find("Tasks").expect("tasks section");
        let diag_pos = text.find("Diagnostics").expect("diagnostics section");
        assert!(
            tasks_pos < diag_pos,
            "Tasks must render above Diagnostics"
        );
    }

    #[test]
    fn expanded_task_shows_output_tail_and_exit_code() {
        let mut sidebar = Sidebar::new();
        sidebar.toggle_task(3);
        let area = Rect::new(0, 0, 32, 40);
        let mut buf = Buffer::empty(area);
        let output: String = (1..=13)
            .map(|i| format!("line {i}\n"))
            .collect();
        let tasks = vec![TaskSnapshot {
            id: 3,
            name: "build".to_string(),
            command: "cargo build".to_string(),
            status: crate::tasks::TaskStatus::Failed,
            exit_code: Some(101),
            elapsed: Duration::from_secs(9),
            output,
        }];

        sidebar.render(
            area,
            &mut buf,
            &[],
            false,
            None,
            &crate::todos::TodoList::default(),
            &tasks,
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
            None,
            &crate::todos::TodoList::default(),
            &tasks,
        );
        assert!(!buffer_text(&buf2).contains("exit 101"));
    }
}
