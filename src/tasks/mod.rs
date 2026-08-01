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

//! Task process system: shell commands tracked independently from the
//! conversation renderer.
//!
//! Background tasks are created through the `task` tool. Foreground `command`
//! calls use the same process lifecycle and output capture, remain hidden while
//! foreground, and enter the task UI if promoted. State lives in a
//! process-global registry because tools only receive their JSON arguments —
//! there is no `App` handle in the tool path.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
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
static NEXT_EVENT_SEQUENCE: AtomicU64 = AtomicU64::new(1);
static TASK_GENERATION: AtomicU64 = AtomicU64::new(1);

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

/// The caller that owns a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskKind {
    Background,
    Command,
}

/// How a process entered the task system. Unlike [`TaskKind`], this remains
/// stable when a foreground command is promoted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskOrigin {
    TaskTool,
    Command,
    PromotedCommand,
    BangCommand,
    Restored,
}

/// A terminal state transition delivered to the application. The output is
/// copied into the event after the process readers have drained, so clearing a
/// finished task cannot erase a notification that is already in flight.
#[derive(Debug, Clone)]
pub struct TaskLifecycleEvent {
    pub sequence: u64,
    pub generation: u64,
    pub task_id: u64,
    pub origin: TaskOrigin,
    pub old_status: TaskStatus,
    pub new_status: TaskStatus,
    pub name: String,
    pub command: String,
    pub exit_code: Option<i32>,
    pub elapsed: Duration,
    pub stdout_tail: String,
    pub stderr_tail: String,
    pub transcript_tail: String,
    pub(crate) notify_agent: Arc<AtomicBool>,
}

impl TaskLifecycleEvent {
    pub(crate) fn should_notify_agent(&self) -> bool {
        self.notify_agent.load(Ordering::Acquire)
    }
}

/// Interactive-task state: a child running in a real PTY, its output parsed
/// into a vt100 screen grid. Present only for tasks created interactively;
/// pipe-based tasks leave this `None` and use `kill`/`stdin_tx` instead.
struct PtyState {
    /// Writes bytes (keystrokes, mouse sequences) to the child. Shared with
    /// the reader thread, which answers cursor-position queries (`ESC[6n`)
    /// on the child's behalf — see `spawn_interactive`.
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
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
    /// Raw byte transcript of everything the child wrote (escape sequences
    /// included), capped like [`MAX_TASK_OUTPUT`]. The vt100 screen only holds
    /// the visible grid plus scrollback; this keeps the whole session so a
    /// finished `!command` can be replayed to the model via [`transcript`].
    transcript: String,
    /// Snapshot of screen text last returned to a [`screen_diff`] caller, so
    /// the next call can compute a delta. `None` means no prior call.
    last_screen_text: Option<String>,
}

/// One background task's bookkeeping entry.
struct TaskEntry {
    id: u64,
    kind: TaskKind,
    origin: TaskOrigin,
    notify_agent: Arc<AtomicBool>,
    generation: u64,
    /// Tool-call id for a foreground `command`, used by live conversation
    /// rendering. Background tasks leave this unset.
    call_id: Option<String>,
    /// Short label shown in the sidebar; defaults to the command itself.
    name: String,
    command: String,
    status: TaskStatus,
    exit_code: Option<i32>,
    started: Instant,
    finished: Option<Instant>,
    /// Stdout (pipe tasks only), capped at the task's `max_output`.
    output: String,
    /// Stderr captured separately (pipe tasks only).
    stderr_output: String,
    /// Interleaved tail used while a foreground command is still running.
    live_output: String,
    /// Per-task output cap in chars. Defaults to [`MAX_TASK_OUTPUT`]; can be
    /// overridden with [`set_max_output`].
    max_output: usize,
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
    /// Stderr captured separately (pipe tasks only; empty otherwise).
    pub stderr: String,
}

/// Lightweight task data used by the sidebar. Output is populated only for
/// expanded tasks and is already reduced to the few lines the sidebar renders.
#[derive(Debug, Clone)]
pub struct SidebarTaskSnapshot {
    pub id: u64,
    pub name: String,
    pub status: TaskStatus,
    pub exit_code: Option<i32>,
    pub elapsed: Duration,
    pub output: Option<SidebarTaskOutput>,
}

#[derive(Debug, Clone)]
pub struct SidebarTaskOutput {
    /// The already-bounded tail rendered under an expanded task.
    pub lines: Vec<String>,
    /// Number of lines omitted before `lines`.
    pub omitted_lines: usize,
}

fn registry() -> &'static Mutex<Vec<TaskEntry>> {
    static REGISTRY: OnceLock<Mutex<Vec<TaskEntry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Vec::new()))
}

fn event_sink() -> &'static Mutex<Option<tokio::sync::mpsc::UnboundedSender<TaskLifecycleEvent>>> {
    static SINK: OnceLock<Mutex<Option<tokio::sync::mpsc::UnboundedSender<TaskLifecycleEvent>>>> =
        OnceLock::new();
    SINK.get_or_init(|| Mutex::new(None))
}

