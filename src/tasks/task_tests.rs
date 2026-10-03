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
pub fn promote_command_for_call(manager: &TaskManager, call_id: &str) -> Result<u64, String> {
    let id = {
        let reg = manager.state.registry.lock().unwrap();
        reg.iter()
            .find(|entry| {
                entry.kind == TaskKind::Command
                    && entry.status == TaskStatus::Running
                    && entry.call_id.as_deref() == Some(call_id)
            })
            .map(|entry| entry.id)
            .ok_or_else(|| format!("error: no running command for call {call_id}"))?
    };
    manager.promote_command(id)?;
    Ok(id)
}

fn echo_cmd() -> &'static str {
    "echo task-out"
}

fn stdin_reader_cmd() -> &'static str {
    if cfg!(windows) { "sort" } else { "cat" }
}

fn notification_enabled(manager: &TaskManager, id: u64) -> bool {
    manager
        .state
        .registry
        .lock()
        .unwrap()
        .iter()
        .find(|entry| entry.id == id)
        .expect("task exists")
        .notify_agent
        .load(Ordering::Acquire)
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
fn lifecycle_events_only_cover_background_tasks_and_can_be_consumed() {
    let manager = TaskManager::default();
    let started = Instant::now();
    let mut entry = TaskEntry {
        id: 41,
        kind: TaskKind::Background,
        origin: TaskOrigin::TaskTool,
        notify_agent: Arc::new(AtomicBool::new(true)),
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

    let event = manager
        .lifecycle_event(
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

    entry.notify_agent.store(false, Ordering::Release);
    assert!(!event.should_notify_agent());

    entry.kind = TaskKind::Command;
    assert!(
        manager
            .lifecycle_event(
                &entry,
                TaskStatus::Running,
                TaskStatus::Completed,
                Some(0),
                started,
            )
            .is_none()
    );
    entry.kind = TaskKind::Background;
    assert!(
        manager
            .lifecycle_event(
                &entry,
                TaskStatus::Running,
                TaskStatus::Killed,
                None,
                started,
            )
            .is_some()
    );
}

#[tokio::test]
async fn spawn_completes_and_captures_output() {
    let manager = TaskManager::default();
    let id = manager
        .spawn(echo_cmd(), None, Some("echo test"))
        .expect("spawn");
    let (snap, still_running) = manager
        .wait(id, Duration::from_secs(10))
        .await
        .expect("wait");
    assert!(!still_running, "echo should finish quickly");
    assert_eq!(snap.status, TaskStatus::Completed);
    assert!(snap.output.contains("task-out"), "output: {}", snap.output);
    assert_eq!(snap.name, "echo test");
}

#[tokio::test]
async fn command_task_is_hidden_and_closes_stdin() {
    let manager = TaskManager::default();
    let id = manager
        .spawn_command(stdin_reader_cmd(), None, Some("command-hidden"))
        .expect("spawn command");
    let snap = manager
        .wait_until_finished(id)
        .await
        .expect("closed stdin exits");
    assert_eq!(snap.status, TaskStatus::Completed);
    assert!(
        !manager.snapshot_all().iter().any(|task| task.id == id),
        "command tasks stay out of the public task list during stage one"
    );
    assert!(
        manager.persist_all().iter().all(|task| task.id != id),
        "command output is already persisted in the conversation"
    );
}

#[tokio::test]
async fn command_live_output_is_keyed_by_call_id() {
    let manager = TaskManager::default();
    let command = if cfg!(windows) {
        "echo command-live && ping -n 3 127.0.0.1 > NUL"
    } else {
        "echo command-live && sleep 1"
    };
    let id = manager
        .spawn_command(command, None, Some("command-live-id"))
        .expect("spawn command");
    let mut seen = false;
    for _ in 0..60 {
        if manager
            .command_live_output("command-live-id")
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
    let _ = manager
        .wait_until_finished(id)
        .await
        .expect("command finishes");
    assert!(manager.command_live_output("command-live-id").is_none());
}

#[tokio::test]
async fn promoting_command_exposes_the_same_running_task() {
    let manager = TaskManager::default();
    let command = if cfg!(windows) {
        "echo before-promote && ping -n 30 127.0.0.1 > NUL"
    } else {
        "echo before-promote && sleep 30"
    };
    let id = manager
        .spawn_command(command, None, Some("promote-command"))
        .expect("spawn command");

    manager.promote_command(id).expect("promote");
    assert!(
        manager
            .wait_until_promoted(id)
            .await
            .expect("promotion state")
    );
    let snapshot = manager
        .snapshot_all()
        .into_iter()
        .find(|task| task.id == id)
        .expect("promoted task is visible");
    assert_eq!(snapshot.status, TaskStatus::Running);
    assert!(manager.task_ids().contains(&id));
    assert!(manager.persist_all().iter().any(|task| task.id == id));
    assert!(manager.command_live_output("promote-command").is_none());

    manager.kill(id).expect("kill promoted task");
    let _ = manager.wait_until_finished(id).await.expect("task stops");
}

#[tokio::test]
async fn failing_command_is_marked_failed() {
    let manager = TaskManager::default();
    let id = manager.spawn("exit 3", None, None).expect("spawn");
    let (snap, _) = manager
        .wait(id, Duration::from_secs(10))
        .await
        .expect("wait");
    assert_eq!(snap.status, TaskStatus::Failed);
    assert_eq!(snap.exit_code, Some(3));
}

#[tokio::test]
async fn kill_terminates_a_running_task() {
    let manager = TaskManager::default();
    let long = if cfg!(windows) {
        "ping -n 60 127.0.0.1"
    } else {
        "sleep 60"
    };
    let id = manager.spawn(long, None, None).expect("spawn");
    manager.kill(id).expect("kill");
    let snap = manager
        .wait_until_finished(id)
        .await
        .expect("the task should have finished after kill");
    assert_eq!(snap.status, TaskStatus::Killed);
}

#[tokio::test]
async fn pipe_stderr_is_kept_separate_from_stdout() {
    let manager = TaskManager::default();
    let command = if cfg!(windows) {
        "echo stdout-text & echo stderr-text 1>&2"
    } else {
        "echo stdout-text; echo stderr-text 1>&2"
    };
    let id = manager.spawn(command, None, None).expect("spawn");
    let snap = manager.wait_until_finished(id).await.expect("wait");
    assert!(
        snap.output.contains("stdout-text"),
        "stdout: {}",
        snap.output
    );
    assert!(
        snap.stderr.contains("stderr-text"),
        "stderr: {}",
        snap.stderr
    );
}

#[test]
fn persist_all_excludes_foreground_commands() {
    let manager = TaskManager::default();
    let entry = TaskEntry {
        id: 99,
        kind: TaskKind::Command,
        origin: TaskOrigin::Command,
        notify_agent: Arc::new(AtomicBool::new(true)),
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
        let mut reg = manager.state.registry.lock().unwrap();
        reg.push(entry);
    }
    assert!(!manager.persist_all().iter().any(|task| task.id == 99));
    // Clean up — the test helper leaked an entry.
    manager
        .state
        .registry
        .lock()
        .unwrap()
        .retain(|e| e.id != 99);
}

#[tokio::test]
async fn closed_stdin_exits_process() {
    let manager = TaskManager::default();
    let id = manager
        .spawn(stdin_reader_cmd(), None, None)
        .expect("spawn");
    // Close stdin immediately — the process should exit quickly.
    manager.write_stdin(id, "", true).expect("close stdin");
    let (snap, _) = manager
        .wait(id, Duration::from_secs(10))
        .await
        .expect("wait");
    assert_eq!(snap.status, TaskStatus::Completed);
}

#[cfg(unix)]
#[tokio::test]
async fn screen_text_returns_visible_grid() {
    let manager = TaskManager::default();
    let id = manager
        .spawn_interactive("echo visible-text", None, None, 24, 80)
        .expect("spawn");
    let _ = manager
        .wait(id, Duration::from_secs(10))
        .await
        .expect("wait");
    tokio::time::sleep(Duration::from_millis(100)).await;
    let text = manager
        .screen_snapshot(id)
        .expect("interactive task has a screen")
        .text;
    assert!(
        text.contains("visible-text"),
        "screen should contain visible-text, got: {text}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn screen_snapshot_clone_is_standalone() {
    let manager = TaskManager::default();
    let id = manager
        .spawn_interactive("echo snapshot-test", None, None, 24, 80)
        .expect("spawn");
    let _ = manager
        .wait(id, Duration::from_secs(10))
        .await
        .expect("wait");
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut snap1 = manager.screen_snapshot(id).expect("first snapshot");
    assert!(!snap1.text.is_empty());
    let original_text = snap1.text.clone();
    snap1.text.push_str(" locally mutated");
    // A fresh snapshot must not share the first snapshot's owned text.
    let snap2 = manager.screen_snapshot(id).expect("second snapshot");
    assert_eq!(snap2.text, original_text);
    manager
        .state
        .registry
        .lock()
        .unwrap()
        .retain(|entry| entry.id != id);
}

#[tokio::test]
async fn wait_times_out_on_running_task() {
    let manager = TaskManager::default();
    let long = if cfg!(windows) {
        "ping -n 30 127.0.0.1"
    } else {
        "sleep 30"
    };
    let id = manager.spawn(long, None, None).expect("spawn");
    let (snap, still_running) = manager
        .wait(id, Duration::from_millis(300))
        .await
        .expect("wait");
    assert!(still_running);
    assert_eq!(snap.status, TaskStatus::Running);
    let _ = manager.kill(id);
}

#[tokio::test]
async fn agent_wait_consumes_completion_but_timeout_restores_notification() {
    let manager = TaskManager::default();
    let completed_id = manager
        .spawn(echo_cmd(), None, None)
        .expect("spawn completed task");
    let (_, still_running) = manager
        .wait_for_agent(completed_id, Duration::from_secs(10))
        .await
        .expect("wait for completed task");
    assert!(!still_running);
    assert!(!notification_enabled(&manager, completed_id));

    let long = if cfg!(windows) {
        "ping -n 30 127.0.0.1"
    } else {
        "sleep 30"
    };
    let running_id = manager.spawn(long, None, None).expect("spawn running task");
    let (_, still_running) = manager
        .wait_for_agent(running_id, Duration::from_millis(10))
        .await
        .expect("timed wait");
    assert!(still_running);
    assert!(notification_enabled(&manager, running_id));
    manager.kill(running_id).expect("clean up running task");
}

#[tokio::test]
async fn cancelled_agent_wait_restores_notification() {
    let manager = TaskManager::default();
    let long = if cfg!(windows) {
        "ping -n 30 127.0.0.1"
    } else {
        "sleep 30"
    };
    let id = manager.spawn(long, None, None).expect("spawn");
    let wait = manager.wait_for_agent(id, Duration::from_secs(30));
    assert!(
        tokio::time::timeout(Duration::from_millis(10), wait)
            .await
            .is_err()
    );
    assert!(notification_enabled(&manager, id));
    manager.kill(id).expect("clean up cancelled wait");
}

#[tokio::test]
async fn agent_kill_consumes_terminal_notification() {
    let manager = TaskManager::default();
    let long = if cfg!(windows) {
        "ping -n 30 127.0.0.1"
    } else {
        "sleep 30"
    };
    let id = manager.spawn(long, None, None).expect("spawn");
    manager.kill_for_agent(id).expect("agent kill");
    let snap = manager.wait_until_finished(id).await.expect("task stops");
    assert_eq!(snap.status, TaskStatus::Killed);
    assert!(!notification_enabled(&manager, id));
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
    let manager = TaskManager::default();
    let id = manager
        .spawn_interactive("printf 'tr-123\\n'", None, None, 24, 80)
        .expect("spawn");
    let (_, still) = manager
        .wait(id, Duration::from_secs(10))
        .await
        .expect("wait");
    assert!(!still);
    // Give the reader thread a moment to drain the PTY tail.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let text = manager
        .transcript(id)
        .expect("interactive task has a transcript");
    assert!(text.contains("tr-123"), "transcript: {text}");
    // Pipe tasks have no transcript.
    let pid = manager.spawn("echo hi", None, None).expect("spawn");
    assert!(manager.transcript(pid).is_none());
    let _ = manager.wait(pid, Duration::from_secs(10)).await;
}

#[tokio::test]
async fn managers_isolate_ids_events_generation_and_cleanup() {
    let first = TaskManager::default();
    let second = TaskManager::default();
    let (first_sender, mut first_events) = tokio::sync::mpsc::unbounded_channel();
    let (second_sender, mut second_events) = tokio::sync::mpsc::unbounded_channel();
    first.install_event_sink(first_sender);
    second.install_event_sink(second_sender);

    let first_id = first
        .spawn("echo first-manager", None, None)
        .expect("first task");
    let second_id = second
        .spawn("echo second-manager", None, None)
        .expect("second task");
    assert_eq!(first_id, 1);
    assert_eq!(second_id, 1);
    let first_clone = first.clone();
    let first_result = first_clone
        .wait_until_finished(first_id)
        .await
        .expect("clone sees task");
    let second_result = second
        .wait_until_finished(second_id)
        .await
        .expect("second completes");
    assert!(first_result.output.contains("first-manager"));
    assert!(!first_result.output.contains("second-manager"));
    assert!(second_result.output.contains("second-manager"));

    let first_event = tokio::time::timeout(Duration::from_secs(5), first_events.recv())
        .await
        .expect("first event arrives")
        .expect("first sink open");
    let second_event = tokio::time::timeout(Duration::from_secs(5), second_events.recv())
        .await
        .expect("second event arrives")
        .expect("second sink open");
    assert_eq!(first_event.sequence, 1);
    assert_eq!(second_event.sequence, 1);
    assert!(first_event.stdout_tail.contains("first-manager"));
    assert!(second_event.stdout_tail.contains("second-manager"));
    assert!(first_events.try_recv().is_err());
    assert!(second_events.try_recv().is_err());

    let second_generation = second.current_generation();
    first.kill_all();
    assert!(first_clone.snapshot_all().is_empty());
    assert!(second.snapshot(second_id).is_some());
    assert_eq!(second.current_generation(), second_generation);
    assert_eq!(first.current_generation(), second_generation + 1);
    assert_eq!(second.clear_finished(), 1);
}

#[tokio::test]
async fn completion_waiters_observe_drained_output_and_late_waits_return() {
    let manager = TaskManager::default();
    let command = if cfg!(windows) {
        "echo final-stdout & echo final-stderr 1>&2"
    } else {
        "printf final-stdout; printf final-stderr >&2"
    };
    let id = manager.spawn(command, None, None).expect("spawn");
    let clone = manager.clone();
    let (first, second) = tokio::join!(
        manager.wait_until_finished(id),
        clone.wait_until_finished(id),
    );
    for snapshot in [first.expect("first waiter"), second.expect("second waiter")] {
        assert_eq!(snapshot.status, TaskStatus::Completed);
        assert!(snapshot.output.contains("final-stdout"));
        assert!(snapshot.stderr.contains("final-stderr"));
    }
    let late = tokio::time::timeout(Duration::from_millis(100), manager.wait_until_finished(id))
        .await
        .expect("already-finished wait is immediate")
        .expect("task retained");
    assert_eq!(late.status, TaskStatus::Completed);
    assert!(manager.wait_until_finished(id + 1).await.is_err());
}

#[tokio::test]
async fn shutdown_is_repeatable_and_closes_admission_across_clones() {
    let manager = TaskManager::default();
    let clone = manager.clone();
    manager.spawn(stdin_reader_cmd(), None, None).unwrap();
    manager.shutdown(Duration::from_secs(5)).await.unwrap();
    clone.shutdown(Duration::ZERO).await.unwrap();
    assert!(clone.spawn(echo_cmd(), None, None).is_err());
    assert!(clone.spawn_command(echo_cmd(), None, None).is_err());
    assert!(
        clone
            .spawn_interactive(echo_cmd(), None, None, 24, 80)
            .is_err()
    );
    assert!(manager.state.lifecycle.lock().unwrap().workers.is_empty());
    let fresh = TaskManager::default();
    fresh.spawn(echo_cmd(), None, None).unwrap();
    fresh.shutdown(Duration::from_secs(5)).await.unwrap();
}

#[tokio::test]
async fn shutdown_joins_workers_even_after_kill_all_clears_registry() {
    let manager = TaskManager::default();
    manager.spawn(stdin_reader_cmd(), None, None).unwrap();
    manager.kill_all();
    assert!(manager.state.registry.lock().unwrap().is_empty());
    // kill_all remains reusable, unlike shutdown.
    manager.spawn(stdin_reader_cmd(), None, None).unwrap();
    manager.shutdown(Duration::from_secs(5)).await.unwrap();
    assert!(manager.state.lifecycle.lock().unwrap().workers.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_racing_spawn_does_not_lose_admitted_workers() {
    let manager = TaskManager::default();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let spawning = {
        let manager = manager.clone();
        let barrier = barrier.clone();
        tokio::spawn(async move {
            barrier.wait();
            for _ in 0..32 {
                if manager.spawn(stdin_reader_cmd(), None, None).is_err() {
                    break;
                }
            }
        })
    };
    barrier.wait();
    let clone = manager.clone();
    let (first, second) = tokio::join!(
        manager.shutdown(Duration::from_secs(5)),
        clone.shutdown(Duration::from_secs(5))
    );
    first.unwrap();
    second.unwrap();
    spawning.await.unwrap();
    assert!(manager.state.lifecycle.lock().unwrap().workers.is_empty());
    assert!(
        manager
            .state
            .registry
            .lock()
            .unwrap()
            .iter()
            .all(|entry| entry.status != TaskStatus::Running)
    );
}

#[tokio::test]
async fn shutdown_timeout_retains_worker_for_retry_and_reports_panics() {
    let manager = TaskManager::default();
    let (release, wait) = tokio::sync::oneshot::channel();
    let worker = tokio::spawn(async move {
        wait.await.unwrap();
        Ok(())
    });
    manager
        .state
        .lifecycle
        .lock()
        .unwrap()
        .workers
        .push(TaskWorker::Async(worker));
    assert!(
        manager
            .shutdown(Duration::ZERO)
            .await
            .unwrap_err()
            .contains("timed out")
    );
    assert_eq!(manager.state.lifecycle.lock().unwrap().workers.len(), 1);
    release.send(()).unwrap();
    manager.shutdown(Duration::from_secs(5)).await.unwrap();

    let manager = TaskManager::default();
    let worker = tokio::spawn(async {
        panic!("injected worker failure");
    });
    manager
        .state
        .lifecycle
        .lock()
        .unwrap()
        .workers
        .push(TaskWorker::Async(worker));
    assert!(
        manager
            .shutdown(Duration::from_secs(5))
            .await
            .unwrap_err()
            .contains("worker failed")
    );
    assert!(manager.shutdown(Duration::ZERO).await.is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn shutdown_kills_descendants_while_pipe_waiter_is_draining() {
    let manager = TaskManager::default();
    let id = manager
        .spawn("sleep 30 & echo shell-exited", None, None)
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    manager.shutdown(Duration::from_secs(5)).await.unwrap();
    let snapshot = manager.wait_until_finished(id).await.unwrap();
    assert!(snapshot.output.contains("shell-exited"));
    assert_eq!(snapshot.status, TaskStatus::Killed);
}

#[tokio::test]
async fn shutdown_joins_pty_waiter_and_reader() {
    let manager = TaskManager::default();
    let id = manager
        .spawn_interactive(stdin_reader_cmd(), None, None, 24, 80)
        .unwrap();
    manager.shutdown(Duration::from_secs(5)).await.unwrap();
    assert_eq!(
        manager.wait_until_finished(id).await.unwrap().status,
        TaskStatus::Killed
    );
    assert!(manager.state.lifecycle.lock().unwrap().workers.is_empty());
}

#[tokio::test]
async fn cancelled_shutdown_keeps_thread_ownership_until_retry() {
    let manager = TaskManager::default();
    let (release, wait) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        wait.recv().unwrap();
        Ok(())
    });
    manager
        .state
        .lifecycle
        .lock()
        .unwrap()
        .workers
        .push(TaskWorker::Thread(worker));
    let shutdown = tokio::time::timeout(
        Duration::from_millis(20),
        manager.shutdown(Duration::from_secs(30)),
    )
    .await;
    assert!(shutdown.is_err());
    assert!(manager.spawn(echo_cmd(), None, None).is_err());
    assert_eq!(manager.state.lifecycle.lock().unwrap().workers.len(), 1);
    release.send(()).unwrap();
    manager.shutdown(Duration::from_secs(5)).await.unwrap();
}
