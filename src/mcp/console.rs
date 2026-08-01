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

//! Interactive dashboard for the `programmer mcp http` server.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, List, ListItem, ListState, Paragraph, StatefulWidget, Wrap,
};
use tokio::sync::mpsc;

use super::http_server::{ApprovalRequest, CallOutcome, ConsoleEvent};
use crate::classifier::WorkMode;
use crate::ui::markdown_theme::palette;

const MAX_CALLS: usize = 500;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CallStatus {
    Received,
    AwaitingApproval,
    Running,
    Succeeded,
    Failed,
    Denied,
}

impl CallStatus {
    fn icon(self) -> &'static str {
        match self {
            Self::Received => "·",
            Self::AwaitingApproval => "?",
            Self::Running => "◆",
            Self::Succeeded => "✓",
            Self::Failed => "!",
            Self::Denied => "×",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Received => "received",
            Self::AwaitingApproval => "awaiting approval",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Denied => "denied",
        }
    }

    fn color(self) -> ratatui::style::Color {
        match self {
            Self::Received => palette::MUTED,
            Self::AwaitingApproval => palette::YELLOW,
            Self::Running => palette::CYAN,
            Self::Succeeded => palette::GREEN,
            Self::Failed | Self::Denied => palette::RED,
        }
    }

    fn is_active(self) -> bool {
        matches!(
            self,
            Self::Received | Self::AwaitingApproval | Self::Running
        )
    }
}

#[derive(Debug)]
struct CallRecord {
    id: u64,
    tool: String,
    args: String,
    status: CallStatus,
    detail: String,
}

struct ConsoleState {
    mode: Arc<Mutex<WorkMode>>,
    addr: SocketAddr,
    allow_yolo: bool,
    calls: VecDeque<CallRecord>,
    pending: VecDeque<ApprovalRequest>,
    selected: Option<usize>,
    detail_scroll: u16,
    follow_latest: bool,
}

impl ConsoleState {
    fn new(mode: Arc<Mutex<WorkMode>>, addr: SocketAddr, allow_yolo: bool) -> Self {
        Self {
            mode,
            addr,
            allow_yolo,
            calls: VecDeque::new(),
            pending: VecDeque::new(),
            selected: None,
            detail_scroll: 0,
            follow_latest: true,
        }
    }

    fn apply(&mut self, event: ConsoleEvent) {
        match event {
            ConsoleEvent::Started { id, tool, args } => {
                self.calls.push_back(CallRecord {
                    id,
                    tool,
                    args,
                    status: CallStatus::Received,
                    detail: String::new(),
                });
                self.trim_history();
                if self.follow_latest {
                    self.select_latest();
                }
            }
            ConsoleEvent::ApprovalRequested(req) => {
                self.set_status(req.id, CallStatus::AwaitingApproval);
                self.pending.push_back(req);
            }
            ConsoleEvent::Running { id } => self.set_status(id, CallStatus::Running),
            ConsoleEvent::Finished {
                id,
                outcome,
                detail,
            } => {
                if let Some(call) = self.calls.iter_mut().find(|call| call.id == id) {
                    call.status = match outcome {
                        CallOutcome::Succeeded => CallStatus::Succeeded,
                        CallOutcome::Failed => CallStatus::Failed,
                        CallOutcome::Denied => CallStatus::Denied,
                    };
                    call.detail = detail;
                }
            }
        }
    }

    fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        if modifiers.contains(KeyModifiers::CONTROL) {
            match code {
                KeyCode::Char('c') => return false,
                KeyCode::Char('t') => {
                    let mut mode = self.mode.lock().unwrap();
                    *mode = mode.next(self.allow_yolo);
                    return true;
                }
                _ => {}
            }
        }

