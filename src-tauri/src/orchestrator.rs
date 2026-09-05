use crate::config::AppConfig;
use crate::model_profiles;
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
    /// Consecutive complete rounds in which every participant agrees.
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
    pub worker_active: bool,
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
            worker_active: false,
        }
    }
}

pub fn validate_debate_config(config: &DebateConfig) -> Result<(), String> {
    crate::validate_team_name(&config.team_name)?;
    if !matches!(
        config.termination.as_str(),
        "fixed" | "topic" | "convergence" | "manual"
    ) {
        return Err("Choose a supported termination mode.".to_string());
    }
    if !matches!(config.visibility.as_str(), "group" | "directed") {
        return Err("Choose group or directed visibility.".to_string());
    }
    if config.termination == "fixed" && config.max_rounds == 0 {
        return Err("Fixed debates need at least one round.".to_string());
    }
    if config.termination == "convergence" && config.convergence_threshold == 0 {
        return Err("Convergence needs at least one round of agreement.".to_string());
    }
    if config.agents.is_empty() {
        return Err("Add at least one agent before starting the debate.".to_string());
    }
    let mut names = HashSet::new();
    for agent in &config.agents {
        let name = agent.name.trim();
        if name.is_empty() || !names.insert(name) {
            return Err("Each agent needs a nonempty, unique name.".to_string());
        }
        if agent.provider.trim().is_empty() || agent.model.trim().is_empty() {
            return Err(format!("Choose a provider and model for agent '{name}'."));
        }
    }
    if config.termination == "topic"
        && (config.topics.is_empty() || config.topics.iter().any(|topic| topic.trim().is_empty()))
    {
        return Err("Topic-based debates need at least one nonempty topic.".to_string());
    }
    Ok(())
}

// Status changes immediately on stop; ownership lasts until the blocking
// provider call and its worker actually exit.
struct DebateWorkerGuard(Arc<Mutex<DebateState>>);

impl DebateWorkerGuard {
    fn claim(state: &Arc<Mutex<DebateState>>, restart: bool) -> Result<Self, String> {
        let mut debate = state.lock().unwrap();
        if debate.worker_active {
            return Err("This debate still has an active worker. Stop it and wait for the current response to finish before starting again.".to_string());
        }
        validate_debate_config(&debate.config)?;
        if restart {
            debate.messages.clear();
            debate.current_round = 0;
            debate.current_agent_idx = 0;
            debate.current_topic_idx = 0;
        }
        debate.worker_active = true;
        debate.status = DebateStatus::Running;
        debate.current_round = debate.current_round.max(1);
        Ok(Self(state.clone()))
    }
}

