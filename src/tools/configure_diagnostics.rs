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

//! The `configure_diagnostics` tool: validate a proposed diagnostics profile,
//! test-run its checkers, and — only if everything holds up — write it to
//! `.programmer/diagnostics.toml`. `/init` uses this so the profile that lands
//! on disk is known-good rather than a guess the agent never exercised.

use std::path::Path;
use std::time::Duration;

use async_openai::types::responses::Tool;
use serde::Deserialize;
use serde_json::json;

use super::function_tool;
use crate::diagnostics::{run_checker, DiagnosticsProfile, PROFILE_PATH};

pub const NAME: &str = "configure_diagnostics";

/// How long a single checker may run during verification before it's assumed to
/// be a long-running/watch command the one-shot backend can't use.
const VERIFY_TIMEOUT: Duration = Duration::from_secs(180);

pub fn tool() -> Tool {
    function_tool(
        NAME,
        "Configure this project's diagnostics profile (how errors/warnings are \
         detected after edits). Provide the full profile as TOML. The profile is \
         validated and every checker is test-run once; if anything fails to run \
         the profile is NOT saved and the error is returned so you can fix it. On \
         success it is written to `.programmer/diagnostics.toml`.\n\n\
         Each checker is a `[[checkers]]` table with: `name`; optional `kind` \
         (`command`, the default, or `lsp`); `command`; `parser` (one of the \
         presets `rustc-json`, `tsc`, `gnu`, or the literal `regex`); optional \
         `pattern` (required when parser is `regex`; a regex with named groups \
         `file`, `line`, `col`, `severity`, `code`, `message`); and optional \
         `run_on` (file globs whose edits trigger this checker; empty = always).\n\n\
         For `kind = \"command\"` (default): `command` is a one-shot shell \
         command — NOT a watch/dev-server — and `parser` handles its output. \
         For `kind = \"lsp\"`: `command` launches a language server over stdio \
         (e.g. `rust-analyzer`, `clangd`, `typescript-language-server --stdio`); \
         `parser` is ignored. LSP is more accurate but re-initializes each run, \
         so it is slower — prefer a command checker unless the project needs it.",
        json!({
            "profile_toml": {
                "type": "string",
                "description": "The complete diagnostics profile as TOML, e.g. \
                    a series of [[checkers]] tables."
            }
        }),
        &["profile_toml"],
    )
}

#[derive(Deserialize)]
struct Args {
    profile_toml: String,
}

pub async fn run(arguments: &str) -> String {
    let cwd = std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
    run_in(arguments, &cwd).await
}

/// The tool body, parameterized on the working directory so tests can target a
/// temp dir instead of writing into the real project.
async fn run_in(arguments: &str, cwd: &Path) -> String {
    let args: Args = match serde_json::from_str(arguments) {
        Ok(args) => args,
        Err(error) => return format!("error: invalid arguments: {error}"),
    };

    let profile = match DiagnosticsProfile::from_toml(&args.profile_toml) {
        Ok(p) => p,
        Err(e) => return format!("error: {e}\nThe profile was NOT saved."),
    };
    if let Err(e) = profile.validate() {
        return format!("error: {e}\nThe profile was NOT saved.");
    }
    if profile.checkers.is_empty() {
        return "error: the profile has no [[checkers]]. Add at least one, or \
                skip diagnostics setup entirely."
            .to_string();
    }

    // Test-run each checker so a bad command or parser is caught now, not on
    // the first real edit. A checker that runs but finds nothing is fine.
    let mut report_lines = Vec::new();
    for checker in &profile.checkers {
        match tokio::time::timeout(VERIFY_TIMEOUT, run_checker(checker, cwd)).await {
            Ok(Ok(diags)) => {
                report_lines.push(format!(
                    "  ✓ {} — ran ok, parsed {} diagnostic(s)",
                    checker.name,
                    diags.len()
                ));
            }
            Ok(Err(e)) => {
                return format!(
                    "error: checker '{}' failed to run: {e}\nThe profile was NOT \
                     saved — fix the command or parser and try again.",
                    checker.name
                );
            }
            Err(_) => {
                return format!(
                    "error: checker '{}' didn't finish within {}s. The command \
                     backend expects a one-shot checker (e.g. `cargo check`, \
                     `tsc --noEmit`), not a watch or dev-server. The profile was \
                     NOT saved.",
                    checker.name,
                    VERIFY_TIMEOUT.as_secs()
                );
            }
        }
    }

    // Everything ran — persist it.
    let toml = match profile.to_toml() {
        Ok(t) => t,
        Err(e) => return format!("error: could not serialize profile: {e}"),
    };
    let path = cwd.join(PROFILE_PATH);
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return format!("error: could not create {}: {e}", parent.display());
        }
    }
    if let Err(e) = std::fs::write(&path, toml) {
        return format!("error: could not write {}: {e}", path.display());
    }

    format!(
        "Diagnostics profile saved to {PROFILE_PATH} with {} checker(s):\n{}",
        profile.checkers.len(),
        report_lines.join("\n")
    )
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A unique scratch directory for one test; removed at the end.
    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "programmer-configdiag-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn rejects_invalid_toml_without_writing() {
        let dir = temp_dir("badtoml");
        let out = run_in(r#"{"profile_toml":"this is not = valid = toml"}"#, &dir).await;
        assert!(out.starts_with("error:"), "got: {out}");
        assert!(out.contains("NOT saved"));
        assert!(!dir.join(PROFILE_PATH).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn rejects_unknown_parser() {
        let dir = temp_dir("badparser");
        let args = json!({
            "profile_toml": "[[checkers]]\nname='x'\ncommand='true'\nparser='bogus'\n"
        })
        .to_string();
        let out = run_in(&args, &dir).await;
        assert!(out.contains("unknown parser preset"), "got: {out}");
        assert!(!dir.join(PROFILE_PATH).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn valid_profile_is_verified_and_written() {
        let dir = temp_dir("ok");
        // `true` runs cleanly and parses zero diagnostics — a good checker.
        let args = json!({
            "profile_toml":
                "[[checkers]]\nname='noop'\ncommand='true'\nparser='gnu'\nrun_on=['*.rs']\n"
        })
        .to_string();
        let out = run_in(&args, &dir).await;
        assert!(out.contains("saved"), "got: {out}");
        assert!(out.contains("ran ok, parsed 0"), "got: {out}");
        let written = dir.join(PROFILE_PATH);
        assert!(written.exists());
        // The saved profile round-trips back to something loadable.
        let reloaded = DiagnosticsProfile::load(&dir).unwrap().unwrap();
        assert_eq!(reloaded.checkers[0].name, "noop");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
