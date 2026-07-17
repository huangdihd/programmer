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

//! Background task system: shell commands running detached from the
//! conversation turn.
//!
//! Tasks are created through the `task` tool, live for the duration of the
//! process (children are killed on exit via `kill_on_drop`), and are shown in
//! the sidebar. State lives in a process-global registry because tools only
//! receive their JSON arguments — there is no `App` handle in the tool path.

use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};

/// Scrollback lines vt100 retains behind the visible screen.
const PTY_SCROLLBACK: usize = 1000;

/// Cap on the output buffer kept per task. When exceeded, the oldest half is
/// dropped so the tail (usually the interesting part) is always available.
const MAX_TASK_OUTPUT: usize = 200_000;

/// Cap on the output persisted per task in the session file.
const MAX_PERSISTED_OUTPUT: usize = 10_000;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Lifecycle state of one background task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Running,
    /// Exited with code 0.
    Completed,
    /// Exited non-zero or failed to run.
    Failed,
    /// Terminated through the `kill` action.
    Killed,
}

impl TaskStatus {
    pub fn label(&self) -> &'static str {
        match self {
            TaskStatus::Running => "running",
            TaskStatus::Completed => "completed",
            TaskStatus::Failed => "failed",
            TaskStatus::Killed => "killed",
        }
    }
}

/// Interactive-task state: a child running in a real PTY, its output parsed
/// into a vt100 screen grid. Present only for tasks created interactively;
/// pipe-based tasks leave this `None` and use `kill`/`stdin_tx` instead.
struct PtyState {
    /// Writes bytes (keystrokes, mouse sequences) to the child.
    writer: Box<dyn Write + Send>,
    /// The screen grid, fed by a background reader thread.
    parser: Arc<Mutex<vt100::Parser>>,
    /// Kept for `resize`.
    master: Box<dyn MasterPty + Send>,
    /// Terminates the child (the waiter thread records the exit).
    killer: Box<dyn ChildKiller + Send + Sync>,
    /// Set by [`kill`] so the waiter records `Killed` instead of the signal
    /// exit `Failed`.
    killed: Arc<AtomicBool>,
    rows: u16,
    cols: u16,
}

/// One background task's bookkeeping entry.
struct TaskEntry {
    id: u64,
    /// Short label shown in the sidebar; defaults to the command itself.
    name: String,
    command: String,
    status: TaskStatus,
    exit_code: Option<i32>,
    started: Instant,
    finished: Option<Instant>,
    /// Combined stdout+stderr, capped at [`MAX_TASK_OUTPUT`]. Empty for
    /// interactive tasks, whose output lives in the vt100 screen instead.
    output: String,
    /// Signals the reader task to kill the child. `None` once finished.
    kill: Option<tokio::sync::oneshot::Sender<()>>,
    /// Feeds chunks to the child's stdin via the writer task. Dropping it
    /// (the `eof` action, or task finish) closes the pipe, delivering EOF.
    stdin_tx: Option<tokio::sync::mpsc::UnboundedSender<String>>,
    /// Interactive PTY state; `None` for pipe-based tasks.
    pty: Option<PtyState>,
}

/// A read-only view of an interactive task's screen, for the agent and the UI.
#[derive(Debug, Clone)]
pub struct ScreenSnapshot {
    /// The visible screen as plain text (no colors/attributes).
    pub text: String,
    pub cursor_row: u16,
    pub cursor_col: u16,
    /// The child is on its alternate screen (full-screen TUI like vim/htop).
    pub alt: bool,
    /// The child has enabled mouse reporting, so mouse events can be forwarded.
    pub mouse: bool,
    pub rows: u16,
    pub cols: u16,
}

/// Read-only copy of a task's state for rendering and tool output.
#[derive(Debug, Clone)]
pub struct TaskSnapshot {
    pub id: u64,
    pub name: String,
    pub command: String,
    pub status: TaskStatus,
    pub exit_code: Option<i32>,
    pub elapsed: Duration,
    pub output: String,
}

