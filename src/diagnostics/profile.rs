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

//! The per-project diagnostics profile (`.programmer/diagnostics.toml`).
//!
//! The profile is plain data the model writes during `/init`: a list of
//! checkers, each naming a command to run and how to parse its output. Keeping
//! it declarative — presets *or* a raw regex — is what makes the diagnostics
//! system language-agnostic without baking toolchains into the binary.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::Parser;

/// Project-relative location of the diagnostics profile, written by `/init`.
pub const PROFILE_PATH: &str = ".programmer/diagnostics.toml";

/// The whole profile: an ordered list of checkers.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct DiagnosticsProfile {
    #[serde(default)]
    pub checkers: Vec<Checker>,
}

/// How a checker produces its diagnostics.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CheckerKind {
    /// Run a shell command and parse its output.
    #[default]
    Command,
    /// Drive a language server (one-shot): initialize, open the `run_on` files,
    /// and collect `publishDiagnostics`. `command` is the server launch line
    /// (e.g. `rust-analyzer`, `clangd`, `typescript-language-server --stdio`).
    Lsp,
}

/// A single checker entry.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct Checker {
    /// Stable identifier shown in messages (e.g. `cargo`, `tsc`).
    pub name: String,
    #[serde(default)]
    pub kind: CheckerKind,
    /// Shell command line to run for the command backend, or the language
    /// server launch command for the LSP backend.
    #[serde(default)]
    pub command: String,
    /// Parser preset name (`rustc-json`, `tsc`, `gnu`) or the literal `regex`
    /// to use the [`Self::pattern`] field.
    #[serde(default)]
    pub parser: String,
    /// The regex used when `parser = "regex"`. Ignored otherwise.
    #[serde(default)]
    pub pattern: Option<String>,
    /// File globs whose edits should trigger this checker. Empty means "any
    /// edit". Consumed by the auto-run loop in a later phase.
    #[serde(default)]
    pub run_on: Vec<String>,
}

impl DiagnosticsProfile {
    /// Parse a profile from TOML text.
    pub fn from_toml(text: &str) -> Result<DiagnosticsProfile, String> {
        toml::from_str(text).map_err(|e| format!("invalid diagnostics profile: {e}"))
    }

    /// Serialize back to TOML (used when `/init` or the configure tool writes
    /// the profile out).
    pub fn to_toml(&self) -> Result<String, String> {
        toml::to_string_pretty(self).map_err(|e| e.to_string())
    }

    /// Absolute path of the profile within `cwd`.
    pub fn path_in(cwd: &Path) -> PathBuf {
        cwd.join(PROFILE_PATH)
    }

    /// Load and parse the project's profile, if one exists. `None` means no
    /// profile has been configured yet (`/init` hasn't run or wrote nothing).
    pub fn load(cwd: &Path) -> Option<Result<DiagnosticsProfile, String>> {
        let path = Self::path_in(cwd);
        if !path.exists() {
            return None;
        }
        Some(
            std::fs::read_to_string(&path)
                .map_err(|e| format!("reading {}: {e}", path.display()))
                .and_then(|text| Self::from_toml(&text)),
        )
    }

    /// Validate every checker up front: known kind wiring and a resolvable
    /// parser. Returns the first problem found, if any.
    pub fn validate(&self) -> Result<(), String> {
        for checker in &self.checkers {
            checker
                .resolve_parser()
                .map_err(|e| format!("checker '{}': {e}", checker.name))?;
            if checker.command.trim().is_empty() {
                return Err(format!("checker '{}': empty command", checker.name));
            }
        }
        Ok(())
    }
}

impl Checker {
    /// Resolve this checker's [`Parser`], turning a preset name or `regex` +
    /// pattern into something runnable. Errors carry a user-facing reason.
    pub fn resolve_parser(&self) -> Result<Parser, String> {
        let name = self.parser.trim();
        if name.eq_ignore_ascii_case("regex") {
            let pattern = self
                .pattern
                .as_deref()
                .filter(|p| !p.trim().is_empty())
                .ok_or("parser = \"regex\" requires a non-empty `pattern`")?;
            return Parser::from_regex(pattern);
        }
        if name.is_empty() {
            return Err("missing `parser` (a preset name or \"regex\")".to_string());
        }
        Parser::from_preset(name)
            .ok_or_else(|| format!("unknown parser preset '{name}' (and it isn't \"regex\")"))
    }

    /// Whether an edit to `path` should trigger this checker. An empty
    /// `run_on` matches everything; otherwise any glob matching wins.
    pub fn applies_to(&self, path: &str) -> bool {
        if self.run_on.is_empty() {
            return true;
        }
        self.run_on.iter().any(|glob| glob_matches(glob, path))
    }
}

