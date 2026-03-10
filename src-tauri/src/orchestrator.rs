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
    pub provider: String,
    pub model: String,
    pub system_prompt: String,
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

fn build_context(state: &DebateState, agent: &AgentConfig) -> Vec<ChatMessage> {
    let mut context = vec![ChatMessage {
        role: "system".to_string(),
        content: agent.system_prompt.clone(),
    }];

    // Add current topic as the first user message
    if let Some(topic) = state.config.topics.get(state.current_topic_idx) {
        context.push(ChatMessage {
            role: "user".to_string(),
            content: format!("debate topic: {topic}"),
        });
    }

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

    context
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
// Orchestrator loop
// ---------------------------------------------------------------------------

pub fn start_debate(
    handle: AppHandle,
    app_config: AppConfig,
    debate_state: Arc<Mutex<DebateState>>,
) {
    std::thread::spawn(move || {
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

        loop {
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

            let response = match provider.chat(&context, &agent_config.model) {
                Ok(text) => text,
                Err(_) => {
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
            };

            let _ = handle.emit("new-message", &msg);

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
                            };
                            let _ = handle.emit("new-message", &topic_msg);
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
    let status_str = match &state.status {
        DebateStatus::Running => "running",
        DebateStatus::Paused => "paused",
        DebateStatus::Stopped => "stopped",
        DebateStatus::Converged => "converged",
        DebateStatus::Error(_) => "error",
    };
    let _ = handle.emit(
        "debate-status",
        DebateStatusEvent {
            team: state.config.team_name.clone(),
            status: status_str.to_string(),
            round: state.current_round,
            total_messages: state.messages.len(),
        },
    );
}