/// Install the single application consumer for lifecycle events.
pub fn install_event_sink(sender: tokio::sync::mpsc::UnboundedSender<TaskLifecycleEvent>) {
    *event_sink().lock().unwrap() = Some(sender);
}

pub fn current_generation() -> u64 {
    TASK_GENERATION.load(Ordering::Relaxed)
}

fn publish_event(event: TaskLifecycleEvent) {
    let sender = event_sink().lock().unwrap().clone();
    if let Some(sender) = sender {
        let _ = sender.send(event);
    }
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
        stderr: e.stderr_output.clone(),
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
    reg.iter()
        .rev()
        .filter(|entry| entry.kind == TaskKind::Background)
        .map(snapshot_entry)
        .collect()
}

pub fn task_ids() -> HashSet<u64> {
    registry()
        .lock()
        .unwrap()
        .iter()
        .filter(|entry| entry.kind == TaskKind::Background)
        .map(|entry| entry.id)
        .collect()
}

/// Snapshot task metadata for sidebar rendering. Only tasks in `expanded_ids`
/// include output, capped to the visible tail so collapsed tasks never copy
/// their potentially large output buffers on every frame.
pub fn snapshot_for_sidebar(expanded_ids: &HashSet<u64>) -> Vec<SidebarTaskSnapshot> {
    const OUTPUT_TAIL_LINES: usize = 10;
    const MAX_LINE_CHARS: usize = 512;

    let reg = registry().lock().unwrap();
    reg.iter()
        .rev()
        .filter(|entry| entry.kind == TaskKind::Background)
        .map(|entry| SidebarTaskSnapshot {
            id: entry.id,
            name: entry.name.clone(),
            status: entry.status,
            exit_code: entry.exit_code,
            elapsed: entry
                .finished
                .map(|finished| finished - entry.started)
                .unwrap_or_else(|| entry.started.elapsed()),
            output: expanded_ids
                .contains(&entry.id)
                .then(|| sidebar_output(entry, OUTPUT_TAIL_LINES, MAX_LINE_CHARS)),
        })
        .collect()
}

fn sidebar_output(
    entry: &TaskEntry,
    tail_lines: usize,
    max_line_chars: usize,
) -> SidebarTaskOutput {
    let text = entry_output(entry);
    sidebar_output_text(&text, tail_lines, max_line_chars)
}

fn sidebar_output_text(text: &str, tail_lines: usize, max_line_chars: usize) -> SidebarTaskOutput {
    let total_lines = text.lines().count();
    let lines = text
        .lines()
        .rev()
        .take(tail_lines)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|line| line.chars().take(max_line_chars).collect())
        .collect::<Vec<_>>();
    SidebarTaskOutput {
        omitted_lines: total_lines.saturating_sub(lines.len()),
        lines,
    }
}

/// Snapshot a single task by id.
pub fn snapshot(id: u64) -> Option<TaskSnapshot> {
    let reg = registry().lock().unwrap();
    reg.iter().find(|e| e.id == id).map(snapshot_entry)
}

/// Remove every task that is no longer running, returning the number removed.
pub fn clear_finished() -> usize {
    let mut reg = registry().lock().unwrap();
    let before = reg.len();
    reg.retain(|entry| entry.status == TaskStatus::Running);
    before - reg.len()
}

/// Spawn `command` through the platform shell as a background task and return
/// its id immediately. Output is captured incrementally; completion is
/// recorded by a detached reader task.
pub fn spawn(command: &str, dir: Option<&str>, name: Option<&str>) -> Result<u64, String> {
    spawn_pipe(command, dir, name, TaskKind::Background, None, true, None)
}

pub fn spawn_secure(
    command: &str,
    dir: Option<&str>,
    name: Option<&str>,
    security: &crate::security::SecurityManager,
) -> Result<u64, String> {
    spawn_pipe(
        command,
        dir,
        name,
        TaskKind::Background,
        None,
        true,
        Some(security),
    )
}

/// Spawn a foreground command tool call through the shared task process
/// implementation. Its stdin is closed immediately, matching the old command
/// runner, while stdout and stderr remain available until the caller finishes.
pub fn spawn_command(
    command: &str,
    dir: Option<&str>,
    call_id: Option<&str>,
) -> Result<u64, String> {
    spawn_pipe(
        command,
        dir,
        Some(command),
        TaskKind::Command,
        call_id,
        false,
        None,
    )
}

pub fn spawn_command_secure(
    command: &str,
    dir: Option<&str>,
    call_id: Option<&str>,
    security: &crate::security::SecurityManager,
) -> Result<u64, String> {
    spawn_pipe(
        command,
        dir,
        Some(command),
        TaskKind::Command,
        call_id,
        false,
        Some(security),
    )
}

