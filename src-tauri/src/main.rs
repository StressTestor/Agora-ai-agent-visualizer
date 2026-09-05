#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod cli;
mod config;
mod model_profiles;
mod orchestrator;
mod presets;
mod provider;
mod tui;

use chrono::DateTime;
use config::AppConfig;
use notify::{Event, EventKind, RecursiveMode, Watcher};
use orchestrator::{DebateConfig, DebateState};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager, State};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Message {
    from: String,
    to: String,
    content: String,
    timestamp: u64,
    team: String,
    #[serde(default)]
    role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TaskUpdate {
    id: String,
    subject: String,
    status: String,
    team: String,
}

struct AppState {
    seen_hashes: HashSet<u64>,
    messages: Vec<Message>,
    known_teams: HashSet<String>,
    config: AppConfig,
    debates: HashMap<String, Arc<Mutex<DebateState>>>,
    inbox_scan_cache: InboxScanCache,
}

#[derive(Debug, PartialEq, Eq)]
struct InboxFingerprint {
    len: u64,
    modified: SystemTime,
    created: Option<SystemTime>,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}

impl InboxFingerprint {
    fn read(path: &Path) -> Option<Self> {
        let metadata = std::fs::metadata(path).ok()?;
        if !metadata.is_file() {
            return None;
        }
        Some(Self {
            len: metadata.len(),
            modified: metadata.modified().ok()?,
            created: metadata.created().ok(),
            #[cfg(unix)]
            identity: {
                use std::os::unix::fs::MetadataExt;
                (
                    metadata.dev(),
                    metadata.ino(),
                    metadata.ctime(),
                    metadata.ctime_nsec(),
                )
            },
        })
    }
}

#[derive(Default)]
struct InboxScanCache {
    files: HashMap<PathBuf, InboxFingerprint>,
    #[cfg(test)]
    parse_attempts: usize,
}

impl InboxScanCache {
    fn invalidate(&mut self, changed_paths: &[PathBuf]) {
        self.files.retain(|path, _| {
            !changed_paths
                .iter()
                .any(|changed| path.starts_with(changed))
        });
    }
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| String::from("/tmp")))
}

fn claude_dir() -> PathBuf {
    home().join(".claude")
}

fn teams_dir() -> PathBuf {
    claude_dir().join("teams")
}

fn validate_team_name(team: &str) -> Result<(), String> {
    let mut parts = Path::new(team).components();
    if team.trim().is_empty()
        || team.contains(['/', '\\', '\0'])
        || !matches!(parts.next(), Some(std::path::Component::Normal(_)))
        || parts.next().is_some()
    {
        return Err("Use a nonempty team name without directory separators.".to_string());
    }
    Ok(())
}

fn team_path_from(root: &Path, team: &str) -> Result<PathBuf, String> {
    validate_team_name(team)?;
    let path = root.join(team);
    if path
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err("Team folders must be ordinary directories.".to_string());
    }
    Ok(path)
}

fn tasks_dir() -> PathBuf {
    claude_dir().join("tasks")
}

// ---------------------------------------------------------------------------
// Hashing / dedup
// ---------------------------------------------------------------------------

fn hash_message(team: &str, from: &str, to: &str, content: &str) -> u64 {
    let mut h = DefaultHasher::new();
    team.hash(&mut h);
    from.hash(&mut h);
    to.hash(&mut h);
    content.hash(&mut h);
    h.finish()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ---------------------------------------------------------------------------
// Inbox parsing — flexible multi-strategy parser
// ---------------------------------------------------------------------------

/// Non-message types that should be filtered out of the chat view.
fn is_system_type(val: &serde_json::Value) -> bool {
    matches!(
        val.get("type").and_then(|v| v.as_str()),
        Some("idle_notification" | "heartbeat" | "ping" | "status_update" | "shutdown_request")
    )
}

/// Try to parse a JSON timestamp value into epoch milliseconds.
fn parse_json_timestamp(val: &serde_json::Value) -> Option<u64> {
    if let Some(ts) = val.get("timestamp") {
        // Numeric epoch ms
        if let Some(n) = ts.as_u64() {
            return Some(n);
        }
        // ISO 8601 string
        if let Some(s) = ts.as_str() {
            if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
                return Some(dt.timestamp_millis() as u64);
            }
            // Try as numeric string
            if let Ok(n) = s.parse::<u64>() {
                return Some(n);
            }
        }
    }
    None
}

/// Try to extract (from, to, content, timestamp) from a single JSON value.
/// Timestamp is 0 if not found in the JSON (caller provides fallback).
fn extract_msg(val: &serde_json::Value, default_to: &str) -> Option<(String, String, String, u64)> {
    // Skip non-message system types
    if is_system_type(val) {
        return None;
    }

    let from = ["from", "sender"]
        .iter()
        .find_map(|k| val.get(k)?.as_str())
        .map(String::from)?;

    let content = ["text", "content", "message", "body"]
        .iter()
        .find_map(|k| val.get(k)?.as_str())
        .map(String::from)?;

    let to = ["to", "recipient"]
        .iter()
        .find_map(|k| val.get(k)?.as_str())
        .map(String::from)
        .unwrap_or_else(|| default_to.to_string());

    let ts = parse_json_timestamp(val).unwrap_or(0);

    Some((from, to, content, ts))
}

