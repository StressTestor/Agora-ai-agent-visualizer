use crate::config::AppConfig;
use crate::orchestrator::{AgentConfig, DebateConfig, DebateMessage, DebateState, DebateStatus};
use crate::presets;
use crate::provider::{self, Provider};
use std::io::Write;

// ---------------------------------------------------------------------------
// ANSI colors
// ---------------------------------------------------------------------------

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const CYAN: &str = "\x1b[36m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RED: &str = "\x1b[31m";
const MAGENTA: &str = "\x1b[35m";
const BLUE: &str = "\x1b[34m";

const AGENT_COLORS: &[&str] = &[CYAN, GREEN, YELLOW, MAGENTA, BLUE, RED];

fn agent_color(idx: usize) -> &'static str {
    AGENT_COLORS[idx % AGENT_COLORS.len()]
}

// ---------------------------------------------------------------------------
// CLI entry point
// ---------------------------------------------------------------------------

pub fn run_cli(args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        return Err(cli_help());
    }

    match args[0].as_str() {
        "debate" => cmd_debate(&args[1..]),
        "list-presets" => cmd_list_presets(),
        "list-models" => {
            if args.len() < 2 {
                return Err("usage: agora list-models <provider>".to_string());
            }
            cmd_list_models(&args[1])
        }
        "help" | "--help" | "-h" => Err(cli_help()),
        other => Err(format!("unknown command: {other}\n\n{}", cli_help())),
    }
}

fn cli_help() -> String {
    format!(
        "{BOLD}agora{RESET} — model debate arena

{BOLD}commands:{RESET}
  debate          run a debate between models
  list-presets    show available debate presets
  list-models     list models for a provider

{BOLD}debate usage:{RESET}
  agora debate [options]

  {DIM}--name <NAME>{RESET}           debate name (required)
  {DIM}--preset <PRESET>{RESET}        use a debate preset (e.g., 1v1-duel)
  {DIM}--agent <SPEC>{RESET}           agent as name:provider:model[:role] (repeatable)
  {DIM}--judge <SPEC>{RESET}           judge as name:provider:model
  {DIM}--topic <TOPIC>{RESET}          debate topic (repeatable)
  {DIM}--rounds <N>{RESET}             max rounds (default: 10)
  {DIM}--termination <MODE>{RESET}     fixed|convergence|topic|manual (default: convergence)
  {DIM}--visibility <MODE>{RESET}      group|directed (default: group)
  {DIM}--no-persist{RESET}             don't write to ~/.claude/teams/

{BOLD}examples:{RESET}
  agora debate \\
    --name rust-vs-go \\
    --agent model-a:openrouter:meta-llama/llama-3.3-70b-instruct:free:debater \\
    --agent model-b:gemini:gemini-2.5-flash:debater \\
    --judge judge:opencode:mimo-v2-pro-free \\
    --topic \"Is Rust better than Go for backend services?\" \\
    --rounds 5

  agora debate \\
    --preset 1v1-duel \\
    --name quick-test \\
    --agent model-a:groq:llama-3.3-70b-versatile:debater \\
    --agent model-b:gemini:gemini-2.5-flash:debater \\
    --judge judge:opencode:mimo-v2-pro-free \\
    --topic \"Tabs vs spaces\"
"
    )
}

// ---------------------------------------------------------------------------
// Parse debate args (hand-rolled to keep it simple + flexible)
// ---------------------------------------------------------------------------

struct DebateArgs {
    name: String,
    _preset: Option<String>,
    agents: Vec<AgentConfig>,
    topics: Vec<String>,
    rounds: u32,
    termination: String,
    visibility: String,
    persist: bool,
    plain: bool,
}

