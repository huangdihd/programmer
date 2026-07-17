# programmer
A coding agent written in Rust
> Initially, computer means a person who computes.   
> When will we pass programmer to coding agents?

## Overview

**programmer** is a terminal-based AI coding agent. It connects to any
OpenAI-compatible API (OpenAI, local models via Ollama/LM Studio, etc.) and
gives the model direct access to your project: it can read files, write files,
edit files with surgical precision, and run shell commands — all inside a
TUI built with [Ratatui](https://ratatui.rs).

## Features

- **Streaming responses** — see the model's answer as it is generated, token by
  token.
- **Tool use** — the model can invoke eight built-in tools:

  | Tool | Description |
  |---|---|
  | `command` | Run a shell command and capture stdout/stderr. |
  | `read_file` | Read a file, optionally with offset and line limit. |
  | `write_file` | Create or overwrite a file (with parent directories). |
  | `edit_file` | Replace an exact substring in a file — minimal, safe edits. |
  | `grep` | Search a regex pattern across files, returning path:lineno:match. |
  | `blob` | Find files by filename regex, returning matching paths. |
  | `ask_user` | Prompt the user with yes/no, multiple choice, or free-text questions. |
  | `configure_diagnostics` | Set up the project's IDE-style diagnostics (language-agnostic checkers). |
- **Markdown rendering** — model responses are rendered with syntax-highlighted
  code blocks, lists, and formatting.
- **IDE-style diagnostics** — after every code edit, the TUI runs a predefined
  set of checkers (e.g. `cargo check`, `tsc --noEmit`) and reports which errors
  were introduced or resolved. Language-agnostic — works with any checker that
  outputs parseable diagnostics (rustc JSON, GNU-style, TypeScript, or custom
  regex patterns). Configured via `/init` or the `configure_diagnostics` tool
  and stored in `.programmer/diagnostics.toml`.
- **Conversation panel** — scrollable chat history with distinct bubbles for
  user, assistant, tool calls, tool results, and errors.
- **Pending messages** — if you type while the model is still responding, your
  input is queued and sent automatically when the turn finishes.
- **Multi-provider** — configure multiple API backends (different keys, base URLs,
  models) and switch between them. Manage via `/providers manage` or `--providers`
  flag.
- **Configurable** — set model, API base URL, and API key via a TOML config
  file or environment variables.
- **Session management** — multi-session support with UUIDs, auto-save on each
  turn, resume with `--resume [uuid]`, restart with `/new`.

## Installation

### Prerequisites

- [Rust toolchain](https://rustup.rs) (MSRV: latest stable)
- An OpenAI-compatible API endpoint and key (e.g. OpenAI, Anthropic via proxy,
  local LLM)

### Build from source

```sh
git clone https://github.com/huangdihd/programmer.git
cd programmer
cargo build --release
```

The binary will be at `target/release/programmer` (or `programmer.exe` on
Windows).

## Configuration

On first launch, `programmer` creates a default config file at:

- **Linux:** `~/.config/programmer/config.toml`
- **macOS:** `~/Library/Application Support/programmer/config.toml`
- **Windows:** `%APPDATA%\programmer\config.toml`

### Minimal config

```toml
default_provider = "openai"

[providers.openai]
base_url = "https://api.openai.com/v1"
api_key = "sk-your-key-here"
```

### Full config

```toml
default_provider = "openai"

# Separate model for the Auto-mode classifier (faster = better).
# Falls back to the chat model when absent. Must be a non-reasoning model.
classifier_model = "openai/gpt-4o-mini"

# Gate YOLO mode behind this flag so it can't be entered by accident.
allow_yolo = true

[providers.openai]
base_url = "https://api.openai.com/v1"
api_key = "sk-your-key-here"
# models = ["gpt-4o", "gpt-4.1"]  # optional: restrict model list
# default_model = "gpt-4o"        # optional: default model for this provider

# [providers.ollama]
# base_url = "http://localhost:11434/v1"
# api_key = "ollama"
```

| Field | Default | Description |
|---|---|---|
| `default_provider` | `"openai"` | Active provider at startup. |
| `classifier_model` | (chat model) | `provider/model` for the Auto-mode classifier. Must be a **non-reasoning** model (see [Auto mode](#work-modes)). |
| `allow_yolo` | `false` | Whether `/mode yolo` and `Ctrl+T` can reach YOLO mode. |

Each provider is a `[providers.<name>]` section. You can add as many as you want.

### Environment variables

Environment variables override file values:

```sh
export Programmer_default_provider="openai"
export Programmer_providers_openai_base_url="https://api.openai.com/v1"
export Programmer_providers_openai_api_key="sk-..."
```

Any OpenAI-compatible `/v1/responses` endpoint works — local models served
by Ollama, LM Studio, vLLM, etc. are supported as long as they expose the
Responses API.

## Work modes

`programmer` has four safety modes that control how tool calls are approved.
Cycle with `Ctrl+T` or `/mode <name>`.

| Mode | Icon | Behaviour |
|---|---|---|
| **Manual** | 🛡 | Every write/edit/command call shows an approval prompt. Read-only tools run automatically. |
| **Allow Edits** | ✏️ | All tool calls auto-approve — no prompts, no LLM overhead. Default mode. |
| **Auto** | 🤖 | Write/edit/command calls are classified by a separate LLM each turn. See below. |
| **YOLO** | ⚡ | Everything runs unchecked. Gated behind `allow_yolo = true` in config. |

### Auto mode classifier

In Auto mode, every **mutating** tool call (`command`, `write_file`, `edit_file`)
is sent to a classifier LLM before execution. Read-only tools (`read_file`,
`grep`, `blob`, `ask_user`) always bypass the classifier.

**How it works — two-pass with fast path:**

1. **Fast probe** (`~1 token`): The classifier gets lightweight context
   (working directory + user request) and is asked: "Should this be
   auto-approved? yes or no." The `yes`/`no` logprob on the first token
   decides immediately — no reasoning needed, no extra cost.

2. **Reasoned fallback** (only when needed): If the fast path is uncertain
   (`no`, ambiguous token, or no logprobs available), the classifier
   re-evaluates with **full context** — assistant replies, tool outputs, and
   recent call history — and produces a reasoned `APPROVE` or
   `DENY: <reason>`.

**User override — per-operation:** The classifier's instructions tell it
to respect explicit per-operation instructions in the user's message:
- "I agree to X, don't do Y" → approve X, deny Y.
- "Go ahead" on a specific previously-denied call → approve it.
- Vague statements ("be careful") do NOT count as overrides.

**Threat model:** The classifier watches for four categories:
- **Overreach** — destructive path to an otherwise valid goal.
- **Honest mistake** — misunderstanding the user's intent.
- **Prompt injection** — external content manipulating the agent.
- **Model misalignment** — the agent pursuing unrequested goals.

#### ⚠️ Thinking/reasoning models

The classifier **does not work with reasoning models** (DeepSeek-R1,
o1, o3, etc.). These models spend their first N tokens on a hidden
reasoning trace; the yes/no answer token never appears in the first
content token, so the fast-path logprob probe fails.

**Use a non-reasoning model for the classifier.** Set it explicitly:
```
/classifier openai/gpt-4o-mini
```
or in config:
```toml
classifier_model = "openai/gpt-4o-mini"
```

If the classifier model turns out to be a thinking model, all Auto-mode
calls will be denied with a clear error message. Switch it to a
non-reasoning model to fix.

## Usage

```sh
programmer
```

### Keyboard shortcuts

| Key | Action |
|---|---|
| `Enter` | Send message |
| `Ctrl+T` | Cycle work mode (Manual → AllowEdits → Auto) |
| `Ctrl+C` / `Ctrl+Q` | Quit |
| Mouse scroll | Scroll conversation history |

### Slash commands

| Command | Action |
|---|---|
| `/model <provider/model>` | Switch to a different model |
| `/mode <manual\|edits\|auto>` | Set work mode (or cycle with `Ctrl+T`) |
| `/mode yolo` | Enter YOLO mode (requires `allow_yolo = true`) |
| `/classifier [provider/model]` | Set/show the Auto-mode classifier model |
| `/classifier clear` | Reset classifier to the chat model |
| `/new` `/n` | Start a new session (auto-saves current) |
| `/session` `/s` | Show current session UUID and info |
| `/providers show` | List all configured providers and models |
| `/providers manage` | Open the provider management panel |
| `/clear` `/c` | Clear the conversation history |
| `/quit` `/q` | Exit the application |
| `/help` `/?` | Show all commands |

### Provider management panel

Open with `/providers manage` or the `--providers` flag.

| Key | Action |
|---|---|
| `↑↓` / `jk` | Navigate provider list |
| `Enter` | Set selected provider as default |
| `a` | Add new provider (opens form) |
| `e` | Edit selected provider |
| `d` | Delete selected provider (confirm with `y`) |
| `m` | Browse model list of selected provider |
| `q` / `Esc` | Close panel |

**In model browser (`m`):**

| Key | Action |
|---|---|
| Type | Filter models (case-insensitive substring match) |
| `Backspace` | Remove filter character |
| `↑↓` / `jk` | Navigate filtered list |
| `Enter` | Set highlighted model as `default_model` |
| `Esc` / `q` | Back to provider list |

**In add/edit form:**

| Key | Action |
|---|---|
| `Tab` / `↑↓` | Next field |
| `Shift+Tab` / `↑` | Previous field |
| `Enter` | Save provider |
| `Esc` | Cancel |

### As an MCP server

`programmer` can also run as an [MCP](https://modelcontextprotocol.io) server,
exposing its own local tools (`command`, `read_file`, `write_file`,
`edit_file`, `grep`, `blob`, `fetch`, `diagnostics`, `todo`, `task`) to any MCP
client — another agent, Claude Desktop, etc. It speaks JSON-RPC 2.0 over stdio;
`ask_user` is not exposed (it needs the interactive UI).

```sh
programmer --mcp-server
```

Tool calls are gated by the same classifier as the TUI, via `--mcp-mode`.
Read-only tools always run; dangerous ones (`command`, `write_file`,
`edit_file`, mutating `task` actions, …) are gated:

| `--mcp-mode` | Behavior for dangerous tools |
|---|---|
| `auto` (default) | The **LLM classifier** decides (needs a configured `classifier_model`/default model); runs only if it approves |
| `manual` | The **human** confirms via MCP [elicitation](https://modelcontextprotocol.io) — the server prompts the client, which asks its user; clients without elicitation support get a refusal |
| `plan` | Refused (read-only exploration only) |
| `yolo` | Everything runs without gating |

Register it with a client by pointing at the binary, e.g.:

```json
{
  "mcpServers": {
    "programmer": { "command": "programmer", "args": ["--mcp-server", "--mcp-mode", "yolo"] }
  }
}
```

The tools run in the server process's working directory.

### Session management

Sessions are saved to `~/.config/programmer/sessions/<uuid>.json`.

| Flag / command | Action |
|---|---|
| `programmer --resume` | Interactive picker to choose a saved session |
| `programmer --resume <uuid>` | Resume a specific session |
| `/new` `/n` | Save current session and start fresh |
| `/session` `/s` | Show current session UUID |

## Project structure

```
src/
├── main.rs           # Entry point, terminal setup, config loading
├── app.rs            # Application state, event loop, stream management
├── config/           # Configuration struct + deserialization
├── classifier.rs     # Tool-call classifier (Auto mode, fast + reasoned paths)
├── commands/         # Slash-command parsing + tab completion
├── session/          # Multi-session persistence (UUID-keyed JSON)
├── clipboard.rs      # Copy-to-clipboard support
├── response/         # Parsing OpenAI response stream events
├── tools/            # Tool definitions + execution (command, read_file, etc.)
├── diagnostics/      # Project diagnostics pipeline (profile, parser, runner, diff)
└── ui/               # Terminal UI (ratatui)
    ├── components/
    │   ├── conversation_panel/   # Chat history rendering
    │   ├── input_panel/          # User input textarea
    │   ├── footer/               # Status bar (mode, model, session)
    │   └── messages/             # Individual message bubble renderers
    ├── markdown_code_block.rs    # Code block rendering
    └── markdown_theme.rs         # Markdown colour theme
```

## License

[GPL-3.0-or-later](LICENSE)
