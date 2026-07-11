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

The program itself is the agent you are interacting with right now.

## Features

- **Streaming responses** — see the model's answer as it is generated, token by
  token.
- **Tool use** — the model can invoke four built-in tools:
  | Tool | Description |
  |---|---|
  | `command` | Run a shell command and capture stdout/stderr. |
  | `read_file` | Read a file from the project directory. |
  | `write_file` | Create or overwrite a file (with parent directories). |
  | `edit_file` | Replace an exact substring in a file — minimal, safe edits. |
- **Markdown rendering** — model responses are rendered with syntax-highlighted
  code blocks, lists, and formatting.
- **Conversation panel** — scrollable chat history with distinct bubbles for
  user, assistant, tool calls, tool results, and errors.
- **Pending messages** — if you type while the model is still responding, your
  input is queued and sent automatically when the turn finishes.
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
model = "your-model-name"
base_url = "https://api.openai.com/v1"
api_key = "sk-your-key-here"
```

Alternatively, set environment variables (they override file values):

```sh
export Programmer_model="gpt-4o"
export Programmer_base_url="https://api.openai.com/v1"
export Programmer_api_key="sk-..."
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
