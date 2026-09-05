# Agora architecture

This document describes the current application, including the September 2026
UI and debate-lifecycle fixes. It does not describe planned v2 work.

## Project overview

Agora is a single Rust binary with three user-facing modes:

| Surface | Entry path | Purpose |
|---|---|---|
| Desktop GUI | Run `agora` without a CLI subcommand | Watch Claude Code team inboxes and configure, start, and observe Agora debates |
| Terminal UI | Run `agora debate ...` | Run a debate in a Ratatui alternate-screen interface |
| Plain CLI | Run `agora debate --plain ...` | Run the same command-line debate configuration with streamed text output |

The desktop application combines two related flows:

1. **Watch mode** reads Claude Code team data from `~/.claude/teams` and task
   data from `~/.claude/tasks`, keeps a process-local message cache, and emits
   Tauri events when files change.
2. **Debate mode** calls configured model providers in round-robin order,
   streams response chunks to the selected UI, and optionally materializes
   results as Claude-compatible inbox JSON.

The GUI, plain CLI, and TUI share debate data types and helper functions from
`orchestrator.rs`. Each surface currently owns a separate debate loop and
event/output adapter.

## Stack and dependencies

| Layer | Technology | Current role |
|---|---|---|
| Application language | Rust 2021 | Backend, CLI, TUI, orchestration, providers, persistence, and file watching |
| Desktop shell | Tauri 2 | macOS application window, webview IPC, application events, and bundling |
| Desktop frontend | Vanilla HTML, CSS, and JavaScript | Transcript, team filtering, settings, debate wizard, and controls |
| Terminal parsing | Clap 4 | `debate`, `list-presets`, and `list-models` commands |
| Terminal UI | Ratatui 0.29 and Crossterm 0.28 | Full-screen streamed debate view and keyboard input |
| HTTP | Reqwest 0.12 blocking client with Rustls | Provider model discovery, chat completions, and SSE response streaming |
| Local process provider | `std::process::Command` and `libc` | Invokes `claude -p` and sends `SIGKILL` on the implemented timeout path |
| File watching | Notify 6 | Watches Claude team and task directories recursively |
| Serialization | Serde and serde_json | Tauri payloads, configuration, team metadata, and inbox JSON |
| Time handling | Chrono and `std::time` | RFC 3339 parsing, file timestamps, and generated epoch-millisecond timestamps |
| Window state | `tauri-plugin-window-state` 2 | Persists desktop window placement and size |
| Build | Cargo, `tauri-build`, and Tauri CLI | Builds the Rust binary and platform application bundles |

The shipped frontend remains static HTML with no framework or build step.
Node.js and npm are used only for the jsdom regression tests; they are not
runtime dependencies of the desktop app. There is no database or server component.

## Directory structure

```text
.
├── .github/workflows/ci.yml     # macOS Rust and frontend pull-request checks
├── ARCHITECTURE.md
├── README.md
├── package.json / package-lock.json # frontend test dependencies and npm test
├── tests/
│   ├── frontend.test.cjs         # simulated Tauri events and DOM regressions
│   ├── splash.test.cjs           # media and IPC startup failure cases
│   └── streaming.bench.cjs       # synthetic stream-processing benchmark
├── LICENSE
├── icon.svg
├── agora-improvements.md
├── agora-v2-plan.md
├── agora-v2-roadmap.md
├── docs/
│   ├── audits/                   # dated functional coverage and verification reports
│   └── superpowers/
│       ├── plans/               # historical implementation plans
│       └── specs/               # historical orchestration design
├── src/
│   ├── index.html               # complete main webview UI: HTML, CSS, and JS
│   ├── splash.html              # startup video webview and handoff command
│   └── agora-intro.mp4
└── src-tauri/
    ├── Cargo.toml
    ├── Cargo.lock
    ├── build.rs
    ├── tauri.conf.json          # window, webview, security, version, and bundle config
    ├── capabilities/
    │   └── default.json         # main-window core and window-state permissions
    ├── icons/                   # platform bundle icons
    ├── gen/schemas/             # generated Tauri capability schemas
    └── src/
        ├── main.rs              # process entry, GUI state, watcher, and Tauri commands
        ├── cli.rs               # Clap schema and plain-text debate runner
        ├── tui.rs               # Ratatui state, rendering, input, and TUI debate runner
        ├── orchestrator.rs      # shared debate types/helpers and GUI debate worker
        ├── provider.rs          # provider trait and concrete provider clients
        ├── config.rs            # provider settings, environment overlay, and JSON storage
        ├── presets.rs           # role and debate preset definitions
        └── model_profiles.rs    # model-specific behavioral profile lookup
```

