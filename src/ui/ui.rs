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

use crate::app::{App, PointerTarget};
use crate::ui::components::completion_popup::CompletionPopup;
use crate::ui::components::conversation_panel::conversation_panel::ActivePhase;
use crate::ui::components::logo::Logo;
use crate::ui::components::status_bar::status_bar::StatusState;
use crate::ui::markdown_theme::palette;
use crate::ui::theme::role;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};

fn terminal_title_subject<'a>(session_title: &'a str, project_name: &'a str) -> &'a str {
    let session_title = session_title.trim();
    if session_title.is_empty() {
        project_name
    } else {
        session_title
    }
}

fn status_for_waiting_subagents(waiting: bool) -> Option<StatusState> {
    waiting.then_some(StatusState::WaitingSubagents)
}

fn sidebar_width(preferred: u16, total: u16) -> u16 {
    let max = total.saturating_sub(20).max(1);
    preferred.clamp(20.min(max), max)
}

fn snapshot_buffer(area: Rect, buf: &Buffer) -> Buffer {
    let mut snapshot = Buffer::empty(area);
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if let (Some(source), Some(target)) = (buf.cell((x, y)), snapshot.cell_mut((x, y))) {
                *target = source.clone();
            }
        }
    }
    snapshot
}

fn paint_snapshot(snapshot: &Buffer, area: Rect, buf: &mut Buffer) {
    Clear.render(area, buf);
    let right = area.right().min(snapshot.area.right());
    let bottom = area.bottom().min(snapshot.area.bottom());
    for y in area.y.max(snapshot.area.y)..bottom {
        for x in area.x.max(snapshot.area.x)..right {
            if let (Some(source), Some(target)) = (snapshot.cell((x, y)), buf.cell_mut((x, y))) {
                *target = source.clone();
            }
        }
    }
}

fn paint_pointer_row(
    buf: &mut Buffer,
    area: Rect,
    row: u16,
    pressed: bool,
    hover_background: ratatui::style::Color,
) {
    let y = area.y.saturating_add(row);
    if y >= area.bottom() {
        return;
    }
    for x in area.x..area.right() {
        if let Some(cell) = buf.cell_mut((x, y)) {
            if pressed {
                cell.set_style(
                    Style::default()
                        .fg(role::SELECTION_TEXT)
                        .bg(role::INTERACTION_PRESSED_BG)
                        .add_modifier(Modifier::BOLD),
                );
            } else {
                cell.set_bg(hover_background);
            }
        }
    }
}

fn status_for_pending_turn(active_turn: bool, retrying: bool) -> StatusState {
    if retrying {
        StatusState::Retrying
    } else if active_turn {
        StatusState::Connecting
    } else {
        StatusState::Idle
    }
}