        match code {
            KeyCode::Char('q') if self.pending.is_empty() => return false,
            KeyCode::Up | KeyCode::Char('k') => self.select_previous(),
            KeyCode::Down | KeyCode::Char('j') => self.select_next(),
            KeyCode::Home | KeyCode::Char('g') => self.select_first(),
            KeyCode::End | KeyCode::Char('G') => self.select_latest(),
            KeyCode::PageUp => self.detail_scroll = self.detail_scroll.saturating_sub(5),
            KeyCode::PageDown => self.detail_scroll = self.detail_scroll.saturating_add(5),
            KeyCode::Char('y') | KeyCode::Char('Y') => self.resolve_approval(true),
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                self.resolve_approval(false);
            }
            _ => {}
        }
        true
    }

    fn set_status(&mut self, id: u64, status: CallStatus) {
        if let Some(call) = self.calls.iter_mut().find(|call| call.id == id) {
            call.status = status;
        }
    }

    fn resolve_approval(&mut self, approved: bool) {
        let Some(req) = self.pending.pop_front() else {
            return;
        };
        self.set_status(
            req.id,
            if approved {
                CallStatus::Running
            } else {
                CallStatus::Denied
            },
        );
        let _ = req.respond.send(approved);
    }

    fn select_previous(&mut self) {
        if self.calls.is_empty() {
            return;
        }
        self.selected = Some(self.selected.unwrap_or(self.calls.len()).saturating_sub(1));
        self.follow_latest = false;
        self.detail_scroll = 0;
    }

    fn select_next(&mut self) {
        if self.calls.is_empty() {
            return;
        }
        let last = self.calls.len() - 1;
        let next = self.selected.unwrap_or(last).saturating_add(1).min(last);
        self.selected = Some(next);
        self.follow_latest = next == last;
        self.detail_scroll = 0;
    }

    fn select_first(&mut self) {
        if !self.calls.is_empty() {
            self.selected = Some(0);
            self.follow_latest = false;
            self.detail_scroll = 0;
        }
    }

    fn select_latest(&mut self) {
        self.selected = self.calls.len().checked_sub(1);
        self.follow_latest = true;
        self.detail_scroll = 0;
    }

    fn trim_history(&mut self) {
        while self.calls.len() > MAX_CALLS {
            let removable = self.calls.iter().position(|call| !call.status.is_active());
            let Some(index) = removable else {
                break;
            };
            self.calls.remove(index);
            self.selected = self.selected.map(|selected| {
                if selected > index {
                    selected - 1
                } else {
                    selected.min(self.calls.len().saturating_sub(1))
                }
            });
        }
    }

    fn selected_call(&self) -> Option<&CallRecord> {
        self.selected.and_then(|index| self.calls.get(index))
    }
}

/// Run the console until the operator quits. Pending approvals are denied on
/// exit so no HTTP request hangs.
pub(crate) async fn run(
    mode: Arc<Mutex<WorkMode>>,
    mut event_rx: mpsc::UnboundedReceiver<ConsoleEvent>,
    addr: SocketAddr,
    allow_yolo: bool,
) -> color_eyre::Result<()> {
    let (_guard, mut terminal) = crate::terminal::TerminalGuard::enter("programmer")?;
    let mut state = ConsoleState::new(mode, addr, allow_yolo);
    let mut events = EventStream::new();

    loop {
        terminal.draw(|frame| render(frame, &mut state))?;

        tokio::select! {
            Some(event) = event_rx.recv() => state.apply(event),
            maybe_event = events.next() => {
                let Some(Ok(Event::Key(key))) = maybe_event else {
                    continue;
                };
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if !state.handle_key(key.code, key.modifiers) {
                    break;
                }
            }
        }
    }

    for req in state.pending {
        let _ = req.respond.send(false);
    }
    Ok(())
}

fn render(frame: &mut ratatui::Frame, state: &mut ConsoleState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(6),
            Constraint::Length(if state.pending.is_empty() { 3 } else { 5 }),
        ])
        .split(frame.area());

    render_header(frame, state, chunks[0]);
    render_calls(frame, state, chunks[1]);
    render_footer(frame, state, chunks[2]);
}

fn render_header(frame: &mut ratatui::Frame, state: &ConsoleState, area: Rect) {
    let mode = *state.mode.lock().unwrap();
    let completed = state
        .calls
        .iter()
        .filter(|call| !call.status.is_active())
        .count();
    let header = Paragraph::new(vec![
        Line::from(vec![
            Span::styled(
                " 🔗 programmer MCP ",
                Style::new().fg(palette::BLUE).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("http://{}/mcp  ", state.addr),
                Style::new().fg(palette::MUTED),
            ),
            Span::styled(
                format!("mode: {} {}", mode.icon(), mode.label()),
                Style::new().fg(palette::GREEN).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                format!(" {}  ", mode_hint(mode)),
                Style::new().fg(palette::MUTED),
            ),
            Span::styled(
                format!(
                    "calls: {}  completed: {}  waiting: {}",
                    state.calls.len(),
                    completed,
                    state.pending.len()
                ),
                Style::new().fg(palette::MUTED),
            ),
        ]),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(palette::BORDER)),
    );
    frame.render_widget(header, area);
}

