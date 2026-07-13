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

//! Multi-session management with UUIDs.
//!
//! Each session is stored as `~/.config/programmer/sessions/<uuid>.json`.
//! Use `--resume [uuid]` on the command line to restore a session, or
//! `--resume` (no argument) to pick interactively from saved sessions.

use crate::response::message_item::MessageItem;
use async_openai::types::responses::{InputItem, OutputItem};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Serializable mirror
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "variant", content = "payload")]
pub(crate) enum SerializableMessageItem {
    Input(InputItem),
    Output(OutputItem),
    OpenAIError { message: String },
    Error(String),
    Warning(String),
    Info(String),
    Meta { label: String, text: String },
    Usage { input_tokens: u32, output_tokens: u32 },
}

impl From<MessageItem> for SerializableMessageItem {
    fn from(item: MessageItem) -> Self {
        match item {
            MessageItem::Input(i) => SerializableMessageItem::Input(i),
            MessageItem::Output(o) => SerializableMessageItem::Output(o),
            MessageItem::OpenAIError(e) => SerializableMessageItem::OpenAIError {
                message: e.to_string(),
            },
            MessageItem::Error(s) => SerializableMessageItem::Error(s),
            MessageItem::Warning(s) => SerializableMessageItem::Warning(s),
            MessageItem::Info(s) => SerializableMessageItem::Info(s),
            MessageItem::Meta { label, text } => SerializableMessageItem::Meta { label, text },
            MessageItem::Usage(i, o) => SerializableMessageItem::Usage {
                input_tokens: i,
                output_tokens: o,
            },
        }
    }
}

impl From<SerializableMessageItem> for MessageItem {
    fn from(item: SerializableMessageItem) -> Self {
        match item {
            SerializableMessageItem::Input(i) => MessageItem::Input(i),
            SerializableMessageItem::Output(o) => MessageItem::Output(o),
            SerializableMessageItem::OpenAIError { message } => {
                MessageItem::Error(format!("(restored) {message}"))
            }
            SerializableMessageItem::Error(s) => MessageItem::Error(s),
            SerializableMessageItem::Warning(s) => MessageItem::Warning(s),
            SerializableMessageItem::Info(s) => MessageItem::Info(s),
            SerializableMessageItem::Meta { label, text } => MessageItem::Meta { label, text },
            SerializableMessageItem::Usage {
                input_tokens,
                output_tokens,
            } => MessageItem::Usage(input_tokens, output_tokens),
        }
    }
}

// ---------------------------------------------------------------------------
// Session manager
// ---------------------------------------------------------------------------

/// Lightweight summary of a saved session (for listing / the picker).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SessionMeta {
    pub(crate) uuid: String,
    /// Truncated first user message for preview.
    pub(crate) first_message: String,
    pub(crate) created_at: u64,
    pub(crate) updated_at: u64,
    pub(crate) working_dir: String,
    /// Number of messages in the session.
    pub(crate) message_count: usize,
}

/// Full session stored on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Session {
    pub(crate) uuid: String,
    pub(crate) first_message: String,
    pub(crate) created_at: u64,
    pub(crate) updated_at: u64,
    pub(crate) working_dir: String,
    /// Number of messages (derived from items).
    pub(crate) message_count: usize,
    pub(crate) items: Vec<SerializableMessageItem>,
    /// Input history (most recent last).
    #[serde(default)]
    pub(crate) history: Vec<String>,
    /// Work mode in effect when the session was last saved, restored on resume.
    #[serde(default)]
    pub(crate) work_mode: Option<crate::classifier::WorkMode>,
    /// Chat model in use when last saved (`provider/model`), restored on resume.
    #[serde(default)]
    pub(crate) current_model: Option<String>,
    /// Auto-mode classifier model when last saved, restored on resume.
    #[serde(default)]
    pub(crate) classifier_model: Option<String>,
    /// Todo list carried with the session, restored on resume.
    #[serde(default)]
    pub(crate) todos: Vec<crate::todos::Todo>,
    /// Names of activated skills, restored on resume.
    #[serde(default)]
    pub(crate) activated_skills: Vec<String>,
}

pub(crate) struct SessionManager {
    sessions_dir: PathBuf,
}

impl SessionManager {
    /// Create a new manager pointing at `~/.config/programmer/sessions/`.
    pub(crate) fn new() -> Option<Self> {
        let dir = dirs::config_dir()?.join("programmer").join("sessions");
        Some(SessionManager { sessions_dir: dir })
    }

