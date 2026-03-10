# Plan 1: Provider Abstraction + Config — Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the HTTP client layer and config system so Agora can talk to 5 LLM providers and persist API keys.

**Architecture:** A `Provider` trait with two implementations (OpenAI-compatible, Anthropic) behind blocking reqwest calls. Config loads from `~/.config/agora/config.json` with env var overrides. Role and debate presets are static data. Everything wires into main.rs as tauri commands.

**Tech Stack:** Rust, reqwest (blocking + json + rustls-tls), serde, tauri 2

**Spec:** `docs/superpowers/specs/2026-03-10-multi-model-orchestration-design.md`

**Build command:** `cd /Volumes/onn/debate-watch && PATH="$HOME/.cargo/bin:/Volumes/onn/.cargo-root/bin:$PATH" CARGO_TARGET_DIR=/Volumes/onn/.cargo-tmp cargo build 2>&1 | tail -10`

---

## Chunk 1: Dependencies and Config

### Task 1: Add reqwest dependency

**Files:**
- Modify: `src-tauri/Cargo.toml`

- [ ] **Step 1: Add reqwest to dependencies**

Add after the `chrono = "0.4"` line:

```toml
reqwest = { version = "0.12", features = ["json", "blocking", "rustls-tls"] }
```

- [ ] **Step 2: Verify it compiles**

Run: `cd /Volumes/onn/debate-watch && PATH="$HOME/.cargo/bin:/Volumes/onn/.cargo-root/bin:$PATH" CARGO_TARGET_DIR=/Volumes/onn/.cargo-tmp cargo check 2>&1 | tail -5`
Expected: `Finished` with no errors (will download reqwest and deps first time)

- [ ] **Step 3: Commit**

```bash
cd /Volumes/onn/debate-watch
git add src-tauri/Cargo.toml
git commit -m "deps: add reqwest for LLM provider HTTP calls"
```

---

### Task 2: Create config module

**Files:**
- Create: `src-tauri/src/config.rs`

- [ ] **Step 1: Write the config module**

Create `src-tauri/src/config.rs`:

```rust
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub api_key: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppConfig {
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,
}

/// Known provider names and their corresponding env var names.
const PROVIDER_ENV_VARS: &[(&str, &str)] = &[
    ("openai", "OPENAI_API_KEY"),
    ("openrouter", "OPENROUTER_API_KEY"),
    ("groq", "GROQ_API_KEY"),
    ("opencode", "OPENCODE_API_KEY"),
    ("anthropic", "ANTHROPIC_API_KEY"),
];

fn config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".config").join("agora").join("config.json")
}

impl AppConfig {
    /// Load config from file, then overlay env var overrides.
    pub fn load() -> Self {
        let path = config_path();
        let mut config: AppConfig = fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();

        // Env vars override file values
        for (provider, env_var) in PROVIDER_ENV_VARS {
            if let Ok(key) = std::env::var(env_var) {
                if !key.is_empty() {
                    config.providers.insert(
                        provider.to_string(),
                        ProviderConfig {
                            api_key: key,
                            enabled: true,
                        },
                    );
                }
            }
        }

        config
    }

    /// Save config to file. Creates parent dirs if needed.
    pub fn save(&self) -> Result<(), String> {
        let path = config_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("failed to create config dir: {e}"))?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("failed to serialize config: {e}"))?;
        fs::write(&path, &json).map_err(|e| format!("failed to write config: {e}"))?;

        // Set 600 permissions on unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = fs::Permissions::from_mode(0o600);
            fs::set_permissions(&path, perms).ok();
        }

        Ok(())
    }

    /// Get the API key for a provider (resolved: env var > file).
    pub fn api_key(&self, provider: &str) -> Option<String> {
        self.providers
            .get(provider)
            .filter(|p| p.enabled && !p.api_key.is_empty())
            .map(|p| p.api_key.clone())
    }

    /// List providers that have a valid API key configured.
    pub fn configured_providers(&self) -> Vec<String> {
        self.providers
            .iter()
            .filter(|(_, v)| v.enabled && !v.api_key.is_empty())
            .map(|(k, _)| k.clone())
            .collect()
    }
}
```

- [ ] **Step 2: Add mod declaration to main.rs**

At the top of `src-tauri/src/main.rs`, after the `#![cfg_attr...]` line, add:

```rust
mod config;
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check`
Expected: compiles with no errors

- [ ] **Step 4: Commit**

```bash
cd /Volumes/onn/debate-watch
git add src-tauri/src/config.rs src-tauri/src/main.rs
git commit -m "feat: config module with file + env var API key resolution"
```

---

## Chunk 2: Provider Abstraction

### Task 3: Create provider module

**Files:**
- Create: `src-tauri/src/provider.rs`

- [ ] **Step 1: Write the provider module**

Create `src-tauri/src/provider.rs`:

```rust
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Shared types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,    // "system", "user", "assistant"
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub provider: String,
}

/// Errors from provider calls.
#[derive(Debug)]
pub enum ProviderError {
    Network(String),
    Auth(String),
    RateLimit(String),
    Other(String),
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Network(e) => write!(f, "network error: {e}"),
            Self::Auth(e) => write!(f, "auth error: {e}"),
            Self::RateLimit(e) => write!(f, "rate limit: {e}"),
            Self::Other(e) => write!(f, "provider error: {e}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Provider trait
// ---------------------------------------------------------------------------

pub trait Provider: Send + Sync {
    fn name(&self) -> &str;
    fn chat(&self, messages: &[ChatMessage], model: &str) -> Result<String, ProviderError>;
    fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError>;
}

// ---------------------------------------------------------------------------
// OpenAI-compatible client (covers OpenAI, OpenRouter, Groq, OpenCode)
// ---------------------------------------------------------------------------

pub struct OpenAiCompatible {
    provider_name: String,
    base_url: String,
    api_key: String,
    client: reqwest::blocking::Client,
}

#[derive(Serialize)]
struct OpenAiRequest {
    model: String,
    messages: Vec<ChatMessage>,
}

#[derive(Deserialize)]
struct OpenAiResponse {
    choices: Vec<OpenAiChoice>,
}

#[derive(Deserialize)]
struct OpenAiChoice {
    message: OpenAiMessage,
}

#[derive(Deserialize)]
struct OpenAiMessage {
    content: String,
}

#[derive(Deserialize)]
struct OpenAiModelsResponse {
    data: Vec<OpenAiModelEntry>,
}

#[derive(Deserialize)]
struct OpenAiModelEntry {
    id: String,
}

impl OpenAiCompatible {
    pub fn new(provider_name: &str, base_url: &str, api_key: &str) -> Self {
        Self {
            provider_name: provider_name.to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            client: reqwest::blocking::Client::new(),
        }
    }

    /// Create a provider for a known provider name. Returns None if unknown.
    pub fn for_provider(name: &str, api_key: &str) -> Option<Self> {
        let base_url = match name {
            "openai" => "https://api.openai.com/v1",
            "openrouter" => "https://openrouter.ai/api/v1",
            "groq" => "https://api.groq.com/openai/v1",
            "opencode" => "https://openrouter.ai/api/v1", // placeholder, same shape
            _ => return None,
        };
        Some(Self::new(name, base_url, api_key))
    }
}

impl Provider for OpenAiCompatible {
    fn name(&self) -> &str {
        &self.provider_name
    }

    fn chat(&self, messages: &[ChatMessage], model: &str) -> Result<String, ProviderError> {
        let url = format!("{}/chat/completions", self.base_url);
        let body = OpenAiRequest {
            model: model.to_string(),
            messages: messages.to_vec(),
        };

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .map_err(|e| ProviderError::Network(e.to_string()))?;

        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ProviderError::Auth("invalid API key".to_string()));
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(ProviderError::RateLimit("rate limited".to_string()));
        }
        if !status.is_success() {
            let text = resp.text().unwrap_or_default();
            return Err(ProviderError::Other(format!("HTTP {status}: {text}")));
        }

        let parsed: OpenAiResponse = resp
            .json()
            .map_err(|e| ProviderError::Other(format!("failed to parse response: {e}")))?;

        parsed
            .choices
            .first()
            .map(|c| c.message.content.clone())
            .ok_or_else(|| ProviderError::Other("no choices in response".to_string()))
    }

    fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        let url = format!("{}/models", self.base_url);
        let resp = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .map_err(|e| ProviderError::Network(e.to_string()))?;

        if !resp.status().is_success() {
            let text = resp.text().unwrap_or_default();
            return Err(ProviderError::Other(format!("failed to list models: {text}")));
        }

        let parsed: OpenAiModelsResponse = resp
            .json()
            .map_err(|e| ProviderError::Other(format!("failed to parse models: {e}")))?;

        Ok(parsed
            .data
            .into_iter()
            .map(|m| ModelInfo {
                id: m.id,
                provider: self.provider_name.clone(),
            })
            .collect())
    }
}

// ---------------------------------------------------------------------------
// Anthropic client
// ---------------------------------------------------------------------------

pub struct AnthropicClient {
    api_key: String,
    client: reqwest::blocking::Client,
}

#[derive(Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<AnthropicMessage>,
}

#[derive(Serialize)]
struct AnthropicMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct AnthropicResponse {
    content: Vec<AnthropicContent>,
}

#[derive(Deserialize)]
struct AnthropicContent {
    text: String,
}

#[derive(Deserialize)]
struct AnthropicModelsResponse {
    data: Vec<AnthropicModelEntry>,
}

#[derive(Deserialize)]
struct AnthropicModelEntry {
    id: String,
}

impl AnthropicClient {
    pub fn new(api_key: &str) -> Self {
        Self {
            api_key: api_key.to_string(),
            client: reqwest::blocking::Client::new(),
        }
    }
}

impl Provider for AnthropicClient {
    fn name(&self) -> &str {
        "anthropic"
    }

    fn chat(&self, messages: &[ChatMessage], model: &str) -> Result<String, ProviderError> {
        // Extract system message if present
        let system = messages
            .iter()
            .find(|m| m.role == "system")
            .map(|m| m.content.clone());

        // Convert remaining messages (skip system role)
        let api_messages: Vec<AnthropicMessage> = messages
            .iter()
            .filter(|m| m.role != "system")
            .map(|m| AnthropicMessage {
                role: m.role.clone(),
                content: m.content.clone(),
            })
            .collect();

        let body = AnthropicRequest {
            model: model.to_string(),
            max_tokens: 4096,
            system,
            messages: api_messages,
        };

        let resp = self
            .client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .map_err(|e| ProviderError::Network(e.to_string()))?;

        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ProviderError::Auth("invalid API key".to_string()));
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(ProviderError::RateLimit("rate limited".to_string()));
        }
        if !status.is_success() {
            let text = resp.text().unwrap_or_default();
            return Err(ProviderError::Other(format!("HTTP {status}: {text}")));
        }

        let parsed: AnthropicResponse = resp
            .json()
            .map_err(|e| ProviderError::Other(format!("failed to parse response: {e}")))?;

        parsed
            .content
            .first()
            .map(|c| c.text.clone())
            .ok_or_else(|| ProviderError::Other("no content in response".to_string()))
    }

    fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        let resp = self
            .client
            .get("https://api.anthropic.com/v1/models")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .send()
            .map_err(|e| ProviderError::Network(e.to_string()))?;

        if !resp.status().is_success() {
            // Anthropic may not have a models endpoint — return known models
            return Ok(vec![
                ModelInfo { id: "claude-opus-4-6".to_string(), provider: "anthropic".to_string() },
                ModelInfo { id: "claude-sonnet-4-6".to_string(), provider: "anthropic".to_string() },
                ModelInfo { id: "claude-haiku-4-5-20251001".to_string(), provider: "anthropic".to_string() },
            ]);
        }

        let parsed: AnthropicModelsResponse = resp
            .json()
            .map_err(|e| ProviderError::Other(format!("failed to parse models: {e}")))?;

        Ok(parsed
            .data
            .into_iter()
            .map(|m| ModelInfo {
                id: m.id,
                provider: "anthropic".to_string(),
            })
            .collect())
    }
}

// ---------------------------------------------------------------------------
// Provider factory
// ---------------------------------------------------------------------------

/// Build a boxed Provider for the given provider name and API key.
pub fn build_provider(name: &str, api_key: &str) -> Option<Box<dyn Provider>> {
    match name {
        "anthropic" => Some(Box::new(AnthropicClient::new(api_key))),
        "openai" | "openrouter" | "groq" | "opencode" => {
            OpenAiCompatible::for_provider(name, api_key).map(|p| Box::new(p) as Box<dyn Provider>)
        }
        _ => None,
    }
}
```

