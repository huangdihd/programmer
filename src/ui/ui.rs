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

use crate::app::App;
use crate::ui::components::completion_popup::CompletionPopup;
use crate::ui::components::conversation_panel::conversation_panel::ActivePhase;
use crate::ui::components::logo::Logo;
use crate::ui::components::status_bar::status_bar::StatusState;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};

impl App<'_> {
    /// The single status the footer shows, by precedence: user-input waits
    /// first, then the current busy phase, then idle.
    fn resolve_status(&self) -> StatusState {
        if self.question_panel.is_some() {
            return StatusState::WaitingAnswer;
        }
        if !self.approval_queue.is_empty() {
            return StatusState::WaitingApproval;
        }
        let cp = &self.conversation_panel;
        match cp.phase {
            ActivePhase::Classifying => StatusState::Classifying,
            ActivePhase::ToolRunning => StatusState::ToolRunning,
            ActivePhase::CreatingToolCall => StatusState::CreatingToolCall,
            ActivePhase::Outputting => StatusState::Outputting,
            ActivePhase::None => match &cp.receiving_response {
                // Request in flight but nothing has streamed back yet: either
                // still connecting, or backing off between retries.
                Some(partial) if !partial.started() => {
                    if self.stream_retrying.load(std::sync::atomic::Ordering::Relaxed) {
                        StatusState::Retrying
                    } else {
                        StatusState::Connecting
                    }
                }
                Some(_) => StatusState::Thinking,
                None => StatusState::Idle,
            },
        }
    }
}

impl Widget for &mut App<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // The provider management panel is modal and replaces the whole UI.
        if let Some(panel) = &self.provider_panel {
            panel.render(&self.config, &self.provider_manager, area, buf);
            return;
        }

        // Resolve the single status the footer should show, then let the
        // status bar track its own busy timer.
        self.footer.status.set(self.resolve_status());
        self.footer.work_mode = self.work_mode;
        self.footer.current_model = self.current_model.clone();

        // When the model is asking a question or waiting for approval,
        // the bottom area grows; the conversation panel shrinks.
        let question_height: u16 = self
            .question_panel
            .as_ref()
            .map(|q| q.needed_height())
            .unwrap_or(3);
        let approval_height: u16 = if self.approval_queue.is_empty() {
            3
        } else {
            let detail_count = format_tool_details(
                &self.approval_queue[0].0.name,
                &self.approval_queue[0].0.arguments,
            ).len() as u16;
            4 + detail_count + 4 // title + reason + details + options
        };
        let bottom_height = question_height.max(approval_height);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(2),
                Constraint::Length(bottom_height),
                Constraint::Length(1),
            ])
            .split(area);
        let logo = Logo::new();
        logo.render(chunks[0], buf);
        self.conversation_panel.render(chunks[1], buf);

        if let Some(panel) = &self.question_panel {
            panel.render(chunks[2], buf);
        } else if !self.approval_queue.is_empty() {
            let current = self.approved_calls.len() + 1;
            let total = self.approved_calls.len() + self.approval_queue.len();
            let (call, reason) = &self.approval_queue[0];
            let detail_lines = format_tool_details(&call.name, &call.arguments);

            let labels = ["Approve", "Deny", "Approve all", "Deny all"];
            let sel = self.approval_selected;
            let option_lines: Vec<Line> = labels.iter().enumerate().map(|(i, label)| {
                let marker = if i == sel { "❯" } else { " " };
                let style = if i == sel {
                    Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Gray)
                };
                Line::from(vec![
                    Span::styled("  ", Style::default()),
                    Span::styled(format!("{marker} {label}"), style),
                ])
            }).collect();

            let mut lines: Vec<Line> = vec![
                Line::from(vec![
                    Span::styled("🛡  ", Style::default().fg(Color::Yellow)),
                    Span::styled(
                        format!("Approve tool call?  ({current}/{total})"),
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(vec![
                    Span::styled("  reason: ", Style::default().fg(Color::DarkGray)),
                    Span::styled(reason.as_str(), Style::default().fg(Color::Yellow)),
                ]),
            ];
            for line in &detail_lines {
                lines.push(Line::from(Span::styled(format!("  {line}"), Style::default().fg(Color::Gray))));
            }
            lines.extend(option_lines);
            Paragraph::new(lines)
                .block(
                    Block::default()
                        .borders(Borders::TOP)
                        .border_style(Style::default().fg(Color::Yellow)),
                )
                .render(chunks[2], buf);
        } else {
            self.input_panel.render(chunks[2], buf);
        }
        (&self.footer).render(chunks[3], buf);

        // ---- completion popup (floats above the input panel) ----
        if let Some(ref completion) = self.input_panel.completion {
            if completion.visible {
                let max_visible = 10u16;
                let count = (completion.candidates.len() as u16).min(max_visible);
                let popup_height = count;

                let token_x = chunks[2].x + 2 + completion.prefix.len() as u16;
                let longest = completion
                    .candidates
                    .iter()
                    .map(|c| c.len())
                    .max()
                    .unwrap_or(0) as u16;
                let popup_width = (longest + 2).clamp(10, chunks[2].width);

                let popup_area = Rect {
                    x: token_x.min(chunks[2].right().saturating_sub(popup_width)),
                    y: chunks[2].y.saturating_sub(popup_height),
                    width: popup_width,
                    height: popup_height.min(chunks[2].y),
                };

                let popup = CompletionPopup {
                    candidates: &completion.candidates,
                    selected: completion.selected,
                    scroll_offset: completion.scroll_offset,
                };
                popup.render(popup_area, buf);
            }
        }
    }
}