impl Drop for DebateWorkerGuard {
    fn drop(&mut self) {
        let mut debate = self.0.lock().unwrap_or_else(|error| error.into_inner());
        debate.worker_active = false;
        if matches!(debate.status, DebateStatus::Running | DebateStatus::Paused) {
            debate.status =
                DebateStatus::Error("The debate worker exited unexpectedly.".to_string());
        }
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ---------------------------------------------------------------------------
// Hidden debate protocol
// ---------------------------------------------------------------------------

const AUTHORITY_ROLES: &[&str] = &[
    "moderator",
    "synthesizer",
    "arbiter",
    "mediator",
    "judge",
    "facilitator",
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

    let full_context_check = my_turn_count.is_multiple_of(2);

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
pub fn normalize_context(context: Vec<ChatMessage>) -> Vec<ChatMessage> {
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

pub fn build_context(state: &DebateState, agent: &AgentConfig) -> Vec<ChatMessage> {
    // Count how many times this agent has spoken so far (0-indexed turn count)
    let my_turn_count = state
        .messages
        .iter()
        .filter(|m| m.from == agent.name)
        .count();

    let hidden = hidden_debate_instructions(agent, &state.config.agents, my_turn_count);

    // Inject model identity profile for arena roles
    let model_identity = if agent.role == "judge" {
        // Judge gets all debater profiles for comparative context
        let debater_profiles: Vec<String> = state
            .config
            .agents
            .iter()
            .filter(|a| a.role == "debater" && a.name != agent.name)
            .filter_map(|a| {
                model_profiles::get_model_profile(&a.provider, &a.model)
                    .map(|p| format!("[{} — {}]: {}", a.name, a.model, p))
            })
            .collect();
        if debater_profiles.is_empty() {
            String::new()
        } else {
            format!(
                "\n\nmodels you are judging:\n{}",
                debater_profiles.join("\n\n")
            )
        }
    } else if agent.role == "debater" {
        model_profiles::get_model_profile(&agent.provider, &agent.model)
            .map(|p| format!("\n\nyour model identity:\n{p}"))
            .unwrap_or_default()
    } else {
        String::new()
    };

    // Embed topic + model identity + hidden protocol in system prompt
    let system_content = if let Some(topic) = state.config.topics.get(state.current_topic_idx) {
        format!(
            "{}{model_identity}\n\ncurrent debate topic: {topic}\n\n{hidden}",
            agent.system_prompt
        )
    } else {
        format!("{}{model_identity}\n\n{hidden}", agent.system_prompt)
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

pub fn should_stop(state: &DebateState) -> bool {
    match state.config.termination.as_str() {
        // current_round identifies the next round to run, starting at one.
        "fixed" => state.current_round > state.config.max_rounds,
        "topic" => {
            state.current_topic_idx >= state.config.topics.len()
                || (state.current_round.saturating_sub(1) / 3) as usize >= state.config.topics.len()
        }
        "convergence" => check_convergence(state),
        "manual" => false,
        _ => false,
    }
}

fn check_convergence(state: &DebateState) -> bool {
    let agent_count = state.config.agents.len();
    let threshold = state.config.convergence_threshold.max(1) as usize;
    let Some(required) = agent_count.checked_mul(threshold) else {
        return false;
    };
    // Agreement is assessed only after complete rounds, never before the
    // remaining participants have had their opportunity to object.
    if agent_count == 0 || state.current_agent_idx != 0 || state.messages.len() < required {
        return false;
    }
    let names: HashSet<&str> = state
        .config
        .agents
        .iter()
        .map(|agent| agent.name.as_str())
        .collect();
    let recent: Vec<_> = state
        .messages
        .iter()
        .rev()
        .filter(|message| names.contains(message.from.as_str()))
        .take(required)
        .collect();
    recent.len() == required
        && recent.chunks(agent_count).all(|round| {
            let speakers: HashSet<_> = round.iter().map(|message| message.from.as_str()).collect();
            speakers == names
                && round
                    .iter()
                    .all(|message| expresses_agreement(&message.content))
        })
}

fn expresses_agreement(content: &str) -> bool {
    let lower = content.to_lowercase().replace('’', "'");
    let words = lower
        .split(|character: char| !character.is_alphanumeric() && character != '\'')
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let text = format!(" {words} ");
    let dissent = [
        "i disagree",
        "i do not agree",
        "i don't agree",
        "not agreed",
        "i reject",
        "no agreement",
        "not converged",
        "i cannot agree",
        "i can't agree",
    ];
    if dissent
        .iter()
        .any(|phrase| text.contains(&format!(" {phrase} ")))
    {
        return false;
    }
    let agreement_signals = [
        "i agree",
        "i concede",
        "you're right",
        "we've converged",
        "no further objections",
        "i accept",
        "agreed",
    ];

    agreement_signals
        .iter()
        .any(|phrase| text.contains(&format!(" {phrase} ")))
}

// ---------------------------------------------------------------------------
// Disk persistence helpers
// ---------------------------------------------------------------------------

fn home_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string()))
}

fn team_inbox_dir(team: &str) -> Result<PathBuf, String> {
    Ok(crate::team_path_from(&home_dir().join(".claude/teams"), team)?.join("inboxes"))
}

fn safe_filename(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Initialize the exact logical team folder; storage errors stop the run.
pub fn init_team_on_disk(config: &DebateConfig) -> Result<(), String> {
    let team_dir = crate::team_path_from(&home_dir().join(".claude/teams"), &config.team_name)?;
    init_team_at(&team_dir, config)
}

fn init_team_at(team_dir: &std::path::Path, config: &DebateConfig) -> Result<(), String> {
    std::fs::create_dir_all(team_dir.join("inboxes"))
        .map_err(|error| format!("Could not create team inboxes: {error}"))?;
    let team_cfg = serde_json::json!({
        "name": config.team_name,
        "description": format!("debate: {}", config.topics.first().map(String::as_str).unwrap_or("general")),
        "members": config.agents.iter().map(|a| serde_json::json!({
            "name": a.name, "model": a.model,
        })).collect::<Vec<_>>(),
    });
    crate::config::write_json_atomic(&team_dir.join("config.json"), &team_cfg)
}

/// Append a complete message without overwriting malformed or unreadable history.
pub fn persist_message(msg: &DebateMessage) -> Result<(), String> {
    persist_message_in(&team_inbox_dir(&msg.team)?, msg)
}

fn persist_message_in(inbox_dir: &std::path::Path, msg: &DebateMessage) -> Result<(), String> {
    std::fs::create_dir_all(inbox_dir)
        .map_err(|error| format!("Could not create inbox directory: {error}"))?;
    let path = inbox_dir.join(format!("{}.json", safe_filename(&msg.to)));
    let mut arr: Vec<serde_json::Value> = match std::fs::read(&path) {
        Ok(raw) => serde_json::from_slice(&raw).map_err(|error| {
            format!("Inbox history is malformed; existing data was preserved: {error}")
        })?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => vec![],
        Err(error) => return Err(format!("Could not read existing inbox history: {error}")),
    };
    arr.push(serde_json::json!({
        "from": msg.from, "to": msg.to, "text": msg.content,
        "timestamp": msg.timestamp, "role": msg.role,
    }));
    crate::config::write_json_atomic(&path, &arr)
}

// ---------------------------------------------------------------------------
// Orchestrator loop
// ---------------------------------------------------------------------------

pub fn start_debate(
    handle: AppHandle,
    app_config: AppConfig,
    debate_state: Arc<Mutex<DebateState>>,
    seen_hashes: Arc<Mutex<HashSet<u64>>>,
    restart: bool,
) -> Result<(), String> {
    let worker = DebateWorkerGuard::claim(&debate_state, restart)?;
    emit_status(&handle, &debate_state.lock().unwrap());
    let failure_state = debate_state.clone();
    std::thread::Builder::new()
        .spawn(move || {
            let _worker = worker;
            // Create team on disk so watch mode picks it up immediately
            {
                let mut state = debate_state.lock().unwrap();
                if let Err(error) = init_team_on_disk(&state.config) {
                    state.status = DebateStatus::Error(error);
                    emit_status(&handle, &state);
                    return;
                }
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

            'debate: loop {
                let (agent_idx, agent_config, team_name);
                {
                    let mut state = debate_state.lock().unwrap();

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
                        let Some(mut state) = wait_for_running(&debate_state) else {
                            break;
                        };
                        state.status = DebateStatus::Error(format!(
                            "no provider configured for agent '{}'",
                            agent_config.name
                        ));
                        emit_status(&handle, &state);
                        break;
                    }
                };

                let response = 'call: {
                    let mut last_err = None;
                    for attempt in 0..4u32 {
                        {
                            let Some(_state) = wait_for_running(&debate_state) else {
                                break 'debate;
                            };
                            let event = DebateThinkingEvent {
                                team: team_name.clone(),
                                agent: agent_config.name.clone(),
                            };
                            let _ = handle.emit("debate-message-reset", &event);
                            let _ = handle.emit("debate-thinking", &event);
                        }
                        let handle_ref = &handle;
                        let agent_name = agent_config.name.clone();
                        let team_ref = team_name.clone();
                        let mut on_chunk = |chunk: &str| {
                            let state = debate_state.lock().unwrap();
                            if state.status != DebateStatus::Running {
                                return;
                            }
                            let _ = handle_ref.emit(
                                "debate-message-chunk",
                                DebateChunkEvent {
                                    team: team_ref.clone(),
                                    agent: agent_name.clone(),
                                    chunk: chunk.to_string(),
                                },
                            );
                        };
                        match provider.chat_streaming(&context, &agent_config.model, &mut on_chunk)
                        {
                            Ok(text) => break 'call text,
                            Err(e) => {
                                let delay = retry_delay(&e, attempt);
                                let retry = should_retry(&e, attempt);
                                last_err = Some(e);
                                if retry {
                                    let deadline = std::time::Instant::now()
                                        + std::time::Duration::from_secs(delay);
                                    while std::time::Instant::now() < deadline {
                                        if wait_for_running(&debate_state).is_none() {
                                            break 'debate;
                                        }
                                        std::thread::sleep(std::time::Duration::from_millis(100));
                                    }
                                } else {
                                    break;
                                }
                            }
                        }
                    }
                    let Some(mut state) = wait_for_running(&debate_state) else {
                        break 'debate;
                    };
                    state.status = DebateStatus::Error(format!(
                        "agent '{}' failed: {}",
                        agent_config.name,
                        last_err.unwrap()
                    ));
                    emit_status(&handle, &state);
                    break 'debate;
                };

                // Publish only while running. Holding this lock keeps publication
                // ordered before any accepted pause/stop command.
                let Some(mut state) = wait_for_running(&debate_state) else {
                    break;
                };
                let (next_idx, new_round) = next_turn(agent_idx, state.config.agents.len());
                let msg = make_message(
                    &agent_config,
                    &response,
                    &state.config,
                    agent_idx,
                    state.current_round,
                );

                // Pre-insert hash so the file-watcher path won't emit a duplicate
                // new-message event for this streamed message.
                let message_hash = {
                    use std::hash::{DefaultHasher, Hash, Hasher};
                    let mut h = DefaultHasher::new();
                    msg.team.hash(&mut h);
                    msg.from.hash(&mut h);
                    msg.to.hash(&mut h);
                    msg.content.hash(&mut h);
                    h.finish()
                };
                let hash_was_new = seen_hashes.lock().unwrap().insert(message_hash);

                // A completed event means the message is durably recorded. Keep
                // the watcher dedup marker in place before the atomic file write.
                if let Err(error) = persist_message(&msg) {
                    if hash_was_new {
                        seen_hashes.lock().unwrap().remove(&message_hash);
                    }
                    state.status = DebateStatus::Error(error);
                    emit_status(&handle, &state);
                    break;
                }

                // Emit complete event so frontend can finalise the streaming bubble
                let _ = handle.emit(
                    "debate-message-complete",
                    DebateMessageCompleteEvent {
                        team: msg.team.clone(),
                        agent: msg.from.clone(),
                        from: msg.from.clone(),
                        to: msg.to.clone(),
                        content: msg.content.clone(),
                        timestamp: msg.timestamp,
                        role: msg.role.clone(),
                    },
                );

                {
                    state.messages.push(msg);
                    state.current_agent_idx = next_idx;

                    if new_round {
                        state.current_round += 1;

                        if let Some(topic) = advance_topic(
                            &state.config,
                            state.current_round,
                            state.current_topic_idx,
                        ) {
                            let topic_msg = DebateMessage {
                                from: "system".to_string(),
                                to: "all".to_string(),
                                content: format!("moving to next topic: {topic}"),
                                timestamp: now_ms(),
                                team: team_name.clone(),
                                role: "system".to_string(),
                            };
                            if let Err(error) = persist_message(&topic_msg) {
                                state.status = DebateStatus::Error(error);
                                emit_status(&handle, &state);
                                break;
                            }
                            state.current_topic_idx += 1;
                            state.messages.push(topic_msg);
                        }
                    }

                    emit_status(&handle, &state);
                }
                drop(state);

                // Delay between turns
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        })
        .map_err(|error| {
            let message = format!("Could not start the debate worker: {error}");
            failure_state.lock().unwrap().status = DebateStatus::Error(message.clone());
            message
        })?;
    Ok(())
}

fn wait_for_running(state: &Mutex<DebateState>) -> Option<std::sync::MutexGuard<'_, DebateState>> {
    loop {
        let debate = state.lock().unwrap();
        match debate.status {
            DebateStatus::Running => return Some(debate),
            DebateStatus::Paused => {
                drop(debate);
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            _ => return None,
        }
    }
}

// ---------------------------------------------------------------------------
// Shared pure functions (used by orchestrator, cli, and tui)
// ---------------------------------------------------------------------------

/// Compute the next agent index and whether a new round starts.
pub fn next_turn(current_idx: usize, agent_count: usize) -> (usize, bool) {
    let next = (current_idx + 1) % agent_count;
    (next, next == 0)
}

/// Construct a DebateMessage from the current turn's output.
pub fn make_message(
    agent: &AgentConfig,
    response: &str,
    config: &DebateConfig,
    current_idx: usize,
    _current_round: u32,
) -> DebateMessage {
    let agent_count = config.agents.len();
    let next_idx = (current_idx + 1) % agent_count;
    let to_name = if config.visibility == "directed" {
        config.agents[next_idx].name.clone()
    } else {
        "all".to_string()
    };
    DebateMessage {
        from: agent.name.clone(),
        to: to_name,
        content: response.to_string(),
        timestamp: now_ms(),
        team: config.team_name.clone(),
        role: agent.role.clone(),
    }
}

/// Check if the topic should advance. Returns the new topic string if so.
pub fn advance_topic(
    config: &DebateConfig,
    current_round: u32,
    current_topic_idx: usize,
) -> Option<String> {
    // Called after incrementing the round: round four follows three completed rounds.
    if config.termination != "topic" || current_round <= 1 || !(current_round - 1).is_multiple_of(3)
    {
        return None;
    }
    let next_idx = current_topic_idx + 1;
    config.topics.get(next_idx).cloned()
}

/// Authentication failures cannot recover by retrying the same credentials.
pub fn should_retry(error: &crate::provider::ProviderError, attempt: u32) -> bool {
    attempt < 3 && !matches!(error, crate::provider::ProviderError::Auth(_))
}

/// Compute retry delay in seconds based on error type and attempt number.
pub fn retry_delay(error: &crate::provider::ProviderError, attempt: u32) -> u64 {
    match error {
        crate::provider::ProviderError::RateLimit(s) => s.parse::<u64>().unwrap_or(60).max(60),
        _ => 2u64.pow(attempt),
    }
}

pub fn emit_status(handle: &AppHandle, state: &DebateState) {
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config(num_agents: usize) -> DebateConfig {
        let agents: Vec<AgentConfig> = (0..num_agents)
            .map(|i| AgentConfig {
                name: format!("agent-{i}"),
                provider: "test".to_string(),
                model: "test-model".to_string(),
                system_prompt: String::new(),
                role: "debater".to_string(),
            })
            .collect();
        DebateConfig {
            team_name: "test-debate".to_string(),
            agents,
            topics: vec!["topic 1".to_string(), "topic 2".to_string()],
            visibility: "group".to_string(),
            termination: "fixed".to_string(),
            max_rounds: 5,
            convergence_threshold: 2,
        }
    }

    #[test]
    fn next_turn_wraps_around() {
        let (next_idx, new_round) = next_turn(1, 3);
        assert_eq!(next_idx, 2);
        assert!(!new_round);
    }

    #[test]
    fn worker_rejects_duplicate_start_and_restart_even_after_stop() {
        let config = test_config(2);
        let message = make_message(&config.agents[0], "preserve this", &config, 0, 1);
        let state = Arc::new(Mutex::new(DebateState::new(config)));
        let worker = DebateWorkerGuard::claim(&state, false).unwrap();
        state.lock().unwrap().messages.push(message);
        for status in [
            DebateStatus::Running,
            DebateStatus::Paused,
            DebateStatus::Stopped,
        ] {
            state.lock().unwrap().status = status.clone();
            assert!(DebateWorkerGuard::claim(&state, false).is_err());
            assert!(DebateWorkerGuard::claim(&state, true).is_err());
            let debate = state.lock().unwrap();
            assert_eq!(debate.messages.len(), 1);
            assert_eq!(debate.status, status);
            assert!(debate.worker_active);
        }
        drop(worker);
        assert!(!state.lock().unwrap().worker_active);
        let _restarted = DebateWorkerGuard::claim(&state, true).unwrap();
        let debate = state.lock().unwrap();
        assert!(debate.messages.is_empty());
        assert_eq!(debate.current_round, 1);
        assert_eq!(debate.current_agent_idx, 0);
        assert_eq!(debate.current_topic_idx, 0);
        assert_eq!(debate.status, DebateStatus::Running);
    }

    #[test]
    fn worker_rejects_empty_agents_without_claiming_or_resetting() {
        let state = Arc::new(Mutex::new(DebateState::new(test_config(0))));
        state.lock().unwrap().current_round = 4;
        assert!(DebateWorkerGuard::claim(&state, true).is_err());
        let debate = state.lock().unwrap();
        assert!(!debate.worker_active);
        assert_eq!(debate.current_round, 4);
        assert_eq!(debate.status, DebateStatus::Stopped);
    }

    #[test]
    fn debate_config_requires_names_models_and_topic_mode_topics() {
        let valid = test_config(2);
        assert!(validate_debate_config(&valid).is_ok());
        let mut invalid = valid.clone();
        invalid.agents[0].name = " ".into();
        assert!(validate_debate_config(&invalid).is_err());
        invalid = valid.clone();
        invalid.agents[1].name = invalid.agents[0].name.clone();
        assert!(validate_debate_config(&invalid).is_err());
        invalid = valid.clone();
        invalid.agents[0].provider.clear();
        assert!(validate_debate_config(&invalid).is_err());
        invalid = valid.clone();
        invalid.agents[0].model = " ".into();
        assert!(validate_debate_config(&invalid).is_err());
        invalid = valid;
        invalid.termination = "topic".into();
        invalid.topics = vec![" ".into()];
        assert!(validate_debate_config(&invalid).is_err());
        invalid.topics.clear();
        assert!(validate_debate_config(&invalid).is_err());
    }

    #[test]
    fn stopped_worker_cannot_publish_or_retry() {
        let state = Mutex::new(DebateState::new(test_config(2)));
        for status in [
            DebateStatus::Stopped,
            DebateStatus::Converged,
            DebateStatus::Error("failed".into()),
        ] {
            state.lock().unwrap().status = status;
            assert!(wait_for_running(&state).is_none());
        }
        state.lock().unwrap().status = DebateStatus::Running;
        assert!(wait_for_running(&state).is_some());
    }

    #[test]
    fn next_turn_starts_new_round() {
        let (next_idx, new_round) = next_turn(2, 3);
        assert_eq!(next_idx, 0);
        assert!(new_round);
    }

    #[test]
    fn make_message_group_visibility() {
        let config = test_config(3);
        let agent = &config.agents[0];
        let msg = make_message(agent, "hello world", &config, 0, 1);
        assert_eq!(msg.from, "agent-0");
        assert_eq!(msg.to, "all");
        assert_eq!(msg.content, "hello world");
    }

    #[test]
    fn make_message_directed_visibility() {
        let mut config = test_config(3);
        config.visibility = "directed".to_string();
        let agent = &config.agents[0];
        let msg = make_message(agent, "hello", &config, 0, 1);
        assert_eq!(msg.to, "agent-1");
    }

    #[test]
    fn advance_topic_not_time() {
        let mut config = test_config(2);
        config.termination = "topic".to_string();
        assert_eq!(advance_topic(&config, 2, 0), None);
    }

    #[test]
    fn advance_topic_advances() {
        let mut config = test_config(2);
        config.termination = "topic".to_string();
        assert_eq!(advance_topic(&config, 4, 0), Some("topic 2".to_string()));
    }

    #[test]
    fn advance_topic_past_end() {
        let mut config = test_config(2);
        config.termination = "topic".to_string();
        assert_eq!(advance_topic(&config, 7, 1), None);
    }

    #[test]
    fn retry_delay_rate_limit() {
        use crate::provider::ProviderError;
        let delay = retry_delay(&ProviderError::RateLimit("90".to_string()), 0);
        assert_eq!(delay, 90);
    }

    #[test]
    fn retry_delay_rate_limit_minimum_60() {
        use crate::provider::ProviderError;
        let delay = retry_delay(&ProviderError::RateLimit("5".to_string()), 0);
        assert_eq!(delay, 60);
    }

    #[test]
    fn retry_delay_other_error_exponential() {
        use crate::provider::ProviderError;
        let delay = retry_delay(&ProviderError::Network("timeout".to_string()), 2);
        assert_eq!(delay, 4);
    }

    #[test]
    fn should_run_fixed_final_round() {
        let config = test_config(2);
        let mut state = DebateState::new(config);
        state.current_round = 5;
        assert!(!should_stop(&state));
    }

    #[test]
    fn should_stop_fixed_below_max() {
        let config = test_config(2);
        let mut state = DebateState::new(config);
        state.current_round = 3;
        assert!(!should_stop(&state));
    }

    #[test]
    fn convergence_requires_each_agent_for_every_configured_round() {
        let mut config = test_config(2);
        config.termination = "convergence".into();
        config.convergence_threshold = 2;
        let mut state = DebateState::new(config.clone());
        for round in 0..2 {
            for (index, agent) in config.agents.iter().enumerate() {
                state
                    .messages
                    .push(make_message(agent, "I agree.", &config, index, round));
            }
            assert_eq!(should_stop(&state), round == 1);
        }
        state.current_agent_idx = 1;
        assert!(
            !should_stop(&state),
            "wait for the rest of the current round"
        );
        state.current_agent_idx = 0;
        state.messages.last_mut().unwrap().content = "I reject this proposal.".into();
        assert!(
            !should_stop(&state),
            "one speaker cannot supply another's agreement"
        );
        state.messages.last_mut().unwrap().content = "I agree.".into();
        state.config.convergence_threshold = 3;
        assert!(
            !should_stop(&state),
            "the configured threshold must be honored"
        );
    }

    #[test]
    fn explicit_dissent_and_substrings_are_not_agreement() {
        for text in [
            "We disagreed.",
            "I agree on cost, but I reject this proposal.",
            "Not agreed.",
            "I don't agree.",
        ] {
            assert!(
                !expresses_agreement(text),
                "mistook dissent for agreement: {text}"
            );
        }
        for text in ["I agree.", "Agreed!", "You're right, I concede."] {
            assert!(expresses_agreement(text));
        }
    }

    #[test]
    fn retries_exclude_authentication_errors_and_exhausted_attempts() {
        use crate::provider::ProviderError;
        assert!(!should_retry(&ProviderError::Auth("invalid key".into()), 0));
        assert!(should_retry(
            &ProviderError::Network("disconnected".into()),
            0
        ));
        assert!(!should_retry(
            &ProviderError::Network("disconnected".into()),
            3
        ));
    }

    // Exercise the shared turn sequencing used by GUI, CLI, and TUI without
    // contacting a provider. The bound also catches a debate that never stops.
    fn simulate_turns(config: DebateConfig) -> Vec<(usize, usize)> {
        let mut state = DebateState::new(config);
        state.current_round = 1;
        let mut turns = Vec::new();
        while !should_stop(&state) {
            assert!(turns.len() < 100, "debate failed to terminate");
            turns.push((state.current_topic_idx, state.current_agent_idx));
            let (next_idx, new_round) =
                next_turn(state.current_agent_idx, state.config.agents.len());
            state.current_agent_idx = next_idx;
            if new_round {
                state.current_round += 1;
                if advance_topic(&state.config, state.current_round, state.current_topic_idx)
                    .is_some()
                {
                    state.current_topic_idx += 1;
                }
            }
        }
        turns
    }

    #[test]
    fn fixed_debate_runs_every_agent_for_exactly_the_requested_rounds() {
        for rounds in [1, 3] {
            let mut config = test_config(2);
            config.max_rounds = rounds;
            let turns = simulate_turns(config);
            let expected: Vec<_> = (0..rounds).flat_map(|_| [(0, 0), (0, 1)]).collect();
            assert_eq!(turns, expected);
        }
    }

    #[test]
    fn topic_debate_runs_three_full_rounds_per_topic_then_stops() {
        for topic_count in [1, 2] {
            let mut config = test_config(2);
            config.termination = "topic".to_string();
            config.topics.truncate(topic_count);
            let turns = simulate_turns(config);
            let expected: Vec<_> = (0..topic_count)
                .flat_map(|topic| (0..3).flat_map(move |_| [(topic, 0), (topic, 1)]))
                .collect();
            assert_eq!(turns, expected);
        }
    }

    #[test]
    fn normalize_context_merges_consecutive_roles() {
        let context = vec![
            ChatMessage {
                role: "system".to_string(),
                content: "sys".to_string(),
            },
            ChatMessage {
                role: "user".to_string(),
                content: "a".to_string(),
            },
            ChatMessage {
                role: "user".to_string(),
                content: "b".to_string(),
            },
        ];
        let result = normalize_context(context);
        assert_eq!(result.len(), 2);
        assert!(result[1].content.contains("a"));
        assert!(result[1].content.contains("b"));
    }

    #[test]
    fn normalize_context_ensures_user_first() {
        let context = vec![ChatMessage {
            role: "assistant".to_string(),
            content: "hello".to_string(),
        }];
        let result = normalize_context(context);
        assert_eq!(result[0].role, "user");
    }

    #[test]
    fn build_context_debater_gets_profile() {
        let config = test_config(2);
        let mut state = DebateState::new(config);
        state.current_round = 1;
        let mut agent = state.config.agents[0].clone();
        agent.model = "claude-opus-4-6".to_string();
        agent.provider = "anthropic".to_string();
        let context = build_context(&state, &agent);
        let system = &context[0].content;
        assert!(system.contains("model identity") || system.contains("Claude Opus"));
    }
}

#[cfg(test)]
mod disk_persistence_tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "agora-persistence-audit-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn message(text: &str) -> DebateMessage {
        DebateMessage {
            from: "agent".into(),
            to: "all".into(),
            content: text.into(),
            timestamp: 1,
            team: "design review".into(),
            role: "judge".into(),
        }
    }

    #[test]
    fn persistence_appends_without_losing_prior_messages_or_role() {
        let fixture = Fixture::new();
        persist_message_in(&fixture.0, &message("first")).unwrap();
        persist_message_in(&fixture.0, &message("second")).unwrap();
        let values: Vec<serde_json::Value> =
            serde_json::from_slice(&std::fs::read(fixture.0.join("all.json")).unwrap()).unwrap();
        assert_eq!(values.len(), 2);
        assert_eq!(values[0]["text"], "first");
        assert_eq!(values[1]["text"], "second");
        assert_eq!(values[1]["role"], "judge");
    }

    #[test]
    fn initialization_reports_config_write_failure_and_keeps_existing_inboxes() {
        let fixture = Fixture::new();
        let inboxes = fixture.0.join("inboxes");
        persist_message_in(&inboxes, &message("keep")).unwrap();
        let prior = std::fs::read(inboxes.join("all.json")).unwrap();
        std::fs::create_dir(fixture.0.join("config.json")).unwrap();
        let config = DebateConfig {
            team_name: "design review".into(),
            agents: vec![],
            topics: vec![],
            visibility: "group".into(),
            termination: "fixed".into(),
            max_rounds: 1,
            convergence_threshold: 2,
        };
        assert!(init_team_at(&fixture.0, &config).is_err());
        assert_eq!(std::fs::read(inboxes.join("all.json")).unwrap(), prior);
        assert!(fixture.0.join("config.json").is_dir());
    }

    #[test]
    fn malformed_history_and_write_failures_are_reported_without_overwrite() {
        let fixture = Fixture::new();
        let path = fixture.0.join("all.json");
        let original = b"[{recoverable incomplete history";
        std::fs::write(&path, original).unwrap();
        assert!(persist_message_in(&fixture.0, &message("new")).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        let blocked = fixture.0.join("blocked");
        std::fs::write(&blocked, b"keep").unwrap();
        assert!(persist_message_in(&blocked, &message("new")).is_err());
        assert_eq!(std::fs::read(&blocked).unwrap(), b"keep");
    }
}
