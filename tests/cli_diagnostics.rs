// Copyright (C) 2026 huangdihd
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// The config isolation hook is deliberately absent from release binaries.
#![cfg(debug_assertions)]

use serde_json::Value;
use std::path::PathBuf;
use std::process::{Command, Output};

struct Fixture {
    root: PathBuf,
    config: String,
}

impl Fixture {
    fn new() -> Self {
        // Parallel tests share a PID and macOS clocks are coarse, so add a
        // per-process counter to keep fixture roots distinct.
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let unique = format!(
            "programmer-cli-{}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(root.join("config")).unwrap();
        std::fs::create_dir_all(root.join("project/.programmer")).unwrap();
        // Modern, valid profiles prevent load-time migration from obscuring
        // accidental persistence of command-line overrides.
        let config = r#"active_security_profile = "default"

[security_profiles.default]
protect_file_changes = false
[security_profiles.default.sandbox]
enabled = false

[security_profiles.ci]
protect_file_changes = false
[security_profiles.ci.sandbox]
enabled = false
"#
        .to_string();
        std::fs::write(root.join("config/config.toml"), &config).unwrap();
        Self { root, config }
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_programmer"));
        // Do not inherit user provider/config overrides; no test calls a model.
        for (key, _) in std::env::vars_os() {
            if key
                .to_string_lossy()
                .to_ascii_uppercase()
                .starts_with("PROGRAMMER_")
            {
                command.env_remove(key);
            }
        }
        let output = command
            .env("PROGRAMMER_TEST_CONFIG_DIR", self.root.join("config"))
            .current_dir(self.root.join("project"))
            .args(args)
            .output()
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(self.root.join("config/config.toml")).unwrap(),
            self.config,
            "CLI overrides must not rewrite configuration"
        );
        output
    }

    fn checker(&self, command: &str) {
        std::fs::write(
            self.root.join("project/.programmer/diagnostics.toml"),
            format!("[[checkers]]\nname = \"fixture\"\ncommand = {command:?}\nparser = \"gnu\"\n"),
        )
        .unwrap();
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
fn diagnostics_json_reports_findings_and_sets_the_exit_status() {
    let fixture = Fixture::new();
    fixture.checker("echo src/main.rs:7:3: error: broken");
    let output = fixture.run(&["diagnostics", "--format", "json"]);
    assert_code(&output, 3);
    assert!(
        output.stderr.is_empty(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["configured"], true);
    assert_eq!(report["passed"], false);
    assert_eq!(report["summary"]["errors"], 1);
    assert_eq!(report["diagnostics"][0]["file"], "src/main.rs");
    assert_eq!(report["diagnostics"][0]["severity"], "error");
}

#[test]
fn diagnostics_missing_failed_and_passing_checkers_have_stable_codes() {
    let fixture = Fixture::new();
    let output = fixture.run(&["diagnostics", "--format", "json"]);
    assert_code(&output, 3);
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["configured"], false);

    // Nonzero compiler exit alone is not a checker failure; use a bad parser.
    std::fs::write(
        fixture.root.join("project/.programmer/diagnostics.toml"),
        "[[checkers]]\nname = \"broken\"\ncommand = \"echo clean\"\nparser = \"regex\"\npattern = \"(\"\n",
    )
    .unwrap();
    assert_code(&fixture.run(&["diagnostics", "--format", "json"]), 3);
    fixture.checker("echo clean");
    let output = fixture.run(&["diagnostics", "--format", "json"]);
    assert_code(&output, 0);
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["passed"], true);
}

#[test]
fn custom_validation_errors_are_usage_errors() {
    let fixture = Fixture::new();
    for args in [
        vec!["init", "--work-mode", "manual"],
        vec!["run", "hello", "--init", "--work-mode", "plan"],
        vec!["init", "--work-mode", "plan"],
        vec!["mcp", "stdio", "--work-mode", "manual"],
    ] {
        assert_code(&fixture.run(&args), 2);
    }
}

#[test]
fn unknown_security_profile_lists_available_profiles() {
    let fixture = Fixture::new();
    for args in [
        vec!["diagnostics", "--security-profile", "missing"],
        vec!["run", "hello", "--security-profile", "missing"],
        vec!["init", "--security-profile", "missing"],
    ] {
        let output = fixture.run(&args);
        assert_code(&output, 2);
        let stderr = String::from_utf8_lossy(&output.stderr);
        for name in ["missing", "default", "ci"] {
            assert!(stderr.contains(name), "missing {name}: {stderr}");
        }
    }
}

#[test]
fn invalid_provider_is_a_runtime_error_without_network_access() {
    let fixture = Fixture::new();
    let output = fixture.run(&["run", "hello", "--model", "nonexistent/model"]);
    assert_code(&output, 1);
    assert!(String::from_utf8_lossy(&output.stderr).contains("nonexistent"));
}

#[test]
fn all_security_overrides_are_session_only() {
    let fixture = Fixture::new();
    fixture.checker("echo clean");
    let output = fixture.run(&[
        "diagnostics",
        "--format",
        "json",
        "--security-profile",
        "ci",
        "--sandbox",
        "off",
        "--allow-read",
        ".",
        "--allow-read",
        ".programmer",
        "--allow-write",
        ".",
        "--allow-write",
        ".programmer",
        "--deny-read",
        "private",
        "--deny-read",
        "secrets",
        "--deny-env",
        "FIXTURE_SECRET_*",
        "--deny-env",
        "FIXTURE_TOKEN",
        "--no-protect-file-changes",
    ]);
    assert_code(&output, 0);
}

#[test]
fn runtime_error_has_one_structured_result_with_effective_security() {
    let fixture = Fixture::new();
    for format in ["json", "jsonl"] {
        let output = fixture.run(&[
            "run",
            "hello",
            "--model",
            "nonexistent/model",
            "--format",
            format,
            "--security-profile",
            "ci",
            "--sandbox",
            "off",
        ]);
        assert_code(&output, 1);
        let text = String::from_utf8_lossy(&output.stdout);
        let result: Value = if format == "jsonl" {
            let lines: Vec<_> = text.lines().collect();
            assert_eq!(lines.len(), 2);
            let start: Value = serde_json::from_str(lines[0]).unwrap();
            assert_eq!(start["type"], "start");
            assert_eq!(start["model"], "nonexistent/model");
            let event: Value = serde_json::from_str(lines[1]).unwrap();
            assert_eq!(event["type"], "result");
            event["result"].clone()
        } else {
            serde_json::from_str(&text).unwrap()
        };
        assert_eq!(result["schema_version"], 1);
        assert_eq!(result["stop_reason"], "error");
        assert_eq!(result["response"], "");
        assert_eq!(result["passed"], false);
        assert_eq!(result["work_mode"], "auto");
        assert_eq!(result["security"]["profile"], "ci");
        assert_eq!(result["security"]["sandbox"], "off");
        assert_eq!(result["security"]["overrides"], true);
    }
}
