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

//! Project diagnostics: the normalized error/warning model that powers the
//! IDE-style feedback loop.
//!
//! A [`Checker`] (from the per-project profile) is run — as a shell command in
//! P1 — and its output is parsed into a set of [`Diagnostic`]s. Two snapshots
//! are compared with [`diff`] to tell the agent which problems it *introduced*
//! and which it *resolved* after an edit. Nothing here talks to the model or
//! the UI; it is pure, testable logic that later phases wire into the app.

mod lsp;
mod parse;
mod profile;
mod runner;

pub use lsp::{shutdown_all as shutdown_lsp, status as lsp_status};
pub use parse::{parse_output, Parser};
pub use profile::{Checker, CheckerKind, DiagnosticsProfile, PROFILE_PATH};
pub use runner::run_checker;

use std::collections::HashSet;
use std::path::Path;

// ---------------------------------------------------------------------------
// Diagnostic
// ---------------------------------------------------------------------------

/// How serious a single diagnostic is. Ordered most-severe first so a sorted
/// list surfaces errors before warnings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Error,
    Warning,
    /// A linter finding (clippy/eslint/…), ranked below a compiler warning —
    /// the equivalent of a JetBrains "weak warning". Only produced by checkers
    /// marked `lint = true`; see [`crate::diagnostics::profile::Checker`].
    Lint,
    Info,
}

impl Severity {
    /// Map a checker's severity word (`error`, `warning`, `warn`, `note`, …) to
    /// a [`Severity`]. Unknown words fall back to [`Severity::Info`] so an
    /// unexpected label never silently reads as an error.
    pub fn parse(word: &str) -> Severity {
        match word.trim().to_ascii_lowercase().as_str() {
            "error" | "err" | "fatal" => Severity::Error,
            "warning" | "warn" => Severity::Warning,
            _ => Severity::Info,
        }
    }

    /// Lower-case label used in the summary handed to the model.
    pub fn label(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Lint => "lint",
            Severity::Info => "info",
        }
    }

    /// Downgrade a parsed severity to the lint tier for a `lint = true` checker:
    /// warnings/notes become lints, but genuine errors (e.g. `#[deny]` clippy
    /// lints) stay errors.
    pub fn as_lint(self) -> Severity {
        match self {
            Severity::Error => Severity::Error,
            _ => Severity::Lint,
        }
    }
}

/// One normalized problem reported by a checker. Two diagnostics are considered
/// the *same* problem when every field matches — that identity is what lets
/// [`diff`] tell a genuinely new error from one that was already present.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Diagnostic {
    /// File the problem is in, as reported (usually project-relative).
    pub file: String,
    /// 1-based line number, or 0 when the checker didn't give one.
    pub line: u32,
    /// 1-based column, when available.
    pub col: Option<u32>,
    pub severity: Severity,
    /// Diagnostic code (e.g. `E0308`, `TS2322`), when available.
    pub code: Option<String>,
    pub message: String,
}

impl Diagnostic {
    /// A compact one-line rendering used inside the model-facing summary, e.g.
    /// `src/foo.rs:42:5 error[E0308] mismatched types`.
    pub fn render(&self) -> String {
        let mut loc = self.file.clone();
        if self.line > 0 {
            loc.push_str(&format!(":{}", self.line));
            if let Some(col) = self.col {
                loc.push_str(&format!(":{col}"));
            }
        }
        let code = self
            .code
            .as_deref()
            .map(|c| format!("[{c}]"))
            .unwrap_or_default();
        format!("{loc} {}{code} {}", self.severity.label(), self.message)
    }
}

// ---------------------------------------------------------------------------
// Diff
// ---------------------------------------------------------------------------

/// The change between two diagnostic snapshots: what appeared and what cleared.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiagnosticDiff {
    pub added: Vec<Diagnostic>,
    pub removed: Vec<Diagnostic>,
}

impl DiagnosticDiff {
    /// No change either way.
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }

    /// A short, token-cheap summary for the model: counts plus each newly
    /// introduced diagnostic (most-severe first), and a count of resolved ones.
    /// Returns `None` when nothing changed.
    pub fn summary(&self) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        let mut out = format!(
            "Diagnostics changed: +{} new, -{} resolved.",
            self.added.len(),
            self.removed.len()
        );
        if !self.added.is_empty() {
            let mut added = self.added.clone();
            added.sort_by(|a, b| {
                a.severity
                    .cmp(&b.severity)
                    .then_with(|| a.file.cmp(&b.file))
                    .then_with(|| a.line.cmp(&b.line))
            });
            out.push_str("\nNEW:");
            for d in &added {
                out.push_str(&format!("\n  {}", d.render()));
            }
        }
        Some(out)
    }
}

// ---------------------------------------------------------------------------
// Snapshot collection
// ---------------------------------------------------------------------------

/// The result of running every checker in the project's profile once.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// All diagnostics reported, deduplicated across checkers.
    pub diagnostics: Vec<Diagnostic>,
    /// Per-checker (or profile) failures that prevented a clean run — surfaced
    /// to the model so a broken checker doesn't silently report "no errors".
    pub errors: Vec<String>,
}