`src-tauri/target` is the default local build-output directory and is ignored.
Generated capability schemas, generated permission files, `node_modules`, and
legacy `dist` output are also ignored. The shipped frontend is still `src`.

## Key patterns

### Process and surface selection

`main.rs` inspects the first process argument before constructing Tauri:

- `debate`, `list-presets`, `list-models`, `--help`, and `-h` enter
  `cli::run_cli()` and exit without launching a desktop window.
- Other invocations construct the GUI. `--team`/`-t` is accepted in this path
  as an event-emission filter for the watcher.

The `debate` command uses the TUI by default and selects plain output when
`--plain` is present.

### Desktop state ownership

The GUI registers two Tauri-managed values:

```text
Arc<Mutex<AppState>>
├── seen_hashes: HashSet<u64>
├── messages: Vec<Message>
├── known_teams: HashSet<String>
├── inbox_scan_cache: InboxScanCache  # successful, stable file fingerprints
├── config: AppConfig
└── debates: HashMap<String, Arc<Mutex<DebateState>>>

Arc<Mutex<HashSet<u64>>>  # hashes inserted by streamed Agora messages
```

`AppState` is shared by Tauri commands and the watcher thread. Each GUI debate
has an independently locked `DebateState`, keyed by the requested team name.
The separate shared hash set lets the orchestrator mark a message before its
disk write is observed, avoiding a second `new-message` event after a
`debate-message-complete` event.

### Watch-mode data flow

```text
~/.claude/teams and ~/.claude/tasks
        │
        ├── Notify recursive filesystem events
        └── 2-second inbox poll fallback
                    │
                    ▼
        list teams / parse inboxes / parse changed tasks
                    │
                    ├── AppState messages and known-team cache
                    └── Tauri events
                        ├── new-message
                        ├── team-added
                        └── task-update
                                    │
                                    ▼
                              src/index.html
```

The inbox parser accepts five shapes:

1. a top-level message array;
2. an object containing `messages: []`;
3. an object containing `inbox: []`;
4. one message object; or
5. an object used as a sender-to-text map.

Message fields accept aliases for sender, recipient, and content. Timestamps
are resolved in this order: JSON timestamp, file modification time, current
time. System-like records such as heartbeats and idle notifications are
filtered. Deduplication hashes team, sender, recipient, and content; timestamp
is not part of the hash.

The initial inbox scan happens during Tauri setup. A dedicated standard thread
then drains Notify events, runs the polling fallback, scans inboxes, and emits
events. Inbox scanning still holds the global `AppState` mutex, but unchanged
inboxes now need only metadata checks. Successful stable reads are cached by
path, size, modification/creation timestamps, and Unix file identity/change
time. Watcher events invalidate affected paths; removed files are pruned and
failed or concurrently changed reads stay retryable. If watcher initialization
fails, the inbox polling fallback continues.

### Debate configuration and state

`DebateConfig` contains:

| Field | Type | Meaning |
|---|---|---|
| `team_name` | string | Debate name and logical team key |
| `agents` | list of `AgentConfig` | Ordered round-robin participants |
| `topics` | list of strings | Debate subjects |
| `visibility` | string | `group` or `directed` routing |
| `termination` | string | `fixed`, `topic`, `convergence`, or `manual` |
| `max_rounds` | unsigned integer | Configured fixed-round limit |
| `convergence_threshold` | unsigned integer | Required consecutive complete rounds of agreement from every participant |

Each `AgentConfig` holds a name, provider identifier, model identifier, system
prompt, and role. `DebateState` holds that configuration plus current status,
message history, round, current agent index, current topic index, and a
`worker_active` ownership flag for the GUI worker.

The GUI creates a debate through a Tauri command, stores its state in
`AppState.debates`, and starts a detached standard thread. The worker:

1. creates Claude-compatible team files;
2. builds one provider object per agent;
3. marks the debate running;
4. checks status and termination;
5. builds the active agent's context from debate history;
6. emits a thinking event;
7. calls the provider with up to four attempts;
8. streams chunks as Tauri events;
9. persists the completed message successfully, then publishes it;
10. advances the agent, round, and possibly topic; and
11. emits updated status.

