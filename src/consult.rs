//! Intelligent consultation (`ai-orchestra consult`): decision support, research, product
//! review, teaching and debate over the same bounded multi-agent engine.
//!
//! **Automatic by default, manual when requested.** A [`RoutingStrategy`] (the built-in
//! [`ConsultationRouter`] is a deterministic rule set: no model call is ever made to route)
//! turns the question plus any explicit settings into a [`RoutingDecision`]: mode, coordinator,
//! participants and review rounds, each marked `auto` or `manual` with a reason. Explicit
//! settings always win and are never silently changed; where something cannot be honoured
//! (an agent is not installed, the rounds make no sense for one participant) the decision says so.
//!
//! The workflow for the four knowledge modes (decision, research, review, learn):
//!
//! 1. **Independent perspectives.** Every participant, the coordinator included, answers the
//!    question in parallel without seeing the others, so nobody is anchored on the coordinator's
//!    opinion. The prompt also asks each one to state the main question, criteria, assumptions,
//!    information gaps and whether outside evidence is needed (stage A costs no extra call).
//! 2. **Cross-review, 1 or 2 rounds.** Each participant that answered reads the others'
//!    answers and looks for mistakes, unsupported claims, contradictions and missing criteria.
//!    Round 2 also shows the round-1 critiques. A participant that failed is not asked again.
//! 3. **Synthesis.** The coordinator writes one answer that keeps the disagreements, says what
//!    is evidence and what is opinion, and states when the evidence is not enough. Agreement
//!    between models is never treated as proof.
//!
//! Calls are bounded by `N + rounds * N + 1` for `N` participants (`1` for a single agent).
//! `debate` mode runs the Milestone 2 debate engine unchanged, with a consultation record on top.
//! Nothing an agent writes can trigger a call or run anything, and all calls are read-only.
//!
//! A consultation is saved as a normal debate record with a [`ConsultationMeta`] attached, so
//! `history`, `show` and the MCP `get_debate` tool keep working for both kinds.

mod adaptive_flow;
mod memory_flow;

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

use crate::config::MAX_DEBATE_ROUNDS;
use crate::debate::{
    DebateEvent, DebateOptions, DebateOutcome, DebateStatus, Emitter, MAX_PEERS, ModelOverride,
    QUESTION_MAX_BYTES, READ_ONLY_NOTE, Role, Runner, Skipped, Slot, Step, clip, elapsed_ms,
    first_line, review_cap,
};
use crate::error::{OrchestraError, Result};
use crate::history::{format_utc, new_debate_id, now_ms};
use crate::memory::service::{MemoryRequest, Prepared};
use crate::orchestrator::Orchestrator;
use crate::provider::AgentProvider;
use crate::research::claims::strip_claims_block;
use crate::research::engine::{
    CLAIM_TABLE_MAX_BYTES, CLAIMS_REQUEST, EVIDENCE_RULES, ResearchReport, ResearchRun,
    claims_table,
};
use crate::research::fetch::SourceFetcher;
use crate::research::plan::{
    Depth, ResearchDecision, ResearchInput, ResearchRequest, WebMode, decide_research,
};
use crate::smart::engine::{SmartSettings, SmartStrategyEngine, StrategyRequest};
use crate::smart::quality::QualityRequirements;
use crate::smart::run::SmartRun;
use crate::smart::strategy::{Flow, SmartPlan, Strategy};
use crate::smart::trace::{OrchestrationRecord, TraceKind};
use crate::store::{Author, EntryKind};
use crate::types::{AgentRequest, ProviderId};

/// Most agents in one consultation (the coordinator plus [`MAX_PEERS`] others).
pub const MAX_PARTICIPANTS: usize = MAX_PEERS + 1;
/// Longest question counted as "simple" for the lightweight single-agent path.
const SIMPLE_QUESTION_WORDS: usize = 20;
/// Questions longer than this get the second review round when routing automatically.
const LONG_QUESTION_BYTES: usize = 600;
const META_SCHEMA: u32 = 1;

// ------------------------------------------------------------------ modes

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsultMode {
    /// Choosing between alternatives.
    Decision,
    /// Investigating a topic.
    Research,
    /// Evaluating a product, technology, service, article or claim.
    Review,
    /// Teaching a subject.
    Learn,
    /// Explicit multi-agent deliberation (the Milestone 2 debate engine).
    Debate,
}

impl ConsultMode {
    pub const ALL: [ConsultMode; 5] = [
        ConsultMode::Decision,
        ConsultMode::Research,
        ConsultMode::Review,
        ConsultMode::Learn,
        ConsultMode::Debate,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ConsultMode::Decision => "decision",
            ConsultMode::Research => "research",
            ConsultMode::Review => "review",
            ConsultMode::Learn => "learn",
            ConsultMode::Debate => "debate",
        }
    }
}

impl std::fmt::Display for ConsultMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ConsultMode {
    type Err = OrchestraError;

    fn from_str(s: &str) -> Result<Self> {
        let wanted = s.trim().to_lowercase();
        ConsultMode::ALL
            .into_iter()
            .find(|m| m.as_str() == wanted)
            .ok_or_else(|| {
                OrchestraError::Config(format!(
                    "unknown consultation mode `{s}` (use auto, decision, research, review, learn or debate)"
                ))
            })
    }
}

/// `auto` (or nothing) means "choose for me".
pub fn parse_mode_arg(value: Option<&str>) -> Result<Option<ConsultMode>> {
    match value.map(str::trim) {
        None | Some("") => Ok(None),
        Some(v) if v.eq_ignore_ascii_case("auto") => Ok(None),
        Some(v) => v.parse().map(Some),
    }
}

/// `auto` (or nothing) means "choose for me"; otherwise 1 or 2.
pub fn parse_rounds_arg(value: Option<&str>) -> Result<Option<u8>> {
    match value.map(str::trim) {
        None | Some("") => Ok(None),
        Some(v) if v.eq_ignore_ascii_case("auto") => Ok(None),
        Some(v) => match v.parse::<u8>() {
            Ok(n) if (1..=MAX_DEBATE_ROUNDS).contains(&n) => Ok(Some(n)),
            _ => Err(OrchestraError::Config(format!(
                "rounds must be auto, 1 or {MAX_DEBATE_ROUNDS}, got `{v}`"
            ))),
        },
    }
}

/// `auto` alone (or nothing) means "choose for me"; otherwise a list of agent names.
pub fn parse_agents_arg(values: &[String]) -> Result<Option<Vec<String>>> {
    let names: Vec<String> = values
        .iter()
        .flat_map(|v| v.split(','))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .collect();
    if names.is_empty() {
        return Ok(None);
    }
    let autos = names
        .iter()
        .filter(|n| n.eq_ignore_ascii_case("auto"))
        .count();
    match (autos, names.len()) {
        (0, _) => Ok(Some(names)),
        (_, 1) => Ok(None),
        _ => Err(OrchestraError::Config(
            "`auto` cannot be combined with agent names in --agents".into(),
        )),
    }
}

/// `auto` (or nothing) means "choose for me"; otherwise an agent name.
pub fn parse_coordinator_arg(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|v| !v.is_empty() && !v.eq_ignore_ascii_case("auto"))
        .map(str::to_string)
}

// ---------------------------------------------------------------- routing

/// Where a routing choice came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// Chosen by the router.
    Auto,
    /// Requested explicitly by the user (CLI flag or MCP parameter).
    Manual,
}

/// A routing choice with its origin and the reason for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Routed<T> {
    pub value: T,
    pub source: Source,
    pub reason: String,
}

impl<T> Routed<T> {
    pub fn auto(value: T, reason: impl Into<String>) -> Self {
        Self {
            value,
            source: Source::Auto,
            reason: reason.into(),
        }
    }

    pub fn manual(value: T, reason: impl Into<String>) -> Self {
        Self {
            value,
            source: Source::Manual,
            reason: reason.into(),
        }
    }
}

/// One registered agent, and whether it can be used for this consultation.
#[derive(Debug, Clone)]
pub struct AgentInfo {
    pub id: ProviderId,
    /// `Err` carries the reason it cannot be used (not installed, disabled, hosts the MCP server).
    pub usable: std::result::Result<(), String>,
    /// Its CLI has a usable web search (`Verified` or `Documented`; see the capability matrix).
    pub can_search: bool,
}

/// Everything the router looks at. Availability is the only provider "capability" it knows:
/// the rules deliberately say nothing about one provider being better than another.
#[derive(Debug, Clone)]
pub struct RouteInput<'a> {
    pub question: &'a str,
    pub mode: Option<ConsultMode>,
    pub coordinator: Option<ProviderId>,
    pub agents: Option<Vec<ProviderId>>,
    pub rounds: Option<u8>,
    /// `None`: brief when the question asks for a short answer, otherwise full.
    pub length: Option<Length>,
    /// Web research settings (all automatic by default).
    pub research: ResearchRequest,
    /// What may be fetched (to reject unusable `--url` values early).
    pub fetch_policy: crate::research::net::FetchPolicy,
    /// Every registered agent, in registry order.
    pub available: Vec<AgentInfo>,
    /// `general.coordinator`, used when it is usable.
    pub configured_coordinator: Option<ProviderId>,
}

/// What will happen, and why. Saved with the consultation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutingDecision {
    pub mode: Routed<ConsultMode>,
    pub coordinator: Routed<ProviderId>,
    /// Coordinator first.
    pub participants: Routed<Vec<ProviderId>>,
    /// Review rounds after the independent answers: 0 (nobody to review), 1 or 2.
    pub rounds: Routed<u8>,
    /// Whether answers are short (`true`) or the full report. Records saved before this
    /// existed read as full.
    #[serde(default = "default_brief")]
    pub brief: Routed<bool>,
    /// Whether and how web research is part of this consultation. Records saved before research
    /// existed read as "not active".
    #[serde(default = "default_research")]
    pub research: ResearchDecision,
    /// Agents that were left out, with reasons (never silently).
    pub skipped: Vec<Skipped>,
    /// Things the user should know, for example a setting that could not be honoured.
    pub warnings: Vec<String>,
    /// Question features the rules matched (for transparency).
    pub signals: Vec<String>,
    /// The smart orchestration plan: strategy, flow, limits and the reasons for them. Records
    /// saved before Milestone 6 read as "none".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smart: Option<crate::smart::strategy::SmartPlan>,
}

impl RoutingDecision {
    /// Names of the settings the user set explicitly.
    pub fn manual_settings(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.mode.source == Source::Manual {
            out.push("mode".to_string());
        }
        if self.coordinator.source == Source::Manual {
            out.push("coordinator".to_string());
        }
        if self.participants.source == Source::Manual {
            out.push("agents".to_string());
        }
        if self.rounds.source == Source::Manual {
            out.push("rounds".to_string());
        }
        if self.brief.source == Source::Manual {
            out.push("length".to_string());
        }
        out
    }

    /// Calls this consultation will make at most.
    pub fn planned_calls(&self) -> usize {
        if let Some(plan) = &self.smart {
            return plan.max_calls;
        }
        let n = self.participants.value.len();
        if self.mode.value == ConsultMode::Debate {
            // proposal + rounds * reviewers + synthesis
            return 2 + usize::from(self.rounds.value) * (n - 1);
        }
        let panel = if n <= 1 {
            1
        } else {
            n + usize::from(self.rounds.value) * n + 1
        };
        // Research adds a planning call and the verification calls (a single agent verifies with
        // at most one call).
        let verifiers = self
            .research
            .depth
            .value
            .profile()
            .verifier_calls
            .min(n.max(1));
        panel
            + if self.research.is_active() {
                usize::from(self.research.discovers_by_search()) + verifiers
            } else {
                0
            }
    }

    /// Human-readable explanation, one line per decision.
    pub fn render(&self) -> String {
        let src = |s: Source| match s {
            Source::Auto => "auto",
            Source::Manual => "manual",
        };
        let mut out = String::new();
        let _ = writeln!(
            out,
            "mode:         {} ({}: {})",
            self.mode.value,
            src(self.mode.source),
            self.mode.reason
        );
        let _ = writeln!(
            out,
            "coordinator:  {} ({}: {})",
            self.coordinator.value,
            src(self.coordinator.source),
            self.coordinator.reason
        );
        let _ = writeln!(
            out,
            "participants: {} ({}: {})",
            join_ids(&self.participants.value),
            src(self.participants.source),
            self.participants.reason
        );
        let _ = writeln!(
            out,
            "rounds:       {} ({}: {})",
            self.rounds.value,
            src(self.rounds.source),
            self.rounds.reason
        );
        for s in &self.skipped {
            let _ = writeln!(out, "not used:     {} ({})", s.provider, s.reason);
        }
        for w in &self.warnings {
            let _ = writeln!(out, "note:         {w}");
        }
        let _ = writeln!(
            out,
            "length:       {} ({}: {})",
            if self.brief.value { "brief" } else { "full" },
            src(self.brief.source),
            self.brief.reason
        );
        let r = &self.research;
        if r.is_active() {
            let _ = writeln!(
                out,
                "research:     on ({}: {}) · depth {} ({}: {}) · up to {} source(s)",
                src(r.active.source),
                r.active.reason,
                r.depth.value.as_str(),
                src(r.depth.source),
                r.depth.reason,
                r.max_sources.value
            );
            if !r.urls.is_empty() {
                let _ = writeln!(out, "sources:      {} URL(s) provided by you", r.urls.len());
            }
            let _ = writeln!(
                out,
                "search:       {}",
                r.discovery_agent.as_ref().map_or("no agent searches the web; only the provided URLs are used".to_string(), |a| format!("{a} searches the web; every page it reports is fetched and checked independently"))
            );
        } else {
            let _ = writeln!(
                out,
                "research:     off ({}: {})",
                src(r.active.source),
                r.active.reason
            );
        }
        for l in &r.limitations {
            let _ = writeln!(out, "limitation:   {l}");
        }
        match &self.smart {
            Some(plan) => out.push_str(&plan.render()),
            None => {
                let _ = writeln!(out, "agent calls:  at most {}", self.planned_calls());
            }
        }
        out
    }
}

