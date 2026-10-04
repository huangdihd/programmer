# PROGRAMMER.md — project map for the coding agent

## Release safeguards

- Follow `scripts/RELEASING.md`. Require explicit approval for the current release scope; stage explicit paths, never `git add -A`. Keep local drafts and runtime artifacts out of commits.
- Wait for every CI job on the exact release commit to succeed and review a complete, self-contained version section in `RELEASE_NOTES.md` before publishing. Do not move published tags.
- Release automation must pin the validated SHA, verify notes and all ten artifacts in a draft, and recheck CI before publication. Test policy scripts with `python3 -m unittest discover -s scripts -p 'test_release*.py' -v`.

## Overview

**programmer** is a terminal-based AI coding agent TUI written in Rust. It
connects to OpenAI-compatible APIs (Responses API), streams responses, and
gives the model local tools including `command`, `read_file`, `write_file`,
`edit_file`, `grep`, `blob`, `ask_user`, `configure_diagnostics`, `fetch`,
`task`, `todo`, `memory`, `conversation_history`, and `agent`,
plus MCP-bridged external tools. The TUI is built with Ratatui and crossterm.
The binary is a single crate at the repo root.

Key features beyond the chat loop:
- **Collapsible usage**: each response defaults to `▸ total tokens(cached% cached) · N memories`; click the header to inspect color-coded token details and the turn's injected memory snapshots from the transcript (never the current memory store). Missing historical snapshots are explicitly unavailable; detail rows do not toggle the disclosure.
- **Multi-provider**: add/edit/delete/switch API backends at runtime.
- **Multi-session**: UUID-keyed JSON persistence in `~/.config/programmer/sessions/`, with per-session OS locks and an interactive fork-or-exit conflict flow.
- **Cross-session collaboration**: TUI-only `peer_session` tool (`list`, `search`, `ask`, `delegate`, `cancel`), with paginated local literal search over saved messages/summaries including pre-compaction history (excluding tool payloads/reasoning), implemented in `src/peers/` with App scheduling in `src/app/peers.rs`. `cancel` requires the target session and returned delegation ID, removes only not-yet-started work, and dismisses an already-open consent prompt; it cannot stop running work. UI-only persisted `PeerExchange` items render one purple collapsed row updated in place from question to answer, separate from API-visible untrusted developer wrappers. UI-only `PeerDelegation` records similarly upsert by original delegation ID with typed Pending/AcceptedQueued/Started/Rejected/Cancelled observations; their task-first purple header and status explanation expand to source/full task. These records persist and support conversation_history but never enter API/classifier context. Durable inbox consent is local-only: reopening resets the display to Pending and asks again. Strict accepted/rejected/started/cancelled wire statuses recover task text only from durable delegation history or a pending envelope matching the original ID and both endpoints; unmatched notices remain informational. Targets send Started back to the source after starting, without inferring completion or granting consent from statuses. Peer consent uses a bounded bottom `QuestionPanel::delegation` (default No, horizontal choices, D disclosure and page scrolling; M opens a searchable live `/model` catalog using `CompletionPopup`, returns without consent, and silently switches the current session through the shared settings path. Esc closes only the picker; rejection retains the model. Existing runners own model snapshots; persistence uses the normal dirty/idle boundary). Questions use a single tool-free context snapshot response, including offline sessions; only user-opened targets queue an additional full agent turn. Delegation requires local Yes/No, supports new workspace sessions with a resume command, and never spawns a target automatically. Durable local inboxes and separate TUI-presence locks distinguish open sessions from temporary inquiry workers. The App schedules housekeeping ticks from startup even while idle so incoming delegations do not depend on a user turn. Drafts, approvals, and explicit Stop block peer auto-execution; clarification/completion uses `ask`, not a separate team/thread API.
- **Activity viewer**: `/session graph` projects durable peer observations (`src/peers/graph.rs`, `peer-inboxes/.history/`) plus available legacy UI messages into a read-only, re-centerable relationship graph/timeline. `ask.related_delegation_id` is explicit metadata, never completion/permission. `src/app/activity.rs` adapts task-first records and separate provenance to `src/ui/components/activity_panel.rs` and its `graph.rs`/`dream.rs` children. Dream uses adapter-supplied typed status and styled snapshot diffs, with source (`s`) and metadata (`m`) disclosure; source Markdown shares the conversation renderer/code-block hooks with copy controls suppressed. Its selected detail layout caches width/disclosure/content state, retains unchanged refreshes, and uses prefix row offsets to paint only visible logical lines; Enter/Esc descend/ascend timeline → details, with mouse navigation and full-width narrow details. Both viewers retain padded full-row selection backgrounds independent of focus. Graph focus is sessions → selected peer's events → details (Enter descends, Esc ascends, Tab/BackTab cycle); peers aggregate by full ID with an explicit unknown-route group. Per-peer selection and detail scroll survive refresh, mouse hits are local to rendered regions, and `m` discloses full metadata. Narrow terminals read details full-width. Modal routing blocks queued execution and owns Esc/paste/mouse; Dream confirmation behavior remains separate.
- **Input suggestions**: after successful turns, a configurable model predicts the next user message as an accept-with-Right placeholder. Acceptance fills the draft without consuming the suggestion; deleting all draft text reveals it again. Submission/session reset still clears the old suggestion.
- **Rewind checkpoints**: prompt-level conversation checkpoints plus content-addressed snapshots for built-in file edits.
- **Context compaction**: manual and provider-usage-triggered background summaries with session overrides, a configurable post-auto-compaction turn cooldown (mandatory/manual compaction bypass it), immediate safe-boundary persistence for generated summaries, a parsed structured working-state snapshot, and read-only search/paging over exact pre-compaction items. The footer projects an active mandatory-compaction wait as Compacting rather than the runner's stale tool phase; cancellation and user-input waits retain precedence, and ordinary background compaction does not mask foreground tools. Background compaction owns a cancellation token shared by provider retries; Esc invalidates late results and pauses queued work until explicit retry, including when no foreground operation exists. Mandatory compaction at a suspended runner safe point includes completed work within the active turn, overriding recent-turn retention when necessary; ordinary concurrent background passes remain bounded before that turn. Do not repeatedly compact summary-only prefixes. A full-history safe-point replacement uses summary usage rather than retaining a phantom tail from cross-provider token subtraction.
- **Persistent memory**: inspectable global/project Markdown indexes and per-entry files, explicit management, automatic per-turn association where the dedicated memory model sees the recent conversation and candidates ranked by kind, confidence, and freshness without keyword filtering (default global/project caps 30/8), excluding memory IDs already added or recalled in live post-compaction context before applying candidate limits, Claude Code style time decay with per-kind half-lives and recall reinforcement, per-memory age plus a staleness caveat on injected memories older than a day, a non-blocking first-step prefetch with one cancellable later-step grace period, reasoning-disabled association requests with a named timeout budget and an informational line when association fails, reinforcing explicit recalls and age-tagged list/recall output, the same association for sub-agents (which get recalled memories but not the memory-mechanics system prompt), legacy JSON migration, and credential rejection.
- **Background consolidation (Dream)**: finished sessions are queued (user/assistant prose only) and consolidated by an in-process worker once both the queued-session and interval thresholds are met; an unattended pass only adds or extends entries the model marked as directly stated or demonstrated, while merges, supersedes, and archives are proposed as a `/memory dream preview` plan that `/memory dream apply` commits without a second model call. Manual preview runs in a spawned foreground operation, not inside the awaited UI command handler; `DreamPreviewFinished` releases the matching operation owner on success, failure or cancellation. Esc cancels planning and records a cancelled generation failure without consuming inputs. Planning errors preserve a bounded Display/source cause chain, classify HTTP transport failures, and strip request URLs before formatting; they never use full request/debug dumps. Passes hold one cross-process lock over the memory root, memory files are replaced atomically, and a cancelled, timed-out, or provider-less pass keeps its queue for the next attempt. Consolidation is user-only: `dream` is absent from the model's `memory` tool (a call naming it is refused) and `💭` is shown at the right edge of the title row while a background pass runs.
- **Dream audit and rollback**: `/memory dream history [session]` displays persisted runs, excerpts, outcomes and before/after snapshots. `src/memory/dream/history.rs` owns whole-run conflict-checked rollback, linked independent rollback audits, and a recoverable cross-scope journal. Ordinary memory access shares the root writer lock. History readers hold a nonblocking shared lock before loading records when available: unfinished Generating audits project to Interrupted only when no writer owns the root; otherwise the UI says Unconfirmed, never inferring per-run liveness from a busy root lock. This does not rewrite audits; transaction validation uses raw history. Incomplete transactions fail closed until `/memory dream recover confirm` explicitly rolls forward in the originating workspace; rollback never requeues excerpts or resets recall counters. Legacy inputs alone cannot be rolled back.
- **Auto-mode classifier**: per-mode LLM classifier that approves/denies/defers tool calls.
- **MCP (Model Context Protocol)**: connect to external MCP servers (stdio + HTTP);
  their tools are advertised to the model as `mcp__<server>__<tool>`.
