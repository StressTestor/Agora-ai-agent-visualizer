# Plan 2: Orchestration Engine — Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the debate orchestration engine that manages agent turns, builds context payloads, and handles 4 termination modes.

**Architecture:** A `DebateState` struct holds the full debate config and message history. The orchestrator runs in a spawned `std::thread`, making blocking provider calls in a loop. It emits the same `new-message` events as the file watcher so the frontend renders orchestrated debates identically to Claude Code teams. Virtual team entries appear in the team selector.

**Tech Stack:** Rust, std::thread, existing provider module from Plan 1

**Depends on:** Plan 1 (provider + config) must be complete.

**Spec:** `docs/superpowers/specs/2026-03-10-multi-model-orchestration-design.md`

**Build command:** `cd /Volumes/onn/debate-watch && PATH="$HOME/.cargo/bin:/Volumes/onn/.cargo-root/bin:$PATH" CARGO_TARGET_DIR=/Volumes/onn/.cargo-tmp cargo build 2>&1 | tail -10`

---

## Chunk 1: Orchestrator Core

### Task 1: Create orchestrator module — types and state

**Files:**
- Create: `src-tauri/src/orchestrator.rs`

- [ ] **Step 1: Write the orchestrator types and state**

Create `src-tauri/src/orchestrator.rs`:

