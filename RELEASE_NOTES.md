# programmer v0.2.0

v0.2.0 is the largest Programmer release so far. It turns the original coding
TUI into a multi-provider agent environment with native safety controls,
diagnostics, MCP and skill extensibility, background terminals, sub-agents, and
headless automation.

## Highlights

### Safer autonomy

- Added Manual, Auto, Plan, and optional YOLO work modes.
- Added an LLM-based Auto-mode classifier with retries and a reasoned fallback.
- Added configurable file access rules, stale-edit protection, named security
  profiles, and scoped permission requests.
- Added native process sandboxing on supported Unix platforms for commands,
  tasks, diagnostics, LSP servers, and stdio MCP servers.

### A complete agent workspace

- Added automatic post-edit diagnostics, lint severity, persistent LSP support,
  diagnostic references, and model-free `programmer diagnostics` checks.
- Added background tasks, live command output, interactive PTYs, terminal
  viewing, command promotion, and completion notifications.
- Added in-process sub-agents with independent conversations, concurrency and
  cancellation controls, permission forwarding, and sidebar status.
- Added session-scoped todos and a persistent right-hand sidebar for providers,
  MCP servers, skills, agents, tasks, todos, and diagnostics.

### MCP and skills

- Added stdio and HTTP MCP clients with progress reporting, management UI, and
  read-only tool metadata support.
- Added `programmer mcp stdio` and `programmer mcp http` to expose Programmer's
  own tools to other agents; HTTP mode includes an interactive approval console.
- Added built-in, shared, global, and project-local skills. Skills are enabled
  by default and loaded on demand.
- Added built-in `programmer-guide`, `initialize-project`, and
  `update-programmer-md` skills. `/init` and headless initialization now share
  the same skill-driven workflow.

### TUI and multimodal improvements

- Added image references, clipboard image paste, `read_image`, and terminal
  graphics rendering with graceful fallback.
- Added provider management, model browsing and refresh, thinking-level control,
  per-turn usage, session management, tab completion, and incremental search.
- Added context-aware tool grouping, absorbed streaming thoughts, reasoning
  summaries, compacting status, and clearer tool, usage, info, and error rows.
- Added `/compact [provider/model]`, native terminal selection mode, a jump-to-
  bottom indicator, and scroll preservation when a response completes.
- Improved cancellation, including reliable Escape handling and a two-step
  exit guard.

### Automation and distribution

- Added a shared turn engine used by both the TUI and headless operation.
- Added `programmer run`, `programmer init`, and `programmer diagnostics` with
  text, JSON, and JSONL output, model and classifier selection, work-mode and
  thinking controls, timeouts, step limits, and final diagnostic checks.
- Added macOS/Linux and Windows installers for prebuilt GitHub Release assets.
- Added `programmer upgrade`, update notifications, and `programmer uninstall`.

## Upgrade notes

- Existing single-provider configuration is migrated to the multi-provider
  format automatically.
- Existing security settings are migrated into the default named security
  profile.
- Process sandboxing is enabled by default on supported Unix platforms. If a
  command needs additional filesystem access, configure it through
  `/permission manage` or a named security profile.
- Auto mode works best with a non-reasoning classifier model. Configure one with
  `/classifier provider/model` or `classifier_model` in `config.toml`.

## Install

macOS or Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/huangdihd/programmer/main/scripts/install.sh | sh
```

Windows PowerShell:

```powershell
Invoke-WebRequest `
  -Uri "https://raw.githubusercontent.com/huangdihd/programmer/main/scripts/install.ps1" `
  -OutFile "install.ps1"
powershell -ExecutionPolicy Bypass -File .\install.ps1
```

Prebuilt binaries are published for x86_64 and ARM64 macOS; x86_64, ARM64, and
x86 Windows; and x86_64, ARM64, ARMv7, RISC-V 64, and i686 Linux.

**Full changelog:** https://github.com/huangdihd/programmer/compare/v0.1.1...v0.2.0