fn join_ids(ids: &[ProviderId]) -> String {
    ids.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Pluggable selection logic. The shipped [`ConsultationRouter`] is rule based; a later
/// milestone can supply a smarter strategy without touching the engine.
pub trait RoutingStrategy: Send + Sync {
    fn route(&self, input: &RouteInput<'_>) -> Result<RoutingDecision>;
}

/// The deterministic router: keyword rules, no model calls, fully testable.
#[derive(Debug, Clone, Copy, Default)]
pub struct ConsultationRouter;

/// Cue words per mode. A trailing `*` matches any word that starts with the cue; everything
/// else matches whole words or phrases. Weights decide between modes; ties go to the earlier
/// entry of [`MODE_PRIORITY`]. English plus a few Spanish cues.
const DEBATE_CUES: &[(&str, u8)] = &[
    ("debate*", 5),
    ("argue both sides", 5),
    ("devil's advocate", 5),
    ("have them argue", 5),
    ("let them argue", 5),
];
const DECISION_CUES: &[(&str, u8)] = &[
    ("should i", 2),
    ("should we", 2),
    ("which", 1),
    ("choose", 2),
    ("choosing", 2),
    ("pick", 1),
    ("decide", 2),
    ("deciding", 2),
    ("decision", 2),
    ("or", 1),
    ("vs", 2),
    ("versus", 2),
    ("trade-off*", 2),
    ("tradeoff*", 2),
    ("pros and cons", 2),
    ("alternative*", 1),
    ("better", 1),
    ("instead of", 2),
    ("compar*", 2),
    ("cuál", 1),
    ("cual", 1),
    ("debería", 2),
    ("deberia", 2),
    ("elegir", 2),
    ("decidir", 2),
    ("compara*", 2),
];
const REVIEW_CUES: &[(&str, u8)] = &[
    ("review*", 3),
    ("buy", 1),
    ("buying", 1),
    ("purchase*", 2),
    ("price*", 2),
    ("cheap*", 1),
    ("expensive", 1),
    ("worth", 2),
    ("value", 1),
    ("best", 1),
    ("spec", 2),
    ("specs", 2),
    ("specification*", 2),
    ("model", 1),
    ("product*", 2),
    ("rating*", 1),
    ("compar*", 1),
    ("comprar", 2),
    ("precio*", 2),
    ("reseña*", 3),
];
const RESEARCH_CUES: &[(&str, u8)] = &[
    ("research*", 3),
    ("investigate*", 3),
    ("investiga*", 3),
    ("latest", 2),
    ("recent", 1),
    ("development*", 1),
    ("state of the art", 2),
    ("find out", 2),
    ("what is known", 2),
    ("history of", 2),
    ("literature", 2),
    ("studies", 2),
    ("study", 1),
    ("evidence", 1),
    ("is it true", 3),
    ("is this true", 3),
    ("actually true", 3),
    ("true or false", 3),
    ("claim*", 2),
    ("fact check*", 3),
    ("fact-check*", 3),
    ("myth*", 2),
    ("verify", 2),
    ("sources", 1),
    ("últimos", 2),
    ("ultimos", 2),
    ("novedades", 2),
];
const LEARN_CUES: &[(&str, u8)] = &[
    ("explain*", 3),
    ("teach*", 3),
    ("tutorial*", 3),
    ("from the beginning", 3),
    ("from scratch", 3),
    ("eli5", 3),
    ("introduction to", 3),
    ("intro to", 2),
    ("how does", 2),
    ("how do", 1),
    ("how is", 1),
    ("what is", 2),
    ("what are", 2),
    ("what's", 2),
    ("why", 1),
    ("learn*", 1),
    ("understand*", 2),
    ("beginner*", 2),
    ("basics", 2),
    ("explica*", 3),
    ("explícame", 3),
    ("explicame", 3),
    ("enséñame", 3),
    ("ensename", 3),
    ("aprender", 2),
    ("qué es", 2),
    ("que es", 2),
];
/// Wording that asks for a short answer ("in two sentences", "briefly", "tl;dr"). A question
/// like this gets the brief style unless `--length full` says otherwise.
const BRIEF_CUES: &[&str] = &[
    "one sentence",
    "single sentence",
    "two sentences",
    "three sentences",
    "few sentences",
    "1 sentence",
    "2 sentences",
    "3 sentences",
    "briefly",
    "brief",
    "concise*",
    "short answer",
    "short summary",
    "in a nutshell",
    "tldr",
    "tl dr",
    "one line",
    "one-line",
    "a single line",
    "one paragraph",
    "in a few words",
    "quick answer",
    "en una frase",
    "brevemente",
    "en pocas palabras",
];

/// Answer length style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Length {
    Brief,
    Full,
}

/// `auto` (or nothing) means "decide from the question"; otherwise `brief` or `full`.
pub fn parse_length_arg(value: Option<&str>) -> Result<Option<Length>> {
    match value.map(|v| v.trim().to_lowercase()).as_deref() {
        None | Some("") | Some("auto") => Ok(None),
        Some("brief") => Ok(Some(Length::Brief)),
        Some("full") => Ok(Some(Length::Full)),
        Some(v) => Err(OrchestraError::Config(format!(
            "unknown length `{v}` (use auto, brief or full)"
        ))),
    }
}

fn asks_for_brevity(normalized: &str) -> Option<String> {
    let matched: Vec<&str> = BRIEF_CUES
        .iter()
        .copied()
        .filter(|c| cue_matches(normalized, c))
        .collect();
    (!matched.is_empty()).then(|| {
        format!(
            "the question asks for a short answer ({})",
            matched
                .iter()
                .take(2)
                .map(|c| c.trim_end_matches('*'))
                .collect::<Vec<_>>()
                .join(", ")
        )
    })
}

fn default_brief() -> Routed<bool> {
    Routed::auto(false, "not recorded")
}

fn default_research() -> ResearchDecision {
    ResearchDecision::inactive(crate::research::plan::WebMode::Auto, "not recorded")
}

/// Questions touching these deserve the second review round when routing automatically.
const CONTESTED_CUES: &[&str] = &[
    "controvers*",
    "contested",
    "disagree*",
    "debate*",
    "pros and cons",
    "trade-off*",
    "tradeoff*",
    "risk*",
    "danger*",
    "safety",
    "medical",
    "legal",
    "invest*",
    "diagnos*",
    "difficult",
    "high-stakes",
    "high stakes",
    "irreversible",
    "mortgage",
    "actually true",
    "myth*",
    "claim*",
    "riesgo*",
];
/// Tie-break order between modes with the same score.
const MODE_PRIORITY: [ConsultMode; 5] = [
    ConsultMode::Debate,
    ConsultMode::Decision,
    ConsultMode::Review,
    ConsultMode::Research,
    ConsultMode::Learn,
];

/// Lower-cased, punctuation turned into spaces, padded with one space on each side.
pub(crate) fn normalize_question(question: &str) -> String {
    let mut out = String::from(" ");
    let mut last_space = true;
    for c in question.to_lowercase().chars() {
        let keep = c.is_alphanumeric() || matches!(c, '\'' | '-' | '$' | '€');
        if keep {
            out.push(c);
            last_space = false;
        } else if !last_space {
            out.push(' ');
            last_space = true;
        }
    }
    if !last_space {
        out.push(' ');
    }
    out
}

pub(crate) fn cue_matches(haystack: &str, cue: &str) -> bool {
    match cue.strip_suffix('*') {
        Some(prefix) => haystack.contains(&format!(" {prefix}")),
        None => haystack.contains(&format!(" {cue} ")),
    }
}

struct ModeScore {
    mode: ConsultMode,
    score: u32,
    cues: Vec<String>,
}

fn score_modes(normalized: &str) -> Vec<ModeScore> {
    let table: [(ConsultMode, &[(&str, u8)]); 5] = [
        (ConsultMode::Debate, DEBATE_CUES),
        (ConsultMode::Decision, DECISION_CUES),
        (ConsultMode::Review, REVIEW_CUES),
        (ConsultMode::Research, RESEARCH_CUES),
        (ConsultMode::Learn, LEARN_CUES),
    ];
    let mut scores: Vec<ModeScore> = table
        .iter()
        .map(|(mode, cues)| {
            let mut score = 0u32;
            let mut matched = Vec::new();
            for (cue, weight) in *cues {
                if cue_matches(normalized, cue) {
                    score += u32::from(*weight);
                    matched.push(cue.trim_end_matches('*').to_string());
                }
            }
            ModeScore {
                mode: *mode,
                score,
                cues: matched,
            }
        })
        .collect();
    // A price symbol is a strong product cue.
    if (normalized.contains('$') || normalized.contains('€'))
        && let Some(r) = scores.iter_mut().find(|s| s.mode == ConsultMode::Review)
    {
        r.score += 2;
        r.cues.push("price symbol".to_string());
    }
    scores
}

struct Classification {
    mode: ConsultMode,
    reason: String,
}

fn classify(normalized: &str) -> Classification {
    let scores = score_modes(normalized);
    let best = MODE_PRIORITY
        .iter()
        .filter_map(|m| scores.iter().find(|s| s.mode == *m))
        .fold(None::<&ModeScore>, |best, s| match best {
            Some(b) if b.score >= s.score => Some(b),
            _ => Some(s),
        });
    match best {
        Some(b) if b.score > 0 => Classification {
            mode: b.mode,
            reason: format!(
                "matched {} cue(s): {}",
                b.mode,
                b.cues
                    .iter()
                    .take(4)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        },
        _ => Classification {
            mode: ConsultMode::Research,
            reason: "no strong cue in the question; defaulting to research (an open question)"
                .to_string(),
        },
    }
}

pub(crate) fn is_contested_question(question: &str, normalized: &str) -> Option<String> {
    let matched: Vec<&str> = CONTESTED_CUES
        .iter()
        .copied()
        .filter(|c| cue_matches(normalized, c))
        .collect();
    if !matched.is_empty() {
        return Some(format!(
            "contested or high-stakes wording ({})",
            matched
                .iter()
                .take(3)
                .map(|c| c.trim_end_matches('*'))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    (question.len() > LONG_QUESTION_BYTES).then(|| "a long, detailed question".to_string())
}

impl RoutingStrategy for ConsultationRouter {
    fn route(&self, input: &RouteInput<'_>) -> Result<RoutingDecision> {
        let normalized = normalize_question(input.question);
        let mut warnings = Vec::new();
        let mut signals = Vec::new();

        // ---- mode
        let mode = match input.mode {
            Some(m) => Routed::manual(m, "requested with --mode"),
            None => {
                let c = classify(&normalized);
                signals.push(c.reason.clone());
                Routed::auto(c.mode, c.reason)
            }
        };
        let contested = is_contested_question(input.question, &normalized);
        if let Some(c) = &contested {
            signals.push(c.clone());
        }
        let words = input.question.split_whitespace().count();
        let simple =
            contested.is_none() && words <= SIMPLE_QUESTION_WORDS && !input.question.contains('\n');

        // ---- length. The debate engine has its own format, so it is never shortened.
        let brief = if mode.value == ConsultMode::Debate {
            if input.length == Some(Length::Brief) {
                warnings.push(
                    "--length brief was ignored: the debate engine has its own fixed format"
                        .to_string(),
                );
                Routed::manual(false, "ignored in debate mode")
            } else {
                Routed::auto(false, "the debate engine has its own fixed format")
            }
        } else {
            match (input.length, asks_for_brevity(&normalized)) {
                (Some(Length::Brief), _) => Routed::manual(true, "requested with --length"),
                (Some(Length::Full), _) => Routed::manual(false, "requested with --length"),
                (None, Some(why)) => Routed::auto(true, why),
                (None, None) => Routed::auto(false, "the question does not ask for a short answer"),
            }
        };

        // ---- usable agents
        let info_of = |id: &ProviderId| input.available.iter().find(|a| &a.id == id);
        let mut skipped: Vec<Skipped> = Vec::new();
        let usable_ids: Vec<ProviderId> = input
            .available
            .iter()
            .filter(|a| a.usable.is_ok())
            .map(|a| a.id.clone())
            .collect();

        // ---- coordinator (manual one must be usable)
        if let Some(wanted) = &input.coordinator {
            match info_of(wanted) {
                Some(AgentInfo {
                    usable: Err(reason),
                    ..
                }) => {
                    return Err(OrchestraError::Config(format!(
                        "the requested coordinator `{wanted}` cannot be used: {reason}"
                    )));
                }
                None => {
                    return Err(OrchestraError::Config(format!(
                        "the requested coordinator `{wanted}` is not a known agent"
                    )));
                }
                Some(_) => {}
            }
        }

        // ---- participants
        let manual_agents = input.agents.is_some();
        let mut participants: Vec<ProviderId> = Vec::new();
        if let Some(wanted) = &input.agents {
            let mut seen = HashSet::new();
            for id in wanted {
                if !seen.insert(id.clone()) {
                    continue;
                }
                match info_of(id) {
                    Some(AgentInfo { usable: Ok(()), .. }) => participants.push(id.clone()),
                    Some(AgentInfo {
                        usable: Err(reason),
                        ..
                    }) => skipped.push(Skipped {
                        provider: id.clone(),
                        reason: format!("requested but unavailable: {reason}"),
                    }),
                    None => {
                        return Err(OrchestraError::Config(format!(
                            "`{id}` in --agents is not a known agent"
                        )));
                    }
                }
            }
            if participants.is_empty() {
                let detail = skipped
                    .iter()
                    .map(|s| format!("{}: {}", s.provider, s.reason))
                    .collect::<Vec<_>>()
                    .join("; ");
                return Err(OrchestraError::Config(format!(
                    "none of the requested agents can be used ({detail})"
                )));
            }
            if let Some(c) = &input.coordinator
                && !participants.contains(c)
            {
                return Err(OrchestraError::Config(format!(
                    "the coordinator `{c}` must be one of the participating agents ({}); add it \
                     to --agents or drop --coordinator",
                    join_ids(&participants)
                )));
            }
        } else {
            participants = usable_ids.clone();
        }
        if participants.is_empty() {
            let detail = input
                .available
                .iter()
                .filter_map(|a| a.usable.as_ref().err().map(|r| format!("{}: {r}", a.id)))
                .collect::<Vec<_>>()
                .join("; ");
            return Err(OrchestraError::Config(format!(
                "no agent is available for this consultation ({detail})"
            )));
        }

        // ---- coordinator choice
        let coordinator = match &input.coordinator {
            Some(c) => Routed::manual(c.clone(), "requested with --coordinator"),
            None => match &input.configured_coordinator {
                Some(c) if participants.contains(c) => Routed::auto(
                    c.clone(),
                    "the configured coordinator (general.coordinator)",
                ),
                Some(c) if manual_agents && usable_ids.contains(c) => Routed::auto(
                    participants[0].clone(),
                    format!(
                        "the configured coordinator ({c}) is not among the requested agents; \
                         using the first of them"
                    ),
                ),
                _ => Routed::auto(
                    participants[0].clone(),
                    "the configured coordinator is not available here; using the first usable agent",
                ),
            },
        };
        // Coordinator first, then the others in registry order.
        participants.retain(|p| p != &coordinator.value);
        participants.insert(0, coordinator.value.clone());

        // ---- rounds
        if let Some(r) = input.rounds
            && !(1..=MAX_DEBATE_ROUNDS).contains(&r)
        {
            return Err(OrchestraError::Config(format!(
                "rounds must be 1 or {MAX_DEBATE_ROUNDS}"
            )));
        }

        // ---- research (web access, depth, sources). Decided with the full set of candidates so a
        // search-capable agent can be chosen; depth may then shape the panel below.
        let can_search =
            |id: &ProviderId| input.available.iter().any(|a| &a.id == id && a.can_search);
        let candidates: Vec<(ProviderId, bool)> = participants
            .iter()
            .map(|p| (p.clone(), can_search(p)))
            .collect();
        let research = decide_research(&ResearchInput {
            question: input.question,
            mode: mode.value,
            brief: brief.value,
            request: &input.research,
            participants: &candidates,
            policy: &input.fetch_policy,
        })?;
        if research.is_active() {
            signals.push(format!("web research: {}", research.active.reason));
        }

        // Quick research is for speed: two agents (the coordinator and the one that searches),
        // unless the agents were named.
        if research.is_active()
            && research.depth.value == Depth::Quick
            && !manual_agents
            && participants.len() > 2
        {
            let mut keep = vec![coordinator.value.clone()];
            if let Some(d) = research
                .discovery_agent
                .clone()
                .filter(|d| d != &coordinator.value)
            {
                keep.push(d);
            }
            for p in &participants {
                if keep.len() >= 2 {
                    break;
                }
                if !keep.contains(p) {
                    keep.push(p.clone());
                }
            }
            for p in participants.iter().filter(|p| !keep.contains(p)) {
                skipped.push(Skipped {
                    provider: p.clone(),
                    reason:
                        "quick research uses two agents (use --depth standard or --agents for more)"
                            .to_string(),
                });
            }
            participants = keep;
        }

        // Lightweight path: a short, uncontested teaching question with nothing set by hand
        // gets one agent and no review. Anything explicit, and any research, disables it.
        let light = !manual_agents
            && input.rounds.is_none()
            && mode.value == ConsultMode::Learn
            && simple
            && !research.is_active()
            && participants.len() > 1;
        let participants = if light {
            Routed::auto(
                vec![coordinator.value.clone()],
                format!(
                    "a short, uncontested teaching question: {} alone is enough (use --agents or \
                     --rounds for an independent review)",
                    coordinator.value
                ),
            )
        } else if manual_agents {
            Routed::manual(participants, "requested with --agents")
        } else {
            // Respect the participant limit; the coordinator is always kept.
            let mut limited = participants;
            for extra in limited.split_off(MAX_PARTICIPANTS.min(limited.len())) {
                skipped.push(Skipped {
                    provider: extra,
                    reason: format!("participant limit of {MAX_PARTICIPANTS} reached"),
                });
            }
            let reason = format!(
                "all {} usable agent(s), so the answers can be compared independently",
                limited.len()
            );
            Routed::auto(limited, reason)
        };
        // Unusable agents that were not asked for are reported, too.
        if !manual_agents {
            for a in &input.available {
                if let Err(reason) = &a.usable {
                    skipped.push(Skipped {
                        provider: a.id.clone(),
                        reason: reason.clone(),
                    });
                }
            }
        }

        let n = participants.value.len();
        if mode.value == ConsultMode::Debate && n < 2 {
            return Err(OrchestraError::Config(
                "a debate needs at least two available agents (the coordinator and one reviewer); \
                 choose another --mode or make another agent available"
                    .into(),
            ));
        }
        let rounds = if n == 1 {
            match input.rounds {
                Some(r) => {
                    warnings.push(format!(
                        "--rounds {r} was ignored: only one agent takes part, so there is nobody \
                         to review its answer"
                    ));
                    Routed::manual(0, "ignored: a single participant has no reviewer")
                }
                None => Routed::auto(0, "a single participant has no reviewer"),
            }
        } else {
            match (input.rounds, &contested) {
                (Some(r), _) => Routed::manual(r, "requested with --rounds"),
                (None, _) if research.is_active() && research.depth.value == Depth::Deep => {
                    Routed::auto(2, "deep research cross-examines the evidence in two rounds")
                }
                (None, _) if research.is_active() && research.depth.value == Depth::Quick => {
                    Routed::auto(
                        0,
                        "quick research goes from the independent answers straight to the synthesis",
                    )
                }
                (None, Some(why)) => Routed::auto(2, format!("two rounds because of {why}")),
                (None, None) => {
                    Routed::auto(1, "one cross-review round is enough for this question")
                }
            }
        };
        if n == 1 {
            warnings.push(
                "single-agent consultation: the answer has had no independent review, so errors \
                 the model makes alone will not be caught"
                    .to_string(),
            );
        }

        Ok(RoutingDecision {
            mode,
            coordinator,
            participants,
            rounds,
            brief,
            research,
            skipped,
            warnings,
            signals,
            smart: None,
        })
    }
}

/// What the quality gate may require of this consultation, from the plan and the explicit settings.
fn quality_requirements(d: &RoutingDecision, o: &ConsultOptions) -> QualityRequirements {
    let research = &d.research;
    let planned_rounds = match d.smart.as_ref().map(|p| p.flow) {
        Some(Flow::Panel) => d.rounds.value,
        _ => 0,
    };
    QualityRequirements {
        research_required: research.is_active()
            && (o.research.web == WebMode::On || o.claim_check || !o.research.urls.is_empty()),
        verification_expected: research.is_active()
            && research.depth.value.profile().verifier_calls > 0,
        explicit_agents: if d.participants.source == Source::Manual {
            d.participants
                .value
                .iter()
                .map(ToString::to_string)
                .collect()
        } else {
            Vec::new()
        },
        explicit_rounds: (d.rounds.source == Source::Manual && d.rounds.value > 0)
            .then_some(d.rounds.value),
        planned_rounds,
    }
}

// --------------------------------------------------------------- the record

/// Consultation-specific facts stored with a [`DebateOutcome`]. The participants, individual
/// answers, review rounds, errors, timings and the final answer live in the outcome itself;
/// this holds how the consultation was routed and what could be read from the synthesis.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ConsultationMeta {
    pub schema: u32,
    pub mode: Option<ConsultMode>,
    /// Automatic choices and manual overrides, with reasons.
    pub routing: Option<RoutingDecision>,
    /// Which settings the user set explicitly (`mode`, `coordinator`, `agents`, `rounds`).
    pub manual_overrides: Vec<String>,
    /// `panel` (independent perspectives, cross-review, synthesis), `single` or `debate`.
    pub strategy: String,
    /// Review rounds that actually ran.
    pub rounds_run: u8,
    /// Calls made per agent.
    pub invocations: BTreeMap<String, usize>,
    /// Calls that failed.
    pub provider_failures: usize,
    /// Wall-clock milliseconds spent in each stage (`perspectives`, `review`, `cross_review`, ...).
    pub stage_ms: BTreeMap<String, u64>,
    /// Tokens as reported by the CLIs, only when at least one call reported them.
    pub token_usage: Option<TokenTotals>,
    // The next five are read from the coordinator's synthesis by section heading. They are
    // `None` when the synthesis does not have that section (or there is no synthesis).
    pub recommendation: Option<String>,
    pub agreements: Option<String>,
    pub disagreements: Option<String>,
    pub assumptions: Option<String>,
    pub evidence_limitations: Option<String>,
    /// Sources, claims, citations and limitations of the web research, when there was any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub research: Option<ResearchReport>,
    /// True when the synthesis lists disagreements that it did not resolve.
    pub unresolved_disagreement: bool,
    /// What the smart orchestration decided while it ran, what it used and how the quality
    /// checks came out. Absent for consultations made without a strategy and for old records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestration: Option<OrchestrationRecord>,
    /// What memory contributed to this consultation: what was recalled, what was left out and
    /// why, and what it cost. Absent when memory was not used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<crate::memory::context::ContextReport>,
    /// The reply language and characters the user asked for, when any (the profile text is not stored here).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<crate::style::StyleRecord>,
}

/// Sum of the token counts that CLIs reported. Calls without metrics are not guessed.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TokenTotals {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub calls_reporting: usize,
    pub calls_total: usize,
}

/// The routing facts known before anything runs.
pub(crate) fn seed_meta(decision: &RoutingDecision) -> ConsultationMeta {
    ConsultationMeta {
        schema: META_SCHEMA,
        mode: Some(decision.mode.value),
        routing: Some(decision.clone()),
        manual_overrides: decision.manual_settings(),
        strategy: match (
            decision.smart.as_ref().map(|p| p.flow),
            decision.mode.value,
            decision.participants.value.len(),
        ) {
            (Some(flow), _, _) => flow.as_str(),
            (None, ConsultMode::Debate, _) => "debate",
            (None, _, 1) => "single",
            _ => "panel",
        }
        .to_string(),
        ..Default::default()
    }
}

fn style_record(options: &ConsultOptions) -> Option<crate::style::StyleRecord> {
    options
        .style
        .as_ref()
        .filter(|s| !s.is_empty())
        .map(|s| s.record())
}

/// Fills in what can only be known after the run.
pub(crate) fn complete_meta(
    mut meta: ConsultationMeta,
    outcome: &DebateOutcome,
) -> ConsultationMeta {
    meta.rounds_run = outcome
        .steps
        .iter()
        .filter(|s| s.ok && matches!(s.phase, Role::Review | Role::CrossReview))
        .map(|s| s.round)
        .max()
        .unwrap_or(0);
    meta.invocations.clear();
    for s in &outcome.steps {
        *meta.invocations.entry(s.agent.to_string()).or_default() += 1;
    }
    meta.provider_failures = outcome.steps.iter().filter(|s| !s.ok).count();

    // Wall-clock span of each stage: parallel calls overlap, so this is not the sum.
    let mut spans: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    for s in &outcome.steps {
        let key = match s.phase {
            Role::Planning => "research_planning",
            Role::Verification => "claim_verification",
            Role::Perspective => "perspectives",
            Role::Proposal => "proposal",
            Role::Review => "review",
            Role::CrossReview => "cross_review",
            Role::Synthesis => "synthesis",
        };
        let end = s.offset_ms + s.duration_ms;
        spans
            .entry(key.to_string())
            .and_modify(|(a, b)| {
                *a = (*a).min(s.offset_ms);
                *b = (*b).max(end);
            })
            .or_insert((s.offset_ms, end));
    }
    meta.stage_ms = spans.into_iter().map(|(k, (a, b))| (k, b - a)).collect();

    let reporting: Vec<_> = outcome
        .steps
        .iter()
        .filter_map(|s| s.usage.as_ref())
        .collect();
    meta.token_usage = (!reporting.is_empty()).then(|| TokenTotals {
        input_tokens: reporting.iter().filter_map(|u| u.input_tokens).sum(),
        output_tokens: reporting.iter().filter_map(|u| u.output_tokens).sum(),
        calls_reporting: reporting.len(),
        calls_total: outcome.steps.len(),
    });

    if outcome.synthesized {
        let answer = &outcome.final_answer;
        meta.recommendation = section(answer, &["recommendation", "recommended solution"]);
        meta.agreements = section(answer, &["areas of agreement"]);
        meta.disagreements = section(answer, &["significant disagreements"]);
        meta.assumptions = section(answer, &["assumptions"]);
        meta.evidence_limitations = section(
            answer,
            &["evidence limitations", "limitations", "unresolved risks"],
        );
        meta.unresolved_disagreement = meta.disagreements.as_deref().is_some_and(|d| {
            let d = d.trim_start_matches(['-', '*', ' ']).to_lowercase();
            !(d.starts_with("none") || d.starts_with("no significant") || d.starts_with("no major"))
        });
    }
    meta
}

/// Text under the first Markdown heading whose title starts with one of `titles`
/// (case-insensitive), up to the next heading of the same or a higher level.
pub(crate) fn section(markdown: &str, titles: &[&str]) -> Option<String> {
    let mut lines = markdown.lines();
    let level = loop {
        let line = lines.next()?;
        let trimmed = line.trim_start();
        let hashes = trimmed.chars().take_while(|c| *c == '#').count();
        if hashes == 0 || !trimmed[hashes..].starts_with(' ') {
            continue;
        }
        let title = trimmed[hashes..]
            .trim()
            .trim_matches(['*', '`', ':'])
            .to_lowercase();
        if titles.iter().any(|t| title.starts_with(t)) {
            break hashes;
        }
    };
    let mut body = Vec::new();
    for line in lines {
        let trimmed = line.trim_start();
        let hashes = trimmed.chars().take_while(|c| *c == '#').count();
        if hashes > 0 && hashes <= level && trimmed[hashes..].starts_with(' ') {
            break;
        }
        body.push(line);
    }
    let text = body.join("\n").trim().to_string();
    (!text.is_empty()).then_some(text)
}

// ------------------------------------------------------------ presentation

/// Header, routing, timings and the answer, for `show`.
pub(crate) fn render_summary(o: &DebateOutcome, meta: &ConsultationMeta) -> String {
    let mut out = String::new();
    let mode = meta.mode.map_or("?".to_string(), |m| m.to_string());
    let _ = writeln!(out, "Consultation {} ({}) · mode {mode}", o.id, o.status);
    let _ = writeln!(out, "Question:     {}", o.question.trim());
    let _ = writeln!(
        out,
        "Coordinator:  {} · participants: {} · {} review round(s) planned, {} run",
        o.coordinator,
        join_ids(&participants_of(o)),
        o.rounds_planned,
        meta.rounds_run
    );
    let _ = writeln!(
        out,
        "Started:      {} · {} calls · {:.1}s",
        format_utc(o.created_at_ms),
        o.calls,
        o.duration_ms as f64 / 1000.0
    );
    if !meta.manual_overrides.is_empty() {
        let _ = writeln!(out, "Set by you:   {}", meta.manual_overrides.join(", "));
    }
    if let Some(r) = &meta.routing {
        let _ = writeln!(out, "\nRouting:");
        for line in r.render().lines() {
            let _ = writeln!(out, "  {line}");
        }
    }
    if let Some(rec) = &meta.orchestration {
        let _ = writeln!(out, "\nOrchestration (what happened while it ran):");
        for line in rec.render().lines() {
            let _ = writeln!(out, "  {line}");
        }
    }
    for s in &o.skipped {
        let _ = writeln!(out, "Not consulted: {} ({})", s.provider, s.reason);
    }
    if let Some(e) = &o.error {
        let _ = writeln!(out, "Error:        {e}");
    }
    for w in &o.warnings {
        let _ = writeln!(out, "Warning:      {w}");
    }
    if let Some(t) = &meta.token_usage {
        let _ = writeln!(
            out,
            "Tokens:       in {} / out {} (reported by {} of {} calls)",
            t.input_tokens, t.output_tokens, t.calls_reporting, t.calls_total
        );
    }
    let _ = writeln!(out, "\nTimings:");
    for s in &o.steps {
        let _ = writeln!(
            out,
            "  {:<22} round {} {:<8} {:>6.1}s  {}",
            s.phase.label(),
            s.round,
            s.agent,
            s.duration_ms as f64 / 1000.0,
            match (&s.error_kind, s.ok) {
                (_, true) => "ok".to_string(),
                (Some(k), false) => format!("FAILED ({k})"),
                (None, false) => "FAILED".to_string(),
            }
        );
    }
    let heading = if o.synthesized {
        "Answer"
    } else if o.final_answer.is_empty() {
        "No result"
    } else {
        "Best available answer (NOT a synthesis; the consultation did not finish)"
    };
    let _ = writeln!(out, "\n--- {heading} ---\n{}", o.final_answer.trim_end());
    out
}

/// Every answer, review and synthesis in order.
pub(crate) fn render_transcript(o: &DebateOutcome, meta: &ConsultationMeta) -> String {
    let mut out = format!(
        "# Consultation transcript {} (mode {})\n",
        o.id,
        meta.mode.map_or("?".to_string(), |m| m.to_string())
    );
    let section_for = |out: &mut String, title: String, steps: Vec<&Step>| {
        if steps.is_empty() {
            return;
        }
        let _ = write!(out, "\n## {title}\n");
        for s in steps {
            match (&s.text, &s.error) {
                (Some(t), _) => {
                    let _ = write!(
                        out,
                        "\n### {} ({:.1}s)\n\n{}\n",
                        s.agent,
                        s.duration_ms as f64 / 1000.0,
                        t.trim_end()
                    );
                }
                (None, e) => {
                    let _ = write!(
                        out,
                        "\n### {} FAILED\n\n{}\n",
                        s.agent,
                        e.as_deref().unwrap_or("no response")
                    );
                }
            }
        }
    };
    let by = |phase: Role, round: Option<u8>| -> Vec<&Step> {
        o.steps
            .iter()
            .filter(|s| s.phase == phase && round.is_none_or(|r| s.round == r))
            .collect()
    };
    section_for(
        &mut out,
        "Independent perspectives".into(),
        by(Role::Perspective, None),
    );
    section_for(
        &mut out,
        "Initial proposal".into(),
        by(Role::Proposal, None),
    );
    for round in 1..=o.rounds_planned.max(meta.rounds_run) {
        let phase = if round == 1 {
            Role::Review
        } else {
            Role::CrossReview
        };
        section_for(
            &mut out,
            format!("Round {round}: {}", phase.label()),
            by(phase, Some(round)),
        );
    }
    section_for(
        &mut out,
        "Final synthesis".into(),
        by(Role::Synthesis, None),
    );
    out
}

fn participants_of(o: &DebateOutcome) -> Vec<ProviderId> {
    let mut all = vec![o.coordinator.clone()];
    all.extend(
        o.participants
            .iter()
            .filter(|p| **p != o.coordinator)
            .cloned(),
    );
    all
}

// ---------------------------------------------------------------- prompts

const NO_FABRICATION: &str = "Do not invent URLs, citations, studies, statistics, prices, model \
numbers or quotations. You cannot assume that you have verified anything online: keep what you \
know from general knowledge apart from what would need checking, and say so plainly. Keep a \
source reference only if it appears in this message.";

const STAGE_A: &str = "Start with a short note headed `Understanding`: the main question as you \
read it, the criteria that matter, your key assumptions, the information that is missing, and \
whether outside evidence (current prices, recent publications, measurements) would be needed to \
answer reliably.";

const BREVITY: &str = "The user wants a SHORT answer. Respect any length the question asks for \
(for example a number of sentences); if it names none, use a few sentences. Skip long analysis.";

const BRIEF_STAGE_A: &str = "Before answering, state in one line any assumption or missing \
information that really matters.";

/// The sections of the final answer in the brief style.
const BRIEF_SECTIONS: &str = "`## Answer` (exactly the length the question asks for, otherwise a \
short paragraph at most), `## Significant disagreements` (one or two lines, or `None`), \
`## Assumptions` (one line) and `## Evidence limitations` (one line)";

fn stage_a(brief: bool) -> &'static str {
    if brief { BRIEF_STAGE_A } else { STAGE_A }
}

fn sections(mode: ConsultMode, brief: bool) -> &'static str {
    if brief {
        BRIEF_SECTIONS
    } else {
        synthesis_sections(mode)
    }
}

fn brevity(brief: bool) -> String {
    if brief {
        format!("{BREVITY}\n\n")
    } else {
        String::new()
    }
}

fn mode_brief(mode: ConsultMode) -> &'static str {
    match mode {
        ConsultMode::Decision => {
            "This is a decision-support question. Identify the available alternatives and the \
             evaluation criteria. Separate objective evidence from personal preferences. Compare \
             advantages and disadvantages, costs, risks and trade-offs. Point out missing \
             information that could change the answer. Give a clear recommendation if the \
             information justifies one, and explain how different priorities would change it. Do \
             not invent numerical probabilities or confidence percentages."
        }
        ConsultMode::Research => {
            "This is a research question. Identify the central research question. Separate \
             established facts, hypotheses and opinions. Point out contradictions and the limits \
             of the available information. State explicitly that nothing here has been verified \
             against external sources unless the message itself contains the source."
        }
        ConsultMode::Review => {
            "This is an evaluation of a product, technology, service, article or claim. \
             Identify the evaluation criteria, then the strengths and weaknesses. Compare relevant \
             alternatives. Keep specifications apart from subjective assessments. Flag \
             information that may be outdated or unverified (prices, versions, availability). \
             Give a recommendation if the information justifies one."
        }
        ConsultMode::Learn => {
            "The user wants to learn this. Explain it from the appropriate level (say which level \
             you assumed), as a logical progression, using clear analogies and examples where they \
             help. Name the prerequisite knowledge."
        }
        ConsultMode::Debate => "Argue your position clearly and preserve real disagreements.",
    }
}

