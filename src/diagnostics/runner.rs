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

//! Running a checker to produce a fresh diagnostic snapshot.
//!
//! P1 supports the command backend only: launch the checker's command line in
//! the project shell, then parse its combined stdout+stderr. A non-zero exit is
//! normal (that's how most compilers signal "there are errors"), so only a
//! failure to *spawn* the process counts as a runner error. The LSP backend is
//! recognized by the profile but not yet driven here.

use std::path::Path;

use super::{parse_output, Checker, CheckerKind, Diagnostic};

/// Run one checker in `cwd` and return the diagnostics it reports.
///
/// `Err` means the checker itself couldn't be run (bad command, unsupported
/// backend) — distinct from "ran fine and found problems", which is `Ok` with a
/// non-empty list.
pub async fn run_checker(checker: &Checker, cwd: &Path) -> Result<Vec<Diagnostic>, String> {
    if checker.kind == CheckerKind::Lsp {
        return super::lsp::collect_lsp(checker, cwd).await;
    }

    // Resolve the parser before spawning so a broken profile fails fast without
    // running anything.
    let parser = checker.resolve_parser()?;

    let (program, flag) = crate::tools::shell();
    let mut cmd = tokio::process::Command::new(program);
    cmd.arg(flag)
        .arg(&checker.command)
        .current_dir(cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);

    // On Windows, give the child its own (windowless) console so it doesn't
    // reset the parent console's input mode, which would disable mouse capture.
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let output = cmd
        .output()
        .await
        .map_err(|e| format!("checker '{}': failed to run: {e}", checker.name))?;

    // Compilers split diagnostics between stdout (rustc/tsc JSON) and stderr
    // (gcc/clang human output), so parse both together.
    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    if !output.stderr.is_empty() {
        if !combined.is_empty() && !combined.ends_with('\n') {
            combined.push('\n');
        }
        combined.push_str(&String::from_utf8_lossy(&output.stderr));
    }

    Ok(parse_output(&parser, &combined))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::diagnostics::Severity;

    fn command_checker(cmd: &str, parser: &str) -> Checker {
        Checker {
            name: "test".into(),
            kind: CheckerKind::Command,
            command: cmd.into(),
            parser: parser.into(),
            pattern: None,
            run_on: vec![],
        }
    }

    #[tokio::test]
    async fn runs_command_and_parses_stderr() {
        // gcc-style diagnostics conventionally go to stderr.
        let checker =
            command_checker(r"printf 'src/a.c:12:5: error: boom\n' 1>&2; exit 1", "gnu");
        let diags = run_checker(&checker, Path::new(".")).await.unwrap();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].file, "src/a.c");
        assert_eq!(diags[0].line, 12);
        assert_eq!(diags[0].severity, Severity::Error);
    }

    #[tokio::test]
    async fn clean_run_yields_no_diagnostics() {
        let checker = command_checker("true", "gnu");
        let diags = run_checker(&checker, Path::new(".")).await.unwrap();
        assert!(diags.is_empty());
    }

    #[tokio::test]
    async fn lsp_checker_that_isnt_a_server_errors() {
        // `true` exits immediately with no LSP handshake, so the initialize
        // response never arrives and the backend reports a run failure rather
        // than pretending the project is clean.
        let mut checker = command_checker("true", "gnu");
        checker.kind = CheckerKind::Lsp;
        let err = run_checker(&checker, Path::new(".")).await.unwrap_err();
        assert!(err.contains("initialize"), "got: {err}");
    }
}