/// Run the project's diagnostics profile and collect a full snapshot.
///
/// Returns `None` when the project has no profile (`/init` hasn't configured
/// one) — the caller then simply skips diagnostics. LSP checkers are recognized
/// but skipped until that backend lands.
pub async fn collect(cwd: &Path) -> Option<Snapshot> {
    let profile = match DiagnosticsProfile::load(cwd)? {
        Ok(p) => p,
        Err(e) => {
            return Some(Snapshot {
                diagnostics: Vec::new(),
                errors: vec![e],
            });
        }
    };

    let mut diagnostics = Vec::new();
    let mut errors = Vec::new();
    for checker in &profile.checkers {
        // `run_checker` dispatches to the command or LSP backend by kind.
        match run_checker(checker, cwd).await {
            Ok(mut ds) => diagnostics.append(&mut ds),
            Err(e) => errors.push(e),
        }
    }

    // Two checkers can surface the same problem; keep one of each.
    let mut seen = HashSet::new();
    diagnostics.retain(|d| seen.insert(d.clone()));

    Some(Snapshot { diagnostics, errors })
}

impl Snapshot {
    /// Render the full current diagnostic list for the `diagnostics` query tool:
    /// counts, then each diagnostic (errors first), then any checker failures.
    pub fn render(&self) -> String {
        if self.diagnostics.is_empty() && self.errors.is_empty() {
            return "No diagnostics — the project is clean.".to_string();
        }
        let errors = self
            .diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .count();
        let warnings = self
            .diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Warning)
            .count();
        let others = self.diagnostics.len() - errors - warnings;

        let mut out = format!("{errors} error(s), {warnings} warning(s)");
        if others > 0 {
            out.push_str(&format!(", {others} other"));
        }
        out.push('.');

        let mut sorted = self.diagnostics.clone();
        sorted.sort_by(|a, b| {
            a.severity
                .cmp(&b.severity)
                .then_with(|| a.file.cmp(&b.file))
                .then_with(|| a.line.cmp(&b.line))
        });
        for d in &sorted {
            out.push_str(&format!("\n  {}", d.render()));
        }
        for e in &self.errors {
            out.push_str(&format!("\n  (checker failed: {e})"));
        }
        out
    }
}