fn spawn_pipe(
    command: &str,
    dir: Option<&str>,
    name: Option<&str>,
    kind: TaskKind,
    call_id: Option<&str>,
    keep_stdin_open: bool,
    security: Option<&crate::security::SecurityManager>,
) -> Result<u64, String> {
    let sandbox = security
        .map(|security| security.sandbox_invocation(command, dir))
        .transpose()?
        .flatten();
    let mut cmd = if let Some(invocation) = sandbox {
        let mut cmd = tokio::process::Command::new(&invocation.program);
        crate::security::sandbox::configure_tokio_command(&mut cmd, invocation);
        cmd
    } else {
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
        cmd
    };
    // Put the shell and its descendants in a dedicated process group so kill,
    // timeout, and cancellation terminate the whole command tree rather than
    // leaving grandchildren alive with stdout/stderr pipes held open.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.as_std_mut().process_group(0);
    }
    cmd.stdin(if keep_stdin_open {
        std::process::Stdio::piped()
    } else {
        std::process::Stdio::null()
    })
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
    let stdin_tx = if keep_stdin_open {
        let (stdin_tx, mut stdin_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let child_stdin = child.stdin.take();
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            let Some(mut stdin) = child_stdin else {
                return;
            };
            while let Some(chunk) = stdin_rx.recv().await {
                if stdin.write_all(chunk.as_bytes()).await.is_err() || stdin.flush().await.is_err()
                {
                    break;
                }
            }
        });
        Some(stdin_tx)
    } else {
        None
    };

    {
        let (origin, notify_agent) = match kind {
            TaskKind::Background => (TaskOrigin::TaskTool, true),
            TaskKind::Command => (TaskOrigin::Command, false),
        };
        let mut reg = registry().lock().unwrap();
        reg.push(TaskEntry {
            id,
            kind,
            origin,
            notify_agent: Arc::new(AtomicBool::new(notify_agent)),
            generation: current_generation(),
            call_id: call_id.map(str::to_string),
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
            stderr_output: String::new(),
            live_output: String::new(),
            // Preserve the old command tool's complete capture. The generic
            // tool-output layer archives long results after the call returns.
            max_output: if kind == TaskKind::Command {
                usize::MAX
            } else {
                MAX_TASK_OUTPUT
            },
            kill: Some(kill_tx),
            stdin_tx,
            pty: None,
        });
    }

    // Drain both pipes in their own tasks; a full pipe would otherwise block
    // the child.
    let out_task = tokio::spawn(drain_stream(child.stdout.take(), id, false));
    let err_task = tokio::spawn(drain_stream(child.stderr.take(), id, true));
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
                kill_process_tree(&mut child).await;
                let _ = tokio::join!(out_task, err_task);
                finish(id, TaskStatus::Killed, None);
            }
        }
    });

    Ok(id)
}

/// Return the interleaved live output for a running foreground command.
pub fn command_live_output(call_id: &str) -> Option<String> {
    let reg = registry().lock().unwrap();
    reg.iter()
        .find(|entry| {
            entry.status == TaskStatus::Running && entry.call_id.as_deref() == Some(call_id)
        })
        .map(|entry| entry.live_output.clone())
}

/// Promote a running foreground command into a normal background task without
/// restarting or interrupting its process.
pub fn promote_command(id: u64) -> Result<(), String> {
    let mut reg = registry().lock().unwrap();
    let entry = reg
        .iter_mut()
        .find(|entry| entry.id == id)
        .ok_or_else(|| format!("error: no task with id {id}"))?;
    if entry.kind != TaskKind::Command {
        return Err(format!("error: task {id} is already a background task"));
    }
    if entry.status != TaskStatus::Running {
        return Err(format!(
            "error: command {id} already finished ({})",
            entry.status.label()
        ));
    }

    entry.kind = TaskKind::Background;
    entry.origin = TaskOrigin::PromotedCommand;
    entry.notify_agent.store(true, Ordering::Release);
    entry.call_id = None;
    entry.max_output = MAX_TASK_OUTPUT;
    cap_buffer(&mut entry.output, MAX_TASK_OUTPUT);
    cap_buffer(&mut entry.stderr_output, MAX_TASK_OUTPUT);
    Ok(())
}

/// Promote the newest running foreground command, used by the global Ctrl+Z
/// shortcut. Mutating tools execute serially, so normally there is only one.
pub fn promote_running_command() -> Option<u64> {
    let id = {
        let reg = registry().lock().unwrap();
        reg.iter()
            .rev()
            .find(|entry| entry.kind == TaskKind::Command && entry.status == TaskStatus::Running)
            .map(|entry| entry.id)
    }?;
    promote_command(id).ok()?;
    Some(id)
}

