---
name: update-programmer-md
description: Create or refresh the repository's PROGRAMMER.md from verified project facts. Use when the user asks to initialize, update, refresh, audit, or repair PROGRAMMER.md, or when Programmer reminds you that accumulated code changes may have made it stale.
---

# Update PROGRAMMER.md

Keep `PROGRAMMER.md` an accurate, concise map of the repository for future
coding sessions.

## Workflow

1. Read the existing `PROGRAMMER.md` if it exists. Preserve useful project-
   specific guidance and the author's intentional organization.
2. Inspect the repository before writing. At minimum, check the main README,
   build manifests, relevant source directories, and the current changes. Read
   the implementation behind any architectural claim that may have changed.
3. Update only facts supported by the repository. Correct stale claims and add
   durable information that will materially help future work.
4. Keep the file compact. Prefer a short overview plus the project's actual
   build, test, and run commands; important directory or module boundaries;
   conventions; and non-obvious gotchas.
5. Review the resulting diff. Ensure commands, paths, names, and configuration
   details are exact and that unrelated user-authored guidance was not lost.

## Content rules

- Describe the repository's current state, not the conversation or the work
  session that produced it.
- Do not add temporary status, recent-change logs, TODO lists, speculative
  plans, marketing language, or facts inferred only from filenames.
- Do not claim a command works unless repository evidence supports it; run
  important commands when practical, and state durable prerequisites instead
  of recording transient failures.
- Make the smallest useful edit when the existing file is already mostly
  correct. If no meaningful project fact changed, leave it untouched.
- If `PROGRAMMER.md` does not exist, create it only when the user requested
  initialization or creation; a maintenance reminder alone is not permission
  to invent a new project guide.
