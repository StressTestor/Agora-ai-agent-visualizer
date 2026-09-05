use crate::config::AppConfig;
use crate::orchestrator::{AgentConfig, DebateConfig, DebateMessage, DebateState, DebateStatus};
use crate::provider::{self, Provider};
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame, Terminal,
};
use std::io::stdout;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Events from debate thread → TUI
// ---------------------------------------------------------------------------

pub enum DebateEvent {
    AgentThinking {
        agent: String,
    },
    Chunk {
        text: String,
    },
    MessageComplete {
        agent: String,
        round: u32,
        content: String,
    },
    RoundAdvance {
        round: u32,
    },
    TopicChange {
        topic: String,
    },
    Error {
        msg: String,
    },
    Done {
        status: String,
        rounds: u32,
        messages: usize,
    },
}

// ---------------------------------------------------------------------------
// Agent colors
// ---------------------------------------------------------------------------

const AGENT_COLORS: &[Color] = &[
    Color::Cyan,
    Color::Green,
    Color::Yellow,
    Color::Magenta,
    Color::Blue,
    Color::Red,
];

fn agent_color(idx: usize) -> Color {
    AGENT_COLORS[idx % AGENT_COLORS.len()]
}

// ---------------------------------------------------------------------------
// Chat entry — a completed or system message
// ---------------------------------------------------------------------------

struct ChatEntry {
    agent_idx: Option<usize>,
    agent_name: String,
    round: u32,
    text: String,
    is_system: bool,
    duration_secs: Option<f64>,
}

// ---------------------------------------------------------------------------
// TUI state
// ---------------------------------------------------------------------------

struct TuiState {
    debate_name: String,
    agents: Vec<AgentConfig>,
    topics: Vec<String>,
    max_rounds: u32,
    termination: String,
    current_round: u32,
    status: String,
    status_detail: String,
    entries: Vec<ChatEntry>,
    current_streaming: String,
    current_agent: String,
    current_agent_idx: Option<usize>,
    is_streaming: bool, // true once first chunk arrives
    scroll_offset: usize,
    auto_scroll: bool,
    done: bool,
    failure: Option<String>,
    total_content_height: usize,
    start_time: Instant,
    agent_start_time: Instant, // when current agent started
}

impl TuiState {
    fn new(config: &DebateConfig) -> Self {
        let now = Instant::now();
        Self {
            debate_name: config.team_name.clone(),
            agents: config.agents.clone(),
            topics: config.topics.clone(),
            max_rounds: config.max_rounds,
            termination: config.termination.clone(),
            current_round: 1,
            status: "starting".to_string(),
            status_detail: String::new(),
            entries: vec![],
            current_streaming: String::new(),
            current_agent: String::new(),
            current_agent_idx: None,
            is_streaming: false,
            scroll_offset: 0,
            auto_scroll: true,
            done: false,
            failure: None,
            total_content_height: 0,
            start_time: now,
            agent_start_time: now,
        }
    }

    fn apply_event(&mut self, event: DebateEvent) {
        match event {
            DebateEvent::AgentThinking { agent } => {
                self.current_agent = agent.clone();
                self.current_agent_idx = self.agent_index(&agent);
                self.current_streaming.clear();
                self.is_streaming = false;
                self.agent_start_time = Instant::now();
                self.status_detail = format!("{agent} is thinking…");
                self.status = "running".to_string();
            }
            DebateEvent::Chunk { text } => {
                if !self.is_streaming {
                    self.is_streaming = true;
                    self.status_detail = format!("{} is responding…", self.current_agent);
                }
                self.current_streaming.push_str(&text);
            }
            DebateEvent::MessageComplete {
                agent,
                round,
                content,
            } => {
                let agent_idx = self.agent_index(&agent);
                let duration = self.agent_start_time.elapsed().as_secs_f64();
                self.entries.push(ChatEntry {
                    agent_idx,
                    agent_name: agent.clone(),
                    round,
                    text: content,
                    is_system: false,
                    duration_secs: Some(duration),
                });
                self.is_streaming = false;
                self.current_streaming.clear();
                self.status_detail.clear();
            }
            DebateEvent::RoundAdvance { round } => {
                self.current_round = round;
                self.entries.push(ChatEntry {
                    agent_idx: None,
                    agent_name: String::new(),
                    round,
                    text: format!("round {round}"),
                    is_system: true,
                    duration_secs: None,
                });
            }
            DebateEvent::TopicChange { topic } => {
                self.entries.push(ChatEntry {
                    agent_idx: None,
                    agent_name: String::new(),
                    round: self.current_round,
                    text: format!("topic: {topic}"),
                    is_system: true,
                    duration_secs: None,
                });
            }
            DebateEvent::Error { msg } => {
                self.status = "error".to_string();
                self.status_detail = msg.clone();
                self.failure = Some(msg);
                self.done = true;
                self.clear_streaming();
            }
            DebateEvent::Done {
                status,
                rounds,
                messages,
            } => {
                self.status = status.clone();
                let round_word = if rounds == 1 { "round" } else { "rounds" };
                let msg_word = if messages == 1 { "message" } else { "messages" };
                self.status_detail = format!(
                    "{status} · {rounds} {round_word} · {messages} {msg_word} · {}",
                    self.elapsed_str()
                );
                self.done = true;
                self.clear_streaming();
            }
        }
    }

