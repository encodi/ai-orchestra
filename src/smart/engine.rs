//! The smart strategy engine: from the question, the routing result, the user's explicit settings,
//! the budgets and the provider history to an execution plan.
//!
//! It runs after the Milestone 4 router and only changes what the router chose *automatically*.
//! A setting the user made (`--mode`, `--coordinator`, `--agents`, `--rounds`, `--depth`,
//! `--max-sources`, `--web`) is never altered and never silently dropped: where a strategy would
//! have chosen differently, the explanation says so. The engine makes no model call; the same
//! inputs always give the same plan.
//!
//! Precedence (highest first): safety restrictions (the host agent is never called, the depth
//! limit, read-only agents); explicit user settings; explicit budgets; the strategy's defaults;
//! the provider history (which only orders agents and never excludes one).

use std::time::Duration;

use super::budget::BudgetLimits;
use super::profiles::{Health, ProfileSnapshot};
use super::strategy::{Flow, QuestionFeatures, SmartPlan, Strategy};
use crate::config::{Config, MAX_DEBATE_ROUNDS};
use crate::consult::{
    ConsultMode, MAX_PARTICIPANTS, RouteInput, Routed, RoutingDecision, Source, cue_matches,
    is_contested_question, normalize_question,
};
use crate::debate::Skipped;
use crate::error::{OrchestraError, Result};
use crate::research::plan::{Depth, ResearchDecision, ResearchInput, WebMode, decide_research};
use crate::types::ProviderId;

/// Defaults and ceilings from the configuration (`[smart]` and the Milestone 1-5 settings).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmartSettings {
    pub max_agent_calls: Option<usize>,
    pub max_duration_secs: Option<u64>,
    pub max_parallel: Option<usize>,
    pub max_response_chars: u64,
    /// The longest any consultation may run at all (`debate.total_timeout_secs`): a per-request
    /// `--max-duration` can lower it but never raise it, so there is one clock and one ceiling.
    pub hard_duration_secs: u64,
}

impl SmartSettings {
    pub fn from_config(config: &Config) -> Self {
        let s = &config.smart;
        Self {
            max_agent_calls: s.max_agent_calls,
            max_duration_secs: s.max_duration_secs,
            max_parallel: s.max_parallel,
            max_response_chars: s.max_response_chars,
            hard_duration_secs: config.debate.total_timeout_secs,
        }
    }
}

impl Default for SmartSettings {
    fn default() -> Self {
        Self::from_config(&Config::default())
    }
}

/// What this request asked for.
#[derive(Debug, Clone, Default)]
pub struct StrategyRequest {
    /// `None`: no strategy (the fixed Milestone 4/5 orchestration), only the limits apply.
    pub strategy: Option<Strategy>,
    pub max_agent_calls: Option<usize>,
    pub max_duration: Option<Duration>,
    pub max_parallel: Option<usize>,
    /// The text is one claim to verify, not a question to answer.
    pub claim_check: bool,
}

/// Words that mark a question as touching something with lasting consequences. Wording only.
const HIGH_STAKES_CUES: &[&str] = &[
    "medical",
    "medicine",
    "medication*",
    "diagnos*",
    "surgery",
    "dose*",
    "dosage",
    "legal",
    "lawsuit*",
    "contract*",
    "mortgage*",
    "loan*",
    "tax",
    "taxes",
    "insurance",
    "retire*",
    "safety",
    "dangerous",
    "salud",
    "hipoteca*",
    "impuesto*",
];

/// Longest question (in words) still counted as short, and the size from which it is "complex".
const COMPLEX_WORDS: usize = 45;

pub fn question_features(question: &str) -> QuestionFeatures {
    let normalized = normalize_question(question);
    let words = question.split_whitespace().count();
    let contested = is_contested_question(question, &normalized).is_some();
    let questions = question.matches('?').count();
    let complex = words >= COMPLEX_WORDS || questions >= 3 || question.len() > 600;
    let high_stakes = HIGH_STAKES_CUES.iter().any(|c| cue_matches(&normalized, c));
    QuestionFeatures {
        words,
        simple: !contested && !complex && words <= 20 && !question.contains('\n'),
        contested,
        complex,
        high_stakes,
    }
}

/// Calls a plan makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Calls {
    typical: usize,
    max: usize,
    /// What must run for the result to be what was asked for (the rest is best effort).
    required: usize,
}

/// A candidate shape for the plan. The degrade loop lowers the automatic parts one step at a
/// time until the plan fits the call limit.
#[derive(Debug, Clone)]
struct Shape {
    flow: Flow,
    /// Participants, lead first.
    agents: Vec<ProviderId>,
    rounds: u8,
    depth: Option<Depth>,
    research_on: bool,
}

struct Context<'a> {
    input: &'a RouteInput<'a>,
    decision: &'a RoutingDecision,
    features: &'a QuestionFeatures,
    strategy: Strategy,
    manual_agents: bool,
    manual_rounds: bool,
    manual_depth: bool,
    /// Candidates in the order they should be used, lead first.
    ordered: Vec<ProviderId>,
    claim_check: bool,
}

pub struct SmartStrategyEngine {
    settings: SmartSettings,
    profiles: ProfileSnapshot,
}

fn conflict(msg: String) -> OrchestraError {
    OrchestraError::Config(msg)
}

impl SmartStrategyEngine {
    pub fn new(settings: SmartSettings, profiles: ProfileSnapshot) -> Self {
        Self { settings, profiles }
    }

    /// The limits that apply to this request: its own, else the configured ones.
    fn limits(&self, req: &StrategyRequest, call_timeout_secs: u64) -> Result<BudgetLimits> {
        if req.max_agent_calls == Some(0) {
            return Err(conflict("--max-agent-calls must be at least 1".into()));
        }
        if req.max_parallel == Some(0) {
            return Err(conflict("--max-parallel must be at least 1".into()));
        }
        if req.max_duration == Some(Duration::ZERO) {
            return Err(conflict("--max-duration must be at least 1 second".into()));
        }
        let duration = req
            .max_duration
            .map(|d| d.as_secs().max(1))
            .or(self.settings.max_duration_secs);
        if let Some(d) = duration
            && d > self.settings.hard_duration_secs
        {
            return Err(conflict(format!(
                "a duration limit of {d} s exceeds the configured ceiling of {} s (debate.total_timeout_secs): raise the ceiling in the configuration or ask for less",
                self.settings.hard_duration_secs
            )));
        }
        Ok(BudgetLimits {
            max_agent_calls: req.max_agent_calls.or(self.settings.max_agent_calls),
            max_duration_secs: duration,
            max_parallel: req.max_parallel.or(self.settings.max_parallel),
            call_timeout_secs: Some(call_timeout_secs),
            context_tokens: None,
            max_response_chars: Some(self.settings.max_response_chars),
        })
    }

