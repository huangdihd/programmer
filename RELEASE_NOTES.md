### programmer v0.2.23

- Separate UI, execution-loop, and session ownership; preserve interrupted
  responses and isolate task, agent, todo, and checkpoint state across sessions.
- Seal, cancel, and join session work before replacement or shutdown; retain
  retryable state on close/save failures without stranding queued input.
- Keep in-flight MCP connection snapshots valid during reload and reject stale
  provider refresh results, including outdated errors and notification counts.
- Recover delegated task text from durable identity-checked evidence and return
  Started status to the source without inferring completion or persisting consent.
- Return full Todo IDs and distinguish cancelled approvals from classifier denials.
- Stop runaway tool-argument generation before execution, return sanitized paired
  tool failures through the normal agent loop, and bound consecutive failures.
- Expand protocol, lifecycle, shutdown, persistence, and local MCP regressions;
  document actual TUI/process boundary acceptance and current ownership diagrams.

**Full changelog:** https://github.com/huangdihd/programmer/compare/v0.2.22...v0.2.23

---

### programmer v0.2.22

- Add browsable Dream audits with source excerpts, inline before/after changes,
  conflict-checked whole-run rollback, and explicit interrupted-transaction recovery.
- Add a read-only session relationship graph and event timeline with persistent
  ancestor selection, padded rows, keyboard/mouse navigation, and narrow layouts.
- Keep Dream preview responsive and cancellable without consuming queued inputs;
  distinguish abandoned generation records from unconfirmed activity without
  rewriting audits, and show readable relative times in Dream status.
- Cache Dream detail layouts, render only visible content, and share conversation
  Markdown and code-block styling for source excerpts.
- Centralize ready-work dispatch so cancellation and compaction cannot strand
  queued input, while preserving draft, approval, and mandatory-compaction gates.
- Show mandatory compaction waits instead of stale Running tools status, group
  consecutive tools starting at two calls, and restore accepted input suggestions
  when the draft is deleted.
- Expand lifecycle, rendering, rollback, scheduling, and input regression coverage
  and update usage and architecture documentation.

**Full changelog:** https://github.com/huangdihd/programmer/compare/v0.2.21...v0.2.22

---

### programmer v0.2.21

- Make operation ownership explicit and preserve queued requests until cancellation
  finishes, without bypassing drafts, approvals, or peer consent.
- Show the last observed execution stage while cancelling, and interrupt pending
  hooks and memory-association waits promptly.
- Publish explicit diagnostics results to the sidebar from the same snapshot,
  rejecting stale updates and retaining known findings after incomplete checks.
- Preserve session files on load failures and keep unsaved changes after save
  failures, without automatic idle retries.
- Isolate task managers per runtime and strengthen runner/tool boundaries with
  typed operation identifiers and narrow question handlers.
- Add lifecycle, persistence, task isolation, diagnostics, and cancellation
  regression coverage and update architecture documentation.

**Full changelog:** https://github.com/huangdihd/programmer/compare/v0.2.20...v0.2.21

---

### programmer v0.2.20

- Preserve raw Responses API reasoning from `reasoning_text.done` events without
  preceding deltas and from reasoning `content_part.done` events.
- Add synthetic SSE regressions for mixed raw/summary reasoning, done-only
  streams, dual-field serialization, invalid targets, and malformed events.
- Add race-safe cancellation for peer delegations that have not started, with
  durable source checks, terminal cancelled lifecycle records, and consent cleanup.
- Let lightweight peer answers use the provider's normal output-token default
  instead of imposing a 2,048-token cap.

**Full changelog:** https://github.com/huangdihd/programmer/compare/v0.2.19...v0.2.20

---

### programmer v0.2.19

- Add cross-session search, questions, and durable delegation with local consent,
  explicit lifecycle records, and an in-panel model picker. Offline inquiries
  use a tool-free snapshot; delegated work never starts a target automatically.
- Keep queued work eligible after Escape cancels the active request, without
  overwriting drafts or bypassing approvals and peer consent.
- Align transcript padding and user gutters, improve peer message rendering,
  and fold token usage and injected-memory details into expandable rows.
- Select relevant memories with a strict JSON schema restricted to candidate IDs.
- Retry failed automatic compaction once, show its active state, and retain
  queued input when mandatory compaction remains blocked.
- Include collaboration and usage design previews and behavioral regressions.

**Full changelog:** https://github.com/huangdihd/programmer/compare/v0.2.18...v0.2.19

---

### programmer v0.2.18

- Remove keyword filtering from memory candidates and increase the default global
  candidate limit from 3 to 30. The memory model selects up to five relevant entries.
- Exclude memories already added or recalled in live context before applying
  candidate limits. Compacted-away records become eligible again; retained turns
  still count. Apply context tracking to interactive, headless, and child agents.
- Show unavailable or incomplete token accounting when responses omit usage or
  report all-zero counts, preserving the last positive context count.
- Allow exact filesystem permission requests while the process sandbox is off,
  including on Windows. Filesystem restrictions and user approval still apply.
- Render sequential `edit_file` batches as individual multiline diffs instead of
  displaying escaped JSON.

**Full changelog:** https://github.com/huangdihd/programmer/compare/v0.2.17...v0.2.18

