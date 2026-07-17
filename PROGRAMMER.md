# PROGRAMMER.md — project map for the coding agent

## Overview

**programmer** is a terminal-based AI coding agent TUI written in Rust. It
connects to OpenAI-compatible APIs (Responses API), streams responses, and
gives the model ten tools: `command`, `read_file`, `write_file`, `edit_file`,
`grep`, `blob`, `ask_user`, `configure_diagnostics`, `fetch`, `task`, `todo`,
plus MCP-bridged external tools. The TUI is built with Ratatui and crossterm.
The binary is a single crate at the repo root.

Key features beyond the chat loop:
- **Multi-provider**: add/edit/delete/switch API backends at runtime.
- **Multi-session**: UUID-keyed JSON persistence in `~/.config/programmer/sessions/`.
- **Auto-mode classifier**: per-mode LLM classifier that approves/denies/defers tool calls.
- **MCP (Model Context Protocol)**: connect to external MCP servers (stdio + HTTP);
  their tools are advertised to the model as `mcp__<server>__<tool>`.
- **Skills**: user-authored `SKILL.md` files that inject prompt segments (Vercel Labs compatible).
- **Background tasks**: shell commands that run detached; shown in the sidebar.
- **Todo list**: per-session task tracking with a `todo` tool and a sidebar panel.
- **Diagnostics pipeline**: IDE-style error/warning feedback after edits (command + LSP backends).
- **Slash-commands**: `/init`, `/model`, `/mode`, `/skills`, `/mcp`, `/todo`, etc. with tab-completion.

## Tech stack

- **Language:** Rust (edition 2024, MSRV: latest stable)
- **Async runtime:** tokio (full features)
- **TUI:** ratatui 0.30.2 + ratatui-widgets 0.3.0 + crossterm 0.29.0
- **API client:** async-openai 0.41.1 (Responses API)
- **Config:** `config` crate (TOML file + env override with `Programmer` prefix)
- **Error handling:** color-eyre + thiserror
- **Serialization:** serde + serde_json + toml
- **Markdown rendering:** ratatui-markdown (forked, with syntax-highlight all langs)
- **HTTP:** reqwest 0.13.4 (rustls)
- **Misc:** uuid, regex, which, html2text, unicode-width

## Build / test / run

```sh
# Build
cargo build              # debug
cargo build --release    # release (LTO, stripped, opt-level=s)

# Run
cargo run

# Run with flags
cargo run -- --resume <uuid>      # resume a specific session
cargo run -- --resume             # session picker
cargo run -- --session            # session picker on startup
cargo run -- --providers          # provider manager on startup
cargo run -- -h                   # help

# Test
cargo test

# Check (fast, no codegen)
cargo check
```

The binary runs a fullscreen TUI. On exit it prints a resume hint:
`Session saved. Resume with: programmer --resume <uuid>`.

## Key directories