    /// Applies the request to the router's decision. Errors are conflicts between explicit
    /// settings, reported before anything runs.
    pub fn apply(
        &self,
        input: &RouteInput<'_>,
        mut decision: RoutingDecision,
        req: &StrategyRequest,
        call_timeout_secs: u64,
    ) -> Result<RoutingDecision> {
        let limits = self.limits(req, call_timeout_secs)?;
        let features = question_features(input.question);
        let mut explanation: Vec<String> = Vec::new();
        let mut profile_notes: Vec<String> = Vec::new();
        let cap = limits.max_agent_calls;

        let Some(requested) = req.strategy else {
            // No strategy: the router's decision stands; only the limits are checked.
            let shape = self.router_shape(&decision);
            let calls = self.calls(&shape, &decision, req.claim_check, &features);
            if let Some(c) = cap
                && calls.required > c
            {
                return Err(conflict(format!(
                    "the consultation needs {} agent call(s) and --max-agent-calls is {c}; raise the limit, name fewer agents or rounds, or choose a strategy to fit it",
                    calls.required
                )));
            }
            explanation.push(
                "no strategy is in force: the fixed panel workflow runs and only the limits apply"
                    .to_string(),
            );
            decision.smart = Some(SmartPlan {
                strategy: None,
                flow: shape.flow,
                escalation_order: Vec::new(),
                limits,
                max_calls: calls.max,
                typical_calls: calls.typical,
                features,
                explanation,
                profile_notes,
            });
            return Ok(decision);
        };

        let usable: Vec<ProviderId> = input
            .available
            .iter()
            .filter(|a| a.usable.is_ok())
            .map(|a| a.id.clone())
            .collect();
        let manual_agents = decision.participants.source == Source::Manual;
        let manual_rounds = input.rounds.is_some();
        let manual_depth = input.research.depth.is_some();

        // ---- which strategy
        let mode = decision.mode.value;
        let (strategy, strategy_reason, strategy_source) = if requested == Strategy::Auto {
            let (s, why) = self.resolve_auto(
                &decision,
                &features,
                usable.len(),
                manual_agents,
                manual_rounds,
                req.claim_check,
                &limits,
            );
            (s, why, Source::Auto)
        } else {
            (
                requested,
                format!("requested with --strategy {requested}"),
                Source::Manual,
            )
        };

        // ---- the agents, in the order they will be used
        let pool: Vec<ProviderId> = if manual_agents {
            decision.participants.value.clone()
        } else {
            usable.iter().take(MAX_PARTICIPANTS).cloned().collect()
        };
        let (ordered, notes, lead_changed) = self.order_pool(&pool, &decision);
        profile_notes.extend(notes);
        if self.profiles.is_empty() {
            profile_notes
                .push("no provider history yet: agents are used in registry order".to_string());
        }
        let cx = Context {
            input,
            decision: &decision,
            features: &features,
            strategy,
            manual_agents,
            manual_rounds,
            manual_depth,
            ordered,
            claim_check: req.claim_check,
        };

        // ---- the shape, lowered step by step until it fits the call limit
        let mut shape = self.initial_shape(&cx, &mut explanation)?;
        let mut calls = self.calls_in(&cx, &shape);
        if let Some(c) = cap {
            while calls.required > c {
                match self.degrade(&cx, &mut shape, &mut explanation) {
                    true => calls = self.calls_in(&cx, &shape),
                    false => {
                        return Err(conflict(format!(
                            "the settings you made need at least {} agent call(s) but --max-agent-calls is {c}. {} Raise the limit or relax one of those settings",
                            calls.required,
                            self.forced_by(&cx, &shape)
                        )));
                    }
                }
            }
        }

        // ---- write the shape into the decision
        let research = self.research_for(&cx, &shape)?;
        let n = shape.agents.len();
        if lead_changed && decision.coordinator.source == Source::Auto {
            decision.coordinator = Routed::auto(
                shape.agents[0].clone(),
                "the configured coordinator has failed repeatedly in recent calls, so a healthier agent leads this time",
            );
        }
        decision.participants = if manual_agents {
            Routed::manual(shape.agents.clone(), "requested with --agents")
        } else {
            Routed::auto(
                shape.agents.clone(),
                participants_reason(strategy, shape.flow, n, mode),
            )
        };
        let router_rounds = decision.rounds.clone();
        decision.rounds = if manual_rounds && n > 1 {
            Routed::manual(shape.rounds, "requested with --rounds")
        } else if n <= 1 {
            Routed::auto(0, "a single participant has no reviewer")
        } else {
            Routed::auto(
                shape.rounds,
                rounds_reason(strategy, shape.flow, shape.rounds),
            )
        };
        if manual_rounds && n <= 1 {
            decision.rounds = router_rounds;
        }
        decision.research = research;
        // Rebuild the list of agents that are not used: unavailable ones stay with the router's
        // reason; usable ones the strategy left out are reported with its reason.
        let mut skipped: Vec<Skipped> = decision
            .skipped
            .iter()
            .filter(|s| {
                input
                    .available
                    .iter()
                    .any(|a| a.id == s.provider && a.usable.is_err())
            })
            .cloned()
            .collect();
        for id in &usable {
            if !shape.agents.contains(id) && !skipped.iter().any(|s| &s.provider == id) {
                skipped.push(Skipped {
                    provider: id.clone(),
                    reason: format!(
                        "not needed by the {strategy} strategy for this question (name it in --agents, or use --strategy thorough, to include it)"
                    ),
                });
            }
        }
        decision.skipped = skipped;
        decision
            .warnings
            .retain(|w| !w.starts_with("single-agent consultation"));
        if n == 1 && mode != ConsultMode::Debate {
            decision.warnings.push(
                "single-agent consultation: the answer has had no independent review, so errors the model makes alone will not be caught (use --strategy balanced or thorough, or --agents, for a review)"
                    .to_string(),
            );
        }
        if matches!(strategy, Strategy::Fast) && decision.research.is_active() {
            explanation.push(format!(
                "fast keeps the evidence check: research runs at {} depth with {} source(s)",
                decision.research.depth.value.as_str(),
                decision.research.max_sources.value
            ));
        }
        let manual: Vec<String> = decision.manual_settings();
        if !manual.is_empty() {
            explanation.push(format!(
                "kept as you set them: {} (the strategy only fills in the rest)",
                manual.join(", ")
            ));
        }
        for h in &profile_notes {
            let _ = h;
        }
        let escalation: Vec<ProviderId> = if shape.flow == Flow::Adaptive {
            shape.agents.iter().skip(1).cloned().collect()
        } else {
            Vec::new()
        };
        decision
            .signals
            .push(format!("strategy: {strategy} ({strategy_reason})"));
        decision.smart = Some(SmartPlan {
            strategy: Some(Routed {
                value: strategy,
                source: strategy_source,
                reason: strategy_reason,
            }),
            flow: shape.flow,
            escalation_order: escalation,
            limits,
            max_calls: calls.max,
            typical_calls: calls.typical,
            features,
            explanation,
            profile_notes,
        });
        Ok(decision)
    }