fn mode_hint(mode: WorkMode) -> &'static str {
    match mode {
        WorkMode::Manual => "asks before state-changing tools",
        WorkMode::Auto => "classifier reviews state-changing tools",
        WorkMode::Plan => "blocks state-changing tools",
        WorkMode::Yolo => "runs every tool unchecked",
    }
}

fn render_calls(frame: &mut ratatui::Frame, state: &mut ConsoleState, area: Rect) {
    let direction = if area.width >= 90 {
        Direction::Horizontal
    } else {
        Direction::Vertical
    };
    let panels = Layout::default()
        .direction(direction)
        .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
        .split(area);

    let items: Vec<ListItem> = state
        .calls
        .iter()
        .map(|call| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!(" {} ", call.status.icon()),
                    Style::new()
                        .fg(call.status.color())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("#{:03} ", call.id), Style::new().fg(palette::MUTED)),
                Span::styled(call.tool.clone(), Style::new().fg(palette::TEXT)),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::new().fg(palette::BORDER))
                .title(" calls "),
        )
        .highlight_style(
            Style::new()
                .fg(palette::TEXT)
                .bg(palette::SURFACE)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("❯");
    let mut list_state = ListState::default();
    list_state.select(state.selected);
    StatefulWidget::render(list, panels[0], frame.buffer_mut(), &mut list_state);

    let detail = if let Some(call) = state.selected_call() {
        let mut lines = vec![
            Line::from(vec![
                Span::styled(
                    format!("{} ", call.tool),
                    Style::new().fg(palette::TEXT).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    call.status.label(),
                    Style::new()
                        .fg(call.status.color())
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                "Arguments",
                Style::new().fg(palette::CYAN).add_modifier(Modifier::BOLD),
            )),
        ];
        lines.extend(text_lines(&pretty_json(&call.args)));
        if !call.detail.is_empty() {
            lines.extend([
                Line::from(""),
                Line::from(Span::styled(
                    if call.status == CallStatus::Denied {
                        "Reason"
                    } else {
                        "Result"
                    },
                    Style::new().fg(palette::CYAN).add_modifier(Modifier::BOLD),
                )),
            ]);
            lines.extend(text_lines(&pretty_json(&call.detail)));
        }
        lines
    } else {
        vec![
            Line::from(""),
            Line::from(Span::styled(
                "  Waiting for tool calls…",
                Style::new().fg(palette::MUTED),
            )),
        ]
    };
    frame.render_widget(
        Paragraph::new(detail)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::new().fg(palette::BORDER))
                    .title(" details "),
            )
            .wrap(Wrap { trim: false })
            .scroll((state.detail_scroll, 0)),
        panels[1],
    );
}

fn render_footer(frame: &mut ratatui::Frame, state: &ConsoleState, area: Rect) {
    let footer = if let Some(req) = state.pending.front() {
        let mut lines = vec![Line::from(Span::styled(
            format!(
                " Approve #{} {}?  ({} waiting)",
                req.id,
                req.tool,
                state.pending.len()
            ),
            Style::new()
                .fg(palette::YELLOW)
                .add_modifier(Modifier::BOLD),
        ))];
        lines.push(Line::from(vec![
            Span::styled(
                " y",
                Style::new().fg(palette::GREEN).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" approve   ", Style::new().fg(palette::MUTED)),
            Span::styled(
                "n / Esc",
                Style::new().fg(palette::RED).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" deny   ", Style::new().fg(palette::MUTED)),
            Span::styled(
                "↑↓",
                Style::new().fg(palette::CYAN).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" inspect calls", Style::new().fg(palette::MUTED)),
        ]));
        lines.push(Line::from(Span::styled(
            " Full arguments are shown in the selected call details.",
            Style::new().fg(palette::MUTED),
        )));
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::new().fg(palette::YELLOW)),
        )
    } else {
        let help = Line::from(vec![
            Span::styled(
                " ↑↓ / jk",
                Style::new().fg(palette::CYAN).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" select   ", Style::new().fg(palette::MUTED)),
            Span::styled(
                "PgUp/PgDn",
                Style::new().fg(palette::CYAN).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" scroll details   ", Style::new().fg(palette::MUTED)),
            Span::styled(
                "Ctrl+T",
                Style::new().fg(palette::CYAN).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" mode   ", Style::new().fg(palette::MUTED)),
            Span::styled(
                "q / Ctrl+C",
                Style::new().fg(palette::CYAN).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" quit", Style::new().fg(palette::MUTED)),
        ]);
        Paragraph::new(help).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::new().fg(palette::BORDER)),
        )
    };
    frame.render_widget(footer, area);
}