async fn kill_process_tree(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        unsafe extern "C" {
            fn kill(pid: i32, signal: i32) -> i32;
        }
        const SIGKILL: i32 = 9;
        // A negative pid addresses the process group created in `spawn_pipe`.
        unsafe {
            kill(-(pid as i32), SIGKILL);
        }
    }

    #[cfg(windows)]
    if let Some(pid) = child.id() {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let pid = pid.to_string();
        let _ = tokio::process::Command::new("taskkill")
            .args(["/PID", pid.as_str(), "/T", "/F"])
            .creation_flags(CREATE_NO_WINDOW)
            .status()
            .await;
    }

    let _ = child.kill().await;
}

/// Whether a command has already handed ownership to the background task
/// system. Cancellation and hard-timeout paths use this to avoid killing a
/// process after the user has pressed Ctrl+Z.
pub fn is_background(id: u64) -> bool {
    let reg = registry().lock().unwrap();
    reg.iter()
        .find(|entry| entry.id == id)
        .is_some_and(|entry| entry.kind == TaskKind::Background)
}

/// Drop a completed foreground command after its result has been copied into
/// the conversation. Promoted background tasks are deliberately retained.
pub fn forget_command(id: u64) {
    let mut reg = registry().lock().unwrap();
    reg.retain(|entry| {
        entry.id != id || entry.kind != TaskKind::Command || entry.status == TaskStatus::Running
    });
}

/// Restrict `LoadLibrary`'s search to the application and system directories.
///
/// Without this, portable-pty's `LoadLibrary("conpty.dll")` probe walks `PATH`
/// and can pick up another program's ConPTY implementation — WezTerm ships
/// `conpty.dll` + `OpenConsole.exe`, and under its host our PTY children hang
/// with no output ever arriving. Cutting `PATH`/cwd out of the DLL search makes
/// the probe fail cleanly so portable-pty falls back to the system ConPTY in
/// kernel32. (Also standard DLL-hijack hardening; spawning child processes is
/// unaffected.) Must run before the first `openpty`, which caches whichever
/// implementation it finds first; `Once` makes repeat calls free.
#[cfg(windows)]
pub fn harden_dll_search() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        const LOAD_LIBRARY_SEARCH_DEFAULT_DIRS: u32 = 0x0000_1000;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn SetDefaultDllDirectories(flags: u32) -> i32;
        }
        unsafe {
            SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_DEFAULT_DIRS);
        }
    });
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
    spawn_interactive_with_origin(command, dir, name, rows, cols, TaskOrigin::TaskTool, None)
}

pub fn spawn_interactive_secure(
    command: &str,
    dir: Option<&str>,
    name: Option<&str>,
    rows: u16,
    cols: u16,
    security: &crate::security::SecurityManager,
) -> Result<u64, String> {
    spawn_interactive_with_origin(
        command,
        dir,
        name,
        rows,
        cols,
        TaskOrigin::TaskTool,
        Some(security),
    )
}

pub fn spawn_bang_secure(
    command: &str,
    dir: Option<&str>,
    name: Option<&str>,
    rows: u16,
    cols: u16,
    security: &crate::security::SecurityManager,
) -> Result<u64, String> {
    spawn_interactive_with_origin(
        command,
        dir,
        name,
        rows,
        cols,
        TaskOrigin::BangCommand,
        Some(security),
    )
}