Agents run in vector order. One round is completed when the next agent index
wraps to zero. Rounds are one-based while running; fixed mode executes exactly
the requested number of rounds. Topic mode executes three full rounds per
topic, then stops after the final topic. Directed visibility addresses a message to the next agent;
group visibility uses `all`. Convergence checks complete rounds and requires each
participant to agree for the configured threshold; explicit dissent blocks agreement.
Each retry clears abandoned streamed output before starting a fresh attempt.
Authentication failures fail immediately instead of using the retry budget.

The plain CLI and TUI repeat this sequencing locally. They reuse shared
functions for context construction, stop checks, next-turn calculation,
message construction, topic advancement, retry delay, and persistence.
The TUI sends `DebateEvent` values over an MPSC channel from its debate thread
to its rendering/input loop. A session guard restores terminal state and signals
cancellation on setup failures and every exit; ended turns clear partial output. Its `AtomicBool` cancellation flag is checked
between provider turns and during the short between-turn delay.

### Provider abstraction

`provider::Provider` is a synchronous, object-safe trait:

```rust
fn chat(&self, messages: &[ChatMessage], model: &str)
fn list_models(&self)
fn chat_streaming(&self, messages: &[ChatMessage], model: &str, on_chunk: &mut dyn FnMut(&str))
```

`chat_streaming` has a default non-streaming fallback. Two HTTP client families
and one subprocess client implement the trait:

| Implementation | Provider identifiers |
|---|---|
| `OpenAiCompatible` | `openai`, `openrouter`, `groq`, `opencode`, `deepseek`, `moonshot`, `minimax`, `zai`, `zai-coding`, `gemini` |
| `AnthropicClient` | `anthropic`, plus `minimax-coding` through a custom Anthropic-compatible base URL |
| `ClaudeCodeProvider` | `claude-code` |

The HTTP implementations use blocking Reqwest clients with a 120-second
timeout and parse server-sent events for streaming responses. Request payloads
borrow existing conversation strings rather than cloning them again before
serialization. GUI model discovery runs on `spawn_blocking`, and topic
enhancement constructs its provider inside its blocking worker.
`ClaudeCodeProvider` locates `claude` in common macOS paths or `PATH`, invokes
one `claude -p` turn with tools and session persistence disabled, and parses
JSON or stream-JSON output. Provider streams must report successful completion;
truncated output and embedded error results fail instead of becoming debate turns.
SSE parsing supports optional spaces after field names and multiline data events.
Subprocess reading and exit waiting share a finite deadline. It uses the user's existing Claude Code
authentication rather than an Agora API key.

`model_profiles.rs` maps known model identifiers and aliases to optional
behavioral instructions. `presets.rs` supplies the role prompts and multi-agent
debate templates exposed by both GUI and CLI.

### Tauri IPC and frontend events

The main webview invokes these custom commands:

| Area | Commands |
|---|---|
| Teams and history | `get_teams`, `list_team_configs`, `delete_team`, `get_messages` |
| Settings and providers | `get_config`, `save_config`, `list_models`, `enhance_topic` |
| Presets | `list_role_presets`, `list_debate_presets` |
| Debate lifecycle | `create_debate`, `start_debate_cmd`, `pause_debate`, `stop_debate`, `restart_debate`, `get_debate_status` |
| Window startup | `show_main_and_close_splash` |

The backend emits:

| Event | Producer | Consumer purpose |
|---|---|---|
| `new-message` | Watcher | Add a newly discovered inbox message |
| `team-added` | Watcher | Add a newly discovered team filter |
| `task-update` | Watcher | Display Claude task status |
| `debate-thinking` | GUI orchestrator | Show the active agent's pending state |
| `debate-message-reset` | GUI orchestrator | Discard abandoned output before each provider attempt |
| `debate-message-chunk` | GUI orchestrator | Append streamed text |
| `debate-message-complete` | GUI orchestrator | Finalize a streamed message |
| `debate-status` | GUI orchestrator | Update lifecycle controls and counters |

`src/index.html` contains the complete main UI and runtime logic. It builds
message DOM nodes, performs escaped Markdown-like formatting, maintains search
and team filters, opens the settings and debate-wizard overlays, invokes Tauri
commands, and subscribes to backend events. Streaming nodes are keyed by team
and agent and retained across filter changes. Chunks accumulate into one text
node per stream and flush at most once per animation frame. Streaming updates
do not rescan completed history. Completed message nodes and normalized search
text are cached with weak references, preserving collapse state and avoiding
repeat Markdown rendering on team switches. Filter changes use one fragment
insertion. Incoming content respects the reading position and active search. Debate controls use the selected team
and cached per-team status. Startup registers event listeners before fetching
history, merges overlapping messages by identity, and hydrates debate controls
without letting an older status response replace a newer event. Delete/start/restart
handlers retain their original team and preserve messages received during IPC.
The title has its own row and the toolbar and
modals fit narrow windows. There is no frontend module system or separate
asset pipeline.