/// Read file mtime as epoch milliseconds, or 0.
fn file_mtime_ms(path: &Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Parse an inbox JSON file; "to" is inferred from the filename.
/// Returns (from, to, content, timestamp) tuples with resolved timestamps.
fn parse_inbox(path: &Path, _team: &str) -> std::io::Result<Vec<(String, String, String, u64)>> {
    let to = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();

    let raw = std::fs::read_to_string(path)?;
    let val: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;

    let mtime = file_mtime_ms(path);
    let fallback = if mtime > 0 { mtime } else { now_ms() };

    // Resolve timestamps: JSON timestamp > file mtime > now
    let resolve = |msgs: Vec<(String, String, String, u64)>| -> Vec<(String, String, String, u64)> {
        msgs.into_iter()
            .map(|(f, t, c, ts)| (f, t, c, if ts > 0 { ts } else { fallback }))
            .collect()
    };

    // Strategy 1: top-level array
    if let Some(arr) = val.as_array() {
        let msgs: Vec<_> = arr.iter().filter_map(|v| extract_msg(v, &to)).collect();
        return Ok(resolve(msgs));
    }

    // Strategy 2: { messages: [...] }
    if let Some(arr) = val.get("messages").and_then(|v| v.as_array()) {
        let msgs: Vec<_> = arr.iter().filter_map(|v| extract_msg(v, &to)).collect();
        return Ok(resolve(msgs));
    }

    // Strategy 3: { inbox: [...] }
    if let Some(arr) = val.get("inbox").and_then(|v| v.as_array()) {
        let msgs: Vec<_> = arr.iter().filter_map(|v| extract_msg(v, &to)).collect();
        return Ok(resolve(msgs));
    }

    // Strategy 4: single message object
    if let Some(msg) = extract_msg(&val, &to) {
        return Ok(resolve(vec![msg]));
    }

    // Recognized envelopes and protocol records are not sender-to-text maps.
    if ["messages", "inbox", "from", "sender", "type"]
        .iter()
        .any(|key| val.get(key).is_some())
    {
        return Ok(vec![]);
    }

    // Strategy 5: { sender_name: "text" } key-value map
    if let Some(obj) = val.as_object() {
        return Ok(obj
            .iter()
            .filter_map(|(k, v)| Some((k.clone(), to.clone(), v.as_str()?.to_string(), fallback)))
            .collect());
    }

    Ok(vec![])
}

// ---------------------------------------------------------------------------
// Team / task scanning
// ---------------------------------------------------------------------------

fn list_teams(dir: &Path) -> Vec<String> {
    let mut teams = vec![];
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                if let Some(name) = e.file_name().to_str() {
                    teams.push(name.to_string());
                }
            }
        }
    }
    teams.sort();
    teams
}

/// Scan all inboxes and return newly-seen messages, mutating seen_hashes.
/// `shared_hashes` is pre-populated by the orchestrator for streaming messages;
/// if a hash appears there we mark it seen but do NOT emit a `new-message` event
/// (the `debate-message-complete` event already did that).
fn scan_inboxes(
    dir: &Path,
    state: &mut AppState,
    shared_hashes: &Arc<Mutex<HashSet<u64>>>,
) -> Vec<Message> {
    let mut new_msgs = vec![];
    let mut present_paths = HashSet::new();

    for team in list_teams(dir) {
        let inbox_dir = dir.join(&team).join("inboxes");
        let Ok(entries) = std::fs::read_dir(&inbox_dir) else {
            continue;
        };
        for e in entries.flatten() {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            present_paths.insert(path.clone());
            let fingerprint = InboxFingerprint::read(&path);
            if fingerprint
                .as_ref()
                .is_some_and(|stamp| state.inbox_scan_cache.files.get(&path) == Some(stamp))
            {
                continue;
            }
            // Failed reads (including partially written JSON) remain retryable.
            state.inbox_scan_cache.files.remove(&path);
            #[cfg(test)]
            {
                state.inbox_scan_cache.parse_attempts += 1;
            }
            let Ok(parsed) = parse_inbox(&path, &team) else {
                continue;
            };
            // A writer may have changed the file while it was being read.
            // Cache only a stable snapshot; the next scan retries otherwise.
            if let Some(fingerprint) = fingerprint {
                if Some(&fingerprint) == InboxFingerprint::read(&path).as_ref() {
                    state
                        .inbox_scan_cache
                        .files
                        .insert(path.clone(), fingerprint);
                }
            }
            for (from, to, content, ts) in parsed {
                let hash = hash_message(&team, &from, &to, &content);
                let streamed = shared_hashes.lock().unwrap().contains(&hash);
                if state.seen_hashes.insert(hash) {
                    if streamed {
                        // Orchestrator already emitted debate-message-complete; skip new-message
                        continue;
                    }
                    let msg = Message {
                        from,
                        to,
                        content,
                        timestamp: ts,
                        team: team.clone(),
                        role: String::new(),
                    };
                    state.messages.push(msg.clone());
                    new_msgs.push(msg);
                }
            }
        }
    }
    state
        .inbox_scan_cache
        .files
        .retain(|path, _| present_paths.contains(path));
    new_msgs
}

/// Parse a task JSON file into a TaskUpdate.
fn parse_task(path: &Path) -> Option<TaskUpdate> {
    let raw = std::fs::read_to_string(path).ok()?;
    let val: serde_json::Value = serde_json::from_str(&raw).ok()?;

    let subject = val["subject"].as_str()?;
    let status = val["status"].as_str().unwrap_or("unknown");
    let id = val["id"].as_str().unwrap_or("?");
    let team = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();

    Some(TaskUpdate {
        id: id.to_string(),
        subject: subject.to_string(),
        status: status.to_string(),
        team,
    })
}

// ---------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------

#[tauri::command]
fn get_teams() -> Vec<String> {
    list_teams(&teams_dir())
}

#[derive(Debug, Clone, Serialize)]
struct TeamMemberConfig {
    name: String,
    model: String,
}

#[derive(Debug, Clone, Serialize)]
struct TeamConfig {
    name: String,
    description: String,
    members: Vec<TeamMemberConfig>,
}

#[tauri::command]
fn list_team_configs() -> Vec<TeamConfig> {
    let tdir = teams_dir();
    let mut result = vec![];
    for team in list_teams(&tdir) {
        let path = tdir.join(&team).join("config.json");
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) else {
            continue;
        };
        let name = val["name"].as_str().unwrap_or(&team).to_string();
        let description = val["description"].as_str().unwrap_or("").to_string();
        let members = val["members"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| {
                        let mname = m["name"].as_str()?.to_string();
                        let model = m["model"].as_str().unwrap_or("").to_string();
                        Some(TeamMemberConfig { name: mname, model })
                    })
                    .collect()
            })
            .unwrap_or_default();
        result.push(TeamConfig {
            name,
            description,
            members,
        });
    }
    result
}

