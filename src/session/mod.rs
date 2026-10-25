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
use async_openai::types::responses::{
    FunctionCallOutput, FunctionCallOutputItemParam, InputItem, Item, OutputItem,
};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ModelOverride {
    #[default]
    Inherit,
    Current,
    Model(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AutoCompactOverride {
    #[default]
    Inherit,
    Disabled,
    Tokens(u32),
}

// ---------------------------------------------------------------------------
// Serializable mirror
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "variant", content = "payload")]
pub(crate) enum SerializableMessageItem {
    Input(InputItem),
    Output(OutputItem),
    /// A tool result with its authoritative failure flag.
    ToolOutput {
        output: FunctionCallOutputItemParam,
        failed: bool,
        #[serde(default)]
        approval_label: Option<String>,
    },
    OpenAIError {
        message: String,
    },
    Error(String),
    Warning(String),
    Info(String),
    PeerExchange {
        id: String,
        from: String,
        question: String,
        answer: Option<String>,
    },
    PeerDelegation {
        id: String,
        from: String,
        body: Option<String>,
        state: crate::response::message_item::PeerDelegationState,
    },
    Meta {
        label: String,
        text: String,
    },
    Usage {
        input_tokens: u32,
        output_tokens: u32,
        #[serde(default)]
        cached_input_tokens: u32,
        #[serde(default)]
        recalled_memories: Option<usize>,
    },
    /// A `/compact` boundary carrying the summary of everything before it.
    Compacted {
        summary: String,
    },
}

impl From<MessageItem> for SerializableMessageItem {
    fn from(item: MessageItem) -> Self {
        match item {
            MessageItem::Input(i) => SerializableMessageItem::Input(i),
            MessageItem::Output(o) => SerializableMessageItem::Output(o),
            MessageItem::ToolOutput {
                output,
                failed,
                approval_label,
            } => SerializableMessageItem::ToolOutput {
                output,
                failed,
                approval_label,
            },
            MessageItem::OpenAIError(e) => SerializableMessageItem::OpenAIError {
                message: e.to_string(),
            },
            MessageItem::Error(s) => SerializableMessageItem::Error(s),
            MessageItem::Warning(s) => SerializableMessageItem::Warning(s),
            MessageItem::Info(s) => SerializableMessageItem::Info(s),
            MessageItem::PeerExchange {
                id,
                from,
                question,
                answer,
            } => SerializableMessageItem::PeerExchange {
                id,
                from,
                question,
                answer,
            },
            MessageItem::PeerDelegation {
                id,
                from,
                body,
                state,
            } => SerializableMessageItem::PeerDelegation {
                id,
                from,
                body,
                state,
            },
            MessageItem::Meta { label, text } => SerializableMessageItem::Meta { label, text },
            MessageItem::Usage(i, o, cached, recalled_memories) => SerializableMessageItem::Usage {
                input_tokens: i,
                output_tokens: o,
                cached_input_tokens: cached,
                recalled_memories,
            },
            MessageItem::Compacted { summary } => SerializableMessageItem::Compacted { summary },
        }
    }
}

impl From<SerializableMessageItem> for MessageItem {
    fn from(item: SerializableMessageItem) -> Self {
        match item {
            // Sessions saved before tool outputs had their own variant stored
            // them as `function_call_output` input items. Migrate those to the
            // `ToolOutput` variant so the render path and classifier treat them
            // uniformly; the failure flag is recomputed once from the text (the
            // only signal such an old session carries).
            SerializableMessageItem::Input(InputItem::Item(Item::FunctionCallOutput(output))) => {
                let failed = matches!(
                    &output.output,
                    FunctionCallOutput::Text(t)
                        if t.trim_start().to_ascii_lowercase().starts_with("error:")
                );
                MessageItem::ToolOutput {
                    output,
                    failed,
                    approval_label: None,
                }
            }
            SerializableMessageItem::Input(i) => MessageItem::Input(i),
            SerializableMessageItem::Output(o) => MessageItem::Output(o),
            SerializableMessageItem::ToolOutput {
                output,
                failed,
                approval_label,
            } => MessageItem::ToolOutput {
                output,
                failed,
                approval_label,
            },
            SerializableMessageItem::OpenAIError { message } => {
                MessageItem::Error(format!("(restored) {message}"))
            }
            SerializableMessageItem::Error(s) => MessageItem::Error(s),
            SerializableMessageItem::Warning(s) => MessageItem::Warning(s),
            SerializableMessageItem::Info(s) => MessageItem::Info(s),
            SerializableMessageItem::PeerExchange {
                id,
                from,
                question,
                answer,
            } => MessageItem::PeerExchange {
                id,
                from,
                question,
                answer,
            },
            SerializableMessageItem::PeerDelegation {
                id,
                from,
                body,
                state,
            } => MessageItem::PeerDelegation {
                id,
                from,
                body,
                state,
            },
            SerializableMessageItem::Meta { label, text } => MessageItem::Meta { label, text },
            SerializableMessageItem::Usage {
                input_tokens,
                output_tokens,
                cached_input_tokens,
                recalled_memories,
            } => MessageItem::Usage(
                input_tokens,
                output_tokens,
                cached_input_tokens,
                recalled_memories,
            ),
            SerializableMessageItem::Compacted { summary } => MessageItem::Compacted { summary },
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
    /// Model-generated title. Empty for legacy or not-yet-titled sessions.
    #[serde(default)]
    pub(crate) title: String,
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
pub(crate) struct SessionSnapshot {
    pub(crate) uuid: String,
    #[serde(default)]
    pub(crate) title: String,
    pub(crate) first_message: String,
    pub(crate) created_at: u64,
    pub(crate) updated_at: u64,
    pub(crate) working_dir: String,
    /// Number of messages (derived from items).
    pub(crate) message_count: usize,
    pub(crate) items: Vec<SerializableMessageItem>,
    /// Parsed structure from the newest compact boundary. Older sessions and
    /// unparsable summaries leave this empty and continue using summary text.
    #[serde(default)]
    pub(crate) session_memory: Option<crate::memory::SessionMemory>,
    /// Input history (most recent last).
    #[serde(default)]
    pub(crate) history: Vec<String>,
    /// Latest model-predicted next message, restored as the empty-input hint.
    #[serde(default)]
    pub(crate) input_suggestion: Option<String>,
    /// Work mode in effect when the session was last saved, restored on resume.
    #[serde(default)]
    pub(crate) work_mode: Option<crate::classifier::WorkMode>,
    /// Headless interaction checkpoint; absent in ordinary TUI sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) headless_pending: Option<serde_json::Value>,
    /// Chat model in use when last saved (`provider/model`), restored on resume.
    #[serde(default)]
    pub(crate) current_model: Option<String>,
    /// Provider-reported input tokens for the latest individual model request.
    #[serde(default)]
    pub(crate) last_request_input_tokens: Option<u32>,
    /// Whether image attachments are sent to the model in this session.
    #[serde(default)]
    pub(crate) vision_enabled: bool,
    /// Reasoning effort for main chat and compaction requests.
    #[serde(default)]
    pub(crate) thinking_level: crate::thinking::ThinkingLevel,
    /// Session-only overrides. Slash commands update these values; provider
    /// management updates their global defaults instead.
    #[serde(default)]
    pub(crate) classifier_model_override: ModelOverride,
    #[serde(default)]
    pub(crate) compact_model_override: ModelOverride,
    #[serde(default)]
    pub(crate) auto_compact_override: AutoCompactOverride,
    #[serde(default)]
    pub(crate) compact_keep_recent_turns_override: Option<usize>,
    /// Todo list carried with the session, restored on resume.
    #[serde(default)]
    pub(crate) todos: Vec<crate::todos::Todo>,
    /// Names of activated skills, restored on resume.
    #[serde(default)]
    pub(crate) activated_skills: Vec<String>,
    /// Whether `activated_skills` was saved after skills became enabled by
    /// default. This distinguishes a deliberate empty selection from legacy
    /// sessions whose implicit default was empty.
    #[serde(default)]
    pub(crate) skill_selection_saved: bool,
    /// Background tasks carried with the session. Tasks recorded as running
    /// are restored as killed — their processes died with the old instance.
    #[serde(default)]
    pub(crate) tasks: Vec<crate::tasks::PersistedTask>,
    /// Sub-agent records carried with the session. Running agents are restored
    /// as cancelled because their processes do not survive a fork or restart.
    #[serde(default)]
    pub(crate) agents: Vec<crate::agents::PersistedAgent>,
    /// File content fingerprints read in this session, used by file protection after resume.
    #[serde(default)]
    pub(crate) file_snapshots: Vec<crate::security::policy::PersistedFileSnapshot>,
}

/// An OS-backed exclusive lock held for the lifetime of an open session.
pub(crate) struct SessionLock {
    file: std::fs::File,
}

impl Drop for SessionLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

/// Informational metadata; the OS lock remains authoritative. Metadata may be
/// stale after release, and legacy lock files contain only a PID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionLockOwner {
    pub(crate) pid: u32,
    pub(crate) process_type: Option<String>,
}

impl SessionLockOwner {
    fn parse(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        Some(Self {
            pid: lines.next()?.trim().parse().ok()?,
            process_type: lines.next().map(str::to_owned),
        })
    }
}

#[derive(Debug)]
pub(crate) enum SessionLockError {
    InUse { pid: Option<u32> },
    Io(String),
}

/// Loading is read-only: missing files are distinct from unreadable or invalid data.
#[derive(Debug)]
pub(crate) enum SessionLoadError {
    Read {
        path: PathBuf,
        error: std::io::Error,
    },
    Parse {
        path: PathBuf,
        error: serde_json::Error,
    },
}

impl std::fmt::Display for SessionLoadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read { path, error } => write!(formatter, "read {}: {error}", path.display()),
            Self::Parse { path, error } => write!(formatter, "parse {}: {error}", path.display()),
        }
    }
}