/// Section list of the final answer, per mode. Every list ends with the shared sections the
/// consultation record reads back (agreement, disagreement, assumptions, limitations).
fn synthesis_sections(mode: ConsultMode) -> &'static str {
    match mode {
        ConsultMode::Decision => {
            "`## Answer`, `## Recommendation`, `## Alternatives and criteria`, `## Objective \
             evidence vs. personal preferences`, `## Trade-offs, costs and risks`, `## How \
             priorities change the recommendation`, `## Areas of agreement`, `## Significant \
             disagreements`, `## Assumptions` and `## Evidence limitations`"
        }
        ConsultMode::Research => {
            "`## Answer`, `## Central question`, `## Established facts`, `## Hypotheses and open \
             questions`, `## Opinions and contested claims`, `## Contradictions`, `## Source \
             references` (only references that appear in the question or the answers; otherwise \
             say none were provided), `## Areas of agreement`, `## Significant disagreements`, \
             `## Assumptions` and `## Evidence limitations` (state that nothing was verified \
             externally, unless it was)"
        }
        ConsultMode::Review => {
            "`## Answer`, `## Recommendation`, `## Evaluation criteria`, `## Strengths and \
             weaknesses`, `## Alternatives compared`, `## Specifications vs. subjective \
             assessments`, `## Outdated or unverified information`, `## Areas of agreement`, \
             `## Significant disagreements`, `## Assumptions` and `## Evidence limitations`"
        }
        ConsultMode::Learn => {
            "`## Lesson` (one coherent lesson: a logical progression with analogies and \
             examples), `## Prerequisites`, `## Corrections made after review` (errors or unclear \
             passages the reviewers found and how you fixed them), `## Areas of agreement`, \
             `## Significant disagreements`, `## Assumptions` and `## Evidence limitations`"
        }
        ConsultMode::Debate => {
            "`## Recommended solution`, `## Technical reasoning`, `## Areas of agreement`, \
             `## Significant disagreements`, `## Unresolved risks` and `## Suggested next steps`"
        }
    }
}

const SYNTHESIS_RULES: &str = "Directly answer the user's question first. Do not count votes: \
agreement between assistants is not proof, because they can share the same blind spots and \
mistakes. Keep real disagreements, attributing each substantive objection to the assistant who \
raised it and saying whether you resolved it and why; never smooth one over to look consistent. \
Separate evidence from opinion. If reliable evidence is not enough to conclude, say so \
explicitly instead of guessing, and leave out a recommendation that the information does not \
justify.";

fn perspective_prompt(mode: ConsultMode, brief: bool, question: &str) -> String {
    format!(
        "You are one of several independent AI assistants consulted on the same question; the \
         others answer separately and you cannot see their work. Give your own best answer.\n\n\
         {}\n\n{}{}\n\n{NO_FABRICATION}\n\n{READ_ONLY_NOTE}\n\n<question>\n{question}\n</question>",
        stage_a(brief),
        brevity(brief),
        mode_brief(mode)
    )
}

fn single_prompt(mode: ConsultMode, brief: bool, question: &str) -> String {
    format!(
        "You are the only AI assistant consulted on this question, so there will be no independent \
         review. Write the complete final answer in Markdown with exactly these sections: {}. \
         Under Areas of agreement and Significant disagreements write `None: no other assistant \
         was consulted`, and in Evidence limitations say that no independent review took place.\n\n\
         {}\n\n{}{}\n\n{SYNTHESIS_RULES}\n\n{NO_FABRICATION}\n\n{READ_ONLY_NOTE}\n\n\
         <question>\n{question}\n</question>",
        sections(mode, brief),
        stage_a(brief),
        brevity(brief),
        mode_brief(mode)
    )
}

fn answers_block(
    tag: &str,
    names: &[String],
    slots: &[Slot],
    skip: Option<usize>,
    cap: usize,
) -> String {
    let mut out = String::new();
    for (i, slot) in slots.iter().enumerate() {
        if Some(i) == skip {
            continue;
        }
        match slot {
            Slot::Text(t) => {
                let _ = writeln!(
                    out,
                    "<{tag} from=\"{}\">\n{}\n</{tag}>",
                    names[i],
                    clip(t, cap)
                );
            }
            Slot::Failed(_) | Slot::Skipped => {
                let _ = writeln!(out, "<{tag} from=\"{}\" status=\"no response\"/>", names[i]);
            }
        }
    }
    out
}

fn count_text(slots: &[Slot]) -> usize {
    slots.iter().filter(|s| matches!(s, Slot::Text(_))).count()
}

fn review_prompt(
    mode: ConsultMode,
    brief: bool,
    question: &str,
    own: usize,
    names: &[String],
    perspectives: &[Slot],
) -> String {
    let cap = review_cap(count_text(perspectives));
    let mine = match &perspectives[own] {
        Slot::Text(t) => clip(t, cap),
        _ => String::new(),
    };
    let others = answers_block("perspective", names, perspectives, Some(own), cap);
    format!(
        "You are one of several independent AI assistants consulted on a question. Everyone has \
         now answered separately; read the other assistants' answers below next to your own and \
         cross-review them. (1) Identify mistakes. (2) Challenge unsupported claims. (3) Detect \
         contradictions between the answers. (4) Point out criteria or considerations that are \
         missing. (5) Explain each disagreement specifically. (6) Say what you would change in \
         your own answer if the others convinced you, and what you still hold. Agreement alone is \
         not evidence. Be specific and concise; do not rewrite the answers.{}\n\n\
         {}\n\n{NO_FABRICATION}\n\n{READ_ONLY_NOTE}\n\n<question>\n{question}\n</question>\n\n\
         <your_answer>\n{mine}\n</your_answer>\n\n<other_answers>\n{others}</other_answers>",
        review_length(brief),
        mode_brief(mode)
    )
}

