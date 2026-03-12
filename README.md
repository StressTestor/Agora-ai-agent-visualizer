# agora

native desktop app for watching claude code multi-agent debates in real time. also runs debates between any combination of models yourself.

i built this because claude code's multi-agent teams dump JSON into inbox files and there's no native way to watch the conversation unfold. then i added a full debate runner on top. then i kept going.

https://github.com/user-attachments/assets/agora-intro.mp4

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
- per-message collapse/expand buttons, content search (Cmd+K), auto-scroll
- archives previous debate messages when a new debate starts on the same team

**debate mode** — run your own multi-model debates

- 13 providers: Anthropic, OpenAI, OpenRouter, Groq, OpenCode, Gemini, DeepSeek, Moonshot, MiniMax, Z.ai, and more
- **claude code CLI provider** — no API key needed, uses your existing CC subscription. runs `claude -p` as a subprocess so you never touch the OAuth token
- mix providers per agent in the same debate (e.g. Claude Haiku via CC vs Gemini Flash via API key)
- 4-step wizard: team name, agents, topics, settings
- **topic refinement** — ✦ button on the topics field sends your rough idea to a configured AI and rewrites it into a specific, debatable topic. uses CC by default, configurable in settings
- import agent configs from existing claude code teams
- termination modes: fixed rounds, topic cycling, convergence detection, manual stop
- visibility modes: group (everyone sees everything) or directed (one-to-one)
- pause/resume/stop/restart controls
- API keys stored locally at `~/.config/agora/config.json`

**25 role presets across 6 categories**

| category | roles |
|---|---|
| core | advocate, critic, synthesizer, moderator |
| pressure | devil's advocate, contrarian, pessimist, optimist |
| domain | domain expert, security auditor, pragmatist, researcher, historian |
| creative | visual designer, brand strategist, art director, minimalist, marketer |
| business | product manager, stakeholder, end user, legal |
| perspective | user advocate, first principles |

**10 debate presets across 5 categories**

| category | presets |
|---|---|
| deliberation | 3-agent deliberation, red team / blue team |
| technical | adversarial probe, architecture decision, go / no-go |
| product | product critique, feature scoping, build vs buy |
| creative | creative review |
| research | research panel, first principles reset |

role and preset dropdowns have search/filter built in.

## providers

| provider | key required | notes |
|---|---|---|
| claude code (CLI) | no | uses CC auth, ~10s per turn (Node startup) |
| anthropic | yes | direct API |
| openai | yes | direct API |
| groq | yes | fast inference, free tier available |
| gemini | yes | OpenAI-compatible endpoint |
| openrouter | yes | routes to any model |
| opencode zen | yes | |
| deepseek | yes | |
| moonshot / kimi | yes | |
| minimax | yes | |
| z.ai | yes | |

## settings

- **api keys** — per-provider, stored in `~/.config/agora/config.json` at mode 600
- **generation model** — provider + model used for topic refinement and other single-shot AI calls. defaults to claude code CLI if unset

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
