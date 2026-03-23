use crate::config::AppConfig;
use crate::orchestrator::{
    AgentConfig, DebateConfig, DebateMessage, DebateState, DebateStatus,
};
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
    AgentThinking { agent: String },
    Chunk { text: String },
    MessageComplete { agent: String, round: u32 },
    RoundAdvance { round: u32 },
    TopicChange { topic: String },
    Error { msg: String },
    Done { status: String, rounds: u32, messages: usize },
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
    is_streaming: bool,       // true once first chunk arrives
    scroll_offset: u16,
    auto_scroll: bool,
    done: bool,
    total_content_height: u16,
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
            total_content_height: 0,
            start_time: now,
            agent_start_time: now,
        }
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
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
            ),
        ];
    }
    if trimmed.starts_with("## ") {
        return vec![
            Span::raw("  "),
            Span::styled(
                trimmed.trim_start_matches('#').trim().to_string(),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
        ];
    }
    if trimmed.starts_with("# ") {
        return vec![
            Span::raw("  "),
            Span::styled(
                trimmed.trim_start_matches('#').trim().to_string(),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            ),
        ];
    }
    // Horizontal rule
    if trimmed == "---" || trimmed == "***" || trimmed == "___" {
        return vec![
            Span::styled("  ─────────", Style::default().fg(Color::DarkGray)),
        ];
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
        .constraints([Constraint::Length(3), Constraint::Min(10), Constraint::Length(3)])
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
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
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

        // Model (truncated to sidebar width)
        let model_display = if agent.model.len() > sidebar_width {
            format!("{}…", &agent.model[..sidebar_width.saturating_sub(1)])
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
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            &state.debate_name,
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
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
        let display = if topic.len() > max_len {
            format!("{}…", &topic[..max_len.saturating_sub(1)])
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
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  ── {}", entry.text),
                    Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD),
                ),
            ]));
            lines.push(Line::from(""));
        } else {
            let color = entry.agent_idx.map(agent_color).unwrap_or(Color::White);

            // Colored separator line before each agent message
            let bar = "─".repeat(chat_width.min(50));
            lines.push(Line::from(vec![
                Span::styled(format!("  {bar}"), Style::default().fg(color).add_modifier(Modifier::DIM)),
            ]));

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
            lines.push(Line::from(vec![
                Span::styled(format!("  {bar}"), Style::default().fg(color).add_modifier(Modifier::DIM)),
            ]));

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
                Span::styled(
                    " ◉",
                    Style::default().fg(color),
                ),
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

    let total_lines = lines.len() as u16;
    state.total_content_height = total_lines;
    let visible_height = inner.height;

    if state.auto_scroll && total_lines > visible_height {
        state.scroll_offset = total_lines.saturating_sub(visible_height);
    }

    let para = Paragraph::new(lines)
        .scroll((state.scroll_offset, 0))
        .wrap(Wrap { trim: false });

    frame.render_widget(para, inner);
}

