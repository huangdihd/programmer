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
        // Parallel tests share a PID and macOS clocks are coarse.
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "programmer-headless-{}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("config")).unwrap();
        std::fs::create_dir_all(root.join("project")).unwrap();
        let config = r#"active_security_profile = "default"
vision_enabled = false

[[mcp_servers]]
name = "fixture-alpha"
command = "must-not-be-started"
[[mcp_servers]]
name = "fixture-beta"
command = "must-not-be-started"

[security_profiles.default]
protect_file_changes = false
[security_profiles.default.sandbox]
enabled = false
"#
        .to_string();
        std::fs::write(root.join("config/config.toml"), &config).unwrap();
        Self { root, config }
    }

    fn write(&self, name: &str, contents: &[u8]) {
        std::fs::write(self.root.join("project").join(name), contents).unwrap();
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_env(args, &[])
    }

    fn run_env(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
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
        let output = command
            .env("PROGRAMMER_TEST_CONFIG_DIR", self.root.join("config"))
            .envs(env.iter().copied())
            .current_dir(self.root.join("project"))
            .args(args)
            .output()
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(self.root.join("config/config.toml")).unwrap(),
            self.config,
            "session overrides must not rewrite the default config"
        );
        output
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn assert_code(output: &Output, expected: i32) {
    assert_eq!(
        output.status.code(),
        Some(expected),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_provider_error(output: &Output) {
    assert_code(output, 1);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unknown provider/model"), "{stderr}");
}

#[test]
fn explicit_config_missing_or_invalid_is_not_created_or_rewritten() {
    let fixture = Fixture::new();
    for command in ["run", "init", "diagnostics"] {
        let mut args = vec![command];
        if command == "run" {
            args.push("hello");
        }
        args.extend(["--config", "missing/config.toml"]);
        assert_code(&fixture.run(&args), 1);
        assert!(!fixture.root.join("project/missing").exists());

        let invalid = b"not valid TOML = [";
        fixture.write("invalid.toml", invalid);
        *args.last_mut().unwrap() = "invalid.toml";
        assert_code(&fixture.run(&args), 1);
        assert_eq!(
            std::fs::read(fixture.root.join("project/invalid.toml")).unwrap(),
            invalid
        );
    }
}

#[test]
fn explicit_config_remains_unchanged_and_environment_overrides_apply() {
    let fixture = Fixture::new();
    // Intentionally minimal: loading this file must not migrate/default-fill it.
    let config = b"vision_enabled = false\n";
    fixture.write("custom.toml", config);
    fixture.write("image.png", b"\x89PNG\r\n\x1a\n");
    let args = [
        "run",
        "hello",
        "--config",
        "custom.toml",
        "--model",
        "nonexistent/model",
        "--image",
        "image.png",
    ];
    assert_code(&fixture.run(&args), 2);
    assert_provider_error(&fixture.run_env(&args, &[("PROGRAMMER_VISION_ENABLED", "true")]));
    assert_eq!(
        std::fs::read(fixture.root.join("project/custom.toml")).unwrap(),
        config
    );
}

#[test]
fn image_requires_vision_and_conflicting_flags_are_usage_errors() {
    let fixture = Fixture::new();
    for flags in [
        vec!["--image", "missing.png"],
        vec!["--image", "missing.png", "--no-vision"],
        vec!["--vision", "--no-vision"],
        vec!["--mcp", "fixture-alpha", "--no-mcp"],
        vec![
            "--coauthor",
            "Example <example@example.com>",
            "--no-coauthor",
        ],
    ] {
        let mut args = vec!["run", "hello", "--model", "nonexistent/model"];
        args.extend(flags);
        assert_code(&fixture.run(&args), 2);
    }
}

#[test]
fn compaction_conflicts_are_offline_usage_errors_for_run_and_init() {
    let fixture = Fixture::new();
    for command in [vec!["run", "hello"], vec!["init"]] {
        for flag in ["--compact-tokens", "--mandatory-compact-tokens"] {
            for value in ["0", "42"] {
                let mut args = command.clone();
                args.extend(["--model", "nonexistent/model", flag, value, "--no-compact"]);
                let output = fixture.run(&args);
                assert_code(&output, 2);
                let stderr = String::from_utf8_lossy(&output.stderr);
                assert!(stderr.contains("cannot be used with"), "{stderr}");
                assert!(
                    stderr.contains(flag) && stderr.contains("--no-compact"),
                    "{stderr}"
                );
                assert!(!stderr.contains("unknown provider/model"), "{stderr}");
            }
        }
    }
}

#[test]
fn compaction_overrides_do_not_rewrite_configuration() {
    let fixture = Fixture::new();
    let config = b"auto_compact_tokens = 123\nmandatory_compact_tokens = 456\n";
    fixture.write("compaction.toml", config);
    for command in [vec!["run", "hello"], vec!["init"]] {
        for flags in [
            vec!["--compact-tokens", "0"],
            vec!["--mandatory-compact-tokens", "0"],
            vec!["--compact-tokens", "42", "--mandatory-compact-tokens", "84"],
            vec!["--no-compact"],
        ] {
            for explicit_config in [false, true] {
                let mut args = command.clone();
                args.extend(["--model", "nonexistent/model", "--no-mcp"]);
                if explicit_config {
                    args.extend(["--config", "compaction.toml"]);
                }
                args.extend(flags.iter().copied());
                assert_provider_error(&fixture.run(&args));
                assert_eq!(
                    std::fs::read(fixture.root.join("project/compaction.toml")).unwrap(),
                    config
                );
            }
        }
    }
}

#[test]
fn unknown_mcp_server_lists_available_names() {
    let fixture = Fixture::new();
    for command in ["run", "init"] {
        let mut args = vec![command];
        if command == "run" {
            args.push("hello");
        }
        args.extend(["--model", "nonexistent/model", "--mcp", "missing"]);
        let output = fixture.run(&args);
        assert_code(&output, 2);
        let stderr = String::from_utf8_lossy(&output.stderr);
        for expected in ["missing", "available", "fixture-alpha", "fixture-beta"] {
            assert!(stderr.contains(expected), "missing {expected}: {stderr}");
        }
    }
}

#[test]
fn repeatable_inputs_and_session_overrides_reach_provider_without_persistence() {
    let fixture = Fixture::new();
    fixture.write("extra.txt", b"additional instruction");
    fixture.write("soul.md", b"Be concise.");
    fixture.write("image.png", b"\x89PNG\r\n\x1a\n");
    let output = fixture.run(&[
        "run",
        "hello",
        "--model",
        "nonexistent/model",
        "--append-prompt-file",
        "extra.txt",
        "--append-prompt",
        "first",
        "--append-prompt",
        "second",
        "--append-prompt-file",
        "extra.txt",
        "--vision",
        "--image",
        "image.png",
        "--image",
        "image.png",
        "--no-memory",
        "--no-mcp",
        "--coauthor",
        "Example <example@example.com>",
        "--soul-file",
        "soul.md",
        "--allowed-tools",
        "read_*,grep",
        "--allowed-tools",
        "mcp__*",
        "--disallowed-tools",
        "command,write_file",
        "--disallowed-tools",
        "mcp__private__*",
    ]);
    assert_provider_error(&output);
    assert_provider_error(&fixture.run(&[
        "init",
        "--model",
        "nonexistent/model",
        "--no-vision",
        "--no-memory",
        "--no-coauthor",
        "--mcp",
        "fixture-alpha",
        "--mcp",
        "fixture-beta",
    ]));
}

#[test]
fn unreadable_prompt_soul_and_image_files_are_runtime_errors() {
    let fixture = Fixture::new();
    for flag in ["--append-prompt-file", "--soul-file", "--image"] {
        let output = fixture.run(&[
            "run",
            "hello",
            "--model",
            "nonexistent/model",
            "--vision",
            flag,
            "missing-file",
        ]);
        assert_code(&output, 1);
        assert!(!String::from_utf8_lossy(&output.stderr).contains("unknown provider/model"));
    }
}

#[test]
fn excessive_images_warn_on_stderr_and_keep_stdout_structured() {
    let fixture = Fixture::new();
    fixture.write("image.png", b"\x89PNG\r\n\x1a\n");
    let mut args = vec![
        "run",
        "hello",
        "--model",
        "nonexistent/model",
        "--vision",
        "--format",
        "json",
    ];
    for _ in 0..21 {
        args.extend(["--image", "image.png"]);
    }
    let output = fixture.run(&args);
    assert_provider_error(&output);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("warning") && stderr.contains("dropped 1"),
        "{stderr}"
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["stop_reason"], "error");
}

#[test]
fn structured_runtime_errors_include_execution_counters() {
    let fixture = Fixture::new();
    for format in ["json", "jsonl"] {
        let output = fixture.run(&[
            "run",
            "hello",
            "--model",
            "nonexistent/model",
            "--format",
            format,
        ]);
        assert_provider_error(&output);
        let text = String::from_utf8_lossy(&output.stdout);
        let result: Value = if format == "jsonl" {
            let events: Vec<Value> = text
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert_eq!(events.len(), 2);
            assert_eq!(events[0]["type"], "start");
            assert_eq!(events[1]["type"], "result");
            events[1]["result"].clone()
        } else {
            serde_json::from_str(&text).unwrap()
        };
        assert_eq!(result["steps"], 0);
        assert_eq!(result["tool_calls"], 0);
        assert!(result["duration_ms"].is_u64(), "{result}");
    }
}
