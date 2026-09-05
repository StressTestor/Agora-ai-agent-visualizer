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

    /// Agent as name:provider:model[:role]; only a built-in trailing role is removed from the model ID
    #[arg(short, long, num_args = 1)]
    pub agent: Vec<String>,

    /// Judge as name:provider:model (repeatable)
    #[arg(short, long, num_args = 1)]
    pub judge: Vec<String>,

    /// Debate topic (repeatable)
    #[arg(short = 'T', long)]
    pub topic: Vec<String>,

    /// Max rounds (preset default, or 10 when no preset is selected)
    #[arg(short, long, value_parser = clap::value_parser!(u32).range(1..))]
    pub rounds: Option<u32>,

    /// Termination mode (preset default, or convergence)
    #[arg(long, value_parser = ["fixed", "convergence", "topic", "manual"])]
    pub termination: Option<String>,

    /// Visibility mode (preset default, or group)
    #[arg(long, value_parser = ["group", "directed"])]
    pub visibility: Option<String>,

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
    parse_participant_spec(spec, None)
}

fn parse_participant_spec(spec: &str, forced_role: Option<&str>) -> Result<AgentConfig, String> {
    let parts: Vec<&str> = spec.splitn(3, ':').collect();
    if parts.len() < 3 {
        return Err(format!(
            "invalid agent spec: '{spec}'. expected name:provider:model[:role]"
        ));
    }

    let role_presets = presets::role_presets();
    // Model IDs may contain colons (for example an OpenRouter :free suffix).
    // Only a recognized final role disambiguates the optional agent role.
    // --judge has no role suffix: its entire remaining string is the model ID.
    let (model, role) = if let Some(role) = forced_role {
        (parts[2], role)
    } else if let Some((model, role)) = parts[2].rsplit_once(':') {
        if role_presets.iter().any(|preset| preset.name == role) {
            (model, role)
        } else {
            (parts[2], "debater")
        }
    } else {
        (parts[2], "debater")
    };
    if parts[0].trim().is_empty() || parts[1].trim().is_empty() || model.trim().is_empty() {
        return Err("Agent name, provider, and model must all be nonempty.".to_string());
    }

    // Look up the final role, including the forced judge role, for its prompt.
    let system_prompt = role_presets
        .iter()
        .find(|r| r.name == role)
        .map(|r| r.system_prompt.clone())
        .unwrap_or_default();

    Ok(AgentConfig {
        name: parts[0].to_string(),
        provider: parts[1].to_string(),
        model: model.to_string(),
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
        println!("  {CYAN}{}{RESET}  {DIM}({}){RESET}", p.name, p.category);
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

fn model_listing_api_key(config: &AppConfig, provider_name: &str) -> Result<String, String> {
    if provider_name == "claude-code" {
        return Ok(String::new());
    }
    config
        .api_key(provider_name)
        .ok_or_else(|| format!("no API key configured for '{provider_name}'"))
}

fn cmd_list_models(provider_name: &str) -> Result<(), String> {
    let config = AppConfig::load();
    let api_key = model_listing_api_key(&config, provider_name)?;
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

fn build_debate_config(args: &DebateArgs) -> Result<DebateConfig, String> {
    // Parse agent specs from --agent flags
    let mut agents: Vec<AgentConfig> = Vec::new();
    for spec in &args.agent {
        agents.push(parse_agent_spec(spec)?);
    }

    // Parse judge specs from --judge flags (force role to "judge")
    for spec in &args.judge {
        agents.push(parse_participant_spec(spec, Some("judge"))?);
    }

    let mut rounds = args.rounds.unwrap_or(10);
    let mut termination = args
        .termination
        .clone()
        .unwrap_or_else(|| "convergence".into());
    let mut visibility = args.visibility.clone().unwrap_or_else(|| "group".into());

    // Apply preset defaults if specified
    if let Some(ref preset_name) = args.preset {
        let debate_presets = presets::debate_presets();
        let found = debate_presets
            .iter()
            .find(|p| p.name == *preset_name || p.name.replace(' ', "-") == *preset_name)
            .ok_or_else(|| format!("unknown preset: {preset_name}"))?;

        if args.termination.is_none() {
            termination = found.termination.clone();
        }
        if args.rounds.is_none() {
            rounds = found.default_rounds;
        }
        if args.visibility.is_none() {
            visibility = found.visibility.clone();
        }

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
                    system_prompt: role_preset.map(|r| r.system_prompt).unwrap_or_default(),
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
    if args.topic.is_empty() || args.topic.iter().any(|topic| topic.trim().is_empty()) {
        return Err("at least one nonempty --topic is required".to_string());
    }
    if args.name.trim().is_empty() {
        return Err("a nonempty --name is required".to_string());
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
    crate::orchestrator::validate_debate_config(&debate_config)?;
    Ok(debate_config)
}

fn cmd_debate(args: DebateArgs) -> Result<(), String> {
    let debate_config = build_debate_config(&args)?;
    let agents = debate_config.agents.clone();
    let rounds = debate_config.max_rounds;
    let persist = !args.no_persist;

    let config = AppConfig::load();
    for agent in &agents {
        if agent.provider != "claude-code" && config.api_key(&agent.provider).is_none() {
            eprintln!(
                "{YELLOW}warning:{RESET} no API key for provider '{}' (agent '{}')",
                agent.provider, agent.name
            );
        }
    }

    // TUI mode (default) vs plain text mode
    if !args.plain {
        return crate::tui::run_tui_debate(debate_config, agents, persist);
    }

    // Plain text mode — print header
    println!();
    println!("  {BOLD}{CYAN}▸ {}{RESET}", args.name);
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

    run_plain_debate(debate_config, providers, persist)
}

fn run_plain_debate(
    debate_config: DebateConfig,
    providers: Vec<Option<Box<dyn Provider>>>,
    persist: bool,
) -> Result<(), String> {
    // Initialize debate state
    let mut state = DebateState::new(debate_config.clone());
    state.status = DebateStatus::Running;
    state.current_round = 1;

    // Persist to disk if requested
    if persist {
        crate::orchestrator::init_team_on_disk(&debate_config)?;
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
                        if !crate::orchestrator::should_retry(&e, attempt) {
                            last_err = Some(e);
                            break;
                        }
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
            let err_msg = format!(
                "agent '{}' failed: {}",
                agent_config.name,
                last_err.unwrap()
            );
            state.status = DebateStatus::Error(err_msg.clone());
            eprintln!("\n  {RED}{BOLD}error:{RESET} {err_msg}");
            break 'debate;
        };

        // Build message via shared helper
        let (next_idx, new_round) =
            crate::orchestrator::next_turn(agent_idx, state.config.agents.len());
        let msg = crate::orchestrator::make_message(
            &agent_config,
            &response,
            &debate_config,
            agent_idx,
            state.current_round,
        );

        // Persist
        if persist {
            crate::orchestrator::persist_message(&msg)?;
        }
        println!("\n");

        // Update state
        state.messages.push(msg);
        state.current_agent_idx = next_idx;

        if new_round {
            state.current_round += 1;
            println!("  {DIM}── round {} ──{RESET}\n", state.current_round);

            if let Some(topic) = crate::orchestrator::advance_topic(
                &state.config,
                state.current_round,
                state.current_topic_idx,
            ) {
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
            debate_config.team_name
        );
    }
    println!();

    match state.status {
        DebateStatus::Error(error) => Err(error),
        _ => Ok(()),
    }
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
            "agora",
            "debate",
            "--name",
            "test",
            "--agent",
            "a:groq:llama-3.3-70b-versatile:debater",
            "--agent",
            "b:groq:llama-3.3-70b-versatile:debater",
            "--topic",
            "test topic",
        ]);
        match args.command {
            Commands::Debate(d) => {
                assert_eq!(d.name, "test");
                assert_eq!(d.agent.len(), 2);
                assert_eq!(d.topic.len(), 1);
                assert_eq!(d.rounds, None);
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
    fn parse_agent_preserves_colon_model_ids_with_optional_role() {
        let agent = parse_agent_spec("a:openrouter:vendor/model:free").unwrap();
        assert_eq!(agent.model, "vendor/model:free");
        assert_eq!(agent.role, "debater");
        let agent = parse_agent_spec("a:openrouter:vendor/model:free:critic").unwrap();
        assert_eq!(agent.model, "vendor/model:free");
        assert_eq!(agent.role, "critic");
        let agent = parse_agent_spec("a:openrouter:vendor/model:version:free:judge").unwrap();
        assert_eq!(agent.model, "vendor/model:version:free");
        assert_eq!(agent.role, "judge");
    }

    #[test]
    fn parse_judge_preserves_model_and_uses_judge_prompt() {
        let judge_prompt = presets::role_presets()
            .into_iter()
            .find(|role| role.name == "judge")
            .unwrap()
            .system_prompt;
        for model in ["vendor/model", "vendor/model:free", "vendor/model:judge"] {
            let judge =
                parse_participant_spec(&format!("j:openrouter:{model}"), Some("judge")).unwrap();
            assert_eq!(judge.model, model);
            assert_eq!(judge.role, "judge");
            assert_eq!(judge.system_prompt, judge_prompt);
        }
    }

    #[test]
    fn parse_agent_rejects_empty_required_fields() {
        for spec in [":groq:model", "a::model", "a:groq:", "a:groq::judge"] {
            assert!(parse_agent_spec(spec).is_err(), "accepted {spec}");
        }
        // A bare model named "judge" is valid; only its explicit suffix is a role.
        assert!(parse_agent_spec("a:groq:judge").is_ok());
    }

    fn valid_debate_args() -> DebateArgs {
        let cli = Cli::try_parse_from([
            "agora",
            "debate",
            "--name",
            "test",
            "--agent",
            "a:groq:model",
            "--judge",
            "j:groq:model",
            "--topic",
            "test topic",
        ])
        .unwrap();
        match cli.command {
            Commands::Debate(args) => args,
            _ => unreachable!(),
        }
    }

    #[test]
    fn debate_validation_runs_before_provider_or_tui_startup() {
        let valid = build_debate_config(&valid_debate_args()).unwrap();
        assert_eq!(valid.agents[1].role, "judge");
        assert_ne!(valid.agents[0].system_prompt, valid.agents[1].system_prompt);

        let mut args = valid_debate_args();
        args.judge = vec!["a:groq:model".into()];
        assert!(build_debate_config(&args)
            .unwrap_err()
            .contains("unique name"));
        let mut args = valid_debate_args();
        args.agent = vec!["a:groq: ".into()];
        assert!(build_debate_config(&args).is_err());
        let mut args = valid_debate_args();
        args.topic = vec![" ".into()];
        assert!(build_debate_config(&args).is_err());
        let mut args = valid_debate_args();
        args.name = " ".into();
        assert!(build_debate_config(&args).is_err());
    }

    #[test]
    fn cli_rejects_invalid_modes_and_zero_rounds() {
        for (flag, value) in [
            ("--termination", "typo"),
            ("--visibility", "typo"),
            ("--rounds", "0"),
        ] {
            assert!(
                Cli::try_parse_from(["agora", "debate", "--name", "test", flag, value]).is_err()
            );
        }
    }

    #[test]
    fn parse_with_preset() {
        let args = Cli::parse_from([
            "agora",
            "debate",
            "--name",
            "test",
            "--preset",
            "1v1-duel",
            "--agent",
            "a:groq:model:debater",
            "--agent",
            "b:groq:model:debater",
            "--topic",
            "test",
        ]);
        match args.command {
            Commands::Debate(d) => {
                assert_eq!(d.preset, Some("1v1-duel".to_string()));
            }
            _ => panic!("expected debate command"),
        }
    }

    #[test]
    fn explicit_flags_override_presets_even_when_equal_to_general_defaults() {
        let mut args = valid_debate_args();
        let defaults = build_debate_config(&args).unwrap();
        assert_eq!(defaults.max_rounds, 10);
        args.preset = Some("1v1-duel".into());
        let preset = build_debate_config(&args).unwrap();
        assert_eq!(preset.max_rounds, 5);
        assert_eq!(preset.termination, "fixed");
        args.rounds = Some(10);
        args.termination = Some("convergence".into());
        args.visibility = Some("directed".into());
        let explicit = build_debate_config(&args).unwrap();
        assert_eq!(explicit.max_rounds, 10);
        assert_eq!(explicit.termination, "convergence");
        assert_eq!(explicit.visibility, "directed");
    }

    #[test]
    fn plain_provider_failure_returns_an_error_without_retrying_authentication() {
        struct Rejected(std::sync::Arc<std::sync::atomic::AtomicUsize>);
        impl Provider for Rejected {
            fn chat(
                &self,
                _: &[crate::provider::ChatMessage],
                _: &str,
            ) -> Result<String, crate::provider::ProviderError> {
                self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Err(crate::provider::ProviderError::Auth(
                    "synthetic rejection".into(),
                ))
            }
            fn list_models(
                &self,
            ) -> Result<Vec<crate::provider::ModelInfo>, crate::provider::ProviderError>
            {
                Ok(vec![])
            }
        }
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let config = build_debate_config(&valid_debate_args()).unwrap();
        let providers = config
            .agents
            .iter()
            .map(|_| Some(Box::new(Rejected(calls.clone())) as Box<dyn Provider>))
            .collect();
        let result = run_plain_debate(config, providers, false);
        assert!(result.unwrap_err().contains("synthetic rejection"));
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[test]
    fn parse_plain_flag() {
        let args = Cli::parse_from([
            "agora",
            "debate",
            "--name",
            "test",
            "--agent",
            "a:groq:model:debater",
            "--agent",
            "b:groq:model:debater",
            "--topic",
            "test",
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

    #[test]
    fn model_listing_allows_keyless_claude_code_but_keeps_api_provider_requirements() {
        let mut config = AppConfig::default();
        assert_eq!(model_listing_api_key(&config, "claude-code").unwrap(), "");
        assert!(model_listing_api_key(&config, "openai").is_err());
        config.providers.insert(
            "openai".into(),
            crate::config::ProviderConfig {
                api_key: "synthetic-test-key".into(),
                enabled: true,
            },
        );
        assert_eq!(
            model_listing_api_key(&config, "openai").unwrap(),
            "synthetic-test-key"
        );
        config.providers.get_mut("openai").unwrap().enabled = false;
        assert!(model_listing_api_key(&config, "openai").is_err());
    }
}