fn render_status_bar(frame: &mut Frame, state: &TuiState, area: Rect) {
    let msg_count = state.entries.iter().filter(|e| !e.is_system).count();
    let msg_word = if msg_count == 1 { "message" } else { "messages" };

    let left = if !state.status_detail.is_empty() {
        let color = if state.done { Color::DarkGray } else { Color::Yellow };
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
        crate::orchestrator::init_team_on_disk(&config);
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

        let _ = tx.send(DebateEvent::AgentThinking {
            agent: agent_config.name.clone(),
        });

        let response = 'call: {
            let mut last_err = None;
            for attempt in 0..4u32 {
                let tx_ref = &tx;
                let mut on_chunk = |chunk: &str| {
                    let _ = tx_ref.send(DebateEvent::Chunk {
                        text: chunk.to_string(),
                    });
                };
                match provider.chat_streaming(&context, &agent_config.model, &mut on_chunk) {
                    Ok(text) => break 'call text,
                    Err(e) => {
                        let delay = match &e {
                            provider::ProviderError::RateLimit(s) => {
                                s.parse::<u64>().unwrap_or(60).max(60)
                            }
                            _ => 2u64.pow(attempt),
                        };
                        last_err = Some(e);
                        if attempt < 3 {
                            std::thread::sleep(Duration::from_secs(delay));
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

        let _ = tx.send(DebateEvent::MessageComplete {
            agent: agent_config.name.clone(),
            round: state.current_round,
        });

        let agent_count = state.config.agents.len();
        let next_idx = (agent_idx + 1) % agent_count;
        let to_name = if state.config.visibility == "directed" {
            state.config.agents[next_idx].name.clone()
        } else {
            "all".to_string()
        };

        let msg = DebateMessage {
            from: agent_config.name.clone(),
            to: to_name,
            content: response,
            timestamp: crate::orchestrator::now_ms(),
            team: state.config.team_name.clone(),
            role: agent_config.role.clone(),
        };

        if persist {
            crate::orchestrator::persist_message(&msg);
        }

        state.messages.push(msg);
        state.current_agent_idx = next_idx;

        if next_idx == 0 {
            state.current_round += 1;
            let _ = tx.send(DebateEvent::RoundAdvance {
                round: state.current_round,
            });

            if state.config.termination == "topic" && state.current_round % 3 == 0 {
                state.current_topic_idx += 1;
                if state.current_topic_idx < state.config.topics.len() {
                    let topic = state.config.topics[state.current_topic_idx].clone();
                    let topic_msg = DebateMessage {
                        from: "system".to_string(),
                        to: "all".to_string(),
                        content: format!("moving to next topic: {topic}"),
                        timestamp: crate::orchestrator::now_ms(),
                        team: state.config.team_name.clone(),
                        role: "system".to_string(),
                    };
                    if persist {
                        crate::orchestrator::persist_message(&topic_msg);
                    }
                    state.messages.push(topic_msg);
                    let _ = tx.send(DebateEvent::TopicChange { topic });
                }
            }
        }

        for _ in 0..6 {
            if cancel.load(Ordering::Relaxed) { break 'debate; }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    let (status_str, rounds, messages) = match &state.status {
        DebateStatus::Converged => ("converged".to_string(), state.current_round, state.messages.len()),
        DebateStatus::Stopped => ("stopped".to_string(), state.current_round, state.messages.len()),
        DebateStatus::Error(e) => (format!("error: {e}"), state.current_round, state.messages.len()),
        _ => ("done".to_string(), state.current_round, state.messages.len()),
    };

    let _ = tx.send(DebateEvent::Done {
        status: status_str,
        rounds: rounds.saturating_sub(1),
        messages,
    });
}

fn run_tui_loop(config: DebateConfig, rx: mpsc::Receiver<DebateEvent>, cancel: Arc<AtomicBool>) -> Result<(), String> {
    // Install panic hook BEFORE entering raw mode
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Best-effort terminal restore
        let _ = disable_raw_mode();
        let _ = stdout().execute(LeaveAlternateScreen);
        original_hook(info);
    }));

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

    // Discard our panic hook (default is reinstalled automatically)
    let _ = std::panic::take_hook();

    disable_raw_mode().ok();
    stdout().execute(LeaveAlternateScreen).ok();

    // Print summary after TUI exits
    if state.done {
        let msg_count = state.entries.iter().filter(|e| !e.is_system).count();
        eprintln!(
            "\nagora: {} · {} messages · {}\n",
            state.status, msg_count, state.elapsed_str()
        );
    }

    result
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
                Ok(evt) => match evt {
                    DebateEvent::AgentThinking { agent } => {
                        state.current_agent = agent.clone();
                        state.current_agent_idx = state.agent_index(&agent);
                        state.current_streaming.clear();
                        state.is_streaming = false;
                        state.agent_start_time = Instant::now();
                        state.status_detail = format!("{agent} is thinking…");
                        state.status = "running".to_string();
                    }
                    DebateEvent::Chunk { text } => {
                        if !state.is_streaming {
                            state.is_streaming = true;
                            state.status_detail =
                                format!("{} is responding…", state.current_agent);
                        }
                        state.current_streaming.push_str(&text);
                        state.auto_scroll = true;
                    }
                    DebateEvent::MessageComplete { agent, round } => {
                        let agent_idx = state.agent_index(&agent);
                        let duration = state.agent_start_time.elapsed().as_secs_f64();
                        state.entries.push(ChatEntry {
                            agent_idx,
                            agent_name: agent.clone(),
                            round,
                            text: std::mem::take(&mut state.current_streaming),
                            is_system: false,
                            duration_secs: Some(duration),
                        });
                        state.is_streaming = false;
                        state.status_detail.clear();
                        state.auto_scroll = true;
                    }
                    DebateEvent::RoundAdvance { round } => {
                        state.current_round = round;
                        state.entries.push(ChatEntry {
                            agent_idx: None,
                            agent_name: String::new(),
                            round,
                            text: format!("round {round}"),
                            is_system: true,
                            duration_secs: None,
                        });
                    }
                    DebateEvent::TopicChange { topic } => {
                        state.entries.push(ChatEntry {
                            agent_idx: None,
                            agent_name: String::new(),
                            round: state.current_round,
                            text: format!("topic: {topic}"),
                            is_system: true,
                            duration_secs: None,
                        });
                    }
                    DebateEvent::Error { msg } => {
                        state.status = "error".to_string();
                        state.status_detail = msg;
                        state.done = true;
                    }
                    DebateEvent::Done {
                        status,
                        rounds,
                        messages,
                    } => {
                        state.status = status.clone();
                        let round_word = if rounds == 1 { "round" } else { "rounds" };
                        let msg_word = if messages == 1 { "message" } else { "messages" };
                        state.status_detail = format!(
                            "{status} · {rounds} {round_word} · {messages} {msg_word} · {}",
                            state.elapsed_str()
                        );
                        state.done = true;
                        state.current_agent.clear();
                    }
                },
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
                        let visible = terminal.size().map(|s| s.height).unwrap_or(40);
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
                        let visible = terminal.size().map(|s| s.height).unwrap_or(40);
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