pub(crate) struct SessionManager {
    sessions_dir: PathBuf,
}

impl SessionManager {
    #[cfg(test)]
    pub(crate) fn for_test(sessions_dir: PathBuf) -> Self {
        Self { sessions_dir }
    }

    /// Create a new manager pointing at `~/.config/programmer/sessions/`.
    pub(crate) fn new() -> Option<Self> {
        // Match load_config's debug-only integration-test isolation hook.
        #[cfg(debug_assertions)]
        if let Some(dir) = std::env::var_os("PROGRAMMER_TEST_CONFIG_DIR") {
            return Some(Self {
                sessions_dir: PathBuf::from(dir).join("sessions"),
            });
        }
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
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            // Quick-read: only parse the metadata fields, not the items.
            if let Ok(bytes) = std::fs::read(&path)
                && let Ok(s) = serde_json::from_slice::<SessionMeta>(&bytes)
            {
                sessions.push(s);
            }
        }
        sessions.sort_by_key(|s| std::cmp::Reverse(s.updated_at));
        Ok(sessions)
    }

    /// Create a brand-new session with a random UUID.
    pub(crate) fn create(&self) -> SessionSnapshot {
        let uuid = uuid_v4();
        let now = now_secs();
        let working_dir =
            std::env::current_dir().map_or_else(|_| ".".to_string(), |p| p.display().to_string());
        SessionSnapshot {
            uuid,
            title: String::new(),
            first_message: String::new(),
            created_at: now,
            updated_at: now,
            working_dir,
            message_count: 0,
            items: Vec::new(),
            session_memory: None,
            history: Vec::new(),
            input_suggestion: None,
            work_mode: None,
            headless_pending: None,
            current_model: None,
            last_request_input_tokens: None,
            vision_enabled: false,
            thinking_level: crate::thinking::ThinkingLevel::default(),
            classifier_model_override: ModelOverride::Inherit,
            compact_model_override: ModelOverride::Inherit,
            auto_compact_override: AutoCompactOverride::Inherit,
            compact_keep_recent_turns_override: None,
            todos: Vec::new(),
            activated_skills: Vec::new(),
            skill_selection_saved: false,
            tasks: Vec::new(),
            agents: Vec::new(),
            file_snapshots: Vec::new(),
        }
    }

    /// A missing file is `Ok(None)`; all other failures preserve the original file.
    pub(crate) fn load(&self, uuid: &str) -> Result<Option<SessionSnapshot>, SessionLoadError> {
        let path = self.session_path(uuid);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(SessionLoadError::Read { path, error }),
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| SessionLoadError::Parse { path, error })
    }

    /// Save a session to its file atomically: write to a temp file first,
    /// then rename, so a crash mid-write never leaves a truncated file.
    pub(crate) fn save(&self, session: &mut SessionSnapshot) -> Result<(), String> {
        self.ensure_dir()?;
        let mut snapshot = session.clone();
        snapshot.updated_at = now_secs();
        let path = self.session_path(&snapshot.uuid);
        let json =
            serde_json::to_string_pretty(&snapshot).map_err(|e| format!("serialize: {e}"))?;
        // Atomic write: tmp → rename.
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &json).map_err(|e| format!("write: {e}"))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("rename: {e}"))?;
        session.updated_at = snapshot.updated_at;
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

    /// Try to exclusively lock a session. The lock file contains the owning PID
    /// for a useful conflict message; the OS lock, not that PID, is authoritative.
    pub(crate) fn try_lock(&self, uuid: &str) -> Result<SessionLock, SessionLockError> {
        self.try_lock_as(uuid, "tui")
    }

    /// Read advisory owner metadata (possibly stale or unavailable on Windows).
    pub(crate) fn lock_owner(&self, uuid: &str) -> Option<SessionLockOwner> {
        let text = std::fs::read_to_string(self.sessions_dir.join(format!("{uuid}.lock"))).ok()?;
        SessionLockOwner::parse(&text)
    }

    /// Lock with a surface label while preserving the PID-only conflict API.
    pub(crate) fn try_lock_as(
        &self,
        uuid: &str,
        process_type: &str,
    ) -> Result<SessionLock, SessionLockError> {
        if process_type.is_empty() || process_type.contains(['\r', '\n']) {
            return Err(SessionLockError::Io("invalid lock process type".into()));
        }
        self.ensure_dir().map_err(SessionLockError::Io)?;
        let path = self.sessions_dir.join(format!("{uuid}.lock"));
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|error| SessionLockError::Io(format!("open {}: {error}", path.display())))?;
        if let Err(error) = file.try_lock_exclusive() {
            let mut owner = String::new();
            let _ = file.rewind();
            let _ = file.read_to_string(&mut owner);
            // Rust does not consistently map Windows ERROR_LOCK_VIOLATION
            // (33) to WouldBlock, but it still means another handle owns the
            // requested range and should be reported as a session conflict.
            let contended = error.kind() == std::io::ErrorKind::WouldBlock
                || cfg!(windows) && error.raw_os_error() == Some(33);
            if contended {
                return Err(SessionLockError::InUse {
                    pid: SessionLockOwner::parse(&owner).map(|owner| owner.pid),
                });
            }
            return Err(SessionLockError::Io(format!(
                "lock {}: {error}",
                path.display()
            )));
        }
        file.set_len(0)
            .and_then(|()| file.rewind())
            .and_then(|()| writeln!(file, "{}\n{process_type}", std::process::id()))
            .and_then(|()| file.sync_data())
            .map_err(|error| SessionLockError::Io(format!("write {}: {error}", path.display())))?;
        Ok(SessionLock { file })
    }

    /// Clone a loaded conversation into a separately persisted session.
    pub(crate) fn fork(&self, source: &SessionSnapshot) -> Result<SessionSnapshot, String> {
        let mut forked = source.clone();
        forked.uuid = uuid_v4();
        let now = now_secs();
        forked.created_at = now;
        forked.updated_at = now;
        crate::checkpoint::CheckpointStore::fork_session(&source.uuid, &forked.uuid)?;
        if !forked.title.is_empty() {
            forked.title.push_str(" (fork)");
        }
        self.save(&mut forked)?;
        Ok(forked)
    }

    /// Convert session items into MessageItems.
    pub(crate) fn into_items(session: SessionSnapshot) -> Vec<MessageItem> {
        session.items.into_iter().map(MessageItem::from).collect()
    }

    /// Replace session items from MessageItems. Also updates message_count.
    pub(crate) fn set_items(session: &mut SessionSnapshot, items: Vec<MessageItem>) {
        session.message_count = items.len();
        session.items = items
            .into_iter()
            .map(SerializableMessageItem::from)
            .collect();
    }

    /// Path to a specific session file.
    pub(crate) fn session_path(&self, uuid: &str) -> PathBuf {
        self.sessions_dir.join(format!("{uuid}.json"))
    }
}