fn review_length(brief: bool) -> &'static str {
    if brief {
        " Keep it to at most five short bullet points."
    } else {
        ""
    }
}

fn cross_review_prompt(
    mode: ConsultMode,
    brief: bool,
    question: &str,
    own: usize,
    names: &[String],
    perspectives: &[Slot],
    round_one: &[Slot],
) -> String {
    let cap = review_cap(count_text(perspectives) + count_text(round_one));
    let mine = match &perspectives[own] {
        Slot::Text(t) => clip(t, cap),
        _ => String::new(),
    };
    let your_review = match &round_one[own] {
        Slot::Text(t) => clip(t, cap),
        _ => String::new(),
    };
    let others = answers_block("perspective", names, perspectives, Some(own), cap);
    let reviews = answers_block("review", names, round_one, Some(own), cap);
    format!(
        "You are one of several independent AI assistants consulted on a question. This is the \
         second cross-review round. You answered, then reviewed the other answers. Now read the \
         other assistants' round-1 reviews too and reconsider: which of their points change your \
         mind and why, which of your own points you withdraw, and which disagreements you still \
         hold. Do not repeat points that are already settled. Agreement alone is not evidence.{}\n\n\
         {NO_FABRICATION}\n\n{READ_ONLY_NOTE}\n\n<question>\n{question}\n</question>\n\n\
         <your_answer>\n{mine}\n</your_answer>\n\n<other_answers>\n{others}</other_answers>\n\n\
         <your_round_1_review>\n{your_review}\n</your_round_1_review>\n\n\
         <other_reviews_round_1>\n{reviews}</other_reviews_round_1>\n\n{}",
        review_length(brief),
        mode_brief(mode)
    )
}

fn synthesis_prompt(
    mode: ConsultMode,
    brief: bool,
    question: &str,
    names: &[String],
    perspectives: &[Slot],
    reviews: &[Vec<Slot>],
) -> String {
    let items = count_text(perspectives) + reviews.iter().map(|r| count_text(r)).sum::<usize>();
    let cap = review_cap(items);
    let mut block = answers_block("perspective", names, perspectives, None, cap);
    for (r, slots) in reviews.iter().enumerate() {
        let kind = if r == 0 { "review" } else { "cross_review" };
        let _ = writeln!(block, "<round number=\"{}\" kind=\"{kind}\">", r + 1);
        block.push_str(&answers_block(kind, names, slots, None, cap));
        let _ = writeln!(block, "</round>");
    }
    let roster = names.join(", ");
    format!(
        "You are the coordinator of a panel of independent AI assistants, and their answers and \
         reviews are in. The panel is exactly: {roster}. Do not mention or speculate about any \
         other assistant, and note any whose entry is marked `no response`. Write the FINAL answer \
         in Markdown with exactly these sections: {}. {SYNTHESIS_RULES}\n\n{}{}\n\n{NO_FABRICATION}\n\n\
         {READ_ONLY_NOTE}\n\n<question>\n{question}\n</question>\n\n<panel>\n{block}</panel>",
        sections(mode, brief),
        brevity(brief),
        mode_brief(mode)
    )
}

// ----------------------------------------------------------------- engine

/// What the user asked for. Every field except the question is optional: `None` means auto.
#[derive(Debug, Clone, Default)]
pub struct ConsultOptions {
    pub question: String,
    pub mode: Option<ConsultMode>,
    pub coordinator: Option<String>,
    pub agents: Option<Vec<String>>,
    /// 1 or 2.
    pub rounds: Option<u8>,
    /// `None`: brief when the question asks for a short answer, otherwise the full report.
    pub length: Option<Length>,
    pub cwd: Option<PathBuf>,
    /// Per-call timeout; defaults to the provider / general timeout.
    pub timeout: Option<Duration>,
    pub overrides: BTreeMap<String, ModelOverride>,
    /// Agents that must not take part (the MCP host). They are reported as not consulted.
    pub exclude: Vec<String>,
    pub isolate: bool,
    pub events: Option<UnboundedSender<DebateEvent>>,
    /// Web research settings: all automatic by default.
    pub research: ResearchRequest,
    /// Verify the text of `question` as ONE claim instead of answering it: find and retrieve
    /// sources, have agents judge the passages, and report the claim's status. No panel answers,
    /// no cross-review, no synthesis. Used by the MCP `verify_claim` tool.
    pub claim_check: bool,
    /// How much AI work to use. `None`: the fixed Milestone 4/5 orchestration (every usable agent
    /// answers, then the review rounds, then the synthesis), as before strategies existed.
    pub strategy: Option<Strategy>,
    /// Most agent calls this consultation may start.
    pub max_agent_calls: Option<usize>,
    /// Wall-clock limit for the whole consultation.
    pub max_duration: Option<Duration>,
    /// Most of this consultation's agent calls running at the same time.
    pub max_parallel: Option<usize>,
    /// What this consultation may recall and remember. `None`: memory is not used (as before
    /// Milestone 7).
    pub memory: Option<MemoryRequest>,
    /// Reply language, named characters and conversational reviews (a user preference written into the prompts).
    pub style: Option<Arc<crate::style::PromptStyle>>,
    /// Replaces the page fetcher. For tests, which fetch from a local server; the product
    /// always builds its own fetcher from `[research]` in the config.
    #[doc(hidden)]
    pub fetcher: Option<Arc<SourceFetcher>>,
}

/// The evidence part of a prompt: the retrieved pages, the claim checks once they exist, and a
/// note when research was part of the task but nothing could be retrieved.
#[derive(Default, Clone)]
struct Evidence {
    block: String,
    table: String,
    note: String,
    /// Recalled memory, placed before the research evidence in every answering prompt.
    memory: String,
}

const NO_EVIDENCE_NOTE: &str = "NOTE: web research was part of this task but no source could be retrieved, so nothing below is verified \
against external sources. Say so plainly, and mark any fact you give from memory `(unverified)`.";

const SYNTHESIS_EVIDENCE_RULES: &str = "Write the answer so that it directly answers the question and cites the retrieved sources with [n] \
(only numbers listed in <evidence>). Keep apart what the evidence establishes (claims marked SUPPORTED), what is contested \
(CONTRADICTED or MIXED_EVIDENCE: say which sources disagree), what could not be checked (INSUFFICIENT_EVIDENCE, NOT_CHECKED, or anything \
from memory marked unverified), and your own interpretation. Do not claim a source says something it does not. Several sources that repeat \
one origin are one source. If the evidence is not enough to conclude, say so instead of guessing. Do not write a Sources section and do not \
mention the <claim_checks> tags: the sources and claim statuses are added automatically.";

impl Evidence {
    fn from_run(run: &ResearchRun, question: &str) -> Self {
        let block = run.evidence(question);
        let note = if block.is_empty() {
            NO_EVIDENCE_NOTE.to_string()
        } else {
            String::new()
        };
        Self {
            block,
            table: String::new(),
            note,
            memory: String::new(),
        }
    }

    fn active(&self) -> bool {
        !self.block.is_empty() || !self.note.is_empty()
    }

    /// The recalled memory (if any) followed by `section`.
    fn with_memory(&self, section: String) -> String {
        match (self.memory.is_empty(), section.is_empty()) {
            (true, _) => section,
            (false, true) => self.memory.clone(),
            (false, false) => format!("{}\n\n{section}", self.memory),
        }
    }

    /// For the independent answers: the evidence, how to use it, and the request for a claims block.
    fn for_answer(&self) -> String {
        self.with_memory({
            if !self.block.is_empty() {
                format!("{EVIDENCE_RULES}\n\n{CLAIMS_REQUEST}\n\n{}", self.block)
            } else {
                self.note.clone()
            }
        })
    }

    /// For the reviews: check the answers' citations against the evidence.
    fn for_review(&self) -> String {
        self.with_memory({
        if !self.block.is_empty() {
            format!(
                "Check every citation in the answers against the evidence: say which [n] does not support the statement it is attached to, \
                 which important statements have no support, and where the answers read a source differently.\n\n{EVIDENCE_RULES}\n\n{}{}",
                self.block, self.table
            )
        } else {
            self.note.clone()
        }
    })
    }

    fn for_synthesis(&self) -> String {
        self.with_memory({
            if !self.block.is_empty() {
                format!("{SYNTHESIS_EVIDENCE_RULES}\n\n{}{}", self.block, self.table)
            } else {
                self.note.clone()
            }
        })
    }
}

/// Adds research limitations to a result that has no research report: as warnings, and as a
/// section at the end of a non-empty answer.
fn warnings_extra(outcome: &mut DebateOutcome, limitations: &[String]) {
    outcome
        .warnings
        .extend(limitations.iter().map(|l| format!("research: {l}")));
    if !outcome.final_answer.trim().is_empty() {
        outcome
            .final_answer
            .push_str("\n\n## Research limitations\n\n");
        for l in limitations {
            outcome.final_answer.push_str(&format!("- {l}\n"));
        }
    }
}

/// Puts `section` into a prompt just before the question, where the template's own text ends and
/// the user's text begins.
fn with_evidence(prompt: String, section: &str) -> String {
    if section.is_empty() {
        return prompt;
    }
    match prompt.find("<question>") {
        Some(i) => format!("{}{section}\n\n{}", &prompt[..i], &prompt[i..]),
        None => prompt,
    }
}

struct Panel<'a> {
    question: &'a str,
    mode: ConsultMode,
    brief: bool,
    providers: Vec<Arc<dyn AgentProvider>>,
    names: Vec<String>,
    rounds: u8,
    runner: Runner,
    options: &'a ConsultOptions,
    /// Web research state, kept here so a failed or cancelled run can still be reported.
    research: tokio::sync::Mutex<Option<ResearchRun>>,
    fetcher: Option<Arc<SourceFetcher>>,
    research_time_budget: Duration,
    flow: Flow,
    /// The plan, with the strategy, the escalation order and the question features.
    plan: Option<SmartPlan>,
    smart: Option<Arc<SmartRun>>,
    /// The recalled-memory block placed before the question in every answering prompt (empty when
    /// nothing was recalled).
    memory_text: String,
}

impl Panel<'_> {
    /// Adds a line to the decision trace, when the consultation keeps one.
    fn trace(&self, kind: TraceKind, message: impl Into<String>) {
        if let Some(smart) = &self.smart {
            smart.trace.event(kind, message);
        }
    }

    /// A read-only request carrying the settings every call of this consultation shares.
    fn request(&self, prompt: String) -> AgentRequest {
        let mut r = AgentRequest::new(prompt);
        r.cwd = self.options.cwd.clone();
        r.timeout = self.options.timeout;
        r.isolate = self.options.isolate;
        r
    }
}

struct PanelResult {
    synthesized: bool,
    initial: String,
    final_answer: String,
    warnings: Vec<String>,
    any_failure: bool,
    /// Everything the plan called for was done (a flow that stopped on purpose, because nothing
    /// called for more, has met its plan; one that ran out of budget has not).
    plan_met: bool,
    /// One agent wrote the answer and nobody reviewed it.
    single_agent: bool,
}

impl Orchestrator {
    /// Decides how a consultation would run, without calling any agent. This is what
    /// `consult --plan` prints.
    pub fn plan_consultation(&self, options: &ConsultOptions) -> Result<RoutingDecision> {
        self.plan_with(&ConsultationRouter, options)
    }