fn registry() -> &'static Mutex<Vec<TaskEntry>> {
    static REGISTRY: OnceLock<Mutex<Vec<TaskEntry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Vec::new()))
}

fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

fn snapshot_entry(e: &TaskEntry) -> TaskSnapshot {
    TaskSnapshot {
        id: e.id,
        name: e.name.clone(),
        command: e.command.clone(),
        status: e.status,
        exit_code: e.exit_code,
        elapsed: e
            .finished
            .map(|f| f - e.started)
            .unwrap_or_else(|| e.started.elapsed()),
        output: entry_output(e),
    }
}

/// The task's output text: the captured pipe buffer, or — for interactive
/// tasks — the current vt100 screen contents.
fn entry_output(e: &TaskEntry) -> String {
    match &e.pty {
        Some(pty) => pty
            .parser
            .lock()
            .map(|p| p.screen().contents())
            .unwrap_or_default(),
        None => e.output.clone(),
    }
}

/// Snapshot every task, newest first.
pub fn snapshot_all() -> Vec<TaskSnapshot> {
    let reg = registry().lock().unwrap();
    reg.iter().rev().map(snapshot_entry).collect()
}

/// Snapshot a single task by id.
pub fn snapshot(id: u64) -> Option<TaskSnapshot> {
    let reg = registry().lock().unwrap();
    reg.iter().find(|e| e.id == id).map(snapshot_entry)
}

/// Spawn `command` through the platform shell as a background task and return
/// its id immediately. Output is captured incrementally; completion is
/// recorded by a detached reader task.
pub fn spawn(command: &str, dir: Option<&str>, name: Option<&str>) -> Result<u64, String> {
    let (program, flag) = crate::tools::shell();
    let mut cmd = tokio::process::Command::new(program);
    cmd.arg(flag);
    // `raw_arg` keeps the command's own quoting intact on Windows (see the
    // `command` tool for the full story).
    #[cfg(windows)]
    cmd.raw_arg(command);
    #[cfg(not(windows))]
    cmd.arg(command);
    if let Some(dir) = dir {
        cmd.current_dir(dir);
    }
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    // Own (windowless) console on Windows so the child can't reset the TUI's
    // mouse capture.
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("error: failed to spawn task: {e}"))?;

    let id = next_id();
    let (kill_tx, kill_rx) = tokio::sync::oneshot::channel::<()>();

    // Stdin writer: owns the child's stdin and drains a channel into it, so
    // `write_stdin` stays synchronous (no lock held across an await). When the
    // channel closes — `eof`, task finish, or a write error — stdin drops and
    // the child sees EOF.
    let (stdin_tx, mut stdin_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let child_stdin = child.stdin.take();
    tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        let Some(mut stdin) = child_stdin else { return };
        while let Some(chunk) = stdin_rx.recv().await {
            if stdin.write_all(chunk.as_bytes()).await.is_err()
                || stdin.flush().await.is_err()
            {
                break;
            }
        }
    });

    {
        let mut reg = registry().lock().unwrap();
        reg.push(TaskEntry {
            id,
            name: name
                .filter(|n| !n.trim().is_empty())
                .unwrap_or(command)
                .to_string(),
            command: command.to_string(),
            status: TaskStatus::Running,
            exit_code: None,
            started: Instant::now(),
            finished: None,
            output: String::new(),
            kill: Some(kill_tx),
            stdin_tx: Some(stdin_tx),
            pty: None,
        });
    }

    // Drain both pipes in their own tasks; a full pipe would otherwise block
    // the child.
    let out_task = tokio::spawn(drain_stream(child.stdout.take(), id));
    let err_task = tokio::spawn(drain_stream(child.stderr.take(), id));
    tokio::spawn(async move {
        tokio::select! {
            result = child.wait() => {
                // Flush whatever is still buffered in the pipes before
                // marking the task finished.
                let _ = tokio::join!(out_task, err_task);
                let (status, code) = match result {
                    Ok(exit) => {
                        let code = exit.code();
                        if exit.success() {
                            (TaskStatus::Completed, code)
                        } else {
                            (TaskStatus::Failed, code)
                        }
                    }
                    Err(e) => {
                        append_output(id, &format!("\n[task error: {e}]"));
                        (TaskStatus::Failed, None)
                    }
                };
                finish(id, status, code);
            }
            _ = kill_rx => {
                let _ = child.kill().await;
                finish(id, TaskStatus::Killed, None);
            }
        }
    });

    Ok(id)
}

