---
name: initialize-project
description: Inspect and initialize a repository for future Programmer sessions by creating or refreshing PROGRAMMER.md and configuring project diagnostics. Use when the user asks to initialize, onboard, or set up Programmer for a repository, including through /init or programmer init.
---

# Initialize Project

Initialize the current repository from facts verified in its files.

## Workflow

1. Explore the repository. Read the main README, build manifests such as
   `Cargo.toml`, `package.json`, `pyproject.toml`, or `go.mod`, and the important
   source directories. Identify the architecture, entry points, build and test
   commands, conventions, and non-obvious constraints. Do not invent details.
2. Create or refresh `PROGRAMMER.md` at the repository root. Preserve useful
   existing project-specific guidance and intentional organization. Keep the
   result concise and factual: include a short overview, the actual technology
   stack, build/test/run commands, key directory boundaries, conventions, and
   durable gotchas. Do not add marketing, temporary status, or speculative
   plans.
3. Configure diagnostics with `configure_diagnostics` so later edits receive
   IDE-style feedback. Prefer terminating one-shot checker commands over watch
   processes or development servers. Use the project's established tools and
   iterate until the configuration validates and saves.
4. Add a distinct linter as a checker with `lint = true` when the project uses
   one. Skip this when compiler and linter diagnostics are not meaningfully
   separate.
5. Review the resulting changes and briefly summarize what was initialized,
   what was verified, and anything that could not be configured.

## Diagnostics guidance

- Rust: prefer `cargo check --message-format=json` with `rustc-json`; add
  `cargo clippy --message-format=json` as a lint checker when appropriate.
- TypeScript: use the project's `tsc --noEmit` command with `tsc`; add its
  configured ESLint command as a lint checker when present.
- Python: use the project's established type checker or test command; add Ruff
  as a lint checker when configured.
- Go: use the project's normal test or vet command; add golangci-lint when
  configured.
- C, C++, and tools that print `file:line:col: severity: message`: use `gnu`.
- Other structured output: use `regex` with an exact pattern derived from a
  real command result.
- Use an LSP checker only when a command checker is unsuitable. It is more
  expensive because it initializes for each diagnostics run.

If no suitable checker exists, record that limitation in `PROGRAMMER.md` and
leave diagnostics unconfigured rather than saving a misleading profile.