fn parse_debate_args(args: &[String]) -> Result<DebateArgs, String> {
    let mut name = String::new();
    let mut preset: Option<String> = None;
    let mut agents: Vec<AgentConfig> = vec![];
    let mut topics: Vec<String> = vec![];
    let mut rounds: u32 = 10;
    let mut termination = "convergence".to_string();
    let mut visibility = "group".to_string();
    let mut persist = true;
    let mut plain = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--name" | "-n" => {
                i += 1;
                name = args.get(i).cloned().ok_or("--name requires a value")?;
            }
            "--preset" | "-p" => {
                i += 1;
                preset = Some(args.get(i).cloned().ok_or("--preset requires a value")?);
            }
            "--agent" | "-a" => {
                i += 1;
                let spec = args.get(i).ok_or("--agent requires a value")?;
                agents.push(parse_agent_spec(spec)?);
            }
            "--judge" | "-j" => {
                i += 1;
                let spec = args.get(i).ok_or("--judge requires a value")?;
                let mut agent = parse_agent_spec(spec)?;
                agent.role = "judge".to_string();
                // Inject default judge prompt if none
                if agent.system_prompt.is_empty() {
                    if let Some(role) = presets::role_presets().iter().find(|r| r.name == "judge") {
                        agent.system_prompt = role.system_prompt.clone();
                    }
                }
                agents.push(agent);
            }
            "--topic" | "-T" => {
                i += 1;
                topics.push(args.get(i).cloned().ok_or("--topic requires a value")?);
            }
            "--rounds" | "-r" => {
                i += 1;
                rounds = args
                    .get(i)
                    .ok_or("--rounds requires a value")?
                    .parse()
                    .map_err(|_| "--rounds must be a number")?;
            }
            "--termination" => {
                i += 1;
                termination = args.get(i).cloned().ok_or("--termination requires a value")?;
            }
            "--visibility" => {
                i += 1;
                visibility = args.get(i).cloned().ok_or("--visibility requires a value")?;
            }
            "--no-persist" => {
                persist = false;
            }
            "--plain" => {
                plain = true;
            }
            other => {
                return Err(format!("unknown option: {other}"));
            }
        }
        i += 1;
    }

    if name.is_empty() {
        return Err("--name is required".to_string());
    }

    // Apply preset defaults if specified
    if let Some(ref preset_name) = preset {
        let debate_presets = presets::debate_presets();
        let found = debate_presets
            .iter()
            .find(|p| p.name == *preset_name || p.name.replace(' ', "-") == *preset_name)
            .ok_or_else(|| format!("unknown preset: {preset_name}"))?;

        if termination == "convergence" {
            termination = found.termination.clone();
        }
        if rounds == 10 {
            rounds = found.default_rounds;
        }
        visibility = found.visibility.clone();

        // If no agents specified, apply preset agent templates
        if agents.is_empty() {
            for pa in &found.agents {
                let role_preset = presets::role_presets()
                    .into_iter()
                    .find(|r| r.name == pa.role);
                agents.push(AgentConfig {
                    name: pa.name.clone(),
                    provider: String::new(),
                    model: String::new(),
                    system_prompt: role_preset
                        .map(|r| r.system_prompt)
                        .unwrap_or_default(),
                    role: pa.role.clone(),
                });
            }
        }
    }

    // Inject role prompts for agents missing system_prompt
    let role_presets = presets::role_presets();
    for agent in &mut agents {
        if agent.system_prompt.is_empty() && !agent.role.is_empty() {
            if let Some(rp) = role_presets.iter().find(|r| r.name == agent.role) {
                agent.system_prompt = rp.system_prompt.clone();
            }
        }
    }

    if agents.len() < 2 {
        return Err("at least 2 agents required (use --agent and/or --judge)".to_string());
    }
    if topics.is_empty() {
        return Err("at least one --topic is required".to_string());
    }

    // Validate providers
    let config = AppConfig::load();
    for agent in &agents {
        if agent.provider.is_empty() {
            return Err(format!(
                "agent '{}' has no provider. specify as name:provider:model[:role]",
                agent.name
            ));
        }
        if config.api_key(&agent.provider).is_none() {
            eprintln!(
                "{YELLOW}warning:{RESET} no API key for provider '{}' (agent '{}')",
                agent.provider, agent.name
            );
        }
    }

    Ok(DebateArgs {
        name,
        _preset: preset,
        agents,
        topics,
        rounds,
        termination,
        visibility,
        persist,
        plain,
    })
}

