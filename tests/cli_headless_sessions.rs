// Copyright (C) 2026 huangdihd
// SPDX-License-Identifier: GPL-3.0-or-later

// Session/config isolation is deliberately unavailable in release binaries.
#![cfg(debug_assertions)]

use fs2::FileExt;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::{Command, Output};

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "programmer-headless-sessions-{}",
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

    fn run(&self, flags: &[&str]) -> Output {
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
            .args(["run", "hello", "--format", "json", "--config"])
            .arg(self.root.join("config/config.toml"))
            .args(flags)
            .output()
            .unwrap()
    }

    fn seed(&self) -> (String, PathBuf, Vec<u8>) {
        let id = uuid::Uuid::new_v4().to_string();
        let path = self.root.join("config/sessions").join(format!("{id}.json"));
        let bytes = serde_json::to_vec_pretty(&json!({
            "uuid": id,
            "first_message": "saved fixture",
            "created_at": 1,
            "updated_at": 2,
            "working_dir": self.root.join("project").canonicalize().unwrap(),
            "message_count": 0,
            "items": [],
            "history": ["saved fixture"],
            "current_model": "fixture-inherited/missing-model"
        }))
        .unwrap();
        std::fs::write(&path, &bytes).unwrap();
        (id, path, bytes)
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

fn assert_error(output: &Output, message: &str) -> Value {
    assert_code(output, 1);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(message), "{stderr}");
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["stop_reason"], "error");
    result
}

#[test]
fn conflicting_or_incomplete_session_options_are_usage_errors() {
    let fixture = Fixture::new();
    let id = uuid::Uuid::new_v4().to_string();
    for flags in [
        vec!["--resume", &id, "--continue"],
        vec!["--resume", "not-a-uuid"],
        vec!["--fork"],
        vec!["--no-save"],
        vec!["--resume", &id, "--no-save", "--save-session"],
    ] {
        assert_code(&fixture.run(&flags), 2);
    }
}

#[test]
fn missing_resume_and_continue_are_session_errors() {
    let fixture = Fixture::new();
    let id = uuid::Uuid::new_v4().to_string();
    assert_error(&fixture.run(&["--resume", &id]), "was not found");
    assert_error(&fixture.run(&["--continue"]), "no saved session");
    assert_code(&fixture.run(&["--resume", &id, "--fork", "--no-save"]), 2);
}

#[test]
fn save_session_reports_identity_even_when_provider_build_fails() {
    let fixture = Fixture::new();
    let result = assert_error(
        &fixture.run(&["--save-session", "--model", "fixture-explicit/missing"]),
        "unknown provider/model: fixture-explicit/missing",
    );
    uuid::Uuid::parse_str(result["session_id"].as_str().unwrap()).unwrap();
    // Provider construction fails before checkpointing: an identity does not
    // imply that a new snapshot has already been saved.
}

#[test]
fn resume_and_continue_inherit_model_and_report_session_identity() {
    let fixture = Fixture::new();
    let (id, _, _) = fixture.seed();
    for flags in [vec!["--resume", id.as_str()], vec!["--continue"]] {
        let result = assert_error(
            &fixture.run(&flags),
            "unknown provider/model: fixture-inherited/missing-model",
        );
        assert_eq!(result["session_id"], id);
    }
    let result = assert_error(
        &fixture.run(&["--resume", &id, "--model", "fixture-explicit/missing"]),
        "unknown provider/model: fixture-explicit/missing",
    );
    assert_eq!(result["session_id"], id);
}

#[test]
fn no_save_preserves_existing_snapshot_bytes() {
    let fixture = Fixture::new();
    let (id, path, before) = fixture.seed();
    let result = assert_error(
        &fixture.run(&["--resume", &id, "--no-save"]),
        "unknown provider/model: fixture-inherited/missing-model",
    );
    assert!(result["session_id"].is_null(), "{result}");
    assert_eq!(std::fs::read(path).unwrap(), before);
}

#[test]
fn locked_session_is_runtime_error_even_for_no_save() {
    let fixture = Fixture::new();
    let (id, path, before) = fixture.seed();
    let lock_path = fixture
        .root
        .join("config/sessions")
        .join(format!("{id}.lock"));
    let lock = std::fs::File::create(lock_path).unwrap();
    lock.try_lock_exclusive().unwrap();
    for flags in [
        vec!["--resume", id.as_str()],
        vec!["--resume", id.as_str(), "--no-save"],
    ] {
        assert_error(&fixture.run(&flags), "is in use");
    }
    assert_eq!(std::fs::read(path).unwrap(), before);
    FileExt::unlock(&lock).unwrap();
    assert_error(
        &fixture.run(&["--resume", &id]),
        "unknown provider/model: fixture-inherited/missing-model",
    );
}
