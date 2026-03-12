use crate::config::AppConfig;
use crate::provider::{self, ChatMessage, Provider};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub name: String,
    pub provider: String,
    pub model: String,
    pub system_prompt: String,
    #[serde(default)]
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebateConfig {
    pub team_name: String,
    pub agents: Vec<AgentConfig>,
    pub topics: Vec<String>,
    pub visibility: String,
    pub termination: String,
    pub max_rounds: u32,
    pub convergence_threshold: u32,
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
    #[serde(default)]
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebateThinkingEvent {
    pub team: String,
    pub agent: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebateChunkEvent {
    pub team: String,
    pub agent: String,
    pub chunk: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebateMessageCompleteEvent {
    pub team: String,
    pub agent: String,
    pub from: String,
    pub to: String,
    pub content: String,
    pub timestamp: u64,
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebateStatusEvent {
    pub team: String,
    pub status: String,
    pub round: u32,
    pub total_messages: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_msg: Option<String>,
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
// Hidden debate protocol
// ---------------------------------------------------------------------------

const AUTHORITY_ROLES: &[&str] = &[
    "moderator", "synthesizer", "arbiter", "mediator", "judge", "facilitator",
];

fn is_authority_role(role: &str) -> bool {
    let r = role.to_lowercase();
    AUTHORITY_ROLES.iter().any(|ar| r == *ar || r.contains(ar))
}

fn hidden_debate_instructions(
    agent: &AgentConfig,
    all_agents: &[AgentConfig],
    my_turn_count: usize,
) -> String {
    let authority: Vec<&str> = all_agents
        .iter()
        .filter(|a| is_authority_role(&a.role) && a.name != agent.name)
        .map(|a| a.name.as_str())
        .collect();

    let participant_names: Vec<&str> = all_agents
        .iter()
        .filter(|a| a.name != agent.name)
        .map(|a| a.name.as_str())
        .collect();

    let full_context_check = my_turn_count % 2 == 0;

    let mut lines = vec![
        String::from("--- debate protocol (hidden from user) ---"),
        format!(
            "you are {} in a structured multi-agent debate. other participants: {}.",
            agent.name,
            if participant_names.is_empty() { "none".to_string() } else { participant_names.join(", ") }
        ),
        String::new(),
        String::from("context: the full conversation history is provided above. always read it before responding."),
        String::new(),
        String::from("rules:"),
        String::from("- respond directly to the most recent message before introducing new points"),
        String::from("- be specific — cite evidence, name tradeoffs, give examples. no hand-waving"),
        String::from("- when you concede a point, say so explicitly (\"i concede\", \"you're right\", \"agreed\")"),
        String::from("- don't repeat arguments that have already been conceded or resolved"),
        String::from("- address other agents by name when responding to a specific argument"),
    ];

    if full_context_check {
        lines.push(String::new());
        lines.push(format!(
            "full-context checkpoint (turn {}): before writing your response, scan the entire conversation above. identify: (1) any points that were conceded or resolved earlier that are being relitigated, (2) arguments you or others made in earlier turns that are now contradicted, (3) any direction from an authority agent you haven't acknowledged yet. your response must be grounded in the full thread, not just the last message.",
            my_turn_count + 1
        ));
    }

    if !authority.is_empty() {
        lines.push(String::new());
        lines.push(format!(
            "authority: {} hold directive authority in this debate. when they issue a direction, call for convergence, or declare a point resolved — comply, or clearly state your remaining objection in one sentence. do not re-litigate settled points.",
            authority.join(" and ")
        ));
    }

    if is_authority_role(&agent.role) {
        lines.push(String::new());
        lines.push(String::from(
            "as an authority agent: monitor the debate for circular arguments and unproductive repetition. call them out directly. when the debate has produced enough signal on a point, declare it resolved and move on. your directives are binding — enforce them.",
        ));
    }

    lines.push(String::from("--- end protocol ---"));
    lines.join("\n")
}

// ---------------------------------------------------------------------------
// Context building
// ---------------------------------------------------------------------------

/// Merge consecutive messages with the same role (required by Anthropic) and
/// ensure at least one user message exists before any assistant message.
fn normalize_context(context: Vec<ChatMessage>) -> Vec<ChatMessage> {
    // Separate system messages; we'll re-prepend them at the end
    let (system_msgs, conv_msgs): (Vec<ChatMessage>, Vec<ChatMessage>) =
        context.into_iter().partition(|m| m.role == "system");

    // Merge consecutive same-role messages by concatenating content
    let mut merged: Vec<ChatMessage> = vec![];
    for msg in conv_msgs {
        if let Some(last) = merged.last_mut() {
            if last.role == msg.role {
                last.content.push_str("\n\n");
                last.content.push_str(&msg.content);
                continue;
            }
        }
        merged.push(msg);
    }

    // Ensure conversation starts with a user message
    if merged.is_empty() {
        merged.push(ChatMessage {
            role: "user".to_string(),
            content: "Begin the debate. Share your opening argument on the topic.".to_string(),
        });
    } else if merged[0].role != "user" {
        merged.insert(
            0,
            ChatMessage {
                role: "user".to_string(),
                content: "Continue the debate.".to_string(),
            },
        );
    }

    let mut result = system_msgs;
    result.extend(merged);
    result
}

fn build_context(state: &DebateState, agent: &AgentConfig) -> Vec<ChatMessage> {
    // Count how many times this agent has spoken so far (0-indexed turn count)
    let my_turn_count = state.messages.iter().filter(|m| m.from == agent.name).count();

    let hidden = hidden_debate_instructions(agent, &state.config.agents, my_turn_count);

    // Embed topic + hidden protocol in system prompt
    let system_content = if let Some(topic) = state.config.topics.get(state.current_topic_idx) {
        format!("{}\n\ncurrent debate topic: {topic}\n\n{hidden}", agent.system_prompt)
    } else {
        format!("{}\n\n{hidden}", agent.system_prompt)
    };

    let mut context = vec![ChatMessage {
        role: "system".to_string(),
        content: system_content,
    }];

    // Add conversation history based on visibility mode
    let history: Vec<&DebateMessage> = match state.config.visibility.as_str() {
        "directed" => state
            .messages
            .iter()
            .filter(|m| m.to == agent.name || m.from == agent.name)
            .collect(),
        _ => state.messages.iter().collect(),
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

    normalize_context(context)
}

// ---------------------------------------------------------------------------
// Termination checks
// ---------------------------------------------------------------------------

fn should_stop(state: &DebateState) -> bool {
    match state.config.termination.as_str() {
        "fixed" => state.current_round >= state.config.max_rounds,
        "topic" => state.current_topic_idx >= state.config.topics.len(),
        "convergence" => check_convergence(state),
        "manual" => false,
        _ => false,
    }
}

fn check_convergence(state: &DebateState) -> bool {
    if state.messages.len() < 6 {
        return false;
    }

    let agent_count = state.config.agents.len();
    let agreement_signals = [
        "i agree",
        "i concede",
        "you're right",
        "we've converged",
        "no further objections",
        "i accept",
        "agreed",
    ];

    let recent: Vec<&DebateMessage> = state.messages.iter().rev().take(agent_count * 2).collect();
    let agreeing: usize = recent
        .iter()
        .filter(|m| {
            let lower = m.content.to_lowercase();
            agreement_signals.iter().any(|s| lower.contains(s))
        })
        .count();

    agreeing >= agent_count
}

// ---------------------------------------------------------------------------
// Disk persistence helpers
// ---------------------------------------------------------------------------

fn home_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string()))
}

fn team_inbox_dir(team: &str) -> PathBuf {
    home_dir()
        .join(".claude")
        .join("teams")
        .join(team)
        .join("inboxes")
}

fn safe_filename(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

/// Write the team config.json and create the inboxes directory so watch mode
/// picks it up immediately.
fn init_team_on_disk(config: &DebateConfig) {
    let team_dir = home_dir()
        .join(".claude")
        .join("teams")
        .join(&config.team_name);
    let _ = std::fs::create_dir_all(team_dir.join("inboxes"));

    let team_cfg = serde_json::json!({
        "name": config.team_name,
        "description": format!("debate: {}", config.topics.first().map(String::as_str).unwrap_or("general")),
        "members": config.agents.iter().map(|a| serde_json::json!({
            "name": a.name,
            "model": a.model,
        })).collect::<Vec<_>>(),
    });
    if let Ok(json) = serde_json::to_string_pretty(&team_cfg) {
        let _ = std::fs::write(team_dir.join("config.json"), json);
    }
}

/// Append a message to ~/.claude/teams/{team}/inboxes/{to}.json
fn persist_message(msg: &DebateMessage) {
    let inbox_dir = team_inbox_dir(&msg.team);
    let _ = std::fs::create_dir_all(&inbox_dir);

    let path = inbox_dir.join(format!("{}.json", safe_filename(&msg.to)));

    let mut arr: Vec<serde_json::Value> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();

    arr.push(serde_json::json!({
        "from": msg.from,
        "to":        msg.to,
        "text":      msg.content,
        "timestamp": msg.timestamp,
        "role":      msg.role,
    }));

    if let Ok(json) = serde_json::to_string_pretty(&arr) {
        let _ = std::fs::write(&path, json);
    }
}

// ---------------------------------------------------------------------------
// Orchestrator loop
// ---------------------------------------------------------------------------

pub fn start_debate(
    handle: AppHandle,
    app_config: AppConfig,
    debate_state: Arc<Mutex<DebateState>>,
    seen_hashes: Arc<Mutex<HashSet<u64>>>,
) {
    std::thread::spawn(move || {
        // Create team on disk so watch mode picks it up immediately
        {
            let state = debate_state.lock().unwrap();
            init_team_on_disk(&state.config);
        }

        // Build providers for each agent
        let providers: Vec<Option<Box<dyn Provider>>> = {
            let state = debate_state.lock().unwrap();
            state
                .config
                .agents
                .iter()
                .map(|agent| {
                    let api_key = app_config.api_key(&agent.provider).unwrap_or_default();
                    provider::build_provider(&agent.provider, &api_key)
                })
                .collect()
        };

        // Set status to running
        {
            let mut state = debate_state.lock().unwrap();
            state.status = DebateStatus::Running;
            state.current_round = 1;
            emit_status(&handle, &state);
        }

        'debate: loop {
            let (agent_idx, agent_config, team_name, visibility);
            {
                let state = debate_state.lock().unwrap();

                match &state.status {
                    DebateStatus::Stopped
                    | DebateStatus::Converged
                    | DebateStatus::Error(_) => break,
                    DebateStatus::Paused => {
                        drop(state);
                        std::thread::sleep(std::time::Duration::from_millis(500));
                        continue;
                    }
                    _ => {}
                }

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

            // Build context
            let context = {
                let state = debate_state.lock().unwrap();
                build_context(&state, &agent_config)
            };

            // Call provider
            let provider = match &providers[agent_idx] {
                Some(p) => p,
                None => {
                    let mut state = debate_state.lock().unwrap();
                    state.status = DebateStatus::Error(format!(
                        "no provider configured for agent '{}'",
                        agent_config.name
                    ));
                    emit_status(&handle, &state);
                    break;
                }
            };

            // Signal to frontend that this agent is waiting for a response
            let _ = handle.emit("debate-thinking", DebateThinkingEvent {
                team: team_name.clone(),
                agent: agent_config.name.clone(),
            });

            let response = 'call: {
                let mut last_err = None;
                for attempt in 0..4u32 {
                    let handle_ref = &handle;
                    let agent_name = agent_config.name.clone();
                    let team_ref = team_name.clone();
                    let mut on_chunk = |chunk: &str| {
                        let _ = handle_ref.emit("debate-message-chunk", DebateChunkEvent {
                            team: team_ref.clone(),
                            agent: agent_name.clone(),
                            chunk: chunk.to_string(),
                        });
                    };
                    match provider.chat_streaming(&context, &agent_config.model, &mut on_chunk) {
                        Ok(text) => break 'call text,
                        Err(e) => {
                            let delay = match &e {
                                provider::ProviderError::RateLimit(s) => {
                                    s.parse::<u64>().unwrap_or(0).max(60)
                                }
                                _ => 2u64.pow(attempt),
                            };
                            last_err = Some(e);
                            if attempt < 3 {
                                std::thread::sleep(std::time::Duration::from_secs(delay));
                            }
                        }
                    }
                }
                let mut state = debate_state.lock().unwrap();
                state.status = DebateStatus::Error(format!(
                    "agent '{}' failed: {}",
                    agent_config.name,
                    last_err.unwrap()
                ));
                emit_status(&handle, &state);
                break 'debate;
            };

            // Determine "to" field
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

            let msg = DebateMessage {
                from: agent_config.name.clone(),
                to: to_name,
                content: response,
                timestamp: now_ms(),
                team: team_name.clone(),
                role: agent_config.role.clone(),
            };

            // Pre-insert hash so the file-watcher path won't emit a duplicate
            // new-message event for this streamed message.
            {
                use std::hash::{DefaultHasher, Hash, Hasher};
                let mut h = DefaultHasher::new();
                msg.team.hash(&mut h);
                msg.from.hash(&mut h);
                msg.to.hash(&mut h);
                msg.content.hash(&mut h);
                let hash = h.finish();
                seen_hashes.lock().unwrap().insert(hash);
            }

            // Emit complete event so frontend can finalise the streaming bubble
            let _ = handle.emit("debate-message-complete", DebateMessageCompleteEvent {
                team: msg.team.clone(),
                agent: msg.from.clone(),
                from: msg.from.clone(),
                to: msg.to.clone(),
                content: msg.content.clone(),
                timestamp: msg.timestamp,
                role: msg.role.clone(),
            });

            // Persist to disk — file watcher will skip due to pre-inserted hash
            persist_message(&msg);

            {
                let mut state = debate_state.lock().unwrap();
                state.messages.push(msg);
                state.current_agent_idx = next_idx;

                if next_idx == 0 {
                    state.current_round += 1;

                    // Topic-based: advance topic every 3 rounds
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
                                role: "system".to_string(),
                            };
                            persist_message(&topic_msg);
                            state.messages.push(topic_msg);
                        }
                    }
                }

                emit_status(&handle, &state);
            }

            // Delay between turns
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    });
}

fn emit_status(handle: &AppHandle, state: &DebateState) {
    let (status_str, error_msg) = match &state.status {
        DebateStatus::Running => ("running", None),
        DebateStatus::Paused => ("paused", None),
        DebateStatus::Stopped => ("stopped", None),
        DebateStatus::Converged => ("converged", None),
        DebateStatus::Error(e) => ("error", Some(e.clone())),
    };
    let _ = handle.emit(
        "debate-status",
        DebateStatusEvent {
            team: state.config.team_name.clone(),
            status: status_str.to_string(),
            round: state.current_round,
            total_messages: state.messages.len(),
            error_msg,
        },
    );
}