    /// Ensure the sessions directory exists.
    fn ensure_dir(&self) -> Result<(), String> {
        std::fs::create_dir_all(&self.sessions_dir)
            .map_err(|e| format!("cannot create sessions dir: {e}"))
    }

    /// List all saved sessions, newest-first.
    pub(crate) fn list_all(&self) -> Result<Vec<SessionMeta>, String> {
        self.ensure_dir()?;
        let mut sessions: Vec<SessionMeta> = Vec::new();
        let entries =
            std::fs::read_dir(&self.sessions_dir).map_err(|e| format!("read dir: {e}"))?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map_or(true, |e| e != "json") {
                continue;
            }
            // Quick-read: only parse the metadata fields, not the items.
            if let Ok(bytes) = std::fs::read(&path) {
                if let Ok(s) = serde_json::from_slice::<SessionMeta>(&bytes) {
                    sessions.push(s);
                }
            }
        }
        sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(sessions)
    }

    /// Create a brand-new session with a random UUID.
    pub(crate) fn create(&self) -> Session {
        let uuid = uuid_v4();
        let now = now_secs();
        let working_dir = std::env::current_dir()
            .map_or_else(|_| ".".to_string(), |p| p.display().to_string());
        Session {
            uuid,
            first_message: String::new(),
            created_at: now,
            updated_at: now,
            working_dir,
            message_count: 0,
            items: Vec::new(),
            history: Vec::new(),
            work_mode: None,
            current_model: None,
            classifier_model: None,
            todos: Vec::new(),
            activated_skills: Vec::new(),
        }
    }

    /// Load a session by UUID. Returns `None` if not found or unreadable.
    pub(crate) fn load(&self, uuid: &str) -> Option<Session> {
        let path = self.session_path(uuid);
        let bytes = std::fs::read(&path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Save a session to its file.
    pub(crate) fn save(&self, session: &mut Session) -> Result<(), String> {
        session.updated_at = now_secs();
        self.ensure_dir()?;
        let path = self.session_path(&session.uuid);
        let json = serde_json::to_string_pretty(session)
            .map_err(|e| format!("serialize: {e}"))?;
        std::fs::write(&path, &json).map_err(|e| format!("write: {e}"))?;
        Ok(())
    }

    /// Delete a session file.
    pub(crate) fn delete(&self, uuid: &str) -> Result<(), String> {
        let path = self.session_path(uuid);
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| format!("delete: {e}"))?;
        }
        Ok(())
    }

    /// Convert session items into MessageItems.
    pub(crate) fn into_items(session: Session) -> Vec<MessageItem> {
        session.items.into_iter().map(MessageItem::from).collect()
    }

    /// Replace session items from MessageItems. Also updates message_count.
    pub(crate) fn set_items(session: &mut Session, items: Vec<MessageItem>) {
        session.message_count = items.len();
        session.items = items.into_iter().map(SerializableMessageItem::from).collect();
    }

    /// Path to a specific session file.
    fn session_path(&self, uuid: &str) -> PathBuf {
        self.sessions_dir.join(format!("{uuid}.json"))
    }

    /// The sessions directory.
    pub(crate) fn dir(&self) -> &Path {
        &self.sessions_dir
    }
}

// ---------------------------------------------------------------------------
// Interactive session picker (TUI)
// ---------------------------------------------------------------------------

