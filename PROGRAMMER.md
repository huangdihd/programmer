# PROGRAMMER.md — project map for the coding agent

## Overview

**programmer** is a terminal-based AI coding agent TUI written in Rust. It
connects to OpenAI-compatible APIs, streams responses, and gives the model
seven tools (command, read_file, write_file, edit_file, grep, blob, ask_user)
plus a `configure_diagnostics` tool. The TUI is built with Ratatui and
crossterm. The binary is a single crate at the repo root.

## Tech stack

- **Language:** Rust (edition 2024, MSRV: latest stable)
- **Async runtime:** tokio (full features)
- **TUI:** ratatui 0.30.2 + crossterm 0.29.0
- **API client:** async-openai 0.41.1 (Responses API)
- **Config:** `config` crate (TOML file + env override with `Programmer` prefix)
- **Error handling:** color-eyre
- **Serialization:** serde + serde_json + toml
- **Markdown rendering:** ratatui-markdown (forked, with syntax-highlight all langs)

## Build / test / run

```sh
# Build
cargo build              # debug
cargo build --release    # release (LTO, stripped, opt-level=s)

# Run
cargo run

# Run with flags
cargo run -- --resume <uuid>
cargo run -- --providers

# Test
cargo test

# Check (fast, no codegen)
cargo check
```

The binary runs a fullscreen TUI. On exit it prints a resume hint:
`Session saved. Resume with: programmer --resume <uuid>`.

## Key directories

```
.programmer/          # Per-project diagnostics profile (diagnostics.toml)
src/
├── main.rs           # Entry point: arg parsing, terminal init, config load, App::run
├── app.rs            # Core event loop, stream management, tool approval queue
├── config/
│   └── programmer_config.rs  # ProgrammerConfig struct, provider list, migration
├── classifier.rs     # Auto-mode tool-call classifier (fast logprob probe + reasoned fallback)
├── commands/         # Slash-command parser + tab completion engine
├── session/          # Multi-session persistence (UUID-keyed JSON in ~/.config/programmer/sessions/)
├── clipboard.rs      # Copy-to-clipboard (OSC 52)
├── providers/        # Multi-provider management (add/edit/delete/switch API backends)
├── response/         # Parsing OpenAI response stream events into MessageItems
├── tools/            # Tool definitions + execution (command, read_file, write_file, edit_file, grep, blob, ask_user, configure_diagnostics)
├── diagnostics/      # Project diagnostics pipeline: profile (TOML), parser (rustc-json/tsc/gnu/regex), runner, diff
└── ui/               # Terminal UI
    ├── components/
    │   ├── conversation_panel/   # Scrollable chat history
    │   ├── input_panel/          # User input textarea with pending-queue support
    │   ├── footer/               # Status bar (mode, model, session)
    │   ├── status_bar/           # Top bar
    │   ├── completion_popup/     # Tab-completion dropdown
    │   ├── messages/             # Per-message-type bubble renderers (assistant, user, tool_result, error, etc.)
    │   ├── question_panel/       # Modal for ask_user responses
    │   └── provider_panel.rs     # Full-screen provider manager
    ├── markdown_code_block.rs    # Syntax-highlighted code block widget
    └── markdown_theme.rs         # Markdown colour palette
```

## Conventions

- **Error handling:** `color_eyre::Result<T>` throughout; `.wrap_err()` for context; `?` propagation.
- **Async:** `tokio::main` on `main()`, `tokio::spawn` for concurrent tasks. All tool execution is async.
- **Configuration:** `ProgrammerConfig` deserializes from TOML via the `config` crate. Environment variables prefixed with `Programmer` override file values (e.g. `Programmer_providers_openai_api_key`).
- **Module visibility:** Most modules are `pub mod`; UI internals are `mod` (private). Tool modules are `pub` within `tools/`.
- **Tests:** Inline `#[cfg(test)]` modules at the bottom of source files. No separate `tests/` directory.
- **Copyright header:** GPL-3.0-or-later header block on every `.rs` file.
- **Naming:** snake_case for modules/functions, CamelCase for types, `pub(crate)` for internal visibility.
- **No `unwrap()` in production code:** Prefer `?`, `.unwrap_or_default()`, or explicit `match`.
- **The diagnostics system** is language-agnostic: it reads `.programmer/diagnostics.toml` for checker definitions (command + parser preset or regex). The `configure_diagnostics` tool writes this file. Parsers: `rustc-json`, `tsc`, `gnu`, or custom regex.