- **Skills**: user-authored `SKILL.md` files that inject prompt segments (Vercel Labs compatible).
- **Background tasks**: shell commands that run detached; shown in the sidebar.
- **Multi-agent**: up to three in-process child runners with independent conversations,
  parent-forwarded approvals, completion delivery, live sidebar inspection, a per-child
  sidebar row showing the running phase (tagged with the agent id so it never touches the
  main turn's status bar), and the same automatic memory association as the main session
  (child runners resolve the configured `memory_model` target rather than inheriting a
  resolved client, so a per-agent model override does not disable recall).
- **Todo list**: per-session task tracking with a `todo` tool and a sidebar panel.
- **Diagnostics pipeline**: IDE-style error/warning feedback after edits (command + LSP backends).
- **Slash-commands**: `/init`, `/model`, `/mode`, `/skill`, `/mcp`, `/todo`, etc. with tab-completion.

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

## App ownership refactor

- `App.ui: UiState` owns interaction and display components; `App.agent_loop: LoopState` owns turn scheduling, a complete pending `UserRequest`, execution phase, active runner snapshot and worker handles.
- `LoopState.session: SessionState` owns the shared `Conversation`, execution settings, authoritative todos, diagnostics (including LSP configuration), checkpoints, tasks and agents. `SessionSnapshot` is the serialized DTO, not a running session. Existing JSON fields remain compatible. Main loads `SessionSeed` after acquiring the session lock; `App::new` consumes that seed without re-reading storage.
- Todo panels emit add/toggle/delete intents; display snapshots never overwrite the whole authoritative list. Replacing a snapshot clamps selection. ConversationPanel only manages display state: domain writes go through Session/Conversation, followed by explicit scroll/cache notifications.
- Runner commits successful and interrupted responses. UI partial responses and aborted-stream cache handoffs are projections, never a second history writer. Cancelled function calls are filtered; preserved failed calls receive paired failure outputs. Runner's argument guard stops runaway tool-argument generation before forwarding the offending event or executing any call from that response. Unambiguous calls receive sanitized arguments and paired failures for normal model recovery; ambiguous identities fail the response. Three consecutive generation-limit failures stop the turn.
- `McpRuntime` is the stable reload owner; `McpManager` remains a connection-set snapshot retained by in-flight runners. ProviderManager owns model discovery statuses and per-provider refresh ownership: only the latest initiated refresh may update that provider's catalog/status; stale or duplicate completions are ignored. Discovery errors remain provider-keyed until acceptance, and refresh notifications count only accepted results. ActivityPage pairs ActivityBrowserState with its panel; Sidebar owns its last visible hit-test region.
- PeerInbox owns transport/presence/lightweight answering; Loop owns nonpersistent delegation authorization; UI owns queued questions. Durable inbox files survive session shutdown.
- Session shutdown closes task/agent admission, releases approvals, cancels and joins work (including the task event forwarder) before replacing session state. `/clear` rejects active turns and closes background work before resetting resources. Ordinary Esc only cancels the current turn and preserves queued successors. Errors are reported, not advertised as successful shutdown/save.
- Architecture prototypes, ownership diagrams, and boundary acceptance notes under `design-demos/architecture-*` and `app-loop-workbench*` are local-only materials, not distributed repository documentation. The explicitly ignored files are optional and must not be assumed present in a fresh checkout.

## Key directories

```
.programmer/                  # Per-project data
├── diagnostics.toml          #   Diagnostics checker profile
└── skills/<name>/SKILL.md    #   Project-specific agent skills

src/
├── main.rs                   # Entry: arg parsing, terminal init, config load, App::run
├── agents/                   # In-process sub-agent registry, runtime, and lifecycle
├── consts.rs                 # Tunable constants (output length, concurrency, tick rate, …)
├── prompts.rs                # Centralised system prompt + classifier instructions
├── cancel.rs                 # CancellationToken and typed foreground OperationId
├── clipboard.rs              # Copy-to-clipboard (OSC 52)
├── terminal.rs               # TerminalGuard: raw-mode enter/restore
│
├── app/                      # Application core
│   ├── mod.rs                #   App.ui / UiState presentation; App.agent_loop / LoopState runtime and SessionState ownership
│   │                        #   SessionState owns execution settings, todos, diagnostics, checkpoints, tasks and agents; explicit paths, no Deref
│   ├── lifecycle.rs          #   Foreground begin/cancel/finish transitions and stale-event ownership
│   ├── scheduling.rs         #   UserRequest text/images, StartupState + WorkSource pure startup policy
│   │                        #   events::handle_event centrally settles queued work after every event; no per-completion handoffs
│   │                        #   Local work precedes peer execution; drafts/approvals/mandatory waits block, idle notifications debounce 200ms
│   │                        #   Failed mandatory compaction blocks automatic retry until explicit resubmit/compact or history reset
│   ├── commands.rs           #   Slash-command dispatch (/:init, :model, :mode, :skills, :mcp, …)
│   ├── diagnostics.rs        #   Diagnostics snapshot + diff integration
│   ├── events/               #   Key + mouse event routing
│   │   ├── keys.rs
│   │   └── mouse.rs
│   ├── helpers.rs            #   Small utilities (drain approvals, mode transitions, …)
│   ├── session.rs            #   Session save/load hooks
│   └── surface.rs            #   TUI adapter for the shared runner's host interface
│
├── checkpoint.rs             # Per-session rewind manifests, blobs, conflict-safe restore
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
├── memory/                   # Layered persistent memory store, freshness-weighted retrieval, and L1 session state
│   ├── mod.rs                #   Markdown store, retrieval, freshness, atomic writes
│   ├── dream.rs              #   Pending queue, background worker, consolidation plans, apply
│   └── dream/history.rs      #   Audits, semantic rollback, cross-scope journal/recovery
│
├── mcp/                      # Model Context Protocol integration
│   ├── mod.rs                #   McpManager: connect, discover tools, route calls
│   ├── types.rs              #   JSON-RPC types, McpTool, McpServerConfig, tool annotations
│   ├── client.rs             #   Stdio transport (spawn + JSON-RPC over stdin/stdout)
│   └── http_client.rs        #   HTTP SSE transport
│
├── providers/                # Multi-provider management
│   └── mod.rs                #   ProviderManager: add/edit/delete/switch API backends
│
├── conversation.rs           # Shared conversation model and API-input projection
├── runner/                   # Shared turn execution for TUI, headless, and child agents
│   ├── mod.rs                #   TurnRunner and execution events
│   ├── surface.rs            #   AgentSurface: host notifications, review, and interaction
│   ├── hooks.rs              #   TurnHook: checks and feedback around tool batches
│   ├── tools.rs              #   Ordered tool batches through ToolRegistry
│   └── stream.rs             #   API streaming and retry
│
├── response/                 # API response parsing
│   ├── mod.rs
│   ├── message_item.rs       #   MessageItem records and shared input-text extraction
│   ├── partial_response.rs   #   Streamed partial response accumulator
│   └── response_finish_reason.rs
│
├── session/                  # Multi-session persistence
│   └── mod.rs                #   SessionManager, SessionSnapshot (compatible JSON DTO), list/pick
│
├── skills/                   # Agent skills (Vercel Labs compatible)
│   ├── builtin/              #   SKILL.md files compiled into the binary
│   ├── mod.rs                #   Skill discovery (built-in + global + project), shadowing
│   └── skill.rs              #   Skill: name, description, body, source, constraints
│
├── tasks/                    # Background task system
│   └── mod.rs                #   Clone-shared TaskManager per app/server, status/io/kill
│
├── todos/                    # Per-session todo list
│   └── mod.rs                #   Todo, TodoList, sync via ~/.config/programmer/todos.json
│
├── tools/                    # Tool definitions + execution
│   ├── mod.rs                #   Tool enum, tools() list, shell(), resolve_program(), environment_info()
│   ├── provider.rs           #   ToolProvider contract and ToolRegistry routing
│   ├── command.rs            #   Shell command execution
│   ├── read_file.rs          #   Read file with offset/limit
│   ├── write_file.rs         #   Write/create file (whole-file replacement)
│   ├── edit_file.rs          #   Unique/replace-all and validated sequential batch edits
│   ├── grep.rs               #   Regex search across files
│   ├── blob.rs               #   File glob (find by name pattern)
│   ├── ask_user.rs           #   Prompt user for input (yes/no, multi-choice, text)
│   ├── configure_diagnostics.rs  # Write .programmer/diagnostics.toml
│   ├── conversation_history.rs   # Search/page exact items hidden by compaction
│   ├── diagnostics.rs        #   Run diagnostics + return current errors/warnings
│   ├── fetch.rs              #   HTTP fetch (html2text conversion)
│   ├── task.rs               #   Background task management (create/list/output/write/wait/kill)
│   ├── agent.rs              #   Sub-agent spawn/list/result/wait/cancel lifecycle
│   ├── memory.rs             #   Persistent memory remember/recall/list/update/forget tool
│   ├── todo.rs               #   Todo list management (add/list/update/delete)
│   └── mcp_bridge.rs         #   Internal: route MCP-prefixed calls to McpManager
│
└── ui/                       # Terminal UI (Ratatui)
    ├── mod.rs
    ├── ui.rs                 #   Main UI layout + render dispatch
    ├── event.rs              #   Event + EventHandler enums
    ├── text.rs               #   Text styling helpers
    ├── markdown_code_block.rs    # Syntax-highlighted code block widget
    ├── markdown_theme.rs         # Original dark Markdown colour palette
    ├── theme.rs                  # auto/light/dark config, detection and final-frame conversion
    ├── tool_details.rs       #   Tool-call detail popup (arguments + output)
    └── components/
        ├── mod.rs
        ├── agent_panel.rs        # Live read-only child conversation viewer
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
        ├── messages/             # Per-message-type renderers; common left inset belongs to transcript_content_area()
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

- **Runner boundaries:** Production runner code depends on `AgentSurface`, not App or UI event types. Tool calls receive a narrow `QuestionHandler` through `ToolCtx`; `app/surface.rs` adapts questions to TUI events synchronously. Cancellation and waiting remain in the tool. Child-agent lifecycle notifications are bound in `AgentRuntime`, not passed through generic tool context. Shared input-text extraction belongs to `response/message_item.rs`.
- **Error handling:** `color_eyre::Result<T>` throughout; `.wrap_err()` for context; `?` propagation. `thiserror` for library-style error types.
- **Cancellation and queues:** Foreground events carry `cancel::OperationId`; zero is explicitly `UNTAGGED` for non-turn surfaces. `app/lifecycle.rs` centralizes begin/cancel/finish transitions: cancellation retains ownership until a terminal event, while non-terminal events require a live token. Esc cancels only the active request, not the queue. Dispatch waits for its matching terminal event; stale events cannot advance the queue. User drafts, approvals, and peer consent still block dispatch. Before any model output, restore the original draft only into an empty input with no queued user request; otherwise retain the cancelled input in conversation history. Task/agent notifications and already-authorized peer work remain eligible after cancellation, without restarting the cancelled request.
- **Async:** `#[tokio::main]` on `main()`, `tokio::spawn` for concurrent tasks. All tool execution is async.
- **Configuration:** `ProgrammerConfig` deserializes from TOML via the `config` crate. Environment variables prefixed with `Programmer` override file values. Config lives at `~/.config/programmer/config.toml`. The optional top-level `soul` value replaces only the identity/mindset section of the developer prompt.
- **Markdown links:** Assistant inline HTTP(S) links are resolved from original cached paragraph styles by `conversation_panel/links.rs`; stationary clicks launch the browser without a shell. Drags cancel activation; ambiguous labels and unsupported link forms are inert.
- **UI themes:** `/theme [auto|light|dark]` persists the top-level `theme` setting (default `auto`). Startup OSC 11 background detection shares the ratatui-image query; missing results fall back to dark. `ui/theme.rs` resolves paired semantic UI-role tokens (decorative headings, borders, tool summaries, and status colors are distinct) at frame presentation, leaving cached Markdown styles intact and child terminal colors untouched.
- **Sessions:** Stored as JSON under the platform config directory's `programmer/sessions/<uuid>.json`. Each session contains message items, history, the latest input suggestion, todos, and persisted task state. `SessionManager::load` distinguishes missing files from typed read/parse failures without renaming the original. App persistence builds an owned snapshot and acknowledges dirty state only after a successful save. `PersistenceState` allows one idle attempt per new change; a failed save is not retried by ticks or timers. New changes or explicit saves may attempt again.
- **Task ownership:** Each application/server owns a clone-shared `TaskManager`; tool providers, child agents, task rendering, and persistence use that same instance. Independent managers isolate task IDs, lifecycle events, generation, and cleanup. Do not reintroduce a global registry.
- **Module visibility:** `pub(crate)` for internal visibility; `pub` only where needed externally. UI internals are `mod` (private). Tool modules are `pub` within `tools/`.
- **Tests:** Primarily inline `#[cfg(test)]` modules at the bottom of source files, plus CLI integration tests under `tests/`.
- **Copyright header:** GPL-3.0-or-later header block on every `.rs` file.
- **Naming:** snake_case for modules/functions, CamelCase for types.
- **No `unwrap()` in production code:** Prefer `?`, `.unwrap_or_default()`, or explicit `match`.
- **The diagnostics system** is language-agnostic: it reads `.programmer/diagnostics.toml` for checker definitions. Each checker can be a one-shot command (parsed via `rustc-json`, `tsc`, `gnu`, or regex) or an LSP server (`kind = "lsp"`). The `configure_diagnostics` tool writes this file.
- **Constants** live in `src/consts.rs` — tunable values like output length limits, concurrency caps, tick rate, and classifier budgets.
- **Prompts** are centralised in `src/prompts.rs`: system prompt, classifier instructions, plan-mode injection, and post-edit reminders. Successful `write_file`/`edit_file` calls append the reminder once per turn unless that turn successfully edits `PROGRAMMER.md`; shell commands do not trigger it. `src/app/setup.rs` owns the skippable, one-time global first-launch guide for configuring providers and assigning chat/classifier/compact/memory/title/suggestion model roles.
- **MCP integration** supports both stdio and HTTP transports. Tools are prefixed `mcp__<server>__<tool>` and merged into the advertised tool list.
- **Skills** are compiled from `src/skills/builtin/<name>/SKILL.md` or discovered from `~/.agents/skills/<name>/SKILL.md` (shared), the platform config directory's `programmer/skills/<name>/SKILL.md` (global), and `.programmer/skills/<name>/SKILL.md` (project). Precedence is project > global > shared > built-in.