impl App<'_> {
    /// The single status the footer shows, by precedence: user-input waits
    /// first, then the current busy phase, then idle.
    pub(crate) fn resolve_status(&self) -> StatusState {
        if self.agent_loop.phase == ActivePhase::Cancelling {
            return StatusState::Cancelling;
        }
        if self.ui.question_panel.is_some() {
            return StatusState::WaitingAnswer;
        }
        if self.agent_loop.pending_review.is_some() {
            return StatusState::WaitingApproval;
        }
        // This is set only while the main runner is executing an `agent wait`
        // tool call. Merely having running children does not mean the parent is
        // waiting for them.
        if let Some(status) = status_for_waiting_subagents(self.agent_loop.waiting_for_subagents) {
            return status;
        }
        // At a mandatory safe point the runner has finished its tools and is
        // waiting for compaction; its last phase is no longer current work.
        // A concurrent background pass must not mask foreground activity.
        if self.agent_loop.auto_compact.mandatory_waiting
            && self.agent_loop.auto_compact.active_id.is_some()
        {
            return StatusState::Compacting;
        }
        let cp = &self.ui.conversation_panel;
        match self.agent_loop.phase {
            ActivePhase::Classifying => StatusState::Classifying,
            ActivePhase::Associating => StatusState::Associating,
            ActivePhase::Checking => StatusState::Checking,
            ActivePhase::Compacting => StatusState::Compacting,
            ActivePhase::Cancelling => StatusState::Cancelling,
            ActivePhase::ToolRunning => StatusState::ToolRunning,
            ActivePhase::CreatingToolCall => StatusState::CreatingToolCall,
            ActivePhase::Outputting => StatusState::Outputting,
            ActivePhase::None if self.agent_loop.auto_compact.active_id.is_some() => {
                StatusState::Compacting
            }
            ActivePhase::None => match &cp.receiving_response {
                // Request in flight but nothing has streamed back yet: either
                // still connecting, or backing off between retries.
                Some(partial) if !partial.started() => status_for_pending_turn(
                    true,
                    self.agent_loop
                        .cancel
                        .stream_retrying
                        .load(std::sync::atomic::Ordering::Relaxed),
                ),
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
                None => status_for_pending_turn(
                    self.agent_loop.cancel.active_id.is_some(),
                    self.agent_loop
                        .cancel
                        .stream_retrying
                        .load(std::sync::atomic::Ordering::Relaxed),
                ),
            },
        }
    }

    /// Render the main vertical layout area (conversation, todo bar,
    /// input, footer, overlays). Called either full-screen or in the left
    /// portion when the sidebar is open.  The logo/title is rendered at the
    /// top level so it spans the full width even when the sidebar is open.
    fn render_main(&mut self, area: Rect, buf: &mut Buffer, bottom_height: u16) {
        // Named indices into the constraint array so they don't drift when
        // rows are added or removed.
        const POS_CONV: usize = 0;
        const POS_BOTTOM: usize = 1;
        const POS_FOOTER: usize = 2;

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(2),                // POS_CONV
                Constraint::Length(bottom_height), // POS_BOTTOM
                Constraint::Length(1),             // POS_FOOTER
            ])
            .split(area);

        // Only the topmost pointer surface may retain feedback. This prevents
        // a conversation/sidebar hover from showing through a newly opened
        // question, approval bar, completion popup, or todo overlay.
        let todo_owns_pointer = self.ui.todo_panel.is_some();
        let question_owns_pointer = !todo_owns_pointer && self.ui.question_panel.is_some();
        let approval_owns_pointer = !todo_owns_pointer
            && !question_owns_pointer
            && self.agent_loop.pending_review.is_some();
        let plan_owns_pointer = !todo_owns_pointer
            && !question_owns_pointer
            && !approval_owns_pointer
            && self.agent_loop.session.work_mode == crate::classifier::WorkMode::Plan
            && self.agent_loop.plan_phase == crate::classifier::PlanPhase::Reviewing;
        let completion_owns_pointer = !todo_owns_pointer
            && !question_owns_pointer
            && !approval_owns_pointer
            && !plan_owns_pointer
            && self
                .ui
                .input_panel
                .completion
                .as_ref()
                .is_some_and(|completion| completion.visible);
        let overlay_owns_pointer = todo_owns_pointer
            || question_owns_pointer
            || approval_owns_pointer
            || plan_owns_pointer
            || completion_owns_pointer;
        let target_is_visible = |target: &PointerTarget| match target {
            PointerTarget::Todo { .. } => todo_owns_pointer,
            PointerTarget::Question { .. } => question_owns_pointer,
            PointerTarget::Approval { .. } => approval_owns_pointer,
            PointerTarget::Plan { .. } => plan_owns_pointer,
            PointerTarget::Completion { .. } => completion_owns_pointer,
            PointerTarget::Sidebar { .. }
            | PointerTarget::SidebarResize
            | PointerTarget::Footer(_) => !overlay_owns_pointer,
        };
        let stale_hover = self
            .ui
            .pointer_hover
            .as_ref()
            .is_some_and(|target| !target_is_visible(target));
        let stale_press = self
            .ui
            .pointer_pressed
            .as_ref()
            .is_some_and(|target| !target_is_visible(target));
        if stale_hover || stale_press {
            self.ui.clear_pointer_feedback();
        }
        if overlay_owns_pointer {
            self.ui.conversation_panel.clear_pointer_feedback();
        }

        self.ui.conversation_panel.render(chunks[POS_CONV], buf);
        self.ui.bottom_interaction_area = Some(chunks[POS_BOTTOM]);
        let hover_background = self.ui.pointer_hover_background();

        if let Some(panel) = &self.ui.question_panel {
            panel.render(chunks[POS_BOTTOM], buf);
            let hovered = match self.ui.pointer_hover {
                Some(PointerTarget::Question { index }) => Some(index),
                _ => None,
            };
            let pressed = match self.ui.pointer_pressed {
                Some(PointerTarget::Question { index }) => Some(index),
                _ => None,
            };
            if let Some(index) = hovered
                && let Some(row) = panel.choice_row_area(chunks[POS_BOTTOM], index)
            {
                paint_pointer_row(buf, row, 0, pressed == Some(index), hover_background);
            }
        } else if let Some(ref review) = self.agent_loop.pending_review {
            let (current, total) = review.position;
            let detail_lines = crate::ui::tool_details::format_tool_details(
                &review.call.name,
                &review.call.arguments,
            );

            let labels = ["Approve", "Deny"];
            let sel = review.selected;
            let option_lines: Vec<Line> = labels
                .iter()
                .enumerate()
                .map(|(i, label)| {
                    let marker = if i == sel { "❯" } else { " " };
                    let hovered = matches!(
                        self.ui.pointer_hover,
                        Some(PointerTarget::Approval { index }) if index == i
                    );
                    let pressed = matches!(
                        self.ui.pointer_pressed,
                        Some(PointerTarget::Approval { index }) if index == i
                    );
                    let style = if hovered && pressed {
                        Style::default()
                            .fg(role::SELECTION_TEXT)
                            .bg(role::INTERACTION_PRESSED_BG)
                            .add_modifier(Modifier::BOLD)
                    } else if hovered {
                        Style::default().fg(palette::TEXT).bg(hover_background)
                    } else if i == sel {
                        Style::default()
                            .fg(role::SELECTION_TEXT)
                            .bg(role::SELECTION_BG)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(palette::MUTED)
                    };
                    Line::from(vec![
                        Span::styled("  ", Style::default()),
                        Span::styled(format!("{marker} {label}"), style),
                    ])
                })
                .collect();

            let mut lines: Vec<Line> = vec![
                Line::from(vec![
                    Span::styled("🛡  ", Style::default().fg(palette::YELLOW)),
                    Span::styled(
                        format!("Approve tool call?  ({current}/{total})"),
                        Style::default()
                            .fg(palette::TEXT)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(vec![
                    Span::styled("  reason: ", Style::default().fg(palette::MUTED)),
                    Span::styled(review.reason.as_str(), Style::default().fg(palette::YELLOW)),
                ]),
            ];
            for line in &detail_lines {
                lines.push(Line::from(Span::styled(
                    format!("  {line}"),
                    Style::default().fg(palette::MUTED),
                )));
            }
            lines.extend(option_lines);
            Paragraph::new(lines)
                .block(
                    Block::default()
                        .borders(Borders::TOP)
                        .border_style(Style::default().fg(palette::YELLOW)),
                )
                .render(chunks[POS_BOTTOM], buf);
            if let Some(PointerTarget::Approval { index }) = self.ui.pointer_hover {
                paint_pointer_row(
                    buf,
                    chunks[POS_BOTTOM],
                    3u16.saturating_add(detail_lines.len() as u16)
                        .saturating_add(index as u16),
                    matches!(
                        self.ui.pointer_pressed,
                        Some(PointerTarget::Approval { index: pressed }) if pressed == index
                    ),
                    hover_background,
                );
            }
        } else if self.agent_loop.session.work_mode == crate::classifier::WorkMode::Plan
            && self.agent_loop.plan_phase == crate::classifier::PlanPhase::Reviewing
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
            let sel = self.ui.plan_review_selected;
            let option_lines: Vec<Line> = options
                .iter()
                .enumerate()
                .map(|(i, label)| {
                    let marker = if i == sel { "\u{2771}" } else { " " };
                    let hovered = matches!(
                        self.ui.pointer_hover,
                        Some(PointerTarget::Plan { index }) if index == i
                    );
                    let pressed = matches!(
                        self.ui.pointer_pressed,
                        Some(PointerTarget::Plan { index }) if index == i
                    );
                    let style = if hovered && pressed {
                        Style::default()
                            .fg(role::SELECTION_TEXT)
                            .bg(role::INTERACTION_PRESSED_BG)
                            .add_modifier(Modifier::BOLD)
                    } else if hovered {
                        Style::default().fg(palette::TEXT).bg(hover_background)
                    } else if i == sel {
                        Style::default()
                            .fg(role::SELECTION_TEXT)
                            .bg(role::SELECTION_BG)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(palette::MUTED)
                    };
                    Line::from(vec![
                        Span::styled("  ", Style::default()),
                        Span::styled(format!("{marker} {label}"), style),
                    ])
                })
                .collect();

            let mut lines: Vec<Line> = vec![
                Line::from(vec![Span::styled(
                    "\u{1f4cb}  Plan received. Choose how to execute:",
                    Style::default()
                        .fg(palette::TEXT)
                        .add_modifier(Modifier::BOLD),
                )]),
                Line::from(vec![Span::styled(
                    "\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}",
                    Style::default().fg(palette::BORDER),
                )]),
            ];
            lines.extend(option_lines);
            // Hint line
            lines.push(Line::from(vec![
                Span::styled(
                    "\u{2191}\u{2193}",
                    Style::default().fg(palette::BLUE).bold(),
                ),
                Span::styled(" select  ", Style::default().fg(palette::MUTED)),
                Span::styled("Enter", Style::default().fg(palette::GREEN).bold()),
                Span::styled(" confirm  ", Style::default().fg(palette::MUTED)),
                Span::styled("Esc", Style::default().fg(palette::BLUE).bold()),
                Span::styled(" cancel", Style::default().fg(palette::MUTED)),
            ]));

            Paragraph::new(lines)
                .block(
                    Block::default()
                        .borders(Borders::TOP)
                        .border_style(Style::default().fg(palette::BLUE)),
                )
                .render(chunks[POS_BOTTOM], buf);
            if let Some(PointerTarget::Plan { index }) = self.ui.pointer_hover {
                paint_pointer_row(
                    buf,
                    chunks[POS_BOTTOM],
                    3u16.saturating_add(index as u16),
                    matches!(
                        self.ui.pointer_pressed,
                        Some(PointerTarget::Plan { index: pressed }) if pressed == index
                    ),
                    hover_background,
                );
            }
        } else {
            self.ui.input_panel.render(chunks[POS_BOTTOM], buf);
        }
        self.ui.footer_area = Some(chunks[POS_FOOTER]);
        (&self.ui.footer).render(chunks[POS_FOOTER], buf);
        if let Some(PointerTarget::Footer(target)) = self.ui.pointer_hover
            && let Some((_, target_area)) = self
                .ui
                .footer
                .regions(chunks[POS_FOOTER])
                .into_iter()
                .find(|(candidate, _)| *candidate == target)
        {
            paint_pointer_row(
                buf,
                target_area,
                0,
                matches!(self.ui.pointer_pressed, Some(PointerTarget::Footer(pressed)) if pressed == target),
                hover_background,
            );
        }

        // ---- completion popup (floats above the input panel) ----
        if completion_owns_pointer && let Some(ref completion) = self.ui.input_panel.completion {
            let max_visible = 10u16;
            let count = (completion.candidates.len() as u16).min(max_visible);
            let popup_height = count;

            let token_x = chunks[POS_BOTTOM].x + 2 + completion.prefix.len() as u16;
            let longest = completion
                .candidates
                .iter()
                .map(|candidate| candidate.label.len())
                .max()
                .unwrap_or(0) as u16;
            let popup_width = (longest + 2).clamp(10, chunks[POS_BOTTOM].width);

            let popup_area = Rect {
                x: token_x.min(chunks[POS_BOTTOM].right().saturating_sub(popup_width)),
                y: chunks[POS_BOTTOM].y.saturating_sub(popup_height),
                width: popup_width,
                height: popup_height.min(chunks[POS_BOTTOM].y),
            };

            self.ui.completion_area = Some(popup_area);
            let popup = CompletionPopup {
                candidates: &completion.candidates,
                label: |candidate| candidate.label.as_str(),
                selected: completion.selected,
                scroll_offset: completion.scroll_offset,
                hovered: match self.ui.pointer_hover {
                    Some(PointerTarget::Completion { index }) => Some(index),
                    _ => None,
                },
                pressed: match self.ui.pointer_pressed {
                    Some(PointerTarget::Completion { index }) => Some(index),
                    _ => None,
                },
                hover_background,
            };
            popup.render(popup_area, buf);
        }

        // ---- todo panel (floating overlay, centered) ----
        if let Some(panel) = &self.ui.todo_panel {
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
                                .fg(palette::MUTED)
                                .add_modifier(Modifier::DIM),
                        );
                    }
                }
            }
            self.ui.todo_area = Some(panel_area);
            panel.render(panel_area, buf);
            if let Some(PointerTarget::Todo { index }) = self.ui.pointer_hover {
                let inner = Block::default().borders(Borders::ALL).inner(panel_area);
                let row = index.saturating_sub(panel.scroll_offset()) as u16;
                paint_pointer_row(
                    buf,
                    inner,
                    row,
                    matches!(
                        self.ui.pointer_pressed,
                        Some(PointerTarget::Todo { index: pressed }) if pressed == index
                    ),
                    hover_background,
                );
            }
        }
    }
}