```rust
use crate::config::AppConfig;
use crate::provider::{self, ChatMessage, Provider};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub name: String,
    pub provider: String,  // "openai", "openrouter", "groq", "opencode", "anthropic"
    pub model: String,
    pub system_prompt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebateConfig {
    pub team_name: String,
    pub agents: Vec<AgentConfig>,
    pub topics: Vec<String>,
    pub visibility: String,      // "group" or "directed"
    pub termination: String,     // "fixed", "topic", "manual", "convergence"
    pub max_rounds: u32,
    pub convergence_threshold: u32, // rounds of agreement before stopping
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum DebateStatus {
    Running,
    Paused,
    Stopped,
    Converged,
    Error(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebateMessage {
    pub from: String,
    pub to: String,
    pub content: String,
    pub timestamp: u64,
    pub team: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebateStatusEvent {
    pub team: String,
    pub status: String,
    pub round: u32,
    pub total_messages: usize,
}

pub struct DebateState {
    pub config: DebateConfig,
    pub messages: Vec<DebateMessage>,
    pub status: DebateStatus,
    pub current_round: u32,
    pub current_agent_idx: usize,
    pub current_topic_idx: usize,
}

impl DebateState {
    pub fn new(config: DebateConfig) -> Self {
        Self {
            config,
            messages: vec![],
            status: DebateStatus::Stopped,
            current_round: 0,
            current_agent_idx: 0,
            current_topic_idx: 0,
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ---------------------------------------------------------------------------
// Context building
// ---------------------------------------------------------------------------

/// Build the message list to send to a specific agent based on visibility mode.
fn build_context(
    state: &DebateState,
    agent: &AgentConfig,
) -> Vec<ChatMessage> {
    let mut context = vec![
        ChatMessage {
            role: "system".to_string(),
            content: agent.system_prompt.clone(),
        },
    ];

    // Add current topic as the first user message if topics exist
    if let Some(topic) = state.config.topics.get(state.current_topic_idx) {
        context.push(ChatMessage {
            role: "user".to_string(),
            content: format!("debate topic: {topic}"),
        });
    }

    // Add conversation history based on visibility mode
    let history: Vec<&DebateMessage> = match state.config.visibility.as_str() {
        "directed" => {
            // Only messages TO this agent or FROM this agent
            state.messages.iter().filter(|m| {
                m.to == agent.name || m.from == agent.name
            }).collect()
        }
        _ => {
            // Group chat: all messages
            state.messages.iter().collect()
        }
    };

    for msg in history {
        let role = if msg.from == agent.name {
            "assistant"
        } else {
            "user"
        };
        let content = if role == "user" {
            format!("[{}]: {}", msg.from, msg.content)
        } else {
            msg.content.clone()
        };
        context.push(ChatMessage {
            role: role.to_string(),
            content,
        });
    }

    context
}

// ---------------------------------------------------------------------------
// Termination checks
// ---------------------------------------------------------------------------

/// Check if the debate should stop based on the termination mode.
fn should_stop(state: &DebateState) -> bool {
    match state.config.termination.as_str() {
        "fixed" => state.current_round >= state.config.max_rounds,
        "topic" => {
            // Move to next topic when round completes, stop after last topic
            state.current_topic_idx >= state.config.topics.len()
        }
        "convergence" => {
            check_convergence(state)
        }
        "manual" => false, // only stops via user action
        _ => false,
    }
}

/// Simple convergence check: if the last N messages from each agent
/// are substantially similar to their previous messages, we've converged.
fn check_convergence(state: &DebateState) -> bool {
    if state.messages.len() < 6 {
        return false; // need at least 2 full rounds
    }

    let threshold = state.config.convergence_threshold as usize;
    let agent_count = state.config.agents.len();

    // Check last `threshold` rounds: does each agent keep saying roughly the same thing?
    if state.messages.len() < agent_count * threshold * 2 {
        return false;
    }

    // Simple heuristic: check if any agent explicitly signals agreement
    let recent: Vec<&DebateMessage> = state.messages.iter().rev().take(agent_count * 2).collect();
    let agreement_signals = ["i agree", "i concede", "you're right", "we've converged",
                             "no further objections", "i accept", "agreed"];

    let agreeing_agents: usize = recent.iter().filter(|m| {
        let lower = m.content.to_lowercase();
        agreement_signals.iter().any(|s| lower.contains(s))
    }).count();

    // If majority of recent messages signal agreement, converge
    agreeing_agents >= agent_count
}

// ---------------------------------------------------------------------------
// Orchestrator loop
// ---------------------------------------------------------------------------

/// Run a debate in a background thread. Emits new-message and debate-status events.
pub fn start_debate(
    handle: AppHandle,
    app_config: AppConfig,
    debate_state: Arc<Mutex<DebateState>>,
) {
    std::thread::spawn(move || {
        // Build providers for each agent
        let providers: Vec<Option<Box<dyn Provider>>> = {
            let state = debate_state.lock().unwrap();
            state.config.agents.iter().map(|agent| {
                let api_key = app_config.api_key(&agent.provider).unwrap_or_default();
                provider::build_provider(&agent.provider, &api_key)
            }).collect()
        };

        // Set status to running
        {
            let mut state = debate_state.lock().unwrap();
            state.status = DebateStatus::Running;
            state.current_round = 1;
            emit_status(&handle, &state);
        }

        loop {
            let (agent_idx, agent_config, team_name, visibility);
            {
                let state = debate_state.lock().unwrap();

                // Check if stopped or paused
                match &state.status {
                    DebateStatus::Stopped | DebateStatus::Converged | DebateStatus::Error(_) => break,
                    DebateStatus::Paused => {
                        drop(state);
                        std::thread::sleep(std::time::Duration::from_millis(500));
                        continue;
                    }
                    _ => {}
                }

                // Check termination
                if should_stop(&state) {
                    drop(state);
                    let mut state = debate_state.lock().unwrap();
                    state.status = if state.config.termination == "convergence" {
                        DebateStatus::Converged
                    } else {
                        DebateStatus::Stopped
                    };
                    emit_status(&handle, &state);
                    break;
                }

                agent_idx = state.current_agent_idx;
                agent_config = state.config.agents[agent_idx].clone();
                team_name = state.config.team_name.clone();
                visibility = state.config.visibility.clone();
            }

            // Build context for current agent
            let context = {
                let state = debate_state.lock().unwrap();
                build_context(&state, &agent_config)
            };

            // Call the provider
            let provider = match &providers[agent_idx] {
                Some(p) => p,
                None => {
                    let mut state = debate_state.lock().unwrap();
                    state.status = DebateStatus::Error(
                        format!("no provider configured for agent '{}'", agent_config.name)
                    );
                    emit_status(&handle, &state);
                    break;
                }
            };

            let response = match provider.chat(&context, &agent_config.model) {
                Ok(text) => text,
                Err(e) => {
                    // Retry once
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    match provider.chat(&context, &agent_config.model) {
                        Ok(text) => text,
                        Err(e2) => {
                            let mut state = debate_state.lock().unwrap();
                            state.status = DebateStatus::Error(format!(
                                "agent '{}' failed after retry: {e2}",
                                agent_config.name
                            ));
                            emit_status(&handle, &state);
                            break;
                        }
                    }
                }
            };

            // Determine the "to" field (round-robin: next agent in the list)
            let agent_count;
            {
                let state = debate_state.lock().unwrap();
                agent_count = state.config.agents.len();
            }
            let next_idx = (agent_idx + 1) % agent_count;
            let to_name = {
                let state = debate_state.lock().unwrap();
                if visibility == "directed" {
                    state.config.agents[next_idx].name.clone()
                } else {
                    "all".to_string()
                }
            };

            // Create message
            let msg = DebateMessage {
                from: agent_config.name.clone(),
                to: to_name,
                content: response,
                timestamp: now_ms(),
                team: team_name.clone(),
            };

            // Emit to frontend (same event as file watcher)
            let _ = handle.emit("new-message", &msg);

            // Store in state and advance
            {
                let mut state = debate_state.lock().unwrap();
                state.messages.push(msg);
                state.current_agent_idx = next_idx;

                // If we've gone through all agents, that's one round
                if next_idx == 0 {
                    state.current_round += 1;

                    // For topic-based: advance topic every N rounds (e.g., every 3 rounds)
                    if state.config.termination == "topic" && state.current_round % 3 == 0 {
                        state.current_topic_idx += 1;
                        if state.current_topic_idx < state.config.topics.len() {
                            let topic = state.config.topics[state.current_topic_idx].clone();
                            let topic_msg = DebateMessage {
                                from: "system".to_string(),
                                to: "all".to_string(),
                                content: format!("moving to next topic: {topic}"),
                                timestamp: now_ms(),
                                team: team_name.clone(),
                            };
                            let _ = handle.emit("new-message", &topic_msg);
                            state.messages.push(topic_msg);
                        }
                    }
                }

                emit_status(&handle, &state);
            }

            // Small delay between turns to not hammer APIs
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    });
}

fn emit_status(handle: &AppHandle, state: &DebateState) {
    let status_str = match &state.status {
        DebateStatus::Running => "running",
        DebateStatus::Paused => "paused",
        DebateStatus::Stopped => "stopped",
        DebateStatus::Converged => "converged",
        DebateStatus::Error(_) => "error",
    };
    let _ = handle.emit("debate-status", DebateStatusEvent {
        team: state.config.team_name.clone(),
        status: status_str.to_string(),
        round: state.current_round,
        total_messages: state.messages.len(),
    });
}
```

