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
            let msg_chunks = wrap_text(&d.message, msg_max);
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
                Span::styled(server.clone(), Style::default().fg(Color::White)),
                Span::styled(
                    format!(" ({tool_n} tools)"),
                    Style::default().fg(Color::DarkGray),
                ),
            ]);
            lines.push(line);
            targets.push(ClickTarget::None);
            count += 1;

            // Live progress reported by this server for in-flight tool calls
            // (notifications/progress).
            let progress = mgr.server_progress_all(&server).unwrap_or_default();
            for info in progress.values() {
                let pct = info
                    .total
                    .filter(|t| *t > 0.0)
                    .map(|t| format!("{:.0}% ", (info.progress / t * 100.0).min(100.0)))
                    .unwrap_or_default();
                let msg = info.message.clone().unwrap_or_else(|| "working…".to_string());
                let text = truncate_msg(&format!("⟳ {pct}{msg}"), width.saturating_sub(6) as usize);
                lines.push(Line::from(vec![
                    Span::raw("    "),
                    Span::styled(text, Style::default().fg(Color::Yellow)),
                ]));
                targets.push(ClickTarget::None);
            }
        }

        for err in &mgr.startup_errors {
            if count >= VISIBLE_PER_SECTION {
                break;
            }
            let msg = truncate_msg(err, 50);
            let line = Line::from(vec![
                Span::raw("  "),
                Span::styled("✗", Style::default().fg(Color::Red)),
                Span::raw(" "),
                Span::styled(msg, Style::default().fg(Color::Yellow)),
            ]);
            lines.push(line);
            targets.push(ClickTarget::None);
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

            let elapsed = format_elapsed(task.elapsed);
            let title_style = Style::default().fg(Color::White);
            let cont_style = Style::default().fg(Color::DarkGray);

            let prefix_spans: Vec<Span<'static>> = vec![
                Span::raw("  "),
                Span::styled(
                    icon.to_string(),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" #{} ", task.id),
                    Style::default().fg(Color::DarkGray),
                ),
            ];
            let prefix_width = spans_width(&prefix_spans);

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
                targets.push(ClickTarget::None);
            }
        }
    }
}

/// Compact human-readable duration: `42s`, `3m12s`, `1h04m`.
fn format_elapsed(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
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
    let chunks = wrap_text(message, msg_max);

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

/// Split `text` into chunks ≤ `max_chars` display columns (CJK chars count
/// as 2), breaking at spaces when possible.
fn wrap_text(text: &str, max_chars: usize) -> Vec<String> {
    if max_chars == 0 {
        return vec![text.to_string()];
    }
    let mut chunks = Vec::new();
    let mut remaining = text;
    while !remaining.is_empty() {
        let fit = byte_index_at_width(remaining, max_chars);
        if fit == remaining.len() {
            chunks.push(remaining.to_string());
            break;
        }
        let mut break_at = fit;
        if let Some(pos) = remaining[..fit].rfind(' ') {
            break_at = pos;
        }
        if break_at == 0 {
            // No usable break point and the first char alone exceeds the
            // budget (e.g. a wide char with max_chars == 1): take it anyway.
            break_at = remaining
                .char_indices()
                .nth(1)
                .map(|(i, _)| i)
                .unwrap_or(remaining.len());
        }
        chunks.push(remaining[..break_at].to_string());
        remaining = remaining[break_at..].trim_start();
    }
    chunks
}

/// Largest byte index in `s` such that `s[..index]` is ≤ `max` display
/// columns wide. Always a char boundary.
fn byte_index_at_width(s: &str, max: usize) -> usize {
    let mut width = 0usize;
    for (i, c) in s.char_indices() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(1);
        if width + w > max {
            return i;
        }
        width += w;
    }
    s.len()
}

/// Total display width of a slice of spans.
fn spans_width(spans: &[Span<'_>]) -> u16 {
    spans.iter().map(|s| s.width() as u16).sum()
}

/// Truncate a string to `max` display columns, appending "…" if cut.
fn truncate_msg(s: &str, max: usize) -> String {
    let end = byte_index_at_width(s, max);
    if end == s.len() {
        s.to_string()
    } else {
        format!("{}…", &s[..end])
    }
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
    fn elapsed_formatting() {
        assert_eq!(format_elapsed(Duration::from_secs(42)), "42s");
        assert_eq!(format_elapsed(Duration::from_secs(192)), "3m12s");
        assert_eq!(format_elapsed(Duration::from_secs(3840)), "1h04m");
    }
}
