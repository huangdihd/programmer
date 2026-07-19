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

//! Standalone helper functions and constants that don't depend on `App`.

use async_openai::types::responses::{
    InputContent, InputItem,
    MessageItem as ApiMessageItem,
};
use crate::response::message_item::MessageItem;

// ---------------------------------------------------------------------------
// PROJECT.md overview reminder
// ---------------------------------------------------------------------------

/// Whether the project's diagnostics profile declares at least one LSP checker.
pub(crate) fn lsp_checker_configured() -> bool {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::Path::new(".").to_path_buf());
    matches!(
        crate::diagnostics::DiagnosticsProfile::load(&cwd),
        Some(Ok(profile))
            if profile
                .checkers
                .iter()
                .any(|c| c.kind == crate::diagnostics::CheckerKind::Lsp)
    )
}

/// The hidden developer prompt that drives the `/init` flow.
pub(crate) fn init_prompt() -> String {
    "Initialize this project for our future work together. Do the following, in order:\n\
     \n\
     1. Explore the repository to understand it: read the README and any build \
     manifests (Cargo.toml, package.json, pyproject.toml, go.mod, etc.), and skim \
     the main source directories to learn the architecture, entry points, and \
     conventions. Ground everything in what you actually read — do not invent.\n\
     \n\
     2. Write a concise `PROGRAMMER.md` at the repository root capturing your \
     understanding: a one-paragraph overview, the tech stack, how to build / test / \
     run, the layout of key directories, and any notable conventions or gotchas. \
     Keep it tight and factual — it is a map for future sessions, not marketing.\n\
     \n\
     3. Set up diagnostics so edits get IDE-style error feedback. Determine how \
     this project surfaces compile/lint errors and call `configure_diagnostics` \
     with a profile of one-shot checker commands. Common cases: Rust → \
     `cargo check --message-format=json` with parser `rustc-json`; TypeScript → \
     `tsc --noEmit` with parser `tsc`; C/C++/others that print \
     `file:line:col: severity: message` → parser `gnu`; anything else → parser \
     `regex` with a `pattern` you write. Prefer commands that terminate (NOT \
     watch/dev-servers). A language server may be used instead via \
     `kind = \"lsp\"` with `command` set to its launch line (e.g. `clangd`), but \
     it re-initializes each run and is slower, so favour a command checker unless \
     there's a clear reason. The tool test-runs each checker and refuses to save \
     a profile that doesn't work, so iterate until it saves. If you genuinely \
     can't find a suitable checker, note that in PROGRAMMER.md and skip this \
     step.\n\
     \n\
     4. If the project has a linter distinct from its compiler (Rust → \
     `cargo clippy --message-format=json`; JS/TS → eslint; Python → ruff; Go → \
     golangci-lint; etc.), add it as an additional checker with `lint = true`. \
     Its findings then show as a lower \"lint\" tier alongside — but below — real \
     errors and warnings, IDE-style. Pick whatever the project actually uses; \
     skip if there's no separate linter.\n\
     \n\
     When done, briefly summarize what you set up."
        .to_string()
}

// ---------------------------------------------------------------------------
// Response parsing helpers
// ---------------------------------------------------------------------------

/// Extract the text of the first user message from a list of items.
pub(crate) fn first_user_text(items: &[MessageItem]) -> Option<String> {
    items.iter().find_map(|item| match item {
        MessageItem::Input(input) => extract_input_text(input),
        _ => None,
    })
}

pub(crate) fn extract_input_text(input: &InputItem) -> Option<String> {
    use async_openai::types::responses::Item;

    match input {
        InputItem::Item(Item::Message(ApiMessageItem::Input(input_msg))) => {
            input_msg.content.iter().find_map(|c| match c {
                InputContent::InputText(t) => Some(t.text.clone()),
                _ => None,
            })
        },
        InputItem::EasyMessage(msg) => match &msg.content {
            async_openai::types::responses::EasyInputContent::Text(t) => Some(t.clone()),
            async_openai::types::responses::EasyInputContent::ContentList(parts) => {
                parts.iter().find_map(|c| match c {
                    InputContent::InputText(t) => Some(t.text.clone()),
                    _ => None,
                })
            }
        },
        _ => None,
    }
}