/// Minimal glob match supporting `*` (any run of non-separator chars) and a
/// leading `**/` "any directory" prefix. Enough for the `run_on` patterns a
/// profile realistically uses (`*.rs`, `src/**/*.ts`, `Cargo.toml`).
fn glob_matches(glob: &str, path: &str) -> bool {
    // A bare `*.ext` should match regardless of directory, so try both the full
    // path and its final component.
    if glob_matches_exact(glob, path) {
        return true;
    }
    if !glob.contains('/') {
        if let Some(name) = path.rsplit('/').next() {
            return glob_matches_exact(glob, name);
        }
    }
    false
}

fn glob_matches_exact(glob: &str, text: &str) -> bool {
    // Translate the glob into a full-match regex.
    let mut re = String::from("(?s)^");
    let mut chars = glob.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' => {
                if chars.peek() == Some(&'*') {
                    chars.next();
                    // `**/` or `**` — match across directory separators.
                    if chars.peek() == Some(&'/') {
                        chars.next();
                    }
                    re.push_str(".*");
                } else {
                    re.push_str("[^/]*");
                }
            }
            '?' => re.push_str("[^/]"),
            c => re.push_str(&regex::escape(&c.to_string())),
        }
    }
    re.push('$');
    regex::Regex::new(&re)
        .map(|r| r.is_match(text))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_command_checker_with_preset() {
        let toml = r#"
            [[checkers]]
            name = "cargo"
            command = "cargo check --message-format=json"
            parser = "rustc-json"
            run_on = ["*.rs", "Cargo.toml"]
        "#;
        let profile = DiagnosticsProfile::from_toml(toml).unwrap();
        assert_eq!(profile.checkers.len(), 1);
        let c = &profile.checkers[0];
        assert_eq!(c.name, "cargo");
        assert_eq!(c.kind, CheckerKind::Command);
        assert!(c.resolve_parser().is_ok());
        profile.validate().unwrap();
    }

    #[test]
    fn regex_parser_requires_pattern() {
        let toml = r#"
            [[checkers]]
            name = "mytool"
            command = "mytool check"
            parser = "regex"
        "#;
        let profile = DiagnosticsProfile::from_toml(toml).unwrap();
        assert!(profile.validate().is_err());
    }

    #[test]
    fn regex_parser_with_pattern_resolves() {
        let toml = r#"
            [[checkers]]
            name = "mytool"
            command = "mytool check"
            parser = "regex"
            pattern = "^(?P<file>\\S+):(?P<line>\\d+): (?P<message>.*)$"
        "#;
        let profile = DiagnosticsProfile::from_toml(toml).unwrap();
        profile.validate().unwrap();
    }

    #[test]
    fn unknown_preset_fails_validation() {
        let toml = r#"
            [[checkers]]
            name = "x"
            command = "x"
            parser = "no-such-preset"
        "#;
        let err = DiagnosticsProfile::from_toml(toml).unwrap().validate().unwrap_err();
        assert!(err.contains("unknown parser preset"));
    }

    #[test]
    fn empty_command_fails_validation() {
        let toml = r#"
            [[checkers]]
            name = "x"
            parser = "gnu"
        "#;
        let err = DiagnosticsProfile::from_toml(toml).unwrap().validate().unwrap_err();
        assert!(err.contains("empty command"));
    }

    #[test]
    fn lsp_kind_parses() {
        let toml = r#"
            [[checkers]]
            name = "rust-analyzer"
            kind = "lsp"
            command = "rust-analyzer"
            parser = "gnu"
        "#;
        let profile = DiagnosticsProfile::from_toml(toml).unwrap();
        assert_eq!(profile.checkers[0].kind, CheckerKind::Lsp);
    }

    #[test]
    fn toml_round_trips() {
        let profile = DiagnosticsProfile {
            checkers: vec![Checker {
                name: "cargo".into(),
                kind: CheckerKind::Command,
                command: "cargo check --message-format=json".into(),
                parser: "rustc-json".into(),
                pattern: None,
                run_on: vec!["*.rs".into()],
            }],
        };
        let text = profile.to_toml().unwrap();
        assert_eq!(DiagnosticsProfile::from_toml(&text).unwrap(), profile);
    }

    #[test]
    fn run_on_globs() {
        let c = Checker {
            name: "t".into(),
            kind: CheckerKind::Command,
            command: "t".into(),
            parser: "gnu".into(),
            pattern: None,
            run_on: vec!["*.rs".into(), "src/**/*.ts".into()],
        };
        assert!(c.applies_to("src/main.rs"));
        assert!(c.applies_to("main.rs"));
        assert!(c.applies_to("src/ui/panel.ts"));
        assert!(!c.applies_to("README.md"));
        assert!(!c.applies_to("docs/notes.ts")); // not under src/
    }

    #[test]
    fn empty_run_on_matches_everything() {
        let c = Checker {
            name: "t".into(),
            kind: CheckerKind::Command,
            command: "t".into(),
            parser: "gnu".into(),
            pattern: None,
            run_on: vec![],
        };
        assert!(c.applies_to("anything.xyz"));
    }
}