/// Spawn `command` in a real PTY as an interactive background task. Its output
/// is parsed into a vt100 screen grid (readable via [`screen_snapshot`]) and it
/// is driven with byte input via [`write_bytes`] rather than line-based stdin.
pub fn spawn_interactive(
    command: &str,
    dir: Option<&str>,
    name: Option<&str>,
    rows: u16,
    cols: u16,
) -> Result<u64, String> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("error: failed to open pty: {e}"))?;

    // Run the command through the host shell, matching the `command`/`task`
    // tools so shell syntax works.
    let (program, flag) = crate::tools::shell();
    let mut cmd = CommandBuilder::new(program);
    cmd.arg(flag);
    cmd.arg(command);
    // portable-pty's CommandBuilder does NOT inherit the parent's cwd (it
    // defaults to the home directory), so set it explicitly — to `dir` when
    // given, otherwise the app's working directory, matching pipe tasks.
    match dir {
        Some(dir) => cmd.cwd(dir),
        None => {
            if let Ok(cwd) = std::env::current_dir() {
                cmd.cwd(cwd);
            }
        }
    }

    let mut child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| format!("error: failed to spawn task: {e}"))?;
    // The parent doesn't need the slave once the child owns it.
    drop(pair.slave);

    let killer = child.clone_killer();
    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| format!("error: failed to read pty: {e}"))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| format!("error: failed to write pty: {e}"))?;
    let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, PTY_SCROLLBACK)));
    let killed = Arc::new(AtomicBool::new(false));

    let id = next_id();

    // Reader thread: pump PTY output into the vt100 parser until EOF. Blocking
    // reads keep this off the async runtime.
    {
        let parser = Arc::clone(&parser);
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if let Ok(mut p) = parser.lock() {
                            p.process(&buf[..n]);
                        }
                    }
                }
            }
        });
    }

    // Waiter thread: record the exit status when the child finishes.
    {
        let killed = Arc::clone(&killed);
        std::thread::spawn(move || {
            let (status, code) = match child.wait() {
                _ if killed.load(Ordering::Relaxed) => (TaskStatus::Killed, None),
                Ok(exit) => {
                    let code = exit.exit_code() as i32;
                    if exit.success() {
                        (TaskStatus::Completed, Some(code))
                    } else {
                        (TaskStatus::Failed, Some(code))
                    }
                }
                Err(_) => (TaskStatus::Failed, None),
            };
            finish(id, status, code);
        });
    }

    {
        let mut reg = registry().lock().unwrap();
        reg.push(TaskEntry {
            id,
            name: name
                .filter(|n| !n.trim().is_empty())
                .unwrap_or(command)
                .to_string(),
            command: command.to_string(),
            status: TaskStatus::Running,
            exit_code: None,
            started: Instant::now(),
            finished: None,
            output: String::new(),
            kill: None,
            stdin_tx: None,
            pty: Some(PtyState {
                writer,
                parser,
                master: pair.master,
                killer,
                killed,
                rows,
                cols,
            }),
        });
    }

    Ok(id)
}