fn delete_team_at(
    root: &Path,
    st: &mut AppState,
    streamed: &Mutex<HashSet<u64>>,
    team: &str,
) -> Result<(), String> {
    let path = team_path_from(root, team)?;
    if st
        .debates
        .get(team)
        .is_some_and(|debate| debate.lock().unwrap().worker_active)
    {
        return Err(
            "Stop the debate and wait for its current response to finish before deleting the team."
                .to_string(),
        );
    }
    if !path.exists() {
        return Err(format!("team '{team}' not found"));
    }
    std::fs::remove_dir_all(&path).map_err(|e| format!("failed to delete team '{team}': {e}"))?;
    let mut hashes: HashSet<_> = st
        .messages
        .iter()
        .filter(|message| message.team == team)
        .map(|message| hash_message(team, &message.from, &message.to, &message.content))
        .collect();
    if let Some(debate) = st.debates.remove(team) {
        hashes.extend(
            debate
                .lock()
                .unwrap()
                .messages
                .iter()
                .map(|message| hash_message(team, &message.from, &message.to, &message.content)),
        );
    }
    st.messages.retain(|message| message.team != team);
    st.known_teams.remove(team);
    st.inbox_scan_cache.invalidate(&[path]);
    st.seen_hashes.retain(|hash| !hashes.contains(hash));
    streamed
        .lock()
        .unwrap()
        .retain(|hash| !hashes.contains(hash));
    Ok(())
}

#[tauri::command]
fn delete_team(
    state: State<'_, Arc<Mutex<AppState>>>,
    seen: State<'_, Arc<Mutex<HashSet<u64>>>>,
    team: String,
) -> Result<(), String> {
    delete_team_at(
        &teams_dir(),
        &mut state.lock().unwrap(),
        seen.inner(),
        &team,
    )
}

fn message_snapshot(st: &AppState) -> Vec<Message> {
    // Keep independently watched messages, including messages for debate teams.
    // Live state overrides metadata only for the exact same message instance.
    let mut messages = Vec::new();
    let mut positions = HashMap::new();
    let mut merge = |message: Message| {
        let key = (
            message.team.clone(),
            message.from.clone(),
            message.to.clone(),
            message.content.clone(),
            message.timestamp,
        );
        if let Some(&index) = positions.get(&key) {
            messages[index] = message;
        } else {
            positions.insert(key, messages.len());
            messages.push(message);
        }
    };
    for message in &st.messages {
        merge(message.clone());
    }
    for debate in st.debates.values() {
        for message in &debate.lock().unwrap().messages {
            merge(Message {
                from: message.from.clone(),
                to: message.to.clone(),
                content: message.content.clone(),
                timestamp: message.timestamp,
                team: message.team.clone(),
                role: message.role.clone(),
            });
        }
    }
    messages.sort_by_key(|message| message.timestamp);
    messages
}

#[tauri::command]
fn get_messages(state: State<'_, Arc<Mutex<AppState>>>) -> Vec<Message> {
    message_snapshot(&state.lock().unwrap())
}

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
async fn list_models(
    state: State<'_, Arc<Mutex<AppState>>>,
    provider_name: String,
) -> Result<Vec<provider::ModelInfo>, String> {
    const NO_KEY_PROVIDERS: &[&str] = &["claude-code"];
    let api_key = if NO_KEY_PROVIDERS.contains(&provider_name.as_str()) {
        String::new()
    } else {
        let st = state.lock().unwrap();
        st.config
            .api_key(&provider_name)
            .ok_or_else(|| format!("no API key configured for '{provider_name}'"))?
    };
    discover_models(provider_name, api_key).await
}

async fn discover_models(
    provider_name: String,
    api_key: String,
) -> Result<Vec<provider::ModelInfo>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let p = provider::build_provider(&provider_name, &api_key)
            .ok_or_else(|| format!("unknown provider '{provider_name}'"))?;
        p.list_models().map_err(|e| e.to_string())
    })
    .await
    .map_err(|error| format!("model discovery task failed: {error}"))?
}

#[tauri::command]
fn list_role_presets() -> Vec<presets::RolePreset> {
    presets::role_presets()
}

#[tauri::command]
fn list_debate_presets() -> Vec<presets::DebatePreset> {
    presets::debate_presets()
}

fn archive_team_inboxes(root: &Path, config: &DebateConfig) -> Result<(), String> {
    let team_dir = team_path_from(root, &config.team_name)?;
    let inbox = team_dir.join("inboxes");
    let entries = match std::fs::read_dir(&inbox) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("Could not inspect prior inboxes: {error}")),
    };
    let mut has_json = false;
    for entry in entries {
        let entry = entry.map_err(|error| format!("Could not inspect prior inboxes: {error}"))?;
        has_json |= entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "json");
    }
    if !has_json {
        return Ok(());
    }
    let topic = config
        .topics
        .first()
        .map(String::as_str)
        .unwrap_or("debate");
    let slug: String = topic
        .chars()
        .take(40)
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let slug = slug.trim_matches('-');
    let slug = if slug.is_empty() { "debate" } else { slug };
    let archive = team_dir.join("archive");
    std::fs::create_dir_all(&archive)
        .map_err(|error| format!("Could not create archive: {error}"))?;
    let mut destination = archive.join(slug);
    let mut suffix = 2;
    while destination.exists() {
        destination = archive.join(format!("{slug}-{suffix}"));
        suffix += 1;
    }
    // One rename avoids partially archiving a collection of inbox files.
    std::fs::rename(&inbox, &destination).map_err(|error| {
        format!("Could not archive prior inboxes; no new debate was created: {error}")
    })?;
    Ok(())
}