    fn clear_streaming(&mut self) {
        self.current_streaming.clear();
        self.current_agent.clear();
        self.current_agent_idx = None;
        self.is_streaming = false;
    }

    fn agent_index(&self, name: &str) -> Option<usize> {
        self.agents.iter().position(|a| a.name == name)
    }

    fn elapsed_str(&self) -> String {
        let secs = self.start_time.elapsed().as_secs();
        if secs < 60 {
            format!("{secs}s")
        } else {
            format!("{}m {:02}s", secs / 60, secs % 60)
        }
    }
}

// ---------------------------------------------------------------------------
// Render a text line with basic markdown formatting
// ---------------------------------------------------------------------------

fn render_text_line(text: &str, base_color: Color) -> Vec<Span<'static>> {
    let trimmed = text.trim_start();

    // Markdown headers → bold + color
    if trimmed.starts_with("### ") {
        return vec![
            Span::raw("  "),
            Span::styled(
                trimmed.trim_start_matches('#').trim().to_string(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ];
    }
    if trimmed.starts_with("## ") {
        return vec![
            Span::raw("  "),
            Span::styled(
                trimmed.trim_start_matches('#').trim().to_string(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ];
    }
    if trimmed.starts_with("# ") {
        return vec![
            Span::raw("  "),
            Span::styled(
                trimmed.trim_start_matches('#').trim().to_string(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            ),
        ];
    }
    // Horizontal rule
    if trimmed == "---" || trimmed == "***" || trimmed == "___" {
        return vec![Span::styled(
            "  ─────────",
            Style::default().fg(Color::DarkGray),
        )];
    }
    // Bullet points
    if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
        return vec![
            Span::raw("  "),
            Span::styled("• ", Style::default().fg(Color::DarkGray)),
            Span::styled(trimmed[2..].to_string(), Style::default().fg(base_color)),
        ];
    }

    vec![
        Span::raw("  "),
        Span::styled(text.to_string(), Style::default().fg(base_color)),
    ]
}

// ---------------------------------------------------------------------------
// TUI rendering
// ---------------------------------------------------------------------------

fn render(frame: &mut Frame, state: &mut TuiState) {
    let area = frame.area();

    let main_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(22), Constraint::Min(40)])
        .split(area);

    let right_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(3),
        ])
        .split(main_layout[1]);

    render_sidebar(frame, state, main_layout[0]);
    render_header(frame, state, right_layout[0]);
    render_chat(frame, state, right_layout[1]);
    render_status_bar(frame, state, right_layout[2]);
}

fn render_sidebar(frame: &mut Frame, state: &TuiState, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(Span::styled(
            " agents ",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let sidebar_width = inner.width.saturating_sub(4) as usize;
    let mut lines: Vec<Line> = vec![];

    for (i, agent) in state.agents.iter().enumerate() {
        let color = agent_color(i);
        let is_active = state.current_agent == agent.name;
        let marker = if is_active { "▸" } else { "●" };

        // Agent name with marker
        let name_style = if is_active {
            Style::default().fg(color).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(color)
        };

        lines.push(Line::from(vec![
            Span::styled(format!(" {marker} "), Style::default().fg(color)),
            Span::styled(&agent.name, name_style),
        ]));

        // Provider
        lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled(&agent.provider, Style::default().fg(Color::DarkGray)),
        ]));

        // Model (truncated to sidebar width, UTF-8 safe)
        let model_display = if agent.model.chars().count() > sidebar_width {
            let truncated: String = agent
                .model
                .chars()
                .take(sidebar_width.saturating_sub(1))
                .collect();
            format!("{truncated}…")
        } else {
            agent.model.clone()
        };
        lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled(model_display, Style::default().fg(Color::DarkGray)),
        ]));

        // Role
        lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled(
                format!("[{}]", agent.role),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
        lines.push(Line::from(""));
    }

    let para = Paragraph::new(lines);
    frame.render_widget(para, inner);
}