- [ ] **Step 2: Add mod declaration to main.rs**

Add after `mod presets;`:

```rust
mod orchestrator;
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check`
Expected: compiles with warnings about unused code (no tauri commands wired yet)

- [ ] **Step 4: Commit**

```bash
cd /Volumes/onn/debate-watch
git add src-tauri/src/orchestrator.rs src-tauri/src/main.rs
git commit -m "feat: orchestrator with turn management, context building, 4 termination modes"
```

---

## Chunk 2: Tauri Commands for Debate Lifecycle

### Task 2: Add debate state to AppState and wire tauri commands

**Files:**
- Modify: `src-tauri/src/main.rs`

- [ ] **Step 1: Add debate tracking to AppState**

Add to the use statements:

```rust
use orchestrator::{AgentConfig, DebateConfig, DebateState, DebateStatusEvent};
use std::collections::HashMap;
```

Modify `AppState` to track active debates:

```rust
struct AppState {
    seen_hashes: HashSet<u64>,
    messages: Vec<Message>,
    known_teams: HashSet<String>,
    config: AppConfig,
    debates: HashMap<String, Arc<Mutex<DebateState>>>,
}
```

Update the initial state construction to include `debates: HashMap::new()`.

- [ ] **Step 2: Add debate lifecycle commands**

Add after the existing tauri commands:

