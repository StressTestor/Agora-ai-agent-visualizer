# functional audit, 2026-09-05

Scope: all eight Rust modules, both webviews, IPC/event boundaries, and existing
tests. This is a functional/data-integrity review, not a claim of exhaustive
security assurance. Earlier uncommitted fixes are included in the reviewed tree.

## coverage and verification

| Area | Criteria | Method | State |
|---|---|---|---|
| Inbox parsing and history | No fabricated messages; reload preserves debate history | Source tracing and synthetic filesystem regressions | Passed |
| Team lifecycle and persistence | Exact identity; no active deletion; failures preserve data | Temporary directories and explicit error cases | Passed |
| Provider responses | Only completed successful responses enter debates | Offline SSE/JSON/channel fixtures | Passed |
| Settings | Failed/corrupt saves preserve recoverable configuration | Synthetic config files; no real credentials | Passed |
| Execution across GUI/CLI/TUI | Overrides honored; errors fail; retries discard abandoned output | Offline provider stubs and state tests | Passed |
| Frontend event ordering | Snapshot/event overlap, delete/start/restart races, status hydration | jsdom with controlled IPC promises | Passed |
| Models/presets/build | Built-in configuration consistency; compilation/lint | Source review and full test/build checks | Passed |

## confirmed findings fixed

- High: UI deletion uses the later selection after awaiting deletion of a different team.
- High: malformed/truncated provider streams and provider-side error results can be accepted as successful text.
- High: file-write/archive failures are ignored; app state can report success without durable data.
- High: settings loading can replace malformed configuration with defaults and later overwrite the recoverable original.
- Medium: GUI history snapshots omit live debate history, and paused controls are not hydrated after reload.
- Medium: initial event subscriptions follow the history snapshot, leaving a gap that drops messages.
- Medium: start/restart completion can remove an early message from the newly started run.
- Medium: failed retry text is combined with the next attempt in the GUI/TUI.
- Medium: CLI preset defaults override explicitly supplied options; provider failure returns a successful exit status.
- Medium: convergence counts repeated agreement from one participant and ignores its configured round threshold.
- Medium: team names are sanitized only by the persistence layer, splitting logical and on-disk identity.
- Medium: deleting a team leaves its runtime state and permits an active worker to recreate it.
- Medium: TUI setup failures bypass terminal cleanup, and failed turns retain active streaming state.
- Medium: empty or notification-only inbox wrapper objects fall through to the sender-map parser and fabricate messages.

Evidence: private synthetic harnesses reproduced frontend races, provider
truncation/error acceptance, CLI override/exit behavior, and convergence/retry
failures before remediation. Permanent regression tests are added alongside the
fixes. No live provider requests, private inbox scans, or user-data deletion are
part of this audit.

## limits

Provider latency, real authentication, billing, upstream model availability,
and native OS failure injection are not exercised. Agreement detection remains
a textual heuristic. HTTP responses stopped by output/context limits are rejected
as incomplete, even when they contain partial text.

Known follow-ups outside this remediation:

- The watcher deduplicates by team/sender/recipient/content, so identical repeated
  external messages can collapse even when timestamps differ.
- Atomic replacement prevents torn writes, but separate Agora processes writing
  the same inbox do not coordinate with a cross-process lock.
- An absent task directory is not attached to Notify when it later appears;
  the fallback poll covers inboxes, not task discovery.
- Custom role metadata survives active debate snapshots but is not restored by
  the watch-mode disk parser after a full application restart.
- Provider cancellation remains cooperative around blocking requests. An upstream
  request can continue after quitting, and GUI restart waits for worker exit.

These limits are recorded explicitly rather than treated as verified fixes.
## verification results

| Check | Result |
|---|---|
| `cargo test --locked` | 108 passed; 1 intentionally ignored manual benchmark |
| `npm test` | 35 passed |
| `cargo clippy --locked --all-targets --all-features -- -D warnings` | Passed |
| `cargo fmt --check` | Passed; Rust formatting normalized in this pass |
| `git diff --check` | Passed |
| `cargo build --release --locked` | Passed |
| Release `--help` and `debate --help` | Passed on final optimized binary |
| `npm run bench:stream` | 500 chunks, 200 prior messages: 1.07 ms, zero history scans, one text node, correct output |

The stream benchmark measures synthetic jsdom processing, not native paint time
or provider latency. The tests use temporary files, in-memory readers, synthetic
provider implementations, controlled IPC promises, and fake process polling.
They do not launch Claude or exercise native terminal failure injection.

Primary protocol references used in the provider review:

- [WHATWG event-stream interpretation](https://html.spec.whatwg.org/multipage/server-sent-events.html#event-stream-interpretation)
- [Anthropic streaming](https://platform.claude.com/docs/en/build-with-claude/streaming)
- [Anthropic stop reasons](https://platform.claude.com/docs/en/build-with-claude/handling-stop-reasons)
- [OpenAI chat completions reference](https://developers.openai.com/api/reference/cli/resources/chat/subresources/completions)