```
.programmer/                  # Per-project data
├── diagnostics.toml          #   Diagnostics checker profile
└── skills/<name>/SKILL.md    #   Project-specific agent skills

src/
├── main.rs                   # Entry: arg parsing, terminal init, config load, App::run
├── consts.rs                 # Tunable constants (output length, concurrency, tick rate, …)
├── prompts.rs                # Centralised system prompt + classifier instructions
├── cancel.rs                 # CancellationToken for request lifecycle
├── clipboard.rs              # Copy-to-clipboard (OSC 52)
├── terminal.rs               # TerminalGuard: raw-mode enter/restore
│
├── app/                      # Application core
│   ├── mod.rs                #   App struct, ApprovalState, DiagnosticsState, CancelState, event loop
│   ├── commands.rs           #   Slash-command dispatch (/:init, :model, :mode, :skills, :mcp, …)
│   ├── diagnostics.rs        #   Diagnostics snapshot + diff integration
│   ├── events/               #   Key + mouse event routing
│   │   ├── keys.rs
│   │   └── mouse.rs
│   ├── helpers.rs            #   Small utilities (drain approvals, mode transitions, …)
│   ├── session.rs            #   Session save/load hooks
│   ├── stream.rs             #   API stream management + retry
│   └── tools.rs              #   Tool-call routing (execute + approval queue)
│
├── classifier/               # Auto-mode tool-call classification
│   ├── mod.rs                #   WorkMode enum, Verdict, Classifier trait
│   └── llm.rs                #   Light (logprob probe) + Full (reasoned) classifier calls
│
├── commands/                 # Slash-command parser + tab-completion engine
│   └── mod.rs
│
├── config/                   # Configuration
│   ├── mod.rs
│   └── programmer_config.rs  #   ProgrammerConfig, provider list, migration
│
├── diagnostics/              # Diagnostics pipeline (language-agnostic)
│   ├── mod.rs                #   Diagnostic type, diff()
│   ├── profile.rs            #   Profile TOML parsing (Checkers)
│   ├── parse.rs              #   Output parsers: rustc-json, tsc, gnu, regex
│   ├── runner.rs             #   Run checkers, collect diagnostics
│   └── lsp.rs                #   LSP-based checker (spawn + query over stdio)
│
├── mcp/                      # Model Context Protocol integration
│   ├── mod.rs                #   McpManager: connect, discover tools, route calls
│   ├── types.rs              #   JSON-RPC types, McpTool, McpServerConfig, McpPolicy
│   ├── client.rs             #   Stdio transport (spawn + JSON-RPC over stdin/stdout)
│   └── http_client.rs        #   HTTP SSE transport
│
├── providers/                # Multi-provider management
│   └── mod.rs                #   ProviderManager: add/edit/delete/switch API backends
│
├── response/                 # API response parsing
│   ├── mod.rs
│   ├── message_item.rs       #   MessageItem enum (User, Assistant, ToolCall, ToolResult, …)
│   ├── partial_response.rs   #   Streamed partial response accumulator
│   └── response_finish_reason.rs
│
├── session/                  # Multi-session persistence
│   └── mod.rs                #   SessionManager, Session struct (JSON on disk), list/pick
│
├── skills/                   # Agent skills (Vercel Labs compatible)
│   ├── mod.rs                #   Skill discovery (project + global), shadowing
│   └── skill.rs              #   Skill: name, description, body, constraints
│
├── tasks/                    # Background task system
│   └── mod.rs                #   TaskRegistry (global), TaskHandle, status/io/kill
│
├── todos/                    # Per-session todo list
│   └── mod.rs                #   Todo, TodoList, sync via ~/.config/programmer/todos.json
│
├── tools/                    # Tool definitions + execution
│   ├── mod.rs                #   Tool enum, tools() list, shell(), resolve_program(), environment_info()
│   ├── command.rs            #   Shell command execution
│   ├── read_file.rs          #   Read file with offset/limit
│   ├── write_file.rs         #   Write/create file (whole-file replacement)
│   ├── edit_file.rs          #   Substring replacement in file
│   ├── grep.rs               #   Regex search across files
│   ├── blob.rs               #   File glob (find by name pattern)
│   ├── ask_user.rs           #   Prompt user for input (yes/no, multi-choice, text)
│   ├── configure_diagnostics.rs  # Write .programmer/diagnostics.toml
│   ├── diagnostics.rs        #   Run diagnostics + return current errors/warnings
│   ├── fetch.rs              #   HTTP fetch (html2text conversion)
│   ├── task.rs               #   Background task management (create/list/output/write/wait/kill)
│   ├── todo.rs               #   Todo list management (add/list/update/delete)
│   └── mcp_bridge.rs         #   Internal: route MCP-prefixed calls to McpManager
│
└── ui/                       # Terminal UI (Ratatui)
    ├── mod.rs
    ├── ui.rs                 #   Main UI layout + render dispatch
    ├── event.rs              #   Event + EventHandler enums
    ├── text.rs               #   Text styling helpers
    ├── markdown_code_block.rs    # Syntax-highlighted code block widget
    ├── markdown_theme.rs         # Markdown colour palette
    ├── tool_details.rs       #   Tool-call detail popup (arguments + output)
    └── components/
        ├── mod.rs
        ├── conversation_panel/   # Scrollable chat history
        │   ├── mod.rs
        │   ├── conversation_panel.rs
        │   └── ui.rs
        ├── input_panel/          # User input textarea + pending-queue indicator
        │   ├── mod.rs
        │   ├── input_panel.rs
        │   └── ui.rs
        ├── footer/               # Status bar (mode, model, session)
        │   ├── mod.rs
        │   ├── footer.rs
        │   └── ui.rs
        ├── status_bar/           # Top bar
        │   ├── mod.rs
        │   ├── status_bar.rs
        │   └── ui.rs
        ├── completion_popup/     # Tab-completion dropdown
        │   ├── mod.rs
        │   └── ui.rs
        ├── messages/             # Per-message-type bubble renderers
        │   ├── mod.rs
        │   ├── assistant/        #   Assistant response (text, reasoning, tool_call, unsupported)
        │   ├── assistant_message.rs
        │   ├── error_message.rs
        │   ├── info_message.rs
        │   ├── pending_message.rs
        │   ├── tool_result.rs
        │   ├── usage_message.rs
        │   ├── user_message.rs
        │   ├── warning_message.rs
        │   └── welcome_message.rs
        ├── question_panel/       # Modal for ask_user responses
        ├── provider_panel/       # Full-screen provider manager
        ├── sidebar/              # Side panel (task list, MCP status)
        ├── skills_panel/         # Skills browser/manager
        ├── mcp_panel/            # MCP server manager
        ├── todo_panel/           # Todo list sidebar
        ├── logo/                 # Startup logo rendering
        └── panel_search.rs       # Search/filter within panels
```