The setup wizard validates each step, preserves selected dropdown values during
filtering, and rejects stale async model/setup responses. Concurrent model-list
requests for the same provider share one request; saving settings invalidates
the cache. Model discovery preserves typed IDs even when absent from the
catalog. Only fixed mode shows the rounds field; other modes explain their
actual stopping rule. Settings save failures preserve the form and show errors.

At GUI startup the main window remains visible behind an optional undecorated
splash webview using `src/splash.html`. Video completion/failure, click, Escape,
the Skip intro button, or an eight-second JavaScript timeout invokes an
idempotent handoff. A separate native nine-second timeout recovers a stuck
splash even if its JavaScript or IPC fails. Splash construction failure leaves
the main window usable.

### Tauri capabilities and webview configuration

`src-tauri/capabilities/default.json` assigns the `main` window
`core:default` and `window-state:default`. Custom commands are registered with
`tauri::generate_handler!` in `main.rs`; there are no command-specific
capability entries in the current manifest.

`tauri.conf.json` currently:

- enables the global Tauri JavaScript object with `withGlobalTauri`;
- sets Content Security Policy to `null`;
- defines one 920×660 main window with overlay title bar;
- uses the static `src` directory as `frontendDist`; and
- enables all supported bundle targets.

The splash window is created programmatically during Tauri setup and is not
listed in `tauri.conf.json`.

## Database schema

Agora has no database and no schema migration system. Durable state is stored
as JSON files in the user's home directory.

### Provider configuration

```text
~/.config/agora/config.json
```

Serialized `AppConfig` shape:

```json
{
  "providers": {
    "<provider-id>": {
      "api_key": "<string>",
      "enabled": true
    }
  },
  "enhance_provider": "<provider-id-or-empty>",
  "enhance_model": "<model-id-or-empty>"
}
```

The file is written as pretty JSON through a same-directory temporary file and
atomic rename. Unix temporary files are created with mode `0600`. Malformed
configuration is reported without exposing its values; saving replacement settings
first preserves the original bytes in a unique `.corrupt-*` backup. Unreadable
existing files block a save. At load time, recognized environment variables
override entries in memory.

### Claude-compatible team persistence

Agora reads and writes under:

```text
~/.claude/
├── teams/
│   └── <team>/
│       ├── config.json
│       ├── inboxes/
│       │   └── <recipient>.json
│       └── archive/
│           └── <topic-slug>[-N]/
│               └── *.json
└── tasks/
    └── <team>/
        └── *.json
```

For Agora-created teams, `config.json` contains `name`, a debate-derived
`description`, and members with `name` and `model`. Each written inbox is a
pretty-printed JSON array. Agora reads the existing array, appends:

```json
{
  "from": "<agent>",
  "to": "<recipient-or-all>",
  "text": "<model-response>",
  "timestamp": 0,
  "role": "<role>"
}
```

and atomically replaces the complete file. `timestamp` is populated with epoch
milliseconds at runtime. Malformed prior inboxes are preserved and return an
error; initialization and write failures stop the runner rather than reporting
a completed turn. Creating or restarting a GUI debate archives the existing
inbox directory with a single rename into a topic-derived directory. Archive
failure preserves the current state and prevents replacement.

Team names retain their exact spelling across commands and persistence and must
be a nonempty single directory component; team-directory symlinks are rejected.
Recipient filenames use `safe_filename`. Deletion rejects an active worker and,
after successful disk removal, clears that team's cached history and runtime state.

### Process-local state

History IPC merges watched rows with active debate history by exact message
identity, preserving live role metadata and externally written same-team messages.

Watched messages, message hashes, known teams, active GUI debates, TUI
transcripts, and model-list caches are process-local only. They are rebuilt
from disk or provider calls after restart. No internal debate database is
maintained.

## Environment variables

`HOME` determines all default configuration and Claude data paths. If it is
unavailable, the Rust code falls back to `/tmp`.

The current configuration loader recognizes:

