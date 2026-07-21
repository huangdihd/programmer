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

//! The `--mcp-http` approval console: a small ratatui screen that shows the
//! current work mode and a live log of tool calls, prompts the operator to
//! approve `manual`-mode calls, and cycles the mode on Ctrl+T.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use tokio::sync::mpsc;

use super::http_server::{ApprovalRequest, LogEntry, LogKind};
use crate::classifier::WorkMode;
use crate::ui::markdown_theme::palette;

/// Maximum log lines kept in the console.
const MAX_LOG: usize = 500;

/// Run the console until the operator quits. Pending approvals are denied on
/// exit so no HTTP request hangs.
pub(crate) async fn run(
    mode: Arc<Mutex<WorkMode>>,
    mut log_rx: mpsc::UnboundedReceiver<LogEntry>,
    mut approval_rx: mpsc::UnboundedReceiver<ApprovalRequest>,
    addr: SocketAddr,
    allow_yolo: bool,
) -> color_eyre::Result<()> {
    let (_guard, mut terminal) = crate::terminal::TerminalGuard::enter("programmer")?;
    let mut logs: VecDeque<LogEntry> = VecDeque::new();
    let mut pending: VecDeque<ApprovalRequest> = VecDeque::new();
    let mut events = EventStream::new();

    loop {
        let current = *mode.lock().unwrap();
        terminal.draw(|frame| {
            render(frame, current, addr, &logs, pending.front());
        })?;

        tokio::select! {
            Some(entry) = log_rx.recv() => {
                logs.push_back(entry);
                while logs.len() > MAX_LOG {
                    logs.pop_front();
                }
            }
            Some(req) = approval_rx.recv() => pending.push_back(req),
            maybe_event = events.next() => {
                let Some(Ok(Event::Key(key))) = maybe_event else {
                    continue;
                };
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                match key.code {
                    KeyCode::Char('c') if ctrl => break,
                    KeyCode::Char('q') if pending.is_empty() => break,
                    KeyCode::Char('t') if ctrl => {
                        let mut m = mode.lock().unwrap();
                        *m = m.next(allow_yolo);
                    }
                    // Resolve the front approval.
                    KeyCode::Char('y') | KeyCode::Char('Y') => {
                        if let Some(req) = pending.pop_front() {
                            let _ = req.respond.send(true);
                            logs.push_back(LogEntry {
                                kind: LogKind::Allowed,
                                text: format!("approved {}", req.tool),
                            });
                        }
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                        if let Some(req) = pending.pop_front() {
                            let _ = req.respond.send(false);
                            logs.push_back(LogEntry {
                                kind: LogKind::Denied,
                                text: format!("denied {}", req.tool),
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    // Deny anything still waiting so the HTTP side doesn't hang.
    for req in pending {
        let _ = req.respond.send(false);
    }
    Ok(())
}

fn render(
    frame: &mut ratatui::Frame,
    mode: WorkMode,
    addr: SocketAddr,
    logs: &VecDeque<LogEntry>,
    pending: Option<&ApprovalRequest>,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Min(3),    // log
            Constraint::Length(4), // approval / help
        ])
        .split(frame.area());

    // Header: title, endpoint, mode.
    let header = Line::from(vec![
        Span::styled(
            " \u{1F517} programmer MCP server ",
            Style::new().fg(palette::BLUE).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("http://{addr}/mcp   "), Style::new().fg(palette::MUTED)),
        Span::styled(
            format!("mode: {} {}", mode.icon(), mode.label()),
            Style::new().fg(palette::GREEN).add_modifier(Modifier::BOLD),
        ),
    ]);
    frame.render_widget(Paragraph::new(header), chunks[0]);

    // Log (newest at the bottom).
    let visible = chunks[1].height.saturating_sub(2) as usize;
    let items: Vec<ListItem> = logs
        .iter()
        .rev()
        .take(visible)
        .rev()
        .map(|e| {
            let color = match e.kind {
                LogKind::Info => palette::MUTED,
                LogKind::Allowed => palette::GREEN,
                LogKind::Denied => palette::RED,
            };
            ListItem::new(Line::from(Span::styled(
                format!("  {}", e.text),
                Style::new().fg(color),
            )))
        })
        .collect();
    frame.render_widget(
        List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::new().fg(palette::BORDER))
                .title(" tool calls "),
        ),
        chunks[1],
    );

    // Footer: pending approval prompt, or the help line.
    let footer = if let Some(req) = pending {
        let mut lines = vec![Line::from(Span::styled(
            format!(" Approve  {}  ?", req.tool),
            Style::new().fg(palette::YELLOW).add_modifier(Modifier::BOLD),
        ))];
        let preview: String = req.args.chars().take(120).collect();
        lines.push(Line::from(Span::styled(
            format!("   {preview}"),
            Style::new().fg(palette::MUTED),
        )));
        lines.push(Line::from(vec![
            Span::styled("   y", Style::new().fg(palette::GREEN).add_modifier(Modifier::BOLD)),
            Span::styled(" approve   ", Style::new().fg(palette::MUTED)),
            Span::styled("n / Esc", Style::new().fg(palette::RED).add_modifier(Modifier::BOLD)),
            Span::styled(" deny", Style::new().fg(palette::MUTED)),
        ]));
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::new().fg(palette::YELLOW)),
        )
    } else {
        let help = Line::from(vec![
            Span::styled(" Ctrl+T", Style::new().fg(palette::CYAN).add_modifier(Modifier::BOLD)),
            Span::styled(" cycle mode   ", Style::new().fg(palette::MUTED)),
            Span::styled("q / Ctrl+C", Style::new().fg(palette::CYAN).add_modifier(Modifier::BOLD)),
            Span::styled(" quit", Style::new().fg(palette::MUTED)),
        ]);
        Paragraph::new(help).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::new().fg(palette::BORDER)),
        )
    };
    frame.render_widget(footer, chunks[2]);
}
