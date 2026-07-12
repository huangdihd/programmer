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
- **Tool use** — the model can invoke seven built-in tools:

  | Tool | Description |
  |---|---|
  | `command` | Run a shell command and capture stdout/stderr. |
  | `read_file` | Read a file, optionally with offset and line limit. |
  | `write_file` | Create or overwrite a file (with parent directories). |
  | `edit_file` | Replace an exact substring in a file — minimal, safe edits. |
  | `grep` | Search a regex pattern across files, returning path:lineno:match. |
  | `blob` | Find files by filename regex, returning matching paths. |
  | `ask_user` | Prompt the user with yes/no, multiple choice, or free-text questions. |
- **Markdown rendering** — model responses are rendered with syntax-highlighted
  code blocks, lists, and formatting.
- **Conversation panel** — scrollable chat history with distinct bubbles for
  user, assistant, tool calls, tool results, and errors.
- **Pending messages** — if you type while the model is still responding, your
  input is queued and sent automatically when the turn finishes.
- **Multi-provider** — configure multiple API backends (different keys, base URLs,
  models) and switch between them. Manage via `/providers manage` or `--providers`
  flag.
- **Configurable** — set model, API base URL, and API key via a TOML config
  file or environment variables.

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

Edit it to provide your credentials:

```toml
default_provider = "openai"

[providers.openai]
base_url = "https://api.openai.com/v1"
api_key = "sk-your-key-here"
# models = ["gpt-4o", "gpt-4.1"]  # optional: restrict model list
# default_model = "gpt-4o"        # optional: default model for this provider

# [providers.ollama]
# base_url = "http://localhost:11434/v1"
# api_key = "ollama"
```

Each provider is a `[providers.<name>]` section. `default_provider` picks which
one is active at startup. You can add as many providers as you want.

Alternatively, set environment variables (they override file values):

```sh
export Programmer_default_provider="openai"
export Programmer_providers_openai_base_url="https://api.openai.com/v1"
export Programmer_providers_openai_api_key="sk-..."
```

Any OpenAI-compatible `/v1/responses` endpoint works — local models served
by Ollama, LM Studio, vLLM, etc. are supported as long as they expose the
Responses API.

## Usage

```sh
programmer
```

Inside the TUI:

| Key | Action |
|---|---|
| Type + `Enter` | Send message |
| `Ctrl+C` / `Ctrl+Q` | Quit |
| Mouse scroll | Scroll conversation history |
| Mouse click | (on clickable areas in the conversation) |
| `/model <name>` | Switch model |
| `/providers manage` | Open provider management panel |

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

When the cursor is on the **default_model** field:
- Press **`Tab`** to open a completion popup showing models that match your input.
- **`↑↓`** navigate the popup, **`Enter`** accepts the highlighted model,
  **`Esc`** closes the popup.
- Typing or pressing backspace refreshes the candidates live.

When you send a message, the model responds. If it decides to use a tool
(read a file, run a command, etc.), the tool executes automatically in the
background and the results are fed back to the model to continue.

## Project structure

```
src/
├── main.rs           # Entry point, terminal setup, config loading
├── app.rs            # Application state, event loop, stream management
├── config/           # Configuration struct + deserialization
├── response/         # Parsing OpenAI response stream events
├── tools/            # Tool definitions + execution (command, read_file, etc.)
└── ui/               # Terminal UI (ratatui)
    ├── components/
    │   ├── conversation_panel/   # Chat history rendering
    │   ├── input_panel/          # User input textarea
    │   └── messages/             # Individual message bubble renderers
    ├── markdown_code_block.rs    # Code block rendering
    └── markdown_theme.rs         # Markdown colour theme
```

## License

[GPL-3.0-or-later](LICENSE)