    // ------------------------------------------------------------------ strategy choice

    #[allow(clippy::too_many_arguments)]
    fn resolve_auto(
        &self,
        decision: &RoutingDecision,
        f: &QuestionFeatures,
        usable: usize,
        manual_agents: bool,
        manual_rounds: bool,
        claim_check: bool,
        limits: &BudgetLimits,
    ) -> (Strategy, String) {
        let mode = decision.mode.value;
        let agents = decision.participants.value.len();
        if usable <= 1 {
            return (
                Strategy::Fast,
                "only one agent can take part, so there is nobody to consult or review".to_string(),
            );
        }
        if mode == ConsultMode::Debate {
            return (
                Strategy::Thorough,
                "a debate is deliberation by design: several agents and review rounds".to_string(),
            );
        }
        if let Some(c) = limits.max_agent_calls
            && c <= 3
        {
            return (
                Strategy::Fast,
                format!("the limit of {c} agent call(s) leaves no room for a review"),
            );
        }
        if let Some(s) = limits.max_duration_secs
            && s < 120
        {
            return (
                Strategy::Fast,
                format!("the {s} s time limit leaves no room for several sequential calls"),
            );
        }
        if claim_check {
            return (
                Strategy::Balanced,
                "a claim check judges the claim against retrieved sources; one or two judges are enough".to_string(),
            );
        }
        if manual_agents || manual_rounds {
            let rounds2 = decision.rounds.value >= 2;
            return if agents >= 3 || rounds2 {
                (
                    Strategy::Thorough,
                    "the agents or rounds you chose define a full panel".to_string(),
                )
            } else {
                (
                    Strategy::Balanced,
                    "the agents or rounds you chose define a small panel".to_string(),
                )
            };
        }
        if decision.research.is_active() && decision.research.depth.value == Depth::Deep {
            return (
                Strategy::Thorough,
                "the research is deep (the question asks to verify or contrast claims)".to_string(),
            );
        }
        if f.simple && !f.high_stakes && matches!(mode, ConsultMode::Learn | ConsultMode::Research)
        {
            return (
                Strategy::Fast,
                "a short, uncontested question: one agent is enough (the evidence check still runs if it needs outside facts)".to_string(),
            );
        }
        if f.complex {
            return (
                Strategy::Thorough,
                "a long or multi-part question benefits from independent perspectives and cross-review".to_string(),
            );
        }
        (
            Strategy::Balanced,
            "one agent answers and an independent review is added when the question or the evidence calls for it".to_string(),
        )
    }

    // ----------------------------------------------------------------- agents and history

    /// The pool ordered lead first. The lead is the router's coordinator, unless it was chosen
    /// automatically and has failed repeatedly in recent calls while another agent has not. The
    /// others are ordered healthy first, then by typical answer latency when the history has
    /// enough recent calls, then in registry order. History only orders; it never excludes.
    fn order_pool(
        &self,
        pool: &[ProviderId],
        decision: &RoutingDecision,
    ) -> (Vec<ProviderId>, Vec<String>, bool) {
        let mut notes = Vec::new();
        let configured = decision.coordinator.value.clone();
        let view = |id: &ProviderId| self.profiles.view(id.as_str());
        let degraded = |id: &ProviderId| matches!(view(id).health, Health::Degraded(_));
        let mut lead = configured.clone();
        let mut changed = false;
        if decision.coordinator.source == Source::Auto
            && degraded(&configured)
            && let Some(healthy) = pool.iter().find(|p| !degraded(p))
        {
            lead = healthy.clone();
            changed = true;
            notes.push(view(&configured).note);
        }
        let mut rest: Vec<(usize, &ProviderId)> = pool
            .iter()
            .enumerate()
            .filter(|(_, p)| **p != lead)
            .collect();
        rest.sort_by_key(|(i, p)| {
            let v = view(p);
            (
                degraded(p),
                v.answer_latency_ms.is_none(),
                v.answer_latency_ms.unwrap_or(0),
                *i,
            )
        });
        for (_, p) in &rest {
            let v = view(p);
            if v.answer_latency_ms.is_some() || degraded(p) {
                notes.push(v.note);
            }
        }
        let mut ordered = vec![lead];
        ordered.extend(rest.into_iter().map(|(_, p)| p.clone()));
        // The lead must be one of the pool (a manual list may leave the configured one out).
        if !pool.contains(&ordered[0]) {
            ordered[0] = pool[0].clone();
        }
        (ordered, notes, changed)
    }

    // ---------------------------------------------------------------------- the shape

    fn router_shape(&self, d: &RoutingDecision) -> Shape {
        let n = d.participants.value.len();
        let flow = if d.mode.value == ConsultMode::Debate {
            Flow::Debate
        } else if n <= 1 {
            Flow::Single
        } else {
            Flow::Panel
        };
        Shape {
            flow,
            agents: d.participants.value.clone(),
            rounds: d.rounds.value,
            depth: Some(d.research.depth.value),
            research_on: d.research.is_active(),
        }
    }