- [ ] **Step 2: Add mod declaration to main.rs**

Add after `mod config;`:

```rust
mod provider;
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check`
Expected: compiles (warnings about unused code are fine at this stage)

- [ ] **Step 4: Commit**

```bash
cd /Volumes/onn/debate-watch
git add src-tauri/src/provider.rs src-tauri/src/main.rs
git commit -m "feat: provider abstraction with OpenAI-compatible and Anthropic clients"
```

---

## Chunk 3: Presets and Tauri Commands

### Task 4: Create presets module

**Files:**
- Create: `src-tauri/src/presets.rs`

- [ ] **Step 1: Write the presets module**

Create `src-tauri/src/presets.rs`:

```rust
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct RolePreset {
    pub name: String,
    pub description: String,
    pub system_prompt: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DebatePresetAgent {
    pub name: String,
    pub role: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DebatePreset {
    pub name: String,
    pub description: String,
    pub agents: Vec<DebatePresetAgent>,
    pub visibility: String,    // "group" or "directed"
    pub termination: String,   // "convergence", "fixed", "topic", "manual"
    pub default_rounds: u32,
}

pub fn role_presets() -> Vec<RolePreset> {
    vec![
        RolePreset {
            name: "advocate".to_string(),
            description: "argues FOR a position".to_string(),
            system_prompt: "you are arguing in favor of the proposed position. make your case with concrete evidence and specific examples. anticipate counterarguments and address them head-on. if your position has genuine weaknesses, acknowledge them and explain why the strengths outweigh them. don't hand-wave — show your work.".to_string(),
        },
        RolePreset {
            name: "critic".to_string(),
            description: "stress-tests proposals".to_string(),
            system_prompt: "you stress-test every proposal that comes your way. find the real weaknesses — implementation complexity, hidden assumptions, edge cases the advocate glossed over. push back hard, but be honest: if an argument genuinely holds up under scrutiny, say so and move on. concede points that are legitimately proven. being wrong is fine. being stubbornly wrong wastes everyone's time.".to_string(),
        },
        RolePreset {
            name: "synthesizer".to_string(),
            description: "neutral arbiter, writes conclusions".to_string(),
            system_prompt: "you are the neutral arbiter. watch the debate, identify where the two sides actually agree vs where the disagreement is real. ask pointed questions that force concrete answers — no hand-waving from either side. when the debate stalls, reframe the problem. your final output is a clear decision with reasoning, not a compromise that makes nobody happy.".to_string(),
        },
        RolePreset {
            name: "researcher".to_string(),
            description: "gathers facts and context".to_string(),
            system_prompt: "you gather facts and context that inform the debate. look up specifics — benchmarks, API docs, implementation examples, known tradeoffs. present what you find without editorializing. if the data contradicts someone's claim, say so plainly. if you can't verify something, say that too.".to_string(),
        },
        RolePreset {
            name: "moderator".to_string(),
            description: "keeps debate productive and focused".to_string(),
            system_prompt: "you keep the debate productive. if someone repeats a point that's already been addressed, call it out. if the conversation drifts off-topic, pull it back. summarize where things stand after each round. you don't take sides, but you do call out weak arguments and demand specifics when someone is being vague.".to_string(),
        },
    ]
}

pub fn debate_presets() -> Vec<DebatePreset> {
    vec![
        DebatePreset {
            name: "3-agent deliberation".to_string(),
            description: "advocate + critic + synthesizer in group chat, runs until convergence".to_string(),
            agents: vec![
                DebatePresetAgent { name: "advocate".to_string(), role: "advocate".to_string() },
                DebatePresetAgent { name: "critic".to_string(), role: "critic".to_string() },
                DebatePresetAgent { name: "synthesizer".to_string(), role: "synthesizer".to_string() },
            ],
            visibility: "group".to_string(),
            termination: "convergence".to_string(),
            default_rounds: 10,
        },
        DebatePreset {
            name: "red team / blue team".to_string(),
            description: "two opposing advocates + moderator, directed messages, 5 rounds".to_string(),
            agents: vec![
                DebatePresetAgent { name: "red-team".to_string(), role: "advocate".to_string() },
                DebatePresetAgent { name: "blue-team".to_string(), role: "advocate".to_string() },
                DebatePresetAgent { name: "moderator".to_string(), role: "moderator".to_string() },
            ],
            visibility: "directed".to_string(),
            termination: "fixed".to_string(),
            default_rounds: 5,
        },
        DebatePreset {
            name: "research panel".to_string(),
            description: "two researchers + synthesizer in group chat, topic-based".to_string(),
            agents: vec![
                DebatePresetAgent { name: "researcher-1".to_string(), role: "researcher".to_string() },
                DebatePresetAgent { name: "researcher-2".to_string(), role: "researcher".to_string() },
                DebatePresetAgent { name: "synthesizer".to_string(), role: "synthesizer".to_string() },
            ],
            visibility: "group".to_string(),
            termination: "topic".to_string(),
            default_rounds: 10,
        },
    ]
}
```