/// Show a ratatui TUI list of sessions and let the user pick one with arrow keys.
/// Runs its own terminal setup/teardown; the main app will re-init the terminal.
/// Returns the chosen UUID, or `None` if the user chose "new session".
/// On quit (q / Esc) the process exits after cleanup.
/// Press `d` on a session to delete it after confirmation.
pub(crate) fn pick_session(
    sessions: &[SessionMeta],
    mgr: &SessionManager,
) -> Option<String> {
    use crossterm::event::{self, Event as CEvent, KeyCode, KeyEventKind};
    use crossterm::execute;
    use crossterm::terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    };
    use ratatui::backend::CrosstermBackend;
    use ratatui::layout::{Constraint, Layout};
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
    use ratatui::{Frame, Terminal};
    use std::io;

    // ---- outcome types for the picker loop ----
    enum Outcome {
        Selected(String),
        NewSession,
        Quit,
    }

    if sessions.is_empty() {
        return None;
    }

    // ---- run the TUI ----
    let outcome = std::panic::catch_unwind(|| {
        let mut stdout = io::stdout();
        let _ = enable_raw_mode();
        let _ = execute!(stdout, EnterAlternateScreen);
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend).expect("terminal init");

        let mut sessions = sessions.to_vec();
        let mut list_state = ListState::default();
        if !sessions.is_empty() {
            list_state.select(Some(0));
        }

        // Confirmation state: when set, shows a confirmation overlay.
        let mut confirm_uuid: Option<String> = None;

        let result = loop {
            let _ = terminal.draw(|f: &mut Frame| {
                let area = f.area();
                let chunks = Layout::default()
                    .direction(ratatui::layout::Direction::Vertical)
                    .constraints([
                        Constraint::Length(2),
                        Constraint::Min(3),
                        Constraint::Length(2),
                    ])
                    .split(area);

                // -- Title --
                let title = Paragraph::new(Line::from(vec![
                    Span::styled(
                        "📂  Saved sessions",
                        Style::default().fg(Color::Cyan).bold(),
                    ),
                    Span::styled(
                        format!("  ({} total, newest first)", sessions.len()),
                        Style::default().fg(Color::Gray).italic(),
                    ),
                ]));
                f.render_widget(title, chunks[0]);

                // -- Session list --
                let highlight_style = Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD);

                let items: Vec<ListItem> = sessions
                    .iter()
                    .map(|s| {
                        let preview = if s.first_message.is_empty() {
                            "(empty)".to_string()
                        } else {
                            truncate_first_line(&s.first_message, 60)
                        };
                        let time_str = unix_to_local(s.updated_at);
                        let short_uuid = &s.uuid[..8.min(s.uuid.len())];

                        ListItem::new(vec![
                            Line::from(vec![Span::styled(
                                preview,
                                Style::default().fg(Color::White),
                            )]),
                            Line::from(vec![
                                Span::styled(
                                    format!("  {} messages", s.message_count),
                                    Style::default().fg(Color::Gray),
                                ),
                                Span::styled(
                                    format!(" · {}", time_str),
                                    Style::default().fg(Color::DarkGray),
                                ),
                                Span::styled(
                                    format!(" · {}", short_uuid),
                                    Style::default().fg(Color::DarkGray),
                                ),
                            ]),
                        ])
                    })
                    .collect();

                let list = List::new(items)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(Color::DarkGray)),
                    )
                    .highlight_style(highlight_style)
                    .highlight_symbol("❯ ");

                f.render_stateful_widget(list, chunks[1], &mut list_state);

                // -- Help bar (or confirmation prompt) --
                if let Some(ref uuid) = confirm_uuid {
                    let short = &uuid[..8.min(uuid.len())];
                    let confirm = Line::from(vec![
                        Span::styled(
                            format!(" Delete session {}?  ", short),
                            Style::default().fg(Color::Yellow).bold(),
                        ),
                        Span::styled("y", Style::default().fg(Color::Green).bold()),
                        Span::styled(" yes  ", Style::default().fg(Color::Gray)),
                        Span::styled("n", Style::default().fg(Color::Red).bold()),
                        Span::styled(" cancel", Style::default().fg(Color::Gray)),
                    ]);
                    f.render_widget(Paragraph::new(confirm), chunks[2]);
                } else {
                    let help = Line::from(vec![
                        Span::styled(" ↑↓", Style::default().fg(Color::Cyan).bold()),
                        Span::styled(" navigate  ", Style::default().fg(Color::Gray)),
                        Span::styled("Enter", Style::default().fg(Color::Cyan).bold()),
                        Span::styled(" select  ", Style::default().fg(Color::Gray)),
                        Span::styled("n", Style::default().fg(Color::Cyan).bold()),
                        Span::styled(" new  ", Style::default().fg(Color::Gray)),
                        Span::styled("d", Style::default().fg(Color::Red).bold()),
                        Span::styled(" delete  ", Style::default().fg(Color::Gray)),
                        Span::styled("q/Esc", Style::default().fg(Color::Cyan).bold()),
                        Span::styled(" quit", Style::default().fg(Color::Gray)),
                    ]);
                    f.render_widget(Paragraph::new(help), chunks[2]);
                }
            });

            let Ok(CEvent::Key(key)) = event::read() else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }

            // ---- confirmation mode ----
            if let Some(ref uuid) = confirm_uuid {
                match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => {
                        // Delete and refresh the list.
                        let _ = mgr.delete(uuid);
                        match mgr.list_all() {
                            Ok(fresh) => {
                                sessions = fresh;
                                if sessions.is_empty() {
                                    break Outcome::NewSession;
                                }
                                list_state.select(Some(0));
                            }
                            Err(_) => {
                                sessions.clear();
                                break Outcome::NewSession;
                            }
                        }
                        confirm_uuid = None;
                    }
                    KeyCode::Char('n')
                    | KeyCode::Char('N')
                    | KeyCode::Esc
                    | KeyCode::Char('d') => {
                        confirm_uuid = None;
                    }
                    _ => {}
                }
                continue;
            }

            // ---- normal mode ----
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => break Outcome::Quit,
                KeyCode::Char('n') => break Outcome::NewSession,
                KeyCode::Char('d') => {
                    if let Some(i) = list_state.selected() {
                        if let Some(s) = sessions.get(i) {
                            confirm_uuid = Some(s.uuid.clone());
                        }
                    }
                }
                KeyCode::Enter => {
                    if let Some(i) = list_state.selected() {
                        if let Some(s) = sessions.get(i) {
                            break Outcome::Selected(s.uuid.clone());
                        }
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    let i = list_state.selected().unwrap_or(0);
                    if i > 0 {
                        list_state.select(Some(i - 1));
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let i = list_state.selected().unwrap_or(0);
                    if i + 1 < sessions.len() {
                        list_state.select(Some(i + 1));
                    }
                }
                _ => {}
            }
        };

        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        result
    });

    match outcome {
        Ok(Outcome::Selected(uuid)) => Some(uuid),
        Ok(Outcome::NewSession) => None,
        Ok(Outcome::Quit) => {
            std::process::exit(0);
        }
        Err(_) => {
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn uuid_v4() -> String {
    let mut buf = [0u8; 16];
    // Read from /dev/urandom (Unix/macOS). Fall back to time-based bytes on
    // other platforms — good enough for a session identifier.
    #[cfg(unix)]
    {
        if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
            use std::io::Read;
            let _ = f.read_exact(&mut buf);
        }
    }
    // If we're on non-unix or /dev/urandom failed, mix in the current time.
    if buf.iter().all(|b| *b == 0) {
        let nanos = now_secs_nanos();
        for (i, b) in buf.iter_mut().enumerate() {
            *b = ((nanos >> (i * 8)) & 0xff) as u8;
        }
    }
    // Set version (4) and variant bits.
    buf[6] = (buf[6] & 0x0f) | 0x40;
    buf[8] = (buf[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        buf[0], buf[1], buf[2], buf[3],
        buf[4], buf[5],
        buf[6], buf[7],
        buf[8], buf[9],
        buf[10], buf[11], buf[12], buf[13], buf[14], buf[15],
    )
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn now_secs_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn unix_to_local(secs: u64) -> String {
    let now = now_secs();
    let diff = now.saturating_sub(secs);
    if diff < 60 {
        "just now".to_string()
    } else if diff < 3600 {
        format!("{}m ago", diff / 60)
    } else if diff < 86400 {
        format!("{}h ago", diff / 3600)
    } else if diff < 604800 {
        format!("{}d ago", diff / 86400)
    } else {
        format!("{}d ago", diff / 86400)
    }
}

/// Truncate to the first line, capped at `max_len` characters.
pub(crate) fn truncate_first_line(s: &str, max_len: usize) -> String {
    let first_line = s.lines().next().unwrap_or("");
    if first_line.len() <= max_len {
        first_line.to_string()
    } else {
        format!("{}…", &first_line[..first_line.floor_char_boundary(max_len)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_short() {
        assert_eq!(truncate_first_line("hello", 80), "hello");
    }

    #[test]
    fn truncate_long() {
        let s = "a".repeat(100);
        let got = truncate_first_line(&s, 80);
        assert_eq!(got.len(), 83); // 80 ASCII chars + 3-byte '…'
        assert!(got.ends_with('…'));
    }

    #[test]
    fn truncate_multiline() {
        assert_eq!(truncate_first_line("hello\nworld", 80), "hello");
    }
}
