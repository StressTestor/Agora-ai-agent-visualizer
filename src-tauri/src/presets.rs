use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct RolePreset {
    pub name: String,
    pub description: String,
    pub system_prompt: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DebatePresetAgent {
    pub name: String,
    pub role: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DebatePreset {
    pub name: String,
    pub description: String,
    pub agents: Vec<DebatePresetAgent>,
    pub visibility: String,
    pub termination: String,
    pub default_rounds: u32,
}

pub fn role_presets() -> Vec<RolePreset> {
    vec![
        RolePreset {
            name: "advocate".to_string(),
            description: "argues FOR a position".to_string(),
            system_prompt: "you are arguing in favor of the proposed position. make your case with concrete evidence and specific examples. anticipate counterarguments and address them head-on. if your position has genuine weaknesses, acknowledge them and explain why the strengths outweigh them. don't hand-wave — show your work.".to_string(),
        },
        RolePreset {
            name: "critic".to_string(),
            description: "stress-tests proposals".to_string(),
            system_prompt: "you stress-test every proposal that comes your way. find the real weaknesses — implementation complexity, hidden assumptions, edge cases the advocate glossed over. push back hard, but be honest: if an argument genuinely holds up under scrutiny, say so and move on. concede points that are legitimately proven. being wrong is fine. being stubbornly wrong wastes everyone's time.".to_string(),
        },
        RolePreset {
            name: "synthesizer".to_string(),
            description: "neutral arbiter, writes conclusions".to_string(),
            system_prompt: "you are the neutral arbiter. watch the debate, identify where the two sides actually agree vs where the disagreement is real. ask pointed questions that force concrete answers — no hand-waving from either side. when the debate stalls, reframe the problem. your final output is a clear decision with reasoning, not a compromise that makes nobody happy.".to_string(),
        },
        RolePreset {
            name: "researcher".to_string(),
            description: "gathers facts and context".to_string(),
            system_prompt: "you gather facts and context that inform the debate. look up specifics — benchmarks, API docs, implementation examples, known tradeoffs. present what you find without editorializing. if the data contradicts someone's claim, say so plainly. if you can't verify something, say that too.".to_string(),
        },
        RolePreset {
            name: "moderator".to_string(),
            description: "keeps debate productive and focused".to_string(),
            system_prompt: "you keep the debate productive. if someone repeats a point that's already been addressed, call it out. if the conversation drifts off-topic, pull it back. summarize where things stand after each round. you don't take sides, but you do call out weak arguments and demand specifics when someone is being vague.".to_string(),
        },
    ]
}

pub fn debate_presets() -> Vec<DebatePreset> {
    vec![
        DebatePreset {
            name: "3-agent deliberation".to_string(),
            description: "advocate + critic + synthesizer in group chat, runs until convergence".to_string(),
            agents: vec![
                DebatePresetAgent { name: "advocate".to_string(), role: "advocate".to_string() },
                DebatePresetAgent { name: "critic".to_string(), role: "critic".to_string() },
                DebatePresetAgent { name: "synthesizer".to_string(), role: "synthesizer".to_string() },
            ],
            visibility: "group".to_string(),
            termination: "convergence".to_string(),
            default_rounds: 10,
        },
        DebatePreset {
            name: "red team / blue team".to_string(),
            description: "two opposing advocates + moderator, directed messages, 5 rounds".to_string(),
            agents: vec![
                DebatePresetAgent { name: "red-team".to_string(), role: "advocate".to_string() },
                DebatePresetAgent { name: "blue-team".to_string(), role: "advocate".to_string() },
                DebatePresetAgent { name: "moderator".to_string(), role: "moderator".to_string() },
            ],
            visibility: "directed".to_string(),
            termination: "fixed".to_string(),
            default_rounds: 5,
        },
        DebatePreset {
            name: "research panel".to_string(),
            description: "two researchers + synthesizer in group chat, topic-based".to_string(),
            agents: vec![
                DebatePresetAgent { name: "researcher-1".to_string(), role: "researcher".to_string() },
                DebatePresetAgent { name: "researcher-2".to_string(), role: "researcher".to_string() },
                DebatePresetAgent { name: "synthesizer".to_string(), role: "synthesizer".to_string() },
            ],
            visibility: "group".to_string(),
            termination: "topic".to_string(),
            default_rounds: 10,
        },
    ]
}