/// Compare two snapshots. `added` is everything in `new` that wasn't in `old`;
/// `removed` is everything in `old` that's gone from `new`. Order within each
/// bucket follows `new`/`old` respectively (callers sort as needed).
pub fn diff(old: &[Diagnostic], new: &[Diagnostic]) -> DiagnosticDiff {
    let old_set: HashSet<&Diagnostic> = old.iter().collect();
    let new_set: HashSet<&Diagnostic> = new.iter().collect();
    DiagnosticDiff {
        added: new
            .iter()
            .filter(|d| !old_set.contains(*d))
            .cloned()
            .collect(),
        removed: old
            .iter()
            .filter(|d| !new_set.contains(*d))
            .cloned()
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diag(file: &str, line: u32, sev: Severity, msg: &str) -> Diagnostic {
        Diagnostic {
            file: file.to_string(),
            line,
            col: None,
            severity: sev,
            code: None,
            message: msg.to_string(),
        }
    }

    #[test]
    fn severity_parsing_and_fallback() {
        assert_eq!(Severity::parse("error"), Severity::Error);
        assert_eq!(Severity::parse("  WARNING "), Severity::Warning);
        assert_eq!(Severity::parse("warn"), Severity::Warning);
        assert_eq!(Severity::parse("note"), Severity::Info);
        assert_eq!(Severity::parse("whatever"), Severity::Info);
    }

    #[test]
    fn lint_downgrade_keeps_errors() {
        // A lint checker's warnings/notes become lints; errors stay errors.
        assert_eq!(Severity::Warning.as_lint(), Severity::Lint);
        assert_eq!(Severity::Info.as_lint(), Severity::Lint);
        assert_eq!(Severity::Error.as_lint(), Severity::Error);
    }

    #[test]
    fn lint_ranks_below_warning_above_info() {
        // Ord drives sort order (most-severe first): Error < Warning < Lint < Info.
        assert!(Severity::Warning < Severity::Lint);
        assert!(Severity::Lint < Severity::Info);
    }

    #[test]
    fn diff_detects_added_and_removed() {
        let old = vec![
            diag("a.rs", 1, Severity::Error, "boom"),
            diag("b.rs", 2, Severity::Warning, "meh"),
        ];
        let new = vec![
            diag("b.rs", 2, Severity::Warning, "meh"), // unchanged
            diag("c.rs", 3, Severity::Error, "new one"),
        ];
        let d = diff(&old, &new);
        assert_eq!(d.added, vec![diag("c.rs", 3, Severity::Error, "new one")]);
        assert_eq!(d.removed, vec![diag("a.rs", 1, Severity::Error, "boom")]);
        assert!(!d.is_empty());
    }

    #[test]
    fn identical_snapshots_have_no_diff() {
        let snap = vec![diag("a.rs", 1, Severity::Error, "boom")];
        let d = diff(&snap, &snap);
        assert!(d.is_empty());
        assert!(d.summary().is_none());
    }

    #[test]
    fn summary_lists_new_errors_first() {
        let old = vec![];
        let new = vec![
            diag("z.rs", 9, Severity::Warning, "small"),
            diag("a.rs", 1, Severity::Error, "big"),
        ];
        let summary = diff(&old, &new).summary().unwrap();
        let err_pos = summary.find("big").unwrap();
        let warn_pos = summary.find("small").unwrap();
        assert!(err_pos < warn_pos, "errors should be listed before warnings");
        assert!(summary.contains("+2 new, -0 resolved"));
    }

    #[test]
    fn snapshot_render_clean_and_populated() {
        let clean = Snapshot::default();
        assert!(clean.render().contains("clean"));

        let snap = Snapshot {
            diagnostics: vec![
                diag("z.rs", 2, Severity::Warning, "w"),
                diag("a.rs", 1, Severity::Error, "e"),
            ],
            errors: vec!["cargo blew up".to_string()],
        };
        let out = snap.render();
        assert!(out.contains("1 error(s), 1 warning(s)"));
        // Errors sort before warnings.
        assert!(out.find("a.rs").unwrap() < out.find("z.rs").unwrap());
        assert!(out.contains("checker failed: cargo blew up"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn collect_none_without_profile_and_runs_with_one() {
        let dir = std::env::temp_dir().join(format!(
            "programmer-collect-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // No profile yet → None.
        assert!(collect(&dir).await.is_none());

        // Write a profile whose checker prints one gnu-style diagnostic.
        let prog = dir.join(PROFILE_PATH);
        std::fs::create_dir_all(prog.parent().unwrap()).unwrap();
        std::fs::write(
            &prog,
            "[[checkers]]\nname='t'\ncommand=\"printf 'a.c:1:1: error: boom\\n' 1>&2\"\nparser='gnu'\n",
        )
        .unwrap();

        let snap = collect(&dir).await.unwrap();
        assert_eq!(snap.diagnostics.len(), 1);
        assert_eq!(snap.diagnostics[0].message, "boom");
        assert!(snap.errors.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn render_includes_location_and_code() {
        let d = Diagnostic {
            file: "src/foo.rs".to_string(),
            line: 42,
            col: Some(5),
            severity: Severity::Error,
            code: Some("E0308".to_string()),
            message: "mismatched types".to_string(),
        };
        assert_eq!(d.render(), "src/foo.rs:42:5 error[E0308] mismatched types");
    }

    /// End-to-end test: create a real Cargo project with a deliberate error,
    /// configure the diagnostics profile, and verify `cargo check` + rustc-json
    /// parser catches it.
    #[cfg(unix)]
    #[tokio::test]
    async fn end_to_end_rustc_json_with_cargo_check() {
        let dir = std::env::temp_dir().join(format!(
            "programmer-e2e-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Create a minimal Cargo project.
        let output = std::process::Command::new("cargo")
            .args(["init", "--name", "testproj"])
            .current_dir(&dir)
            .output()
            .expect("cargo init should succeed");
        assert!(output.status.success(), "cargo init failed: {:?}", output);

        // Inject a deliberate compile error into src/main.rs.
        std::fs::write(
            dir.join("src/main.rs"),
            "fn main() {\n    let _ = undefined_variable;\n}\n",
        )
        .unwrap();

        // Write the diagnostics profile (same format as this project's own).
        let profile_dir = dir.join(".programmer");
        std::fs::create_dir_all(&profile_dir).unwrap();
        std::fs::write(
            dir.join(PROFILE_PATH),
            r#"[[checkers]]
name = "cargo-check"
kind = "command"
command = "cargo check --message-format=json"
parser = "rustc-json"
run_on = ["*.rs"]
"#,
        )
        .unwrap();

        // Load and validate the profile.
        let profile = DiagnosticsProfile::load(&dir)
            .expect("profile should exist")
            .expect("profile should parse");
        profile.validate().expect("profile should be valid");

        // Run the checker.
        let diags = run_checker(&profile.checkers[0], &dir)
            .await
            .expect("checker should run");

        assert_eq!(diags.len(), 1, "should find exactly one error");
        assert_eq!(diags[0].file, "src/main.rs");
        assert_eq!(diags[0].line, 2);
        assert_eq!(diags[0].severity, Severity::Error);
        assert_eq!(diags[0].code.as_deref(), Some("E0425"));
        assert!(
            diags[0].message.contains("undefined_variable"),
            "message should mention the undefined variable: {}",
            diags[0].message
        );

        // Full collect should also work.
        let snap = collect(&dir).await.expect("collect should succeed");
        assert_eq!(snap.diagnostics.len(), 1);
        assert!(snap.errors.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