/// Whether a task is interactive (runs in a PTY).
pub fn is_interactive(id: u64) -> bool {
    registry()
        .lock()
        .unwrap()
        .iter()
        .find(|e| e.id == id)
        .map(|e| e.pty.is_some())
        .unwrap_or(false)
}

/// Write raw bytes (keystrokes, mouse sequences) to an interactive task's PTY.
pub fn write_bytes(id: u64, bytes: &[u8]) -> Result<(), String> {
    let mut reg = registry().lock().unwrap();
    let entry = reg
        .iter_mut()
        .find(|e| e.id == id)
        .ok_or_else(|| format!("error: no task with id {id}"))?;
    let status = entry.status;
    let Some(pty) = entry.pty.as_mut() else {
        return Err(format!(
            "error: task {id} is not interactive; use write/eof for stdin"
        ));
    };
    if status != TaskStatus::Running {
        return Err(format!("error: task {id} already finished ({})", status.label()));
    }
    pty.writer
        .write_all(bytes)
        .and_then(|()| pty.writer.flush())
        .map_err(|e| format!("error: failed to send input to task {id}: {e}"))
}

/// Snapshot an interactive task's current screen.
pub fn screen_snapshot(id: u64) -> Result<ScreenSnapshot, String> {
    let reg = registry().lock().unwrap();
    let entry = reg
        .iter()
        .find(|e| e.id == id)
        .ok_or_else(|| format!("error: no task with id {id}"))?;
    let pty = entry
        .pty
        .as_ref()
        .ok_or_else(|| format!("error: task {id} is not interactive"))?;
    let parser = pty.parser.lock().unwrap();
    let screen = parser.screen();
    let (cursor_row, cursor_col) = screen.cursor_position();
    Ok(ScreenSnapshot {
        text: screen.contents(),
        cursor_row,
        cursor_col,
        alt: screen.alternate_screen(),
        mouse: screen.mouse_protocol_mode() != vt100::MouseProtocolMode::None,
        rows: pty.rows,
        cols: pty.cols,
    })
}

/// Run `f` with a locked reference to an interactive task's vt100 screen, for
/// cell-level rendering. Returns `None` if the task doesn't exist or isn't
/// interactive.
pub fn with_screen<R>(id: u64, f: impl FnOnce(&vt100::Screen) -> R) -> Option<R> {
    let reg = registry().lock().unwrap();
    let entry = reg.iter().find(|e| e.id == id)?;
    let pty = entry.pty.as_ref()?;
    let parser = pty.parser.lock().ok()?;
    Some(f(parser.screen()))
}

/// Resize an interactive task's PTY and screen grid.
pub fn resize(id: u64, rows: u16, cols: u16) -> Result<(), String> {
    let mut reg = registry().lock().unwrap();
    let entry = reg
        .iter_mut()
        .find(|e| e.id == id)
        .ok_or_else(|| format!("error: no task with id {id}"))?;
    let pty = entry
        .pty
        .as_mut()
        .ok_or_else(|| format!("error: task {id} is not interactive"))?;
    pty.master
        .resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("error: failed to resize task {id}: {e}"))?;
    pty.parser.lock().unwrap().set_size(rows, cols);
    pty.rows = rows;
    pty.cols = cols;
    Ok(())
}

