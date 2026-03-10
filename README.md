# debate-watch

native desktop viewer for claude code team debates. watches `~/.claude/teams/` for inbox messages and renders them as a live chat feed.

i built this because claude code's multi-agent teams dump JSON into inbox files and there's no way to actually watch the debate happen in real time. this fixes that.

## what it does

- watches `~/.claude/teams/*/inboxes/*.json` via macOS FSEvents (+ 2s poll fallback)
- parses 5 different inbox JSON formats (array, `{messages:[]}`, `{inbox:[]}`, single object, key-value map)
- deduplicates messages by hashing team+from+to+content
- detects new teams appearing at runtime
- tracks task status changes from `~/.claude/tasks/`
- color-codes agents: advocate (cyan), critic (red), synthesizer (purple), team-lead (amber), fallback palette for custom agents
- click any message body to expand/collapse (truncated at 200px by default)
- team selector dropdown to filter by team
- auto-scroll with a "scroll to bottom" button when you scroll up

## install

requires [rust](https://rustup.rs/) and the tauri CLI.

```bash
cargo install tauri-cli --version '^2'
```

then build:

```bash
git clone https://github.com/StressTestor/debate-watch.git
cd debate-watch
cargo tauri build
```

the `.app` bundle lands in `src-tauri/target/release/bundle/macos/debate-watch.app`. copy it to `/Applications/` or run directly.

## usage

```bash
# open the app
open /Applications/debate-watch.app

# or filter to a single team on launch
./debate-watch --team deliberation
```

## stack

- tauri 2 (rust backend, webview frontend)
- notify 6 (FSEvents file watcher)
- vanilla HTML/CSS/JS frontend (no framework, no bundler)
- ~400 lines of rust, ~480 lines of HTML/JS
- single binary, no runtime deps

## how it works

the rust backend spawns a watcher thread that monitors `~/.claude/teams/` recursively. when an inbox JSON file is created or modified, it re-scans all inboxes, deduplicates against previously seen messages, and emits `new-message` events to the webview via tauri's event system.

the frontend listens for these events and appends message nodes to the chat feed. no polling from the JS side, it's all push from the backend.

task updates work the same way but watch `~/.claude/tasks/` for JSON changes.

## license

MIT