    fn initial_shape(&self, cx: &Context<'_>, why: &mut Vec<String>) -> Result<Shape> {
        let mode = cx.decision.mode.value;
        let pool = cx.ordered.len();
        let strategy = cx.strategy;
        // How many agents the strategy wants, before anything explicit is applied.
        let wanted = match strategy {
            Strategy::Fast | Strategy::Auto => 1,
            Strategy::Balanced => 3.min(pool),
            Strategy::Thorough => pool,
        };
        let manual_review = cx.manual_rounds && cx.input.rounds.is_some_and(|r| r >= 1);
        let mut n = if cx.manual_agents {
            pool
        } else {
            wanted.max(1)
        };
        if !cx.manual_agents && manual_review && pool >= 2 {
            let target = if strategy == Strategy::Thorough {
                pool
            } else {
                2
            };
            if n < target {
                why.push(format!(
                    "you asked for {} review round(s), which needs at least two agents, so {target} take part",
                    cx.input.rounds.unwrap_or(1)
                ));
                n = target;
            }
        }
        if mode == ConsultMode::Debate && n < 2 && pool >= 2 {
            why.push("a debate needs at least two agents".to_string());
            n = 2;
        }
        if cx.claim_check {
            // A claim check has no panel: the agents only judge passages.
            let verifiers = cx
                .decision
                .research
                .depth
                .value
                .profile()
                .verifier_calls
                .max(1);
            n = if cx.manual_agents {
                pool
            } else {
                verifiers.min(pool).max(1)
            };
        }
        let mut agents: Vec<ProviderId> = cx.ordered.iter().take(n).cloned().collect();
        // Research needs someone to search: bring the best search-capable agent in if needed.
        let research = &cx.decision.research;
        if research.is_active() {
            let can = |id: &ProviderId| {
                cx.input
                    .available
                    .iter()
                    .any(|a| &a.id == id && a.can_search)
            };
            let search_wanted =
                cx.input.research.web == WebMode::On || cx.input.research.urls.is_empty();
            if search_wanted && !agents.iter().any(can) {
                let lead_is_manual = cx.decision.coordinator.source == Source::Manual;
                if let Some(searcher) = cx.ordered.iter().find(|p| can(p)).cloned() {
                    if agents.len() == 1 && !lead_is_manual {
                        why.push(format!(
                            "{searcher} leads instead because the question needs the web and only agents that can search the web can find sources"
                        ));
                        agents[0] = searcher;
                    } else if agents.len() > 1 {
                        let last = agents.len() - 1;
                        why.push(format!(
                            "{searcher} takes part because it can search the web"
                        ));
                        agents[last] = searcher;
                    } else {
                        why.push(format!(
                            "{searcher} also takes part because it can search the web"
                        ));
                        agents.push(searcher);
                    }
                }
            }
        }
        let flow = if mode == ConsultMode::Debate {
            Flow::Debate
        } else if cx.claim_check || agents.len() == 1 {
            Flow::Single
        } else if cx.manual_agents || manual_review {
            Flow::Panel
        } else {
            match strategy {
                Strategy::Balanced => Flow::Adaptive,
                Strategy::Thorough => Flow::Panel,
                Strategy::Fast | Strategy::Auto => Flow::Single,
            }
        };
        let rounds = if flow == Flow::Single {
            0
        } else if cx.manual_rounds {
            cx.decision.rounds.value
        } else {
            match (strategy, flow) {
                (Strategy::Fast | Strategy::Auto, Flow::Debate) => 1,
                (Strategy::Fast | Strategy::Auto, _) => 0,
                (Strategy::Balanced, _) => 1,
                (Strategy::Thorough, _) => MAX_DEBATE_ROUNDS,
            }
        };
        let depth = self.strategy_depth(cx);
        if let Some(d) = depth
            && !cx.manual_depth
            && cx.decision.research.is_active()
            && Some(d) != Some(cx.decision.research.depth.value)
        {
            why.push(format!(
                "research depth is {} instead of {}: {}",
                d.as_str(),
                cx.decision.research.depth.value.as_str(),
                depth_reason(strategy)
            ));
        }
        if flow == Flow::Single {
            agents.truncate(if cx.claim_check { agents.len() } else { 1 });
        }
        Ok(Shape {
            flow,
            agents,
            rounds,
            depth,
            research_on: research.is_active(),
        })
    }

    /// The research depth the strategy wants when the user did not choose one.
    fn strategy_depth(&self, cx: &Context<'_>) -> Option<Depth> {
        if !cx.decision.research.is_active() || cx.manual_depth {
            return None;
        }
        let router = cx.decision.research.depth.value;
        Some(match cx.strategy {
            // Quick depth finds passages but never judges them; fast and balanced keep the judge.
            Strategy::Fast | Strategy::Balanced | Strategy::Auto => match router {
                Depth::Quick => Depth::Standard,
                d => d,
            },
            Strategy::Thorough => Depth::Deep,
        })
    }

    /// The research decision for this shape, re-made with the final participants.
    fn research_for(&self, cx: &Context<'_>, shape: &Shape) -> Result<ResearchDecision> {
        if cx.decision.mode.value == ConsultMode::Debate {
            return Ok(cx.decision.research.clone());
        }
        if !cx.decision.research.is_active() {
            // Research was never part of this consultation: nothing to change or to disclose.
            return Ok(cx.decision.research.clone());
        }
        if !shape.research_on {
            let mut d = ResearchDecision::inactive(
                cx.input.research.web,
                "research was skipped to fit the call limit",
            );
            d.active = Routed::auto(false, "skipped to fit --max-agent-calls");
            d.limitations.push(
                "web research was skipped to fit the agent-call limit: nothing in this answer was checked against external sources".to_string(),
            );
            return Ok(d);
        }
        let candidates: Vec<(ProviderId, bool)> = shape
            .agents
            .iter()
            .map(|id| {
                (
                    id.clone(),
                    cx.input
                        .available
                        .iter()
                        .any(|a| &a.id == id && a.can_search),
                )
            })
            .collect();
        let mut request = cx.input.research.clone();
        if request.depth.is_none() {
            request.depth = shape.depth;
        }
        let mut d = decide_research(&ResearchInput {
            question: cx.input.question,
            mode: cx.decision.mode.value,
            brief: cx.decision.brief.value,
            request: &request,
            participants: &candidates,
            policy: &cx.input.fetch_policy,
        })
        .map_err(|e| match e {
            OrchestraError::Config(m) => OrchestraError::Config(format!(
                "{m}. The {} strategy chose {} for this question; name a search-capable agent with --agents or use --strategy thorough",
                cx.strategy,
                shape.agents.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")
            )),
            other => other,
        })?;
        if cx.input.research.depth.is_none()
            && d.is_active()
            && let Some(depth) = shape.depth
        {
            d.depth = Routed::auto(depth, depth_reason(cx.strategy));
        }
        Ok(d)
    }

    // -------------------------------------------------------------------------- calls

    fn calls_in(&self, cx: &Context<'_>, shape: &Shape) -> Calls {
        self.calls(shape, cx.decision, cx.claim_check, cx.features)
            .adjust_for(shape, cx)
    }

