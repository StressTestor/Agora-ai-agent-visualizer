use clap::{Parser, Subcommand};

use crate::config::AppConfig;
use crate::orchestrator::{AgentConfig, DebateConfig, DebateState, DebateStatus};
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
// Clap CLI definition
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(name = "agora", about = "model debate arena")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Run a debate between models
    Debate(DebateArgs),
    /// Show available debate presets
    ListPresets,
    /// List models for a provider
    ListModels {
        /// Provider name (e.g., groq, openrouter)
        provider: String,
    },
}

#[derive(clap::Args)]
pub struct DebateArgs {
    /// Debate name
    #[arg(short, long)]
    pub name: String,

    /// Use a debate preset (e.g., 1v1-duel)
    #[arg(short, long)]
    pub preset: Option<String>,

    /// Agent as name:provider:model[:role] (repeatable)
    #[arg(short, long, num_args = 1)]
    pub agent: Vec<String>,

    /// Judge as name:provider:model (repeatable)
    #[arg(short, long, num_args = 1)]
    pub judge: Vec<String>,

    /// Debate topic (repeatable)
    #[arg(short = 'T', long)]
    pub topic: Vec<String>,

    /// Max rounds (default: 10)
    #[arg(short, long, default_value_t = 10)]
    pub rounds: u32,

    /// Termination mode: fixed|convergence|topic|manual
    #[arg(long, default_value = "convergence")]
    pub termination: String,

    /// Visibility mode: group|directed
    #[arg(long, default_value = "group")]
    pub visibility: String,

    /// Don't write to ~/.claude/teams/
    #[arg(long)]
    pub no_persist: bool,

    /// Plain text output (no TUI)
    #[arg(long)]
    pub plain: bool,
}

// ---------------------------------------------------------------------------
// CLI entry point
// ---------------------------------------------------------------------------

pub fn run_cli() -> Result<(), String> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Debate(args) => cmd_debate(args),
        Commands::ListPresets => cmd_list_presets(),
        Commands::ListModels { provider } => cmd_list_models(&provider),
    }
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

