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
use crate::ui::components::sidebar::Sidebar;
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
        if self.pending_review.is_some() {
            return StatusState::WaitingApproval;
        }
        let cp = &self.conversation_panel;
        match cp.phase {
            ActivePhase::Classifying => StatusState::Classifying,
            ActivePhase::Checking => StatusState::Checking,
            ActivePhase::Compacting => StatusState::Compacting,
            ActivePhase::ToolRunning => StatusState::ToolRunning,
            ActivePhase::CreatingToolCall => StatusState::CreatingToolCall,
            ActivePhase::Outputting => StatusState::Outputting,
            ActivePhase::None => match &cp.receiving_response {
                // Request in flight but nothing has streamed back yet: either
                // still connecting, or backing off between retries.
                Some(partial) if !partial.started() => {
                    if self.cancel.stream_retrying.load(std::sync::atomic::Ordering::Relaxed) {
                        StatusState::Retrying
                    } else {
                        StatusState::Connecting
                    }
                }
                // Streaming: derive the state from what the model is emitting
                // right now — reasoning, visible text, or a tool call.
                Some(partial) => match partial.streaming_kind() {
                    Some(crate::response::partial_response::StreamingKind::ToolCall) => {
                        StatusState::CreatingToolCall
                    }
                    Some(crate::response::partial_response::StreamingKind::Message) => {
                        StatusState::Outputting
                    }
                    _ => StatusState::Thinking,
                },
                None => StatusState::Idle,
            },
        }
    }

    /// Render the main vertical layout area (conversation, todo bar,
    /// input, footer, overlays). Called either full-screen or in the left
    /// portion when the sidebar is open.  The logo/title is rendered at the
    /// top level so it spans the full width even when the sidebar is open.
    fn render_main(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        has_todo_bar: bool,
        todo_bar_height: u16,
        bottom_height: u16,
    ) {
        // Named indices into the constraint array so they don't drift when
        // rows are added or removed.
        const POS_CONV: usize = 0;
        const POS_TODO: usize = 1;
        const POS_BOTTOM: usize = 2;
        const POS_FOOTER: usize = 3;

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(2),                  // POS_CONV
                Constraint::Length(todo_bar_height), // POS_TODO
                Constraint::Length(bottom_height),   // POS_BOTTOM
                Constraint::Length(1),               // POS_FOOTER
            ])
            .split(area);
        self.conversation_panel.render(chunks[POS_CONV], buf);

        // ---- compact todo bar (inline, above the input area) ----
        if has_todo_bar {
            let pending = self.todo_list.todos.iter().filter(|t| t.status == crate::todos::TodoStatus::Pending).count();
            let in_progress = self.todo_list.todos.iter().filter(|t| t.status == crate::todos::TodoStatus::InProgress).count();
            let completed = self.todo_list.todos.iter().filter(|t| t.status == crate::todos::TodoStatus::Completed).count();
            let mut parts = Vec::new();
            if pending > 0 { parts.push(format!("{} pending", pending)); }
            if in_progress > 0 { parts.push(format!("{} in progress", in_progress)); }
            if completed > 0 { parts.push(format!("{} completed", completed)); }
            let summary = parts.join(", ");
            let line = Line::from(vec![
                Span::styled(" ☐ Todos: ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                Span::styled(summary, Style::default().fg(Color::Gray)),
                Span::styled("  │  ", Style::default().fg(Color::DarkGray)),
                Span::styled("/todo", Style::default().fg(Color::Cyan)),
                Span::styled(" to manage", Style::default().fg(Color::DarkGray)),
            ]);
            Paragraph::new(line).render(chunks[POS_TODO], buf);
        }

        if let Some(panel) = &self.question_panel {
            panel.render(chunks[POS_BOTTOM], buf);
        } else if let Some(ref review) = self.pending_review {
            let (current, total) = review.position;
            let detail_lines = crate::ui::tool_details::format_tool_details(&review.call.name, &review.call.arguments);

            let labels = ["Approve", "Deny"];
            let sel = review.selected;
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
                    Span::styled(review.reason.as_str(), Style::default().fg(Color::Yellow)),
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
                .render(chunks[POS_BOTTOM], buf);
        } else if self.work_mode == crate::classifier::WorkMode::Plan
            && self.plan_phase == crate::classifier::PlanPhase::Reviewing
        {
            let yolo_on = self.config.allow_yolo;
            let options: &[&str] = if yolo_on {
                &[
                    "Execute with Manual  (approve each action)",
                    "Execute with Auto    (AI reviews each action)",
                    "Execute with YOLO    (run everything unchecked)",
                    "Propose changes\u{2026}     (give feedback first)",
                ]
            } else {
                &[
                    "Execute with Manual  (approve each action)",
                    "Execute with Auto    (AI reviews each action)",
                    "Propose changes\u{2026}     (give feedback first)",
                ]
            };
            let sel = self.plan_review_selected;
            let option_lines: Vec<Line> = options
                .iter()
                .enumerate()
                .map(|(i, label)| {
                    let marker = if i == sel { "\u{2771}" } else { " " };
                    let style = if i == sel {
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Cyan)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::Gray)
                    };
                    Line::from(vec![
                        Span::styled("  ", Style::default()),
                        Span::styled(format!("{marker} {label}"), style),
                    ])
                })
                .collect();

            let mut lines: Vec<Line> = vec![
                Line::from(vec![
                    Span::styled(
                        "\u{1f4cb}  Plan received. Choose how to execute:",
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(vec![Span::styled(
                    "\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}",
                    Style::default().fg(Color::DarkGray),
                )]),
            ];
            lines.extend(option_lines);
            // Hint line
            lines.push(Line::from(vec![
                Span::styled("\u{2191}\u{2193}", Style::default().fg(Color::Cyan).bold()),
                Span::styled(" select  ", Style::default().fg(Color::DarkGray)),
                Span::styled("Enter", Style::default().fg(Color::Green).bold()),
                Span::styled(" confirm  ", Style::default().fg(Color::DarkGray)),
                Span::styled("Esc", Style::default().fg(Color::Cyan).bold()),
                Span::styled(" cancel", Style::default().fg(Color::DarkGray)),
            ]));

            Paragraph::new(lines)
                .block(
                    Block::default()
                        .borders(Borders::TOP)
                        .border_style(Style::default().fg(Color::Cyan)),
                )
                .render(chunks[POS_BOTTOM], buf);
        } else {
            self.input_panel.render(chunks[POS_BOTTOM], buf);
        }
        (&self.footer).render(chunks[POS_FOOTER], buf);

        // ---- completion popup (floats above the input panel) ----
        if let Some(ref completion) = self.input_panel.completion
            && completion.visible {
                let max_visible = 10u16;
                let count = (completion.candidates.len() as u16).min(max_visible);
                let popup_height = count;

                let token_x = chunks[POS_BOTTOM].x + 2 + completion.prefix.len() as u16;
                let longest = completion
                    .candidates
                    .iter()
                    .map(|c| c.len())
                    .max()
                    .unwrap_or(0) as u16;
                let popup_width = (longest + 2).clamp(10, chunks[POS_BOTTOM].width);

                let popup_area = Rect {
                    x: token_x.min(chunks[POS_BOTTOM].right().saturating_sub(popup_width)),
                    y: chunks[POS_BOTTOM].y.saturating_sub(popup_height),
                    width: popup_width,
                    height: popup_height.min(chunks[POS_BOTTOM].y),
                };

                let popup = CompletionPopup {
                    candidates: &completion.candidates,
                    selected: completion.selected,
                    scroll_offset: completion.scroll_offset,
                };
                popup.render(popup_area, buf);
            }

        // ---- todo panel (floating overlay, centered) ----
        if let Some(panel) = &self.todo_panel {
            let panel_height = panel.needed_height().min(area.height.saturating_sub(4));
            let panel_width = (area.width / 2 + 20).min(area.width.saturating_sub(4));
            let x = area.x + (area.width.saturating_sub(panel_width)) / 2;
            let y = area.y + (area.height.saturating_sub(panel_height)) / 2;
            let panel_area = Rect {
                x,
                y,
                width: panel_width,
                height: panel_height,
            };
            // Dim the background.
            for row in area.y..area.y + area.height {
                for col in area.x..area.x + area.width {
                    if let Some(cell) = buf.cell_mut((col, row))
                        && (col < panel_area.x
                            || col >= panel_area.x + panel_area.width
                            || row < panel_area.y
                            || row >= panel_area.y + panel_area.height)
                        {
                            cell.set_style(
                                Style::default()
                                    .fg(Color::DarkGray)
                                    .add_modifier(Modifier::DIM),
                            );
                        }
                }
            }
            panel.render(panel_area, buf);
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
        // The skills management panel is modal and replaces the whole UI.
        if let Some(panel) = &self.skills_panel {
            panel.render(&self.skill_registry, area, buf);
            return;
        }
        // The MCP management panel is modal and replaces the whole UI.
        if let Some(panel) = &self.mcp_panel {
            panel.render(&self.config, self.mcp_manager.as_deref(), area, buf);
            return;
        }
        // The interactive terminal panel is modal and replaces the whole UI.
        // Push the visible grid size to the PTY before painting so the child
        // reflows to the panel.
        if let Some(pane) = &mut self.terminal_pane {
            use crate::ui::components::terminal_panel;
            let grid = terminal_panel::grid_area(area);
            pane.grid = Some(grid);
            pane.maybe_resize(grid.height.max(1), grid.width.max(1));
            terminal_panel::render(pane, area, buf);
            return;
        }

        // Resolve the single status the footer should show, then let the
        // status bar track its own busy timer.
        self.footer.status.set(self.resolve_status());
        // Keep the terminal title in sync with the current status.
        crate::terminal::set_terminal_title(&format!(
            "{} {} \u{b7} programmer",
            self.footer.status.status.emoji_label(),
            self.project_name,
        ));
        // Live MCP progress rides along as status detail while a call runs
        // (progress state is cleared when the call finishes).
        self.footer.status.detail = self
            .mcp_manager
            .as_deref()
            .and_then(|m| m.active_progress())
            .map(|(server, info)| {
                let pct = info
                    .total
                    .filter(|t| *t > 0.0)
                    .map(|t| format!("{:.0}% ", (info.progress / t * 100.0).min(100.0)))
                    .unwrap_or_default();
                let msg = info.message.unwrap_or_default();
                let text = format!("{server}: {pct}{msg}");
                let mut trimmed: String = text.trim_end().chars().take(60).collect();
                if trimmed.chars().count() == 60 {
                    trimmed.push('…');
                }
                trimmed
            });
        self.footer.work_mode = self.work_mode;
        self.footer.current_model = self.current_model.clone();
        self.footer.lsp_configured = self.diag.lsp_configured;
        self.footer.active_skills = self
            .skill_registry
            .activated_names()
            .join(",");

        // When the model is asking a question or waiting for approval,
        // the bottom area grows; the conversation panel shrinks.
        let question_height: u16 = self
            .question_panel
            .as_ref()
            .map(|q| q.needed_height())
            .unwrap_or(3);
        let approval_height: u16 = if let Some(ref review) = self.pending_review {
            let detail_count = crate::ui::tool_details::format_tool_details(
                &review.call.name,
                &review.call.arguments,
            ).len() as u16;
            4 + detail_count + 2 // title + reason + details + options (2: approve/deny)
        } else {
            3
        };
        // The bottom row is either a modal (question / approval / plan review) or the input.
        // When it's the input, let it grow with multi-line content.
        let plan_review_height: u16 = if self.work_mode == crate::classifier::WorkMode::Plan
            && self.plan_phase == crate::classifier::PlanPhase::Reviewing
        {
            let option_count: u16 = if self.config.allow_yolo { 4 } else { 3 };
            5 + option_count // header + separator + options
        } else {
            0
        };
        let bottom_height = if self.question_panel.is_some() {
            question_height
        } else if self.pending_review.is_some() {
            approval_height
        } else if plan_review_height > 0 {
            plan_review_height
        } else {
            self.input_panel.needed_height()
        };

        // Show a compact todo bar when there are items and the full modal
        // isn't open.
        let has_todo_bar = !self.todo_list.todos.is_empty() && self.todo_panel.is_none();
        let todo_bar_height: u16 = if has_todo_bar { 1 } else { 0 };

        // ---- logo at top (full width, even with sidebar open) ----
        let vert = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),  // logo
                Constraint::Min(1),     // content area
            ])
            .split(area);
        let logo = Logo::new();
        logo.render(vert[0], buf);
        let content_area = vert[1];

        // ---- sidebar: conditionally split the content area horizontally ----
        if self.sidebar.is_some() {
            let sidebar_width = Sidebar::needed_width().min(content_area.width / 3);
            let horiz = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Min(10),               // main area
                    Constraint::Length(sidebar_width), // sidebar
                ])
                .split(content_area);
            self.sidebar_area = Some(horiz[1]);

            self.render_main(horiz[0], buf, has_todo_bar, todo_bar_height, bottom_height);

            self.sidebar.as_mut().unwrap()
                .render(
                    horiz[1],
                    buf,
                    self.diagnostics_state.lock().unwrap().baseline.as_deref().unwrap_or(&[]),
                    self.diag.lsp_configured,
                    self.mcp_manager.as_deref(),
                    &self.todo_list,
                    &crate::tasks::snapshot_all(),
                );
        } else {
            self.sidebar_area = None;
            self.render_main(content_area, buf, has_todo_bar, todo_bar_height, bottom_height);
        }
    }
}