// ---------------------------------------------------------------------------
// Interactive session picker (TUI)
// ---------------------------------------------------------------------------

pub(crate) fn sessions_for_working_dir(
    sessions: &[SessionMeta],
    working_dir: &Path,
) -> Vec<SessionMeta> {
    let working_dir = working_dir
        .canonicalize()
        .unwrap_or_else(|_| working_dir.to_path_buf());
    sessions
        .iter()
        .filter(|session| {
            let session_dir = Path::new(&session.working_dir);
            session_dir
                .canonicalize()
                .unwrap_or_else(|_| session_dir.to_path_buf())
                == working_dir
        })
        .cloned()
        .collect()
}

const PICKER_QUIT_CONFIRM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
const PICKER_DOUBLE_CLICK_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);

fn picker_quit_is_confirmed(previous: Option<std::time::Instant>, now: std::time::Instant) -> bool {
    previous.is_some_and(|pressed_at| now.duration_since(pressed_at) <= PICKER_QUIT_CONFIRM_TIMEOUT)
}

fn picker_session_double_click(
    previous: &mut Option<(String, std::time::Instant)>,
    uuid: &str,
    now: std::time::Instant,
) -> bool {
    let double_click = previous.as_ref().is_some_and(|(previous_uuid, clicked_at)| {
        previous_uuid == uuid && now.duration_since(*clicked_at) <= PICKER_DOUBLE_CLICK_TIMEOUT
    });
    if double_click {
        *previous = None;
    } else {
        *previous = Some((uuid.to_string(), now));
    }
    double_click
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PickerPointerTarget {
    Session(usize),
    Filter,
    Resume,
    NewSession,
    Delete,
    Quit,
    ConfirmDelete,
    CancelDelete,
}

#[derive(Default)]
struct PickerPointerState {
    hovered: Option<PickerPointerTarget>,
    pressed: Option<PickerPointerTarget>,
    animation: crate::ui::interaction::InteractionAnimation,
}

impl PickerPointerState {
    fn move_to(&mut self, target: Option<PickerPointerTarget>) {
        if self.hovered == target {
            return;
        }
        self.hovered = target;
        if self.hovered.is_some() {
            self.animation.restart();
        } else {
            self.animation.cancel();
        }
    }

    fn press(&mut self, target: Option<PickerPointerTarget>) {
        self.pressed = target;
    }

    fn release(&mut self, target: Option<PickerPointerTarget>) -> Option<PickerPointerTarget> {
        let pressed = self.pressed.take();
        if pressed.as_ref() == target.as_ref() {
            pressed
        } else {
            None
        }
    }

    fn clear(&mut self) {
        self.hovered = None;
        self.pressed = None;
        self.animation.cancel();
    }
}

#[derive(Clone, Copy)]
struct PickerAreas {
    list: ratatui::layout::Rect,
    filter: ratatui::layout::Rect,
    filter_footer: ratatui::layout::Rect,
    resume: ratatui::layout::Rect,
    new_session: ratatui::layout::Rect,
    delete: ratatui::layout::Rect,
    quit: ratatui::layout::Rect,
    confirm_delete: ratatui::layout::Rect,
    cancel_delete: ratatui::layout::Rect,
}

fn picker_areas(area: ratatui::layout::Rect) -> PickerAreas {
    use ratatui::layout::{Constraint, Direction, Layout};

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(3),
            Constraint::Length(2),
        ])
        .split(area);
    let title = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(24), Constraint::Length(24)])
        .split(chunks[0]);
    let footer = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(13),
            Constraint::Length(13),
            Constraint::Length(6),
            Constraint::Length(9),
            Constraint::Length(14),
            Constraint::Length(11),
            Constraint::Min(0),
        ])
        .split(chunks[2]);
    let confirmation = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(30),
            Constraint::Length(6),
            Constraint::Length(9),
            Constraint::Min(0),
        ])
        .split(chunks[2]);

    let first_line = |area: ratatui::layout::Rect| {
        ratatui::layout::Rect::new(area.x, area.y, area.width, area.height.min(1))
    };
    let title_filter = ratatui::layout::Rect::new(
        title[1].right().saturating_sub(18),
        title[1].y,
        title[1].width.min(18),
        title[1].height.min(1),
    );

    PickerAreas {
        list: chunks[1],
        filter: title_filter,
        filter_footer: first_line(footer[4]),
        resume: first_line(footer[1]),
        new_session: first_line(footer[2]),
        delete: first_line(footer[3]),
        quit: first_line(footer[5]),
        confirm_delete: first_line(confirmation[1]),
        cancel_delete: first_line(confirmation[2]),
    }
}

fn picker_rect_contains(area: ratatui::layout::Rect, column: u16, row: u16) -> bool {
    column >= area.x && column < area.right() && row >= area.y && row < area.bottom()
}