- [ ] **Step 2: Add mod declaration to main.rs**

Add after `mod provider;`:

```rust
mod presets;
```

- [ ] **Step 3: Commit**

```bash
cd /Volumes/onn/debate-watch
git add src-tauri/src/presets.rs src-tauri/src/main.rs
git commit -m "feat: role and debate presets with tightened system prompts"
```

---

### Task 5: Wire config and provider tauri commands into main.rs

**Files:**
- Modify: `src-tauri/src/main.rs`

- [ ] **Step 1: Add new use statements**

At the top of main.rs, add:

```rust
use config::AppConfig;
use provider::ModelInfo;
```

- [ ] **Step 2: Add config to shared state**

Modify the `AppState` struct to include config:

```rust
struct AppState {
    seen_hashes: HashSet<u64>,
    messages: Vec<Message>,
    known_teams: HashSet<String>,
    config: AppConfig,
}
```

Update the initial state construction in `main()` to load config:

```rust
    let shared_state = Arc::new(Mutex::new(AppState {
        seen_hashes: HashSet::new(),
        messages: vec![],
        known_teams: initial_known,
        config: AppConfig::load(),
    }));
```

- [ ] **Step 3: Add tauri commands**

Add after the existing `get_messages` command:

```rust
#[tauri::command]
fn get_config(state: State<'_, Arc<Mutex<AppState>>>) -> AppConfig {
    state.lock().unwrap().config.clone()
}

#[tauri::command]
fn save_config(state: State<'_, Arc<Mutex<AppState>>>, config: AppConfig) -> Result<(), String> {
    config.save()?;
    state.lock().unwrap().config = config;
    Ok(())
}

#[tauri::command]
fn list_models(state: State<'_, Arc<Mutex<AppState>>>, provider_name: String) -> Result<Vec<ModelInfo>, String> {
    let config = state.lock().unwrap().config.clone();
    let api_key = config
        .api_key(&provider_name)
        .ok_or_else(|| format!("no API key configured for {provider_name}"))?;
    let p = provider::build_provider(&provider_name, &api_key)
        .ok_or_else(|| format!("unknown provider: {provider_name}"))?;
    p.list_models().map_err(|e| e.to_string())
}

#[tauri::command]
fn list_role_presets() -> Vec<presets::RolePreset> {
    presets::role_presets()
}

#[tauri::command]
fn list_debate_presets() -> Vec<presets::DebatePreset> {
    presets::debate_presets()
}
```

- [ ] **Step 4: Register new commands in the invoke handler**

Update the `.invoke_handler()` line:

```rust
        .invoke_handler(tauri::generate_handler![
            get_teams,
            get_messages,
            get_config,
            save_config,
            list_models,
            list_role_presets,
            list_debate_presets,
        ])
```

- [ ] **Step 5: Verify it compiles**

Run: `cargo check`
Expected: compiles with no errors

- [ ] **Step 6: Verify commands work from dev console**

Run: `cargo tauri dev`

In the WebView dev console (Cmd+Option+I), test:

```javascript
// Should return the config object
await window.__TAURI__.core.invoke('get_config')

// Should return role presets array
await window.__TAURI__.core.invoke('list_role_presets')

// Should return debate presets array
await window.__TAURI__.core.invoke('list_debate_presets')
```

- [ ] **Step 7: Commit**

```bash
cd /Volumes/onn/debate-watch
git add src-tauri/src/main.rs
git commit -m "feat: tauri commands for config, models, and presets"
```
