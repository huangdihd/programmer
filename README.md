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

- **Multi-provider model access** — connect to OpenAI-compatible Responses API
  endpoints, manage providers and their models in the TUI, and use a separate
  lightweight model for tool-call classification.
- **Complete coding toolset** — read, search, edit, and write files; run commands;
  fetch web pages; inspect images; ask questions; manage todos; run diagnostics;
  control background or interactive tasks; and request scoped permissions.
- **Safety modes and native sandboxing** — choose Manual, Auto, Plan, or optional
  YOLO mode. File freshness checks, configurable access rules, named security
  profiles, and native process sandboxing protect the workspace.
- **IDE-style diagnostics** — automatically run configured checkers after edits,
  compare findings against a baseline, and integrate persistent LSP diagnostics.
  `/init` can create both `PROGRAMMER.md` and `.programmer/diagnostics.toml`.
- **MCP and skills** — connect stdio or HTTP MCP servers, expose Programmer's own
  tools as an MCP server, and extend the agent with built-in, shared, global, or
  project-local skills.
- **Tasks and sub-agents** — stream command output, promote long-running commands
  to background tasks, drive interactive PTYs from the TUI, and delegate bounded
  work to independent in-process agents.
- **Multimodal terminal UI** — paste or reference images, render supported images
  with terminal graphics protocols, group related tool calls and reasoning, and
  keep providers, MCP servers, skills, todos, tasks, diagnostics, and agents in a
  scrollable sidebar.
- **Persistent sessions and context control** — resume UUID-keyed conversations,
  track per-turn token usage, queue pending messages, and compact older history
  without losing its summary.
- **Headless automation** — run one-shot jobs with text, JSON, or JSONL output,
  initialize projects, execute diagnostics without a model, and enforce time,
  step, and diagnostic-failure limits from the CLI.

## Installation

### Prebuilt release

On macOS or Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/huangdihd/programmer/main/scripts/install.sh | sh
```

On Windows PowerShell:

```powershell
Invoke-WebRequest `
  -Uri "https://raw.githubusercontent.com/huangdihd/programmer/main/scripts/install.ps1" `
  -OutFile "install.ps1"