fn picker_session_at(
    area: ratatui::layout::Rect,
    offset: usize,
    session_count: usize,
    column: u16,
    row: u16,
) -> Option<usize> {
    let inner = ratatui::widgets::Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .inner(area);
    if !picker_rect_contains(inner, column, row) {
        return None;
    }
    let index = offset + usize::from(row - inner.y) / 2;
    (index < session_count).then_some(index)
}

fn picker_pointer_target(
    areas: PickerAreas,
    offset: usize,
    session_count: usize,
    confirmation: bool,
    quit_warning: bool,
    column: u16,
    row: u16,
) -> Option<PickerPointerTarget> {
    if quit_warning {
        return None;
    }
    if confirmation {
        return if picker_rect_contains(areas.confirm_delete, column, row) {
            Some(PickerPointerTarget::ConfirmDelete)
        } else if picker_rect_contains(areas.cancel_delete, column, row) {
            Some(PickerPointerTarget::CancelDelete)
        } else {
            None
        };
    }
    if picker_rect_contains(areas.filter, column, row)
        || picker_rect_contains(areas.filter_footer, column, row)
    {
        return Some(PickerPointerTarget::Filter);
    }
    if let Some(index) = picker_session_at(areas.list, offset, session_count, column, row) {
        return Some(PickerPointerTarget::Session(index));
    }
    [
        (areas.resume, PickerPointerTarget::Resume),
        (areas.new_session, PickerPointerTarget::NewSession),
        (areas.delete, PickerPointerTarget::Delete),
        (areas.quit, PickerPointerTarget::Quit),
    ]
    .into_iter()
    .find_map(|(area, target)| picker_rect_contains(area, column, row).then_some(target))
}

fn picker_scroll_selection(
    selected: Option<usize>,
    session_count: usize,
    scroll_up: bool,
) -> Option<usize> {
    if session_count == 0 {
        return None;
    }
    let selected = selected.unwrap_or(0).min(session_count - 1);
    Some(if scroll_up {
        selected.saturating_sub(1)
    } else {
        (selected + 1).min(session_count - 1)
    })
}

fn picker_interaction_style(
    pointer: &PickerPointerState,
    target: &PickerPointerTarget,
) -> ratatui::style::Style {
    use crate::ui::theme::role;
    use ratatui::style::Style;

    if pointer.pressed.as_ref() == Some(target) {
        Style::default().bg(role::INTERACTION_PRESSED_BG)
    } else if pointer.hovered.as_ref() == Some(target) {
        Style::default().bg(pointer.animation.hover_background())
    } else {
        Style::default()
    }
}