/// Parse a tool call's JSON arguments into human-readable lines.
fn format_tool_details(tool_name: &str, arguments: &str) -> Vec<String> {
    let v: serde_json::Value = match serde_json::from_str(arguments) {
        Ok(v) => v,
        Err(_) => return vec![arguments.to_string()],
    };
    match tool_name {
        "command" => {
            let mut lines = Vec::new();
            if let Some(cmd) = v.get("command").and_then(|c| c.as_str()) {
                lines.push(format!("command: {cmd}"));
            }
            if let Some(dir) = v.get("dir").and_then(|d| d.as_str()) {
                lines.push(format!("  dir: {dir}"));
            }
            if let Some(t) = v.get("timeout") {
                lines.push(format!("  timeout: {t}s"));
            }
            if lines.is_empty() { lines.push(arguments.to_string()); }
            lines
        }
        "write_file" => {
            let mut lines = Vec::new();
            if let Some(path) = v.get("path").and_then(|p| p.as_str()) {
                lines.push(format!("path: {path}"));
            }
            if let Some(content) = v.get("content").and_then(|c| c.as_str()) {
                let preview: String = content.lines().take(5).collect::<Vec<_>>().join("\n");
                let tail = if content.lines().count() > 5 { "…" } else { "" };
                lines.push(format!("content: {preview}{tail} ({len} bytes)", len = content.len()));
            }
            if lines.is_empty() { lines.push(arguments.to_string()); }
            lines
        }
        "edit_file" => {
            let mut lines = Vec::new();
            if let Some(path) = v.get("path").and_then(|p| p.as_str()) {
                lines.push(format!("path: {path}"));
            }
            if let Some(old) = v.get("old_string").and_then(|o| o.as_str()) {
                let preview: String = old.chars().take(80).collect();
                lines.push(format!("old: {preview}"));
            }
            if let Some(new) = v.get("new_string").and_then(|n| n.as_str()) {
                let preview: String = new.chars().take(80).collect();
                lines.push(format!("new: {preview}"));
            }
            if lines.is_empty() { lines.push(arguments.to_string()); }
            lines
        }
        "read_file" => {
            let mut lines = Vec::new();
            if let Some(path) = v.get("path").and_then(|p| p.as_str()) {
                lines.push(format!("path: {path}"));
            }
            if let Some(offset) = v.get("offset") {
                lines.push(format!("  offset: {offset}"));
            }
            if let Some(limit) = v.get("limit") {
                lines.push(format!("  limit: {limit}"));
            }
            lines
        }
        _ => vec![arguments.to_string()],
    }
}