fn render_header(frame: &mut Frame, state: &TuiState, area: Rect) {
    let round_info = if state.termination == "convergence" {
        format!("round {}", state.current_round)
    } else {
        format!("round {}/{}", state.current_round, state.max_rounds)
    };

    let status_color = match state.status.as_str() {
        "running" => Color::Green,
        "converged" => Color::Cyan,
        "stopped" => Color::Yellow,
        "error" => Color::Red,
        _ => Color::DarkGray,
    };

    let elapsed = state.elapsed_str();

    let header_line = Line::from(vec![
        Span::styled(
            " ▸ ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            &state.debate_name,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(&round_info, Style::default().fg(Color::DarkGray)),
        Span::raw("  "),
        Span::styled("● ", Style::default().fg(status_color)),
        Span::styled(&state.status, Style::default().fg(status_color)),
        Span::raw("  "),
        Span::styled(elapsed, Style::default().fg(Color::DarkGray)),
    ]);

    let topic_line = if let Some(topic) = state.topics.first() {
        let max_len = area.width.saturating_sub(6) as usize;
        let display = if topic.chars().count() > max_len {
            let truncated: String = topic.chars().take(max_len.saturating_sub(1)).collect();
            format!("{truncated}…")
        } else {
            topic.clone()
        };
        Line::from(vec![
            Span::raw("   "),
            Span::styled(display, Style::default().fg(Color::DarkGray)),
        ])
    } else {
        Line::from("")
    };

    let block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(Style::default().fg(Color::DarkGray));

    let para = Paragraph::new(vec![header_line, topic_line]).block(block);
    frame.render_widget(para, area);
}

fn render_chat(frame: &mut Frame, state: &mut TuiState, area: Rect) {
    let inner = area;
    let chat_width = inner.width.saturating_sub(4) as usize;
    if chat_width == 0 {
        return;
    }

    let mut lines: Vec<Line> = vec![];

    for entry in &state.entries {
        if entry.is_system {
            // System/round separator — full-width colored line
            lines.push(Line::from(""));
            lines.push(Line::from(vec![Span::styled(
                format!("  ── {}", entry.text),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            )]));
            lines.push(Line::from(""));
        } else {
            let color = entry.agent_idx.map(agent_color).unwrap_or(Color::White);

            // Colored separator line before each agent message
            let bar = "─".repeat(chat_width.min(50));
            lines.push(Line::from(vec![Span::styled(
                format!("  {bar}"),
                Style::default().fg(color).add_modifier(Modifier::DIM),
            )]));

            // Agent header with duration
            let mut header_spans = vec![
                Span::styled(
                    format!("  {} ", entry.agent_name),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("· round {}", entry.round),
                    Style::default().fg(Color::DarkGray),
                ),
            ];
            if let Some(dur) = entry.duration_secs {
                header_spans.push(Span::styled(
                    format!("  {dur:.1}s"),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            lines.push(Line::from(header_spans));
            lines.push(Line::from(""));

            // Content with markdown formatting
            for text_line in entry.text.lines() {
                if text_line.is_empty() {
                    lines.push(Line::from(""));
                } else {
                    for wrapped in word_wrap(text_line, chat_width.saturating_sub(2)) {
                        lines.push(Line::from(render_text_line(&wrapped, Color::White)));
                    }
                }
            }
            lines.push(Line::from(""));
        }
    }

    // Current streaming text
    if !state.current_streaming.is_empty() {
        if let Some(idx) = state.current_agent_idx {
            let color = agent_color(idx);

            // Separator
            let bar = "─".repeat(chat_width.min(50));
            lines.push(Line::from(vec![Span::styled(
                format!("  {bar}"),
                Style::default().fg(color).add_modifier(Modifier::DIM),
            )]));

            // Header
            let elapsed = state.agent_start_time.elapsed().as_secs_f64();
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {} ", state.current_agent),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("· round {}", state.current_round),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    format!("  {elapsed:.0}s"),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(" ◉", Style::default().fg(color)),
            ]));
            lines.push(Line::from(""));

            for text_line in state.current_streaming.lines() {
                if text_line.is_empty() {
                    lines.push(Line::from(""));
                } else {
                    for wrapped in word_wrap(text_line, chat_width.saturating_sub(2)) {
                        lines.push(Line::from(render_text_line(&wrapped, Color::White)));
                    }
                }
            }
        }
    }

    let total_lines = lines.len();
    state.total_content_height = total_lines;
    let visible_height = inner.height as usize;

    if state.auto_scroll && total_lines > visible_height {
        state.scroll_offset = total_lines.saturating_sub(visible_height);
    }

    let para = Paragraph::new(lines)
        .scroll((state.scroll_offset as u16, 0))
        .wrap(Wrap { trim: false });

    frame.render_widget(para, inner);
}

fn render_status_bar(frame: &mut Frame, state: &TuiState, area: Rect) {
    let msg_count = state.entries.iter().filter(|e| !e.is_system).count();
    let msg_word = if msg_count == 1 {
        "message"
    } else {
        "messages"
    };

    let left = if !state.status_detail.is_empty() {
        let color = if state.done {
            Color::DarkGray
        } else {
            Color::Yellow
        };
        Span::styled(
            format!(" {} ", state.status_detail),
            Style::default().fg(color),
        )
    } else if state.done {
        Span::styled(" debate complete ", Style::default().fg(Color::Green))
    } else {
        Span::styled(" waiting... ", Style::default().fg(Color::DarkGray))
    };

    let right = Span::styled(
        format!(" {msg_count} {msg_word}  ↑↓ scroll  q quit "),
        Style::default().fg(Color::DarkGray),
    );

    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(Color::DarkGray));

    let line = Line::from(vec![left, Span::raw("  "), right]);
    let para = Paragraph::new(line).block(block);
    frame.render_widget(para, area);
}