fn cmd_debate(args: DebateArgs) -> Result<(), String> {
    // Parse agent specs from --agent flags
    let mut agents: Vec<AgentConfig> = Vec::new();
    for spec in &args.agent {
        agents.push(parse_agent_spec(spec)?);
    }

    // Parse judge specs from --judge flags (force role to "judge")
    for spec in &args.judge {
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

    let mut rounds = args.rounds;
    let mut termination = args.termination.clone();
    let mut visibility = args.visibility.clone();
    let persist = !args.no_persist;

    // Apply preset defaults if specified
    if let Some(ref preset_name) = args.preset {
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
    if args.topic.is_empty() {
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

    let debate_config = DebateConfig {
        team_name: args.name.clone(),
        agents: agents.clone(),
        topics: args.topic.clone(),
        visibility: visibility.clone(),
        termination: termination.clone(),
        max_rounds: rounds,
        convergence_threshold: 2,
    };

    // TUI mode (default) vs plain text mode
    if !args.plain {
        return crate::tui::run_tui_debate(
            debate_config,
            agents,
            persist,
        );
    }

    // Plain text mode — print header
    println!();
    println!(
        "  {BOLD}{CYAN}▸ {}{RESET}",
        args.name
    );
    println!();
    for (i, agent) in agents.iter().enumerate() {
        let color = agent_color(i);
        println!(
            "    {color}●{RESET} {BOLD}{}{RESET}  {DIM}{} / {}{RESET}  {DIM}[{}]{RESET}",
            agent.name, agent.provider, agent.model, agent.role
        );
    }
    println!();
    for (i, topic) in args.topic.iter().enumerate() {
        println!("    topic {}: {}", i + 1, topic);
    }
    println!(
        "    {DIM}rounds: {} | termination: {} | visibility: {}{RESET}",
        rounds, debate_config.termination, debate_config.visibility
    );
    println!();
    println!("  {DIM}─────────────────────────────────────────{RESET}");
    println!();

    // Build providers
    let providers: Vec<Option<Box<dyn Provider>>> = agents
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
                agents[i].name
            ));
        }
    }

    // Initialize debate state
    let mut state = DebateState::new(debate_config.clone());
    state.status = DebateStatus::Running;
    state.current_round = 1;

    // Persist to disk if requested
    if persist {
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
                            let delay = crate::orchestrator::retry_delay(&e, attempt);
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

        // Build message via shared helper
        let (next_idx, new_round) = crate::orchestrator::next_turn(agent_idx, state.config.agents.len());
        let msg = crate::orchestrator::make_message(&agent_config, &response, &debate_config, agent_idx, state.current_round);

        // Persist
        if persist {
            crate::orchestrator::persist_message(&msg);
        }

        // Update state
        state.messages.push(msg);
        state.current_agent_idx = next_idx;

        if new_round {
            state.current_round += 1;
            println!("  {DIM}── round {} ──{RESET}\n", state.current_round);

            if let Some(topic) = crate::orchestrator::advance_topic(&state.config, state.current_round, state.current_topic_idx) {
                state.current_topic_idx += 1;
                println!("  {MAGENTA}▸ next topic:{RESET} {topic}\n");
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
    if persist {
        println!(
            "  {DIM}saved to: ~/.claude/teams/{}/{RESET}",
            args.name
        );
    }
    println!();

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parse_minimal_debate() {
        let args = Cli::parse_from([
            "agora", "debate",
            "--name", "test",
            "--agent", "a:groq:llama-3.3-70b-versatile:debater",
            "--agent", "b:groq:llama-3.3-70b-versatile:debater",
            "--topic", "test topic",
        ]);
        match args.command {
            Commands::Debate(d) => {
                assert_eq!(d.name, "test");
                assert_eq!(d.agent.len(), 2);
                assert_eq!(d.topic.len(), 1);
                assert_eq!(d.rounds, 10);
            }
            _ => panic!("expected debate command"),
        }
    }

    #[test]
    fn parse_agent_spec_valid_3_parts() {
        let result = parse_agent_spec("model-a:openrouter:meta-llama/llama-3.3-70b-instruct");
        assert!(result.is_ok());
        let agent = result.unwrap();
        assert_eq!(agent.name, "model-a");
        assert_eq!(agent.provider, "openrouter");
        assert_eq!(agent.model, "meta-llama/llama-3.3-70b-instruct");
        assert_eq!(agent.role, "debater");
    }

    #[test]
    fn parse_agent_spec_valid_4_parts() {
        let result = parse_agent_spec("judge-1:groq:llama:judge");
        assert!(result.is_ok());
        assert_eq!(result.unwrap().role, "judge");
    }

    #[test]
    fn parse_agent_spec_invalid_too_few() {
        let result = parse_agent_spec("only-one-part");
        assert!(result.is_err());
    }

    #[test]
    fn parse_with_preset() {
        let args = Cli::parse_from([
            "agora", "debate",
            "--name", "test",
            "--preset", "1v1-duel",
            "--agent", "a:groq:model:debater",
            "--agent", "b:groq:model:debater",
            "--topic", "test",
        ]);
        match args.command {
            Commands::Debate(d) => {
                assert_eq!(d.preset, Some("1v1-duel".to_string()));
            }
            _ => panic!("expected debate command"),
        }
    }

    #[test]
    fn parse_plain_flag() {
        let args = Cli::parse_from([
            "agora", "debate",
            "--name", "test",
            "--agent", "a:groq:model:debater",
            "--agent", "b:groq:model:debater",
            "--topic", "test",
            "--plain",
        ]);
        match args.command {
            Commands::Debate(d) => assert!(d.plain),
            _ => panic!("expected debate command"),
        }
    }

    #[test]
    fn list_presets_command() {
        let args = Cli::parse_from(["agora", "list-presets"]);
        assert!(matches!(args.command, Commands::ListPresets));
    }

    #[test]
    fn list_models_command() {
        let args = Cli::parse_from(["agora", "list-models", "groq"]);
        match args.command {
            Commands::ListModels { provider } => assert_eq!(provider, "groq"),
            _ => panic!("expected list-models command"),
        }
    }
}