impl Widget for &mut App<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.ui.bottom_interaction_area = None;
        self.ui.footer_area = None;
        self.ui.completion_area = None;
        self.ui.todo_area = None;
        self.ui.conversation_panel.phase = self.agent_loop.phase;
        self.ui.conversation_panel.pending_message = self
            .agent_loop
            .pending_request
            .as_ref()
            .map(|request| request.text.clone());
        let theme = self.config.theme;
        let terminal_pane = self.ui.terminal_pane.is_some();
        self.render_content(area, buf);
        // A child terminal owns its ANSI colors, not the application's theme.
        if !terminal_pane {
            crate::ui::theme::apply(theme, area, buf);
        }
    }
}

impl App<'_> {
    fn render_content(&mut self, area: Rect, buf: &mut Buffer) {
        self.ui.frame_area = area;
        if let Some(sidebar) = &mut self.ui.sidebar {
            sidebar.hide();
        }
        if self.ui.image_viewer.is_some() {
            self.ui.clear_pointer_feedback();
            self.ui.conversation_panel.clear_pointer_feedback();
        }
        let full_screen_surface = self.ui.provider_panel.is_some()
            || self.ui.rewind_panel.is_some()
            || self.ui.skills_panel.is_some()
            || self.ui.mcp_panel.is_some()
            || self.ui.diagnostics_panel.is_some()
            || self.ui.security_panel.is_some()
            || self.ui.terminal_pane.is_some()
            || self.ui.activity_page.is_some()
            || self.ui.agent_panel.is_some();
        if full_screen_surface {
            self.ui.clear_pointer_feedback();
            self.ui.conversation_panel.clear_pointer_feedback();
        }
        if let Some(panel) = &self.ui.provider_panel {
            panel.render(&self.config, &self.provider_manager, area, buf);
            return;
        }
        if let Some(panel) = &self.ui.rewind_panel {
            panel.render(area, buf);
            return;
        }
        // The skills management panel is modal and replaces the whole UI.
        if let Some(panel) = &self.ui.skills_panel {
            panel.render(&self.agent_loop.skill_registry, area, buf);
            return;
        }
        // The MCP management panel is modal and replaces the whole UI.
        if let Some(panel) = &self.ui.mcp_panel {
            panel.render(
                &self.config,
                self.mcp_runtime.connections().map(AsRef::as_ref),
                area,
                buf,
            );
            return;
        }
        // The diagnostics management panel is modal and replaces the whole UI.
        if let Some(panel) = &self.ui.diagnostics_panel {
            panel.render(area, buf);
            return;
        }
        // The security profile panel is modal and replaces the whole UI.
        if let Some(panel) = &self.ui.security_panel {
            panel.render(&self.config, area, buf);
            return;
        }
        // The task panel is modal and replaces the whole UI. Interactive tasks
        // receive the visible grid size; pipe tasks render captured output.
        if let Some(pane) = &mut self.ui.terminal_pane {
            use crate::ui::components::terminal_panel;
            let grid = terminal_panel::grid_area(area);
            pane.grid = Some(grid);
            pane.maybe_resize(grid.height.max(1), grid.width.max(1));
            terminal_panel::render(pane, area, buf);
            return;
        }
        if let Some(page) = &mut self.ui.activity_page {
            page.panel.render(area, buf);
            return;
        }
        if let Some(panel) = &mut self.ui.agent_panel {
            panel.render(area, buf);
            return;
        }

        // Resolve the single status the footer should show, then let the
        // status bar track its own busy timer.
        self.ui.footer.status.set(self.resolve_status());
        // Keep the terminal title in sync with the current status. Once the
        // session has a generated title, use it in place of the project
        // directory so several Programmer windows remain distinguishable.
        let title_subject =
            terminal_title_subject(&self.agent_loop.session.title, &self.project_name);
        crate::terminal::set_terminal_title(&format!(
            "{} {} \u{b7} programmer",
            self.ui.footer.status.status.emoji_label(),
            title_subject,
        ));
        self.ui.footer.status.detail = (self.ui.footer.status.status == StatusState::Cancelling)
            .then(|| {
                format!(
                    "waiting for {} to return (last observed)",
                    self.agent_loop
                        .cancel
                        .activity
                        .as_deref()
                        .unwrap_or("runner")
                )
            });
        self.ui.footer.work_mode = self.agent_loop.session.work_mode;
        self.ui.footer.sandbox_mode = self.security.sandbox_mode();
        self.ui.footer.sandbox_profile = self.config.active_security_profile.clone();
        self.ui.footer.current_model = self.agent_loop.session.current_model.clone();
        self.ui.footer.thinking_level = self.agent_loop.session.thinking_level;
        self.ui.footer.lsp_configured = self
            .agent_loop
            .session
            .diagnostics_state
            .lock()
            .unwrap()
            .lsp_configured;

        // When the model is asking a question or waiting for approval,
        // the bottom area grows; the conversation panel shrinks.
        let question_height: u16 = self
            .ui
            .question_panel
            .as_ref()
            .map(|q| {
                let sidebar_width = if self.ui.sidebar.is_some() {
                    sidebar_width(self.ui.sidebar_width, area.width)
                } else {
                    0
                };
                q.needed_height_for_width(area.width.saturating_sub(sidebar_width))
            })
            .unwrap_or(3);
        let approval_height: u16 = if let Some(ref review) = self.agent_loop.pending_review {
            let detail_count = crate::ui::tool_details::format_tool_details(
                &review.call.name,
                &review.call.arguments,
            )
            .len() as u16;
            4 + detail_count + 2 // title + reason + details + options (2: approve/deny)
        } else {
            3
        };
        // The bottom row is either a modal (question / approval / plan review) or the input.
        // When it's the input, let it grow with multi-line content.
        let plan_review_height: u16 = if self.agent_loop.session.work_mode
            == crate::classifier::WorkMode::Plan
            && self.agent_loop.plan_phase == crate::classifier::PlanPhase::Reviewing
        {
            let option_count: u16 = if self.config.allow_yolo { 4 } else { 3 };
            5 + option_count // header + separator + options
        } else {
            0
        };
        let bottom_height = if self.ui.question_panel.is_some() {
            question_height
        } else if self.agent_loop.pending_review.is_some() {
            approval_height
        } else if plan_review_height > 0 {
            plan_review_height
        } else {
            self.ui.input_panel.needed_height()
        };

        // ---- logo at top (full width, even with sidebar open) ----
        let vert = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2), // logo
                Constraint::Min(1),    // content area
            ])
            .split(area);
        let title = if self.agent_loop.session.title.is_empty() {
            "Programmer"
        } else {
            &self.agent_loop.session.title
        };
        let content_area = vert[1];
        let sidebar_divider_x = self.ui.sidebar.as_ref().map(|_| {
            content_area
                .right()
                .saturating_sub(sidebar_width(self.ui.sidebar_width, content_area.width))
        });
        let sidebar_resize_preview_x = self.ui.sidebar_resize_preview.map(|width| {
            content_area
                .right()
                .saturating_sub(sidebar_width(width, content_area.width))
        });
        let mut logo = Logo::new(title)
            .with_dream(self.dream_active.load(std::sync::atomic::Ordering::Relaxed));
        if let Some(x) = sidebar_divider_x {
            logo = logo.with_divider_at(x);
        }
        logo.render(vert[0], buf);
        if let Some(x) = sidebar_resize_preview_x.filter(|x| Some(*x) != sidebar_divider_x)
            && let Some(cell) = buf.cell_mut((x, vert[0].y + 1))
        {
            cell.set_symbol("┬").set_style(
                Style::default()
                    .fg(role::FOCUS)
                    .bg(palette::CODE_BG)
                    .add_modifier(Modifier::BOLD),
            );
        }

        // ---- sidebar: conditionally split the content area horizontally ----
        if self.ui.sidebar.is_some() {
            let rendered_sidebar_width = sidebar_width(self.ui.sidebar_width, content_area.width);
            let horiz = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Min(10),                        // main area
                    Constraint::Length(rendered_sidebar_width), // sidebar
                ])
                .split(content_area);

            let main_area = horiz[0];
            if self.ui.sidebar_resizing {
                let cache_valid =
                    self.ui
                        .sidebar_resize_main
                        .as_ref()
                        .is_some_and(|(screen_width, snapshot)| {
                            *screen_width == area.width
                                && snapshot.area.x == main_area.x
                                && snapshot.area.y == main_area.y
                                && snapshot.area.height == main_area.height
                        });
                if !cache_valid {
                    self.ui.sidebar_resize_main = None;
                }
                if let Some((_, snapshot)) = &self.ui.sidebar_resize_main {
                    paint_snapshot(snapshot, main_area, buf);
                } else {
                    self.render_main(main_area, buf, bottom_height);
                    self.ui.sidebar_resize_main =
                        Some((area.width, snapshot_buffer(main_area, buf)));
                }
            } else {
                self.ui.sidebar_resize_main = None;
                self.render_main(main_area, buf, bottom_height);
            }

            let expanded_task_ids = self
                .ui
                .sidebar
                .as_ref()
                .map(|sidebar| sidebar.expanded_task_ids().clone())
                .unwrap_or_default();
            let sidebar_tasks = self
                .agent_loop
                .session
                .tasks
                .snapshot_for_sidebar(&expanded_task_ids);
            let sidebar_agents = self.agent_loop.session.agents.snapshot_all();
            let hover_row = match self.ui.pointer_hover {
                Some(PointerTarget::Sidebar { row }) => Some(row),
                _ => None,
            };
            let pressed_row = match self.ui.pointer_pressed {
                Some(PointerTarget::Sidebar { row }) => Some(row),
                _ => None,
            };
            let hover_background = self.ui.pointer_hover_background();
            self.ui.sidebar.as_mut().unwrap().set_pointer_rows(
                hover_row,
                pressed_row,
                hover_background,
            );
            let active_provider = self
                .agent_loop
                .session
                .current_model
                .split_once('/')
                .map_or(self.config.default_provider.as_str(), |(provider, _)| {
                    provider
                });
            let diagnostics_state = self.agent_loop.session.diagnostics_state.lock().unwrap();
            self.ui.sidebar.as_mut().unwrap().render(
                horiz[1],
                buf,
                diagnostics_state.baseline.as_deref().unwrap_or(&[]),
                diagnostics_state.lsp_configured,
                self.mcp_runtime.statuses(),
                self.provider_manager.model_statuses(),
                active_provider,
                self.agent_loop.skill_registry.activated_names(),
                &self.ui.todo_list,
                &sidebar_tasks,
                &sidebar_agents,
            );
            if matches!(self.ui.pointer_hover, Some(PointerTarget::SidebarResize)) {
                let x = sidebar_resize_preview_x.unwrap_or(horiz[1].x);
                let style = if self.ui.sidebar_resizing {
                    Style::default()
                        .fg(role::FOCUS)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(role::FOCUS)
                };
                for y in horiz[1].y..horiz[1].bottom() {
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.set_symbol("│").set_style(style);
                    }
                }
            }
        } else {
            self.ui.sidebar_resize_main = None;
            self.render_main(content_area, buf, bottom_height);
        }

        if let Some(viewer) = self.ui.image_viewer.as_mut() {
            viewer.render(area, buf);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        sidebar_width, status_for_pending_turn, status_for_waiting_subagents,
        terminal_title_subject,
    };
    use crate::ui::components::status_bar::status_bar::StatusState;

    #[test]
    fn active_turn_is_connecting_before_streaming_phase_arrives() {
        assert_eq!(
            status_for_pending_turn(true, false),
            StatusState::Connecting
        );
        assert_eq!(status_for_pending_turn(true, true), StatusState::Retrying);
        assert_eq!(status_for_pending_turn(false, false), StatusState::Idle);
    }

    #[test]
    fn waiting_subagents_requires_an_active_agent_wait_call() {
        assert_eq!(
            status_for_waiting_subagents(true),
            Some(StatusState::WaitingSubagents)
        );
        assert_eq!(status_for_waiting_subagents(false), None);
    }

    #[test]
    fn sidebar_width_is_resizable_without_starving_the_main_pane() {
        assert_eq!(sidebar_width(32, 120), 32);
        assert_eq!(sidebar_width(48, 120), 48);
        assert_eq!(sidebar_width(80, 60), 40);
        assert_eq!(sidebar_width(10, 120), 20);
    }

    #[test]
    fn session_title_replaces_project_name_in_terminal_title() {
        assert_eq!(
            terminal_title_subject("Fix compaction", "programmer"),
            "Fix compaction"
        );
        assert_eq!(terminal_title_subject("   ", "programmer"), "programmer");
    }
}