fn parse_agent_spec(spec: &str) -> Result<AgentConfig, String> {
    let parts: Vec<&str> = spec.splitn(4, ':').collect();
    if parts.len() < 3 {
        return Err(format!(
            "invalid agent spec: '{spec}'. expected name:provider:model[:role]"
        ));
    }

    let role = if parts.len() >= 4 { parts[3] } else { "debater" };

    // Look up role preset for system prompt
    let role_presets = presets::role_presets();
    let system_prompt = role_presets
        .iter()
        .find(|r| r.name == role)
        .map(|r| r.system_prompt.clone())
        .unwrap_or_default();

    Ok(AgentConfig {
        name: parts[0].to_string(),
        provider: parts[1].to_string(),
        model: parts[2].to_string(),
        system_prompt,
        role: role.to_string(),
    })
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

fn cmd_list_presets() -> Result<(), String> {
    let presets = presets::debate_presets();
    println!("{BOLD}debate presets:{RESET}\n");
    for p in &presets {
        let agents: Vec<&str> = p.agents.iter().map(|a| a.name.as_str()).collect();
        println!(
            "  {CYAN}{}{RESET}  {DIM}({}){RESET}",
            p.name,
            p.category
        );
        println!("    {}", p.description);
        println!(
            "    agents: {}  |  {} {} rounds",
            agents.join(", "),
            p.termination,
            p.default_rounds
        );
        println!();
    }
    Ok(())
}

fn cmd_list_models(provider_name: &str) -> Result<(), String> {
    let config = AppConfig::load();
    let api_key = config
        .api_key(provider_name)
        .ok_or_else(|| format!("no API key configured for '{provider_name}'"))?;
    let p = provider::build_provider(provider_name, &api_key)
        .ok_or_else(|| format!("unknown provider: '{provider_name}'"))?;

    let models = p
        .list_models()
        .map_err(|e| format!("failed to list models: {e}"))?;

    println!("{BOLD}{provider_name}{RESET} models:\n");
    for m in &models {
        println!("  {}", m.id);
    }
    println!("\n  ({} models)", models.len());
    Ok(())
}

fn cmd_debate(args: &[String]) -> Result<(), String> {
    let parsed = parse_debate_args(args)?;

    let debate_config = DebateConfig {
        team_name: parsed.name.clone(),
        agents: parsed.agents.clone(),
        topics: parsed.topics.clone(),
        visibility: parsed.visibility.clone(),
        termination: parsed.termination.clone(),
        max_rounds: parsed.rounds,
        convergence_threshold: 2,
    };

    // TUI mode (default) vs plain text mode
    if !parsed.plain {
        return crate::tui::run_tui_debate(
            debate_config,
            parsed.agents,
            parsed.persist,
        );
    }

    let config = AppConfig::load();

    // Plain text mode — print header
    println!();
    println!(
        "  {BOLD}{CYAN}▸ {}{RESET}",
        parsed.name
    );
    println!();
    for (i, agent) in parsed.agents.iter().enumerate() {
        let color = agent_color(i);
        println!(
            "    {color}●{RESET} {BOLD}{}{RESET}  {DIM}{} / {}{RESET}  {DIM}[{}]{RESET}",
            agent.name, agent.provider, agent.model, agent.role
        );
    }
    println!();
    for (i, topic) in parsed.topics.iter().enumerate() {
        println!("    topic {}: {}", i + 1, topic);
    }
    println!(
        "    {DIM}rounds: {} | termination: {} | visibility: {}{RESET}",
        parsed.rounds, debate_config.termination, debate_config.visibility
    );
    println!();
    println!("  {DIM}─────────────────────────────────────────{RESET}");
    println!();

    // Build providers
    let providers: Vec<Option<Box<dyn Provider>>> = parsed
        .agents
        .iter()
        .map(|agent| {
            let api_key = config.api_key(&agent.provider).unwrap_or_default();
            provider::build_provider(&agent.provider, &api_key)
        })
        .collect();

    // Check all providers exist
    for (i, p) in providers.iter().enumerate() {
        if p.is_none() {
            return Err(format!(
                "no provider configured for agent '{}'",
                parsed.agents[i].name
            ));
        }
    }

    // Initialize debate state
    let mut state = DebateState::new(debate_config.clone());
    state.status = DebateStatus::Running;
    state.current_round = 1;

    // Persist to disk if requested
    if parsed.persist {
        crate::orchestrator::init_team_on_disk(&debate_config);
    }

    // Main debate loop
    'debate: loop {
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
        let color = agent_color(agent_idx);

        // Build context
        let context = crate::orchestrator::build_context(&state, &agent_config);

        let provider = providers[agent_idx].as_ref().unwrap();

        // Print agent header
        print!(
            "  {color}{BOLD}{}{RESET} {DIM}(round {}){RESET}\n  ",
            agent_config.name, state.current_round
        );
        std::io::stdout().flush().unwrap_or(());

        // Call with streaming — print chunks as they arrive
        let response = 'call: {
            let mut last_err = None;
            for attempt in 0..4u32 {
                let mut first_chunk = true;
                let mut on_chunk = |chunk: &str| {
                    if first_chunk {
                        first_chunk = false;
                    }
                    print!("{}", chunk);
                    std::io::stdout().flush().unwrap_or(());
                };
                match provider.chat_streaming(&context, &agent_config.model, &mut on_chunk) {
                    Ok(text) => break 'call text,
                    Err(e) => {
                        if attempt < 3 {
                            let delay = match &e {
                                provider::ProviderError::RateLimit(s) => {
                                    s.parse::<u64>().unwrap_or(60).max(60)
                                }
                                _ => 2u64.pow(attempt),
                            };
                            eprintln!(
                                "\n  {YELLOW}retry {}/{} ({e}), waiting {delay}s...{RESET}",
                                attempt + 1,
                                3
                            );
                            std::thread::sleep(std::time::Duration::from_secs(delay));
                        }
                        last_err = Some(e);
                    }
                }
            }
            let err_msg = format!("agent '{}' failed: {}", agent_config.name, last_err.unwrap());
            state.status = DebateStatus::Error(err_msg.clone());
            eprintln!("\n  {RED}{BOLD}error:{RESET} {err_msg}");
            break 'debate;
        };

        println!("\n");

        // Determine recipient
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

        // Persist
        if parsed.persist {
            crate::orchestrator::persist_message(&msg);
        }

        // Update state
        state.messages.push(msg);
        state.current_agent_idx = next_idx;

        if next_idx == 0 {
            state.current_round += 1;
            println!("  {DIM}── round {} ──{RESET}\n", state.current_round);

            if state.config.termination == "topic" && state.current_round % 3 == 0 {
                state.current_topic_idx += 1;
                if state.current_topic_idx < state.config.topics.len() {
                    let topic = &state.config.topics[state.current_topic_idx];
                    println!("  {MAGENTA}▸ next topic:{RESET} {topic}\n");
                }
            }
        }

        // Brief pause between turns
        std::thread::sleep(std::time::Duration::from_millis(300));
    }

    // Print summary
    println!("  {DIM}─────────────────────────────────────────{RESET}");
    let status_str = match &state.status {
        DebateStatus::Converged => format!("{GREEN}converged{RESET}"),
        DebateStatus::Stopped => format!("{YELLOW}stopped{RESET}"),
        DebateStatus::Error(e) => format!("{RED}error: {e}{RESET}"),
        _ => "done".to_string(),
    };
    println!(
        "  {BOLD}result:{RESET} {status_str} after {} rounds, {} messages",
        state.current_round.saturating_sub(1),
        state.messages.len()
    );
    if parsed.persist {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        println!(
            "  {DIM}saved to: ~/.claude/teams/{}/{RESET}",
            parsed.name
        );
        let _ = home; // suppress warning
    }
    println!();

    Ok(())
}