| Provider ID | Environment variable |
|---|---|
| `openai` | `OPENAI_API_KEY` |
| `openrouter` | `OPENROUTER_API_KEY` |
| `groq` | `GROQ_API_KEY` |
| `opencode` | `OPENCODE_API_KEY` |
| `anthropic` | `ANTHROPIC_API_KEY` |
| `deepseek` | `DEEPSEEK_API_KEY` |
| `moonshot` | `MOONSHOT_API_KEY` |
| `minimax` | `MINIMAX_API_KEY` |
| `minimax-coding` | `MINIMAX_CODING_API_KEY` |
| `zai` | `ZAI_API_KEY` |
| `zai-coding` | `ZAI_CODING_API_KEY` |
| `gemini` | `GEMINI_API_KEY` |

`claude-code` intentionally uses no
Agora API-key environment variable.

`CARGO_TARGET_DIR` is not read by Agora itself, but Cargo honors it when
placing build output.

## Deployment and infrastructure

Agora is a local desktop/terminal application. It does not deploy an
application server, database, queue, scheduled job, telemetry backend, or
cloud infrastructure.

Version `0.4.0` appears separately in `src-tauri/Cargo.toml` and
`src-tauri/tauri.conf.json`. Tauri builds:

```text
src-tauri/target/release/agora
src-tauri/target/release/bundle/macos/agora.app
src-tauri/target/release/bundle/dmg/*.dmg
```

The application bundle uses identifier `dev.notbatman.agora`, the icons under
`src-tauri/icons`, and the static frontend directory under `src`.

Public installation documented in the README is:

```bash
brew tap stresstestor/tap
brew install --cask agora
```

or installation from a GitHub release DMG. The `.github/workflows/ci.yml` workflow runs frontend tests, Rust tests,
formatting, and Clippy on macOS for pull requests and pushes to `main`. It uses
Rust 1.97.1, Node.js 22, read-only repository permissions, and commit-pinned
GitHub Actions. Signing, notarization, release upload, and Homebrew cask updates
are not automated by this workflow.

## External services and integrations

| Integration | Mechanism |
|---|---|
| Claude Code teams | Reads team configs and inboxes from `~/.claude/teams`; writes Agora debates back in compatible JSON |
| Claude Code tasks | Watches and parses JSON below `~/.claude/tasks` |
| Claude Code CLI | Spawns `claude -p` for the `claude-code` provider |
| Anthropic | Native Messages API and models API |
| OpenAI | OpenAI-compatible chat, streaming, and models APIs |
| OpenRouter | OpenAI-compatible API plus `HTTP-Referer` and `X-Title` headers |
| Groq | OpenAI-compatible API |
| OpenCode Zen | OpenAI-compatible API |
| Gemini | Google's OpenAI-compatible endpoint |
| DeepSeek | OpenAI-compatible API |
| Moonshot/Kimi | OpenAI-compatible API |
| MiniMax | OpenAI-compatible API and a separate Anthropic-compatible coding endpoint |
| Z.ai | Standard and coding-plan OpenAI-compatible endpoints |
| GitHub Releases | Distribution location linked by the README |
| Homebrew tap | macOS cask installation path documented by the README |

All provider endpoints are compiled into `provider.rs`. Provider/model
selection and topic-enhancement fallback models are also code-defined rather
than fetched from a central registry.

## Gotchas

- The GUI, plain CLI, and TUI share helpers but contain three separate debate
  loops. Lifecycle and output behavior can differ between surfaces.
- Termination and visibility are strings in `DebateConfig`, not Rust enums.
  GUI creation and worker startup validate nonempty agent lists, unique names,
  provider/model values, and topics for topic mode.
- GUI workers have an ownership guard that lasts until the worker exits.
  Start, restart, and replacement reject an active worker, including a worker
  draining a request after stop. Rejected restart preserves the transcript.
- GUI pause and stop emit status immediately. Retry waits observe the status,
  and stopped workers discard late chunks and responses. An in-flight blocking
  provider call still runs to completion or timeout; pause defers publication
  until resumed, and restart must wait for worker exit.
- TUI cancellation uses an `AtomicBool`; the current checks occur outside
  blocking provider calls.
- Provider HTTP and Claude CLI calls are synchronous. HTTP clients use a
  120-second timeout, and the Claude CLI implementation has its own
  120-second timeout path.
- Context construction uses the in-memory debate message history. There is no
  persisted summary or token-budget layer.
