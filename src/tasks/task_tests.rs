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

use super::*;

/// Promote the running foreground command associated with one tool call.
pub fn promote_command_for_call(call_id: &str) -> Result<u64, String> {
    let id = {
        let reg = registry().lock().unwrap();
        reg.iter()
            .find(|entry| {
                entry.kind == TaskKind::Command
                    && entry.status == TaskStatus::Running
                    && entry.call_id.as_deref() == Some(call_id)
            })
            .map(|entry| entry.id)
            .ok_or_else(|| format!("error: no running command for call {call_id}"))?
    };
    promote_command(id)?;
    Ok(id)
}

fn echo_cmd() -> &'static str {
    "echo task-out"
}

#[test]
fn sidebar_output_keeps_only_a_bounded_tail() {
    let text: String = (1..=13)
        .map(|line| format!("line {line} with extra text\n"))
        .collect();
    let preview = sidebar_output_text(&text, 10, 8);
    assert_eq!(preview.omitted_lines, 3);
    assert_eq!(preview.lines.len(), 10);
    assert_eq!(preview.lines.first().unwrap(), "line 4 w");
    assert_eq!(preview.lines.last().unwrap(), "line 13 ");
}

#[test]
fn lifecycle_event_is_emitted_only_for_agent_owned_background_tasks() {
    let started = Instant::now();
    let mut entry = TaskEntry {
        id: 41,
        kind: TaskKind::Background,
        origin: TaskOrigin::TaskTool,
        notification_policy: TaskNotificationPolicy::Agent,
        generation: 7,
        call_id: None,
        name: "tests".to_string(),
        command: "cargo test".to_string(),
        status: TaskStatus::Completed,
        exit_code: Some(0),
        started,
        finished: Some(started),
        output: "\u{1b}[32mok\u{1b}[0m\n".to_string(),
        stderr_output: String::new(),
        live_output: String::new(),
        max_output: MAX_TASK_OUTPUT,
        kill: None,
        stdin_tx: None,
        pty: None,
    };

    let event = lifecycle_event(
        &entry,
        TaskStatus::Running,
        TaskStatus::Completed,
        Some(0),
        started,
    )
    .expect("background task should notify");
    assert_eq!(event.task_id, 41);
    assert_eq!(event.generation, 7);
    assert_eq!(event.new_status, TaskStatus::Completed);
    assert_eq!(event.stdout_tail, "ok\n");

    entry.kind = TaskKind::Command;
    assert!(
        lifecycle_event(
            &entry,
            TaskStatus::Running,
            TaskStatus::Completed,
            Some(0),
            started,
        )
        .is_none()
    );
    entry.kind = TaskKind::Background;
    entry.notification_policy = TaskNotificationPolicy::Silent;
    assert!(
        lifecycle_event(
            &entry,
            TaskStatus::Running,
            TaskStatus::Killed,
            None,
            started,
        )
        .is_none()
    );
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
async fn command_task_is_hidden_and_closes_stdin() {
    let command = if cfg!(windows) { "findstr ." } else { "cat" };
    let id = spawn_command(command, None, Some("command-hidden")).expect("spawn command");
    let snap = wait_until_finished(id).await.expect("closed stdin exits");
    assert_eq!(snap.status, TaskStatus::Completed);
    assert!(
        !snapshot_all().iter().any(|task| task.id == id),
        "command tasks stay out of the public task list during stage one"
    );
    assert!(
        persist_all().iter().all(|task| task.id != id),
        "command output is already persisted in the conversation"
    );
}

#[tokio::test]
async fn command_live_output_is_keyed_by_call_id() {
    let command = if cfg!(windows) {
        "echo command-live && ping -n 3 127.0.0.1 > NUL"
    } else {
        "echo command-live && sleep 1"
    };
    let id = spawn_command(command, None, Some("command-live-id")).expect("spawn command");
    let mut seen = false;
    for _ in 0..60 {
        if command_live_output("command-live-id")
            .is_some_and(|text| text.contains("command-live"))
        {
            seen = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        seen,
        "live output should be readable while the command runs"
    );
    let _ = wait_until_finished(id).await.expect("command finishes");
    assert!(command_live_output("command-live-id").is_none());
}

#[tokio::test]
async fn promoting_command_exposes_the_same_running_task() {
    let command = if cfg!(windows) {
        "echo before-promote && ping -n 30 127.0.0.1 > NUL"
    } else {
        "echo before-promote && sleep 30"
    };
    let id = spawn_command(command, None, Some("promote-command")).expect("spawn command");

    promote_command(id).expect("promote");
    assert!(wait_until_promoted(id).await.expect("promotion state"));
    let snapshot = snapshot_all()
        .into_iter()
        .find(|task| task.id == id)
        .expect("promoted task is visible");
    assert_eq!(snapshot.status, TaskStatus::Running);
    assert!(task_ids().contains(&id));
    assert!(persist_all().iter().any(|task| task.id == id));
    assert!(command_live_output("promote-command").is_none());

    kill(id).expect("kill promoted task");
    let _ = wait_until_finished(id).await.expect("task stops");
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
    let long = if cfg!(windows) {
        "ping -n 60 127.0.0.1"
    } else {
        "sleep 60"
    };
    let id = spawn(long, None, None).expect("spawn");
    kill(id).expect("kill");
    let snap = wait_until_finished(id)
        .await
        .expect("the task should have finished after kill");
    assert_eq!(snap.status, TaskStatus::Killed);
}

#[tokio::test]
async fn pipe_stderr_is_kept_separate_from_stdout() {
    let command = if cfg!(windows) {
        "echo stdout-text & echo stderr-text 1>&2"
    } else {
        "echo stdout-text; echo stderr-text 1>&2"
    };
    let id = spawn(command, None, None).expect("spawn");
    let snap = wait_until_finished(id).await.expect("wait");
    assert!(snap.output.contains("stdout-text"), "stdout: {}", snap.output);
    assert!(
        snap.stderr.contains("stderr-text"),
        "stderr: {}",
        snap.stderr
    );
}

#[test]
fn persist_all_excludes_foreground_commands() {
    let mut entry = TaskEntry {
        id: 99,
        kind: TaskKind::Command,
        origin: TaskOrigin::Command,
        notification_policy: TaskNotificationPolicy::Agent,
        generation: 1,
        call_id: Some("persist-filter".into()),
        name: "test".into(),
        command: "test".into(),
        status: TaskStatus::Completed,
        exit_code: Some(0),
        started: Instant::now(),
        finished: Some(Instant::now()),
        output: String::new(),
        stderr_output: String::new(),
        live_output: String::new(),
        max_output: MAX_TASK_OUTPUT,
        kill: None,
        stdin_tx: None,
        pty: None,
    };
    {
        let mut reg = registry().lock().unwrap();
        reg.push(entry);
    }
    assert!(!persist_all().iter().any(|task| task.id == 99));
    // Clean up — the test helper leaked an entry.
    registry().lock().unwrap().retain(|e| e.id != 99);
}

#[tokio::test]
async fn closed_stdin_exits_process() {
    let command = if cfg!(windows) { "findstr ." } else { "cat" };
    let id = spawn(command, None, None).expect("spawn");
    // Close stdin immediately — the process should exit quickly.
    let (snap, _) = wait(id, Duration::from_secs(10)).await.expect("wait");
    assert_eq!(snap.status, TaskStatus::Completed);
}

#[cfg(unix)]
#[tokio::test]
async fn screen_text_returns_visible_grid() {
    let id = spawn_interactive("echo visible-text", None, None, 24, 80).expect("spawn");
    let _ = wait(id, Duration::from_secs(10)).await.expect("wait");
    tokio::time::sleep(Duration::from_millis(100)).await;
    let text = screen_snapshot(id).expect("interactive task has a screen").text;
    assert!(
        text.contains("visible-text"),
        "screen should contain visible-text, got: {text}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn screen_snapshot_clone_is_standalone() {
    let id = spawn_interactive("echo snapshot-test", None, None, 24, 80).expect("spawn");
    let _ = wait(id, Duration::from_secs(10)).await.expect("wait");
    tokio::time::sleep(Duration::from_millis(100)).await;
    let snap1 = screen_snapshot(id).expect("first snapshot");
    assert!(!snap1.text.is_empty());
    // The second snapshot is a fresh clone, not sharing the same screen.
    let snap2 = screen_snapshot(id).expect("second snapshot");
    assert_eq!(snap1.text, snap2.text);
    forget_command(id);
    assert!(screen_snapshot(id).is_err());
}

#[tokio::test]
async fn wait_times_out_on_running_task() {
    let long = if cfg!(windows) {
        "ping -n 30 127.0.0.1"
    } else {
        "sleep 30"
    };
    let id = spawn(long, None, None).expect("spawn");
    let (snap, still_running) = wait(id, Duration::from_millis(300)).await.expect("wait");
    assert!(still_running);
    assert_eq!(snap.status, TaskStatus::Running);
    let _ = kill(id);
}

#[test]
fn strip_ansi_removes_escapes_and_handles_cr() {
    // SGR colors and cursor movement disappear; text stays.
    assert_eq!(strip_ansi("\x1b[1;32mok\x1b[0m done"), "ok done");
    // OSC title sequences (BEL- and ST-terminated) disappear.
    assert_eq!(strip_ansi("\x1b]0;title\x07hi"), "hi");
    assert_eq!(strip_ansi("\x1b]0;title\x1b\\hi"), "hi");
    // CRLF is a plain newline; a lone CR restarts the line.
    assert_eq!(strip_ansi("one\r\ntwo"), "one\ntwo");
    assert_eq!(strip_ansi("50%\r100%\ndone"), "100%\ndone");
    // A CR restart only rewinds to the current line, not earlier ones.
    assert_eq!(strip_ansi("keep\nold\rnew"), "keep\nnew");
}

#[cfg(unix)]
#[tokio::test]
async fn interactive_task_records_transcript() {
    let id = spawn_interactive("printf 'tr-123\\n'", None, None, 24, 80).expect("spawn");
    let (_, still) = wait(id, Duration::from_secs(10)).await.expect("wait");
    assert!(!still);
    // Give the reader thread a moment to drain the PTY tail.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let text = transcript(id).expect("interactive task has a transcript");
    assert!(text.contains("tr-123"), "transcript: {text}");
    // Pipe tasks have no transcript.
    let pid = spawn("echo hi", None, None).expect("spawn");
    assert!(transcript(pid).is_none());
    let _ = wait(pid, Duration::from_secs(10)).await;
}
