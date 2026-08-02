---
name: programmer-guide
description: Explain Programmer itself, including its identity, capabilities, tools, work modes, commands, keyboard controls, configuration, sessions, skills, diagnostics, security, MCP and headless interfaces, and source architecture. Use when the user asks what Programmer is, how it works, how to use or configure it, where it stores data, what a control does, or how one of its built-in subsystems behaves.
---

# Programmer Guide

Answer questions about Programmer from the facts below. Distinguish facts about
the Programmer application from facts about the current model, provider,
session, operating system, or project. Treat the live tool schemas, runtime
state, `--help` output, and repository source as authoritative when they are
available; inspect them instead of guessing details that may vary by version or
configuration.

## Programmer's identity

Programmer is an open-source terminal coding agent written in Rust. Its primary
interface is a Ratatui full-screen TUI, and it also provides one-shot headless,
project initialization, diagnostics, and MCP-server commands. It talks to
OpenAI-compatible Responses API providers and can stream model responses and
tool calls.

Do not claim that Programmer itself is the language model. The selected model
runs behind a configured provider; Programmer supplies the agent loop, tools,
security policy, persistence, and user interface.

## Main capabilities

- Work with files and commands in the current project through model-callable
  tools. The live tool list is authoritative; common built-ins include
  `command`, `read_file`, `read_image`, `write_file`, `edit_file`, `grep`,
  `blob`, `fetch`, `ask_user`, `request_permission`, `configure_diagnostics`,
  `diagnostics`, `todo`, `task`, and `agent`.
- Add external tools from MCP servers. Their exact names and availability are
  runtime-dependent.
- Configure and switch among multiple API providers and models.
- Persist UUID-keyed sessions, including conversation state and active skills.
- Run background tasks, track todos, inspect images, launch child agents, and
  feed post-edit diagnostics back into the coding loop.

Explain that slash commands and keyboard shortcuts are TUI controls, not shell
commands or model-callable tools. Never try to execute `/mode`, `/skill`, or a
similar slash command through `command`.

## Work modes and security

Programmer separates the work mode from its filesystem and process sandbox:

- `manual` asks the user to approve mutating operations.
- `auto` sends operations needing review to a classifier model.
- `plan` keeps the agent in a planning/read-only workflow until approved.
- `yolo` bypasses review and is gated by configuration.

The security layer can independently restrict reads, writes, environment
variables, network access, system reads, temporary writes, and child processes.
Changing a work mode does not imply that the sandbox is disabled. Use
`/permission` or `/sandbox` to inspect or manage the effective policy, and use
`request_permission` when the current operation needs a narrowly scoped grant.

## User controls

Important TUI commands include:

- `/model`, `/thinking`, `/mode`, and `/classifier` for model behavior.
- `/providers show|manage|refresh` for provider configuration.
- `/permission` or `/sandbox` for security settings.
- `/skill <name>`, `/skill list`, `/skill manage`, and `/skill off` for skills.
- `/mcp show|manage` for external MCP servers.
- `/init` and diagnostics tooling for project understanding and checks.
- `/todo`, `/terminal`, `/compact`, `/vision`, `/session`, `/usage`, `/new`,
  `/clear`, `/help`, and `/quit` for session and UI workflows.

Common controls include `Enter` to send, `Ctrl+T` to cycle work modes,
`Ctrl+C` or `Ctrl+Q` to quit, mouse scrolling for conversation history, and
`!<command>` to open an interactive terminal command. Use `/help` or command
completion for the exact controls in the installed version.

## Files and persistence

- Project instructions: `PROGRAMMER.md`.
- Project diagnostics: `.programmer/diagnostics.toml`.
- Project skills: `.programmer/skills/<name>/SKILL.md`.
- Long tool-output archives: `.programmer/outputs/`.
- Global configuration, sessions, todos, and skills live below the platform's
  standard application config directory under `programmer/`. On macOS this is
  normally `~/Library/Application Support/programmer/`; on Linux it is normally
  `~/.config/programmer/`; on Windows it is normally `%APPDATA%\programmer\`.

Built-in skills are compiled into Programmer. Global skills can override a
built-in with the same name, and project skills can override both. Skills are
activated explicitly and the active set is saved with the session.

## Other interfaces

- `programmer run ...` performs a one-shot agent task without the TUI.
- `programmer init ...` creates or refreshes project guidance and diagnostics.
- `programmer diagnostics ...` runs configured project checks without calling
  a model.
- `programmer mcp stdio` and `programmer mcp http` expose a non-interactive
  subset of Programmer's local tools to MCP clients. Availability and approval
  behavior differ from the interactive TUI, so consult `programmer --help` and
  the relevant subcommand help before giving exact invocation advice.

## Source architecture

When the Programmer source tree is available, inspect it before answering
implementation questions. The main areas are `src/runner/` for the shared agent
loop, `src/app/` and `src/ui/` for the TUI, `src/tools/` for local tools and
tool providers, `src/security/` for authorization and sandboxing, `src/skills/`
for skill discovery, `src/mcp/` for MCP transports, `src/session/` for
persistence, `src/providers/` for API backends, and `src/prompts.rs` for static
prompt text.

For claims about the installed version, current model, enabled capabilities,
paths, or active mode, verify live state or clearly say that the answer is the
documented default rather than confirmed runtime state.
