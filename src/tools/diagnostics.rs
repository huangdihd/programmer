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

pub async fn run(_arguments: &str) -> String {
    let cwd = std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
    match diagnostics::collect(&cwd).await {
        Some(snapshot) => snapshot.render(),
        None => "No diagnostics profile is configured. Run /init or call \
                 configure_diagnostics to set one up."
            .to_string(),
    }
}