/// Translate a named key into the bytes a terminal sends for it. Returns `None`
/// for unknown names. Shared by the `task keys` action and (later) the UI's
/// keyboard forwarding. `ctrl-<letter>` / `c-<letter>` map to control bytes.
pub fn key_to_bytes(name: &str) -> Option<Vec<u8>> {
    let n = name.trim().to_ascii_lowercase();
    let bytes: Vec<u8> = match n.as_str() {
        "enter" | "return" => vec![b'\r'],
        "tab" => vec![b'\t'],
        "escape" | "esc" => vec![0x1b],
        "backspace" | "bs" => vec![0x7f],
        "space" => vec![b' '],
        "up" => vec![0x1b, b'[', b'A'],
        "down" => vec![0x1b, b'[', b'B'],
        "right" => vec![0x1b, b'[', b'C'],
        "left" => vec![0x1b, b'[', b'D'],
        "home" => vec![0x1b, b'[', b'H'],
        "end" => vec![0x1b, b'[', b'F'],
        "pageup" | "pgup" => vec![0x1b, b'[', b'5', b'~'],
        "pagedown" | "pgdn" => vec![0x1b, b'[', b'6', b'~'],
        "delete" | "del" => vec![0x1b, b'[', b'3', b'~'],
        "insert" | "ins" => vec![0x1b, b'[', b'2', b'~'],
        "f1" => vec![0x1b, b'O', b'P'],
        "f2" => vec![0x1b, b'O', b'Q'],
        "f3" => vec![0x1b, b'O', b'R'],
        "f4" => vec![0x1b, b'O', b'S'],
        "f5" => vec![0x1b, b'[', b'1', b'5', b'~'],
        "f6" => vec![0x1b, b'[', b'1', b'7', b'~'],
        "f7" => vec![0x1b, b'[', b'1', b'8', b'~'],
        "f8" => vec![0x1b, b'[', b'1', b'9', b'~'],
        "f9" => vec![0x1b, b'[', b'2', b'0', b'~'],
        "f10" => vec![0x1b, b'[', b'2', b'1', b'~'],
        "f11" => vec![0x1b, b'[', b'2', b'3', b'~'],
        "f12" => vec![0x1b, b'[', b'2', b'4', b'~'],
        _ => {
            let rest = n.strip_prefix("ctrl-").or_else(|| n.strip_prefix("c-"))?;
            let [ch] = rest.as_bytes() else { return None };
            if !ch.is_ascii_alphabetic() {
                return None;
            }
            // Ctrl+A..Ctrl+Z → 0x01..0x1a.
            vec![ch.to_ascii_lowercase() - b'a' + 1]
        }
    };
    Some(bytes)
}

/// Read a child pipe to EOF, appending chunks to the task's output buffer.
async fn drain_stream<R>(stream: Option<R>, id: u64)
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let Some(mut s) = stream else { return };
    let mut buf = [0u8; 4096];
    loop {
        match s.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => append_output(id, &String::from_utf8_lossy(&buf[..n])),
        }
    }
}

/// Send input to a running task's stdin. With `eof`, close stdin after the
/// write (an empty `text` with `eof` just closes it), delivering EOF to the
/// child. Errors if the id is unknown, the task finished, or stdin was
/// already closed.
pub fn write_stdin(id: u64, text: &str, eof: bool) -> Result<(), String> {
    let mut reg = registry().lock().unwrap();
    let entry = reg
        .iter_mut()
        .find(|e| e.id == id)
        .ok_or_else(|| format!("error: no task with id {id}"))?;
    if entry.status != TaskStatus::Running {
        return Err(format!(
            "error: task {id} already finished ({})",
            entry.status.label()
        ));
    }
    let Some(tx) = entry.stdin_tx.as_ref() else {
        return Err(format!("error: task {id} stdin is already closed"));
    };
    if !text.is_empty() {
        tx.send(text.to_string())
            .map_err(|_| format!("error: task {id} stdin is already closed"))?;
    }
    if eof {
        // Dropping the sender closes the channel; the writer task flushes any
        // queued chunks first, then drops stdin → EOF.
        entry.stdin_tx = None;
    }
    Ok(())
}