- Inbox persistence rewrites a complete JSON array per message. Errors propagate,
  but independent processes writing the same inbox are not serialized by a file lock. The watcher reparses changed inboxes in full,
  while unchanged files use the metadata cache.
- Watcher deduplication does not include the timestamp, so otherwise identical
  messages share a hash.
- The watcher scans while holding the global application-state mutex.
- Environment-resolved provider keys are merged into the same serializable
  `AppConfig` returned to the GUI.
- The frontend is one inline HTML/CSS/JavaScript document. `npm test` runs
  jsdom regressions with synthetic Tauri events and no provider calls. There
  is no frontend lint configuration.
- The capability manifest grants core and window-state permissions to `main`,
  while custom commands are registered globally in Rust rather than listed as
  command-specific permissions.
- The current webview configuration exposes `window.__TAURI__` globally and
  does not configure a CSP.
- CLI agent specifications use `name:provider:model[:role]`. Only a trailing
  built-in role is interpreted as a role; other colon suffixes stay in the
  model ID. Append an explicit role when a model suffix itself matches a role.
  `--judge name:provider:model` preserves the entire model tail and always uses
  the judge prompt. CLI validation runs before provider setup; mode typos and
  zero rounds are rejected. Explicit options take precedence over preset defaults.
  Provider and persistence failures produce a nonzero exit status in CLI/TUI.
  `list-models claude-code` needs no API key.
- Model fallbacks and provider URLs are static source data and can drift from
  provider availability.
- `Cargo.toml` and `tauri.conf.json` each carry the application version and
  must currently be updated together.

## Commands

Run Cargo commands from `src-tauri` unless shown otherwise.

```bash
# Development desktop app; requires Tauri CLI
cargo tauri dev

# Production application and bundles
cargo tauri build

# Rust compile check
cargo check

# Unit tests
cargo test

# Verify the committed lockfile is usable
cargo test --locked

# Formatting check
cargo fmt --check

# Lint all Rust targets and features
cargo clippy --all-targets --all-features -- -D warnings

# Terminal help
cargo run -- --help

# List built-in debate presets
cargo run -- list-presets

# List models for a configured provider
cargo run -- list-models <provider>

# Run a TUI debate
cargo run -- debate \
  --name <team> \
  --agent '<name>:<provider>:<model>:<role>' \
  --agent '<name>:<provider>:<model>:<role>' \
  --topic '<topic>'

# Run the plain streaming CLI
cargo run -- debate --plain \
  --name <team> \
  --agent '<name>:<provider>:<model>:<role>' \
  --agent '<name>:<provider>:<model>:<role>' \
  --topic '<topic>'
```

Rust unit tests cover inbox parsing/caching, lifecycle and persistence, provider
protocols, settings integrity, CLI configuration, model profiles, orchestration,
and TUI state. The dated report in `docs/audits/2026-09-05-functional.md` records
the deep functional audit and its limits. Run frontend tests from the repository root:

```bash
npm ci
npm test
```

The frontend tests use mocked IPC and exercise scroll position, streaming
across team filters, search counts, protocol-noise filtering, control routing,
rejected restart preservation, setup validation/async races, and media/IPC
startup failures. No model requests are needed by these tests.
CI runs the same frontend tests and locked Rust checks in `.github/workflows/ci.yml`.

### Performance checks

These are local synthetic samples from this change, not end-to-end model latency
or native WebView paint measurements. Timings vary with hardware and load.

| Workload | Before | After |
|---|---|---|
| 20 polls, 8 unchanged inboxes, 2,000 messages | 186.6 ms, 160 JSON parses | 1.6 ms, zero JSON parses |
| 500 chunks, 200 existing messages, simulated paint every 8 chunks (jsdom) | 864 ms, 500 history scans, 500 text nodes | 2.4 ms, zero history scans, one text node |

The scan benchmark compares forced reparsing against a warm cache. The stream
sample compares the pre-optimization frontend from this working session against
the batched frontend. Both verify unchanged output. Provider payload preparation
also removes an extra full-content copy; serialization and network time remain.

```bash
# From the repository root
npm run bench:stream
# Optionally compare an earlier frontend HTML snapshot
node tests/streaming.bench.cjs /path/to/earlier-index.html

# From src-tauri
cargo test --locked repeated_scan_benchmark -- --nocapture
cargo test --locked benchmark_request_preparation -- --ignored --nocapture
cargo build --release --locked
```

---

Last updated: 2026-09-05.
