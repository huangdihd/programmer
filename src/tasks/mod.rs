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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

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
    /// Combined stdout+stderr, capped at [`MAX_TASK_OUTPUT`].
    output: String,
    /// Signals the reader task to kill the child. `None` once finished.
    kill: Option<tokio::sync::oneshot::Sender<()>>,
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
        output: e.output.clone(),
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
    cmd.stdin(std::process::Stdio::null())
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

/// Request termination of a running task. Returns an error if the id is
/// unknown or the task already finished.
pub fn kill(id: u64) -> Result<(), String> {
    let mut reg = registry().lock().unwrap();
    let entry = reg
        .iter_mut()
        .find(|e| e.id == id)
        .ok_or_else(|| format!("error: no task with id {id}"))?;
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
            let output = if e.output.chars().count() > MAX_PERSISTED_OUTPUT {
                let skip = e.output.chars().count() - MAX_PERSISTED_OUTPUT;
                e.output.chars().skip(skip).collect()
            } else {
                e.output.clone()
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