/// Request termination of a running task. Returns an error if the id is
/// unknown or the task already finished.
pub fn kill(id: u64) -> Result<(), String> {
    let mut reg = registry().lock().unwrap();
    let entry = reg
        .iter_mut()
        .find(|e| e.id == id)
        .ok_or_else(|| format!("error: no task with id {id}"))?;
    // Interactive tasks are killed through the PTY child killer; the waiter
    // thread records the exit.
    let status = entry.status;
    if let Some(pty) = entry.pty.as_mut() {
        if status != TaskStatus::Running {
            return Err(format!(
                "error: task {id} already finished ({})",
                status.label()
            ));
        }
        pty.killed.store(true, Ordering::Relaxed);
        return pty
            .killer
            .kill()
            .map_err(|e| format!("error: failed to kill task {id}: {e}"));
    }
    match entry.kill.take() {
        Some(tx) => {
            let _ = tx.send(());
            Ok(())
        }
        None => Err(format!(
            "error: task {id} already finished ({})",
            entry.status.label()
        )),
    }
}

/// Wait until the task finishes or `timeout` elapses. Returns the final
/// snapshot either way, plus whether it is still running.
pub async fn wait(id: u64, timeout: Duration) -> Result<(TaskSnapshot, bool), String> {
    let deadline = Instant::now() + timeout;
    loop {
        let snap = snapshot(id).ok_or_else(|| format!("error: no task with id {id}"))?;
        if snap.status != TaskStatus::Running {
            return Ok((snap, false));
        }
        if Instant::now() >= deadline {
            return Ok((snap, true));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

// ---------------------------------------------------------------------------
// Session persistence
// ---------------------------------------------------------------------------

/// Serialized form of one task, carried in the session file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedTask {
    pub id: u64,
    pub name: String,
    pub command: String,
    /// [`TaskStatus::label`] string ("running", "completed", …).
    pub status: String,
    pub exit_code: Option<i32>,
    pub elapsed_secs: u64,
    /// Tail of the captured output.
    pub output: String,
}

/// Snapshot every task for session storage, oldest first.
pub fn persist_all() -> Vec<PersistedTask> {
    let reg = registry().lock().unwrap();
    reg.iter()
        .map(|e| {
            let full = entry_output(e);
            let output = if full.chars().count() > MAX_PERSISTED_OUTPUT {
                let skip = full.chars().count() - MAX_PERSISTED_OUTPUT;
                full.chars().skip(skip).collect()
            } else {
                full
            };
            PersistedTask {
                id: e.id,
                name: e.name.clone(),
                command: e.command.clone(),
                status: e.status.label().to_string(),
                exit_code: e.exit_code,
                elapsed_secs: e
                    .finished
                    .map(|f| f - e.started)
                    .unwrap_or_else(|| e.started.elapsed())
                    .as_secs(),
                output,
            }
        })
        .collect()
}

/// Restore tasks saved in a session. Tasks that were still running when the
/// session was saved come back as [`TaskStatus::Killed`] — their processes
/// died with the previous instance. New task ids continue above the restored
/// ones.
pub fn restore(saved: &[PersistedTask]) {
    let mut reg = registry().lock().unwrap();
    let now = Instant::now();
    for t in saved {
        if reg.iter().any(|e| e.id == t.id) {
            continue;
        }
        let status = match t.status.as_str() {
            "completed" => TaskStatus::Completed,
            "failed" => TaskStatus::Failed,
            // "running" becomes Killed: the child died with the old process.
            _ => TaskStatus::Killed,
        };
        let started = now
            .checked_sub(Duration::from_secs(t.elapsed_secs))
            .unwrap_or(now);
        reg.push(TaskEntry {
            id: t.id,
            name: t.name.clone(),
            command: t.command.clone(),
            status,
            exit_code: t.exit_code,
            started,
            finished: Some(now),
            output: t.output.clone(),
            kill: None,
            stdin_tx: None,
            pty: None,
        });
    }
    let max_id = reg.iter().map(|e| e.id).max().unwrap_or(0);
    NEXT_ID.fetch_max(max_id + 1, Ordering::Relaxed);
}

fn append_output(id: u64, chunk: &str) {
    let mut reg = registry().lock().unwrap();
    if let Some(entry) = reg.iter_mut().find(|e| e.id == id) {
        entry.output.push_str(chunk);
        if entry.output.len() > MAX_TASK_OUTPUT {
            // Drop the oldest half at a char boundary.
            let mut cut = entry.output.len() / 2;
            while !entry.output.is_char_boundary(cut) {
                cut += 1;
            }
            entry.output.replace_range(..cut, "[earlier output dropped]\n");
        }
    }
}

fn finish(id: u64, status: TaskStatus, exit_code: Option<i32>) {
    let mut reg = registry().lock().unwrap();
    if let Some(entry) = reg.iter_mut().find(|e| e.id == id) {
        entry.status = status;
        entry.exit_code = exit_code;
        entry.finished = Some(Instant::now());
        entry.kill = None;
        entry.stdin_tx = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn echo_cmd() -> &'static str {
        if cfg!(windows) { "echo task-out" } else { "echo task-out" }
    }

    #[tokio::test]
    async fn spawn_completes_and_captures_output() {
        let id = spawn(echo_cmd(), None, Some("echo test")).expect("spawn");
        let (snap, still_running) = wait(id, Duration::from_secs(10)).await.expect("wait");
        assert!(!still_running, "echo should finish quickly");
        assert_eq!(snap.status, TaskStatus::Completed);
        assert!(snap.output.contains("task-out"), "output: {}", snap.output);
        assert_eq!(snap.name, "echo test");
    }

    #[tokio::test]
    async fn failing_command_is_marked_failed() {
        let id = spawn("exit 3", None, None).expect("spawn");
        let (snap, _) = wait(id, Duration::from_secs(10)).await.expect("wait");
        assert_eq!(snap.status, TaskStatus::Failed);
        assert_eq!(snap.exit_code, Some(3));
    }

    #[tokio::test]
    async fn kill_terminates_a_running_task() {
        let long = if cfg!(windows) { "ping -n 30 127.0.0.1" } else { "sleep 30" };
        let id = spawn(long, None, None).expect("spawn");
        kill(id).expect("kill should succeed while running");
        let (snap, still_running) = wait(id, Duration::from_secs(10)).await.expect("wait");
        assert!(!still_running);
        assert_eq!(snap.status, TaskStatus::Killed);
        // A second kill reports the task as finished.
        assert!(kill(id).is_err());
    }

    #[test]
    fn restore_marks_running_tasks_as_killed_and_bumps_ids() {
        // Ids far above anything the other tests allocate, so the shared
        // global registry doesn't collide.
        let saved = vec![
            PersistedTask {
                id: 900_001,
                name: "dev server".to_string(),
                command: "npm run dev".to_string(),
                status: "running".to_string(),
                exit_code: None,
                elapsed_secs: 90,
                output: "listening on :3000".to_string(),
            },
            PersistedTask {
                id: 900_002,
                name: "build".to_string(),
                command: "cargo build".to_string(),
                status: "completed".to_string(),
                exit_code: Some(0),
                elapsed_secs: 30,
                output: String::new(),
            },
        ];
        restore(&saved);

        let running = snapshot(900_001).expect("restored task");
        assert_eq!(running.status, TaskStatus::Killed, "running → killed on restore");
        assert_eq!(running.elapsed.as_secs(), 90);
        assert!(running.output.contains("listening"));

        let done = snapshot(900_002).expect("restored task");
        assert_eq!(done.status, TaskStatus::Completed);
        assert_eq!(done.exit_code, Some(0));

        // New ids continue above the restored ones.
        assert!(next_id() > 900_002);

        // Restoring again must not duplicate entries.
        restore(&saved);
        let dupes = snapshot_all()
            .iter()
            .filter(|t| t.id == 900_001)
            .count();
        assert_eq!(dupes, 1);

        // Round trip: the restored tasks serialize back out.
        let persisted = persist_all();
        assert!(persisted.iter().any(|t| t.id == 900_001 && t.status == "killed"));
    }

    #[tokio::test]
    async fn write_feeds_stdin_and_eof_closes_it() {
        // Both commands echo stdin lines back and exit on EOF.
        let cmd = if cfg!(windows) { "findstr ." } else { "cat" };
        let id = spawn(cmd, None, None).expect("spawn");
        write_stdin(id, "hello-stdin\n", false).expect("write while running");
        write_stdin(id, "", true).expect("eof closes stdin");
        let (snap, still_running) = wait(id, Duration::from_secs(10)).await.expect("wait");
        assert!(!still_running, "EOF should end the reader");
        assert!(
            snap.output.contains("hello-stdin"),
            "output: {}",
            snap.output
        );
        // Writes after finish are rejected.
        let err = write_stdin(id, "late\n", false).expect_err("finished task");
        assert!(err.contains("finished"), "got: {err}");
    }

    #[test]
    fn key_to_bytes_maps_named_and_ctrl_keys() {
        assert_eq!(key_to_bytes("enter"), Some(vec![b'\r']));
        assert_eq!(key_to_bytes("Up"), Some(vec![0x1b, b'[', b'A']));
        assert_eq!(key_to_bytes("ctrl-c"), Some(vec![0x03]));
        assert_eq!(key_to_bytes("c-u"), Some(vec![0x15]));
        assert_eq!(key_to_bytes("f1"), Some(vec![0x1b, b'O', b'P']));
        assert!(key_to_bytes("nonsense").is_none());
        assert!(key_to_bytes("ctrl-1").is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn interactive_pty_takes_input_and_reads_screen() {
        // `cat` echoes what we type; drive it through the PTY and read it back.
        let id = spawn_interactive("cat", None, Some("cat"), 24, 80).expect("spawn");
        assert!(is_interactive(id));
        write_bytes(id, b"pty-echo-123\r").expect("write bytes");
        tokio::time::sleep(Duration::from_millis(500)).await;

        let snap = screen_snapshot(id).expect("screen");
        assert!(snap.text.contains("pty-echo-123"), "screen: {}", snap.text);
        assert!(!snap.alt, "cat should not use the alternate screen");

        // Pipe-only helpers reject interactive tasks.
        assert!(write_stdin(id, "x\n", false).is_err());

        kill(id).expect("kill running interactive task");
        let (snap, still) = wait(id, Duration::from_secs(10)).await.expect("wait");
        assert!(!still);
        assert_eq!(snap.status, TaskStatus::Killed);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn interactive_task_defaults_to_current_dir() {
        // With no `dir`, the child must run in the app's cwd — not the home
        // directory portable-pty would otherwise default to.
        let cwd = std::env::current_dir().unwrap();
        let id = spawn_interactive("pwd", None, Some("pwd"), 10, 80).expect("spawn");
        tokio::time::sleep(Duration::from_millis(500)).await;
        let snap = screen_snapshot(id).expect("screen");
        assert!(
            snap.text.contains(&cwd.to_string_lossy().to_string()),
            "expected cwd {cwd:?} in screen: {}",
            snap.text
        );
        kill(id).ok();
    }

    #[tokio::test]
    async fn non_interactive_task_rejects_pty_ops() {
        // A pipe task (id that isn't interactive) has no PTY surface.
        let id = spawn("echo hi", None, None).expect("spawn");
        assert!(!is_interactive(id));
        assert!(write_bytes(id, b"x").is_err());
        assert!(screen_snapshot(id).is_err());
    }

    #[tokio::test]
    async fn wait_times_out_on_running_task() {
        let long = if cfg!(windows) { "ping -n 30 127.0.0.1" } else { "sleep 30" };
        let id = spawn(long, None, None).expect("spawn");
        let (snap, still_running) =
            wait(id, Duration::from_millis(300)).await.expect("wait");
        assert!(still_running);
        assert_eq!(snap.status, TaskStatus::Running);
        let _ = kill(id);
    }
}