```rust
#[tauri::command]
fn create_debate(
    state: State<'_, Arc<Mutex<AppState>>>,
    config: DebateConfig,
) -> Result<String, String> {
    let team_name = config.team_name.clone();
    if team_name.is_empty() {
        return Err("team name cannot be empty".to_string());
    }
    let debate_state = Arc::new(Mutex::new(DebateState::new(config)));
    let mut st = state.lock().unwrap();
    st.debates.insert(team_name.clone(), debate_state);
    Ok(team_name)
}

#[tauri::command]
fn start_debate_cmd(
    app: AppHandle,
    state: State<'_, Arc<Mutex<AppState>>>,
    team_name: String,
) -> Result<(), String> {
    let st = state.lock().unwrap();
    let debate_state = st.debates
        .get(&team_name)
        .ok_or_else(|| format!("no debate found: {team_name}"))?
        .clone();
    let app_config = st.config.clone();
    drop(st);

    // Emit team-added event so the team selector picks it up
    let _ = app.emit("team-added", &team_name);

    orchestrator::start_debate(app, app_config, debate_state);
    Ok(())
}

#[tauri::command]
fn stop_debate(
    state: State<'_, Arc<Mutex<AppState>>>,
    team_name: String,
) -> Result<(), String> {
    let st = state.lock().unwrap();
    let debate_state = st.debates
        .get(&team_name)
        .ok_or_else(|| format!("no debate found: {team_name}"))?;
    let mut ds = debate_state.lock().unwrap();
    ds.status = orchestrator::DebateStatus::Stopped;
    Ok(())
}

#[tauri::command]
fn pause_debate(
    state: State<'_, Arc<Mutex<AppState>>>,
    team_name: String,
) -> Result<(), String> {
    let st = state.lock().unwrap();
    let debate_state = st.debates
        .get(&team_name)
        .ok_or_else(|| format!("no debate found: {team_name}"))?;
    let mut ds = debate_state.lock().unwrap();
    match ds.status {
        orchestrator::DebateStatus::Running => {
            ds.status = orchestrator::DebateStatus::Paused;
        }
        orchestrator::DebateStatus::Paused => {
            ds.status = orchestrator::DebateStatus::Running;
        }
        _ => {}
    }
    Ok(())
}

#[tauri::command]
fn get_debate_status(
    state: State<'_, Arc<Mutex<AppState>>>,
    team_name: String,
) -> Result<DebateStatusEvent, String> {
    let st = state.lock().unwrap();
    let debate_state = st.debates
        .get(&team_name)
        .ok_or_else(|| format!("no debate found: {team_name}"))?;
    let ds = debate_state.lock().unwrap();
    let status_str = match &ds.status {
        orchestrator::DebateStatus::Running => "running",
        orchestrator::DebateStatus::Paused => "paused",
        orchestrator::DebateStatus::Stopped => "stopped",
        orchestrator::DebateStatus::Converged => "converged",
        orchestrator::DebateStatus::Error(_) => "error",
    };
    Ok(DebateStatusEvent {
        team: team_name,
        status: status_str.to_string(),
        round: ds.current_round,
        total_messages: ds.messages.len(),
    })
}
```

- [ ] **Step 3: Register all new commands**

Update the `.invoke_handler()`:

```rust
        .invoke_handler(tauri::generate_handler![
            get_teams,
            get_messages,
            get_config,
            save_config,
            list_models,
            list_role_presets,
            list_debate_presets,
            create_debate,
            start_debate_cmd,
            stop_debate,
            pause_debate,
            get_debate_status,
        ])
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo check`
Expected: compiles with no errors

- [ ] **Step 5: Smoke test from dev console**

Run `cargo tauri dev`, then in the WebView console:

```javascript
// Create a debate (won't run yet — no real API keys)
await window.__TAURI__.core.invoke('create_debate', {
  config: {
    team_name: 'test-debate',
    agents: [
      { name: 'alice', provider: 'openai', model: 'gpt-4o', system_prompt: 'you are helpful' },
      { name: 'bob', provider: 'openai', model: 'gpt-4o', system_prompt: 'you are critical' },
    ],
    topics: ['should we use rust or go?'],
    visibility: 'group',
    termination: 'fixed',
    max_rounds: 3,
    convergence_threshold: 2,
  }
})
// Should return 'test-debate'

// Check status
await window.__TAURI__.core.invoke('get_debate_status', { teamName: 'test-debate' })
// Should return { team: 'test-debate', status: 'stopped', round: 0, total_messages: 0 }
```

- [ ] **Step 6: Commit**

```bash
cd /Volumes/onn/debate-watch
git add src-tauri/src/main.rs
git commit -m "feat: tauri commands for debate lifecycle (create, start, stop, pause, status)"
```
