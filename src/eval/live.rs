//! The live evaluation runner. It calls real providers, so it spends subscription quota.
//!
//! Safety, in order:
//! 1. [`plan`] is free: it routes every case and arm exactly as a real run would and reports how many
//!    agent calls each will make at most, calling nothing.
//! 2. [`run`] needs a [`LiveAuthorization`], which only the command line constructs (from an explicit
//!    flag, after printing the plan). The MCP server never has one, so a tool call cannot start a run.
//! 3. A run is refused when its worst case exceeds the call ceiling the caller set.
//! 4. Each consultation is isolated (no memory) and bounded by the per-consultation call limit and the
//!    timeout; a cancellation stops the run and what finished is kept, marked as partial.
//!
//! Results are recorded with provenance `live` and never mixed with fixture or mock results.

use tokio_util::sync::CancellationToken;

use super::citations;
use super::dataset::{Arm, Dataset, Provenance, RecordedResponse};
use super::run::{CaseResult, EvalRun, RunKind, evaluate_response, new_run};
use crate::consult::ConsultOptions;
use crate::error::{OrchestraError, Result};
use crate::orchestrator::Orchestrator;
use crate::research::plan::{ResearchRequest, WebMode};
use crate::smart::strategy::Strategy;

/// Proof that a person asked for a live run. Only constructed by the command line.
pub struct LiveAuthorization(());

impl LiveAuthorization {
    /// Call this only from an explicit user action (`--yes-run-live`). Never from a tool or a default.
    pub fn explicitly_granted_by_the_user() -> Self {
        Self(())
    }
}

#[derive(Debug, Clone)]
pub struct LiveLimits {
    /// Most agent calls one consultation may start.
    pub max_calls_per_consultation: usize,
    /// The whole run is refused when its worst case is above this.
    pub max_total_calls: usize,
    pub timeout_secs: Option<u64>,
}