## Conventions

- **Error handling:** `color_eyre::Result<T>` throughout; `.wrap_err()` for context; `?` propagation. `thiserror` for library-style error types.
- **Async:** `#[tokio::main]` on `main()`, `tokio::spawn` for concurrent tasks. All tool execution is async.
- **Configuration:** `ProgrammerConfig` deserializes from TOML via the `config` crate. Environment variables prefixed with `Programmer` override file values. Config lives at `~/.config/programmer/config.toml`.
- **Sessions:** Stored as JSON at `~/.config/programmer/sessions/<uuid>.json`. Each session contains message items, history, todos, and persisted task state.
- **Module visibility:** `pub(crate)` for internal visibility; `pub` only where needed externally. UI internals are `mod` (private). Tool modules are `pub` within `tools/`.
- **Tests:** Inline `#[cfg(test)]` modules at the bottom of source files. No separate `tests/` directory.
- **Copyright header:** GPL-3.0-or-later header block on every `.rs` file.
- **Naming:** snake_case for modules/functions, CamelCase for types.
- **No `unwrap()` in production code:** Prefer `?`, `.unwrap_or_default()`, or explicit `match`.
- **The diagnostics system** is language-agnostic: it reads `.programmer/diagnostics.toml` for checker definitions. Each checker can be a one-shot command (parsed via `rustc-json`, `tsc`, `gnu`, or regex) or an LSP server (`kind = "lsp"`). The `configure_diagnostics` tool writes this file.
- **Constants** live in `src/consts.rs` — tunable values like output length limits, concurrency caps, tick rate, and classifier budgets.
- **Prompts** are centralised in `src/prompts.rs`: system prompt, classifier instructions, and plan-mode injection.
- **MCP integration** supports both stdio and HTTP transports. Tools are prefixed `mcp__<server>__<tool>` and merged into the advertised tool list.
- **Skills** are discovered from `.programmer/skills/<name>/SKILL.md` (project) and `~/.config/programmer/skills/<name>/SKILL.md` (global). Project skills shadow global ones.
