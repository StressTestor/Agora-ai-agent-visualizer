# Agora architecture

This document describes the architecture implemented on `main` at commit
`cad305b`. It is a baseline for the current pre-refactor application, not a
description of planned v2 work.

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

There is no JavaScript package manager, frontend framework, frontend build
step, application database, or server component.

## Directory structure

```text
.
├── ARCHITECTURE.md
├── README.md
├── LICENSE
├── icon.svg
├── agora-improvements.md
├── agora-v2-plan.md
├── agora-v2-roadmap.md
├── docs/
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
The generated capability schemas under `src-tauri/gen/schemas` are also
ignored, although generated copies are present in the current checkout.

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
events. The current implementation performs inbox scanning while holding the
global `AppState` mutex.

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
| `convergence_threshold` | unsigned integer | Stored convergence setting |

Each `AgentConfig` holds a name, provider identifier, model identifier, system
prompt, and role. `DebateState` holds that configuration plus current status,
message history, round, current agent index, and current topic index.

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
9. emits and persists the completed message;
10. advances the agent, round, and possibly topic; and
11. emits updated status.

Agents run in vector order. One round is completed when the next agent index
wraps to zero. Directed visibility addresses a message to the next agent;
group visibility uses `all`.

The plain CLI and TUI repeat this sequencing locally. They reuse shared
functions for context construction, stop checks, next-turn calculation,
message construction, topic advancement, retry delay, and persistence.
The TUI sends `DebateEvent` values over an MPSC channel from its debate thread
to its rendering/input loop. Its `AtomicBool` cancellation flag is checked
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
timeout and parse server-sent events for streaming responses.
`ClaudeCodeProvider` locates `claude` in common macOS paths or `PATH`, invokes
one `claude -p` turn with tools and session persistence disabled, and parses
JSON or stream-JSON output. It uses the user's existing Claude Code
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
| `debate-message-chunk` | GUI orchestrator | Append streamed text |
| `debate-message-complete` | GUI orchestrator | Finalize a streamed message |
| `debate-status` | GUI orchestrator | Update lifecycle controls and counters |

`src/index.html` contains the complete main UI and runtime logic. It builds
message DOM nodes, performs escaped Markdown-like formatting, maintains search
and team filters, opens the settings and debate-wizard overlays, invokes Tauri
commands, and subscribes to backend events. There is no frontend module system
or separate asset pipeline.

At GUI startup, Tauri hides the main window and creates an undecorated splash
webview using `src/splash.html`. Video completion, video failure, a click, or
an eight-second timeout invokes `show_main_and_close_splash`.

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

The file is written as pretty JSON. On Unix, the implementation attempts to
set mode `0600` after writing it. At load time, recognized environment
variables override entries in memory.

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

and rewrites the complete file. `timestamp` is populated with epoch
milliseconds at runtime. Starting a GUI debate archives existing inbox JSON
files into a directory derived from the first topic before creating the new
in-memory debate state.

Team and recipient names used by orchestrator persistence are transformed by
`safe_filename`: characters other than alphanumeric, `-`, and `_` become `_`.
Other team commands in `main.rs` currently use the supplied team string
directly.

### Process-local state

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

`GEMINI_API_KEY` is not currently included in the environment overlay even
though `gemini` is a supported provider. `claude-code` intentionally uses no
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

or installation from a GitHub release DMG. The source repository contains no
workflow under `.github/workflows` on `main`; build, signing, notarization,
release upload, and Homebrew cask updates are not defined in this repository's
current tracked files.

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
- Termination and visibility are represented as strings in `DebateConfig`, not
  Rust enums. The GUI command boundary does not currently centralize debate
  validation.
- The GUI starts detached standard threads for debates and stores debate state,
  but not worker join handles or cancellation tokens.
- GUI pause and stop are represented by changes to `DebateStatus`. A blocking
  provider request or retry sleep does not observe those changes until control
  returns to the debate loop.
- TUI cancellation uses an `AtomicBool`; the current checks occur outside
  blocking provider calls.
- Provider HTTP and Claude CLI calls are synchronous. HTTP clients use a
  120-second timeout, and the Claude CLI implementation has its own
  120-second timeout path.
- Context construction uses the in-memory debate message history. There is no
  persisted summary or token-budget layer.
- Inbox persistence rewrites a complete JSON array per message and ignores
  several filesystem errors. The watcher reparses complete inbox files.
- Watcher deduplication does not include the timestamp, so otherwise identical
  messages share a hash.
- The watcher scans while holding the global application-state mutex.
- Team paths do not use one common identifier type: orchestrator writes
  sanitized names, while team listing, archiving, and deletion use path joins
  in `main.rs`.
- Environment-resolved provider keys are merged into the same serializable
  `AppConfig` returned to the GUI.
- The frontend is one inline HTML/CSS/JavaScript document with no automated
  frontend test or lint command in the repository.
- The capability manifest grants core and window-state permissions to `main`,
  while custom commands are registered globally in Rust rather than listed as
  command-specific permissions.
- The current webview configuration exposes `window.__TAURI__` globally and
  does not configure a CSP.
- CLI agent specifications use colon-delimited
  `name:provider:model[:role]` strings, while some provider model identifiers
  also contain colons.
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

The repository currently defines Rust unit tests inside `cli.rs`,
`model_profiles.rs`, `orchestrator.rs`, and `tui.rs`. There is no standalone
frontend test command or tracked CI command wrapper.

---

Last updated: 2026-07-23, baseline commit `cad305b`.
