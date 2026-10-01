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

//! The `diagnostics` tool: on demand, run the project's configured checkers and
//! return the current errors/warnings. Edits already trigger diagnostics
//! automatically, but this lets the model pull the *full* current list whenever
//! it wants to — e.g. to confirm a fix cleared everything, or to see problems
//! that predate its edits.

use std::path::Path;

use async_openai::types::responses::Tool;
use serde_json::json;

use super::function_tool;
use crate::diagnostics;

pub const NAME: &str = "diagnostics";

pub fn tool() -> Tool {
    function_tool(
        NAME,
        "Run this project's configured diagnostics checkers and return the \
         current list of errors and warnings (file, line, severity, message). \
         Use it to check the project's health or confirm a fix. Requires that \
         diagnostics have been set up (via /init or configure_diagnostics); if \
         not, it says so.",
        json!({}),
        &[],
    )
}

pub async fn run(_arguments: &str) -> Result<String, String> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
    run_in(
        &cwd,
        &Default::default(),
        &crate::cancel::CancellationToken::new(),
    )
    .await
}

pub(crate) async fn run_with_state(
    state: &std::sync::Arc<std::sync::Mutex<diagnostics::DiagnosticsState>>,
    cancel: &crate::cancel::CancellationToken,
) -> Result<String, String> {
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    run_in(&cwd, state, cancel).await
}

async fn run_in(
    cwd: &Path,
    state: &std::sync::Arc<std::sync::Mutex<diagnostics::DiagnosticsState>>,
    cancel: &crate::cancel::CancellationToken,
) -> Result<String, String> {
    let generation = state.lock().unwrap().begin_update();
    let snapshot = cancel.wait_or(diagnostics::collect(cwd, cancel)).await;
    if cancel.is_cancelled() {
        state.lock().unwrap().publish(generation, None);
        return Err("Diagnostics cancelled".to_string());
    }
    let snapshot = snapshot.flatten();
    state.lock().unwrap().publish(generation, snapshot.as_ref());
    Ok(match snapshot {
        Some(snapshot) => snapshot.render(),
        None => "No diagnostics profile is configured. Run /init or call configure_diagnostics to set one up.".to_string(),
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[tokio::test]
    async fn missing_failed_and_cancelled_diagnostics_never_report_clean() {
        let directory = std::path::PathBuf::from(".programmer")
            .join(format!("diagnostics-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(directory.join(".programmer")).unwrap();
        let state = Arc::new(Mutex::new(diagnostics::DiagnosticsState::default()));
        let cancel = crate::cancel::CancellationToken::new();
        let missing = run_in(&directory, &state, &cancel).await.unwrap();
        assert!(missing.contains("No diagnostics profile"));
        assert!(state.lock().unwrap().baseline.is_none());
        std::fs::write(directory.join(diagnostics::PROFILE_PATH), "invalid toml").unwrap();
        let failed = run_in(&directory, &state, &cancel).await.unwrap();
        assert!(failed.contains("checker failed"));
        assert!(!failed.contains("project is clean"));
        assert!(state.lock().unwrap().baseline.is_none());
        std::fs::write(
            directory.join(diagnostics::PROFILE_PATH),
            "[[checkers]]\nname = 'slow'\ncommand = 'sleep 5'\nparser = 'gnu'\n",
        )
        .unwrap();
        let cancellation = cancel.clone();
        let (result, ()) = tokio::join!(
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                run_in(&directory, &state, &cancel)
            ),
            async move {
                tokio::time::sleep(std::time::Duration::from_millis(30)).await;
                cancellation.cancel();
            }
        );
        assert!(result.unwrap().unwrap_err().contains("cancelled"));
        assert!(state.lock().unwrap().baseline.is_none());
    }

    #[tokio::test]
    async fn tool_clean_snapshot_replaces_sidebar_four_warnings_six_lints_once() {
        let directory = std::path::PathBuf::from(".programmer")
            .join(format!("diagnostics-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(directory.join(".programmer")).unwrap();
        std::fs::write(
            directory.join(diagnostics::PROFILE_PATH),
            "[[checkers]]\nname = 'count'\ncommand = 'echo run >> runs'\nparser = 'gnu'\n",
        )
        .unwrap();
        let old = (0..10)
            .map(|index| diagnostics::Diagnostic {
                file: "src/main.rs".into(),
                line: index + 1,
                col: None,
                severity: if index < 4 {
                    diagnostics::Severity::Warning
                } else {
                    diagnostics::Severity::Lint
                },
                code: None,
                message: "old finding".into(),
            })
            .collect();
        let state = Arc::new(Mutex::new(crate::runner::DiagnosticsState::default()));
        state.lock().unwrap().baseline = Some(old);
        let output = run_in(
            directory.as_path(),
            &state,
            &crate::cancel::CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(output.contains("project is clean"));
        assert_eq!(
            std::fs::read_to_string(directory.join("runs")).unwrap(),
            "run\n"
        );
        assert_eq!(state.lock().unwrap().baseline, Some(Vec::new()));
    }
}