fn pretty_json(value: &str) -> String {
    serde_json::from_str::<serde_json::Value>(value)
        .ok()
        .and_then(|value| serde_json::to_string_pretty(&value).ok())
        .unwrap_or_else(|| value.to_string())
}

fn text_lines(value: &str) -> Vec<Line<'static>> {
    value
        .lines()
        .map(|line| {
            Line::from(Span::styled(
                line.to_string(),
                Style::new().fg(palette::MUTED),
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use tokio::sync::oneshot;

    fn state(mode: WorkMode) -> ConsoleState {
        ConsoleState::new(
            Arc::new(Mutex::new(mode)),
            "127.0.0.1:8765".parse().unwrap(),
            false,
        )
    }

    fn start_call(state: &mut ConsoleState, id: u64, tool: &str) {
        state.apply(ConsoleEvent::Started {
            id,
            tool: tool.to_string(),
            args: r#"{"path":"src/main.rs"}"#.to_string(),
        });
    }

    #[test]
    fn tracks_call_lifecycle_and_details() {
        let mut state = state(WorkMode::Manual);
        start_call(&mut state, 7, "read_file");

        assert_eq!(state.selected, Some(0));
        assert_eq!(state.calls[0].status, CallStatus::Received);

        state.apply(ConsoleEvent::Running { id: 7 });
        state.apply(ConsoleEvent::Finished {
            id: 7,
            outcome: CallOutcome::Succeeded,
            detail: "file contents".to_string(),
        });

        assert_eq!(state.calls[0].status, CallStatus::Succeeded);
        assert_eq!(state.calls[0].detail, "file contents");
    }

    #[test]
    fn resolves_approvals_in_queue_order() {
        let mut state = state(WorkMode::Manual);
        start_call(&mut state, 1, "command");
        let (respond, mut decision) = oneshot::channel();
        state.apply(ConsoleEvent::ApprovalRequested(ApprovalRequest {
            id: 1,
            tool: "command".to_string(),
            respond,
        }));

        assert_eq!(state.calls[0].status, CallStatus::AwaitingApproval);
        assert_eq!(state.pending.len(), 1);

        assert!(state.handle_key(KeyCode::Char('y'), KeyModifiers::NONE));
        assert_eq!(decision.try_recv(), Ok(true));
        assert!(state.pending.is_empty());
        assert_eq!(state.calls[0].status, CallStatus::Running);
    }

    #[test]
    fn navigation_stops_and_restores_follow_latest() {
        let mut state = state(WorkMode::Manual);
        start_call(&mut state, 1, "read_file");
        start_call(&mut state, 2, "list_files");

        state.select_previous();
        assert_eq!(state.selected, Some(0));
        assert!(!state.follow_latest);

        start_call(&mut state, 3, "search");
        assert_eq!(state.selected, Some(0));

        state.select_latest();
        start_call(&mut state, 4, "command");
        assert_eq!(state.selected, Some(3));
        assert!(state.follow_latest);
    }

    #[test]
    fn ctrl_t_cycles_shared_mode() {
        let mut state = state(WorkMode::Manual);
        assert!(state.handle_key(KeyCode::Char('t'), KeyModifiers::CONTROL));
        assert_eq!(*state.mode.lock().unwrap(), WorkMode::Auto);
    }

    #[test]
    fn render_shows_server_call_and_full_detail_sections() {
        let mut state = state(WorkMode::Manual);
        start_call(&mut state, 1, "read_file");
        state.apply(ConsoleEvent::Finished {
            id: 1,
            outcome: CallOutcome::Succeeded,
            detail: r#"{"ok":true}"#.to_string(),
        });

        let backend = TestBackend::new(120, 28);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, &mut state)).unwrap();
        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();

        assert!(rendered.contains("http://127.0.0.1:8765/mcp"));
        assert!(rendered.contains("read_file"));
        assert!(rendered.contains("Arguments"));
        assert!(rendered.contains("Result"));
        assert!(rendered.contains("\"path\": \"src/main.rs\""));
        assert!(rendered.contains("\"ok\": true"));
    }
}