fn spawn_interactive_with_origin(
    command: &str,
    dir: Option<&str>,
    name: Option<&str>,
    rows: u16,
    cols: u16,
    origin: TaskOrigin,
    security: Option<&crate::security::SecurityManager>,
) -> Result<u64, String> {
    #[cfg(windows)]
    harden_dll_search();
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
    let sandbox = security
        .map(|security| security.sandbox_invocation(command, dir))
        .transpose()?
        .flatten();
    let cmd = if let Some(invocation) = sandbox {
        let mut cmd = CommandBuilder::new(&invocation.program);
        crate::security::sandbox::configure_pty_command(&mut cmd, invocation);
        cmd
    } else {
        let (program, flag) = crate::tools::shell();
        let mut cmd = CommandBuilder::new(program);
        cmd.arg(flag);
        cmd.arg(command);
        // portable-pty's CommandBuilder does NOT inherit the parent's cwd.
        match dir {
            Some(dir) => cmd.cwd(dir),
            None => {
                if let Ok(cwd) = std::env::current_dir() {
                    cmd.cwd(cwd);
                }
            }
        }
        cmd
    };

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
    let writer = Arc::new(Mutex::new(writer));
    let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, PTY_SCROLLBACK)));
    let killed = Arc::new(AtomicBool::new(false));

    let id = next_id();

    // Register the entry before the reader thread starts, so the first bytes
    // (which can arrive within milliseconds) find the transcript buffer.
    {
        let mut reg = registry().lock().unwrap();
        reg.push(TaskEntry {
            id,
            kind: TaskKind::Background,
            origin,
            notify_agent: Arc::new(AtomicBool::new(true)),
            generation: current_generation(),
            call_id: None,
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
            stderr_output: String::new(),
            live_output: String::new(),
            max_output: MAX_TASK_OUTPUT,
            kill: None,
            stdin_tx: None,
            pty: Some(PtyState {
                writer: Arc::clone(&writer),
                parser: Arc::clone(&parser),
                master: pair.master,
                killer,
                killed: Arc::clone(&killed),
                rows,
                cols,
                transcript: String::new(),
                last_screen_text: None,
            }),
        });
    }

    // Reader thread: pump PTY output into the vt100 parser until EOF. Blocking
    // reads keep this off the async runtime.
    //
    // No real terminal sits behind this PTY, so cursor-position queries
    // (`ESC[6n`) are answered here from the vt100 grid. Windows ConPTY makes
    // this load-bearing: it is created with INHERIT_CURSOR and blocks the
    // child's startup on exactly that query until a report arrives.
    let (reader_done_tx, reader_done_rx) = std::sync::mpsc::channel();
    {
        let parser = Arc::clone(&parser);
        let writer = Arc::clone(&writer);
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            // Tail of the previous chunk, kept so a query split across two
            // reads is still recognised.
            let mut window: Vec<u8> = Vec::new();
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if let Ok(mut p) = parser.lock() {
                            p.process(&buf[..n]);
                        }
                        append_transcript(id, &String::from_utf8_lossy(&buf[..n]));

                        window.extend_from_slice(&buf[..n]);
                        if window.windows(4).any(|w| w == b"\x1b[6n") {
                            let (row, col) = parser
                                .lock()
                                .map(|p| p.screen().cursor_position())
                                .unwrap_or((0, 0));
                            // One write_all, not write!: conhost's
                            // INHERIT_CURSOR handshake needs the whole report
                            // in a single pipe write — write! would split it
                            // per format fragment and the child never starts.
                            let reply = format!("\x1b[{};{}R", row + 1, col + 1);
                            if let Ok(mut w) = writer.lock() {
                                let _ = w.write_all(reply.as_bytes());
                                let _ = w.flush();
                            }
                            window.clear();
                        } else if window.len() > 3 {
                            window.drain(..window.len() - 3);
                        }
                    }
                }
            }
            let _ = reader_done_tx.send(());
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
            // PTY output is read on a separate blocking thread. Wait for EOF so
            // the lifecycle event includes the final screen/transcript tail.
            let _ = reader_done_rx.recv_timeout(Duration::from_secs(1));
            finish(id, status, code);
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
        return Err(format!(
            "error: task {id} already finished ({})",
            status.label()
        ));
    }
    let mut writer = pty.writer.lock().unwrap();
    writer
        .write_all(bytes)
        .and_then(|()| writer.flush())
        .map_err(|e| format!("error: failed to send input to task {id}: {e}"))
}

