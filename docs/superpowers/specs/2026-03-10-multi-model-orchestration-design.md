# agora multi-model orchestration

## goal

agora gains the ability to orchestrate LLM-powered debates directly, using any combination of OpenAI, OpenRouter, Groq, OpenCode, and Anthropic models. debates appear in the same unified chat feed alongside Claude Code team messages.

## architecture

the rust backend gains three new modules alongside the existing file watcher. the two systems are independent — the file watcher continues monitoring `~/.claude/teams/` for Claude Code teams, while the orchestration engine manages agora-native debates. both produce the same output: messages that flow into the unified chat feed.

### new rust modules

- `orchestrator` — debate lifecycle (start, stop, pause), turn management, termination modes, context building
- `provider` — two HTTP client implementations (OpenAI-compatible, Anthropic Messages API), model discovery
- `config` — API key storage/retrieval (config file + env var override), debate presets, role definitions

### new dependency

- `reqwest` with `json` and `rustls-tls` features — HTTP client for LLM API calls. only new crate.

### new tauri commands (frontend → backend)

- `save_config` / `get_config` — API keys and provider settings
- `list_models` — query available models from configured providers
- `create_debate` — configure agents, roles, topics, rules, visibility mode
- `start_debate` / `stop_debate` / `pause_debate` — lifecycle control
- `list_presets` — built-in debate templates

### new tauri events (backend → frontend)

- same `new-message` event already used — orchestrator emits messages in the same format
- `debate-status` — state changes (running, paused, stopped, converged)

## provider abstraction

two client implementations behind a common trait:

```rust
trait Provider {
    async fn chat(&self, messages: Vec<ChatMessage>, model: &str) -> Result<String>;
    async fn list_models(&self) -> Result<Vec<String>>;
    fn name(&self) -> &str;
}
```

### openai-compatible client

handles OpenAI, OpenRouter, Groq, and OpenCode. all four use the same request/response format (`/v1/chat/completions`). one struct with a `base_url` field covers all of them.

| provider | base URL |
|----------|----------|
| openai | `api.openai.com/v1` |
| openrouter | `openrouter.ai/api/v1` |
| groq | `api.groq.com/openai/v1` |
| opencode | TBD — same shape |

auth: `Authorization: Bearer <key>` header for all four.

### anthropic client

handles Anthropic (`api.anthropic.com/v1/messages`). different request shape (system prompt is a top-level field, not a message role), different response shape. uses `x-api-key` header and requires `anthropic-version` header.

### model discovery

`list_models()` calls each provider's models endpoint. results cached in memory for the session.

## config and API key management

### config file

location: `~/.config/agora/config.json`

```json
{
  "providers": {
    "openai": { "api_key": "sk-...", "enabled": true },
    "openrouter": { "api_key": "sk-or-...", "enabled": true },
    "groq": { "api_key": "gsk_...", "enabled": true },
    "opencode": { "api_key": "...", "enabled": true },
    "anthropic": { "api_key": "sk-ant-...", "enabled": true }
  }
}
```

### key resolution order

1. env var (`OPENAI_API_KEY`, `OPENROUTER_API_KEY`, `GROQ_API_KEY`, `OPENCODE_API_KEY`, `ANTHROPIC_API_KEY`)
2. config file value
3. not configured (provider disabled, models grayed out in wizard)

file created on first save from settings wizard. 600 permissions. keys never touch frontend JS.

## debate orchestration engine

### debate config (created by wizard)

- list of agents, each with: name, provider, model, system prompt
- topic(s) to debate
- visibility mode: group chat (all see all) or directed (only see messages addressed to you)
- termination mode: fixed rounds, topic-based, manual stop, or convergence detection
- turn order: round-robin

### turn execution

1. orchestrator builds context payload for current agent based on visibility mode
2. calls agent's provider with context + system prompt
3. receives response, creates a Message struct (same format as file watcher messages)
4. emits `new-message` event to frontend
5. stores message in debate state
6. advances to next agent's turn

