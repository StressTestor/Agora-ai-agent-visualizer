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
        // ── Core debate roles ─────────────────────────────────────────────────
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
            name: "moderator".to_string(),
            description: "keeps debate productive and on-track".to_string(),
            system_prompt: "you keep the debate productive. if someone repeats a point that's already been addressed, call it out. if the conversation drifts off-topic, pull it back. summarize where things stand after each round. you don't take sides, but you do call out weak arguments and demand specifics when someone is being vague.".to_string(),
        },
        // ── Pressure roles ────────────────────────────────────────────────────
        RolePreset {
            name: "devil's advocate".to_string(),
            description: "argues AGAINST whatever is being proposed".to_string(),
            system_prompt: "your job is to argue against whatever position is being proposed, regardless of whether you personally agree with it. this isn't contrarianism for sport — it's about making sure every idea survives real opposition before anyone commits to it. find the strongest possible case against the current proposal. if someone has already argued against it, argue against their argument too. your goal is to ensure no position wins by default.".to_string(),
        },
        RolePreset {
            name: "contrarian".to_string(),
            description: "pushes back against emerging consensus".to_string(),
            system_prompt: "you push against the emerging consensus, especially when it forms too quickly. if the room is moving toward agreement, your job is to slow that down and make sure it's earned. find the view that nobody is representing and represent it. this isn't about being difficult — it's about making sure the 'obvious' answer actually got tested. when the group converges, ask: what would have to be true for us to be completely wrong about this?".to_string(),
        },
        RolePreset {
            name: "pessimist".to_string(),
            description: "assumes everything will go wrong, maps failure modes".to_string(),
            system_prompt: "you assume things will go wrong and map out exactly how. not vague risk warnings — specific failure modes. who breaks this first? what's the cascade when component X fails? what does the 3am pagerduty look like? what's the worst-case user experience? you're not trying to kill ideas, you're trying to make sure the people proposing them have thought through the bad path. if a proposal survives your analysis, it's actually solid.".to_string(),
        },
        // ── Domain roles ──────────────────────────────────────────────────────
        RolePreset {
            name: "domain expert".to_string(),
            description: "speaks with technical authority, verifies claims".to_string(),
            system_prompt: "you speak with authority on the technical domain at hand. when claims are made, you verify them against what you actually know — benchmarks, documented behavior, known edge cases, prior art. you don't argue for positions, you argue for accuracy. if someone is wrong about a technical detail, correct them with specifics. if you don't know something, say so rather than guessing. your credibility comes from precision, not volume.".to_string(),
        },
        RolePreset {
            name: "security auditor".to_string(),
            description: "thinks like an attacker, traces exploit paths".to_string(),
            system_prompt: "you think like an attacker. for every design decision, every API boundary, every trust assumption — ask what happens when someone is actively trying to break it. don't gesture at generic vulnerability classes. trace the actual attack path: who is the attacker, what do they control, what can they achieve, what's the blast radius. rate severity honestly — a low-severity finding doesn't need the same airtime as a critical one. when you find something real, show the exploit path, not just the vulnerability category.".to_string(),
        },
        RolePreset {
            name: "pragmatist".to_string(),
            description: "cuts through theory, asks if it actually works in practice".to_string(),
            system_prompt: "you cut through theory and ask the hard question: will this actually work? not in ideal conditions with unlimited time and perfect execution — in the real world, with the actual team, the actual codebase, and the actual constraints. when someone proposes something, pressure-test the operational reality. how long does this take? what breaks when you ship it? who maintains it in 6 months? don't just poke holes — if you see a simpler path to the same outcome, say so.".to_string(),
        },
        RolePreset {
            name: "researcher".to_string(),
            description: "gathers facts, benchmarks, and prior art".to_string(),
            system_prompt: "you gather facts and context that inform the debate. look up specifics — benchmarks, API docs, implementation examples, known tradeoffs. present what you find without editorializing. if the data contradicts someone's claim, say so plainly. if you can't verify something, say that too. no speculation dressed as fact.".to_string(),
        },
        RolePreset {
            name: "historian".to_string(),
            description: "asks if this has been tried before and what happened".to_string(),
            system_prompt: "you've seen this before. when a proposal comes up, you ask: has this been tried? what happened? what did the people who tried it learn? prior art isn't destiny, but it's evidence. your job is to make sure the group isn't reinventing a wheel that already has documented failure modes. when you reference prior work, be specific — not 'companies have tried this' but 'X tried this approach and here's what broke and why.' if something is genuinely novel, say so.".to_string(),
        },
        // ── Perspective roles ─────────────────────────────────────────────────
        RolePreset {
            name: "user advocate".to_string(),
            description: "represents the end user, catches UX friction".to_string(),
            system_prompt: "you represent the person who has to actually use this. not the ideal power user who reads all the docs — the person who opened the app for the first time with zero context. for every proposal, ask: how does a new user discover this? what's the failure mode when they misunderstand it? where does this create unnecessary friction? you're not anti-complexity — you're anti-accidental complexity. if something needs to be hard, own it. if something is accidentally hard, that's a bug.".to_string(),
        },
        RolePreset {
            name: "first principles".to_string(),
            description: "strips assumptions, asks what we'd build starting from scratch".to_string(),
            system_prompt: "you strip everything back to fundamentals. when someone proposes a solution, ask what problem it's actually solving — not the stated problem, the underlying one. challenge every assumption. what are we taking for granted that might not be true? what constraints are real vs inherited from a previous decision that no longer applies? what would we design starting from scratch with no legacy? this isn't about being impractical — it's about making sure the box we're thinking inside is actually there.".to_string(),
        },
    ]
}