    fn calls(
        &self,
        shape: &Shape,
        decision: &RoutingDecision,
        claim_check: bool,
        f: &QuestionFeatures,
    ) -> Calls {
        let n = shape.agents.len();
        let (planning, verify) = if shape.research_on && shape.flow != Flow::Debate {
            let depth = shape.depth.unwrap_or(decision.research.depth.value);
            let searches = decision.research.discovers_by_search()
                || (decision.research.urls.is_empty() && decision.research.is_active());
            (
                usize::from(searches),
                depth.profile().verifier_calls.min(n.max(1)),
            )
        } else {
            (0, 0)
        };
        let research = planning + verify;
        if claim_check {
            let c = planning + verify;
            return Calls {
                typical: c,
                max: c,
                required: c,
            };
        }
        match shape.flow {
            Flow::Single => {
                let c = 1 + research;
                Calls {
                    typical: c,
                    max: c,
                    required: c,
                }
            }
            Flow::Panel => {
                let c = n + usize::from(shape.rounds) * n + 1 + research;
                Calls {
                    typical: c,
                    max: c,
                    required: c,
                }
            }
            Flow::Debate => {
                let c = 2 + usize::from(shape.rounds) * n.saturating_sub(1);
                Calls {
                    typical: c,
                    max: c,
                    required: c,
                }
            }
            Flow::Adaptive => {
                let review_expected = initial_review_expected(decision.mode.value, f);
                Calls {
                    typical: research + if review_expected { 3 } else { 1 },
                    // lead + review + (a third perspective or a reconsideration) + synthesis, and
                    // the evidence check of a further perspective's claims
                    max: research + 4 + verify,
                    required: 1 + research,
                }
            }
        }
    }

    /// One automatic setting is lowered. Returns false when nothing automatic is left to lower.
    fn degrade(&self, cx: &Context<'_>, shape: &mut Shape, why: &mut Vec<String>) -> bool {
        // The debate engine needs at least one review round; the panel can go straight to the synthesis.
        let min_rounds = u8::from(shape.flow == Flow::Debate);
        if shape.flow != Flow::Single && shape.rounds > min_rounds && !cx.manual_rounds {
            shape.rounds -= 1;
            why.push(format!(
                "review rounds lowered to {} to fit the call limit",
                shape.rounds
            ));
            return true;
        }
        if !cx.manual_agents
            && shape.agents.len() > 2
            && shape.flow != Flow::Single
            && !cx.claim_check
        {
            shape.agents.truncate(2);
            why.push("two agents take part, to fit the call limit".to_string());
            return true;
        }
        if !cx.manual_agents
            && shape.agents.len() == 2
            && !cx.manual_rounds
            && matches!(shape.flow, Flow::Panel | Flow::Adaptive)
            && cx.decision.mode.value != ConsultMode::Debate
        {
            shape.agents.truncate(1);
            shape.flow = Flow::Single;
            shape.rounds = 0;
            why.push("one agent answers, to fit the call limit".to_string());
            return true;
        }
        let web_required = cx.input.research.web == WebMode::On
            || cx.claim_check
            || !cx.input.research.urls.is_empty();
        if shape.research_on
            && !cx.manual_depth
            && shape.depth != Some(Depth::Quick)
            && !web_required
        {
            shape.depth = Some(Depth::Quick);
            why.push("the evidence check was dropped (quick depth finds passages but does not judge them) to fit the call limit".to_string());
            return true;
        }
        if shape.research_on && !web_required {
            shape.research_on = false;
            why.push("web research was skipped to fit the call limit".to_string());
            return true;
        }
        false
    }

    fn forced_by(&self, cx: &Context<'_>, shape: &Shape) -> String {
        let mut parts = Vec::new();
        if cx.manual_agents {
            parts.push(format!("--agents names {} agent(s)", shape.agents.len()));
        }
        if cx.manual_rounds {
            parts.push(format!("--rounds asks for {} round(s)", shape.rounds));
        }
        if cx.input.research.web == WebMode::On
            || cx.claim_check
            || !cx.input.research.urls.is_empty()
        {
            parts.push(
                "the research you asked for needs its planning and verification calls".to_string(),
            );
        }
        if cx.decision.mode.value == ConsultMode::Debate {
            parts.push("a debate needs a proposal, its reviews and a synthesis".to_string());
        }
        if parts.is_empty() {
            "Even the smallest plan does not fit.".to_string()
        } else {
            format!("What forces it: {}.", parts.join("; "))
        }
    }
}

impl Calls {
    fn adjust_for(self, _shape: &Shape, _cx: &Context<'_>) -> Calls {
        self
    }
}

/// Whether the adaptive flow will ask for a review right after the lead's answer, judging by the
/// question alone (the evidence may add more reasons while it runs).
pub fn initial_review_expected(mode: ConsultMode, f: &QuestionFeatures) -> bool {
    matches!(mode, ConsultMode::Decision | ConsultMode::Review)
        || f.contested
        || f.complex
        || f.high_stakes
}

fn depth_reason(strategy: Strategy) -> &'static str {
    match strategy {
        Strategy::Thorough => "thorough checks the evidence deeply",
        _ => {
            "quick depth would find passages but never judge them, and this strategy keeps the evidence check"
        }
    }
}

fn participants_reason(strategy: Strategy, flow: Flow, n: usize, mode: ConsultMode) -> String {
    match (strategy, flow) {
        (_, Flow::Debate) => format!("a debate between {n} agents"),
        (_, Flow::Single) => format!(
            "the {strategy} strategy uses one agent; the others are not called unless you name them with --agents"
        ),
        (_, Flow::Adaptive) => format!(
            "{n} agents may take part: the lead answers, and the others are asked only if the {mode} result calls for it"
        ),
        _ => format!("all {n} usable agent(s), so the answers can be compared independently"),
    }
}

