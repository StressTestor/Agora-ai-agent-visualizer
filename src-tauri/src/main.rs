#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod orchestrator;
mod presets;
mod provider;

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
use tauri::{AppHandle, Emitter, State};

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
fn parse_inbox(path: &Path, _team: &str) -> Vec<(String, String, String, u64)> {
    let to = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();

    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return vec![],
    };

    let val: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return vec![],
    };

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
        if !msgs.is_empty() {
            return resolve(msgs);
        }
    }

    // Strategy 2: { messages: [...] }
    if let Some(arr) = val.get("messages").and_then(|v| v.as_array()) {
        let msgs: Vec<_> = arr.iter().filter_map(|v| extract_msg(v, &to)).collect();
        if !msgs.is_empty() {
            return resolve(msgs);
        }
    }

    // Strategy 3: { inbox: [...] }
    if let Some(arr) = val.get("inbox").and_then(|v| v.as_array()) {
        let msgs: Vec<_> = arr.iter().filter_map(|v| extract_msg(v, &to)).collect();
        if !msgs.is_empty() {
            return resolve(msgs);
        }
    }

    // Strategy 4: single message object
    if let Some(msg) = extract_msg(&val, &to) {
        return resolve(vec![msg]);
    }

    // Strategy 5: { sender_name: "text" } key-value map
    if let Some(obj) = val.as_object() {
        return obj
            .iter()
            .map(|(k, v)| {
                let content = v
                    .as_str()
                    .map(String::from)
                    .unwrap_or_else(|| v.to_string());
                (k.clone(), to.clone(), content, fallback)
            })
            .collect();
    }

    vec![]
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
fn scan_inboxes(dir: &Path, state: &mut AppState) -> Vec<Message> {
    let mut new_msgs = vec![];

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
            for (from, to, content, ts) in parse_inbox(&path, &team) {
                let hash = hash_message(&team, &from, &to, &content);
                if state.seen_hashes.insert(hash) {
                    let msg = Message {
                        from,
                        to,
                        content,
                        timestamp: ts,
                        team: team.clone(),
                    };
                    state.messages.push(msg.clone());
                    new_msgs.push(msg);
                }
            }
        }
    }
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

#[tauri::command]
fn get_messages(state: State<'_, Arc<Mutex<AppState>>>) -> Vec<Message> {
    state.lock().unwrap().messages.clone()
}

#[tauri::command]
fn get_config(state: State<'_, Arc<Mutex<AppState>>>) -> AppConfig {
    state.lock().unwrap().config.clone()
}

#[tauri::command]
fn save_config(
    state: State<'_, Arc<Mutex<AppState>>>,
    config: AppConfig,
) -> Result<(), String> {
    config.save()?;
    state.lock().unwrap().config = config;
    Ok(())
}

