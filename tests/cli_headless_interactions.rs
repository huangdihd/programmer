// Copyright (C) 2026 huangdihd
// SPDX-License-Identifier: GPL-3.0-or-later

// Session/config isolation is deliberately unavailable in release binaries.
#![cfg(debug_assertions)]

use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::{Command, Output};

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "programmer-headless-interactions-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(root.join("config/sessions")).unwrap();
        std::fs::create_dir_all(root.join("project")).unwrap();
        std::fs::write(
            root.join("config/config.toml"),
            "active_security_profile = \"default\"\n[security_profiles.default]\nprotect_file_changes = false\n[security_profiles.default.sandbox]\nenabled = false\n",
        )
        .unwrap();
        Self { root }
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_programmer"));
        for (key, _) in std::env::vars_os() {
            if key
                .to_string_lossy()
                .to_ascii_uppercase()
                .starts_with("PROGRAMMER_")
            {
                command.env_remove(key);
            }
        }
        command
            .env("PROGRAMMER_TEST_CONFIG_DIR", self.root.join("config"))
            .current_dir(self.root.join("project"))
            .args(args)
            .arg("--config")
            .arg(self.root.join("config/config.toml"))
            .output()
            .unwrap()
    }

    fn seed_pending(&self) -> (String, PathBuf, Vec<u8>, Value) {
        let id = uuid::Uuid::new_v4().to_string();
        let pending_id = uuid::Uuid::new_v4().to_string();
        // Plan review needs no runner checkpoint, but uses the same durable
        // envelope and session snapshot format as a real suspended run.
        let pending = json!({
            "id": pending_id,
            "kind": "plan_review",
            "prompt": "Review the saved plan",
            "session_id": id,
            "resume_command": format!("programmer run --resume {id} --answer-id {pending_id} --answer '<ANSWER>'"),
            "options": [{"answer": "manual"}, {"answer": "auto"}, {"answer": "yolo"}, {"answer": "revise:<feedback>"}]
        });
        let path = self.root.join("config/sessions").join(format!("{id}.json"));
        let bytes = serde_json::to_vec_pretty(&json!({
            "uuid": id,
            "first_message": "saved pending fixture",
            "created_at": 1,
            "updated_at": 2,
            "working_dir": self.root.join("project").canonicalize().unwrap(),
            "message_count": 0,
            "items": [],
            "history": ["saved pending fixture"],
            "current_model": "fixture-interactions/missing-model",
            "headless_pending": pending
        }))
        .unwrap();
        std::fs::write(&path, &bytes).unwrap();
        (id, path, bytes, pending)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn assert_code(output: &Output, code: i32) {
    assert_eq!(
        output.status.code(),
        Some(code),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn answer_options_require_a_session_and_are_mutually_exclusive() {
    let fixture = Fixture::new();
    let id = uuid::Uuid::new_v4().to_string();
    for args in [
        vec!["run", "hello", "--answer", "auto"],
        vec!["run", "hello", "--answer-file", "answer.txt"],
        vec![
            "run",
            "hello",
            "--resume",
            &id,
            "--answer",
            "auto",
            "--answer-file",
            "answer.txt",
        ],
        vec!["run", "hello", "--resume", &id, "--answer-id", "pending-1"],
    ] {
        assert_code(&fixture.run(&args), 2);
    }
}

#[test]
fn manual_run_reaches_provider_construction_offline() {
    let fixture = Fixture::new();
    let output = fixture.run(&[
        "run",
        "hello",
        "--format",
        "json",
        "--work-mode",
        "manual",
        "--model",
        "fixture-interactions/missing-model",
    ]);
    assert_code(&output, 1);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unknown provider/model: fixture-interactions/missing-model"),
        "{stderr}"
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["stop_reason"], "error");
}

#[test]
fn manual_init_and_mcp_stdio_are_usage_errors() {
    let fixture = Fixture::new();
    for args in [
        vec!["init", "--work-mode", "manual"],
        vec!["mcp", "stdio", "--work-mode", "manual"],
    ] {
        assert_code(&fixture.run(&args), 2);
    }
}

#[test]
fn suspension_conflicts_with_no_save() {
    let fixture = Fixture::new();
    let id = uuid::Uuid::new_v4().to_string();
    assert_code(
        &fixture.run(&["run", "hello", "--resume", &id, "--suspend", "--no-save"]),
        2,
    );
}

#[test]
fn missing_invalid_or_mismatched_answers_preserve_pending_session() {
    let fixture = Fixture::new();
    let (id, path, before, pending) = fixture.seed_pending();
    for (flags, message) in [
        (
            vec!["hello"],
            "pending interaction requires --answer or --answer-file",
        ),
        (
            vec!["--answer", "not-a-plan-answer"],
            "invalid --answer for pending interaction",
        ),
        (
            vec!["--answer", "auto", "--answer-id", "wrong-id"],
            "--answer-id does not match pending interaction",
        ),
    ] {
        let mut args = vec!["run", "--format", "json", "--resume", id.as_str()];
        args.extend(flags);
        let output = fixture.run(&args);
        assert_code(&output, 2);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(message), "{stderr}");
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["pending"], pending);
        assert_eq!(result["session_id"], id);
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}