fn rounds_reason(strategy: Strategy, flow: Flow, rounds: u8) -> String {
    match flow {
        Flow::Adaptive => "one independent review; a second round only if the reviewer disagrees materially and no other agent is left".to_string(),
        _ => match rounds {
            0 => format!("the {strategy} strategy goes from the independent answers straight to the synthesis"),
            1 => format!("one cross-review round ({strategy})"),
            _ => format!("two cross-review rounds ({strategy})"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consult::{AgentInfo, ConsultationRouter, RoutingStrategy};
    use crate::research::net::FetchPolicy;
    use crate::research::plan::ResearchRequest;
    use crate::smart::profiles::{Observation, ProfileFile, ProfileSettings, ProviderProfile};

    fn id(s: &str) -> ProviderId {
        ProviderId::new(s)
    }

    fn agent(name: &str, search: bool) -> AgentInfo {
        AgentInfo {
            id: id(name),
            usable: Ok(()),
            can_search: search,
        }
    }

    fn down(name: &str) -> AgentInfo {
        AgentInfo {
            id: id(name),
            usable: Err("hosts this server".into()),
            can_search: false,
        }
    }

    fn three() -> Vec<AgentInfo> {
        vec![
            agent("lead", true),
            agent("beta", false),
            agent("gamma", false),
        ]
    }

    fn input<'a>(q: &'a str, agents: Vec<AgentInfo>) -> RouteInput<'a> {
        RouteInput {
            question: q,
            mode: None,
            coordinator: None,
            agents: None,
            rounds: None,
            length: None,
            research: ResearchRequest {
                web: WebMode::Off,
                ..Default::default()
            },
            fetch_policy: FetchPolicy::default(),
            available: agents,
            configured_coordinator: Some(id("lead")),
        }
    }

    fn engine() -> SmartStrategyEngine {
        SmartStrategyEngine::new(
            SmartSettings::default(),
            ProfileSnapshot::empty(ProfileSettings::default(), 0),
        )
    }

    fn plan_with(
        engine: &SmartStrategyEngine,
        i: &RouteInput<'_>,
        strategy: Option<Strategy>,
        tweak: impl FnOnce(&mut StrategyRequest),
    ) -> Result<RoutingDecision> {
        let d = ConsultationRouter.route(i).unwrap();
        let mut req = StrategyRequest {
            strategy,
            ..Default::default()
        };
        tweak(&mut req);
        engine.apply(i, d, &req, 300)
    }

    fn plan(i: &RouteInput<'_>, strategy: Strategy) -> RoutingDecision {
        plan_with(&engine(), i, Some(strategy), |_| {}).unwrap()
    }

    const DECISION_Q: &str = "Should I buy a budget or a professional oscilloscope?";
    const SIMPLE_Q: &str = "Explain electrical resistance";

    fn smart(d: &RoutingDecision) -> &SmartPlan {
        d.smart.as_ref().unwrap()
    }

    fn names(d: &RoutingDecision) -> Vec<String> {
        d.participants
            .value
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    // ---- the four strategies

    #[test]
    fn fast_uses_the_coordinator_alone_with_no_review() {
        let d = plan(&input(DECISION_Q, three()), Strategy::Fast);
        assert_eq!(names(&d), ["lead"]);
        assert_eq!((d.rounds.value, smart(&d).flow), (0, Flow::Single));
        assert_eq!(smart(&d).max_calls, 1);
        assert_eq!(smart(&d).strategy.as_ref().unwrap().source, Source::Manual);
        assert!(
            d.skipped
                .iter()
                .any(|s| s.provider == id("beta") && s.reason.contains("fast strategy"))
        );
        assert!(
            d.warnings
                .iter()
                .any(|w| w.contains("single-agent consultation"))
        );
    }

    #[test]
    fn balanced_starts_with_one_agent_and_adds_others_only_on_demand() {
        let d = plan(&input(DECISION_Q, three()), Strategy::Balanced);
        assert_eq!(smart(&d).flow, Flow::Adaptive);
        assert_eq!(
            names(&d),
            ["lead", "beta", "gamma"],
            "all may be asked, in this order"
        );
        assert_eq!(smart(&d).escalation_order, vec![id("beta"), id("gamma")]);
        assert_eq!((smart(&d).typical_calls, smart(&d).max_calls), (3, 4));
        assert_eq!(d.rounds.value, 1);
        // A simple teaching question needs no review at all.
        let simple = plan(&input(SIMPLE_Q, three()), Strategy::Balanced);
        assert_eq!(smart(&simple).typical_calls, 1);
    }

    #[test]
    fn thorough_runs_the_full_panel_with_two_rounds() {
        let d = plan(&input(DECISION_Q, three()), Strategy::Thorough);
        assert_eq!((smart(&d).flow, d.rounds.value), (Flow::Panel, 2));
        assert_eq!(names(&d).len(), 3);
        assert_eq!(smart(&d).max_calls, 3 + 2 * 3 + 1);
        assert!(d.skipped.is_empty());
    }

    // ---- automatic choice

    #[test]
    fn auto_chooses_by_the_question_and_says_why() {
        let cases = [
            (SIMPLE_Q, Strategy::Fast),
            (DECISION_Q, Strategy::Balanced),
            (
                "Compare the Acme S100 and the Borealis B200 oscilloscopes",
                Strategy::Balanced,
            ),
            (
                "Should I buy a budget or a professional oscilloscope? I mostly debug audio circuits up to 5 MHz, my budget is 400 EUR, I need USB export and probably protocol decoding later, I share the bench with my kids, and I may sell it in two years. What are the risks of each path, and which features really matter for my case?",
                Strategy::Thorough,
            ),
            (
                "Have the agents debate tabs versus spaces",
                Strategy::Thorough,
            ),
        ];
        for (q, expected) in cases {
            let d = plan(&input(q, three()), Strategy::Auto);
            let s = smart(&d).strategy.as_ref().unwrap();
            assert_eq!(s.value, expected, "{q}: {}", s.reason);
            assert_eq!(s.source, Source::Auto);
            assert!(!s.reason.is_empty());
        }
    }

    #[test]
    fn auto_uses_fast_when_only_one_agent_is_usable_or_the_budget_is_tiny() {
        let one = vec![agent("lead", false), down("beta"), down("gamma")];
        let d = plan(&input(DECISION_Q, one), Strategy::Auto);
        assert_eq!(smart(&d).strategy_value(), Some(Strategy::Fast));
        let i = input(DECISION_Q, three());
        let d = plan_with(&engine(), &i, Some(Strategy::Auto), |r| {
            r.max_agent_calls = Some(3)
        })
        .unwrap();
        assert_eq!(smart(&d).strategy_value(), Some(Strategy::Fast));
        assert!(
            smart(&d)
                .strategy
                .as_ref()
                .unwrap()
                .reason
                .contains("limit of 3")
        );
        let d = plan_with(&engine(), &i, Some(Strategy::Auto), |r| {
            r.max_duration = Some(Duration::from_secs(60))
        })
        .unwrap();
        assert_eq!(smart(&d).strategy_value(), Some(Strategy::Fast));
    }

    #[test]
    fn auto_is_deterministic() {
        let i = input(DECISION_Q, three());
        let first = plan(&i, Strategy::Auto);
        for _ in 0..20 {
            assert_eq!(plan(&i, Strategy::Auto), first);
        }
    }

    // ---- manual settings always win

    #[test]
    fn named_agents_are_never_dropped_whatever_the_strategy() {
        let mut i = input(DECISION_Q, three());
        i.agents = Some(vec![id("lead"), id("beta"), id("gamma")]);
        for s in [
            Strategy::Fast,
            Strategy::Balanced,
            Strategy::Thorough,
            Strategy::Auto,
        ] {
            let d = plan(&i, s);
            assert_eq!(names(&d), ["lead", "beta", "gamma"], "{s}");
            assert_eq!(d.participants.source, Source::Manual);
            assert_eq!(
                smart(&d).flow,
                Flow::Panel,
                "{s}: named agents run the full independent panel"
            );
            assert!(d.skipped.is_empty());
        }
        // The strategy still fills in the rounds the user did not set.
        assert_eq!(plan(&i, Strategy::Fast).rounds.value, 0);
        assert_eq!(plan(&i, Strategy::Thorough).rounds.value, 2);
    }

    #[test]
    fn explicit_rounds_are_honoured_by_every_strategy_and_bring_in_a_reviewer() {
        let mut i = input(SIMPLE_Q, three());
        i.rounds = Some(2);
        for s in [Strategy::Fast, Strategy::Balanced, Strategy::Thorough] {
            let d = plan(&i, s);
            assert_eq!(d.rounds.value, 2, "{s}");
            assert_eq!(d.rounds.source, Source::Manual);
            assert!(
                d.participants.value.len() >= 2,
                "{s}: a review needs a second agent"
            );
            assert_eq!(smart(&d).flow, Flow::Panel);
        }
    }

    #[test]
    fn a_manual_coordinator_leads_and_a_manual_mode_is_kept() {
        let mut i = input(DECISION_Q, three());
        i.coordinator = Some(id("gamma"));
        i.mode = Some(ConsultMode::Learn);
        let d = plan(&i, Strategy::Fast);
        assert_eq!(names(&d), ["gamma"]);
        assert_eq!(d.coordinator.source, Source::Manual);
        assert_eq!(d.mode.source, Source::Manual);
        assert!(
            smart(&d)
                .explanation
                .iter()
                .any(|l| l.contains("kept as you set them") && l.contains("coordinator"))
        );
    }

    #[test]
    fn the_host_agent_is_never_chosen_by_any_strategy() {
        let agents = vec![down("claude"), agent("codex", true), agent("google", false)];
        for s in [
            Strategy::Fast,
            Strategy::Balanced,
            Strategy::Thorough,
            Strategy::Auto,
        ] {
            let mut i = input(DECISION_Q, agents.clone());
            i.configured_coordinator = Some(id("codex"));
            let d = plan(&i, s);
            assert!(!names(&d).contains(&"claude".to_string()), "{s}");
            assert!(
                d.skipped
                    .iter()
                    .any(|x| x.provider == id("claude") && x.reason.contains("hosts this server"))
            );
        }
    }

    // ---- conflicts are rejected before anything runs

    #[test]
    fn explicit_agents_and_rounds_that_cannot_fit_the_call_limit_are_rejected_with_the_arithmetic()
    {
        let mut i = input(DECISION_Q, three());
        i.agents = Some(vec![id("lead"), id("beta"), id("gamma")]);
        i.rounds = Some(2);
        let err = plan_with(&engine(), &i, Some(Strategy::Auto), |r| {
            r.max_agent_calls = Some(5)
        })
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("at least 10 agent call(s)") && err.contains("--max-agent-calls is 5"),
            "{err}"
        );
        assert!(
            err.contains("--agents names 3") && err.contains("--rounds asks for 2"),
            "{err}"
        );
        // With enough budget the same request is fine.
        assert!(
            plan_with(&engine(), &i, Some(Strategy::Auto), |r| r.max_agent_calls =
                Some(10))
            .is_ok()
        );
    }

    #[test]
    fn automatic_parts_are_lowered_to_fit_a_call_limit_and_the_plan_says_so() {
        let i = input(DECISION_Q, three());
        let d = plan_with(&engine(), &i, Some(Strategy::Thorough), |r| {
            r.max_agent_calls = Some(5)
        })
        .unwrap();
        assert!(smart(&d).max_calls <= 5, "{} calls", smart(&d).max_calls);
        assert_eq!(
            smart(&d).strategy.as_ref().unwrap().source,
            Source::Manual,
            "the strategy stays what was asked"
        );
        assert!(
            smart(&d)
                .explanation
                .iter()
                .any(|l| l.contains("to fit the call limit")),
            "{:?}",
            smart(&d).explanation
        );
        let one = plan_with(&engine(), &i, Some(Strategy::Thorough), |r| {
            r.max_agent_calls = Some(1)
        })
        .unwrap();
        assert_eq!((names(&one).len(), smart(&one).max_calls), (1, 1));
    }

    #[test]
    fn limits_that_make_no_sense_or_exceed_the_ceiling_are_rejected() {
        let i = input(DECISION_Q, three());
        for tweak in [
            (|r: &mut StrategyRequest| r.max_agent_calls = Some(0)) as fn(&mut StrategyRequest),
            |r| r.max_parallel = Some(0),
            |r| r.max_duration = Some(Duration::ZERO),
            |r| r.max_duration = Some(Duration::from_secs(99_999)),
        ] {
            assert!(plan_with(&engine(), &i, Some(Strategy::Fast), tweak).is_err());
        }
        let err = plan_with(&engine(), &i, Some(Strategy::Fast), |r| {
            r.max_duration = Some(Duration::from_secs(99_999))
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("exceeds the configured ceiling"), "{err}");
    }

    #[test]
    fn a_debate_keeps_two_agents_even_under_fast_and_still_fits_a_small_limit_or_is_rejected() {
        let mut i = input("Have the agents debate tabs versus spaces", three());
        i.mode = Some(ConsultMode::Debate);
        let d = plan(&i, Strategy::Fast);
        assert_eq!(
            (smart(&d).flow, d.participants.value.len()),
            (Flow::Debate, 2)
        );
        let err = plan_with(&engine(), &i, Some(Strategy::Fast), |r| {
            r.max_agent_calls = Some(2)
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("a debate needs"), "{err}");
    }

    // ---- research

    #[test]
    fn fast_keeps_the_evidence_check_instead_of_quick_depth() {
        let mut i = input("What is the latest stable version of Rust?", three());
        i.research = ResearchRequest {
            web: WebMode::On,
            ..Default::default()
        };
        let router = ConsultationRouter.route(&i).unwrap();
        let d = plan(&i, Strategy::Fast);
        assert!(d.research.is_active());
        assert_eq!(d.research.depth.value, Depth::Standard);
        assert_eq!(d.research.depth.source, Source::Auto);
        assert_eq!(names(&d), ["lead"], "the lead can search the web");
        assert!(
            smart(&d).max_calls < router.planned_calls(),
            "{} vs {}",
            smart(&d).max_calls,
            router.planned_calls()
        );
        assert_eq!(
            smart(&d).max_calls,
            3,
            "planning + answer + one verification"
        );
    }

    #[test]
    fn an_explicit_depth_is_never_changed_by_a_strategy() {
        let mut i = input("What is the latest stable version of Rust?", three());
        i.research = ResearchRequest {
            web: WebMode::On,
            depth: Some(Depth::Quick),
            ..Default::default()
        };
        for s in [Strategy::Fast, Strategy::Balanced, Strategy::Thorough] {
            let d = plan(&i, s);
            assert_eq!(
                (d.research.depth.value, d.research.depth.source),
                (Depth::Quick, Source::Manual),
                "{s}"
            );
        }
        i.research.depth = None;
        assert_eq!(
            plan(&i, Strategy::Thorough).research.depth.value,
            Depth::Deep
        );
    }

    #[test]
    fn fast_leads_with_the_agent_that_can_search_when_the_question_needs_the_web() {
        let agents = vec![
            agent("lead", false),
            agent("beta", true),
            agent("gamma", false),
        ];
        let mut i = input("What is the latest stable version of Rust?", agents);
        i.research = ResearchRequest {
            web: WebMode::On,
            ..Default::default()
        };
        let d = plan(&i, Strategy::Fast);
        assert_eq!(names(&d), ["beta"]);
        assert!(
            smart(&d)
                .explanation
                .iter()
                .any(|l| l.contains("beta leads instead")),
            "{:?}",
            smart(&d).explanation
        );
        // A manual coordinator that cannot search conflicts with --web on and fast.
        i.coordinator = Some(id("lead"));
        let err = plan_with(&engine(), &i, Some(Strategy::Fast), |_| {})
            .unwrap_err()
            .to_string();
        assert!(err.contains("search-capable agent"), "{err}");
    }

    #[test]
    fn a_call_limit_drops_only_automatic_research_and_never_a_required_one() {
        let mut i = input("What is the latest stable version of Rust?", three());
        i.research = ResearchRequest {
            web: WebMode::Auto,
            ..Default::default()
        };
        let d = plan_with(&engine(), &i, Some(Strategy::Fast), |r| {
            r.max_agent_calls = Some(1)
        })
        .unwrap();
        assert!(!d.research.is_active());
        assert!(
            d.research
                .limitations
                .iter()
                .any(|l| l.contains("skipped to fit"))
        );
        assert_eq!(smart(&d).max_calls, 1);
        i.research.web = WebMode::On;
        let err = plan_with(&engine(), &i, Some(Strategy::Fast), |r| {
            r.max_agent_calls = Some(1)
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("research you asked for"), "{err}");
    }

    // ---- provider history

    fn history(entries: &[(&str, u64, u32)]) -> ProfileSnapshot {
        // (provider, answer latency ms, failure streak)
        let mut file = ProfileFile::default();
        for (name, ms, streak) in entries {
            let mut p = ProviderProfile::default();
            for _ in 0..4 {
                p.observe(
                    &Observation {
                        provider: (*name).into(),
                        role: crate::debate::Role::Perspective,
                        ok: true,
                        error_kind: None,
                        duration_ms: *ms,
                        response_chars: 10,
                        input_tokens: None,
                        output_tokens: None,
                    },
                    1000,
                    0.3,
                );
            }
            for _ in 0..*streak {
                p.observe(
                    &Observation {
                        provider: (*name).into(),
                        role: crate::debate::Role::Perspective,
                        ok: false,
                        error_kind: Some("quota".into()),
                        duration_ms: 1,
                        response_chars: 0,
                        input_tokens: None,
                        output_tokens: None,
                    },
                    1000,
                    0.3,
                );
            }
            file.providers.insert((*name).into(), p);
        }
        ProfileSnapshot::new(file, ProfileSettings::default(), 1000)
    }

    #[test]
    fn history_orders_the_other_agents_by_typical_latency_but_never_excludes_one() {
        let engine = SmartStrategyEngine::new(
            SmartSettings::default(),
            history(&[("beta", 9_000, 0), ("gamma", 2_000, 0)]),
        );
        let i = input(DECISION_Q, three());
        let d = plan_with(&engine, &i, Some(Strategy::Balanced), |_| {}).unwrap();
        assert_eq!(
            names(&d),
            ["lead", "gamma", "beta"],
            "the faster agent is asked first"
        );
        assert!(
            smart(&d)
                .profile_notes
                .iter()
                .any(|n| n.contains("gamma") && n.contains("2000 ms")),
            "{:?}",
            smart(&d).profile_notes
        );
        let thorough = plan_with(&engine, &i, Some(Strategy::Thorough), |_| {}).unwrap();
        assert_eq!(names(&thorough).len(), 3, "thorough still uses everyone");
    }

    #[test]
    fn without_enough_history_the_registry_order_is_used_and_the_plan_says_so() {
        let i = input(DECISION_Q, three());
        let d = plan(&i, Strategy::Balanced);
        assert_eq!(names(&d), ["lead", "beta", "gamma"]);
        assert!(
            smart(&d)
                .profile_notes
                .iter()
                .any(|n| n.contains("no provider history yet"))
        );
    }

    #[test]
    fn a_repeatedly_failing_automatic_coordinator_is_replaced_but_a_manual_one_never_is() {
        let engine = SmartStrategyEngine::new(
            SmartSettings::default(),
            history(&[("lead", 1_000, 3), ("beta", 1_000, 0)]),
        );
        let i = input(DECISION_Q, three());
        let d = plan_with(&engine, &i, Some(Strategy::Fast), |_| {}).unwrap();
        assert_eq!(names(&d), ["beta"]);
        assert_eq!(d.coordinator.value, id("beta"));
        assert!(d.coordinator.reason.contains("failed repeatedly"));
        let mut manual = input(DECISION_Q, three());
        manual.coordinator = Some(id("lead"));
        let d = plan_with(&engine, &manual, Some(Strategy::Fast), |_| {}).unwrap();
        assert_eq!(
            names(&d),
            ["lead"],
            "an explicit coordinator is never changed"
        );
    }

    // ---- without a strategy

    #[test]
    fn without_a_strategy_the_router_decision_stands_and_only_limits_are_checked() {
        let i = input(DECISION_Q, three());
        let router = ConsultationRouter.route(&i).unwrap();
        let d = plan_with(&engine(), &i, None, |r| r.max_agent_calls = Some(20)).unwrap();
        assert_eq!(d.participants, router.participants);
        assert_eq!(d.rounds, router.rounds);
        assert!(smart(&d).strategy.is_none());
        assert_eq!(smart(&d).max_calls, router.planned_calls());
        let err = plan_with(&engine(), &i, None, |r| r.max_agent_calls = Some(2))
            .unwrap_err()
            .to_string();
        assert!(err.contains("--max-agent-calls is 2"), "{err}");
    }

    #[test]
    fn question_features_are_wording_only_and_stable() {
        let f = question_features(SIMPLE_Q);
        assert!(f.simple && !f.contested && !f.complex && !f.high_stakes);
        let f =
            question_features("Is this medication dose safe for a child with a legal guardian?");
        assert!(f.high_stakes && !f.simple);
        let f = question_features(&"word ".repeat(60));
        assert!(f.complex && !f.simple);
        assert!(
            question_features("A? B? C?").complex,
            "three separate questions"
        );
    }
}