fn archive_and_clear_watched(
    root: &Path,
    st: &mut AppState,
    config: &DebateConfig,
) -> Result<(), String> {
    let path = team_path_from(root, &config.team_name)?;
    archive_team_inboxes(root, config)?;
    st.messages
        .retain(|message| message.team != config.team_name);
    st.inbox_scan_cache.invalidate(&[path]);
    Ok(())
}

#[tauri::command]
fn create_debate(
    state: State<'_, Arc<Mutex<AppState>>>,
    config: DebateConfig,
) -> Result<String, String> {
    orchestrator::validate_debate_config(&config)?;
    let team = config.team_name.clone();
    // Serialize replacement with start/restart before touching this team's files.
    let mut st = state.lock().unwrap();
    if st
        .debates
        .get(&team)
        .is_some_and(|debate| debate.lock().unwrap().worker_active)
    {
        return Err("This team's debate is still active. Stop it and wait for the current response to finish before replacing it.".to_string());
    }

    archive_and_clear_watched(&teams_dir(), &mut st, &config)?;

    let debate_state = Arc::new(Mutex::new(DebateState::new(config)));
    st.debates.insert(team.clone(), debate_state);
    Ok(team)
}

#[tauri::command]
fn start_debate_cmd(
    app: AppHandle,
    state: State<'_, Arc<Mutex<AppState>>>,
    seen: State<'_, Arc<Mutex<HashSet<u64>>>>,
    team: String,
) -> Result<(), String> {
    let st = state.lock().unwrap();
    let debate_state = st
        .debates
        .get(&team)
        .ok_or_else(|| format!("no debate '{team}'"))?
        .clone();
    orchestrator::start_debate(
        app,
        st.config.clone(),
        debate_state,
        seen.inner().clone(),
        false,
    )
}

#[tauri::command]
fn stop_debate(
    app: AppHandle,
    state: State<'_, Arc<Mutex<AppState>>>,
    team: String,
) -> Result<(), String> {
    let st = state.lock().unwrap();
    let ds = st
        .debates
        .get(&team)
        .ok_or_else(|| format!("no debate '{team}'"))?;
    let mut debate = ds.lock().unwrap();
    debate.status = orchestrator::DebateStatus::Stopped;
    orchestrator::emit_status(&app, &debate);
    Ok(())
}

#[tauri::command]
fn pause_debate(
    app: AppHandle,
    state: State<'_, Arc<Mutex<AppState>>>,
    team: String,
) -> Result<(), String> {
    let st = state.lock().unwrap();
    let ds = st
        .debates
        .get(&team)
        .ok_or_else(|| format!("no debate '{team}'"))?;
    let mut debate = ds.lock().unwrap();
    match debate.status {
        orchestrator::DebateStatus::Running => {
            debate.status = orchestrator::DebateStatus::Paused;
        }
        orchestrator::DebateStatus::Paused => {
            debate.status = orchestrator::DebateStatus::Running;
        }
        _ => {}
    }
    orchestrator::emit_status(&app, &debate);
    Ok(())
}

#[tauri::command]
fn restart_debate(
    app: AppHandle,
    state: State<'_, Arc<Mutex<AppState>>>,
    seen: State<'_, Arc<Mutex<HashSet<u64>>>>,
    team: String,
) -> Result<(), String> {
    let mut st = state.lock().unwrap();
    let debate_state = st
        .debates
        .get(&team)
        .ok_or_else(|| format!("no debate '{team}'"))?
        .clone();
    {
        let debate = debate_state.lock().unwrap();
        if debate.worker_active {
            return Err(
                "Stop the debate and wait for its current response to finish before restarting."
                    .to_string(),
            );
        }
        orchestrator::validate_debate_config(&debate.config)?;
        archive_and_clear_watched(&teams_dir(), &mut st, &debate.config)?;
    }
    orchestrator::start_debate(
        app,
        st.config.clone(),
        debate_state,
        seen.inner().clone(),
        true,
    )
}

#[tauri::command]
fn get_debate_status(
    state: State<'_, Arc<Mutex<AppState>>>,
    team: String,
) -> Result<orchestrator::DebateStatusEvent, String> {
    let st = state.lock().unwrap();
    let ds = st
        .debates
        .get(&team)
        .ok_or_else(|| format!("no debate '{team}'"))?;
    let debate = ds.lock().unwrap();
    let (status_str, error_msg) = match &debate.status {
        orchestrator::DebateStatus::Running => ("running", None),
        orchestrator::DebateStatus::Paused => ("paused", None),
        orchestrator::DebateStatus::Stopped => ("stopped", None),
        orchestrator::DebateStatus::Converged => ("converged", None),
        orchestrator::DebateStatus::Error(e) => ("error", Some(e.clone())),
    };
    Ok(orchestrator::DebateStatusEvent {
        team: debate.config.team_name.clone(),
        status: status_str.to_string(),
        round: debate.current_round,
        total_messages: debate.messages.len(),
        error_msg,
    })
}