/// Write raw bytes to an interactive task's PTY, then wait until the screen
/// stabilizes (no changes for `stable_ms`). Returns the final screen text.
pub async fn write_bytes_and_wait(
    id: u64,
    bytes: &[u8],
    stable_ms: u64,
    timeout_ms: u64,
) -> Result<String, String> {
    write_bytes(id, bytes)?;

    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let mut last_text = screen_snapshot(id)?.text;
    let mut last_change = Instant::now();

    loop {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let current = screen_snapshot(id)?.text;
        if current != last_text {
            last_text = current;
            last_change = Instant::now();
        } else if last_change.elapsed() >= Duration::from_millis(stable_ms) {
            // Screen stable — input consumed.
            return Ok(last_text);
        }
        if Instant::now() >= deadline {
            return Ok(last_text);
        }
    }
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

/// Override the output buffer cap for a task. New tasks default to
/// [`MAX_TASK_OUTPUT`].
#[allow(dead_code)]
pub fn set_max_output(id: u64, max: usize) -> Result<(), String> {
    let mut reg = registry().lock().unwrap();
    let entry = reg
        .iter_mut()
        .find(|e| e.id == id)
        .ok_or_else(|| format!("error: no task with id {id}"))?;
    entry.max_output = max;
    // Apply the new cap immediately to both buffers.
    cap_buffer(&mut entry.output, max);
    cap_buffer(&mut entry.stderr_output, max);
    Ok(())
}

/// Return the stderr captured so far for a pipe task. `None` if the id is
/// unknown or the task is interactive (whose stderr is untagged in the PTY).
#[allow(dead_code)]
pub fn stderr_output(id: u64) -> Option<String> {
    let reg = registry().lock().unwrap();
    reg.iter()
        .find(|e| e.id == id)
        .map(|e| e.stderr_output.clone())
}

/// Poll an interactive task's screen until `pattern` appears or `timeout`
/// elapses. Returns the final snapshot and whether the pattern matched.
pub async fn expect_screen(
    id: u64,
    pattern: &str,
    timeout: Duration,
) -> Result<(ScreenSnapshot, bool), String> {
    let deadline = Instant::now() + timeout;
    loop {
        let snap = screen_snapshot(id)?;
        if snap.text.contains(pattern) {
            return Ok((snap, true));
        }
        if Instant::now() >= deadline {
            return Ok((snap, false));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Return only the lines that changed since the last [`screen_diff`] call (or
/// the entire screen on the first call). Each changed line is prefixed with
/// `+` (added), `-` (removed), or `~` (modified). Lines that exist in both
/// old and new are omitted.
pub fn screen_diff(id: u64) -> Result<String, String> {
    let mut reg = registry().lock().unwrap();
    let entry = reg
        .iter_mut()
        .find(|e| e.id == id)
        .ok_or_else(|| format!("error: no task with id {id}"))?;
    let pty = entry
        .pty
        .as_mut()
        .ok_or_else(|| format!("error: task {id} is not interactive"))?;
    let parser = pty.parser.lock().unwrap();
    let screen = parser.screen();
    let current = screen.contents();
    let old = std::mem::take(&mut pty.last_screen_text);
    // Drop the lock before computing the diff (it's pure string work).
    drop(parser);
    drop(reg);

    let Some(old_text) = old else {
        // First call — return the full screen.
        let mut reg2 = registry().lock().unwrap();
        #[allow(clippy::collapsible_if)]
        if let Some(entry) = reg2.iter_mut().find(|e| e.id == id) {
            if let Some(pty) = entry.pty.as_mut() {
                pty.last_screen_text = Some(current.clone());
            }
        }
        return Ok(format!("--- first diff (full screen) ---\n{current}"));
    };

    // Simple line-based diff: compare old and new line-by-line.
    let old_lines: Vec<&str> = old_text.lines().collect();
    let new_lines: Vec<&str> = current.lines().collect();
    let mut out = String::new();
    let max_len = old_lines.len().max(new_lines.len());
    for i in 0..max_len {
        match (old_lines.get(i), new_lines.get(i)) {
            (Some(o), Some(n)) if o == n => {} // unchanged — skip
            (Some(_), Some(n)) => {
                out.push_str(&format!("~{n}\n"));
            }
            (Some(_), None) => {
                out.push_str(&format!("-{}\n", old_lines[i]));
            }
            (None, Some(n)) => {
                out.push_str(&format!("+{n}\n"));
            }
            (None, None) => unreachable!(),
        }
    }

    // Restore the new text as last_screen_text for the next call.
    let mut reg2 = registry().lock().unwrap();
    #[allow(clippy::collapsible_if)]
    if let Some(entry) = reg2.iter_mut().find(|e| e.id == id) {
        if let Some(pty) = entry.pty.as_mut() {
            pty.last_screen_text = Some(current);
        }
    }

    if out.is_empty() {
        Ok("(no changes)".to_string())
    } else {
        Ok(out.trim_end().to_string())
    }
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

/// Scroll an interactive task's scrollback view by `delta` lines (positive =
/// toward older output). Clamps at the bottom (0). Use `i32::MIN` to snap back
/// to live output. No-op for non-interactive tasks.
pub fn scroll_screen(id: u64, delta: i32) {
    let reg = registry().lock().unwrap();
    if let Some(entry) = reg.iter().find(|e| e.id == id)
        && let Some(pty) = &entry.pty
        && let Ok(mut parser) = pty.parser.lock()
    {
        let current = parser.screen().scrollback() as i64;
        let next = (current + delta as i64).max(0) as usize;
        parser.set_scrollback(next);
    }
}

/// Encode one SGR (1006) mouse report. `code` is the button/motion/wheel code
/// (with any modifier bits already applied); `col0`/`row0` are 0-based cell
/// coordinates (SGR is 1-based, so they're incremented); `release` selects the
/// final `m` (release) vs `M` (press/motion). Shared by the UI's mouse
/// forwarding and the `task send_mouse` action.
pub fn sgr_mouse(code: u8, col0: u16, row0: u16, release: bool) -> Vec<u8> {
    let final_char = if release { 'm' } else { 'M' };
    format!("\x1b[<{code};{};{}{final_char}", col0 + 1, row0 + 1).into_bytes()
}

/// Read a child pipe to EOF, appending chunks to the task's output buffer
/// (stdout) or stderr buffer.
async fn drain_stream<R>(stream: Option<R>, id: u64, stderr: bool)
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let Some(mut s) = stream else { return };
    let mut buf = [0u8; 4096];
    loop {
        match s.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let chunk = String::from_utf8_lossy(&buf[..n]);
                if stderr {
                    append_stderr(id, &chunk);
                } else {
                    append_output(id, &chunk);
                }
            }
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
    kill_inner(id, false)
}

/// Terminate a task on the agent's request without reporting the resulting
/// terminal state back to that same agent.
pub fn kill_for_agent(id: u64) -> Result<(), String> {
    kill_inner(id, true)
}

fn kill_inner(id: u64, consume_notification: bool) -> Result<(), String> {
    let mut reg = registry().lock().unwrap();
    let entry = reg
        .iter_mut()
        .find(|e| e.id == id)
        .ok_or_else(|| format!("error: no task with id {id}"))?;
    let notify_agent = Arc::clone(&entry.notify_agent);
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
        let result = pty
            .killer
            .kill()
            .map_err(|e| format!("error: failed to kill task {id}: {e}"));
        if result.is_ok() && consume_notification {
            notify_agent.store(false, Ordering::Release);
        }
        return result;
    }
    match entry.kill.take() {
        Some(tx) => {
            let _ = tx.send(());
            if consume_notification {
                notify_agent.store(false, Ordering::Release);
            }
            Ok(())
        }
        None => Err(format!(
            "error: task {id} already finished ({})",
            entry.status.label()
        )),
    }
}

/// Kill every running task and clear the registry so the next session starts
/// clean.
pub fn kill_all() -> usize {
    TASK_GENERATION.fetch_add(1, Ordering::Relaxed);
    let mut reg = registry().lock().unwrap();
    let mut count = 0;
    for entry in reg.iter_mut() {
        if entry.status != TaskStatus::Running {
            continue;
        }
        if let Some(pty) = entry.pty.as_mut() {
            pty.killed.store(true, Ordering::Relaxed);
            if pty.killer.kill().is_ok() {
                count += 1;
            }
        } else if let Some(tx) = entry.kill.take() {
            let _ = tx.send(());
            count += 1;
        }
    }
    reg.clear();
    count
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

/// Wait for a task result requested by the agent. A terminal result returned
/// here consumes the automatic lifecycle notification; a timeout or cancelled
/// future restores the task's previous notification state.
pub async fn wait_for_agent(id: u64, timeout: Duration) -> Result<(TaskSnapshot, bool), String> {
    let notify_agent = {
        let reg = registry().lock().unwrap();
        Arc::clone(
            &reg.iter()
                .find(|entry| entry.id == id)
                .ok_or_else(|| format!("error: no task with id {id}"))?
                .notify_agent,
        )
    };
    let suppression = NotificationSuppression::new(notify_agent);
    let result = wait(id, timeout).await?;
    if result.1 {
        return Ok(result);
    }
    suppression.consume();
    Ok(result)
}

struct NotificationSuppression {
    notify_agent: Arc<AtomicBool>,
    restore_to: Option<bool>,
}

impl NotificationSuppression {
    fn new(notify_agent: Arc<AtomicBool>) -> Self {
        let restore_to = notify_agent.swap(false, Ordering::AcqRel);
        Self {
            notify_agent,
            restore_to: Some(restore_to),
        }
    }

    fn consume(mut self) {
        self.restore_to = None;
    }
}

impl Drop for NotificationSuppression {
    fn drop(&mut self) {
        if let Some(restore_to) = self.restore_to {
            self.notify_agent.store(restore_to, Ordering::Release);
        }
    }
}

/// Wait without imposing a second timeout. Callers that need timeout or
/// cancellation semantics can race this future and then terminate the task.
pub async fn wait_until_finished(id: u64) -> Result<TaskSnapshot, String> {
    loop {
        let status = {
            let reg = registry().lock().unwrap();
            reg.iter()
                .find(|entry| entry.id == id)
                .map(|entry| entry.status)
                .ok_or_else(|| format!("error: no task with id {id}"))?
        };
        if status != TaskStatus::Running {
            return snapshot(id).ok_or_else(|| format!("error: no task with id {id}"));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Wait until a foreground command is promoted. Returns `false` if the process
/// finishes before promotion, so the caller can collect its normal result.
pub async fn wait_until_promoted(id: u64) -> Result<bool, String> {
    loop {
        let state = {
            let reg = registry().lock().unwrap();
            let entry = reg
                .iter()
                .find(|entry| entry.id == id)
                .ok_or_else(|| format!("error: no task with id {id}"))?;
            (entry.kind, entry.status)
        };
        if state.0 == TaskKind::Background {
            return Ok(true);
        }
        if state.1 != TaskStatus::Running {
            return Ok(false);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
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
        .filter(|entry| entry.kind == TaskKind::Background)
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
            kind: TaskKind::Background,
            origin: TaskOrigin::Restored,
            notify_agent: Arc::new(AtomicBool::new(false)),
            generation: current_generation(),
            call_id: None,
            name: t.name.clone(),
            command: t.command.clone(),
            status,
            exit_code: t.exit_code,
            started,
            finished: Some(now),
            output: t.output.clone(),
            stderr_output: String::new(),
            live_output: String::new(),
            max_output: MAX_TASK_OUTPUT,
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
        append_live_output(entry, chunk);
        let max = entry.max_output;
        cap_buffer(&mut entry.output, max);
    }
}

fn append_stderr(id: u64, chunk: &str) {
    let mut reg = registry().lock().unwrap();
    if let Some(entry) = reg.iter_mut().find(|e| e.id == id) {
        entry.stderr_output.push_str(chunk);
        append_live_output(entry, chunk);
        let max = entry.max_output;
        cap_buffer(&mut entry.stderr_output, max);
    }
}

fn append_live_output(entry: &mut TaskEntry, chunk: &str) {
    if entry.call_id.is_none() {
        return;
    }
    entry.live_output.push_str(chunk);
    cap_buffer(&mut entry.live_output, entry.max_output);
}

/// Append a raw PTY chunk to an interactive task's transcript buffer.
fn append_transcript(id: u64, chunk: &str) {
    let mut reg = registry().lock().unwrap();
    if let Some(pty) = reg
        .iter_mut()
        .find(|e| e.id == id)
        .and_then(|e| e.pty.as_mut())
    {
        pty.transcript.push_str(chunk);
        cap_buffer(&mut pty.transcript, MAX_TASK_OUTPUT);
    }
}

/// Keep `buf` under `max` by dropping the oldest half at a char boundary.
fn cap_buffer(buf: &mut String, max: usize) {
    if buf.len() > max {
        let mut cut = buf.len() / 2;
        while !buf.is_char_boundary(cut) {
            cut += 1;
        }
        buf.replace_range(..cut, "[earlier output dropped]\n");
    }
}

/// The full session transcript of an interactive task as plain text: raw PTY
/// output run through [`strip_ansi`]. `None` if the id is unknown or the task
/// isn't interactive.
pub fn transcript(id: u64) -> Option<String> {
    let reg = registry().lock().unwrap();
    reg.iter()
        .find(|e| e.id == id)
        .and_then(|e| e.pty.as_ref())
        .map(|pty| strip_ansi(&pty.transcript))
}

/// Strip terminal escape sequences and control characters from raw PTY output,
/// keeping printable text, newlines, and tabs. A carriage return not followed
/// by a newline restarts its line (so progress-bar redraws keep only the final
/// state).
pub fn strip_ansi(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut line_start = 0usize; // byte index in `out` where the current line begins
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.peek() {
                // CSI: `ESC [` then parameter/intermediate bytes until a final
                // byte in 0x40..=0x7e.
                Some('[') => {
                    chars.next();
                    for n in chars.by_ref() {
                        if ('\x40'..='\x7e').contains(&n) {
                            break;
                        }
                    }
                }
                // OSC: `ESC ]` until BEL or ST (`ESC \`).
                Some(']') => {
                    chars.next();
                    while let Some(n) = chars.next() {
                        if n == '\x07' {
                            break;
                        }
                        if n == '\x1b' {
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                // Two-character escape (charset selection, keypad modes, …).
                Some(_) => {
                    chars.next();
                }
                None => {}
            },
            '\n' => {
                out.push('\n');
                line_start = out.len();
            }
            '\r' => {
                // Part of a CRLF line ending — let the `\n` handle it.
                if chars.peek() != Some(&'\n') {
                    out.truncate(line_start);
                }
            }
            '\t' => out.push('\t'),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

fn finish(id: u64, status: TaskStatus, exit_code: Option<i32>) {
    let mut reg = registry().lock().unwrap();
    let Some(entry) = reg.iter_mut().find(|e| e.id == id) else {
        return;
    };
    if entry.status != TaskStatus::Running {
        return;
    }

    let old_status = entry.status;
    let finished = Instant::now();
    entry.status = status;
    entry.exit_code = exit_code;
    entry.finished = Some(finished);
    entry.kill = None;
    entry.stdin_tx = None;

    let event = lifecycle_event(entry, old_status, status, exit_code, finished);
    drop(reg);

    if let Some(event) = event {
        publish_event(event);
    }
}

fn lifecycle_event(
    entry: &TaskEntry,
    old_status: TaskStatus,
    new_status: TaskStatus,
    exit_code: Option<i32>,
    finished: Instant,
) -> Option<TaskLifecycleEvent> {
    if entry.kind != TaskKind::Background {
        return None;
    }
    let transcript = entry
        .pty
        .as_ref()
        .map(|pty| strip_ansi(&pty.transcript))
        .unwrap_or_default();
    Some(TaskLifecycleEvent {
        sequence: NEXT_EVENT_SEQUENCE.fetch_add(1, Ordering::Relaxed),
        generation: entry.generation,
        task_id: entry.id,
        origin: entry.origin,
        old_status,
        new_status,
        name: entry.name.clone(),
        command: entry.command.clone(),
        exit_code,
        elapsed: finished - entry.started,
        stdout_tail: tail_chars(&strip_ansi(&entry.output), 8_000),
        stderr_tail: tail_chars(&strip_ansi(&entry.stderr_output), 4_000),
        transcript_tail: tail_chars(&transcript, 8_000),
        notify_agent: Arc::clone(&entry.notify_agent),
    })
}

fn tail_chars(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_string();
    }
    text.chars().skip(count - max_chars).collect()
}


#[cfg(test)]
#[path = "task_tests.rs"]
mod tests;

#[cfg(test)]
pub use tests::promote_command_for_call;