// ---------------------------------------------------------------------------
// Word wrap
// ---------------------------------------------------------------------------

fn word_wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut lines = vec![];
    let mut current = String::new();

    for word in text.split_whitespace() {
        if current.is_empty() {
            current = word.to_string();
        } else if current.len() + 1 + word.len() > width {
            lines.push(current);
            current = word.to_string();
        } else {
            current.push(' ');
            current.push_str(word);
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

// ---------------------------------------------------------------------------
// Run the debate with TUI
// ---------------------------------------------------------------------------

pub fn run_tui_debate(
    debate_config: DebateConfig,
    agents: Vec<AgentConfig>,
    persist: bool,
) -> Result<(), String> {
    let app_config = AppConfig::load();

    let (tx, rx) = mpsc::channel::<DebateEvent>();

    let providers: Vec<Option<Box<dyn Provider>>> = agents
        .iter()
        .map(|agent| {
            let api_key = app_config.api_key(&agent.provider).unwrap_or_default();
            provider::build_provider(&agent.provider, &api_key)
        })
        .collect();

    for (i, p) in providers.iter().enumerate() {
        if p.is_none() {
            return Err(format!(
                "no provider configured for agent '{}'",
                agents[i].name
            ));
        }
    }

    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_clone = cancel.clone();

    let config_clone = debate_config.clone();
    std::thread::spawn(move || {
        run_debate_thread(config_clone, providers, persist, tx, cancel_clone);
    });

    run_tui_loop(debate_config, rx, cancel)
}

fn run_debate_thread(
    config: DebateConfig,
    providers: Vec<Option<Box<dyn Provider>>>,
    persist: bool,
    tx: mpsc::Sender<DebateEvent>,
    cancel: Arc<AtomicBool>,
) {
    let mut state = DebateState::new(config.clone());
    state.status = DebateStatus::Running;
    state.current_round = 1;

    if persist {
        if let Err(error) = crate::orchestrator::init_team_on_disk(&config) {
            let _ = tx.send(DebateEvent::Error { msg: error });
            return;
        }
    }

    'debate: loop {
        if cancel.load(Ordering::Relaxed) {
            break;
        }

        match &state.status {
            DebateStatus::Stopped | DebateStatus::Converged | DebateStatus::Error(_) => break,
            _ => {}
        }

        if crate::orchestrator::should_stop(&state) {
            state.status = if state.config.termination == "convergence" {
                DebateStatus::Converged
            } else {
                DebateStatus::Stopped
            };
            break;
        }

        let agent_idx = state.current_agent_idx;
        let agent_config = state.config.agents[agent_idx].clone();

        let context = crate::orchestrator::build_context(&state, &agent_config);
        let provider = providers[agent_idx].as_ref().unwrap();

        let response = 'call: {
            let mut last_err = None;
            for attempt in 0..4u32 {
                if cancel.load(Ordering::Relaxed) {
                    break 'debate;
                }
                let _ = tx.send(DebateEvent::AgentThinking {
                    agent: agent_config.name.clone(),
                });
                let tx_ref = &tx;
                let mut on_chunk = |chunk: &str| {
                    let _ = tx_ref.send(DebateEvent::Chunk {
                        text: chunk.to_string(),
                    });
                };
                match provider.chat_streaming(&context, &agent_config.model, &mut on_chunk) {
                    Ok(text) => break 'call text,
                    Err(e) => {
                        let delay = crate::orchestrator::retry_delay(&e, attempt);
                        let retry = crate::orchestrator::should_retry(&e, attempt);
                        last_err = Some(e);
                        if retry {
                            let deadline = Instant::now() + Duration::from_secs(delay);
                            while Instant::now() < deadline {
                                if cancel.load(Ordering::Relaxed) {
                                    break 'debate;
                                }
                                std::thread::sleep(Duration::from_millis(50));
                            }
                        } else {
                            break;
                        }
                    }
                }
            }
            let err_msg = format!(
                "agent '{}' failed: {}",
                agent_config.name,
                last_err.unwrap()
            );
            state.status = DebateStatus::Error(err_msg.clone());
            let _ = tx.send(DebateEvent::Error { msg: err_msg });
            break 'debate;
        };

        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let (next_idx, new_round) =
            crate::orchestrator::next_turn(agent_idx, state.config.agents.len());
        let msg = crate::orchestrator::make_message(
            &agent_config,
            &response,
            &config,
            agent_idx,
            state.current_round,
        );

        if persist {
            if let Err(error) = crate::orchestrator::persist_message(&msg) {
                state.status = DebateStatus::Error(error.clone());
                let _ = tx.send(DebateEvent::Error { msg: error });
                break;
            }
        }

        let _ = tx.send(DebateEvent::MessageComplete {
            agent: agent_config.name.clone(),
            round: state.current_round,
            content: response.clone(),
        });

        state.messages.push(msg);
        state.current_agent_idx = next_idx;

        if new_round {
            state.current_round += 1;
            let _ = tx.send(DebateEvent::RoundAdvance {
                round: state.current_round,
            });

            if let Some(topic) = crate::orchestrator::advance_topic(
                &state.config,
                state.current_round,
                state.current_topic_idx,
            ) {
                let topic_msg = DebateMessage {
                    from: "system".to_string(),
                    to: "all".to_string(),
                    content: format!("moving to next topic: {topic}"),
                    timestamp: crate::orchestrator::now_ms(),
                    team: state.config.team_name.clone(),
                    role: "system".to_string(),
                };
                if persist {
                    if let Err(error) = crate::orchestrator::persist_message(&topic_msg) {
                        state.status = DebateStatus::Error(error.clone());
                        let _ = tx.send(DebateEvent::Error { msg: error });
                        break;
                    }
                }
                state.current_topic_idx += 1;
                state.messages.push(topic_msg);
                let _ = tx.send(DebateEvent::TopicChange { topic });
            }
        }

        for _ in 0..6 {
            if cancel.load(Ordering::Relaxed) {
                break 'debate;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    let (status_str, rounds, messages) = match &state.status {
        DebateStatus::Converged => (
            "converged".to_string(),
            state.current_round,
            state.messages.len(),
        ),
        DebateStatus::Stopped => (
            "stopped".to_string(),
            state.current_round,
            state.messages.len(),
        ),
        DebateStatus::Error(e) => (
            format!("error: {e}"),
            state.current_round,
            state.messages.len(),
        ),
        _ => (
            "done".to_string(),
            state.current_round,
            state.messages.len(),
        ),
    };

    let _ = tx.send(DebateEvent::Done {
        status: status_str,
        rounds: rounds.saturating_sub(1),
        messages,
    });
}

// Own cleanup before the first fallible terminal operation. The callback keeps
// cleanup testable without changing the test runner's real terminal or hooks.
struct TerminalSession<F: FnOnce()> {
    cancel: Arc<AtomicBool>,
    cleanup: Option<F>,
}

impl<F: FnOnce()> TerminalSession<F> {
    fn new(cancel: Arc<AtomicBool>, cleanup: F) -> Self {
        Self {
            cancel,
            cleanup: Some(cleanup),
        }
    }
}

impl<F: FnOnce()> Drop for TerminalSession<F> {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(cleanup) = self.cleanup.take() {
            cleanup();
        }
    }
}

fn run_tui_loop(
    config: DebateConfig,
    rx: mpsc::Receiver<DebateEvent>,
    cancel: Arc<AtomicBool>,
) -> Result<(), String> {
    // Install panic hook BEFORE entering raw mode
    let original_hook = Arc::new(std::panic::take_hook());
    let forwarded_hook = original_hook.clone();
    std::panic::set_hook(Box::new(move |info| {
        // Best-effort terminal restore
        let _ = disable_raw_mode();
        let _ = stdout().execute(LeaveAlternateScreen);
        forwarded_hook(info);
    }));

    let session = TerminalSession::new(cancel.clone(), move || {
        let _ = disable_raw_mode();
        let _ = stdout().execute(LeaveAlternateScreen);
        // set_hook itself panics during unwinding. The installed hook has
        // already restored the display and forwarded the original panic then.
        if !std::thread::panicking() {
            std::panic::set_hook(Box::new(move |info| original_hook(info)));
        }
    });

    stdout()
        .execute(EnterAlternateScreen)
        .map_err(|e| format!("failed to enter alternate screen: {e}"))?;
    enable_raw_mode().map_err(|e| format!("failed to enable raw mode: {e}"))?;

    let backend = ratatui::backend::CrosstermBackend::new(stdout());
    let mut terminal =
        Terminal::new(backend).map_err(|e| format!("failed to create terminal: {e}"))?;

    let mut state = TuiState::new(&config);
    state.status = "running".to_string();

    let result = run_event_loop(&mut terminal, &mut state, rx, cancel);

    drop(terminal);
    drop(session);

    // Print summary after TUI exits
    if state.done {
        let msg_count = state.entries.iter().filter(|e| !e.is_system).count();
        eprintln!(
            "\nagora: {} · {} messages · {}\n",
            state.status,
            msg_count,
            state.elapsed_str()
        );
    }

    result.and_then(|_| state.failure.map_or(Ok(()), Err))
}

fn run_event_loop(
    terminal: &mut Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    state: &mut TuiState,
    rx: mpsc::Receiver<DebateEvent>,
    cancel: Arc<AtomicBool>,
) -> Result<(), String> {
    loop {
        terminal
            .draw(|frame| render(frame, state))
            .map_err(|e| format!("draw error: {e}"))?;

        // Process pending debate events
        loop {
            match rx.try_recv() {
                Ok(evt) => state.apply_event(evt),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    if !state.done {
                        state.done = true;
                        state.status = "done".to_string();
                    }
                    break;
                }
            }
        }

        // Keyboard input
        if event::poll(Duration::from_millis(30)).map_err(|e| format!("poll error: {e}"))? {
            if let Event::Key(key) = event::read().map_err(|e| format!("read error: {e}"))? {
                match key.code {
                    KeyCode::Char('q') => {
                        cancel.store(true, Ordering::Relaxed);
                        break;
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        cancel.store(true, Ordering::Relaxed);
                        break;
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        state.auto_scroll = false;
                        state.scroll_offset = state.scroll_offset.saturating_sub(3);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        state.scroll_offset = state.scroll_offset.saturating_add(3);
                        let visible = terminal.size().map(|s| s.height as usize).unwrap_or(40);
                        if state.scroll_offset + visible >= state.total_content_height {
                            state.auto_scroll = true;
                        }
                    }
                    KeyCode::PageUp => {
                        state.auto_scroll = false;
                        state.scroll_offset = state.scroll_offset.saturating_sub(20);
                    }
                    KeyCode::PageDown => {
                        state.scroll_offset = state.scroll_offset.saturating_add(20);
                        let visible = terminal.size().map(|s| s.height as usize).unwrap_or(40);
                        if state.scroll_offset + visible >= state.total_content_height {
                            state.auto_scroll = true;
                        }
                    }
                    KeyCode::Home => {
                        state.auto_scroll = false;
                        state.scroll_offset = 0;
                    }
                    KeyCode::End => {
                        state.auto_scroll = true;
                    }
                    _ => {}
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn streaming_test_config() -> DebateConfig {
        DebateConfig {
            team_name: "synthetic-tui-test".into(),
            agents: vec![AgentConfig {
                name: "agent".into(),
                provider: "mock".into(),
                model: "mock".into(),
                system_prompt: String::new(),
                role: "debater".into(),
            }],
            topics: vec!["test topic".into()],
            visibility: "group".into(),
            termination: "fixed".into(),
            max_rounds: 1,
            convergence_threshold: 2,
        }
    }

    #[test]
    fn retried_message_uses_authoritative_result_and_preserves_scroll_position() {
        struct Retried(std::sync::atomic::AtomicUsize);
        impl Provider for Retried {
            fn chat(
                &self,
                _: &[crate::provider::ChatMessage],
                _: &str,
            ) -> Result<String, crate::provider::ProviderError> {
                unreachable!("the test calls streaming")
            }
            fn list_models(
                &self,
            ) -> Result<Vec<crate::provider::ModelInfo>, crate::provider::ProviderError>
            {
                Ok(vec![])
            }
            fn chat_streaming(
                &self,
                _: &[crate::provider::ChatMessage],
                _: &str,
                chunk: &mut dyn FnMut(&str),
            ) -> Result<String, crate::provider::ProviderError> {
                if self.0.fetch_add(1, Ordering::Relaxed) == 0 {
                    chunk("discard this failed attempt");
                    Err(crate::provider::ProviderError::Network(
                        "synthetic disconnect".into(),
                    ))
                } else {
                    // A provider may return a final answer without any chunks.
                    Ok("final answer".into())
                }
            }
        }
        let config = streaming_test_config();
        let mut ui = TuiState::new(&config);
        ui.auto_scroll = false;
        let (tx, rx) = mpsc::channel();
        run_debate_thread(
            config,
            vec![Some(Box::new(Retried(
                std::sync::atomic::AtomicUsize::new(0),
            )))],
            false,
            tx,
            Arc::new(AtomicBool::new(false)),
        );
        for event in rx {
            ui.apply_event(event);
        }
        let messages: Vec<_> = ui.entries.iter().filter(|entry| !entry.is_system).collect();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].text, "final answer");
        assert!(ui.current_streaming.is_empty());
        assert!(
            !ui.auto_scroll,
            "incoming chunks and completions must not override manual scrolling"
        );
        assert!(ui.failure.is_none());
    }

    #[test]
    fn provider_failure_is_retained_for_the_tui_exit_result() {
        let config = streaming_test_config();
        let mut ui = TuiState::new(&config);
        ui.apply_event(DebateEvent::AgentThinking {
            agent: "agent".into(),
        });
        ui.apply_event(DebateEvent::Chunk {
            text: "unfinished response".into(),
        });
        ui.apply_event(DebateEvent::Error {
            msg: "synthetic rejection".into(),
        });
        assert!(ui.current_streaming.is_empty());
        assert!(!ui.is_streaming);
        assert!(ui.current_agent_idx.is_none());
        ui.apply_event(DebateEvent::Done {
            status: "error: synthetic rejection".into(),
            rounds: 0,
            messages: 0,
        });
        assert_eq!(ui.failure.as_deref(), Some("synthetic rejection"));
    }

    #[test]
    fn terminal_session_restores_and_cancels_on_every_setup_exit() {
        fn simulated_setup(
            cancel: Arc<AtomicBool>,
            cleanups: &std::sync::atomic::AtomicUsize,
            fail_at: Option<usize>,
        ) -> Result<(), &'static str> {
            let cleanup_cancel = cancel.clone();
            let _session = TerminalSession::new(cancel, || {
                assert!(cleanup_cancel.load(Ordering::Relaxed));
                cleanups.fetch_add(1, Ordering::Relaxed);
            });
            for stage in 0..3 {
                if fail_at == Some(stage) {
                    return Err("synthetic setup failure");
                }
            }
            Ok(())
        }
        for fail_at in [None, Some(0), Some(1), Some(2)] {
            let cancel = Arc::new(AtomicBool::new(false));
            let cleanups = std::sync::atomic::AtomicUsize::new(0);
            let result = simulated_setup(cancel.clone(), &cleanups, fail_at);
            assert_eq!(result.is_err(), fail_at.is_some());
            assert!(cancel.load(Ordering::Relaxed));
            assert_eq!(cleanups.load(Ordering::Relaxed), 1);
        }
    }

    #[test]
    fn done_event_clears_an_unfinished_stream() {
        let mut ui = TuiState::new(&streaming_test_config());
        ui.apply_event(DebateEvent::AgentThinking {
            agent: "agent".into(),
        });
        ui.apply_event(DebateEvent::Chunk {
            text: "unfinished response".into(),
        });
        ui.apply_event(DebateEvent::Done {
            status: "stopped".into(),
            rounds: 0,
            messages: 0,
        });
        assert!(ui.done);
        assert!(!ui.is_streaming);
        assert!(ui.current_streaming.is_empty());
        assert!(ui.current_agent.is_empty());
    }

    #[test]
    fn word_wrap_short_string() {
        let result = word_wrap("hello world", 80);
        assert_eq!(result, vec!["hello world"]);
    }

    #[test]
    fn word_wrap_wraps_at_width() {
        let result = word_wrap("hello world foo bar", 11);
        assert_eq!(result, vec!["hello world", "foo bar"]);
    }

    #[test]
    fn word_wrap_empty_string() {
        let result = word_wrap("", 80);
        assert_eq!(result, vec![""]);
    }

    #[test]
    fn word_wrap_zero_width() {
        let result = word_wrap("hello", 0);
        assert_eq!(result, vec!["hello"]);
    }

    #[test]
    fn word_wrap_single_long_word() {
        let result = word_wrap("superlongword", 5);
        assert_eq!(result, vec!["superlongword"]);
    }

    #[test]
    fn word_wrap_exact_width() {
        let result = word_wrap("ab cd", 5);
        assert_eq!(result, vec!["ab cd"]);
    }

    #[test]
    fn render_text_line_plain() {
        let spans = render_text_line("hello", Color::White);
        assert_eq!(spans.len(), 2);
    }

    #[test]
    fn render_text_line_h1() {
        let spans = render_text_line("# Title", Color::White);
        assert_eq!(spans.len(), 2);
    }

    #[test]
    fn render_text_line_bullet() {
        let spans = render_text_line("- item", Color::White);
        assert_eq!(spans.len(), 3);
    }

    #[test]
    fn render_text_line_horizontal_rule() {
        let spans = render_text_line("---", Color::White);
        assert_eq!(spans.len(), 1);
    }

    #[test]
    fn tui_state_agent_index_found() {
        let config = crate::orchestrator::DebateConfig {
            team_name: "test".to_string(),
            agents: vec![
                AgentConfig {
                    name: "alice".to_string(),
                    provider: "test".to_string(),
                    model: "test".to_string(),
                    system_prompt: String::new(),
                    role: "debater".to_string(),
                },
                AgentConfig {
                    name: "bob".to_string(),
                    provider: "test".to_string(),
                    model: "test".to_string(),
                    system_prompt: String::new(),
                    role: "debater".to_string(),
                },
            ],
            topics: vec!["topic".to_string()],
            visibility: "group".to_string(),
            termination: "fixed".to_string(),
            max_rounds: 5,
            convergence_threshold: 2,
        };
        let state = TuiState::new(&config);
        assert_eq!(state.agent_index("bob"), Some(1));
        assert_eq!(state.agent_index("alice"), Some(0));
        assert_eq!(state.agent_index("unknown"), None);
    }

    #[test]
    fn tui_state_elapsed_str_seconds() {
        let config = crate::orchestrator::DebateConfig {
            team_name: "test".to_string(),
            agents: vec![],
            topics: vec![],
            visibility: "group".to_string(),
            termination: "fixed".to_string(),
            max_rounds: 1,
            convergence_threshold: 2,
        };
        let state = TuiState::new(&config);
        let elapsed = state.elapsed_str();
        assert!(elapsed.contains("s"));
    }

    #[test]
    fn agent_color_wraps() {
        let c0 = agent_color(0);
        let c6 = agent_color(6);
        assert_eq!(c0, c6);
    }
}
