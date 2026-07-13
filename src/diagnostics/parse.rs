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

//! Turning a checker's raw output into normalized [`Diagnostic`]s.
//!
//! A [`Parser`] is either one of a few built-in presets for common toolchains
//! or a user-supplied regex. The regex escape hatch is deliberate: it lets a
//! project wire up *any* compiler or linter from the profile without a code
//! change here, so nothing about a specific language is hard-coded into the
//! binary — presets are just conveniences.

use regex::Regex;
use serde_json::Value;

use super::{Diagnostic, Severity};

/// A resolved output parser, ready to turn text into diagnostics.
pub enum Parser {
    /// `cargo`/`rustc` JSON diagnostics (`--message-format=json`): one JSON
    /// object per line.
    RustcJson,
    /// `tsc` human output: `path(line,col): error TS1234: message`.
    Tsc,
    /// The classic GCC/Clang/eslint-compact shape:
    /// `path:line:col: severity: message` (column optional).
    Gnu,
    /// A caller-supplied regex with named groups: `file` (required), `line`,
    /// `col`, `severity`, `code`, `message`. Applied per line.
    Regex(Regex),
}

impl Parser {
    /// Resolve a preset name into a [`Parser`]. Returns `None` for an unknown
    /// name so the caller can surface a clear profile error.
    pub fn from_preset(name: &str) -> Option<Parser> {
        match name.trim().to_ascii_lowercase().as_str() {
            "rustc-json" | "cargo-json" | "rustc" | "cargo" => Some(Parser::RustcJson),
            "tsc" | "typescript" => Some(Parser::Tsc),
            "gnu" | "gcc" | "clang" | "eslint-compact" => Some(Parser::Gnu),
            _ => None,
        }
    }

    /// Build a regex parser from a pattern string, validating it up front.
    pub fn from_regex(pattern: &str) -> Result<Parser, String> {
        Regex::new(pattern)
            .map(Parser::Regex)
            .map_err(|e| format!("invalid diagnostics regex: {e}"))
    }
}

/// Parse a checker's combined stdout+stderr into diagnostics.
pub fn parse_output(parser: &Parser, output: &str) -> Vec<Diagnostic> {
    match parser {
        Parser::RustcJson => parse_rustc_json(output),
        Parser::Tsc => parse_tsc(output),
        Parser::Gnu => parse_gnu(output),
        Parser::Regex(re) => parse_regex(re, output),
    }
}

// ---------------------------------------------------------------------------
// rustc / cargo JSON
// ---------------------------------------------------------------------------

fn parse_rustc_json(output: &str) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if !line.starts_with('{') {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        // cargo wraps each compiler message; plain rustc emits the message
        // object directly. Accept both.
        let msg = match value.get("reason").and_then(Value::as_str) {
            Some("compiler-message") => value.get("message"),
            Some(_) => continue, // build-script / artifact lines, etc.
            None => Some(&value),
        };
        let Some(msg) = msg else { continue };
        if let Some(d) = diagnostic_from_rustc_message(msg) {
            diags.push(d);
        }
    }
    diags
}

