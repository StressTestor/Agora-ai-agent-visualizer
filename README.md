# agora

native desktop app for watching claude code multi-agent debates in real time. also lets you orchestrate debates between models yourself.

i built this because claude code's multi-agent teams dump JSON into inbox files and there's no native way to watch the conversation unfold. then i added a full debate runner on top.

## install

```bash
brew tap stresstestor/tap
brew install --cask agora
```

or grab the DMG from [releases](https://github.com/StressTestor/Agora-ai-agent-visualizer/releases). Apple Silicon only for now.

## what it does

**watch mode** — passive viewer for claude code team debates

- watches `~/.claude/teams/*/inboxes/*.json` via macOS FSEvents (+ 2s poll fallback)
- parses 5 different inbox JSON formats (array, `{messages:[]}`, `{inbox:[]}`, single object, key-value map)
- deduplicates messages by content hash
- detects new teams appearing at runtime
- tracks task status changes from `~/.claude/tasks/`
- color-coded agents: advocate (cyan), critic (red), synthesizer (purple), team-lead (amber), fallback palette for custom agents
- team selector dropdown, content search (Cmd+K), expand/collapse message bodies
- auto-scroll with jump-to-bottom button

**debate mode** — run your own multi-model debates

- configure agents on any LLM provider: Anthropic, OpenAI, OpenRouter, Groq, OpenCode
- 4-step wizard: providers, agents, topics, settings
- import agent configs from existing claude code teams
- termination modes: fixed rounds, topic cycling, convergence detection, manual stop
- visibility modes: all agents see everything, or directed one-to-one
- live status dot, round counter, pause/resume/stop controls
- API keys stored locally at `~/.config/agora/config.json`

## build from source

requires [rust](https://rustup.rs/) and tauri CLI:

```bash
cargo install tauri-cli --version '^2'
git clone https://github.com/StressTestor/Agora-ai-agent-visualizer.git
cd Agora-ai-agent-visualizer
cargo tauri build
```

the `.app` lands in `src-tauri/target/release/bundle/macos/`. copy to `/Applications/` or run directly.

if your root disk is full, build to an external drive:

```bash
CARGO_TARGET_DIR=/Volumes/yourDrive/.cargo-tmp cargo tauri build
```

## stack

- tauri 2 (rust backend, webview frontend)
- notify 6 (FSEvents watcher)
- reqwest 0.12 (blocking HTTP for provider calls)
- vanilla HTML/CSS/JS frontend — no framework, no bundler
- single binary, no runtime deps

## license

MIT