#[tauri::command]
async fn enhance_topic(
    state: State<'_, Arc<Mutex<AppState>>>,
    text: String,
) -> Result<String, String> {
    // Prefer fast direct-API providers for topic refinement. CC CLI has ~15s overhead
    // from MCP server loading which makes it a bad fit for single-shot calls.
    // CC CLI is last resort — still works, just slow.
    const PRIORITY: &[(&str, &str)] = &[
        ("groq", "llama-3.3-70b-versatile"),
        ("gemini", "gemini-2.0-flash"),
        ("openai", "gpt-4o-mini"),
        ("anthropic", "claude-haiku-4-5-20251001"),
        ("openrouter", "meta-llama/llama-3.3-70b-instruct:free"),
        ("opencode", "minimax/MiniMax-M2.5"),
        ("claude-code", "haiku"),
    ];

    let config = { state.lock().unwrap().config.clone() };

    // Use user-configured provider if set, otherwise auto-select from priority list.
    let (provider_name, model): (String, String) = if !config.enhance_provider.is_empty() {
        let model = if config.enhance_model.is_empty() {
            PRIORITY
                .iter()
                .find(|(p, _)| *p == config.enhance_provider.as_str())
                .map(|(_, m)| m.to_string())
                .unwrap_or_else(|| "haiku".to_string())
        } else {
            config.enhance_model.clone()
        };
        (config.enhance_provider.clone(), model)
    } else {
        let (p, m) = PRIORITY
            .iter()
            .find(|(p, _)| *p == "claude-code" || config.api_key(p).is_some())
            .ok_or_else(|| "no providers configured — add an API key in settings".to_string())?;
        (p.to_string(), m.to_string())
    };
    let api_key = if provider_name == "claude-code" {
        String::new()
    } else {
        config.api_key(&provider_name).unwrap_or_default()
    };

    let messages = vec![
        provider::ChatMessage {
            role: "system".to_string(),
            content: "You refine debate topics. Reply with only the refined topic — no explanation, no bullet points, no surrounding quotes, no lead-in.".to_string(),
        },
        provider::ChatMessage {
            role: "user".to_string(),
            content: format!(
                "Refine this into a specific, debatable topic for a structured multi-agent debate. Make it concrete enough that agents can take clear opposing positions:\n\n{text}"
            ),
        },
    ];

    // Run on background thread to avoid blocking the main/UI thread
    tauri::async_runtime::spawn_blocking(move || {
        let provider = provider::build_provider(&provider_name, &api_key)
            .ok_or_else(|| format!("unknown provider '{provider_name}'"))?;
        provider.chat(&messages, &model).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("task join error: {e}"))?
}

#[tauri::command]
fn show_main_and_close_splash(app: AppHandle) -> Result<(), String> {
    let main = app
        .get_webview_window("main")
        .ok_or_else(|| "The main window is unavailable.".to_string())?;
    main.show()
        .map_err(|error| format!("Could not show the main window: {error}"))?;
    if let Err(error) = main.set_focus() {
        eprintln!("could not focus the main window: {error}");
    }
    if let Some(splash) = app.get_webview_window("splash") {
        splash
            .close()
            .map_err(|error| format!("Could not close the intro window: {error}"))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    // Check for CLI subcommands before launching the GUI
    let args: Vec<String> = std::env::args().collect();
    let cli_commands = ["debate", "list-presets", "list-models", "--help", "-h"];
    if args.len() > 1 && cli_commands.contains(&args[1].as_str()) {
        match cli::run_cli() {
            Ok(()) => std::process::exit(0),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
    }

    let cli_team: Option<String> = {
        let mut team = None;
        for i in 0..args.len() {
            if (args[i] == "--team" || args[i] == "-t") && i + 1 < args.len() {
                team = Some(args[i + 1].clone());
            }
        }
        team
    };

    let tdir = teams_dir();
    let initial_known: HashSet<String> = list_teams(&tdir).into_iter().collect();

    let app_config = AppConfig::load();

    let shared_hashes: Arc<Mutex<HashSet<u64>>> = Arc::new(Mutex::new(HashSet::new()));

    let shared_state = Arc::new(Mutex::new(AppState {
        seen_hashes: HashSet::new(),
        messages: vec![],
        known_teams: initial_known,
        config: app_config,
        debates: HashMap::new(),
        inbox_scan_cache: InboxScanCache::default(),
    }));

    let state_for_setup = shared_state.clone();
    let hashes_for_setup = shared_hashes.clone();

    tauri::Builder::default()
        .plugin(tauri_plugin_window_state::Builder::new().build())
        .manage(shared_state)
        .manage(shared_hashes)
        .invoke_handler(tauri::generate_handler![
            get_teams,
            delete_team,
            list_team_configs,
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
            restart_debate,
            get_debate_status,
            enhance_topic,
            show_main_and_close_splash,
        ])
        .setup(move |app| {
            let handle: AppHandle = app.handle().clone();
            let state = state_for_setup.clone();
            let initial_team = cli_team.clone();

            // Keep a usable window behind the optional intro, including when
            // restored window state or a failed splash would otherwise hide it.
            if let Some(main) = app.get_webview_window("main") {
                main.show()?;
            }
            let splash = tauri::WebviewWindowBuilder::new(
                app,
                "splash",
                tauri::WebviewUrl::App("splash.html".into()),
            )
            .title("")
            .inner_size(464.0, 688.0)
            .decorations(false)
            .resizable(false)
            .center()
            .always_on_top(true)
            .build();
            match splash {
                Ok(_) => {
                    // Native fallback also works when splash JS never loads or
                    // IPC is unavailable. Do not steal focus once it has closed.
                    let splash_handle = handle.clone();
                    if let Err(error) = std::thread::Builder::new()
                        .name("splash-timeout".into())
                        .spawn(move || {
                            std::thread::sleep(std::time::Duration::from_secs(9));
                            if splash_handle.get_webview_window("splash").is_some() {
                                if let Err(error) = show_main_and_close_splash(splash_handle) {
                                    eprintln!("intro timeout recovery failed: {error}");
                                }
                            }
                        })
                    {
                        eprintln!("could not start intro timeout: {error}");
                        if let Err(error) = show_main_and_close_splash(handle.clone()) {
                            eprintln!("intro recovery failed: {error}");
                        }
                    }
                }
                Err(error) => eprintln!(
                    "could not open intro window; continuing with the main window: {error}"
                ),
            }

            // Initial scan on the calling thread (fast)
            {
                let mut st = state.lock().unwrap();
                let new_msgs = scan_inboxes(&tdir, &mut st, &hashes_for_setup);
                drop(new_msgs); // already stored in state.messages; frontend loads via get_messages
            }

            // Background watcher thread
            let hashes_for_watcher = hashes_for_setup.clone();
            std::thread::spawn(move || {
                let (tx, rx) = std::sync::mpsc::channel::<notify::Result<Event>>();

                let mut watcher = match notify::recommended_watcher(move |res| {
                    let _ = tx.send(res);
                }) {
                    Ok(watcher) => Some(watcher),
                    Err(error) => {
                        eprintln!(
                            "could not create file watcher; continuing inbox polling: {error}"
                        );
                        None
                    }
                };

                let tdir = teams_dir();
                let tkdir = tasks_dir();

                if let Some(watcher) = &mut watcher {
                    for directory in [&tdir, &tkdir] {
                        if directory.exists() {
                            if let Err(error) = watcher.watch(directory, RecursiveMode::Recursive) {
                                eprintln!("could not watch {}: {error}", directory.display());
                            }
                        }
                    }
                }

                let poll_interval = std::time::Duration::from_secs(2);
                let mut last_poll = std::time::Instant::now();

                loop {
                    let mut inbox_dirty = false;
                    let mut changed_inbox_paths = Vec::new();
                    let mut changed_task_paths: Vec<PathBuf> = vec![];

                    // Drain watcher events (non-blocking)
                    loop {
                        match rx.try_recv() {
                            Ok(Ok(event)) => match event.kind {
                                EventKind::Modify(_)
                                | EventKind::Create(_)
                                | EventKind::Remove(_) => {
                                    for p in &event.paths {
                                        let s = p.to_string_lossy();
                                        if s.contains("inboxes") {
                                            inbox_dirty = true;
                                            changed_inbox_paths.push(p.clone());
                                        } else if s.contains("tasks") && s.ends_with(".json") {
                                            changed_task_paths.push(p.clone());
                                        } else if s.contains("teams") {
                                            // Could be a new team dir
                                            inbox_dirty = true;
                                            changed_inbox_paths.push(p.clone());
                                        }
                                    }
                                }
                                _ => {}
                            },
                            Ok(Err(e)) => eprintln!("watcher error: {e}"),
                            Err(std::sync::mpsc::TryRecvError::Empty) => break,
                            Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
                        }
                    }

                    // Periodic poll fallback
                    if last_poll.elapsed() >= poll_interval {
                        inbox_dirty = true;
                        last_poll = std::time::Instant::now();
                    }

                    if inbox_dirty {
                        let mut st = state.lock().unwrap();
                        st.inbox_scan_cache.invalidate(&changed_inbox_paths);

                        // Detect new teams
                        let current_teams: HashSet<String> =
                            list_teams(&tdir).into_iter().collect();
                        for team in &current_teams {
                            if !st.known_teams.contains(team) {
                                let _ = handle.emit("team-added", team.clone());
                            }
                        }
                        st.known_teams = current_teams;

                        // Scan inboxes for new messages
                        let new_msgs = scan_inboxes(&tdir, &mut st, &hashes_for_watcher);
                        drop(st);

                        for msg in new_msgs {
                            // Respect CLI team filter for emitted events
                            if let Some(ref filter) = initial_team {
                                if &msg.team != filter {
                                    continue;
                                }
                            }
                            let _ = handle.emit("new-message", &msg);
                        }
                    }

                    // Emit task updates
                    for path in changed_task_paths {
                        if let Some(update) = parse_task(&path) {
                            let _ = handle.emit("task-update", &update);
                        }
                    }

                    std::thread::sleep(std::time::Duration::from_millis(150));
                }
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod inbox_scan_tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Fixture {
        root: PathBuf,
        inbox: PathBuf,
        shared: Arc<Mutex<HashSet<u64>>>,
    }

    impl Fixture {
        fn new() -> Self {
            static NEXT_ID: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "agora-inbox-test-{}-{}-{}",
                std::process::id(),
                now_ms(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed),
            ));
            let inbox = root.join("synthetic/inboxes");
            std::fs::create_dir_all(&inbox).unwrap();
            Self {
                root,
                inbox,
                shared: Arc::new(Mutex::new(HashSet::new())),
            }
        }

        fn write(&self, file: &str, contents: &[&str]) -> PathBuf {
            let messages: Vec<_> = contents
                .iter()
                .map(|content| {
                    serde_json::json!({
                        "from": "agent", "to": "all", "text": content, "timestamp": 1,
                    })
                })
                .collect();
            let path = self.inbox.join(file);
            std::fs::write(&path, serde_json::to_vec(&messages).unwrap()).unwrap();
            path
        }

        fn scan(&self, state: &mut AppState) -> Vec<Message> {
            scan_inboxes(&self.root, state, &self.shared)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn empty_state() -> AppState {
        AppState {
            seen_hashes: HashSet::new(),
            messages: vec![],
            known_teams: HashSet::new(),
            config: AppConfig::default(),
            debates: HashMap::new(),
            inbox_scan_cache: InboxScanCache::default(),
        }
    }

    #[test]
    fn unchanged_inboxes_are_skipped_and_appends_keep_stream_deduplication() {
        let fixture = Fixture::new();
        let mut state = empty_state();
        fixture.write("all.json", &["first"]);
        assert_eq!(fixture.scan(&mut state).len(), 1);
        assert!(fixture.scan(&mut state).is_empty());
        assert_eq!(state.inbox_scan_cache.parse_attempts, 1);

        fixture.shared.lock().unwrap().insert(hash_message(
            "synthetic",
            "agent",
            "all",
            "streamed",
        ));
        fixture.write("all.json", &["first", "streamed", "later"]);
        let fresh = fixture.scan(&mut state);
        assert_eq!(fresh.len(), 1);
        assert_eq!(fresh[0].content, "later");
        assert_eq!(state.inbox_scan_cache.parse_attempts, 2);
        assert_eq!(state.messages.len(), 2);
        assert!(fixture.scan(&mut state).is_empty());
        assert_eq!(state.inbox_scan_cache.parse_attempts, 2);
    }

    #[test]
    fn failed_parses_retry_and_removed_inboxes_are_pruned() {
        let fixture = Fixture::new();
        let mut state = empty_state();
        let path = fixture.inbox.join("all.json");
        std::fs::write(&path, "{").unwrap();
        assert!(fixture.scan(&mut state).is_empty());
        assert!(fixture.scan(&mut state).is_empty());
        assert_eq!(state.inbox_scan_cache.parse_attempts, 2);
        assert!(state.inbox_scan_cache.files.is_empty());
        fixture.write("all.json", &["recovered"]);
        assert_eq!(fixture.scan(&mut state).len(), 1);
        std::fs::remove_file(&path).unwrap();
        fixture.scan(&mut state);
        assert!(state.inbox_scan_cache.files.is_empty());
        fixture.write("all.json", &["recreated"]);
        assert_eq!(fixture.scan(&mut state)[0].content, "recreated");
    }

    #[test]
    fn watcher_events_invalidate_only_the_changed_inbox_or_directory() {
        let fixture = Fixture::new();
        let mut state = empty_state();
        let first = fixture.write("first.json", &["first"]);
        fixture.write("second.json", &["second"]);
        fixture.scan(&mut state);
        state.inbox_scan_cache.invalidate(&[first]);
        fixture.scan(&mut state);
        assert_eq!(state.inbox_scan_cache.parse_attempts, 3);
        state
            .inbox_scan_cache
            .invalidate(std::slice::from_ref(&fixture.inbox));
        fixture.scan(&mut state);
        assert_eq!(state.inbox_scan_cache.parse_attempts, 5);
        assert_eq!(state.messages.len(), 2);
    }

    #[test]
    #[cfg(unix)]
    fn atomic_replacement_is_read_even_with_identical_size_and_mtime() {
        let fixture = Fixture::new();
        let mut state = empty_state();
        let path = fixture.write("all.json", &["before"]);
        fixture.scan(&mut state);
        let original = std::fs::metadata(&path).unwrap();
        let replacement = fixture.write("replacement.tmp", &["after!"]);
        std::fs::File::options()
            .write(true)
            .open(&replacement)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(original.modified().unwrap()))
            .unwrap();
        assert_eq!(
            std::fs::metadata(&replacement).unwrap().len(),
            original.len()
        );
        std::fs::rename(replacement, &path).unwrap();
        let fresh = fixture.scan(&mut state);
        assert_eq!(fresh.len(), 1);
        assert_eq!(fresh[0].content, "after!");
        assert_eq!(state.inbox_scan_cache.parse_attempts, 2);
    }

    #[test]
    fn repeated_scan_benchmark_avoids_all_unchanged_json_parses() {
        let fixture = Fixture::new();
        const FILES: usize = 8;
        const POLLS: usize = 20;
        for file in 0..FILES {
            let messages: Vec<_> = (0..250)
                .map(|message| {
                    format!(
                        "synthetic file {file} message {message}: {}",
                        "text ".repeat(24)
                    )
                })
                .collect();
            fixture.write(
                &format!("{file}.json"),
                &messages.iter().map(String::as_str).collect::<Vec<_>>(),
            );
        }
        let mut uncached = empty_state();
        let before = std::time::Instant::now();
        for _ in 0..POLLS {
            uncached.inbox_scan_cache.files.clear();
            fixture.scan(&mut uncached);
        }
        let uncached_elapsed = before.elapsed();

        let mut cached = empty_state();
        fixture.scan(&mut cached);
        cached.inbox_scan_cache.parse_attempts = 0;
        let before = std::time::Instant::now();
        for _ in 0..POLLS {
            assert!(fixture.scan(&mut cached).is_empty());
        }
        let cached_elapsed = before.elapsed();
        assert_eq!(uncached.inbox_scan_cache.parse_attempts, FILES * POLLS);
        assert_eq!(cached.inbox_scan_cache.parse_attempts, 0);
        assert_eq!(cached.messages.len(), uncached.messages.len());
        eprintln!("synthetic scan benchmark: {FILES} inboxes, 2,000 messages, {POLLS} polls; uncached {uncached_elapsed:?} / {} parses; cached {cached_elapsed:?} / 0 parses", FILES * POLLS);
    }
}

#[cfg(test)]
mod model_discovery_tests {
    use super::*;

    #[test]
    fn background_model_discovery_supports_local_catalog_without_credentials() {
        let models =
            tauri::async_runtime::block_on(discover_models("claude-code".into(), String::new()))
                .unwrap();
        assert_eq!(
            models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            vec!["haiku", "sonnet", "opus"]
        );
    }

    #[test]
    fn background_model_discovery_preserves_provider_errors() {
        let error = tauri::async_runtime::block_on(discover_models(
            "unknown-test-provider".into(),
            String::new(),
        ))
        .unwrap_err();
        assert_eq!(error, "unknown provider 'unknown-test-provider'");
    }
}

#[cfg(test)]
mod team_lifecycle_tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "agora-team-audit-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn config(team: &str) -> DebateConfig {
        DebateConfig {
            team_name: team.into(),
            agents: vec![orchestrator::AgentConfig {
                name: "agent".into(),
                provider: "test".into(),
                model: "model".into(),
                system_prompt: String::new(),
                role: "judge".into(),
            }],
            topics: vec!["topic".into()],
            visibility: "group".into(),
            termination: "fixed".into(),
            max_rounds: 1,
            convergence_threshold: 2,
        }
    }
    fn state() -> AppState {
        AppState {
            seen_hashes: HashSet::new(),
            messages: vec![],
            known_teams: HashSet::new(),
            config: AppConfig::default(),
            debates: HashMap::new(),
            inbox_scan_cache: InboxScanCache::default(),
        }
    }
    fn message(team: &str, text: &str) -> Message {
        Message {
            from: "agent".into(),
            to: "all".into(),
            content: text.into(),
            timestamp: 1,
            team: team.into(),
            role: String::new(),
        }
    }

    #[test]
    fn recognized_empty_and_protocol_inboxes_do_not_fabricate_sender_messages() {
        let fixture = Fixture::new();
        let path = fixture.0.join("all.json");
        for raw in [
            r#"{"messages":[]}"#,
            r#"{"inbox":[{"type":"heartbeat"}]}"#,
            r#"{"type":"heartbeat","from":"agent","text":"idle"}"#,
        ] {
            std::fs::write(&path, raw).unwrap();
            assert!(parse_inbox(&path, "team").unwrap().is_empty());
        }
        std::fs::write(&path, r#"{"agent":"real text","metadata":{}}"#).unwrap();
        let parsed = parse_inbox(&path, "team").unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].2, "real text");
    }

    #[test]
    fn snapshot_merges_same_team_watched_history_with_exact_live_duplicates() {
        let mut st = state();
        let mut debate = DebateState::new(config("team"));
        debate.status = orchestrator::DebateStatus::Stopped;
        let mut live =
            orchestrator::make_message(&debate.config.agents[0], "live turn", &debate.config, 0, 1);
        live.timestamp = 20;
        let mut duplicate = message("team", "live turn");
        duplicate.timestamp = 20;
        let mut earlier = duplicate.clone();
        earlier.timestamp = 10;
        st.messages = vec![
            message("team", "independent watched turn"),
            message("other", "watched turn"),
            duplicate,
            earlier,
        ];
        debate.messages.push(live);
        st.debates
            .insert("team".into(), Arc::new(Mutex::new(debate)));
        let snapshot = message_snapshot(&st);
        assert_eq!(snapshot.len(), 4);
        assert!(snapshot
            .iter()
            .any(|message| message.content == "watched turn"));
        assert!(snapshot
            .iter()
            .any(|message| message.content == "independent watched turn"));
        assert!(snapshot.iter().any(|message| message.content == "live turn"
            && message.timestamp == 20
            && message.role == "judge"));
        assert!(snapshot
            .iter()
            .any(|message| message.content == "live turn" && message.timestamp == 10));
    }

    #[test]
    fn deleting_active_team_is_rejected_then_success_clears_only_its_runtime_state() {
        let fixture = Fixture::new();
        std::fs::create_dir_all(fixture.0.join("team/inboxes")).unwrap();
        std::fs::create_dir_all(fixture.0.join("other/inboxes")).unwrap();
        let mut st = state();
        st.messages = vec![message("team", "turn"), message("other", "keep")];
        st.known_teams.extend(["team".into(), "other".into()]);
        let hash = hash_message("team", "agent", "all", "turn");
        st.seen_hashes.insert(hash);
        let streamed = Mutex::new(HashSet::from([hash]));
        let mut debate = DebateState::new(config("team"));
        debate.worker_active = true;
        let debate = Arc::new(Mutex::new(debate));
        st.debates.insert("team".into(), debate.clone());
        assert!(delete_team_at(&fixture.0, &mut st, &streamed, "team").is_err());
        assert!(fixture.0.join("team").exists());
        assert_eq!(st.messages.len(), 2);
        debate.lock().unwrap().worker_active = false;
        delete_team_at(&fixture.0, &mut st, &streamed, "team").unwrap();
        assert!(!fixture.0.join("team").exists());
        assert!(fixture.0.join("other").exists());
        assert_eq!(st.messages[0].team, "other");
        assert!(!st.debates.contains_key("team"));
        assert!(!st.known_teams.contains("team"));
        assert!(!st.seen_hashes.contains(&hash));
        assert!(!streamed.lock().unwrap().contains(&hash));
    }

    #[test]
    fn team_names_keep_their_exact_identity() {
        let fixture = Fixture::new();
        let first = team_path_from(&fixture.0, "design review").unwrap();
        let second = team_path_from(&fixture.0, "design_review").unwrap();
        assert_ne!(first, second);
        assert_eq!(first.file_name().unwrap(), "design review");
        assert!(validate_team_name("").is_err());
        assert!(validate_team_name("nested/team").is_err());
    }

    #[test]
    fn archive_failure_preserves_inboxes_and_success_moves_them_together() {
        let fixture = Fixture::new();
        let team = fixture.0.join("design review");
        std::fs::create_dir_all(team.join("inboxes")).unwrap();
        std::fs::write(team.join("inboxes/all.json"), b"[]").unwrap();
        std::fs::write(team.join("inboxes/agent.json"), b"[]").unwrap();
        std::fs::write(team.join("archive"), b"blocked").unwrap();
        assert!(archive_team_inboxes(&fixture.0, &config("design review")).is_err());
        assert_eq!(std::fs::read(team.join("inboxes/all.json")).unwrap(), b"[]");
        assert!(team.join("inboxes/agent.json").exists());
        std::fs::remove_file(team.join("archive")).unwrap();
        archive_team_inboxes(&fixture.0, &config("design review")).unwrap();
        assert!(!team.join("inboxes").exists());
        assert!(team.join("archive/topic/all.json").exists());
        assert!(team.join("archive/topic/agent.json").exists());
    }

    #[test]
    fn reset_clears_only_archived_team_rows_and_only_after_archive_success() {
        let fixture = Fixture::new();
        let team = fixture.0.join("team");
        std::fs::create_dir_all(team.join("inboxes")).unwrap();
        std::fs::write(team.join("inboxes/all.json"), b"[]").unwrap();
        std::fs::write(team.join("archive"), b"blocked").unwrap();
        let mut st = state();
        st.messages = vec![
            message("team", "archive this"),
            message("other", "keep this"),
        ];
        assert!(archive_and_clear_watched(&fixture.0, &mut st, &config("team")).is_err());
        assert_eq!(st.messages.len(), 2);
        std::fs::remove_file(team.join("archive")).unwrap();
        archive_and_clear_watched(&fixture.0, &mut st, &config("team")).unwrap();
        assert_eq!(st.messages.len(), 1);
        assert_eq!(st.messages[0].team, "other");
        assert!(team.join("archive/topic/all.json").exists());
    }
}