fn diagnostic_from_rustc_message(msg: &Value) -> Option<Diagnostic> {
    let level = msg.get("level").and_then(Value::as_str).unwrap_or("");
    let severity = Severity::parse(level);
    // Summary lines like "aborting due to 2 previous errors" carry no span;
    // they'd only add noise to the diff, so skip anything without a location.
    let spans = msg.get("spans").and_then(Value::as_array)?;
    let span = spans
        .iter()
        .find(|s| s.get("is_primary").and_then(Value::as_bool) == Some(true))
        .or_else(|| spans.first())?;

    let file = span.get("file_name").and_then(Value::as_str)?.to_string();
    let line = span
        .get("line_start")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let col = span
        .get("column_start")
        .and_then(Value::as_u64)
        .map(|c| c as u32);
    let code = msg
        .get("code")
        .and_then(|c| c.get("code"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let message = msg
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    Some(Diagnostic {
        file,
        line,
        col,
        severity,
        code,
        message,
    })
}

// ---------------------------------------------------------------------------
// tsc
// ---------------------------------------------------------------------------

fn parse_tsc(output: &str) -> Vec<Diagnostic> {
    // e.g. `src/app.ts(12,5): error TS2322: Type 'x' is not assignable...`
    let re = Regex::new(
        r"(?m)^(?P<file>[^()\n]+)\((?P<line>\d+),(?P<col>\d+)\):\s+(?P<severity>error|warning)\s+(?P<code>TS\d+):\s+(?P<message>.*)$",
    )
    .expect("static tsc regex is valid");
    parse_regex(&re, output)
}

// ---------------------------------------------------------------------------
// GNU / gcc / clang / eslint-compact
// ---------------------------------------------------------------------------

fn parse_gnu(output: &str) -> Vec<Diagnostic> {
    // e.g. `src/foo.c:12:5: error: expected ';'` (column optional).
    let re = Regex::new(
        r"(?m)^(?P<file>[^:\n]+):(?P<line>\d+)(?::(?P<col>\d+))?:\s*(?P<severity>error|warning|note|fatal error):\s*(?P<message>.*)$",
    )
    .expect("static gnu regex is valid");
    parse_regex(&re, output)
}

// ---------------------------------------------------------------------------
// Generic regex
// ---------------------------------------------------------------------------

fn parse_regex(re: &Regex, output: &str) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    for caps in re.captures_iter(output) {
        let Some(file) = caps.name("file") else {
            continue; // file is the one mandatory field
        };
        let line = caps
            .name("line")
            .and_then(|m| m.as_str().parse::<u32>().ok())
            .unwrap_or(0);
        let col = caps
            .name("col")
            .and_then(|m| m.as_str().parse::<u32>().ok());
        let severity = caps
            .name("severity")
            .map(|m| Severity::parse(m.as_str()))
            .unwrap_or(Severity::Error);
        let code = caps
            .name("code")
            .map(|m| m.as_str().to_string())
            .filter(|s| !s.is_empty());
        let message = caps
            .name("message")
            .map(|m| m.as_str().trim().to_string())
            .unwrap_or_default();
        diags.push(Diagnostic {
            file: file.as_str().to_string(),
            line,
            col,
            severity,
            code,
            message,
        });
    }
    diags
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rustc_json_extracts_primary_span() {
        let line = r#"{"reason":"compiler-message","message":{"message":"mismatched types","code":{"code":"E0308"},"level":"error","spans":[{"file_name":"src/foo.rs","line_start":42,"column_start":5,"is_primary":true}]}}"#;
        let diags = parse_output(&Parser::RustcJson, line);
        assert_eq!(diags.len(), 1);
        let d = &diags[0];
        assert_eq!(d.file, "src/foo.rs");
        assert_eq!(d.line, 42);
        assert_eq!(d.col, Some(5));
        assert_eq!(d.severity, Severity::Error);
        assert_eq!(d.code.as_deref(), Some("E0308"));
        assert_eq!(d.message, "mismatched types");
    }

    #[test]
    fn rustc_json_skips_artifacts_and_spanless_summaries() {
        let output = concat!(
            r#"{"reason":"compiler-artifact","target":{"name":"x"}}"#,
            "\n",
            r#"{"reason":"compiler-message","message":{"message":"aborting due to previous error","level":"error","spans":[]}}"#,
            "\n",
            "not json at all",
        );
        assert!(parse_output(&Parser::RustcJson, output).is_empty());
    }

    #[test]
    fn rustc_json_prefers_primary_over_first_span() {
        let line = r#"{"reason":"compiler-message","message":{"message":"m","level":"warning","spans":[{"file_name":"a.rs","line_start":1,"column_start":1,"is_primary":false},{"file_name":"b.rs","line_start":9,"column_start":2,"is_primary":true}]}}"#;
        let d = &parse_output(&Parser::RustcJson, line)[0];
        assert_eq!(d.file, "b.rs");
        assert_eq!(d.line, 9);
        assert_eq!(d.severity, Severity::Warning);
    }

    #[test]
    fn tsc_preset_parses_line_and_code() {
        let output = "src/app.ts(12,5): error TS2322: Type 'x' is not assignable to type 'y'.";
        let d = &parse_output(&Parser::Tsc, output)[0];
        assert_eq!(d.file, "src/app.ts");
        assert_eq!(d.line, 12);
        assert_eq!(d.col, Some(5));
        assert_eq!(d.code.as_deref(), Some("TS2322"));
        assert_eq!(d.severity, Severity::Error);
    }

    #[test]
    fn gnu_preset_parses_with_optional_column() {
        let output = "src/foo.c:12:5: error: expected ';'\nsrc/bar.c:3: warning: unused variable";
        let diags = parse_output(&Parser::Gnu, output);
        assert_eq!(diags.len(), 2);
        assert_eq!(diags[0].col, Some(5));
        assert_eq!(diags[0].severity, Severity::Error);
        assert_eq!(diags[1].col, None);
        assert_eq!(diags[1].severity, Severity::Warning);
    }

    #[test]
    fn generic_regex_uses_named_groups() {
        let re = Parser::from_regex(
            r"(?m)^(?P<file>\S+):(?P<line>\d+): (?P<severity>\w+): (?P<message>.*)$",
        )
        .unwrap();
        let d = &parse_output(&re, "lib.py:7: error: undefined name 'x'")[0];
        assert_eq!(d.file, "lib.py");
        assert_eq!(d.line, 7);
        assert_eq!(d.severity, Severity::Error);
        assert_eq!(d.message, "undefined name 'x'");
    }

    #[test]
    fn invalid_regex_is_rejected() {
        assert!(Parser::from_regex(r"(?P<file>").is_err());
    }

    #[test]
    fn unknown_preset_is_none() {
        assert!(Parser::from_preset("cobol-lint").is_none());
    }
}
