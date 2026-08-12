// Copyright (C) 2026 huangdihd
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

use serde_json::Value;
use std::process::Command;

#[test]
fn diagnostics_json_reports_findings_and_sets_the_exit_status() {
    let unique = format!(
        "programmer-cli-diagnostics-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let project = std::env::temp_dir().join(unique);
    let profile_dir = project.join(".programmer");
    std::fs::create_dir_all(&profile_dir).unwrap();
    let command = if cfg!(windows) {
        "echo src/main.rs:7:3: error: broken"
    } else {
        "printf 'src/main.rs:7:3: error: broken\\n'"
    };
    std::fs::write(
        profile_dir.join("diagnostics.toml"),
        format!(
            r#"
[[checkers]]
name = "fixture"
command = {command:?}
parser = "gnu"
"#
        ),
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_programmer"))
        .args([
            "diagnostics",
            "--format",
            "json",
            "--cwd",
            project.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
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

    std::fs::remove_dir_all(project).unwrap();
}
