# Ownership refactor — boundary acceptance

## Integrated verification

Final integrated run: `CARGO_INCREMENTAL=0 cargo test --offline --quiet`: 920 unit tests passed, 2 ignored; 1 integration test passed. Offline build and `cargo clippy --offline --all-targets -- -D warnings` passed. Logs: `.programmer/boundary-final-{tests,clippy}.log`.

## Real TUI / process observations

- UI-created Todo was visible to the model. Found truncated list IDs incompatible with exact update; fixed full IDs and verified model update back into UI.
- Two live children (pipe PID 97818, PTY PID 97858) disappeared after `/new`; new session task list was empty.
- Queued inputs remained blocked by an unsent draft after Esc cancelled streaming. Removing draft started the next turn with both queued texts, following existing merging semantics.
- Manual review Esc denied execution; next turn succeeded. Fixed cancellation attribution and re-ran TUI: `Approval cancelled; tool was not executed`.
- Rewind fork produced UUID e9017f7d-e96c-4d23-a647-1c3bd7bd50be from d1086b14-a761-4c8e-88a1-691dfbf04ad2, restored prompt into input and successfully continued.
- Normal quit removed pipe PID 99582, PTY PID 99583, agent command PID 99629 and parent PID 97403. On actual process restart, tasks were killed and agent cancelled, not running.
- Actual peer consent appeared; explicitly user-authorized first delegation executed. A separate unapproved request survived close/restart and was then rejected.
- After peer repair, two newly built TUI processes verified sender delegate -> recipient denial -> sender row with original `PEER-BODY-ROUNDTRIP` text. No `Task text unavailable` in that new row.

## Deterministic regressions and fixes

- Provider old success/failure overwriting newer refresh reproduced, then fixed with per-provider refresh ownership. Partial stale batches filter errors and notification counts too.
- Peer statuses recover original delegation from durable evidence, matching ID and endpoints. Started is sent through the existing status channel; late statuses cannot regress Started. No automatic completion inference.
- Durable inbox close and reconstruction tests use actual isolated disk mail; consent is not reconstructed from persisted status.
- App internal 10-second shutdown timeout and retry tested; worker ownership and closed admission retained. Save failure test now explicitly rejects success messages.
- Test EventHandler does not start a real crossterm reader; production reader unchanged. Removes incidental `reader source not set` panics. Deliberate runner-panic fault-injection test remains.
- Real loopback MCP connections verify old snapshot remains callable during/after reload, new snapshot uses new server, and late old generation cannot replace it.

## Follow-up dual-TUI E2E

- Tasks 178/179: recipient was busy streaming numbers; approved `E2E-REAPPROVAL-178` with Yes (queue). Sender displayed the original task and Waiting to start. Recipient exited before delegated execution.
- Task 180 restarted the same recipient session and displayed a fresh No/Yes consent prompt for that task, without executing it automatically. Reapproved explicitly.
- Sender task 179 then displayed the original task with `Delegated execution started; no completion report implied.` Recipient independently answered `E2E-STARTED-OK`. This verifies Started delivery, not a completion-report protocol.
- All three processes exited with code 0. No production code changed for this follow-up.

## Explicit limits
- `/clear` active/background paths covered by component tests, not destructive manual clearing of saved test sessions.
- No real remote MCP server failure injection, forced process crash recovery, or physical disk-full injection. Local MCP protocol tests are not remote-production E2E.
- Existing old sender rows are not proactively migrated; updated behavior applies to newly processed status notifications.

No dependency added, no commit or release performed. Test session archives retained; test TUI instances explicitly exited.