powershell -ExecutionPolicy Bypass -File .\install.ps1
```

Both installers select the release asset for the current OS and architecture.
Use `--version v0.2.3` with `install.sh`, or `-Version v0.2.3` with
`install.ps1`, to install a specific release.

## Quick start

1. Open the provider manager:

   ```sh
   programmer --providers
   ```

2. Press `a`, then enter a provider name, an OpenAI-compatible API base URL,
   an API key, and a default model. Press `Enter` to save it, select the
   provider, and press `Enter` again to make it the default.

3. Start Programmer from the project you want it to work on:

   ```sh
   cd your-project
   programmer
   ```

4. Send a concrete first task, for example:

   ```text
   Explain this repository and run its existing tests. Do not change files yet.
   ```

   Use `/init` if you want Programmer to create project guidance and configure
   diagnostics before making changes. The default Auto mode asks a classifier
   model to review mutating tool calls; use a non-reasoning model for that role.

If the provider works but the model list is empty, configure `models` and
`default_model` explicitly in `config.toml`; see
[Provider compatibility](#provider-compatibility).

### Update or uninstall

Once installed, Programmer can update or remove its own executable:

```sh
programmer upgrade --check
programmer upgrade
programmer upgrade --tag v0.2.3
programmer uninstall
programmer uninstall --purge
```

`--purge` also deletes Programmer's configuration, sessions, and global skills.
It cannot be undone.

### Build from source

Prerequisites:

- [Rust toolchain](https://rustup.rs) (MSRV: latest stable)

```sh
git clone https://github.com/huangdihd/programmer.git
cd programmer
cargo build --release
```

The binary will be at `target/release/programmer` (or `programmer.exe` on
Windows).

At runtime, configure an OpenAI-compatible Responses API endpoint and key, or a
compatible local server such as Ollama, LM Studio, or vLLM.

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

# Alternative-token count for the classifier's fast logprob probe. OpenAI
# accepts up to 20; some compatible providers have a lower limit (Qwen: 5).
classifier_top_logprobs = 20

# Model used for manual and automatic context compaction. Falls back to the
# current chat model when absent.
compact_model = "openai/gpt-4o-mini"

# Automatically compact after a response reports at least this many input
# tokens. This uses provider-reported usage (not an estimate); 0 disables it.
auto_compact_tokens = 100000

# Keep this many recent complete turns verbatim after compaction.
compact_keep_recent_turns = 2

# Gate YOLO mode behind this flag so it can't be entered by accident.
allow_yolo = true

# Check GitHub Releases at startup and show a notice when an update exists.
auto_update_check = true

# Co-author trailer added to git commits the agent writes. For the co-author to
# show a GitHub avatar, use an email tied to a GitHub account — e.g. a machine
# user's or a GitHub App bot's `<id>+<name>@users.noreply.github.com`. Set to
# "" to disable. (GitHub organizations can't be commit co-authors.)
git_coauthor = "programmer <noreply@programmer.local>"

[security]
enabled = true
protect_file_changes = true
allow_read_outside_workspace = true

[security.sandbox]
# Enabled by default on supported Unix platforms with network access allowed.
enabled = true
network = true
allow_system_read = true
allow_temp_write = true
fail_closed = true
readable_paths = []
writable_paths = []
denied_read_paths = []
denied_environment = []

# Rules use absolute globs or the portable `workspace/**` prefix. Explicit
# denies take precedence over allows.
[[security.rules]]
operation = "read"
pattern = "/path/to/private/**"
effect = "deny"

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
| `classifier_top_logprobs` | `20` | Alternative-token count for the fast classifier probe (`0`–`20`). Lower this for providers with a smaller limit; Qwen accepts at most `5`. |
| `compact_model` | (chat model) | `provider/model` used for manual and automatic context compaction. |
| `auto_compact_tokens` | `100000` | Provider-reported input-token threshold for seamless background compaction. `0` disables it. Providers that do not report usage do not trigger it. |
| `compact_keep_recent_turns` | `2` | Number of recent complete turns kept verbatim when context is compacted. |
| `allow_yolo` | `false` | Whether `/mode yolo` and `Ctrl+T` can reach YOLO mode. |
| `auto_update_check` | `true` | Check GitHub Releases at startup and show a non-blocking update notice. |
| `git_coauthor` | `programmer <noreply@programmer.local>` | `Co-Authored-By:` trailer added to the agent's git commits. Use a GitHub-linked email for an avatar; `""` disables. |
| `security.protect_file_changes` | `true` | Require an existing file to be read before overwrite and reject writes if it changed after that read. |
| `security.allow_read_outside_workspace` | `true` | Permit direct read tools outside the project unless a rule denies the path. |
| `security.sandbox.enabled` | `true` on supported Unix platforms | Apply the native OS process sandbox to commands, tasks, diagnostics, LSP, and stdio MCP servers. |
| `security.sandbox.network` | `true` | Permit network access from sandboxed child processes. |
| `security.sandbox.allow_system_read` | `true` | Permit reads needed to execute system programs and load shared libraries. |
| `security.sandbox.allow_temp_write` | `true` | Permit writes to the platform temporary directory. |
| `security.sandbox.fail_closed` | `true` | Refuse to run a child process when the platform backend cannot enforce its policy. |
| `security.sandbox.readable_paths` | `[]` | Additional paths sandboxed child processes may read. `~` and workspace-relative paths are supported. |
| `security.sandbox.writable_paths` | `[]` | Additional paths sandboxed child processes may modify. |
| `security.sandbox.denied_read_paths` | `[]` | Paths sandboxed child processes must not read. |
| `security.sandbox.denied_environment` | `[]` | Environment variable name globs removed from sandboxed child processes. An empty list inherits the complete parent environment. |

Each provider is a `[providers.<name>]` section. You can add as many as you want.

## Provider compatibility

Programmer uses the OpenAI **Responses API**, not only the older Chat
Completions API. An endpoint describing itself as OpenAI-compatible may still
implement only part of the protocol.

| Capability | Requirement and fallback |
|---|---|
| `POST /responses` with streaming and tool calls | Required for normal agent conversations. A Chat Completions-only endpoint is not compatible. |
| `GET /models` | Optional. If discovery fails or returns a non-standard response, set `models` and `default_model` manually. |
| Response usage with `input_tokens` | Optional. Without it, `/usage` may be incomplete and token-triggered automatic compaction will not run; manual `/compact` still works. |
| Image input in the Responses format | Optional. Keep `/vision off` when the selected model or provider does not accept image content. |
| Output logprobs | Optional for chat, but used by the Auto classifier's fast probe. Missing or inconclusive logprobs fall back to the full classifier pass. Provider-specific limits may require a lower global `classifier_top_logprobs` value. |

### DeepSeek official API

The DeepSeek official API supports Responses API requests, but its `/models`
entries omit creation-time metadata expected by Programmer's OpenAI client.
Automatic model discovery therefore fails with a missing `created` or
`created_at` field. Configure the current model IDs explicitly:

```toml
default_provider = "deepseek"

[providers.deepseek]
base_url = "https://api.deepseek.com"
api_key = "sk-your-key-here"
models = ["deepseek-v4-flash", "deepseek-v4-pro"]
default_model = "deepseek-v4-pro"
```

The provider remains usable when model discovery fails; the manual list powers
startup model selection, `/model` completion, and the provider model browser.

### Known limitations

- Reasoning models are unsuitable for the Auto-mode classifier because their
  first emitted token is not a direct yes/no decision. Use a non-reasoning
  classifier model.
- Provider support for logprobs and image content varies independently from
  basic text and tool-call support.
- Native process sandboxing is available only on supported Unix platforms.
- `/rewind` can restore only successful changes made through Programmer's
  built-in `write_file` and `edit_file` tools. Shell commands, MCP tools, IDE
  edits, and remote side effects are not reversible by Programmer.

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
| **Auto** | 🤖 | Calls that need review are classified by a separate LLM each turn. Default mode; see below. |
| **Plan** | 📋 | The agent explores read-only, presents a plan, and waits for approval before execution. |
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

Some OpenAI-compatible providers limit how many alternatives may be requested.
For example, Qwen accepts at most 5. Adjust it at runtime:

```
/classifier logprobs 5
```

or in config:

```toml
classifier_top_logprobs = 5
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
| `Esc` | Cancel the active request; before any model output, restore the original draft to the input |
| `Ctrl+T` | Cycle work mode (Manual → Auto → Plan → optional YOLO) |
| `Ctrl+C` / `Ctrl+Q` twice | Quit |
| `Ctrl+V` | Paste an image from the clipboard |
| Mouse scroll | Scroll conversation history |
| `!<command>` + `Enter` | Run a command interactively in a terminal panel (Ctrl+O releases input) |

### Slash commands

| Command | Action |
|---|---|
| `/model <provider/model>` | Switch to a different model |
| `/mode <manual\|auto\|plan>` | Set work mode (or cycle with `Ctrl+T`) |
| `/mode yolo` | Enter YOLO mode (requires `allow_yolo = true`) |
| `/plan <approve\|cancel>` | Approve or cancel the current Plan-mode proposal |
| `/classifier [show]` | Show the effective Auto-mode classifier settings and their source |
| `/classifier <provider/model>` | Override the classifier model for this session |
| `/classifier current` | Force this session to follow the current chat model |
| `/classifier default` | Clear the session override and inherit the global setting |
| `/classifier logprobs <0-20\|default>` | Set or reset the persisted global fast-probe alternative-token count |
| `/init` | Create or refresh `PROGRAMMER.md` and project diagnostics |
| `/diagnostics manage` | Open the project diagnostics checker management panel |
| `/diagnostics update` | Re-run configured checkers and refresh the sidebar diagnostics |
| `/thinking [level]` | Set/show reasoning effort for chat and compaction |
| `/compact` | Manually compact older complete turns with the effective compact model |
| `/compact show` | Show compact model, threshold, latest reported usage, and background status |
| `/compact set model <provider/model\|current\|default>` | Set the compact model for this session |
| `/compact set tokens <number\|off\|default>` | Set, disable, or inherit automatic compaction for this session |
| `/compact set keep <number\|default>` | Set or inherit recent-turn retention for this session |
| `/rewind` | Restore conversation and/or built-in `write_file`/`edit_file` changes to a previous user prompt |
| `/vision <on\|off>` | Enable/disable `@image` attachments for this session |
| `/select [on\|off]` | Toggle native terminal text selection and copying |
| `/permission` `/sandbox` | Show sandbox, file protection, and permission status |
| `/todo` `/t` | Open this session's todo list |
| `/skill <name\|list\|off>` | Activate, list, or clear skills |
| `/skill manage` | Open the skills management panel |
| `/mcp show` | List MCP server status |
| `/mcp manage` | Open the MCP management panel |
| `/terminal [id]` | Open a running or completed task's terminal viewer |
| `/terminal clear` | Remove completed, failed, and killed tasks |
| `/usage` | Show token usage for the session and latest turn |
| `/new` `/n` | Start a new session (auto-saves current) |
| `/session` `/s` | Show current session UUID and info |
| `/providers show` | List all configured providers and models |
| `/providers manage` | Open the provider management panel |
| `/providers refresh [provider]` | Refetch auto-discovered model lists (optionally for one provider) |
| `/clear` `/c` | Delete the current session and reset its chat, todos, images, and diagnostics |
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
| `Enter` (model list) | Choose the model's global chat/classifier/compact role |
| `g` | Edit global classifier logprobs and automatic-compaction settings |
| `q` / `Esc` | Close panel |

Classifier and compact model slash commands, plus compact thresholds and
retention, change only the current session. `/classifier logprobs` is the
exception: it changes the single global value and persists it to `config.toml`.
Changes made in this panel are also global and persisted. Effective model
precedence is session override → global role → current chat model.

### Rewind and automatic compaction

`/rewind` creates a checkpoint for every user prompt that actually starts. It
can restore the conversation, built-in file edits, or both. File rewind tracks
only successful `write_file` and `edit_file` calls (including sub-agents); shell
commands, MCP tools, IDE edits, and remote side effects are outside its scope.
If a tracked file no longer has the content Programmer last wrote, restore
stops before changing any file. Before changing files, Programmer creates a
recovery checkpoint so its file changes can be undone from the same panel.

Automatic compaction observes the real `input_tokens` returned after every API
response. For tool-using responses it waits until all call outputs are recorded,
then summarizes a stable prefix in the background. Input stays usable and new
messages remain outside that prefix. A stale summary is discarded after
`/clear`, `/new`, or `/rewind`.

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

### Skills

Reusable instruction modules are enabled by default and can be toggled with
`/skill <name>` or managed in the `/skill manage` panel. Programmer ships with
three built-in skills: `programmer-guide`, which explains Programmer's own
features and architecture; `initialize-project`, which drives `/init` and the
headless initialization flow; and `update-programmer-md`, which refreshes the
repository guide from verified project facts. Their full instructions are
loaded only when a request needs them.

```text
/skill programmer-guide
/skill initialize-project
/skill update-programmer-md
```

Additional skills are loaded from the cross-agent `~/.agents/skills/<name>/`
directory, from the platform config directory's
`programmer/skills/<name>/SKILL.md`, and from
`.programmer/skills/<name>/SKILL.md` in the current project. Name collisions
resolve in this order: project > Programmer global > shared > built-in. The
active skill set is saved with the session.

### Headless mode

Use `run` for a one-shot agent operation without starting the TUI:

```sh
programmer run "fix the failing tests"
programmer run --model openai/gpt-5 --thinking high --check "refactor this module"
programmer run --init --format json "implement the first todo item"
printf 'explain this repository' | programmer run -
```

`run --init` first executes the same `initialize-project` skill as `/init` in a
hidden Developer turn, then sends the user prompt in the same conversation.
Automatic post-edit diagnostics are enabled whenever
`.programmer/diagnostics.toml` exists; use `--no-diagnostics` to disable the
hook or `--check` for an additional final snapshot. Other controls include
`--classifier-model`, `--work-mode auto|plan|yolo`, `--cwd`,
`--timeout`, `--max-steps`, and `--prompt-file`.

`--format text` prints only the final answer to stdout, `json` emits one
versioned result document, and `jsonl` emits progress events followed by a
result event. Diagnostics from a text-mode final check go to stderr so stdout
remains pipe-friendly.

Project setup and model-free checks are also standalone commands:

```sh
programmer init --model openai/gpt-5
programmer diagnostics
programmer diagnostics --format json --fail-on warning
```

`diagnostics` reads the project profile and runs its checkers without
initializing an LLM provider. It exits unsuccessfully when a checker fails, no
profile exists, or a finding meets the `--fail-on error|warning|lint`
threshold.

### As an MCP server

`programmer` can also run as an [MCP](https://modelcontextprotocol.io) server,
exposing its own local tools (`command`, `read_file`, `write_file`,
`edit_file`, `grep`, `blob`, `fetch`, `diagnostics`, `task`) to any MCP
client — another agent, Claude Desktop, etc. It speaks JSON-RPC 2.0 over stdio;
`ask_user` and `todo` are not exposed because they require an interactive
session.

```sh
programmer mcp stdio
```

`programmer mcp stdio` is **headless**: a client launches it as a subprocess with no
terminal, so it only accepts the non-interactive gating modes. Tool calls are
gated by the same classifier as the TUI, via `--work-mode`. Read-only tools
always run; dangerous ones (`command`, `write_file`, `edit_file`, mutating
`task` actions, …) are gated:

| `--work-mode` | Behavior for dangerous tools |
|---|---|
| `auto` (default) | The **LLM classifier** decides (needs a configured `classifier_model`/default model); runs only if it approves |
| `yolo` | Everything runs without gating |

`manual` (human confirmation) and `plan` (read-only) need an approval surface, so
`mcp stdio` rejects them at startup — use `mcp http` (below), which has a
console, for those.

Register it with a client by pointing at the binary, e.g.:

```json
{
  "mcpServers": {
    "programmer": { "command": "programmer", "args": ["mcp", "stdio", "--work-mode", "yolo"] }
  }
}
```

The tools run in the server process's working directory.

#### Over HTTP, with an approval console

```sh
programmer mcp http
programmer mcp http 0.0.0.0:9000 --work-mode manual
```

`programmer mcp http` serves the same tools over plain-HTTP JSON-RPC (`POST /mcp`) and,
because the transport isn't stdio, keeps the terminal for a small **ratatui
approval console**. The dashboard keeps a selectable call history with full
arguments, results, and approval status. Use `↑`/`↓` or `j`/`k` to inspect
calls, `PgUp`/`PgDn` to scroll details, `y`/`n` to resolve `manual`-mode
approvals, and `Ctrl+T` to switch the work mode live. `auto` still uses the LLM
classifier, while `manual` waits for you at the console.

### Session management

Sessions are saved to `~/.config/programmer/sessions/<uuid>.json`. Conversation
history, model/work mode, the `/vision` switch, and todos are restored
independently for each session.

With `/vision on`, referencing a local PNG, JPEG, WEBP, or non-animated GIF as
`@path` attaches it as an image input. `/vision off` stops sending both new and
historical images without deleting them from the session; turning it back on
restores them. Other local files are referenced by path only; their contents are
not copied into the request context.

The agent can inspect a local image itself with the read-only `read_image` tool.
Its result is sent back to vision-capable models as image content, and expanding
the tool call in the TUI shows a compact true-color half-block preview.

You can also copy an image to the system clipboard and press `Ctrl+V` in the
main input to attach it. The input shows a `[Pasted image #N WIDTHxHEIGHT]`
placeholder; deleting that placeholder removes the attachment before sending.
After sending, the message replaces the placeholder with the same compact
true-color half-block preview used by `read_image`.
Terminal emulators generally reserve `Cmd+V` for text paste, so image paste uses
`Ctrl+V` on macOS as well.

| Flag / command | Action |
|---|---|
| `programmer --resume` | Interactive picker to choose a saved session |
| `programmer --resume <uuid>` | Resume a specific session |
| `/new` `/n` | Save current session and start fresh |
| `/session` `/s` | Show current session UUID |
| `/usage` | Show session and most recent turn token usage |

## Project structure

```
src/
├── main.rs           # Entry point and subcommand dispatch
├── cli.rs            # Clap CLI definitions and validation
├── app/              # TUI application state, events, sessions, and surfaces
├── runner/           # Shared model/tool turn engine for TUI and headless modes
├── conversation.rs   # UI-independent conversation and request history
├── classifier/       # Manual, Auto, Plan, and YOLO tool-call policies
├── security/         # Access rules, named profiles, and process sandboxing
├── tools/            # Built-in tool definitions, policies, and execution
├── tasks/            # Background commands and interactive PTY lifecycle
├── agents/           # In-process sub-agent registry and lifecycle
├── mcp/              # MCP client, server, HTTP transport, and approval console
├── skills/           # Built-in and filesystem-discovered agent skills
├── diagnostics/      # Checker profiles, parsers, baselines, and LSP support
├── session/          # UUID-keyed persistent conversations
├── providers/        # Provider/model discovery and selection
├── headless.rs       # Non-interactive run, init, and diagnostics surfaces
├── upgrade.rs        # Release checks, self-update, and uninstall
└── ui/               # Ratatui components, rendering, and terminal images
```

## Contributing and feedback

Found a bug or provider compatibility issue? Use the structured
[GitHub issue templates](https://github.com/huangdihd/programmer/issues/new/choose).
Pull requests are welcome; see [CONTRIBUTING.md](CONTRIBUTING.md) for the
development workflow and required checks. Never include API keys, private
prompts, or proprietary code in reports.

## License

[GPL-3.0-or-later](LICENSE)