---

### programmer v0.2.17

- Add persistent `/theme auto|light|dark` with startup terminal background detection,
  a complete light palette, matching welcome border, and light completion selection.
- Open inline HTTP(S) Markdown links and the welcome card's project GitHub link
  in the default browser; dragging text never activates a link.
- Add `edit_file` `replace_all` and sequential `edits` batches. Validate the entire
  batch before writing, preserving the file when any edit fails.
- Include the reviewed light-theme preview and update usage documentation.

Auto theme detection falls back to dark when unavailable and uses the startup
result, not live terminal theme notifications. Markdown link activation is
conservative: unsupported or ambiguous destinations remain inert.

**Full changelog:** https://github.com/huangdihd/programmer/compare/v0.2.16...v0.2.17

---

# programmer v0.2.16

v0.2.16 adds background memory consolidation, session-level vision controls,
and richer, more compatible tool integrations.

## Highlights

- Add Dream background consolidation for completed interactive and headless
  sessions, with conservative automatic additions and auditable preview/apply
  workflows for broader memory changes.
- Add `/vision on|off [session|global]` and a persisted default for controlling
  image input without discarding stored images.
- Preserve image content returned by MCP tools as multimodal model input instead
  of flattening it to text.
- Generate built-in tool parameter schemas from their Rust argument types and
  centralize slash-command metadata to keep parsing, help, and completion in
  sync.

## Safety and reliability

- Keep Dream unavailable to model-invoked memory tools, reject transcripts that
  appear to contain credentials, serialize cross-process consolidation, and
  retain queued work after cancellation or provider failures.
- Respect MCP read-only annotations when deciding whether a tool needs approval,
  while conservatively classifying tools without an explicit hint.

**Full changelog:** https://github.com/huangdihd/programmer/compare/v0.2.15...v0.2.16

---

# programmer v0.2.15

v0.2.15 improves session continuity, streaming responsiveness, and robustness
against malformed provider data.

## Highlights

- Persist completed sub-agent entries across session resumes and preserve
  rewind checkpoints when a conversation is forked.
- Separate high-rate streaming redraws from lower-rate status housekeeping for
  smoother output without unnecessary idle refreshes.
- Improve provider startup and model-management error handling, Unicode-safe UI
  truncation and highlighting, and live Markdown rendering behavior.

## Fixes

- Bound provider-controlled output and reasoning-part indices to prevent
  malformed streaming events from causing excessive memory allocation.
- Saturate oversized file offsets and limits instead of overflowing.
- Continue auto-scrolling a mouse text selection while the pointer rests at
  the top or bottom edge, without requiring horizontal movement.
- Normalize malformed LSP positions and harden several session and UI edge
  cases.

**Full changelog:** https://github.com/huangdihd/programmer/compare/v0.2.14...v0.2.15

---

# programmer v0.2.4

v0.2.4 focuses on streaming rendering performance and dependency updates.

## Highlights

- Render streaming Markdown off the UI thread: long assistant responses no
  longer block input handling or redraws while tokens arrive.
- Virtualize and cache conversation panel layout work so long conversations
  scroll and render smoothly.
- Optimize streaming Markdown rendering for fewer intermediate re-parses.

## Dependencies

- Bumped async-openai from 0.41.1 to 0.41.3.
- Bumped serde from 1.0.228 to 1.0.229.

**Full changelog:** https://github.com/huangdihd/programmer/compare/v0.2.3...v0.2.4

---

# programmer v0.2.2

v0.2.2 adds recoverable conversation navigation, automatic context management,
and in-app diagnostics configuration.

## Highlights

- Added `/rewind` checkpoints with independent code/conversation restore modes
  and conversation forking that preserves the source session.
- Restored an unsent prompt to the input editor when a request is cancelled
  before the model begins responding.
- Added manual and automatic context compaction with configurable global and
  session-level models, token thresholds, and recent-turn retention.
- Added `/diagnostics manage` and `/diagnostics update` for editing and
  refreshing diagnostics without leaving the TUI.
- Added global classifier and compaction model controls to provider management,
  plus model search and modal keyboard-routing fixes.
- Made `classifier_top_logprobs` a single global setting; `/classifier
  logprobs` now updates and persists that value directly.

## Fixes

- Fixed Escape in management panels cancelling the active conversation.
- Fixed provider model filtering swallowing characters used by Vim-style
  navigation and improved model-list colors and refresh feedback.

**Full changelog:** https://github.com/huangdihd/programmer/compare/v0.2.1...v0.2.2

---

# programmer v0.2.1

v0.2.1 makes the Auto-mode classifier compatible with providers that impose a
smaller `top_logprobs` limit than OpenAI.

## Fixes

- Added the persisted `classifier_top_logprobs` setting with validation for the
  Responses API range of 0–20.
- Added `/classifier logprobs <0-20>` to inspect and change the setting without
  editing `config.toml` manually.
- Passed the configured value through TUI, headless, child-agent, and MCP
  classifier paths.
- Documented that Qwen endpoints accept at most 5 top logprobs.

**Full changelog:** https://github.com/huangdihd/programmer/compare/v0.1.1...v0.2.1

---

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