### termination modes

- **fixed rounds** — counter tracks rounds, stops when limit hit
- **topic-based** — orchestrator injects topic transitions. after each topic, checks for convergence, then moves to next or stops
- **manual stop** — runs until user hits stop
- **convergence detection** — after each round, checks if last N messages show agreement patterns (agent's position substantially similar to previous position). configurable threshold.

### state management

debate state lives in `Arc<Mutex<DebateState>>` alongside existing `AppState`. orchestrator runs on a spawned async task so it doesn't block file watcher or UI.

### team integration

each debate creates a virtual "team" entry so it appears in the team selector alongside Claude Code teams. team name is user-defined in the wizard.

## role presets

5 built-in roles with system prompts:

### advocate
"you are arguing in favor of the proposed position. make your case with concrete evidence and specific examples. anticipate counterarguments and address them head-on. if your position has genuine weaknesses, acknowledge them and explain why the strengths outweigh them. don't hand-wave — show your work."

### critic
"you stress-test every proposal that comes your way. find the real weaknesses — implementation complexity, hidden assumptions, edge cases the advocate glossed over. push back hard, but be honest: if an argument genuinely holds up under scrutiny, say so and move on. concede points that are legitimately proven. being wrong is fine. being stubbornly wrong wastes everyone's time."

### synthesizer
"you are the neutral arbiter. watch the debate, identify where the two sides actually agree vs where the disagreement is real. ask pointed questions that force concrete answers — no hand-waving from either side. when the debate stalls, reframe the problem. your final output is a clear decision with reasoning, not a compromise that makes nobody happy."

### researcher
"you gather facts and context that inform the debate. look up specifics — benchmarks, API docs, implementation examples, known tradeoffs. present what you find without editorializing. if the data contradicts someone's claim, say so plainly. if you can't verify something, say that too."

### moderator
"you keep the debate productive. if someone repeats a point that's already been addressed, call it out. if the conversation drifts off-topic, pull it back. summarize where things stand after each round. you don't take sides, but you do call out weak arguments and demand specifics when someone is being vague."

## debate presets

| preset | agents | visibility | termination |
|--------|--------|------------|-------------|
| 3-agent deliberation | advocate + critic + synthesizer | group chat | convergence |
| red team / blue team | 2 advocates (opposing prompts) + moderator | directed | fixed 5 rounds |
| research panel | 2 researchers + synthesizer | group chat | topic-based |

## frontend

everything stays in `src/index.html`. no new files.

### setup wizard (4-step modal)

**step 1: provider & preset**
- preset dropdown (deliberation, red/blue, research, custom)
- preset pre-fills steps 2-4
- provider status indicators (green = key found, gray = not configured)

**step 2: agents**
- list of agent rows: name, provider dropdown, model dropdown (from `list_models()`), role dropdown
- selecting a role auto-fills system prompt (editable)
- add/remove buttons. minimum 2 agents.

**step 3: rules**
- topics: text area, one per line
- visibility: group chat vs directed toggle
- termination: dropdown (fixed rounds, topic-based, manual, convergence)
- turn order: round-robin

**step 4: review & launch**
- summary of config. edit buttons to jump back.
- "start debate" button

### debate control bar

appears in header when debate is active:
- debate name + status (running/paused/stopped)
- pause / resume / stop buttons
- current round counter

### settings panel

gear icon in header. API key fields per provider with test button (calls `list_models()` to verify).

## what doesn't change

file watcher, message dedup, chat rendering, search, chips, export, shortcuts, code blocks, animations, activity pulse, bar chart — all existing and v2 features untouched.

## constraints

- zero new tauri plugins
- single-file frontend
- one new rust crate (reqwest)
- API keys never in frontend JS
- orchestrated debates and Claude Code teams coexist in the same UI
