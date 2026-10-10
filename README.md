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
  The `diagnostics` tool publishes the same checker snapshot to the sidebar (no
  second run). Tool calls follow turn cancellation; incomplete runs keep the last
  known findings rather than declaring them resolved. Older background refreshes
  and baseline seeds cannot overwrite a newer diagnostics request.
  `/init` can create both `PROGRAMMER.md` and `.programmer/diagnostics.toml`.
- **MCP and skills** — connect stdio or HTTP MCP servers, expose Programmer's own
  tools as an MCP server, and extend the agent with built-in, shared, global, or
  project-local skills.
- **Tasks and sub-agents** — stream command output, promote long-running commands
  to background tasks, drive interactive PTYs from the TUI, and delegate bounded
  work to independent in-process agents whose live phase (including the memory
  model's association pass) shows on their own sidebar row.
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
Use `--version v0.2.19` with `install.sh`, or `-Version v0.2.19` with
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

   On Programmer's first interactive launch, a one-time global setup guide opens
   provider management so you can configure providers and assign models for the
   chat, classifier, compact, memory, title, and suggestion roles. It can also be
   skipped. After a successful `write_file` or `edit_file` call, the next model
   step receives a standalone developer reminder to check `PROGRAMMER.md`,
   related documentation, and tests. The reminder is omitted if that turn
   successfully edits `PROGRAMMER.md`; shell commands alone do not trigger it.
   The default Auto mode asks a classifier model to review
   mutating tool calls; use a non-reasoning model for that role.

If the provider works but the model list is empty, configure `models` and
`default_model` explicitly in `config.toml`; see
[Provider compatibility](#provider-compatibility).

### Update or uninstall

Once installed, Programmer can update or remove its own executable:

```sh
programmer upgrade --check
programmer upgrade
programmer upgrade --tag v0.2.19
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

## Cross-session collaboration

In the TUI, the model can use `peer_session` to list saved sessions, ask another
session a question, search saved conversation content, or delegate work:

- **`search`** finds case-insensitive literal `query` text in saved messages and
  summaries, including retained pre-compaction history. Returns session IDs,
  titles, workspaces, and up to three short matching excerpts per session.
  Optional `workspace` filters by directory; `limit` defaults to 10 (maximum 20).
  Each call scans at most 100 sessions in newest-update-first order; pass
  `next_offset` as `offset` to continue, even if the current page has no matches.
  Tool payloads, reasoning, and unsaved changes are excluded. This local read-only
  search makes no model request and does not wake or authorize another session.

- **`ask`** uses the target's context for one tool-free model answer. Offline
  sessions can answer too. The exchange is retained in the target's context.
  A user-opened target also queues a normal agent turn; a busy target answers
  from its committed snapshot and waits before running that turn.
- **`delegate`** queues a task for an existing UUID, or creates a session in an
  existing `workspace` and returns a `cd … && programmer --resume …` command.
  Nothing opens automatically. The target user must choose **Yes/No**; Yes
  while busy means accepted and queued, not completed.
- **`cancel`** removes a delegation that has not started. Pass the target
  `session_id` and the delegation `id` returned by `delegate`. Cancellation
  dismisses an already-open consent prompt, but does not stop running work.
- Peer questions and their lightweight replies appear as one persisted, purple
  `↔` row, collapsed by default. Click its disclosure to inspect the full source
  session, question, and answer; an arriving reply updates the same row.
- Delegations have a persisted, task-first purple disclosure row plus a status
  explanation. The same entry updates through **pending**, **accepted · queued**,
  **started**, **rejected**, or **cancelled**; expand it for the source and full task. Started
  is not completed. Reopening a still-pending inbox asks for consent again and
  resets the display to pending. Source-side accepted/rejected/started notifications
  recover the original task from durable delegation history (or its pending envelope),
  matching the original ID and both sessions. Unmatched notices remain informational;
  statuses never grant execution permission or infer completion. These UI records are excluded from
  model context (the existing untrusted developer wrappers remain separate).
- Delegation consent uses a compact bottom panel, defaulting to **No**.
  Use **Left/Right** then **Enter**, or **Esc** to reject. **D** expands the
  full task, and **PgUp/PgDn** scroll it. Busy acceptance is labelled **queue**.
  **M** opens a searchable picker of configured models; **Up/Down** and **Enter**
  change the target session's model for subsequent dialogue and return to consent
  without accepting. **Esc** in the picker only closes it. Rejecting the delegation
  does not undo a model choice. Running requests keep their original model snapshot;
  selection is saved at the normal idle session-save boundary (sessions without
  conversation input are not persisted).
- Clarifications and completion reports use the same `ask` channel. Peer text
  is not user permission and does not bypass ordinary tool approvals.

Draft input and approval panels take precedence. After Stop, queued peer work
stays pending until an explicit user continuation. There is no automatic
anti-loop policy: users supervise and stop repeated inter-session inquiries.
Transport is local to this machine, with durable inboxes alongside the sessions
in Programmer's configuration directory; it is not a remote collaboration service.

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

# Optional replacement for Programmer's built-in identity and mindset section.
# All environment, safety, tool-use, and editing instructions remain in place.
soul = """
You are a thoughtful, direct pair programmer.
"""

# Separate model for the Auto-mode classifier (faster = better).
# Falls back to the chat model when absent. Must be a non-reasoning model.
classifier_model = "openai/gpt-4o-mini"

# Alternative-token count for the classifier's fast logprob probe. OpenAI
# accepts up to 20; some compatible providers have a lower limit (Qwen: 5).
classifier_top_logprobs = 20

# Model used for manual and automatic context compaction. Falls back to the
# current chat model when absent.
compact_model = "openai/gpt-4o-mini"

# Dedicated model that associates the current turn with stored memories. It
# receives the recent conversation plus a manifest of local lexical candidates
# and returns the few worth injecting. Falls back to the current chat model
# when absent.
memory_model = "openai/gpt-4o-mini"

# Model used to generate a concise title from a session's first message. Falls
# back to the current chat model when absent.
title_model = "openai/gpt-4o-mini"

# Model used after each completed turn to predict the user's next message for
# the input placeholder. Falls back to the current chat model when absent.
suggestion_model = "openai/gpt-4o-mini"

# Automatically compact after a response reports at least this many input
# tokens. This uses provider-reported usage (not an estimate); 0 disables it.
auto_compact_tokens = 100000

# Hard limit checked from provider-reported usage. At or above this value,
# Programmer blocks the next model request until compaction reduces the context.
# 0 disables the hard gate.
mandatory_compact_tokens = 150000

# Keep this many recent complete turns verbatim after compaction.
# Mandatory hard-limit compaction overrides this retention.
compact_keep_recent_turns = 2

# After an automatic compaction, suppress another one for this many subsequent
# user turns. Manual and mandatory compaction bypass this cooldown; 0 disables it.
auto_compact_cooldown_turns = 5

# Enable image input for new interactive and headless sessions. Resumed sessions
# restore their own saved state; `/vision on|off global` updates this value.
vision_enabled = true

# UI color mode: auto (default), light, or dark. /theme persists this setting.
theme = "auto"

# Gate YOLO mode behind this flag so it can't be entered by accident.
allow_yolo = true

# Check GitHub Releases at startup and show a notice when an update exists.
auto_update_check = true

# Co-author trailer added to git commits the agent writes. For the co-author to
# show a GitHub avatar, use an email tied to a GitHub account — e.g. a machine
# user's or a GitHub App bot's `<id>+<name>@users.noreply.github.com`. Set to
# "" to disable. (GitHub organizations can't be commit co-authors.)
git_coauthor = "programmer <noreply@programmer.local>"

[memory]
# Durable memory is local Markdown outside the repository. Automatic association
# runs alongside the first request and can supply memories to later tool steps.
# Candidates do not require keyword overlap; the memory model selects relevance.
# Memories added or recalled in the live context are excluded before candidate
# limits. Compacted-away records no longer exclude them; retained turns still do.
# Explicit recall falls back to local candidate order if model selection fails.
enabled = true
global_enabled = true
project_enabled = true
max_global_results = 30
max_project_results = 8
# Background consolidation ("Dream"). Completed sessions are queued on exit and
# consolidated by an in-process worker: after at least this many queued sessions
# and this many hours since the last pass, a dedicated model proposes memory
# operations and only high-confidence additions are applied automatically.
dream_enabled = true
dream_min_sessions = 5
dream_min_interval_hours = 24
dream_timeout_secs = 60

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
| `soul` | (built-in identity) | Replaces only Programmer's identity and mindset section in the developer prompt; operational and safety instructions remain active. Supports TOML multiline strings. |
| `classifier_model` | (chat model) | `provider/model` for the Auto-mode classifier. Must be a **non-reasoning** model (see [Auto mode](#work-modes)). |
| `classifier_top_logprobs` | `20` | Alternative-token count for the fast classifier probe (`0`–`20`). Lower this for providers with a smaller limit; Qwen accepts at most `5`. |
| `compact_model` | (chat model) | `provider/model` used for manual and automatic context compaction. |
| `memory_model` | (chat model) | Dedicated `provider/model` used to associate the current turn with relevant stored memories. |
| `title_model` | (chat model) | `provider/model` used to generate a concise title for each new session. |
| `suggestion_model` | (chat model) | `provider/model` used after successful turns to predict the next user message shown in the input placeholder. |
| `auto_compact_tokens` | `100000` | Provider-reported input-token threshold for seamless background compaction. `0` disables it. Providers that do not report usage do not trigger it. |
| `mandatory_compact_tokens` | `150000` | Hard provider-reported context limit. Programmer blocks further model requests until a proven-smaller compaction drops below it. `0` disables the gate. |
| `compact_keep_recent_turns` | `2` | Number of recent complete turns kept verbatim; mandatory hard-limit compaction overrides this retention. |
| `auto_compact_cooldown_turns` | `5` | Suppress another automatic compaction for this many subsequent user turns; manual and mandatory compaction bypass it. `0` disables the cooldown. |
| `memory.enabled` | `true` | Enable the persistent-memory store, automatic association, and the `memory` tool. |
| `memory.global_enabled` / `project_enabled` | `true` | Include cross-project preferences and current-project memories when recalling. |
| `memory.max_global_results` / `max_project_results` | `30` / `8` | Default per-scope memory candidate limits. |
| `memory.dream_enabled` | `true` | Queue completed sessions for background consolidation and run the Dream worker. |
| `memory.dream_min_sessions` | `5` | Queued sessions required before an automatic Dream pass runs. |
| `memory.dream_min_interval_hours` | `24` | Minimum hours between automatic Dream passes. |
| `memory.dream_timeout_secs` | `60` | Timeout for one Dream planning request. |
| `vision_enabled` | `true` | Enable image input for new interactive and headless sessions. Resumed sessions restore their saved state. |
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

`request_permission` refuses filesystem access requests when the sandbox is off. Sandbox-mode requests may only relax isolation (for example, `restricted` → `network`); requests that would make the current mode stricter are rejected without prompting.

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

### Tool argument generation failures

Programmer stops a response if tool arguments develop a long exact repetitive
suffix (16 KiB, repeating units up to 1 KiB), or exceed 8 MiB in aggregate.
No tools from that response execute. When call identities are unambiguous,
Programmer records paired tool failures with sanitized empty arguments and lets
the model correct them through the normal agent loop; the generated garbage is
not returned to the model. Missing or ambiguous identities fail the response
instead. Three consecutive generation-limit failures stop the turn.
Legitimate highly repetitive or oversized payloads may also hit these limits;
split such changes into smaller calls.

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
   `DENY: <reason>`. Each complete classifier operation has a 30-second
   deadline, including transient retries (up to three retries); cancellation
   interrupts both the request and retry backoff. On failure it retains the
   existing safe denial fallback.

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

### Token usage

Completed responses retain a visible usage status even when the provider omits
accounting or reports all-zero token counts. These cases are distinguished as
`Usage unavailable`; mixed turns show the reported totals plus an
`Usage incomplete` notice. Missing values are never replaced with estimates.
Zero input-token reports do not erase the last known positive context-size count.

### Keyboard shortcuts

| Key | Action |
|---|---|
| `Enter` | Send message |
| `Right` | Accept the model-generated next-message suggestion when the input is empty; typing hides it, and deleting the draft reveals it again—even after accepting with Right. Sending clears the old suggestion. |
| `Up` | Move a queued message back into the empty input for editing |
| `Esc` | Cancel only the active request; after it stops, automatically continue queued work (unless an unsent draft or approval blocks it). Before any model output, restore the original draft only when the input is empty and no user request is queued |
| `Ctrl+T` | Cycle work mode (Manual → Auto → Plan → optional YOLO) |
| `Ctrl+C` / `Ctrl+Q` twice | Quit |
| `Ctrl+V` | Paste an image from the clipboard |
| Mouse scroll | Scroll conversation history |
| `!<command>` + `Enter` | Run a command interactively in a terminal panel (Ctrl+O releases input) |

### Markdown links

Click a rendered inline HTTP(S) Markdown link in an assistant message to open
it in your default browser. The underlined project GitHub URL in the welcome
card is also clickable. Dragging still selects text and never opens a link.
This works with wrapped/scrolled text and either theme. With `/select on`, mouse
handling belongs to the terminal instead. Reference links, bare URLs, reasoning
and tool detail panes are not currently clickable; ambiguous duplicate labels
are deliberately ignored rather than opening the wrong destination.

### Batch file edits

The `edit_file` tool accepts `replace_all: true` to replace every non-overlapping
match within the optional `offset`/`limit` range (at least one match is required).
The default remains a single, unique match. For several changes to one file:

```json
{
  "path": "src/example.rs",
  "edits": [
    {"old_string": "old_name", "new_string": "new_name", "replace_all": true},
    {"old_string": "const LIMIT: usize = 10;", "new_string": "const LIMIT: usize = 20;"}
  ]
}
```

Batch entries are applied sequentially in memory, including their line ranges;
the file is written only after every entry validates. Do not combine `edits`
with top-level replacement/range fields. Existing read-before-write and
permission checks still apply.

### Appearance

`/theme light` and `/theme dark` apply immediately, including already-rendered
Markdown and management panels. `/theme auto` (the default) uses the terminal
background detected at startup via OSC 11; unsupported queries fall back to dark.
Detection shares the existing graphics-protocol query with a 500 ms inactivity
timeout, before the event reader starts. `/theme` reports the setting, effective
mode; manual overrides are explicitly labeled and only auto reports the detected background. Auto uses the startup result, not live terminal
appearance notifications; restart after changing your terminal's theme.
Embedded task terminals retain their own ANSI colors.

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
| `/keepretry [exponential\|fixed <duration>]` | Retry the previous model request forever until success or `Esc`; exponential retries continue indefinitely with a maximum 30s interval, and fixed durations accept `ms`, `s`, or `m` |
| `/compact` | Manually compact older complete turns with the effective compact model |
| `/compact show` | Show compact model, threshold, latest reported usage, and background status |
| `/compact set model <provider/model\|current\|default>` | Set the compact model for this session |
| `/compact set tokens <number\|off\|default>` | Set, disable, or inherit automatic compaction for this session |
| `/compact set keep <number\|default>` | Set or inherit recent-turn retention for this session |
| `/rewind` | Restore conversation and/or built-in `write_file`/`edit_file` changes to a previous user prompt |
| `/vision <on\|off> [session]` | Enable/disable image input for this session (the default scope) |
| `/vision <on\|off> global` | Enable/disable image input now and persist the default for new sessions |
| `/select [on\|off]` | Toggle native terminal text selection and copying |
| `/theme [auto\|light\|dark]` | Show the effective theme or persist a theme override |
| `/permission` `/sandbox` | Show sandbox, file protection, and permission status |
| `/todo` `/t` | Open this session's todo list |
| `/memory list [global\|project]` | List active persistent memories |
| `/memory recall <query>` | Search relevant memories |
| `/memory remember <global\|project> <kind> <content>` | Store an explicit stable preference, fact, or decision |
| `/memory update <id> <content>` | Correct an existing memory |
| `/memory forget <id>` | Permanently remove a memory |
| `/memory dream [status\|preview\|apply] [global\|project]` | Show, plan, or apply background consolidation |
| `/memory dream history [session]` | Browse Dream audits and preview whole-run rollback (`r`, then `y` to confirm) |
| `/memory dream recover [confirm]` | Explain or explicitly complete an interrupted Dream transaction |
| `/memory <on\|off>` | Enable or disable memory for this run |
| `/skill <name\|list\|off>` | Activate, list, or clear skills |
| `/skill manage` | Open the skills management panel |
| `/mcp show` | List MCP server status |
| `/mcp manage` | Open the MCP management panel |
| `/terminal [id]` | Open a running or completed task's terminal viewer |
| `/terminal clear` | Remove completed, failed, and killed tasks |
| `/usage` | Show cumulative token usage and the latest model request's input-token count |
| `/new` `/n` | Stop and await the current session's work, save it, then start a new session |
| `/session` `/s` | Show current session UUID and info |
| `/session graph` | Read-only cross-session question/delegation graph, timeline, and details |
| `/title [text]` | Regenerate the current session title, or set it manually when text is provided |
| `/providers show` | List all configured providers and models |
| `/providers manage` | Open the provider management panel |
| `/providers refresh [provider]` | Refetch auto-discovered model lists (optionally for one provider) |
| `/clear` `/c` | Delete the current session and reset its chat, todos, images, and diagnostics (refused while a turn is active) |
| `/quit` `/q` | Exit the application |
| `/help` `/?` | Show all commands |

Session switches, `/clear`, and TUI exit stop and await session-owned tasks and sub-agents. `/clear` refuses an active foreground turn; when idle, it also closes background work before resetting session resources. A failed shutdown or save is reported; `/new` does not silently replace the old session, and stopped sessions cannot accept new execution until the transition is retried successfully. **Esc remains turn-local**: it cancels the current request, then queued work resumes when drafts and approvals permit it.

Memory is stored as inspectable Markdown under the platform config directory:
global memories use `programmer/memory/global/`, and project memories use
`programmer/memory/projects/<workspace-id>/`. Each entry has its own Markdown
file and `MEMORY.md` is an index. Older JSON stores are migrated losslessly on
first access and retained as a backup. They are not written into the repository.
Credential-like content is rejected.

At the start of every user turn the memory model is asked to associate the
recent conversation with the strongest local candidates for the current
request. Sub-agents run the same association pass against their own task
prompt, matching Claude Code, where the recall prefetch lives in the shared
query loop with no agent guard. What a sub-agent does *not* get is the memory
mechanics section of the system prompt, so it receives recalled memories but is
never told how to read or write the store; that stays a main-session
instruction. Association is prefetched concurrently with the first model response, so even
a slow memory model never delays first-token latency. If that response calls
tools, a later model step collects a ready result or gives it one cancellable
2.5-second grace period; the few selected memories are then appended as a
developer message and remain in context for the rest of the turn. A one-response
turn never waits and simply discards an unfinished prefetch. This preserves the
cached prompt prefix and system prompt, and an existing associated block is
never added again. `Associating` is shown only during the later grace period;
for a sub-agent it appears on that agent's own sidebar row rather than the main
turn's status. Candidates are ranked locally first, weighted by keyword
overlap, memory kind, confidence, and freshness: memories decay with a per-kind
half-life (durable preferences slowly, volatile workflows quickly) and are
reinforced by each recall that returns them. Decay only ever lowers a memory's
association weight — entries are never deleted for being old. Because a weight
only decides what is retrieved, each injected memory also carries its own age
(`saved today`, `saved 47 days ago`), and anything older than a day is instead
prefixed with a caveat that it is a point-in-time observation whose claims
about code behavior or `file:line` citations may be outdated and must be
verified against current code before being asserted as fact. Age is measured
from `updated_at`, the last time the memory's content changed, not from the
recall that just reinforced it.

The association request itself runs with reasoning disabled — selecting IDs
from a short manifest is not a thinking task, and a reasoning model would spend
the entire budget before answering — under a 30-second budget that can be
cancelled at any point. A lookup that fails or times out never fails the turn:
it recalls nothing and reports the reason as an informational line, because
`Memories recalled: 0` on its own cannot distinguish "nothing was relevant"
from "the model never answered". When the model returns nothing usable, that
line carries what actually came back — response status, output item kinds, and
the reasoning tokens spent — so the cause is visible without reproducing the
call by hand.

Explicit recall remains available through the `memory` tool and `/memory
recall`; it lists each entry with its age in days and reinforces what it
returned, so a manual recall counts toward a memory's freshness exactly like an
automatic one. `memory list`, `update`, and `forget` behave as before, except
that `list` also shows each entry's age.

Session replacement (`/new` or a rewind fork) first stops and joins the source
session's work, then saves it before switching identities. If closing or saving
fails, that session remains stopped: no user, notification, peer, init, or retry
turn can start. Use `/new` to retry, or quit. Ordinary Esc only cancels the current
turn; it does not close the session. Rewind forks use fresh task/agent registries,
conversation storage, and todo storage; background work is not transferred.

### Background consolidation (Dream)

Dream is the part of memory that works while you are not looking at it: it reads
finished sessions and turns them into durable entries, rather than only storing
what a single turn or an explicit `remember` happened to capture.

When a session ends — quitting the TUI, `/new`, or a finished `programmer run`
— its user/assistant transcript, never tool output, images, or Programmer's own
injected messages, is written to a pending queue kept beside the project's
memory entries (`programmer/memory/projects/<workspace-id>/pending/`, with
consumed sessions moved to `processed/`; a very long session is truncated to its
most recent 12,000 characters, and a transcript that looks like it contains a
credential is refused outright). An in-process
worker, started with the TUI and stopped with it, then consolidates that queue
once both `dream_min_sessions` queued sessions and `dream_min_interval_hours`
since the last pass have elapsed. There is no cron job, launchd agent, or Task
Scheduler entry: persistence lives in the queue and in `.dream-state.json`, so
nothing needs installing and interrupted work simply resumes on the next launch
(a one-shot `programmer run` only enqueues; the next interactive launch
consolidates it).

Each pass gives the model the pending transcripts plus the current memory
manifest and asks for a JSON list of `create`, `update`, `supersede`, and
`archive` operations. An unattended pass is deliberately conservative: it only
adds or extends entries the model marked as directly stated or demonstrated in
the session, and it never merges, supersedes, or archives anything on the
model's judgement alone. Those changes are proposed as a preview you can read
and apply:

```
/memory dream            # status: pending sessions, preview, relative last-run time, last error
/memory dream preview    # write an auditable plan without touching memory
/memory dream apply      # apply that plan, then retire the consumed sessions
```

`preview` runs as a cancellable foreground operation without blocking the UI;
Esc cancels its model request, retains queued inputs, and does not apply memories.
Another foreground operation cannot start a concurrent preview. It writes
`.dream-preview.json` in the project memory directory and consumes nothing; `apply` performs no model call at all, so reviewing a plan
costs one request. Both are slash commands only: `dream` is deliberately absent
from the `memory` tool the model can call, so an agent can neither inspect the
queue nor trigger consolidation — and a hand-written tool call naming it is
refused. A pass that is
cancelled, times out, or loses its provider keeps its pending sessions for the
next attempt and records the reason in the status line. New planning failures
include the available underlying error chain in the audit and error output;
HTTP connection failures, client timeouts, and response-body failures are labeled
separately. Request URLs are omitted from HTTP errors to avoid exposing URL
credentials. Earlier audits cannot recover causes that were not recorded.
All passes take one
cross-process lock over the memory root, so two Programmer instances, or an
explicit `apply` racing the background worker, can never interleave their writes;
memory files and their `MEMORY.md` index are still replaced atomically through a
temporary file and rename.

### Dream history and rollback

`/memory dream history` opens the current workspace's run history; append
`session` to include only runs whose input excerpts came from the current session.
The viewer shows recorded plans, source excerpts (not full conversations),
operation outcomes, errors, and memory before/after values. Old processed queue
files are input excerpts, not historical change snapshots: their missing changes
cannot be reconstructed or rolled back.

A recorded generation start is not a live progress indicator. The reader probes
an existing root lock without waiting: if it obtains a shared lock before loading
the record, an unfinished generation is displayed as `Interrupted`. If another
writer holds the lock (or no lock file exists), it displays `Unconfirmed · generation
started`, not a claim that this run is still active. This is a read-only projection:
viewing history neither rewrites audits nor consumes or reruns queued inputs.

The timeline uses padded rows with persistent selection; moving focus into
its details does not remove the selected run's background. Details show inline
before/after changes first, then operation results. Status and diff colors come
from recorded states and snapshots, not from transcript text. `s` discloses source
excerpts using the conversation's Markdown and code highlighting (without copy
buttons); `m` discloses literal metadata and the recorded plan. Detail layouts are
cached across scrolling and unchanged refreshes.

Use arrows/`j`/`k` to select, `Enter` to enter details, and `Esc` to return to the
timeline (then close). `Tab`/Shift+Tab switch regions; mouse clicks select/focus
and the wheel navigates or scrolls. Narrow terminals show one region at a time.
`/` searches and `R`/F5 refreshes. In an applied run, `r` previews **whole-run rollback**;
`y` confirms and `n`/Esc cancels. Any subsequent semantic change to an affected
memory blocks the entire rollback. Usage counts and last-recalled times, unrelated
memories, and later independent changes are preserved. Rollback produces a
separate linked audit; it never requeues the source excerpts. There is no force
rollback or selective per-operation undo.

Dream commits and rollbacks use a recoverable journal shared by all memory
writers. An interrupted commit is shown as incomplete and blocks memory access;
`/memory dream recover` explains recovery, and `/memory dream recover confirm`
explicitly **rolls forward the recorded target**, rather than undoing it. Run it
in the originating workspace. Recovery never repeats a model request. History is
stored in the project memory directory's `history/`; `.dream-transaction.json`
lives at the shared memory root. These records contain local source excerpts and
memory snapshots; they are not sent to a visualization service.

### Session activity graph

`/session graph` opens a read-only three-region view: related sessions at upper
left, the selected session's questions/delegations below, and event details at
right. Each peer appears once, with direction and question/delegation counts;
records with unavailable routing remain accessible in an explicit unknown-peer
group. Event titles prioritize the task text rather than identifiers.

Use ↑/↓ or `j`/`k` in the focused region. `Enter` moves from sessions to events,
then details; `Esc` moves back one region and closes from sessions. `Tab` and
Shift+Tab cycle all three regions. Clicking a region focuses it; the mouse wheel
navigates its list or scrolls details. Selection updates dependent regions without
moving focus. Selected sessions and events retain their full-row backgrounds,
including padding, when focus moves to another region; only the focus indicator
moves. Each peer remembers its selected event, and refresh preserves
selection and detail scrolling. Narrow terminals use full-width detail reading.

`/` searches the selected peer's events, `f` cycles all/questions/delegations,
and Backspace resets event filters without changing the peer. `m` toggles record
metadata (full identifiers, session/workspace information); `R` refreshes and `q`
closes. `e` re-centers on the selected peer without opening or activating it;
reopen `/session graph` to return to the active session. Dream rollback
confirmations still suspend automatic refresh.

Graph history survives inbox consumption in `peer-inboxes/.history/`. Existing
typed conversation records supplement it where available; missing timestamps or
transitions are marked incomplete. History is observational: accepted means
queued, started does not mean completed, and an answered question does not prove
a delegated task finished. Session presence is not inferred from these events.
`peer_session ask` accepts an optional `related_delegation_id` for an explicitly
associated question/report; it grants no permission and implies no completion.
The graph never accepts work, executes it, or wakes a peer. History currently uses
local file scans, not a persistent query index.

While the background worker is consolidating, its title row shows a `💭`
indicator at the far right, so a pass is never invisible; it disappears as soon
as the pass finishes.

Saved sessions use an OS-backed per-session lock. If a second Programmer opens
an in-use conversation, it reports the owning PID and asks whether to fork the
conversation under a new UUID or exit the later process.

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

When Esc requests cancellation, the footer keeps the last observed phase, tool
batch, hook, or response-finalization wait until the matching operation finishes.
`Cancelling` is an acknowledgement wait, not confirmation that a subprocess has
been killed. Queued requests still wait for that terminal acknowledgement.

Automatic compaction observes the real `input_tokens` returned after every API
response. For tool-using responses it waits until all call outputs are recorded,
then summarizes a stable prefix in the background. Ordinary background passes
leave the active turn untouched. At the mandatory limit, the runner pauses at a
safe boundary and compaction can include the completed part of that same turn,
including paired tool calls and outputs. This lets long tasks continue without
repeatedly summarizing only history from before the task. Recent-turn retention
yields to this hard-limit pass; summary-only history is not repeatedly compacted
without new model-visible messages. Input stays usable and new messages remain
outside the snapshotted prefix. Once the summary is installed, the session
is saved immediately when idle (or at the next safe turn boundary), so reopening
the conversation reuses the generated summary. When the summary lands while no
turn is running, the input title announces that the next turn will use compacted
context, and a marker is inserted immediately before that turn's messages when it
starts. When it lands inside a running turn — a compaction forced by the
mandatory limit at a tool boundary — the turn's own next request is already the
first to use it, so no title is raised and the change is recorded by the
`context compacted` divider alone; nothing is left above the input to go stale.
Automatic compaction retries a failed provider request once while keeping the
job active. When the foreground is idle, the footer shows `Compacting` until
that job finishes. If mandatory compaction still fails, queued input is retained
and further model requests remain blocked. Submitting a request again can retry the failed history
prefix; idle polling does not repeatedly retry the same failure. You can also
use `/compact` to compact manually. Esc also cancels background compaction,
including its provider retry; late results are ignored. Cancelling either manual
or background compaction leaves queued input paused until an explicit retry,
rather than immediately starting another compaction.
The read-only `conversation_history` tool is
then exposed to the main agent and its sub-agents, allowing them to search the
exact pre-compaction items and page through a matching message or tool result
when the summary omits a needed detail. A stale summary is discarded after
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

`run` and `init` connect the configured `mcp_servers` before the first turn,
so the agent and its sub-agents get the same `mcp__<server>__<tool>` tools as
the TUI. Servers connect concurrently, and startup counts against `--timeout`.
A server that fails to start is skipped with a warning on stderr; the run
continues without it.

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
history, model/work mode, the `/vision` switch, generated title, latest input
suggestion, and todos are restored independently for each session. Completed
suggestions are saved immediately when idle, so the gray hint is available after
`--resume` without another model request. A background request using
`title_model` (or the current chat model) names a new session from its first
message; the resume picker and `/session` show that title. Once available, the
generated title also replaces the project directory name in the terminal window
title; untitled sessions continue to show the directory name.

Vision defaults to `vision_enabled` from `config.toml` (`true` by default) for
new interactive and headless sessions. Resumed sessions restore their saved
state. `/vision on|off` and `/vision on|off session` change only the current
session; adding `global` also persists the default used by future sessions.

With vision enabled, referencing a local PNG, JPEG, WEBP, or non-animated GIF
as `@path` attaches it as an image input. Disabling vision stops sending both
new and historical images without deleting them from the session; turning it
back on restores them. Other local files are referenced by path only; their
contents are not copied into the request context.

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
| `programmer --resume` | Interactive picker, filtered to the current working directory by default; press `f` to show all sessions |
| `programmer --resume <uuid>` | Resume a specific session |
| `/new` `/n` | Save current session and start fresh |
| `/session` `/s` | Show current session UUID |
| `/title [text]` | Regenerate the current session title, or set it manually |
| `/usage` | Show cumulative usage and the latest request's input-token count |

Session loading distinguishes missing files from read or parse failures. Failed
loads report an error and leave the original file untouched; they are not
silently replaced with an empty session. Failed saves retain the unsaved state
and report an error, but idle ticks do not retry them. A new change or an explicit
save attempt can try again.

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