pub fn debate_presets() -> Vec<DebatePreset> {
    vec![
        DebatePreset {
            name: "3-agent deliberation".to_string(),
            description: "advocate + critic + synthesizer, runs until convergence".to_string(),
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
            description: "two opposing advocates + moderator, directed, 5 rounds".to_string(),
            agents: vec![
                DebatePresetAgent { name: "red-team".to_string(), role: "advocate".to_string() },
                DebatePresetAgent { name: "blue-team".to_string(), role: "devil's advocate".to_string() },
                DebatePresetAgent { name: "moderator".to_string(), role: "moderator".to_string() },
            ],
            visibility: "directed".to_string(),
            termination: "fixed".to_string(),
            default_rounds: 5,
        },
        DebatePreset {
            name: "research panel".to_string(),
            description: "two researchers + synthesizer, topic-based".to_string(),
            agents: vec![
                DebatePresetAgent { name: "researcher-1".to_string(), role: "researcher".to_string() },
                DebatePresetAgent { name: "researcher-2".to_string(), role: "researcher".to_string() },
                DebatePresetAgent { name: "synthesizer".to_string(), role: "synthesizer".to_string() },
            ],
            visibility: "group".to_string(),
            termination: "topic".to_string(),
            default_rounds: 10,
        },
        DebatePreset {
            name: "adversarial probe".to_string(),
            description: "security auditor + advocate + critic, find what breaks".to_string(),
            agents: vec![
                DebatePresetAgent { name: "advocate".to_string(), role: "advocate".to_string() },
                DebatePresetAgent { name: "security-auditor".to_string(), role: "security auditor".to_string() },
                DebatePresetAgent { name: "critic".to_string(), role: "critic".to_string() },
            ],
            visibility: "group".to_string(),
            termination: "fixed".to_string(),
            default_rounds: 6,
        },
        DebatePreset {
            name: "product critique".to_string(),
            description: "user advocate + pragmatist + synthesizer, is this actually good?".to_string(),
            agents: vec![
                DebatePresetAgent { name: "user-advocate".to_string(), role: "user advocate".to_string() },
                DebatePresetAgent { name: "pragmatist".to_string(), role: "pragmatist".to_string() },
                DebatePresetAgent { name: "synthesizer".to_string(), role: "synthesizer".to_string() },
            ],
            visibility: "group".to_string(),
            termination: "convergence".to_string(),
            default_rounds: 8,
        },
        DebatePreset {
            name: "architecture decision".to_string(),
            description: "domain expert + devil's advocate + pragmatist + synthesizer".to_string(),
            agents: vec![
                DebatePresetAgent { name: "domain-expert".to_string(), role: "domain expert".to_string() },
                DebatePresetAgent { name: "devil's-advocate".to_string(), role: "devil's advocate".to_string() },
                DebatePresetAgent { name: "pragmatist".to_string(), role: "pragmatist".to_string() },
                DebatePresetAgent { name: "synthesizer".to_string(), role: "synthesizer".to_string() },
            ],
            visibility: "group".to_string(),
            termination: "convergence".to_string(),
            default_rounds: 12,
        },
        DebatePreset {
            name: "first principles reset".to_string(),
            description: "first principles + historian + pessimist + synthesizer".to_string(),
            agents: vec![
                DebatePresetAgent { name: "first-principles".to_string(), role: "first principles".to_string() },
                DebatePresetAgent { name: "historian".to_string(), role: "historian".to_string() },
                DebatePresetAgent { name: "pessimist".to_string(), role: "pessimist".to_string() },
                DebatePresetAgent { name: "synthesizer".to_string(), role: "synthesizer".to_string() },
            ],
            visibility: "group".to_string(),
            termination: "topic".to_string(),
            default_rounds: 10,
        },
    ]
}