#[tauri::command]
fn list_models(
    state: State<'_, Arc<Mutex<AppState>>>,
    provider_name: String,
) -> Result<Vec<provider::ModelInfo>, String> {
    let api_key = {
        let st = state.lock().unwrap();
        st.config
            .api_key(&provider_name)
            .ok_or_else(|| format!("no API key configured for '{provider_name}'"))?
    };
    let p = provider::build_provider(&provider_name, &api_key)
        .ok_or_else(|| format!("unknown provider '{provider_name}'"))?;
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

#[tauri::command]
fn create_debate(
    state: State<'_, Arc<Mutex<AppState>>>,
    config: DebateConfig,
) -> Result<String, String> {
    let team = config.team_name.clone();
    let debate_state = Arc::new(Mutex::new(DebateState::new(config)));
    let mut st = state.lock().unwrap();
    st.debates.insert(team.clone(), debate_state);
    Ok(team)
}

#[tauri::command]
fn start_debate_cmd(
    app: AppHandle,
    state: State<'_, Arc<Mutex<AppState>>>,
    team: String,
) -> Result<(), String> {
    let (debate_state, app_config) = {
        let st = state.lock().unwrap();
        let ds = st
            .debates
            .get(&team)
            .ok_or_else(|| format!("no debate '{team}'"))?
            .clone();
        (ds, st.config.clone())
    };
    orchestrator::start_debate(app, app_config, debate_state);
    Ok(())
}

#[tauri::command]
fn stop_debate(
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
    Ok(())
}

#[tauri::command]
fn pause_debate(
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
    Ok(())
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
    let status_str = match &debate.status {
        orchestrator::DebateStatus::Running => "running",
        orchestrator::DebateStatus::Paused => "paused",
        orchestrator::DebateStatus::Stopped => "stopped",
        orchestrator::DebateStatus::Converged => "converged",
        orchestrator::DebateStatus::Error(_) => "error",
    };
    Ok(orchestrator::DebateStatusEvent {
        team: debate.config.team_name.clone(),
        status: status_str.to_string(),
        round: debate.current_round,
        total_messages: debate.messages.len(),
    })
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    let cli_team: Option<String> = {
        let args: Vec<String> = std::env::args().collect();
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

    let shared_state = Arc::new(Mutex::new(AppState {
        seen_hashes: HashSet::new(),
        messages: vec![],
        known_teams: initial_known,
        config: app_config,
        debates: HashMap::new(),
    }));

    let state_for_setup = shared_state.clone();

    tauri::Builder::default()
        .plugin(tauri_plugin_window_state::Builder::new().build())
        .manage(shared_state)
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
        .setup(move |app| {
            let handle: AppHandle = app.handle().clone();
            let state = state_for_setup.clone();
            let initial_team = cli_team.clone();

            // Initial scan on the calling thread (fast)
            {
                let mut st = state.lock().unwrap();
                let new_msgs = scan_inboxes(&tdir, &mut st);
                drop(new_msgs); // already stored in state.messages; frontend loads via get_messages
            }

            // Background watcher thread
            std::thread::spawn(move || {
                let (tx, rx) = std::sync::mpsc::channel::<notify::Result<Event>>();

                let mut watcher = notify::recommended_watcher(move |res| {
                    let _ = tx.send(res);
                })
                .expect("failed to create file watcher");

                let tdir = teams_dir();
                let tkdir = tasks_dir();

                if tdir.exists() {
                    watcher.watch(&tdir, RecursiveMode::Recursive).ok();
                }
                if tkdir.exists() {
                    watcher.watch(&tkdir, RecursiveMode::Recursive).ok();
                }

                let poll_interval = std::time::Duration::from_secs(2);
                let mut last_poll = std::time::Instant::now();

                loop {
                    let mut inbox_dirty = false;
                    let mut changed_task_paths: Vec<PathBuf> = vec![];

                    // Drain watcher events (non-blocking)
                    loop {
                        match rx.try_recv() {
                            Ok(Ok(event)) => match event.kind {
                                EventKind::Modify(_) | EventKind::Create(_) => {
                                    for p in &event.paths {
                                        let s = p.to_string_lossy();
                                        if s.contains("inboxes") {
                                            inbox_dirty = true;
                                        } else if s.contains("tasks") && s.ends_with(".json") {
                                            changed_task_paths.push(p.clone());
                                        } else if s.contains("teams") {
                                            // Could be a new team dir
                                            inbox_dirty = true;
                                        }
                                    }
                                }
                                _ => {}
                            },
                            Ok(Err(e)) => eprintln!("watcher error: {e}"),
                            Err(std::sync::mpsc::TryRecvError::Empty) => break,
                            Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
                        }
                    }

                    // Periodic poll fallback
                    if last_poll.elapsed() >= poll_interval {
                        inbox_dirty = true;
                        last_poll = std::time::Instant::now();
                    }

                    if inbox_dirty {
                        let mut st = state.lock().unwrap();

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
                        let new_msgs = scan_inboxes(&tdir, &mut st);
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