    pub fn plan_with(
        &self,
        router: &dyn RoutingStrategy,
        options: &ConsultOptions,
    ) -> Result<RoutingDecision> {
        if options.question.trim().is_empty() {
            return Err(OrchestraError::Config("the question is empty".into()));
        }
        if options.question.len() > QUESTION_MAX_BYTES {
            return Err(OrchestraError::PromptTooLarge {
                len: options.question.len(),
                max: QUESTION_MAX_BYTES,
                target: "a consultation question".to_string(),
            });
        }
        if let Some(r) = options.rounds
            && !(1..=MAX_DEBATE_ROUNDS).contains(&r)
        {
            return Err(OrchestraError::Config(format!(
                "rounds must be auto, 1 or {MAX_DEBATE_ROUNDS}"
            )));
        }
        let registry = self.registry();
        let ctx = self.context();
        let mut excluded = Vec::new();
        for name in &options.exclude {
            excluded.push(registry.resolve(name)?.id().clone());
        }
        let available: Vec<AgentInfo> = registry
            .all()
            .iter()
            .map(|p| AgentInfo {
                id: p.id().clone(),
                usable: if excluded.contains(p.id()) {
                    Err(
                        "hosts this MCP server; calling it would start a second copy of the \
                         agent you are talking to"
                            .to_string(),
                    )
                } else {
                    p.availability(ctx)
                },
                can_search: p.research_capabilities().can_discover(),
            })
            .collect();
        let coordinator = options
            .coordinator
            .as_deref()
            .map(|n| registry.resolve(n).map(|p| p.id().clone()))
            .transpose()?;
        let agents = options
            .agents
            .as_ref()
            .map(|names| {
                names
                    .iter()
                    .map(|n| registry.resolve(n).map(|p| p.id().clone()))
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?;
        let configured_coordinator = registry
            .resolve(&self.config().general.coordinator)
            .ok()
            .map(|p| p.id().clone());
        let route_input = RouteInput {
            question: &options.question,
            mode: options.mode,
            coordinator,
            agents,
            rounds: options.rounds,
            length: options.length,
            research: options.research.clone(),
            fetch_policy: options
                .fetcher
                .as_ref()
                .map(|f| f.policy().clone())
                .unwrap_or_else(|| self.config().research.fetch_policy()),
            available,
            configured_coordinator,
        };
        let decision = router.route(&route_input)?;
        // Smart orchestration runs when a strategy or any limit is in force. Without either, the
        // router's decision stands untouched, exactly as before Milestone 6.
        let smart = &self.config().smart;
        let limited = options.max_agent_calls.is_some()
            || options.max_duration.is_some()
            || options.max_parallel.is_some()
            || smart.max_agent_calls.is_some()
            || smart.max_duration_secs.is_some()
            || smart.max_parallel.is_some();
        if options.strategy.is_none() && !limited {
            return Ok(decision);
        }
        let (snapshot, history_note) = self.profile_snapshot();
        let engine = SmartStrategyEngine::new(SmartSettings::from_config(self.config()), snapshot);
        let mut decision = engine.apply(
            &route_input,
            decision,
            &StrategyRequest {
                strategy: options.strategy,
                max_agent_calls: options.max_agent_calls,
                max_duration: options.max_duration,
                max_parallel: options.max_parallel,
                claim_check: options.claim_check,
            },
            self.config().general.timeout_secs,
        )?;
        if let (Some(note), Some(plan)) = (history_note, decision.smart.as_mut()) {
            plan.profile_notes.push(note);
        }
        Ok(decision)
    }

    /// Runs a consultation with the built-in router.
    pub async fn consult(
        &self,
        options: ConsultOptions,
        cancel: &CancellationToken,
    ) -> Result<DebateOutcome> {
        self.consult_with(&ConsultationRouter, options, cancel)
            .await
    }

    /// Runs a consultation, routing with `router`.
    pub async fn consult_with(
        &self,
        router: &dyn RoutingStrategy,
        options: ConsultOptions,
        cancel: &CancellationToken,
    ) -> Result<DebateOutcome> {
        self.check_not_nested()?;
        let mut options = options;
        if options.claim_check {
            options.mode = Some(ConsultMode::Research);
            options.rounds = None;
        }
        let mut decision = self.plan_with(router, &options)?;
        if options.claim_check {
            if !decision.research.is_active() {
                return Err(OrchestraError::Config(
                    "a claim check needs web research: give --url or use an agent that can search the web, and do not use --web off".into(),
                ));
            }
            decision.rounds = Routed::auto(0, "a claim check has no review rounds");
        }
        let overrides = self.resolve_overrides(&options.overrides)?;
        let mode = decision.mode.value;
        let memory_budget = self.memory_budget(&options);
        let smart: Option<Arc<SmartRun>> = decision.smart.as_ref().map(|plan| {
            let mut limits = plan.limits.clone();
            limits.context_tokens = memory_budget;
            let run = SmartRun::new(
                limits,
                quality_requirements(&decision, &options),
                plan.strategy.is_some(),
                plan.profile_notes.clone(),
            );
            run.trace.event(
                TraceKind::Stage,
                format!(
                    "planned: {} strategy, {} flow, {} agent(s), at most {} call(s), limits: {}",
                    plan.strategy_value()
                        .map_or("no".to_string(), |s| s.to_string()),
                    plan.flow,
                    decision.participants.value.len(),
                    plan.max_calls,
                    plan.limits.describe()
                ),
            );
            run
        });
        // Recall (read-only). A session that cannot be used is refused here, before any agent runs.
        let prepared: Option<Prepared> = self
            .prepare_memory(&options, &decision, smart.as_ref())
            .await?;
        info!(
            mode = %mode,
            participants = decision.participants.value.len(),
            rounds = decision.rounds.value,
            "starting consultation"
        );

        if mode == ConsultMode::Debate {
            // The Milestone 2 engine, unchanged, with the routing facts attached to its record.
            let coordinator = decision.coordinator.value.clone();
            let peers: Vec<String> = decision
                .participants
                .value
                .iter()
                .filter(|p| **p != coordinator)
                .map(ToString::to_string)
                .collect();
            let mut meta = seed_meta(&decision);
            meta.strategy = "debate".to_string();
            meta.style = style_record(&options);
            let debate = DebateOptions {
                question: options.question.clone(),
                coordinator: Some(coordinator.to_string()),
                peers: Some(peers),
                rounds: Some(decision.rounds.value),
                cwd: options.cwd.clone(),
                timeout: options.timeout,
                overrides: options.overrides.clone(),
                exclude: options.exclude.clone(),
                isolate: options.isolate,
                events: options.events.clone(),
                consultation: Some(meta),
                general_purpose: true,
                smart: smart.clone(),
                style: options.style.clone(),
            };
            let mut outcome = self.debate(debate, cancel).await?;
            // Routing notes belong with the result as well.
            for w in &decision.warnings {
                outcome.warnings.push(w.clone());
            }
            // A debate does not recall memory (it runs the fixed Milestone 2 workflow), but its
            // result is remembered like any other.
            if let Some(p) = &prepared {
                self.remember_outcome(p, &mut outcome, Some("debate")).await;
            }
            return Ok(outcome);
        }

        let providers = decision
            .participants
            .value
            .iter()
            .map(|id| self.registry().resolve(id.as_str()))
            .collect::<Result<Vec<_>>>()?;
        let names: Vec<String> = providers.iter().map(|p| p.id().to_string()).collect();

        let conversation = self.store().new_conversation();
        self.store().record(
            conversation,
            Author::User,
            EntryKind::Prompt,
            &options.question,
        );
        let id = match self.history() {
            Some(h) => h.fresh_id(),
            None => new_debate_id(),
        };
        let created_at_ms = now_ms();
        let started = Instant::now();
        let steps = Arc::new(Mutex::new(Vec::new()));
        let emit = Emitter(options.events.clone());
        let runner = Runner {
            emit: emit.clone(),
            store: self.store().clone(),
            ctx: self.context().clone(),
            conversation,
            overrides: Arc::new(overrides),
            steps: steps.clone(),
            started,
            smart: smart.clone(),
            style: options.style.clone(),
        };
        let others: Vec<ProviderId> = decision
            .participants
            .value
            .iter()
            .skip(1)
            .cloned()
            .collect();
        emit.emit(DebateEvent::Started {
            debate_id: id.clone(),
            mode: Some(mode.to_string()),
            question: options.question.clone(),
            coordinator: decision.coordinator.value.clone(),
            reviewers: others.clone(),
            skipped: decision.skipped.clone(),
            rounds: decision.rounds.value,
        });
        if let Some(p) = &prepared
            && p.mode != crate::memory::model::MemoryMode::Off
        {
            emit.emit(DebateEvent::Memory {
                message: format!(
                    "{} of {} recalled item(s) used (~{} tokens, estimated; budget {}){}",
                    p.report.selected,
                    p.report.retrieved,
                    p.report.estimated_tokens,
                    p.report.budget_tokens,
                    p.session
                        .as_ref()
                        .map_or(String::new(), |s| format!(" · session {}", s.id))
                ),
            });
        }

        let research_run = decision.research.is_active().then(|| {
            let mut run = ResearchRun::new(decision.research.clone());
            run.claim_check = options.claim_check;
            run
        });
        let fetcher = match (&research_run, &options.fetcher) {
            (None, _) => None,
            (Some(_), Some(f)) => Some(f.clone()),
            (Some(_), None) => Some(Arc::new(
                SourceFetcher::new(self.config().research.fetch_policy()).map_err(|e| {
                    OrchestraError::Config(format!("cannot start the page fetcher: {e}"))
                })?,
            )),
        };
        if let Some(run) = &research_run {
            emit.emit(DebateEvent::Research {
                message: format!(
                    "research is on ({} depth, up to {} source(s){})",
                    run.decision.depth.value.as_str(),
                    run.decision.max_sources.value,
                    run.decision
                        .discovery_agent
                        .as_ref()
                        .map_or(String::new(), |a| format!(", {a} searches the web"))
                ),
            });
        }
        let panel = Panel {
            question: &options.question,
            mode,
            brief: decision.brief.value,
            providers,
            names,
            rounds: decision.rounds.value,
            runner,
            options: &options,
            research: tokio::sync::Mutex::new(research_run),
            fetcher,
            // One clock: research may not outlast the consultation's own time limit.
            research_time_budget: {
                let configured = Duration::from_secs(self.config().research.total_timeout_secs);
                smart
                    .as_ref()
                    .and_then(|s| s.budget.remaining_time())
                    .map_or(configured, |left| configured.min(left))
            },
            flow: decision.smart.as_ref().map_or(
                if decision.participants.value.len() == 1 {
                    Flow::Single
                } else {
                    Flow::Panel
                },
                |p| p.flow,
            ),
            plan: decision.smart.clone(),
            smart: smart.clone(),
            memory_text: prepared
                .as_ref()
                .map(|p| p.text.clone())
                .unwrap_or_default(),
        };
        let total = Duration::from_secs(self.config().debate.total_timeout_secs);
        let guard = cancel.child_token();
        let result = {
            let workflow = self.run_panel(&panel, &guard);
            tokio::pin!(workflow);
            tokio::select! {
                result = &mut workflow => result,
                _ = tokio::time::sleep(total) => {
                    guard.cancel();
                    let _ = workflow.await; // let the agents be terminated cleanly
                    Err(OrchestraError::DebateTimeout { after: total })
                }
            }
        };

        let recorded = steps.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let duration_ms = elapsed_ms(started);
        let mut seed = seed_meta(&decision);
        seed.style = style_record(&options);
        let report: Option<ResearchReport> = panel
            .research
            .lock()
            .await
            .as_ref()
            .map(ResearchRun::report);
        let context_report = prepared
            .as_ref()
            .filter(|p| p.mode != crate::memory::model::MemoryMode::Off)
            .map(|p| p.report.clone());
        let strategy_name: Option<String> = decision
            .smart
            .as_ref()
            .and_then(|s| s.strategy_value())
            .map(|s| s.to_string());
        let build = |status: DebateStatus, tail: PanelResult, error: Option<String>| {
            let single_agent = tail.single_agent;
            let mut warnings = decision.warnings.clone();
            warnings.extend(tail.warnings.iter().cloned());
            if let Some(r) = &report {
                warnings.extend(r.limitations.iter().map(|l| format!("research: {l}")));
            }
            let mut outcome = DebateOutcome {
                schema: 1,
                id: id.clone(),
                conversation,
                created_at_ms,
                question: options.question.clone(),
                coordinator: decision.coordinator.value.clone(),
                participants: others.clone(),
                skipped: decision.skipped.clone(),
                rounds_planned: decision.rounds.value,
                status,
                synthesized: tail.synthesized,
                calls: recorded.len(),
                steps: recorded.clone(),
                initial_proposal: tail.initial,
                final_answer: tail.final_answer,
                error,
                warnings,
                duration_ms,
                consultation: None,
                summaries: Vec::new(),
                saved_to: None,
            };
            // Research that was not run (web off, or no way to search) is still disclosed: the
            // answer says it was not checked against external sources.
            if report.is_none() && !decision.research.limitations.is_empty() {
                warnings_extra(&mut outcome, &decision.research.limitations);
            }
            let mut meta = complete_meta(seed.clone(), &outcome);
            meta.research = report.clone();
            meta.context = context_report.clone();
            outcome.consultation = Some(meta);
            // The quality gate and the decision trace are part of the record.
            if let Some(smart) = &smart {
                smart.finish(&mut outcome, report.as_ref(), single_agent);
            }
            outcome
        };

        match result {
            Ok(tail) => {
                let status = if tail.synthesized && !tail.any_failure && tail.plan_met {
                    DebateStatus::Complete
                } else {
                    DebateStatus::Partial
                };
                self.store().record(
                    conversation,
                    Author::System,
                    EntryKind::Decision,
                    &tail.final_answer,
                );
                let mut outcome = build(status, tail, None);
                if let Some(p) = &prepared {
                    self.remember_outcome(p, &mut outcome, strategy_name.as_deref())
                        .await;
                    for w in &p.warnings {
                        outcome.warnings.push(w.clone());
                    }
                }
                // The quality gate may have made a "complete" result partial: report what was saved.
                emit.emit(DebateEvent::Finished {
                    status: outcome.status,
                    calls: recorded.len(),
                    duration_ms,
                });
                self.record_profiles(&outcome);
                self.persist(&mut outcome);
                Ok(outcome)
            }
            Err(e) => {
                emit.emit(DebateEvent::Failed {
                    error: e.to_string(),
                });
                // A failed or cancelled consultation is saved too, with everything gathered so far.
                let status = if matches!(e, OrchestraError::Cancelled { .. }) {
                    DebateStatus::Cancelled
                } else {
                    DebateStatus::Failed
                };
                let initial = recorded
                    .iter()
                    .find(|s| s.ok && matches!(s.phase, Role::Perspective))
                    .and_then(|s| s.text.clone())
                    .unwrap_or_default();
                let mut failed = build(
                    status,
                    PanelResult {
                        synthesized: false,
                        initial,
                        final_answer: String::new(),
                        warnings: Vec::new(),
                        any_failure: true,
                        plan_met: false,
                        single_agent: false,
                    },
                    Some(e.to_string()),
                );
                self.record_profiles(&failed);
                self.persist(&mut failed);
                Err(e)
            }
        }
    }

    /// Finds and retrieves the sources before anyone answers, when research is part of the task.
    /// Returns the evidence to put in the prompts and whether research produced nothing at all.
    async fn gather_research(
        &self,
        p: &Panel<'_>,
        cancel: &CancellationToken,
    ) -> Result<(Evidence, bool)> {
        let request = |prompt: String| p.request(prompt);
        let mut evidence = Evidence::default();
        let mut nothing = false;
        if let Some(fetcher) = &p.fetcher {
            let mut guard = p.research.lock().await;
            if let Some(run) = guard.as_mut() {
                let searcher = run
                    .decision
                    .discovery_agent
                    .clone()
                    .and_then(|id| p.providers.iter().find(|a| a.id() == &id).cloned());
                run.gather(
                    &p.runner,
                    searcher.as_ref(),
                    &request,
                    p.question,
                    fetcher,
                    p.research_time_budget,
                    cancel,
                )
                .await?;
                let retrieved = run.set.retrieved_ids().len();
                p.runner.emit.emit(DebateEvent::Research {
                    message: format!(
                        "{retrieved} source(s) retrieved of {} considered",
                        run.set.records().len()
                    ),
                });
                if let Some(smart) = &p.smart {
                    smart.trace.event(
                        TraceKind::Research,
                        format!(
                            "{retrieved} source(s) retrieved of {} considered; {} fetch attempt(s)",
                            run.set.records().len(),
                            run.budget.fetch_attempts
                        ),
                    );
                }
                nothing = retrieved == 0; // research was part of the task and produced nothing
                evidence = Evidence::from_run(run, p.question);
            }
        }
        Ok((evidence, nothing))
    }

    async fn run_panel(&self, p: &Panel<'_>, cancel: &CancellationToken) -> Result<PanelResult> {
        if p.flow == Flow::Adaptive {
            return self.run_adaptive(p, cancel).await;
        }
        let request = |prompt: String| p.request(prompt);
        let coordinator = p.providers[0].clone();
        let cancelled = || OrchestraError::Cancelled {
            provider: coordinator.id().clone(),
        };
        let mut warnings = Vec::new();
        let mut any_failure = false;

        // ---- Research: find and retrieve the sources before anyone answers.
        let research_on = p.research.lock().await.is_some();
        let (mut evidence, nothing_retrieved) = self.gather_research(p, cancel).await?;
        evidence.memory.clone_from(&p.memory_text);
        any_failure |= nothing_retrieved;

        // ---- A claim check: verify the one claim against the retrieved sources and stop.
        if p.options.claim_check {
            let claim = crate::research::claims::merge_claims(
                vec![crate::research::claims::RawClaim {
                    text: p.question.split_whitespace().collect::<Vec<_>>().join(" "),
                    category: crate::research::quality::ClaimCategory::Other,
                    sources: Vec::new(),
                    needs_external_evidence: true,
                    agent: "user".to_string(),
                }],
                1,
            );
            let mut guard = p.research.lock().await;
            let Some(run) = guard.as_mut() else {
                return Err(OrchestraError::Config(
                    "a claim check needs web research".into(),
                ));
            };
            if !evidence.block.is_empty() {
                run.verify_claims(
                    claim,
                    &p.runner,
                    &p.providers,
                    &request,
                    p.question,
                    now_ms(),
                    cancel,
                )
                .await?;
                any_failure |= run.had_failure;
            }
            let verdict = run
                .claims
                .first()
                .map_or("not checked".to_string(), |c| c.status.as_str().to_string());
            // The sources whose passages bear on the claim are the ones this result rests on.
            let mut used: Vec<u32> = run
                .claims
                .iter()
                .flat_map(|c| c.evidence.iter().map(|e| e.source_id))
                .collect();
            used.sort_unstable();
            used.dedup();
            let rests_on = if used.is_empty() {
                "No retrieved passage bears on the claim.".to_string()
            } else {
                format!(
                    "Passages used: {}.",
                    used.iter()
                        .map(|n| format!("[{n}]"))
                        .collect::<Vec<_>>()
                        .join(" ")
                )
            };
            let text = run.finalize_answer(&format!("Claim checked: {}\n\nResult: {verdict}. The status comes from passages retrieved from the sources below, judged by an AI model; it is not a consensus of models. {rests_on}", p.question.trim()));
            return Ok(PanelResult {
                synthesized: true,
                initial: p.question.to_string(),
                final_answer: text,
                warnings,
                any_failure,
                plan_met: true,
                single_agent: false,
            });
        }

        // ---- One participant: a single call that writes the final answer directly.
        if p.providers.len() == 1 {
            p.trace(
                TraceKind::Stage,
                format!("{} answers alone; no review is planned", coordinator.id()),
            );
            let (result, _) = p
                .runner
                .call(
                    coordinator.clone(),
                    request(with_evidence(
                        single_prompt(p.mode, p.brief, p.question),
                        &evidence.for_answer(),
                    )),
                    0,
                    Role::Synthesis,
                    cancel,
                )
                .await;
            let raw = result?.text;
            let mut text = strip_claims_block(&raw);
            if evidence.active() {
                let mut guard = p.research.lock().await;
                if let Some(run) = guard.as_mut() {
                    if !evidence.block.is_empty() {
                        let analyses = vec![(coordinator.id().to_string(), raw.clone())];
                        run.verify(
                            &p.runner,
                            &analyses,
                            &p.providers,
                            &request,
                            p.question,
                            now_ms(),
                            cancel,
                        )
                        .await?;
                    }
                    text = run.finalize_answer(&raw);
                    any_failure |= run.had_failure;
                }
            }
            return Ok(PanelResult {
                synthesized: true,
                initial: text.clone(),
                final_answer: text,
                warnings,
                any_failure,
                plan_met: true,
                single_agent: true,
            });
        }

        // ---- Stage B: independent perspectives, all in parallel; nobody sees the others.
        p.trace(
            TraceKind::Stage,
            format!(
                "{} participants answer independently and in parallel",
                p.providers.len()
            ),
        );
        let mut first_error: Option<OrchestraError> = None;
        let mut perspectives = vec![Slot::Skipped; p.providers.len()];
        let mut tasks = JoinSet::new();
        for (index, provider) in p.providers.iter().enumerate() {
            let (runner, provider, cancel) = (p.runner.clone(), provider.clone(), cancel.clone());
            let req = request(with_evidence(
                perspective_prompt(p.mode, p.brief, p.question),
                &evidence.for_answer(),
            ));
            tasks.spawn(async move {
                let (result, _) = runner
                    .call(provider, req, 0, Role::Perspective, &cancel)
                    .await;
                (index, result)
            });
        }
        while let Some(joined) = tasks.join_next().await {
            let Ok((index, result)) = joined else {
                continue;
            };
            perspectives[index] = match result {
                Ok(r) => Slot::Text(r.text),
                Err(e) => {
                    debug!(agent = %p.names[index], error = %e, "perspective failed");
                    any_failure = true;
                    warnings.push(format!(
                        "{} failed to give its own answer: {}",
                        p.names[index],
                        first_line(&e.to_string())
                    ));
                    let text = e.to_string();
                    first_error.get_or_insert(e);
                    Slot::Failed(text)
                }
            };
        }
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        for (i, slot) in perspectives.iter_mut().enumerate() {
            if matches!(slot, Slot::Skipped) {
                any_failure = true;
                *slot = Slot::Failed("internal error: task failed".to_string());
                warnings.push(format!("{} failed: internal error", p.names[i]));
            }
        }
        if count_text(&perspectives) == 0 {
            return Err(first_error.unwrap_or_else(|| {
                OrchestraError::Config("no participant produced an answer".into())
            }));
        }

        p.trace(
            TraceKind::Stage,
            format!(
                "{} of {} independent answers arrived",
                count_text(&perspectives),
                p.providers.len()
            ),
        );
        // ---- Verify the claims the agents relied on, against the retrieved passages, before
        // the cross-review, so the reviewers see what held up. The claims blocks are machine
        // readable and are not passed on to the other agents.
        if !evidence.block.is_empty() {
            let analyses: Vec<(String, String)> = perspectives
                .iter()
                .enumerate()
                .filter_map(|(i, s)| match s {
                    Slot::Text(t) => Some((p.names[i].clone(), t.clone())),
                    _ => None,
                })
                .collect();
            let alive: Vec<Arc<dyn AgentProvider>> = p
                .providers
                .iter()
                .enumerate()
                .filter(|(i, _)| matches!(perspectives[*i], Slot::Text(_)))
                .map(|(_, a)| a.clone())
                .collect();
            let mut guard = p.research.lock().await;
            if let Some(run) = guard.as_mut() {
                run.verify(
                    &p.runner,
                    &analyses,
                    &alive,
                    &request,
                    p.question,
                    now_ms(),
                    cancel,
                )
                .await?;
                any_failure |= run.had_failure;
                evidence.table = claims_table(&run.claims, CLAIM_TABLE_MAX_BYTES);
                let summary = format!(
                    "{} claim(s) checked: {}",
                    run.claims.len(),
                    run.claims
                        .iter()
                        .map(|c| c.status.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                p.trace(TraceKind::Research, summary.clone());
                p.runner
                    .emit
                    .emit(DebateEvent::Research { message: summary });
            }
        }
        if research_on {
            for slot in &mut perspectives {
                if let Slot::Text(t) = slot {
                    *t = strip_claims_block(t);
                }
            }
        }
        let initial = match &perspectives[0] {
            Slot::Text(t) => t.clone(),
            _ => String::new(),
        };

        // ---- Stage C: bounded cross-review. A participant that failed is not asked again.
        let mut reviews: Vec<Vec<Slot>> = Vec::new();
        let mut active: Vec<usize> = (0..p.providers.len())
            .filter(|i| matches!(perspectives[*i], Slot::Text(_)))
            .collect();
        let mut rounds_run = 0u8;
        for round in 1..=p.rounds {
            if active.len() < 2 {
                warnings.push(format!(
                    "round {round}: fewer than two participants have an answer, so there is \
                     nothing to cross-review; skipping the remaining round(s)"
                ));
                break;
            }
            let role = if round == 1 {
                Role::Review
            } else {
                Role::CrossReview
            };
            p.trace(
                TraceKind::Stage,
                format!(
                    "review round {round} of {}: {} participant(s) review the others' answers",
                    p.rounds,
                    active.len()
                ),
            );
            let mut tasks = JoinSet::new();
            for &index in &active {
                let prompt = if round == 1 {
                    review_prompt(p.mode, p.brief, p.question, index, &p.names, &perspectives)
                } else {
                    cross_review_prompt(
                        p.mode,
                        p.brief,
                        p.question,
                        index,
                        &p.names,
                        &perspectives,
                        &reviews[0],
                    )
                };
                let prompt = with_evidence(prompt, &evidence.for_review());
                let (runner, provider, cancel) =
                    (p.runner.clone(), p.providers[index].clone(), cancel.clone());
                let req = request(prompt);
                tasks.spawn(async move {
                    let (result, _) = runner.call(provider, req, round, role, &cancel).await;
                    (index, result)
                });
            }
            let mut slots = vec![Slot::Skipped; p.providers.len()];
            while let Some(joined) = tasks.join_next().await {
                let Ok((index, result)) = joined else {
                    continue;
                };
                slots[index] = match result {
                    Ok(r) => Slot::Text(r.text),
                    Err(e) => {
                        debug!(agent = %p.names[index], error = %e, "reviewer failed");
                        any_failure = true;
                        warnings.push(format!(
                            "round {round}: {} failed: {}",
                            p.names[index],
                            first_line(&e.to_string())
                        ));
                        Slot::Failed(e.to_string())
                    }
                };
            }
            if cancel.is_cancelled() {
                return Err(cancelled());
            }
            for &index in &active {
                if matches!(slots[index], Slot::Skipped) {
                    any_failure = true;
                    slots[index] = Slot::Failed("internal error: review task failed".to_string());
                    warnings.push(format!(
                        "round {round}: {} failed: internal error",
                        p.names[index]
                    ));
                }
            }
            rounds_run = round;
            active.retain(|&i| matches!(slots[i], Slot::Text(_)));
            reviews.push(slots);
        }

        // ---- Stage D: the coordinator's synthesis. If the coordinator could not even give its
        // own answer, it is not asked to write the synthesis (no retries, no failover).
        let coordinator_ok = matches!(perspectives[0], Slot::Text(_));
        p.trace(
            TraceKind::Stage,
            if coordinator_ok {
                format!(
                    "{} writes the final answer from everything gathered",
                    coordinator.id()
                )
            } else {
                format!(
                    "{} gave no answer of its own, so it writes no synthesis",
                    coordinator.id()
                )
            },
        );
        let mut synthesized = false;
        let best = perspectives
            .iter()
            .find_map(|s| match s {
                Slot::Text(t) => Some(t.clone()),
                _ => None,
            })
            .unwrap_or_default();
        let mut final_answer = best;
        if coordinator_ok {
            let prompt = with_evidence(
                synthesis_prompt(
                    p.mode,
                    p.brief,
                    p.question,
                    &p.names,
                    &perspectives,
                    &reviews,
                ),
                &evidence.for_synthesis(),
            );
            let (response, _) = p
                .runner
                .call(
                    coordinator.clone(),
                    request(prompt),
                    rounds_run,
                    Role::Synthesis,
                    cancel,
                )
                .await;
            match response {
                Ok(r) => {
                    final_answer = r.text;
                    synthesized = true;
                }
                Err(OrchestraError::Cancelled { .. }) => return Err(cancelled()),
                Err(e) => {
                    any_failure = true;
                    warnings.push(format!(
                        "the coordinator {} failed to write the synthesis: {}; the answer shown \
                         is the first available individual answer, and the others are in the \
                         transcript",
                        coordinator.id(),
                        first_line(&e.to_string())
                    ));
                }
            }
        } else {
            warnings.push(format!(
                "the coordinator {} did not give its own answer, so it was not asked to write \
                 the synthesis; the answer shown is the first available individual answer, NOT a \
                 synthesis (choose another --coordinator to retry)",
                coordinator.id()
            ));
        }
        // Citations are validated, and the sources and claim table are generated from the
        // recorded sources, never taken from a model.
        if research_on {
            let mut guard = p.research.lock().await;
            if let Some(run) = guard.as_mut() {
                final_answer = run.finalize_answer(&final_answer);
                any_failure |= run.had_failure;
            }
        }
        Ok(PanelResult {
            synthesized,
            initial,
            final_answer,
            warnings,
            any_failure,
            plan_met: rounds_run == p.rounds,
            single_agent: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::history::DebateStore;
    use crate::provider::{InvokeContext, ORCHESTRA_ACTIVE_ENV, ProviderRegistry};
    use crate::providers::mock::{MockProvider, MockReply};
    use tokio::sync::Barrier;

    fn id(s: &str) -> ProviderId {
        ProviderId::new(s)
    }

    fn ok_agent(name: &str) -> AgentInfo {
        AgentInfo {
            id: id(name),
            usable: Ok(()),
            can_search: false,
        }
    }

    fn down_agent(name: &str, why: &str) -> AgentInfo {
        AgentInfo {
            id: id(name),
            usable: Err(why.to_string()),
            can_search: false,
        }
    }

    fn input<'a>(question: &'a str, agents: Vec<AgentInfo>) -> RouteInput<'a> {
        RouteInput {
            question,
            mode: None,
            coordinator: None,
            agents: None,
            rounds: None,
            length: None,
            research: ResearchRequest::default(),
            fetch_policy: crate::research::net::FetchPolicy::default(),
            available: agents,
            configured_coordinator: Some(id("lead")),
        }
    }

    fn three() -> Vec<AgentInfo> {
        vec![ok_agent("lead"), ok_agent("beta"), ok_agent("gamma")]
    }

    fn route(i: &RouteInput<'_>) -> Result<RoutingDecision> {
        ConsultationRouter.route(i)
    }

    // ---- router: mode ------------------------------------------------------

    #[test]
    fn the_mode_is_chosen_from_the_question_with_fixed_rules() {
        let cases = [
            (
                "Which of these products offers the best value?",
                ConsultMode::Review,
            ),
            (
                "Should I buy a cheap oscilloscope or invest in a professional one?",
                ConsultMode::Decision,
            ),
            ("Compare Rust and C++", ConsultMode::Decision),
            (
                "Compare the specs of two oscilloscopes",
                ConsultMode::Review,
            ),
            (
                "Is this scientific claim actually true?",
                ConsultMode::Research,
            ),
            (
                "Research the latest developments in quantum computing.",
                ConsultMode::Research,
            ),
            (
                "Explain electronics to me from the beginning.",
                ConsultMode::Learn,
            ),
            ("Explain quantum entanglement", ConsultMode::Learn),
            (
                "Help me make a difficult decision based on evidence and tradeoffs.",
                ConsultMode::Decision,
            ),
            (
                "Have the agents debate whether tabs beat spaces",
                ConsultMode::Debate,
            ),
            ("Explícame la electrónica desde cero", ConsultMode::Learn),
        ];
        for (question, expected) in cases {
            let d = route(&input(question, three())).unwrap();
            assert_eq!(d.mode.value, expected, "{question}: {}", d.mode.reason);
            assert_eq!(d.mode.source, Source::Auto);
            assert!(!d.mode.reason.is_empty());
        }
    }

    #[test]
    fn a_question_without_cues_defaults_to_research_and_says_so() {
        let d = route(&input("Rust ownership and tokio", three())).unwrap();
        assert_eq!(d.mode.value, ConsultMode::Research);
        assert!(d.mode.reason.contains("defaulting"), "{}", d.mode.reason);
    }

    #[test]
    fn routing_is_deterministic() {
        let q = "Should I buy a cheap oscilloscope or invest in a professional one?";
        let first = route(&input(q, three())).unwrap();
        for _ in 0..20 {
            assert_eq!(route(&input(q, three())).unwrap(), first);
        }
    }

    #[test]
    fn a_manual_mode_always_wins_and_is_marked_manual() {
        for mode in ConsultMode::ALL {
            let mut i = input("Explain quantum entanglement", three());
            i.mode = Some(mode);
            let d = route(&i).unwrap();
            assert_eq!(d.mode.value, mode);
            assert_eq!(d.mode.source, Source::Manual);
            assert!(d.manual_settings().contains(&"mode".to_string()));
        }
    }

    #[test]
    fn mode_arguments_accept_auto_and_reject_unknown_values() {
        assert_eq!(parse_mode_arg(None).unwrap(), None);
        assert_eq!(parse_mode_arg(Some("AUTO")).unwrap(), None);
        assert_eq!(
            parse_mode_arg(Some("Learn")).unwrap(),
            Some(ConsultMode::Learn)
        );
        assert!(parse_mode_arg(Some("poetry")).is_err());
        assert_eq!(parse_rounds_arg(Some("auto")).unwrap(), None);
        assert_eq!(parse_rounds_arg(Some("2")).unwrap(), Some(2));
        for bad in ["0", "3", "x", "-1"] {
            assert!(parse_rounds_arg(Some(bad)).is_err(), "{bad}");
        }
        assert_eq!(parse_agents_arg(&["auto".into()]).unwrap(), None);
        assert_eq!(parse_agents_arg(&[]).unwrap(), None);
        assert_eq!(
            parse_agents_arg(&["a,b".into(), "c".into()]).unwrap(),
            Some(vec!["a".into(), "b".into(), "c".into()])
        );
        assert!(parse_agents_arg(&["auto".into(), "a".into()]).is_err());
        assert_eq!(parse_coordinator_arg(Some("auto")), None);
        assert_eq!(parse_coordinator_arg(Some("codex")), Some("codex".into()));
    }

    // ---- router: coordinator and agents ------------------------------------

    #[test]
    fn the_coordinator_is_the_configured_one_when_usable_else_the_first_usable() {
        let d = route(&input("Compare A and B", three())).unwrap();
        assert_eq!(d.coordinator.value, id("lead"));
        assert_eq!(d.coordinator.source, Source::Auto);
        assert_eq!(
            d.participants.value[0],
            id("lead"),
            "coordinator comes first"
        );

        let agents = vec![
            down_agent("lead", "not installed"),
            ok_agent("beta"),
            ok_agent("gamma"),
        ];
        let d = route(&input("Compare A and B", agents)).unwrap();
        assert_eq!(d.coordinator.value, id("beta"));
        assert!(d.coordinator.reason.contains("first usable"));
        let skipped: Vec<_> = d.skipped.iter().map(|s| s.provider.to_string()).collect();
        assert_eq!(skipped, ["lead"], "the unusable agent is reported");
    }

    #[test]
    fn a_manual_coordinator_wins_and_must_be_usable_and_participating() {
        let mut i = input("Compare A and B", three());
        i.coordinator = Some(id("gamma"));
        let d = route(&i).unwrap();
        assert_eq!(d.coordinator.value, id("gamma"));
        assert_eq!(d.coordinator.source, Source::Manual);
        assert_eq!(d.participants.value[0], id("gamma"));

        i.coordinator = Some(id("beta"));
        i.available = vec![
            ok_agent("lead"),
            down_agent("beta", "not installed"),
            ok_agent("gamma"),
        ];
        let err = route(&i).unwrap_err().to_string();
        assert!(
            err.contains("beta") && err.contains("not installed"),
            "{err}"
        );

        // Not among the requested agents.
        let mut i = input("Compare A and B", three());
        i.coordinator = Some(id("gamma"));
        i.agents = Some(vec![id("lead"), id("beta")]);
        let err = route(&i).unwrap_err().to_string();
        assert!(
            err.contains("must be one of the participating agents"),
            "{err}"
        );
    }

    #[test]
    fn a_configured_coordinator_outside_the_requested_agents_is_explained_accurately() {
        let mut i = input("Compare A and B", three());
        i.agents = Some(vec![id("beta"), id("gamma")]);
        let d = route(&i).unwrap();
        assert_eq!(d.coordinator.value, id("beta"));
        assert!(
            d.coordinator
                .reason
                .contains("not among the requested agents"),
            "{}",
            d.coordinator.reason
        );
        assert!(
            !d.coordinator.reason.contains("not available"),
            "it IS available, just not requested"
        );
    }

    #[test]
    fn automatic_agents_are_every_usable_one_and_unusable_ones_are_reported() {
        let agents = vec![
            ok_agent("lead"),
            down_agent("beta", "disabled in config"),
            ok_agent("gamma"),
        ];
        let d = route(&input("Compare A and B", agents)).unwrap();
        assert_eq!(d.participants.source, Source::Auto);
        assert_eq!(d.participants.value, [id("lead"), id("gamma")]);
        assert_eq!(d.skipped.len(), 1);
        assert!(d.skipped[0].reason.contains("disabled"));
    }

    #[test]
    fn manual_agents_are_honoured_and_unavailable_ones_reported() {
        let mut i = input(
            "Compare A and B",
            vec![
                ok_agent("lead"),
                down_agent("beta", "not installed"),
                ok_agent("gamma"),
            ],
        );
        i.agents = Some(vec![id("gamma"), id("beta"), id("gamma"), id("lead")]);
        let d = route(&i).unwrap();
        assert_eq!(d.participants.source, Source::Manual);
        assert_eq!(
            d.participants.value,
            [id("lead"), id("gamma")],
            "deduplicated, coordinator first"
        );
        assert_eq!(d.skipped.len(), 1);
        assert_eq!(d.skipped[0].provider, id("beta"));
        assert!(d.skipped[0].reason.contains("requested but unavailable"));

        i.agents = Some(vec![id("beta")]);
        let err = route(&i).unwrap_err().to_string();
        assert!(
            err.contains("none of the requested agents can be used"),
            "{err}"
        );
    }

    #[test]
    fn no_usable_agent_at_all_is_a_clear_error() {
        let agents = vec![
            down_agent("lead", "not installed"),
            down_agent("beta", "disabled in config"),
        ];
        let err = route(&input("Compare A and B", agents))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("no agent is available") && err.contains("not installed"),
            "{err}"
        );
    }

    // ---- router: rounds and the lightweight path -----------------------------

    #[test]
    fn rounds_are_one_by_default_and_two_for_contested_or_long_questions() {
        let d = route(&input("Compare A and B", three())).unwrap();
        assert_eq!((d.rounds.value, d.rounds.source), (1, Source::Auto));
        for q in [
            "Is it safe to invest my savings in a single stock?",
            "Is this scientific claim actually true?",
            "Have the agents debate monorepos",
        ] {
            let d = route(&input(q, three())).unwrap();
            assert_eq!(d.rounds.value, 2, "{q}: {}", d.rounds.reason);
        }
        let long = format!("Compare these options: {}", "detail ".repeat(120));
        assert_eq!(route(&input(&long, three())).unwrap().rounds.value, 2);
    }

    #[test]
    fn manual_rounds_are_respected_even_when_the_question_is_contested() {
        let mut i = input(
            "Is it safe to invest my savings in a single stock?",
            three(),
        );
        i.rounds = Some(1);
        let d = route(&i).unwrap();
        assert_eq!((d.rounds.value, d.rounds.source), (1, Source::Manual));
        i.rounds = Some(2);
        assert_eq!(route(&i).unwrap().rounds.value, 2);
        i.rounds = Some(3);
        assert!(route(&i).is_err());
        i.rounds = Some(0);
        assert!(route(&i).is_err());
    }

    #[test]
    fn a_short_uncontested_lesson_uses_one_agent_unless_anything_is_set_by_hand() {
        let q = "Explain what a resistor does";
        let d = route(&input(q, three())).unwrap();
        assert_eq!(d.mode.value, ConsultMode::Learn);
        assert_eq!(d.participants.value, [id("lead")]);
        assert_eq!(d.rounds.value, 0);
        assert_eq!(d.planned_calls(), 1);
        assert!(
            d.warnings
                .iter()
                .any(|w| w.contains("no independent review"))
        );

        // Manual agents, rounds, or mode: the lightweight path is off.
        let mut i = input(q, three());
        i.agents = Some(vec![id("lead"), id("beta")]);
        assert_eq!(route(&i).unwrap().participants.value.len(), 2);
        let mut i = input(q, three());
        i.rounds = Some(1);
        let d = route(&i).unwrap();
        assert_eq!((d.participants.value.len(), d.rounds.value), (3, 1));
        // A contested lesson also gets the full panel.
        let d = route(&input(
            "Explain the risks of this medical procedure",
            three(),
        ))
        .unwrap();
        assert_eq!(d.participants.value.len(), 3);
        // Other modes never take the lightweight path.
        let d = route(&input("Compare A and B", three())).unwrap();
        assert_eq!(d.participants.value.len(), 3);
    }

    #[test]
    fn a_single_participant_has_no_review_rounds_and_a_manual_rounds_value_is_reported_as_ignored()
    {
        let mut i = input("Compare A and B", three());
        i.agents = Some(vec![id("beta")]);
        i.rounds = Some(2);
        let d = route(&i).unwrap();
        assert_eq!(d.rounds.value, 0);
        assert!(
            d.warnings
                .iter()
                .any(|w| w.contains("--rounds 2 was ignored")),
            "{:?}",
            d.warnings
        );
        assert_eq!(d.coordinator.value, id("beta"));

        // Only one agent installed: same.
        let d = route(&input(
            "Compare A and B",
            vec![ok_agent("lead"), down_agent("beta", "missing")],
        ))
        .unwrap();
        assert_eq!((d.participants.value.len(), d.rounds.value), (1, 0));
    }

    #[test]
    fn a_debate_needs_two_agents() {
        let mut i = input("Compare A and B", vec![ok_agent("lead")]);
        i.mode = Some(ConsultMode::Debate);
        let err = route(&i).unwrap_err().to_string();
        assert!(err.contains("at least two"), "{err}");
    }

    #[test]
    fn planned_calls_follow_the_documented_bounds() {
        let mut i = input("Compare A and B", three());
        i.rounds = Some(1);
        assert_eq!(route(&i).unwrap().planned_calls(), 3 + 3 + 1);
        i.rounds = Some(2);
        assert_eq!(route(&i).unwrap().planned_calls(), 3 + 6 + 1);
        i.mode = Some(ConsultMode::Debate);
        assert_eq!(route(&i).unwrap().planned_calls(), 2 + 2 * 2);
    }

    // ---- sections ------------------------------------------------------------

    #[test]
    fn sections_are_read_back_by_heading() {
        let md = "## Answer\nA\n\n## Recommendation\nBuy it.\nTwice.\n\n### Detail\nsub\n\n## Areas of Agreement\nAll agree\n## Significant disagreements\nNone\n";
        assert_eq!(
            section(md, &["recommendation"]).unwrap(),
            "Buy it.\nTwice.\n\n### Detail\nsub"
        );
        assert_eq!(section(md, &["areas of agreement"]).unwrap(), "All agree");
        assert_eq!(section(md, &["significant disagreements"]).unwrap(), "None");
        assert_eq!(section(md, &["assumptions"]), None);
        assert_eq!(section("no headings here", &["answer"]), None);
        assert_eq!(
            section("## Recommendation\n\n## Next", &["recommendation"]),
            None,
            "empty body"
        );
    }

    // ---- engine --------------------------------------------------------------

    const SYNTH: &str = "## Answer\nGet the entry model.\n\n## Recommendation\nBuy the entry model.\n\n## Areas of agreement\nPrice matters.\n\n## Significant disagreements\n- beta wants a professional model; unresolved.\n\n## Assumptions\nBudget under 300.\n\n## Evidence limitations\nPrices were not verified.\n";

    fn t(s: &str) -> MockReply {
        MockReply::Text(s.to_string())
    }

    fn lead_with(replies: &[&str]) -> Arc<MockProvider> {
        let mut m = MockProvider::new("lead");
        for r in replies {
            m = m.with_reply(t(r));
        }
        Arc::new(m)
    }

    fn plain(name: &str) -> Arc<MockProvider> {
        Arc::new(MockProvider::new(name))
    }

    fn orch(agents: Vec<Arc<MockProvider>>, edit: impl FnOnce(&mut Config)) -> Orchestrator {
        let mut config = Config::default();
        config.general.coordinator = "lead".into();
        edit(&mut config);
        let providers: Vec<Arc<dyn AgentProvider>> = agents
            .into_iter()
            .map(|a| a as Arc<dyn AgentProvider>)
            .collect();
        Orchestrator::with_registry(config, ProviderRegistry::new(providers)).unwrap()
    }

    fn with_store(o: Orchestrator) -> (Orchestrator, DebateStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = DebateStore::new(dir.path().join("debates"));
        (o.with_history(store.clone()), store, dir)
    }

    fn q(question: &str, mode: ConsultMode, rounds: u8) -> ConsultOptions {
        ConsultOptions {
            question: question.to_string(),
            mode: Some(mode),
            rounds: Some(rounds),
            ..Default::default()
        }
    }

    async fn go(o: &Orchestrator, options: ConsultOptions) -> Result<DebateOutcome> {
        o.consult(options, &CancellationToken::new()).await
    }

    fn phases(out: &DebateOutcome) -> Vec<(Role, u8)> {
        out.steps.iter().map(|s| (s.phase, s.round)).collect()
    }

    #[tokio::test]
    async fn a_panel_answers_independently_then_cross_reviews_then_synthesizes() {
        let lead = lead_with(&["LEAD-ANSWER", "LEAD-REVIEW", SYNTH]);
        let beta = Arc::new(
            MockProvider::new("beta")
                .with_reply(t("BETA-ANSWER"))
                .with_reply(t("BETA-REVIEW")),
        );
        let gamma = Arc::new(
            MockProvider::new("gamma")
                .with_reply(t("GAMMA-ANSWER"))
                .with_reply(t("GAMMA-REVIEW")),
        );
        let o = orch(vec![lead.clone(), beta.clone(), gamma.clone()], |_| {});
        let out = go(
            &o,
            q("Which oscilloscope should I buy?", ConsultMode::Decision, 1),
        )
        .await
        .unwrap();

        assert_eq!(out.status, DebateStatus::Complete);
        assert!(out.synthesized);
        assert_eq!(out.calls, 3 + 3 + 1);
        assert_eq!(out.final_answer, SYNTH);
        let ph = phases(&out);
        assert_eq!(
            ph.iter().filter(|p| **p == (Role::Perspective, 0)).count(),
            3
        );
        assert_eq!(ph.iter().filter(|p| **p == (Role::Review, 1)).count(), 3);
        assert_eq!(ph.last(), Some(&(Role::Synthesis, 1)));
        assert_eq!(out.coordinator, id("lead"));
        assert_eq!(out.rounds_planned, 1);

        // Independence: the first prompt each agent sees contains only the question.
        for m in [&lead, &beta, &gamma] {
            let first = &m.requests()[0].prompt;
            assert!(first.contains("Which oscilloscope should I buy?"));
            assert!(first.contains("cannot see their work") && first.contains("Understanding"));
            for other in ["LEAD-ANSWER", "BETA-ANSWER", "GAMMA-ANSWER"] {
                assert!(!first.contains(other), "perspective prompt leaked {other}");
            }
        }
        // Cross-review: each reviewer sees the others' answers, labelled, and its own separately.
        let beta_review = &beta.requests()[1].prompt;
        assert!(beta_review.contains("LEAD-ANSWER") && beta_review.contains("GAMMA-ANSWER"));
        assert!(beta_review.contains("<your_answer>\nBETA-ANSWER"));
        assert!(
            !beta_review.contains("GAMMA-REVIEW"),
            "round-1 reviews are independent"
        );
        // Synthesis: everything, the roster and the evidence rules.
        let synth = &lead.requests()[2].prompt;
        for needle in [
            "LEAD-ANSWER",
            "BETA-ANSWER",
            "GAMMA-ANSWER",
            "BETA-REVIEW",
            "GAMMA-REVIEW",
            "lead, beta, gamma",
            "agreement between assistants is not proof",
            "## Alternatives and criteria",
            "## Significant disagreements",
        ] {
            assert!(synth.contains(needle), "synthesis prompt lacks {needle}");
        }
        for r in lead
            .requests()
            .iter()
            .chain(&beta.requests())
            .chain(&gamma.requests())
        {
            assert_eq!(
                r.access,
                crate::types::AccessMode::ReadOnly,
                "all calls are read-only"
            );
        }
    }

    #[tokio::test]
    async fn independent_perspectives_run_in_parallel() {
        // beta and gamma each wait for the other inside the call, in both stages. Sequential
        // execution would never release the barrier.
        let gate = Arc::new(Barrier::new(2));
        let beta = Arc::new(MockProvider::new("beta").with_gate(gate.clone()));
        let gamma = Arc::new(MockProvider::new("gamma").with_gate(gate));
        let o = orch(vec![plain("lead"), beta, gamma], |_| {});
        let out = tokio::time::timeout(
            Duration::from_secs(30),
            go(&o, q("Compare A and B", ConsultMode::Decision, 1)),
        )
        .await
        .expect("parallel stages must not deadlock")
        .unwrap();
        assert_eq!(out.status, DebateStatus::Complete);
    }

    #[tokio::test]
    async fn two_rounds_add_a_cross_review_that_sees_the_round_one_reviews() {
        let lead = lead_with(&["L1", "L-R1", "L-R2", SYNTH]);
        let beta = Arc::new(
            MockProvider::new("beta")
                .with_reply(t("B1"))
                .with_reply(t("B-R1"))
                .with_reply(t("B-R2")),
        );
        let gamma = Arc::new(
            MockProvider::new("gamma")
                .with_reply(t("G1"))
                .with_reply(t("G-R1"))
                .with_reply(t("G-R2")),
        );
        let o = orch(vec![lead.clone(), beta.clone(), gamma.clone()], |_| {});
        let out = go(&o, q("Compare A and B", ConsultMode::Decision, 2))
            .await
            .unwrap();
        assert_eq!(out.status, DebateStatus::Complete);
        assert_eq!(out.calls, 3 + 3 + 3 + 1);
        assert_eq!(out.rounds_planned, 2);
        let ph = phases(&out);
        assert_eq!(
            ph.iter().filter(|p| **p == (Role::CrossReview, 2)).count(),
            3
        );
        assert_eq!(ph.last(), Some(&(Role::Synthesis, 2)));
        let round2 = &beta.requests()[2].prompt;
        assert!(
            round2.contains("G-R1") && round2.contains("L-R1"),
            "{round2}"
        );
        assert!(round2.contains("<your_round_1_review>\nB-R1"));
        assert_eq!(out.consultation.as_ref().unwrap().rounds_run, 2);
    }

    #[tokio::test]
    async fn one_agent_consultation_makes_one_call_and_says_there_was_no_review() {
        let lead = lead_with(&[SYNTH]);
        let o = orch(vec![lead.clone(), plain("beta")], |_| {});
        let out = go(
            &o,
            ConsultOptions {
                question: "Compare A and B".into(),
                agents: Some(vec!["lead".into()]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(out.calls, 1);
        assert_eq!(out.status, DebateStatus::Complete);
        assert!(out.synthesized);
        assert_eq!(out.rounds_planned, 0);
        assert!(out.participants.is_empty());
        assert!(
            out.warnings
                .iter()
                .any(|w| w.contains("no independent review"))
        );
        let prompt = &lead.requests()[0].prompt;
        assert!(
            prompt.contains("only AI assistant")
                && prompt.contains("no other assistant was consulted")
        );
        assert_eq!(out.consultation.as_ref().unwrap().strategy, "single");
    }

    #[tokio::test]
    async fn a_short_lesson_is_answered_by_one_agent_without_extra_calls() {
        let lead = lead_with(&["## Lesson\nResistors resist current."]);
        let beta = plain("beta");
        let o = orch(vec![lead, beta.clone()], |_| {});
        let out = go(
            &o,
            ConsultOptions {
                question: "Explain what a resistor does".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(out.calls, 1);
        assert!(
            beta.requests().is_empty(),
            "the second agent is not consulted"
        );
        let routing = out.consultation.unwrap().routing.unwrap();
        assert_eq!(routing.mode.value, ConsultMode::Learn);
        assert_eq!(routing.participants.source, Source::Auto);
    }

    #[tokio::test]
    async fn each_mode_uses_its_own_instructions_and_sections() {
        let cases = [
            (
                ConsultMode::Decision,
                "decision-support question",
                "## Alternatives and criteria",
            ),
            (
                ConsultMode::Research,
                "research question",
                "## Established facts",
            ),
            (
                ConsultMode::Review,
                "evaluation of a product",
                "## Strengths and weaknesses",
            ),
            (ConsultMode::Learn, "wants to learn this", "## Lesson"),
        ];
        for (mode, brief, section_heading) in cases {
            let lead = lead_with(&["a", "b", SYNTH]);
            let o = orch(vec![lead.clone(), plain("beta")], |_| {});
            let out = go(&o, q("A question", mode, 1)).await.unwrap();
            assert_eq!(out.consultation.as_ref().unwrap().mode, Some(mode));
            let reqs = lead.requests();
            assert!(reqs[0].prompt.contains(brief), "{mode}: {}", reqs[0].prompt);
            assert!(reqs[2].prompt.contains(section_heading), "{mode}");
            // Every mode forbids invented sources and numeric confidence.
            assert!(reqs[0].prompt.contains("Do not invent URLs, citations"));
        }
        // Decision mode specifically forbids invented probabilities.
        let lead = lead_with(&[]);
        let o = orch(vec![lead.clone(), plain("beta")], |_| {});
        go(&o, q("A question", ConsultMode::Decision, 1))
            .await
            .unwrap();
        assert!(
            lead.requests()[0]
                .prompt
                .contains("numerical probabilities or confidence percentages")
        );
        // Research mode says nothing was verified externally.
        let lead = lead_with(&[]);
        let o = orch(vec![lead.clone(), plain("beta")], |_| {});
        go(&o, q("A question", ConsultMode::Research, 1))
            .await
            .unwrap();
        assert!(
            lead.requests()[0]
                .prompt
                .contains("has been verified against external sources")
        );
    }

    #[tokio::test]
    async fn debate_mode_runs_the_milestone_two_engine_with_a_consultation_record() {
        let lead = lead_with(&["PROPOSAL", SYNTH]);
        let beta = Arc::new(MockProvider::new("beta").with_reply(t("BETA-REVIEW")));
        let gamma = Arc::new(MockProvider::new("gamma").with_reply(t("GAMMA-REVIEW")));
        let (o, store, _dir) = with_store(orch(vec![lead.clone(), beta, gamma], |_| {}));
        let out = go(&o, q("Tabs or spaces?", ConsultMode::Debate, 1))
            .await
            .unwrap();
        assert_eq!(out.calls, 1 + 2 + 1);
        assert_eq!(phases(&out).first(), Some(&(Role::Proposal, 0)));
        assert_eq!(out.status, DebateStatus::Complete);
        let meta = out.consultation.as_ref().unwrap();
        assert_eq!(meta.mode, Some(ConsultMode::Debate));
        assert_eq!(meta.strategy, "debate");
        // General wording for non-engineering subjects.
        assert!(!lead.requests()[0].prompt.contains("engineering"));
        // Saved with the consultation attached.
        let saved = store.load(&out.id).unwrap();
        assert_eq!(
            saved.consultation.as_ref().unwrap().mode,
            Some(ConsultMode::Debate)
        );
    }

    #[tokio::test]
    async fn unresolved_disagreement_is_kept_and_flagged() {
        let lead = lead_with(&["a", "b", SYNTH]);
        let o = orch(vec![lead, plain("beta")], |_| {});
        let out = go(&o, q("Compare A and B", ConsultMode::Decision, 1))
            .await
            .unwrap();
        let meta = out.consultation.unwrap();
        assert!(meta.unresolved_disagreement);
        assert!(
            meta.disagreements
                .unwrap()
                .contains("beta wants a professional model")
        );
        assert_eq!(meta.recommendation.as_deref(), Some("Buy the entry model."));
        assert_eq!(meta.agreements.as_deref(), Some("Price matters."));
        assert_eq!(meta.assumptions.as_deref(), Some("Budget under 300."));
        assert_eq!(
            meta.evidence_limitations.as_deref(),
            Some("Prices were not verified.")
        );

        let resolved =
            "## Answer\nx\n\n## Significant disagreements\nNone: both reviewers agreed.\n";
        let lead = lead_with(&["a", "b", resolved]);
        let o = orch(vec![lead, plain("beta")], |_| {});
        let out = go(&o, q("Compare A and B", ConsultMode::Decision, 1))
            .await
            .unwrap();
        assert!(!out.consultation.unwrap().unresolved_disagreement);
    }

    #[tokio::test]
    async fn a_failed_coordinator_is_not_asked_again_and_the_result_is_partial() {
        let lead = Arc::new(
            MockProvider::new("lead").with_reply(MockReply::Quota("limit reached".into())),
        );
        let beta = Arc::new(MockProvider::new("beta").with_reply(t("BETA-ANSWER")));
        let gamma = Arc::new(MockProvider::new("gamma").with_reply(t("GAMMA-ANSWER")));
        let o = orch(vec![lead.clone(), beta, gamma], |_| {});
        let out = go(&o, q("Compare A and B", ConsultMode::Decision, 1))
            .await
            .unwrap();
        assert_eq!(out.status, DebateStatus::Partial);
        assert!(!out.synthesized);
        assert_eq!(
            out.final_answer, "BETA-ANSWER",
            "the first available answer, not a synthesis"
        );
        assert_eq!(lead.requests().len(), 1, "no retry, no synthesis attempt");
        assert_eq!(out.calls, 3 + 2, "only the two that answered cross-review");
        assert!(out.warnings.iter().any(|w| w.contains("NOT a synthesis")));
        assert_eq!(out.consultation.as_ref().unwrap().provider_failures, 1);
        assert!(out.render_summary().contains("NOT a synthesis"));
    }

    #[tokio::test]
    async fn a_failed_reviewer_makes_the_result_partial_but_the_synthesis_still_happens() {
        let lead = lead_with(&["L1", "L-R1", SYNTH]);
        let beta = Arc::new(MockProvider::new("beta").with_reply(MockReply::Timeout));
        let gamma = Arc::new(
            MockProvider::new("gamma")
                .with_reply(t("G1"))
                .with_reply(t("G-R1")),
        );
        let o = orch(vec![lead.clone(), beta.clone(), gamma], |_| {});
        let out = go(&o, q("Compare A and B", ConsultMode::Decision, 1))
            .await
            .unwrap();
        assert_eq!(out.status, DebateStatus::Partial);
        assert!(out.synthesized);
        assert_eq!(out.calls, 3 + 2 + 1);
        assert_eq!(
            beta.requests().len(),
            1,
            "a failed participant is not asked again"
        );
        let failed = out.steps.iter().find(|s| !s.ok).unwrap();
        assert_eq!(failed.error_kind.as_deref(), Some("timeout"));
        let synth = &lead.requests()[2].prompt;
        assert!(synth.contains("status=\"no response\""));
    }

    #[tokio::test]
    async fn a_reviewer_that_fails_in_round_two_is_not_retried_and_the_run_is_partial() {
        let lead = lead_with(&["L1", "L-R1", "L-R2", SYNTH]);
        let beta = Arc::new(
            MockProvider::new("beta")
                .with_reply(t("B1"))
                .with_reply(t("B-R1"))
                .with_reply(MockReply::Malformed("garbage".into())),
        );
        let o = orch(vec![lead, beta.clone()], |_| {});
        let out = go(&o, q("Compare A and B", ConsultMode::Decision, 2))
            .await
            .unwrap();
        assert_eq!(out.status, DebateStatus::Partial);
        assert_eq!(beta.requests().len(), 3);
        assert!(
            out.warnings
                .iter()
                .any(|w| w.contains("round 2: beta failed"))
        );
    }

    #[tokio::test]
    async fn if_nobody_answers_the_consultation_fails_and_is_saved_as_failed() {
        let fail = |n: &str| {
            Arc::new(MockProvider::new(n).with_reply(MockReply::Auth("not logged in".into())))
        };
        let (o, store, _dir) = with_store(orch(vec![fail("lead"), fail("beta")], |_| {}));
        let err = go(&o, q("Compare A and B", ConsultMode::Decision, 1))
            .await
            .unwrap_err();
        assert!(matches!(err, OrchestraError::Auth { .. }), "{err:?}");
        let listing = store.list();
        assert_eq!(listing.debates.len(), 1);
        let saved = store.load(&listing.debates[0].id).unwrap();
        assert_eq!(saved.status, DebateStatus::Failed);
        assert_eq!(
            saved.consultation.unwrap().mode,
            Some(ConsultMode::Decision)
        );
    }

    #[tokio::test]
    async fn cancellation_stops_a_consultation_and_it_is_recorded_as_cancelled() {
        let gate = Arc::new(Barrier::new(9));
        let peers: Vec<_> = ["beta", "gamma"]
            .iter()
            .map(|n| Arc::new(MockProvider::new(n).with_gate(gate.clone())))
            .collect();
        let mut agents = vec![plain("lead")];
        agents.extend(peers);
        let (o, store, _dir) = with_store(orch(agents, |_| {}));
        let o = Arc::new(o);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut options = q("Compare A and B", ConsultMode::Decision, 1);
        options.events = Some(tx);
        let cancel = CancellationToken::new();
        let (o2, c2) = (o.clone(), cancel.clone());
        let task = tokio::spawn(async move { o2.consult(options, &c2).await });
        // Cancel once a perspective call has demonstrably started (no sleeps).
        while let Some(event) = rx.recv().await {
            if matches!(
                event,
                DebateEvent::CallStarted {
                    role: Role::Perspective,
                    ..
                }
            ) {
                break;
            }
        }
        cancel.cancel();
        let err = tokio::time::timeout(Duration::from_secs(30), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(matches!(err, OrchestraError::Cancelled { .. }), "{err:?}");
        let saved = store.load(&store.list().debates[0].id).unwrap();
        assert_eq!(saved.status, DebateStatus::Cancelled);
        assert!(saved.consultation.is_some());
    }

    #[tokio::test]
    async fn the_total_time_budget_stops_everything() {
        let gate = Arc::new(Barrier::new(9));
        let stuck = Arc::new(MockProvider::new("beta").with_gate(gate));
        let o = orch(vec![plain("lead"), stuck], |c| {
            c.debate.total_timeout_secs = 1
        });
        let err = go(&o, q("Compare A and B", ConsultMode::Decision, 1))
            .await
            .unwrap_err();
        assert!(
            matches!(err, OrchestraError::DebateTimeout { .. }),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn consultations_are_saved_listed_and_read_back_with_their_routing() {
        let lead = lead_with(&["a", "b", SYNTH]);
        let (o, store, _dir) = with_store(orch(vec![lead, plain("beta")], |_| {}));
        let mut options = q("Compare A and B", ConsultMode::Decision, 1);
        options.coordinator = Some("lead".into());
        let out = go(&o, options).await.unwrap();
        let path = out.saved_to.clone().expect("saved");
        assert!(path.exists());

        let back = store.load(&out.id).unwrap();
        let meta = back.consultation.as_ref().unwrap();
        assert_eq!(meta.manual_overrides, ["mode", "coordinator", "rounds"]);
        let routing = meta.routing.as_ref().unwrap();
        assert_eq!(routing.participants.source, Source::Auto);
        assert_eq!(back.final_answer, SYNTH);
        assert_eq!(back.steps.len(), out.steps.len());

        let listing = store.list();
        assert_eq!(listing.debates[0].mode.as_deref(), Some("decision"));
        let table = crate::history::render_history(&listing, 10);
        assert!(
            table.contains("KIND") && table.contains("decision"),
            "{table}"
        );

        // Rendering knows about consultations.
        let summary = back.render_summary();
        assert!(
            summary.starts_with(&format!(
                "Consultation {} (complete) · mode decision",
                back.id
            )),
            "{summary}"
        );
        assert!(summary.contains("Set by you:   mode, coordinator, rounds"));
        assert!(summary.contains("Routing:") && summary.contains("participants:"));
        let transcript = back.render_transcript();
        for part in [
            "## Independent perspectives",
            "## Round 1: review",
            "## Final synthesis",
        ] {
            assert!(transcript.contains(part), "{transcript}");
        }
    }

    #[tokio::test]
    async fn plain_debate_records_and_old_files_are_unaffected() {
        // A debate record has no consultation key and loads without one.
        let d = crate::history::tests::sample("20261011-094600-a1b2", 1, DebateStatus::Complete);
        let json = serde_json::to_value(&d).unwrap();
        assert!(
            json.get("consultation").is_none(),
            "debates keep their old shape"
        );
        assert!(json["steps"][0].get("usage").is_none());
        let back: DebateOutcome = serde_json::from_value(json).unwrap();
        assert!(back.consultation.is_none());
        // And a debate-only listing keeps the old header.
        let dir = tempfile::tempdir().unwrap();
        let store = DebateStore::new(dir.path().to_path_buf());
        store.save(&d).unwrap();
        let table = crate::history::render_history(&store.list(), 5);
        assert!(!table.contains("KIND"), "{table}");
    }

    #[tokio::test]
    async fn token_usage_is_reported_only_when_the_providers_report_it() {
        let lead = lead_with(&["a", "b", SYNTH]);
        let o = orch(vec![lead, plain("beta")], |_| {});
        let out = go(&o, q("Compare A and B", ConsultMode::Decision, 1))
            .await
            .unwrap();
        assert!(
            out.consultation.unwrap().token_usage.is_none(),
            "mocks report no tokens; none are invented"
        );
    }

    #[tokio::test]
    async fn invocation_counts_timings_and_events_are_recorded() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let o = orch(vec![plain("lead"), plain("beta")], |_| {});
        let mut options = q("Compare A and B", ConsultMode::Decision, 1);
        options.events = Some(tx);
        let out = go(&o, options).await.unwrap();
        let meta = out.consultation.as_ref().unwrap();
        assert_eq!(meta.invocations["lead"], 3);
        assert_eq!(meta.invocations["beta"], 2);
        assert_eq!(meta.provider_failures, 0);
        for stage in ["perspectives", "review", "synthesis"] {
            assert!(meta.stage_ms.contains_key(stage), "{stage}");
        }
        let mut events = Vec::new();
        while let Ok(e) = rx.try_recv() {
            events.push(e);
        }
        assert!(
            matches!(&events[0], DebateEvent::Started { mode: Some(m), .. } if m == "decision")
        );
        assert!(matches!(
            events.last(),
            Some(DebateEvent::Finished {
                status: DebateStatus::Complete,
                calls: 5,
                ..
            })
        ));
    }

    // ---- engine: validation, availability, recursion -----------------------------

    #[tokio::test]
    async fn invalid_requests_are_rejected_before_any_agent_is_called() {
        let lead = plain("lead");
        let o = orch(vec![lead.clone(), plain("beta")], |_| {});
        let bad = |edit: &dyn Fn(&mut ConsultOptions)| {
            let mut options = q("Compare A and B", ConsultMode::Decision, 1);
            edit(&mut options);
            options
        };
        assert!(matches!(
            go(&o, bad(&|o| o.question = "  ".into()))
                .await
                .unwrap_err(),
            OrchestraError::Config(_)
        ));
        assert!(matches!(
            go(&o, bad(&|o| o.rounds = Some(3))).await.unwrap_err(),
            OrchestraError::Config(_)
        ));
        assert!(matches!(
            go(&o, bad(&|o| o.coordinator = Some("nobody".into())))
                .await
                .unwrap_err(),
            OrchestraError::UnknownProvider { .. }
        ));
        assert!(matches!(
            go(
                &o,
                bad(&|o| o.agents = Some(vec!["lead".into(), "nobody".into()]))
            )
            .await
            .unwrap_err(),
            OrchestraError::UnknownProvider { .. }
        ));
        assert!(matches!(
            go(
                &o,
                bad(&|o| o.question = "x".repeat(QUESTION_MAX_BYTES + 1))
            )
            .await
            .unwrap_err(),
            OrchestraError::PromptTooLarge { .. }
        ));
        assert!(lead.requests().is_empty());
    }

    #[tokio::test]
    async fn unavailable_agents_are_skipped_and_reported_never_silently() {
        let o = orch(
            vec![
                plain("lead"),
                Arc::new(MockProvider::new("beta").missing()),
                plain("gamma"),
            ],
            |_| {},
        );
        // Automatic selection.
        let out = go(&o, q("Compare A and B", ConsultMode::Decision, 1))
            .await
            .unwrap();
        assert_eq!(out.skipped.len(), 1);
        assert_eq!(out.skipped[0].provider, id("beta"));
        assert_eq!(out.calls, 2 + 2 + 1);
        // Manual selection that includes the missing one.
        let mut options = q("Compare A and B", ConsultMode::Decision, 1);
        options.agents = Some(vec!["lead".into(), "beta".into(), "gamma".into()]);
        let out = go(&o, options).await.unwrap();
        assert!(
            out.skipped[0].reason.contains("requested but unavailable"),
            "{:?}",
            out.skipped
        );
        let plan = o.plan_consultation(&ConsultOptions {
            agents: Some(vec!["beta".into()]),
            ..q("x y", ConsultMode::Decision, 1)
        });
        assert!(
            plan.unwrap_err()
                .to_string()
                .contains("none of the requested agents")
        );
    }

    #[tokio::test]
    async fn an_excluded_host_is_never_called_and_cannot_coordinate() {
        let beta = plain("beta");
        let o = orch(vec![plain("lead"), beta.clone(), plain("gamma")], |c| {
            c.general.coordinator = "beta".into()
        });
        let mut options = q("Compare A and B", ConsultMode::Decision, 1);
        options.exclude = vec!["beta".into()];
        let out = go(&o, options.clone()).await.unwrap();
        assert!(beta.requests().is_empty(), "the host is never called");
        assert_ne!(
            out.coordinator,
            id("beta"),
            "the configured coordinator is the host, so another is chosen"
        );
        assert!(
            out.skipped
                .iter()
                .any(|s| s.provider == id("beta") && s.reason.contains("hosts this MCP server"))
        );
        // Explicitly asking for the host as coordinator is an error, not a silent change.
        options.coordinator = Some("beta".into());
        let err = go(&o, options).await.unwrap_err().to_string();
        assert!(
            err.contains("beta") && err.contains("hosts this MCP server"),
            "{err}"
        );
        assert!(beta.requests().is_empty());
    }

    #[tokio::test]
    async fn nested_orchestration_is_refused() {
        let mut config = Config::default();
        config.general.coordinator = "lead".into();
        let mut ctx = InvokeContext::from_config(&config);
        ctx.depth = ctx.max_depth;
        ctx.host_env.insert(ORCHESTRA_ACTIVE_ENV.to_string());
        let providers: Vec<Arc<dyn AgentProvider>> = vec![plain("lead"), plain("beta")];
        let o = Orchestrator::with_context(config, ProviderRegistry::new(providers), ctx).unwrap();
        let err = go(&o, q("Compare A and B", ConsultMode::Decision, 1))
            .await
            .unwrap_err();
        assert!(
            matches!(err, OrchestraError::NestedInvocation { .. }),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn a_custom_routing_strategy_can_replace_the_built_in_rules() {
        struct AlwaysLearn;
        impl RoutingStrategy for AlwaysLearn {
            fn route(&self, input: &RouteInput<'_>) -> Result<RoutingDecision> {
                let mut d = ConsultationRouter.route(input)?;
                d.mode = Routed::auto(ConsultMode::Learn, "custom strategy");
                Ok(d)
            }
        }
        let o = orch(vec![plain("lead"), plain("beta")], |_| {});
        let out = o
            .consult_with(
                &AlwaysLearn,
                ConsultOptions {
                    question: "Compare A and B".into(),
                    rounds: Some(1),
                    ..Default::default()
                },
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(out.consultation.unwrap().mode, Some(ConsultMode::Learn));
    }

    #[tokio::test]
    async fn parallel_stages_take_the_time_of_the_slowest_agent_not_the_sum() {
        // 3 agents x (answer + review) + 1 synthesis = 7 calls of 200 ms each. Run one at a
        // time that is 1.4 s; in parallel stages it is three sequential steps (~0.6 s). The
        // bound is loose on purpose: it only has to separate "parallel" from "sequential".
        let delay = Duration::from_millis(200);
        let agents: Vec<_> = ["lead", "beta", "gamma"]
            .iter()
            .map(|n| Arc::new(MockProvider::new(n).with_delay(delay)))
            .collect();
        let o = orch(agents, |_| {});
        let t = Instant::now();
        let out = go(&o, q("Compare A and B", ConsultMode::Decision, 1))
            .await
            .unwrap();
        let wall = t.elapsed();
        eprintln!(
            "PERF consult (mock, 200 ms per call): {} calls in {:?} (sequential would be {:?}); engine reported {} ms",
            out.calls,
            wall,
            delay * out.calls as u32,
            out.duration_ms
        );
        assert_eq!(out.calls, 7);
        assert!(wall < delay * 6, "stages must overlap, took {wall:?}");
    }

    #[test]
    fn a_question_that_asks_for_a_short_answer_gets_the_brief_style() {
        for q in [
            "In two sentences: tabs or spaces for a team of five?",
            "Briefly, should I use Rust or Go?",
            "Give me a one-line answer: monorepo or polyrepo?",
            "tl;dr: which oscilloscope should I buy?",
            "Compara Rust y Go en una frase",
        ] {
            let d = route(&input(q, three())).unwrap();
            assert!(d.brief.value, "{q}");
            assert_eq!(d.brief.source, Source::Auto);
            assert!(
                d.brief.reason.contains("asks for a short answer"),
                "{}",
                d.brief.reason
            );
        }
        for q in [
            "Which electronics microscope should I buy?",
            "Compare Rust and C++",
            "What is a sentence in grammar? Explain it deeply",
        ] {
            assert!(!route(&input(q, three())).unwrap().brief.value, "{q}");
        }
        assert!(
            !route(&input("Compare Rust and C++", three()))
                .unwrap()
                .brief
                .value
        );
    }

    #[test]
    fn length_can_be_forced_either_way_and_is_ignored_for_debates() {
        let q = "In two sentences: tabs or spaces?";
        let mut i = input(q, three());
        i.length = Some(Length::Full);
        let d = route(&i).unwrap();
        assert_eq!((d.brief.value, d.brief.source), (false, Source::Manual));
        assert!(d.manual_settings().contains(&"length".to_string()));

        let mut i = input("Compare Rust and C++", three());
        i.length = Some(Length::Brief);
        let d = route(&i).unwrap();
        assert_eq!((d.brief.value, d.brief.source), (true, Source::Manual));

        // The debate engine has its own format: brief is reported as ignored, never silently.
        i.mode = Some(ConsultMode::Debate);
        let d = route(&i).unwrap();
        assert!(!d.brief.value);
        assert!(
            d.warnings
                .iter()
                .any(|w| w.contains("--length brief was ignored")),
            "{:?}",
            d.warnings
        );
        let d = route(&input(
            "In two sentences: tabs or spaces? Debate it",
            three(),
        ))
        .unwrap();
        assert_eq!(d.mode.value, ConsultMode::Debate);
        assert!(!d.brief.value);

        assert_eq!(parse_length_arg(None).unwrap(), None);
        assert_eq!(parse_length_arg(Some("auto")).unwrap(), None);
        assert_eq!(
            parse_length_arg(Some("Brief")).unwrap(),
            Some(Length::Brief)
        );
        assert_eq!(parse_length_arg(Some("full")).unwrap(), Some(Length::Full));
        assert!(parse_length_arg(Some("huge")).is_err());
    }

    #[test]
    fn routing_decisions_saved_before_the_length_setting_still_load() {
        let d = route(&input("Compare A and B", three())).unwrap();
        let mut json = serde_json::to_value(&d).unwrap();
        json.as_object_mut().unwrap().remove("brief");
        let old: RoutingDecision = serde_json::from_value(json).unwrap();
        assert!(!old.brief.value, "older records read as the full style");
    }

    #[tokio::test]
    async fn the_brief_style_shortens_every_prompt_and_the_sections() {
        let lead = lead_with(&[]);
        let o = orch(vec![lead.clone(), plain("beta")], |_| {});
        let out = go(
            &o,
            ConsultOptions {
                question: "In two sentences: tabs or spaces?".into(),
                mode: Some(ConsultMode::Decision),
                rounds: Some(1),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let reqs = lead.requests();
        // Answer: no long "Understanding" note, a length instruction instead.
        assert!(
            reqs[0].prompt.contains("SHORT answer") && !reqs[0].prompt.contains("Understanding")
        );
        assert!(reqs[1].prompt.contains("at most five short bullet points"));
        // Synthesis: only the four short sections.
        let synth = &reqs[2].prompt;
        assert!(
            synth.contains("`## Answer` (exactly the length the question asks for"),
            "{synth}"
        );
        assert!(
            !synth.contains("## Alternatives and criteria"),
            "the long report is not requested"
        );
        assert!(out.consultation.unwrap().routing.unwrap().brief.value);

        // The full style is unchanged.
        let lead = lead_with(&[]);
        let o = orch(vec![lead.clone(), plain("beta")], |_| {});
        go(
            &o,
            q("Which design should we use?", ConsultMode::Decision, 1),
        )
        .await
        .unwrap();
        assert!(lead.requests()[0].prompt.contains("Understanding"));
        assert!(
            lead.requests()[2]
                .prompt
                .contains("## Alternatives and criteria")
        );
    }

    #[test]
    fn worst_case_prompts_stay_below_the_smallest_cli_limit() {
        // agy takes the prompt as an argument, capped at 128 KiB.
        let huge = "x".repeat(60 * 1024);
        let names: Vec<String> = ["a", "b", "c", "d", "e"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let slots: Vec<Slot> = names.iter().map(|_| Slot::Text(huge.clone())).collect();
        let question = "q".repeat(QUESTION_MAX_BYTES);
        let limit = 128 * 1024;
        for (mode, brief) in ConsultMode::ALL
            .into_iter()
            .flat_map(|m| [(m, false), (m, true)])
        {
            assert!(perspective_prompt(mode, brief, &question).len() < limit);
            assert!(
                review_prompt(mode, brief, &question, 0, &names, &slots).len() < limit,
                "{mode} review"
            );
            assert!(
                cross_review_prompt(mode, brief, &question, 0, &names, &slots, &slots).len()
                    < limit,
                "{mode} cross-review"
            );
            let reviews = vec![slots.clone(), slots.clone()];
            let p = synthesis_prompt(mode, brief, &question, &names, &slots, &reviews);
            assert!(p.len() < limit, "{mode} synthesis is {} bytes", p.len());
        }
    }
}