impl Default for LiveLimits {
    fn default() -> Self {
        Self {
            max_calls_per_consultation: 10,
            max_total_calls: 150,
            timeout_secs: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PlanItem {
    pub case_id: String,
    pub arm: Arm,
    pub typical_calls: usize,
    pub max_calls: usize,
    /// Why it cannot run here, if so (it is then recorded as failed, not retried).
    pub blocked: Option<String>,
}

#[derive(Debug, Clone)]
pub struct LivePlan {
    pub items: Vec<PlanItem>,
    pub typical_total: usize,
    pub max_total: usize,
    pub limits: LiveLimits,
}

impl LivePlan {
    pub fn render(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::from(
            "LIVE EVALUATION PLAN - this will call the providers and use subscription quota.\n\n",
        );
        let _ = writeln!(
            out,
            "{:<22} {:<16} {:>8} {:>8}  note",
            "case", "arm", "typical", "at most"
        );
        for i in &self.items {
            let _ = writeln!(
                out,
                "{:<22} {:<16} {:>8} {:>8}  {}",
                i.case_id,
                i.arm.0,
                i.typical_calls,
                i.max_calls,
                i.blocked
                    .as_deref()
                    .map_or(String::new(), |b| format!("NOT RUNNABLE HERE: {b}"))
            );
        }
        let _ = writeln!(
            out,
            "\n{} consultation(s): about {} agent call(s), at most {}. Limits: {} call(s) per consultation, ceiling {} for the run{}.",
            self.items.len(),
            self.typical_total,
            self.max_total,
            self.limits.max_calls_per_consultation,
            self.limits.max_total_calls,
            self.limits
                .timeout_secs
                .map_or(String::new(), |t| format!(", {t}s per agent call"))
        );
        out.push_str("Each consultation runs alone with memory off, so arms stay comparable. Nothing is retried. Ctrl-C stops the run and keeps what finished.\n");
        out
    }
}

fn question_for(case: &super::dataset::Case) -> String {
    let mut q = String::new();
    if !case.evidence.is_empty() {
        q.push_str("Sources supplied with this question (cite them as [n]; they are the only evidence you have):\n");
        for e in &case.evidence {
            q.push_str(&format!(
                "[{}] {} ({}): {}\n",
                e.id, e.title, e.url, e.passage
            ));
        }
        q.push('\n');
    }
    if !case.history.is_empty() {
        q.push_str("Earlier in this conversation:\n");
        for t in &case.history {
            q.push_str(&format!("Q: {}\nA: {}\n", t.question, t.answer));
        }
        q.push_str("\nNew question: ");
    }
    q.push_str(&case.question);
    q
}

/// The sources a case supplies, as the citation checks see them: supplied, so they count as retrieved.
pub(crate) fn supplied_sources(case: &super::dataset::Case) -> Vec<super::dataset::ResponseSource> {
    case.evidence
        .iter()
        .map(|e| super::dataset::ResponseSource {
            id: e.id,
            url: e.url.clone(),
            retrieved: true,
            published: e.published.clone(),
            retrieved_at: Some("supplied with the question".to_string()),
            passage: e.passage.clone(),
        })
        .collect()
}

fn options_for(
    case: &super::dataset::Case,
    arm: &Arm,
    limits: &LiveLimits,
) -> Result<ConsultOptions> {
    let mode = crate::consult::parse_mode_arg(case.mode.as_deref())?;
    let (strategy, agents, coordinator) = match arm.single_agent() {
        Some(agent) => (
            Some(Strategy::Fast),
            Some(vec![agent.to_string()]),
            Some(agent.to_string()),
        ),
        None => (Some(arm.0.parse::<Strategy>()?), None, None),
    };
    Ok(ConsultOptions {
        question: question_for(case),
        mode,
        coordinator,
        agents,
        strategy,
        max_agent_calls: Some(limits.max_calls_per_consultation),
        timeout: limits.timeout_secs.map(std::time::Duration::from_secs),
        research: ResearchRequest {
            web: if case.web { WebMode::On } else { WebMode::Off },
            ..Default::default()
        },
        memory: None,
        ..Default::default()
    })
}

/// Which cases and arms to run: all cases when `case_ids` is `None`.
pub fn plan(
    orch: &Orchestrator,
    dataset: &Dataset,
    case_ids: Option<&[String]>,
    arms: &[Arm],
    limits: &LiveLimits,
) -> Result<LivePlan> {
    dataset.validate()?;
    if arms.is_empty() {
        return Err(OrchestraError::Config("name at least one arm".into()));
    }
    let mut items = Vec::new();
    for case in dataset
        .cases
        .iter()
        .filter(|c| case_ids.is_none_or(|ids| ids.contains(&c.id)))
    {
        for arm in arms {
            let options = options_for(case, arm, limits)?;
            let (typical, max, blocked) = match orch.plan_consultation(&options) {
                Ok(d) => {
                    let smart = d.smart.as_ref();
                    (
                        smart.map_or(1, |p| p.typical_calls),
                        smart
                            .map_or(1, |p| p.max_calls)
                            .min(limits.max_calls_per_consultation),
                        None,
                    )
                }
                Err(e) => (0, 0, Some(e.to_string())),
            };
            items.push(PlanItem {
                case_id: case.id.clone(),
                arm: arm.clone(),
                typical_calls: typical,
                max_calls: max,
                blocked,
            });
        }
    }
    if items.is_empty() {
        return Err(OrchestraError::Config(
            "no case matches the selection".into(),
        ));
    }
    Ok(LivePlan {
        typical_total: items.iter().map(|i| i.typical_calls).sum(),
        max_total: items.iter().map(|i| i.max_calls).sum(),
        items,
        limits: limits.clone(),
    })
}

fn failed_response(arm: &Arm, why: &str) -> RecordedResponse {
    RecordedResponse {
        arm: arm.clone(),
        provenance: Provenance::Live,
        answer: format!("(no answer: {why})"),
        status: "failed".to_string(),
        latency_ms: None,
        invocations: Some(0),
        agents: Vec::new(),
        context_chars: None,
        input_tokens: None,
        sources: Vec::new(),
    }
}

/// The supplied sources of a case, for tests that build a response by hand.
#[doc(hidden)]
pub fn supplied_sources_for_tests(
    case: &super::dataset::Case,
) -> Vec<super::dataset::ResponseSource> {
    supplied_sources(case)
}

/// Runs the plan sequentially. Returns the run even when cancelled part-way (marked in its notes).
pub async fn run(
    orch: &Orchestrator,
    dataset: &Dataset,
    case_ids: Option<&[String]>,
    arms: &[Arm],
    limits: &LiveLimits,
    _authorization: &LiveAuthorization,
    cancel: &CancellationToken,
) -> Result<EvalRun> {
    let plan = plan(orch, dataset, case_ids, arms, limits)?;
    if plan.max_total > limits.max_total_calls {
        return Err(OrchestraError::Config(format!(
            "the worst case is {} agent calls, above the ceiling of {} (raise --max-total-calls, or choose fewer cases or arms)",
            plan.max_total, limits.max_total_calls
        )));
    }
    let mut cases: Vec<CaseResult> = Vec::new();
    let mut notes = Vec::new();
    let mut done = 0usize;
    'outer: for case in dataset
        .cases
        .iter()
        .filter(|c| case_ids.is_none_or(|ids| ids.contains(&c.id)))
    {
        let mut results = Vec::new();
        for arm in arms {
            if cancel.is_cancelled() {
                notes.push(format!(
                    "CANCELLED after {done} of {} consultation(s): the run is partial",
                    plan.items.len()
                ));
                cases.push(case_result(case, results));
                break 'outer;
            }
            let options = options_for(case, arm, limits)?;
            let mut warnings: Vec<String> = Vec::new();
            let response = match orch.consult(options, cancel).await {
                Ok(o) => {
                    warnings = o
                        .warnings
                        .iter()
                        .take(10)
                        .map(|w| crate::store::redact_secrets(w))
                        .collect();
                    let mut sources = supplied_sources(case);
                    if let Some(r) = o.consultation.as_ref().and_then(|m| m.research.as_ref()) {
                        // Sources the program retrieved itself (web research) come after the supplied ones.
                        sources.extend(citations::sources_from_report(r));
                    }
                    let tokens = o
                        .consultation
                        .as_ref()
                        .and_then(|m| m.token_usage.as_ref())
                        .filter(|t| t.calls_reporting == t.calls_total && t.calls_total > 0);
                    RecordedResponse {
                        arm: arm.clone(),
                        provenance: Provenance::Live,
                        answer: o.final_answer.clone(),
                        status: o.status.as_str().to_string(),
                        latency_ms: Some(o.duration_ms),
                        invocations: Some(o.calls),
                        agents: std::iter::once(o.coordinator.to_string())
                            .chain(o.participants.iter().map(ToString::to_string))
                            .collect(),
                        context_chars: None,
                        input_tokens: tokens.map(|t| t.input_tokens),
                        sources,
                    }
                }
                Err(e) => {
                    notes.push(format!("case `{}` arm `{arm}`: {e}", case.id));
                    failed_response(arm, &e.to_string())
                }
            };
            let mut result = evaluate_response(case, &response);
            result.warnings = warnings;
            results.push(result);
            done += 1;
        }
        cases.push(case_result(case, results));
    }
    let mut all_arms = arms.to_vec();
    all_arms.sort();
    all_arms.dedup();
    Ok(new_run(dataset, RunKind::Live, all_arms, cases, notes))
}

fn case_result(case: &super::dataset::Case, arms: Vec<super::run::ArmResult>) -> CaseResult {
    CaseResult {
        case_id: case.id.clone(),
        category: case.category,
        objective: case.objective,
        question: case.question.clone(),
        instructions: case.instructions.clone(),
        single_agent_sufficient: case.expect.single_agent_sufficient,
        arms,
    }
}