/// Show a ratatui TUI list of sessions and let the user pick one with arrow keys or the mouse.
/// Runs its own terminal setup/teardown; the main app will re-init the terminal.
/// Returns the chosen UUID, or `None` if the user chose "new session".
/// On quit (q / Esc) the process exits after cleanup.
/// Press `d` on a session to delete it after confirmation.
pub(crate) fn pick_session(sessions: &[SessionMeta], mgr: &SessionManager) -> Option<String> {
    use crate::ui::markdown_theme::palette;
    use crate::ui::theme::role;
    use crossterm::event::{
        self, Event as CEvent, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
    };
    use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
    use crossterm::execute;
    use crossterm::terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    };
    use ratatui::backend::CrosstermBackend;
    use ratatui::layout::{Constraint, Direction, Layout};
    use ratatui::style::{Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
    use ratatui::{Frame, Terminal};
    use std::io;

    enum Outcome {
        Selected(String),
        NewSession,
        Quit,
    }

    struct PickerTerminalGuard;

    impl Drop for PickerTerminalGuard {
        fn drop(&mut self) {
            let _ = execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
            let _ = disable_raw_mode();
        }
    }

    if sessions.is_empty() {
        return None;
    }

    let outcome = std::panic::catch_unwind(|| {
        let mut stdout = io::stdout();
        let _ = enable_raw_mode();
        let _ = execute!(stdout, EnterAlternateScreen, EnableMouseCapture);
        let _terminal_guard = PickerTerminalGuard;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend).expect("terminal init");

        let mut all_sessions = sessions.to_vec();
        let working_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let mut filter_working_dir = true;
        let mut sessions = sessions_for_working_dir(&all_sessions, &working_dir);
        let mut list_state = ListState::default();
        if !sessions.is_empty() {
            list_state.select(Some(0));
        }

        let mut confirm_uuid: Option<String> = None;
        let mut quit_requested_at: Option<std::time::Instant> = None;
        let mut last_session_click: Option<(String, std::time::Instant)> = None;
        let mut pointer = PickerPointerState::default();
        let mut rendered_area = ratatui::layout::Rect::default();

        loop {
            let _ = terminal.draw(|f: &mut Frame| {
                let area = f.area();
                rendered_area = area;
                let areas = picker_areas(area);
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(2),
                        Constraint::Min(3),
                        Constraint::Length(2),
                    ])
                    .split(area);
                let title_chunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Min(24), Constraint::Length(24)])
                    .split(chunks[0]);

                let title = Paragraph::new(Line::from(vec![
                    Span::styled(
                        "📂  Saved sessions",
                        Style::default().fg(palette::CYAN).bold(),
                    ),
                    Span::styled(
                        format!(
                            "  ({} of {}, newest first)",
                            sessions.len(),
                            all_sessions.len()
                        ),
                        Style::default().fg(palette::MUTED).italic(),
                    ),
                ]));
                f.render_widget(title, title_chunks[0]);
                let filter_label = if filter_working_dir {
                    "f current dir: ON"
                } else {
                    "f current dir: OFF"
                };
                let filter = Paragraph::new(Span::styled(
                    filter_label,
                    Style::default()
                        .fg(if filter_working_dir {
                            palette::GREEN
                        } else {
                            palette::MUTED
                        })
                        .bold()
                        .patch(picker_interaction_style(
                            &pointer,
                            &PickerPointerTarget::Filter,
                        )),
                ))
                .alignment(ratatui::layout::Alignment::Right);
                f.render_widget(filter, title_chunks[1]);

                let selected_target = list_state.selected().map(PickerPointerTarget::Session);
                let highlight_background = if pointer.pressed.as_ref() == selected_target.as_ref() {
                    role::INTERACTION_PRESSED_BG
                } else if pointer.hovered.as_ref() == selected_target.as_ref() {
                    pointer.animation.hover_background()
                } else {
                    role::SELECTION_BG
                };
                let highlight_style = Style::default()
                    .fg(role::SELECTION_TEXT)
                    .bg(highlight_background)
                    .add_modifier(Modifier::BOLD);

                let items: Vec<ListItem> = sessions
                    .iter()
                    .enumerate()
                    .map(|(index, s)| {
                        let preview = if !s.title.is_empty() {
                            truncate_first_line(&s.title, 60)
                        } else if !s.first_message.is_empty() {
                            truncate_first_line(&s.first_message, 60)
                        } else {
                            "(empty)".to_string()
                        };
                        let time_str = unix_to_local(s.updated_at);
                        let short_uuid = &s.uuid[..s.uuid.floor_char_boundary(8)];
                        let target = PickerPointerTarget::Session(index);

                        ListItem::new(vec![
                            Line::from(vec![Span::styled(
                                preview,
                                Style::default().fg(palette::TEXT),
                            )]),
                            Line::from(vec![
                                Span::styled(
                                    format!("  {} messages", s.message_count),
                                    Style::default().fg(palette::MUTED),
                                ),
                                Span::styled(
                                    format!(" · {}", time_str),
                                    Style::default().fg(palette::FAINT),
                                ),
                                Span::styled(
                                    format!(" · {}", short_uuid),
                                    Style::default().fg(palette::FAINT),
                                ),
                            ]),
                        ])
                        .style(picker_interaction_style(&pointer, &target))
                    })
                    .collect();

                let list = List::new(items)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(role::BORDER)),
                    )
                    .highlight_style(highlight_style)
                    .highlight_symbol("❯ ");
                f.render_stateful_widget(list, chunks[1], &mut list_state);

                if quit_requested_at.is_some() {
                    let warning = Span::styled(
                        " Press Ctrl+C again within 2 seconds to exit.",
                        Style::default().fg(palette::YELLOW).bold(),
                    );
                    f.render_widget(Paragraph::new(warning), chunks[2]);
                } else if let Some(ref uuid) = confirm_uuid {
                    let short = &uuid[..uuid.floor_char_boundary(8)];
                    let confirmation = Layout::default()
                        .direction(Direction::Horizontal)
                        .constraints([
                            Constraint::Length(30),
                            Constraint::Length(6),
                            Constraint::Length(9),
                            Constraint::Min(0),
                        ])
                        .split(chunks[2]);
                    f.render_widget(
                        Paragraph::new(Span::styled(
                            format!(" Delete session {}?", short),
                            Style::default().fg(palette::YELLOW).bold(),
                        )),
                        confirmation[0],
                    );
                    f.render_widget(
                        Paragraph::new(Span::styled(
                            " y yes",
                            Style::default().fg(palette::RED).bold().patch(
                                picker_interaction_style(
                                    &pointer,
                                    &PickerPointerTarget::ConfirmDelete,
                                ),
                            ),
                        )),
                        areas.confirm_delete,
                    );
                    f.render_widget(
                        Paragraph::new(Span::styled(
                            " n cancel",
                            Style::default().fg(palette::GREEN).bold().patch(
                                picker_interaction_style(
                                    &pointer,
                                    &PickerPointerTarget::CancelDelete,
                                ),
                            ),
                        )),
                        areas.cancel_delete,
                    );
                } else {
                    let footer = Layout::default()
                        .direction(Direction::Horizontal)
                        .constraints([
                            Constraint::Length(13),
                            Constraint::Length(13),
                            Constraint::Length(6),
                            Constraint::Length(9),
                            Constraint::Length(14),
                            Constraint::Length(11),
                            Constraint::Min(0),
                        ])
                        .split(chunks[2]);
                    f.render_widget(
                        Paragraph::new(Span::styled(
                            " ↑↓/wheel",
                            Style::default().fg(palette::MUTED),
                        )),
                        footer[0],
                    );
                    for (area, label, target, color) in [
                        (
                            areas.resume,
                            " Enter resume",
                            PickerPointerTarget::Resume,
                            palette::CYAN,
                        ),
                        (
                            areas.new_session,
                            " n new",
                            PickerPointerTarget::NewSession,
                            palette::CYAN,
                        ),
                        (
                            areas.delete,
                            " d delete",
                            PickerPointerTarget::Delete,
                            palette::RED,
                        ),
                        (
                            areas.quit,
                            " q/Esc quit",
                            PickerPointerTarget::Quit,
                            palette::CYAN,
                        ),
                    ] {
                        if label.is_empty() {
                            continue;
                        }
                        f.render_widget(
                            Paragraph::new(Span::styled(
                                label,
                                Style::default()
                                    .fg(color)
                                    .bold()
                                    .patch(picker_interaction_style(&pointer, &target)),
                            )),
                            area,
                        );
                    }
                    f.render_widget(
                        Paragraph::new(Span::styled(
                            " f current dir",
                            Style::default().fg(palette::CYAN).bold().patch(
                                picker_interaction_style(&pointer, &PickerPointerTarget::Filter),
                            ),
                        )),
                        areas.filter_footer,
                    );
                }

                crate::ui::theme::apply(
                    crate::ui::theme::Theme::Auto.effective(),
                    area,
                    f.buffer_mut(),
                );
            });

            let next_event = if pointer.animation.active() {
                match event::poll(std::time::Duration::from_millis(16)) {
                    Ok(true) => event::read().ok(),
                    Ok(false) => continue,
                    Err(_) => None,
                }
            } else {
                event::read().ok()
            };
            let Some(next_event) = next_event else {
                continue;
            };

            let mut input = match next_event {
                CEvent::Key(key) if key.kind == KeyEventKind::Press => {
                    last_session_click = None;
                    Some((key.code, key.modifiers))
                }
                CEvent::Mouse(mouse) => {
                    let areas = picker_areas(rendered_area);
                    let target = picker_pointer_target(
                        areas,
                        list_state.offset(),
                        sessions.len(),
                        confirm_uuid.is_some(),
                        quit_requested_at.is_some(),
                        mouse.column,
                        mouse.row,
                    );
                    pointer.move_to(target.clone());
                    match mouse.kind {
                        MouseEventKind::Moved => None,
                        MouseEventKind::Down(MouseButton::Left) => {
                            pointer.press(target);
                            None
                        }
                        MouseEventKind::Up(MouseButton::Left) => {
                            let released = pointer.release(target);
                            if !matches!(&released, Some(PickerPointerTarget::Session(_))) {
                                last_session_click = None;
                            }
                            match released {
                                Some(PickerPointerTarget::Session(index)) => {
                                    list_state.select(Some(index));
                                    sessions.get(index).and_then(|session| {
                                        picker_session_double_click(
                                            &mut last_session_click,
                                            &session.uuid,
                                            std::time::Instant::now(),
                                        )
                                        .then_some((KeyCode::Enter, KeyModifiers::NONE))
                                    })
                                }
                                Some(PickerPointerTarget::Filter) => {
                                    Some((KeyCode::Char('f'), KeyModifiers::NONE))
                                }
                                Some(PickerPointerTarget::Resume) => {
                                    Some((KeyCode::Enter, KeyModifiers::NONE))
                                }
                                Some(PickerPointerTarget::NewSession) => {
                                    Some((KeyCode::Char('n'), KeyModifiers::NONE))
                                }
                                Some(PickerPointerTarget::Delete) => {
                                    Some((KeyCode::Char('d'), KeyModifiers::NONE))
                                }
                                Some(PickerPointerTarget::Quit) => {
                                    Some((KeyCode::Char('q'), KeyModifiers::NONE))
                                }
                                Some(PickerPointerTarget::ConfirmDelete) => {
                                    Some((KeyCode::Char('y'), KeyModifiers::NONE))
                                }
                                Some(PickerPointerTarget::CancelDelete) => {
                                    Some((KeyCode::Char('n'), KeyModifiers::NONE))
                                }
                                None => None,
                            }
                        }
                        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                            if confirm_uuid.is_none() && quit_requested_at.is_none() =>
                        {
                            last_session_click = None;
                            list_state.select(picker_scroll_selection(
                                list_state.selected(),
                                sessions.len(),
                                mouse.kind == MouseEventKind::ScrollUp,
                            ));
                            None
                        }
                        _ => None,
                    }
                }
                _ => None,
            };
            let Some((code, modifiers)) = input.take() else {
                continue;
            };

            let now = std::time::Instant::now();
            if code == KeyCode::Char('c') && modifiers.contains(KeyModifiers::CONTROL) {
                if picker_quit_is_confirmed(quit_requested_at, now) {
                    break Outcome::Quit;
                }
                quit_requested_at = Some(now);
                confirm_uuid = None;
                pointer.clear();
                continue;
            }
            if quit_requested_at.is_some_and(|pressed_at| {
                now.duration_since(pressed_at) > PICKER_QUIT_CONFIRM_TIMEOUT
            }) {
                quit_requested_at = None;
            }

            if let Some(ref uuid) = confirm_uuid {
                match code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => {
                        let _ = mgr.delete(uuid);
                        match mgr.list_all() {
                            Ok(fresh) => {
                                all_sessions = fresh;
                                sessions = if filter_working_dir {
                                    sessions_for_working_dir(&all_sessions, &working_dir)
                                } else {
                                    all_sessions.clone()
                                };
                                list_state.select((!sessions.is_empty()).then_some(0));
                            }
                            Err(_) => {
                                all_sessions.clear();
                                sessions.clear();
                                list_state.select(None);
                            }
                        }
                        confirm_uuid = None;
                        pointer.clear();
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc | KeyCode::Char('d') => {
                        confirm_uuid = None;
                        pointer.clear();
                    }
                    _ => {}
                }
                continue;
            }

            match code {
                KeyCode::Char('q') | KeyCode::Esc => break Outcome::Quit,
                KeyCode::Char('n') => break Outcome::NewSession,
                KeyCode::Char('f') | KeyCode::Char('F') => {
                    filter_working_dir = !filter_working_dir;
                    sessions = if filter_working_dir {
                        sessions_for_working_dir(&all_sessions, &working_dir)
                    } else {
                        all_sessions.clone()
                    };
                    list_state.select((!sessions.is_empty()).then_some(0));
                    pointer.clear();
                }
                KeyCode::Char('d') => {
                    if let Some(i) = list_state.selected()
                        && let Some(s) = sessions.get(i)
                    {
                        confirm_uuid = Some(s.uuid.clone());
                        pointer.clear();
                    }
                }
                KeyCode::Enter => {
                    if let Some(i) = list_state.selected()
                        && let Some(s) = sessions.get(i)
                    {
                        break Outcome::Selected(s.uuid.clone());
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    list_state.select(picker_scroll_selection(
                        list_state.selected(),
                        sessions.len(),
                        true,
                    ));
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    list_state.select(picker_scroll_selection(
                        list_state.selected(),
                        sessions.len(),
                        false,
                    ));
                }
                _ => {}
            }
        }
    });

    match outcome {
        Ok(Outcome::Selected(uuid)) => Some(uuid),
        Ok(Outcome::NewSession) => None,
        Ok(Outcome::Quit) => {
            std::process::exit(0);
        }
        Err(_) => None,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn uuid_v4() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
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
        format!(
            "{}…",
            &first_line[..first_line.floor_char_boundary(max_len)]
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persistence_load_distinguishes_missing_read_and_parse_without_mutation() {
        let sessions_dir = std::env::temp_dir().join(format!("programmer-load-test-{}", uuid_v4()));
        let manager = SessionManager {
            sessions_dir: sessions_dir.clone(),
        };
        assert!(manager.load("missing").unwrap().is_none());
        assert!(!sessions_dir.exists());
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let unreadable = manager.session_path("directory");
        std::fs::create_dir(&unreadable).unwrap();
        assert!(matches!(
            manager.load("directory"),
            Err(SessionLoadError::Read { .. })
        ));
        let invalid = manager.session_path("invalid");
        std::fs::write(&invalid, b"{invalid JSON").unwrap();
        let previous_quarantine = invalid.with_extension("json.corrupt");
        std::fs::write(&previous_quarantine, b"older recovery data").unwrap();
        for _ in 0..2 {
            assert!(matches!(
                manager.load("invalid"),
                Err(SessionLoadError::Parse { .. })
            ));
            assert_eq!(std::fs::read(&invalid).unwrap(), b"{invalid JSON");
            assert_eq!(
                std::fs::read(&previous_quarantine).unwrap(),
                b"older recovery data"
            );
        }
        std::fs::remove_dir_all(sessions_dir).unwrap();
    }

    #[test]
    fn persistence_write_failure_preserves_saved_bytes_and_can_retry() {
        let sessions_dir = std::env::temp_dir().join(format!("programmer-save-test-{}", uuid_v4()));
        let manager = SessionManager {
            sessions_dir: sessions_dir.clone(),
        };
        let mut session = manager.create();
        manager.save(&mut session).unwrap();
        let path = manager.session_path(&session.uuid);
        let original = std::fs::read(&path).unwrap();
        session.updated_at = 1;
        session.title = "retry me".to_string();
        let temporary = path.with_extension("tmp");
        std::fs::create_dir(&temporary).unwrap();
        assert!(manager.save(&mut session).is_err());
        assert_eq!(session.updated_at, 1);
        assert_eq!(std::fs::read(&path).unwrap(), original);
        std::fs::remove_dir(&temporary).unwrap();
        manager.save(&mut session).unwrap();
        assert_eq!(
            manager.load(&session.uuid).unwrap().unwrap().title,
            "retry me"
        );
        std::fs::remove_dir_all(sessions_dir).unwrap();
    }

    #[test]
    fn persistence_rename_failure_does_not_acknowledge_snapshot() {
        let sessions_dir =
            std::env::temp_dir().join(format!("programmer-rename-test-{}", uuid_v4()));
        let manager = SessionManager {
            sessions_dir: sessions_dir.clone(),
        };
        let mut session = manager.create();
        session.updated_at = 1;
        let path = manager.session_path(&session.uuid);
        std::fs::create_dir_all(&path).unwrap();
        assert!(
            manager
                .save(&mut session)
                .unwrap_err()
                .starts_with("rename:")
        );
        assert_eq!(session.updated_at, 1);
        assert!(path.is_dir());
        std::fs::remove_dir(&path).unwrap();
        manager.save(&mut session).unwrap();
        assert!(manager.load(&session.uuid).unwrap().is_some());
        std::fs::remove_dir_all(sessions_dir).unwrap();
    }

    #[test]
    fn peer_delegation_round_trips_all_states_and_missing_task() {
        use crate::response::message_item::PeerDelegationState::*;
        for state in [Pending, AcceptedQueued, Started, Rejected, Cancelled] {
            for body in [None, Some("任务\nfull task".to_owned())] {
                let item = MessageItem::PeerDelegation {
                    id: "stable-id".into(),
                    from: "source".into(),
                    body: body.clone(),
                    state,
                };
                let json =
                    serde_json::to_string(&SerializableMessageItem::from(item.clone())).unwrap();
                let saved: SerializableMessageItem = serde_json::from_str(&json).unwrap();
                let restored = MessageItem::from(saved);
                assert!(
                    matches!(restored, MessageItem::PeerDelegation { id, from, body: saved, state: actual }
                    if id == "stable-id" && from == "source" && saved == body && actual == state)
                );
            }
        }
    }

    #[test]
    fn peer_exchange_round_trips_pending_and_answered() {
        for answer in [None, Some("full answer\nsecond line".to_string())] {
            let original = MessageItem::PeerExchange {
                id: "exchange-id".into(),
                from: "full-source-session".into(),
                question: "full question\nsecond line".into(),
                answer: answer.clone(),
            };
            let json =
                serde_json::to_string(&SerializableMessageItem::from(original.clone())).unwrap();
            let stored: SerializableMessageItem = serde_json::from_str(&json).unwrap();
            let MessageItem::PeerExchange {
                id,
                from,
                question,
                answer: restored,
            } = MessageItem::from(stored)
            else {
                panic!("exchange variant lost");
            };
            assert_eq!(id, "exchange-id");
            assert_eq!(from, "full-source-session");
            assert_eq!(question, "full question\nsecond line");
            assert_eq!(restored, answer);
        }
    }

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

    #[test]
    fn working_directory_filter_keeps_only_matching_sessions() {
        let current = std::env::temp_dir().join("programmer-project-a");
        let sessions = vec![
            SessionMeta {
                uuid: "matching".to_string(),
                title: String::new(),
                first_message: String::new(),
                created_at: 0,
                updated_at: 2,
                working_dir: current.display().to_string(),
                message_count: 0,
            },
            SessionMeta {
                uuid: "other".to_string(),
                title: String::new(),
                first_message: String::new(),
                created_at: 0,
                updated_at: 1,
                working_dir: std::env::temp_dir()
                    .join("programmer-project-b")
                    .display()
                    .to_string(),
                message_count: 0,
            },
        ];

        let filtered = sessions_for_working_dir(&sessions, &current);

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].uuid, "matching");
    }

    #[test]
    fn picker_requires_two_ctrl_c_presses_within_timeout() {
        let now = std::time::Instant::now();
        assert!(!picker_quit_is_confirmed(None, now));
        assert!(picker_quit_is_confirmed(
            Some(now - std::time::Duration::from_secs(1)),
            now
        ));
        assert!(!picker_quit_is_confirmed(
            Some(now - std::time::Duration::from_secs(3)),
            now
        ));
    }

    #[test]
    fn picker_double_click_requires_the_same_session_within_timeout() {
        let now = std::time::Instant::now();
        let mut previous = None;

        assert!(!picker_session_double_click(&mut previous, "first", now));
        assert!(!picker_session_double_click(
            &mut previous,
            "second",
            now + std::time::Duration::from_millis(100),
        ));
        assert!(picker_session_double_click(
            &mut previous,
            "second",
            now + std::time::Duration::from_millis(200),
        ));
        assert!(previous.is_none());

        assert!(!picker_session_double_click(
            &mut previous,
            "first",
            now + std::time::Duration::from_secs(1),
        ));
        assert!(!picker_session_double_click(
            &mut previous,
            "first",
            now + std::time::Duration::from_secs(2),
        ));
    }

    #[test]
    fn picker_hit_tests_both_lines_of_visible_session_rows() {
        let areas = picker_areas(ratatui::layout::Rect::new(0, 0, 100, 20));
        let inner = ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .inner(areas.list);

        assert_eq!(
            picker_pointer_target(areas, 2, 8, false, false, inner.x + 1, inner.y),
            Some(PickerPointerTarget::Session(2))
        );
        assert_eq!(
            picker_pointer_target(areas, 2, 8, false, false, inner.x + 1, inner.y + 1),
            Some(PickerPointerTarget::Session(2))
        );
        assert_eq!(
            picker_pointer_target(areas, 2, 8, false, false, inner.x + 1, inner.y + 2),
            Some(PickerPointerTarget::Session(3))
        );
        assert_eq!(
            picker_pointer_target(areas, 2, 3, false, false, inner.x + 1, inner.y + 2),
            None
        );
    }

    #[test]
    fn picker_only_activates_a_press_released_on_the_same_target() {
        let first = PickerPointerTarget::Session(0);
        let second = PickerPointerTarget::Session(1);
        let mut pointer = PickerPointerState::default();

        pointer.press(Some(first.clone()));
        assert_eq!(pointer.release(Some(second)), None);
        assert_eq!(pointer.pressed, None);

        pointer.press(Some(first.clone()));
        assert_eq!(pointer.release(Some(first.clone())), Some(first));
        assert_eq!(pointer.pressed, None);
        assert_eq!(pointer.release(Some(PickerPointerTarget::Session(0))), None);
    }

    #[test]
    fn picker_wheel_navigation_is_bounded_and_handles_empty_lists() {
        assert_eq!(picker_scroll_selection(None, 0, false), None);
        assert_eq!(picker_scroll_selection(Some(0), 3, true), Some(0));
        assert_eq!(picker_scroll_selection(Some(0), 3, false), Some(1));
        assert_eq!(picker_scroll_selection(Some(2), 3, false), Some(2));
        assert_eq!(picker_scroll_selection(Some(99), 3, true), Some(1));
    }

    #[test]
    fn picker_confirmation_exposes_only_safe_confirmation_controls() {
        let areas = picker_areas(ratatui::layout::Rect::new(0, 0, 100, 20));
        assert_eq!(
            picker_pointer_target(
                areas,
                0,
                3,
                true,
                false,
                areas.confirm_delete.x,
                areas.confirm_delete.y,
            ),
            Some(PickerPointerTarget::ConfirmDelete)
        );
        assert_eq!(
            picker_pointer_target(
                areas,
                0,
                3,
                true,
                false,
                areas.cancel_delete.x,
                areas.cancel_delete.y,
            ),
            Some(PickerPointerTarget::CancelDelete)
        );
        let list_inner = ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .inner(areas.list);
        assert_eq!(
            picker_pointer_target(areas, 0, 3, true, false, list_inner.x, list_inner.y),
            None
        );
    }

    #[test]
    fn working_directory_filter_preserves_newest_first_order() {
        let current = std::env::temp_dir().join("programmer-project");
        let sessions = ["newest", "older"]
            .into_iter()
            .map(|uuid| SessionMeta {
                uuid: uuid.to_string(),
                title: String::new(),
                first_message: String::new(),
                created_at: 0,
                updated_at: 0,
                working_dir: current.display().to_string(),
                message_count: 0,
            })
            .collect::<Vec<_>>();

        let filtered = sessions_for_working_dir(&sessions, &current);

        assert_eq!(
            filtered
                .iter()
                .map(|session| session.uuid.as_str())
                .collect::<Vec<_>>(),
            vec!["newest", "older"]
        );
    }

    #[test]
    fn snapshot_preserves_legacy_json_shape() {
        let legacy = serde_json::json!({
            "uuid": "legacy-session",
            "first_message": "hello",
            "created_at": 1,
            "updated_at": 2,
            "working_dir": "/legacy/workspace",
            "message_count": 0,
            "items": []
        });
        let snapshot: SessionSnapshot = serde_json::from_value(legacy.clone()).unwrap();
        let serialized = serde_json::to_value(&snapshot).unwrap();
        for (key, value) in legacy.as_object().unwrap() {
            assert_eq!(&serialized[key], value, "legacy field {key} changed");
        }
        assert!(serialized.get("SessionSnapshot").is_none());
        assert!(serialized.get("Session").is_none());
        let restored: SessionSnapshot = serde_json::from_value(serialized.clone()).unwrap();
        assert_eq!(serde_json::to_value(restored).unwrap(), serialized);
    }

    #[test]
    fn older_sessions_default_to_auto_thinking() {
        let sessions_dir =
            std::env::temp_dir().join(format!("programmer-session-test-{}", uuid_v4()));
        let mgr = SessionManager { sessions_dir };
        let mut value = serde_json::to_value(mgr.create()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .remove("thinking_level")
            .unwrap();

        let loaded: SessionSnapshot = serde_json::from_value(value).unwrap();
        assert_eq!(loaded.thinking_level, crate::thinking::ThinkingLevel::Auto);
    }

    #[test]
    fn older_usage_items_default_cached_tokens_to_zero() {
        let item: SerializableMessageItem = serde_json::from_value(serde_json::json!({
            "variant": "Usage",
            "payload": {
                "input_tokens": 10,
                "output_tokens": 2
            }
        }))
        .unwrap();

        assert!(matches!(
            MessageItem::from(item),
            MessageItem::Usage(10, 2, 0, None)
        ));
    }

    #[test]
    fn older_sessions_load_without_structured_memory() {
        let sessions_dir =
            std::env::temp_dir().join(format!("programmer-session-test-{}", uuid_v4()));
        let mgr = SessionManager { sessions_dir };
        let mut value = serde_json::to_value(mgr.create()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .remove("session_memory")
            .unwrap();

        let loaded: SessionSnapshot = serde_json::from_value(value).unwrap();
        assert!(loaded.session_memory.is_none());
    }

    #[test]
    fn older_sessions_load_without_an_input_suggestion() {
        let sessions_dir =
            std::env::temp_dir().join(format!("programmer-session-test-{}", uuid_v4()));
        let mgr = SessionManager { sessions_dir };
        let mut value = serde_json::to_value(mgr.create()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .remove("input_suggestion")
            .unwrap();

        let loaded: SessionSnapshot = serde_json::from_value(value).unwrap();
        assert!(loaded.input_suggestion.is_none());
    }

    #[test]
    fn legacy_sessions_have_no_explicit_skill_selection() {
        let sessions_dir =
            std::env::temp_dir().join(format!("programmer-session-test-{}", uuid_v4()));
        let mgr = SessionManager { sessions_dir };
        let mut value = serde_json::to_value(mgr.create()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .remove("skill_selection_saved")
            .unwrap();

        let loaded: SessionSnapshot = serde_json::from_value(value).unwrap();
        assert!(!loaded.skill_selection_saved);
    }

    #[test]
    fn session_settings_and_todos_round_trip_independently() {
        let sessions_dir =
            std::env::temp_dir().join(format!("programmer-session-test-{}", uuid_v4()));
        let mgr = SessionManager {
            sessions_dir: sessions_dir.clone(),
        };

        let mut first = mgr.create();
        first.vision_enabled = true;
        first.thinking_level = crate::thinking::ThinkingLevel::High;
        first.input_suggestion = Some("continue first session".to_string());
        let mut first_todos = crate::todos::TodoList::default();
        first_todos.add("first session only".to_string(), None);
        first.todos = first_todos.todos;

        let mut second = mgr.create();
        second.vision_enabled = false;
        second.thinking_level = crate::thinking::ThinkingLevel::None;
        let mut second_todos = crate::todos::TodoList::default();
        second_todos.add("second session only".to_string(), None);
        second.todos = second_todos.todos;

        mgr.save(&mut first).unwrap();
        mgr.save(&mut second).unwrap();

        let loaded_first = mgr.load(&first.uuid).unwrap().unwrap();
        let loaded_second = mgr.load(&second.uuid).unwrap().unwrap();
        assert!(loaded_first.vision_enabled);
        assert!(!loaded_second.vision_enabled);
        assert_eq!(
            loaded_first.thinking_level,
            crate::thinking::ThinkingLevel::High
        );
        assert_eq!(
            loaded_second.thinking_level,
            crate::thinking::ThinkingLevel::None
        );
        assert_eq!(loaded_first.todos[0].title, "first session only");
        assert_eq!(loaded_second.todos[0].title, "second session only");
        assert_eq!(
            loaded_first.input_suggestion.as_deref(),
            Some("continue first session")
        );
        assert!(loaded_second.input_suggestion.is_none());

        std::fs::remove_dir_all(sessions_dir).unwrap();
    }

    #[test]
    fn session_lock_reports_the_owning_process_and_releases_on_drop() {
        let sessions_dir =
            std::env::temp_dir().join(format!("programmer-session-lock-test-{}", uuid_v4()));
        let mgr = SessionManager {
            sessions_dir: sessions_dir.clone(),
        };
        let session = mgr.create();
        let lock = mgr.try_lock(&session.uuid).unwrap();
        let second_lock = mgr.try_lock(&session.uuid);
        #[cfg(not(windows))]
        assert!(matches!(
            second_lock,
            Err(SessionLockError::InUse { pid: Some(pid) }) if pid == std::process::id()
        ));
        // Windows range locks prevent the competing handle from reading the PID
        // stored in the locked file. The OS lock remains authoritative, so the
        // conflict must still be reported even when owner metadata is unavailable.
        #[cfg(windows)]
        assert!(matches!(second_lock, Err(SessionLockError::InUse { .. })));
        drop(lock);
        let reacquired = mgr.try_lock(&session.uuid).unwrap();
        drop(reacquired);
        std::fs::remove_dir_all(sessions_dir).unwrap();
    }

    #[test]
    fn lock_owner_supports_legacy_and_surface_labels() {
        assert_eq!(SessionLockOwner::parse("123\n").unwrap().process_type, None);
        let owner = SessionLockOwner::parse("123\nheadless\n").unwrap();
        assert_eq!(owner.pid, 123);
        assert_eq!(owner.process_type.as_deref(), Some("headless"));
        assert!(SessionLockOwner::parse("invalid").is_none());
    }

    #[test]
    fn fork_copies_session_content_under_a_new_uuid() {
        let sessions_dir =
            std::env::temp_dir().join(format!("programmer-session-fork-test-{}", uuid_v4()));
        let mgr = SessionManager {
            sessions_dir: sessions_dir.clone(),
        };
        let mut source = mgr.create();
        source.title = "Original".to_string();
        source.history.push("previous command".to_string());
        let forked = mgr.fork(&source).unwrap();
        assert_ne!(forked.uuid, source.uuid);
        assert_eq!(forked.title, "Original (fork)");
        assert_eq!(forked.history, source.history);
        assert!(mgr.load(&forked.uuid).unwrap().is_some());
        std::fs::remove_dir_all(sessions_dir).unwrap();
    }
}
