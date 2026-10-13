//! The quality gate: structural checks on a finished consultation, so that an optimisation can
//! never turn into an overclaim.
//!
//! The gate looks at objective properties of the result and of its evidence records: whether the
//! evidence that was required is there, whether every citation in the answer names a retrieved
//! source, whether the claims that were meant to be checked were, whether claims the evidence
//! contradicts are visible in the answer, whether the review the user asked for took place, whether
//! limitations are disclosed, and whether `complete` was awarded honestly. It does **not** judge
//! whether the answer is *correct*: a result can pass every check and still be wrong. A passed
//! check means a required piece is present, nothing more.
//!
//! A failed blocking check downgrades `complete` to `partial`, adds a warning and a short
//! "Quality notes" section to the answer, so the result is honestly limited rather than silently
//! overstated.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::budget::BudgetStop;
use crate::debate::{DebateOutcome, DebateStatus};
use crate::research::citations::citation_numbers;
use crate::research::claims::ClaimStatus;
use crate::research::engine::ResearchReport;
use crate::research::source::SourceStatus;

/// What the user and the plan required of this consultation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QualityRequirements {
    /// Web research was explicitly required (`--web on`, user URLs, a claim check).
    pub research_required: bool,
    /// The research depth judges claims against passages, so claims are expected to be checked.
    pub verification_expected: bool,
    /// Agents the user named. Each must have been called.
    pub explicit_agents: Vec<String>,
    /// Review rounds the user asked for.
    pub explicit_rounds: Option<u8>,
    /// Review rounds the plan includes (a fixed panel).
    pub planned_rounds: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// A failure makes the result partial.
    Blocking,
    /// A failure is reported but does not change the status.
    Advisory,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QualityCheck {
    pub name: String,
    pub severity: Severity,
    pub passed: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QualityReport {
    pub checks: Vec<QualityCheck>,
}

impl QualityReport {
    pub fn violations(&self) -> Vec<&QualityCheck> {
        self.checks
            .iter()
            .filter(|c| !c.passed && c.severity == Severity::Blocking)
            .collect()
    }

    pub fn advisories(&self) -> Vec<&QualityCheck> {
        self.checks
            .iter()
            .filter(|c| !c.passed && c.severity == Severity::Advisory)
            .collect()
    }

    pub fn passed(&self) -> bool {
        self.violations().is_empty()
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct QualityGate;

/// What happened, from the point of view of the gate.
pub struct QualityInput<'a> {
    pub outcome: &'a DebateOutcome,
    pub research: Option<&'a ResearchReport>,
    pub budget_stop: Option<&'a BudgetStop>,
    pub requirements: &'a QualityRequirements,
    /// The plan was one agent writing the answer (no review was planned).
    pub single_agent: bool,
}

/// The part of an answer written by the agents: everything before the sections that this program
/// generates (the claim table, the sources, the limitations).
fn agent_body(answer: &str) -> &str {
    [
        "\n## Claim verification",
        "\n## Sources",
        "\n## Research limitations",
    ]
    .iter()
    .filter_map(|h| answer.find(h))
    .min()
    .map_or(answer, |i| &answer[..i])
}

impl QualityGate {
    pub fn check(&self, input: &QualityInput<'_>) -> QualityReport {
        let o = input.outcome;
        let req = input.requirements;
        let mut checks = Vec::new();
        let mut add = |name: &str, severity: Severity, passed: bool, detail: String| {
            checks.push(QualityCheck {
                name: name.to_string(),
                severity,
                passed,
                detail,
            });
        };

        let retrieved: BTreeSet<u32> = input
            .research
            .map(|r| {
                r.sources
                    .iter()
                    .filter(|s| s.status == SourceStatus::Retrieved)
                    .map(|s| s.id)
                    .collect()
            })
            .unwrap_or_default();

        // 1. The evidence that was required exists.
        if req.research_required {
            let ok = !retrieved.is_empty();
            add(
                "required_evidence_present",
                Severity::Blocking,
                ok,
                if ok {
                    format!("{} source(s) were retrieved", retrieved.len())
                } else {
                    "research was required but no source could be retrieved".to_string()
                },
            );
        }

        // 2. Every citation in the agents' text names a retrieved source.
        if input.research.is_some() {
            let cited = citation_numbers(agent_body(&o.final_answer));
            let bad: Vec<u32> = cited.difference(&retrieved).copied().collect();
            add(
                "citation_references_valid",
                Severity::Blocking,
                bad.is_empty(),
                if bad.is_empty() {
                    format!(
                        "{} citation number(s), all naming retrieved sources",
                        cited.len()
                    )
                } else {
                    format!("the answer cites {bad:?}, which are not retrieved sources")
                },
            );
        }

        if let Some(r) = input.research {
            // 3. Claims that were meant to be checked were.
            if req.verification_expected && !retrieved.is_empty() && !r.claims.is_empty() {
                let judged = r
                    .claims
                    .iter()
                    .filter(|c| c.status != ClaimStatus::NotChecked)
                    .count();
                add(
                    "claims_checked",
                    Severity::Blocking,
                    judged > 0,
                    if judged > 0 {
                        format!(
                            "{judged} of {} claim(s) were checked against the sources",
                            r.claims.len()
                        )
                    } else {
                        "the claims were listed but none was checked against the sources"
                            .to_string()
                    },
                );
            }
            // 4. Contradicted claims are visible in the answer.
            let contested: Vec<&crate::research::claims::ClaimRecord> = r
                .claims
                .iter()
                .filter(|c| {
                    matches!(
                        c.status,
                        ClaimStatus::Contradicted | ClaimStatus::MixedEvidence
                    )
                })
                .collect();
            if !contested.is_empty() {
                let hidden: Vec<u32> = contested
                    .iter()
                    .filter(|c| {
                        !(o.final_answer.contains(&c.text)
                            && o.final_answer.contains(c.status.as_str()))
                    })
                    .map(|c| c.id)
                    .collect();
                add(
                    "contradictions_visible",
                    Severity::Blocking,
                    hidden.is_empty(),
                    if hidden.is_empty() {
                        format!(
                            "{} contested claim(s) are listed in the answer",
                            contested.len()
                        )
                    } else {
                        format!("contested claim(s) {hidden:?} do not appear in the answer")
                    },
                );
            }
        }

        // 5. The review and the agents the user asked for took place.
        let rounds_run = o.consultation.as_ref().map_or(0, |m| m.rounds_run);
        if let Some(want) = req.explicit_rounds {
            let enough_answers = o.steps.iter().filter(|s| s.ok).count() >= 2;
            let ok = rounds_run >= want || !enough_answers;
            add(
                "requested_review_satisfied",
                Severity::Blocking,
                ok,
                if rounds_run >= want {
                    format!("{rounds_run} review round(s) ran, {want} requested")
                } else if !enough_answers {
                    "fewer than two agents answered, so there was nothing to review (reported as a failure)".to_string()
                } else {
                    format!("only {rounds_run} of the {want} review round(s) you asked for ran")
                },
            );
        }
        if !req.explicit_agents.is_empty() {
            let called: BTreeSet<String> = o.steps.iter().map(|s| s.agent.to_string()).collect();
            let missing: Vec<&String> = req
                .explicit_agents
                .iter()
                .filter(|a| !called.contains(*a))
                .filter(|a| !o.skipped.iter().any(|s| &s.provider.to_string() == *a))
                .collect();
            add(
                "requested_agents_consulted",
                Severity::Blocking,
                missing.is_empty(),
                if missing.is_empty() {
                    "every agent you named was called or is reported as unavailable".to_string()
                } else {
                    format!("{missing:?} were named but never called and are not reported")
                },
            );
        }

        // 6. Failures and stops are disclosed.
        let failed = o.steps.iter().filter(|s| !s.ok).count();
        let problem = failed > 0 || input.budget_stop.is_some();
        if problem {
            let disclosed = !o.warnings.is_empty() && o.status != DebateStatus::Complete;
            add(
                "limitations_disclosed",
                Severity::Blocking,
                disclosed,
                if disclosed {
                    format!(
                        "{failed} failed call(s){} are reported as warnings",
                        input
                            .budget_stop
                            .map_or(String::new(), |b| format!(" and a budget stop ({b})"))
                    )
                } else {
                    "calls failed or a limit stopped the work, but the result is not marked partial with warnings".to_string()
                },
            );
        }

        // 7. `complete` is only awarded when everything planned ran.
        if o.status == DebateStatus::Complete {
            let mut why = Vec::new();
            if failed > 0 {
                why.push(format!("{failed} agent call(s) failed"));
            }
            if let Some(b) = input.budget_stop {
                why.push(format!("work stopped early: {b}"));
            }
            if req.planned_rounds > 0 && !input.single_agent && rounds_run < req.planned_rounds {
                why.push(format!(
                    "{rounds_run} of {} planned review round(s) ran",
                    req.planned_rounds
                ));
            }
            add(
                "completeness_classified",
                Severity::Blocking,
                why.is_empty(),
                if why.is_empty() {
                    "everything that was planned ran".to_string()
                } else {
                    format!("marked complete although {}", why.join("; "))
                },
            );
        }

        // Advisory: a single agent's answer has had no independent review.
        if input.single_agent {
            add(
                "independent_review",
                Severity::Advisory,
                false,
                "one agent wrote the answer and nobody reviewed it".to_string(),
            );
        }
        QualityReport { checks }
    }

    /// Makes the result honest about a failed blocking check: `complete` becomes `partial`, the
    /// failed checks become warnings and a short section at the end of the answer.
    pub fn apply(&self, report: &QualityReport, outcome: &mut DebateOutcome) {
        let violations = report.violations();
        if violations.is_empty() {
            return;
        }
        if outcome.status == DebateStatus::Complete {
            outcome.status = DebateStatus::Partial;
        }
        for v in &violations {
            outcome
                .warnings
                .push(format!("quality gate: {}: {}", v.name, v.detail));
        }
        if !outcome.final_answer.trim().is_empty() {
            outcome.final_answer.push_str("\n\n## Quality notes\n\n");
            for v in &violations {
                outcome.final_answer.push_str(&format!(
                    "- {}: {}\n",
                    v.name.replace('_', " "),
                    v.detail
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consult::ConsultationMeta;
    use crate::debate::{Role, Step};
    use crate::research::claims::ClaimRecord;
    use crate::research::engine::{CitationSummary, ResearchBudget};
    use crate::research::plan::{Depth, WebMode};
    use crate::research::quality::ClaimCategory;
    use crate::research::source::SourceRecord;
    use crate::store::ConversationId;
    use crate::types::ProviderId;

    fn step(agent: &str, ok: bool) -> Step {
        Step {
            phase: Role::Perspective,
            round: 0,
            agent: ProviderId::new(agent),
            offset_ms: 0,
            duration_ms: 1,
            ok,
            text: ok.then(|| "t".to_string()),
            error: (!ok).then(|| "boom".to_string()),
            error_kind: (!ok).then(|| "error".to_string()),
            settings: None,
            usage: None,
        }
    }

    fn outcome(steps: Vec<Step>, answer: &str, status: DebateStatus) -> DebateOutcome {
        DebateOutcome {
            schema: 1,
            id: "id".into(),
            conversation: ConversationId(0),
            created_at_ms: 0,
            question: "q".into(),
            coordinator: ProviderId::new("a"),
            participants: vec![],
            skipped: vec![],
            rounds_planned: 1,
            status,
            synthesized: true,
            calls: steps.len(),
            steps,
            initial_proposal: String::new(),
            final_answer: answer.into(),
            error: None,
            warnings: vec![],
            duration_ms: 1,
            consultation: Some(ConsultationMeta::default()),
            summaries: Vec::new(),
            saved_to: None,
        }
    }

    fn source(id: u32, status: SourceStatus) -> SourceRecord {
        let mut set = crate::research::source::SourceSet::new();
        let n = set
            .add_candidate(
                &format!("https://example.org/{id}"),
                crate::research::source::Discovery {
                    method: crate::research::source::DiscoveryMethod::UserProvided,
                    provider: None,
                },
            )
            .unwrap();
        let mut r = set.get(n).unwrap().clone();
        r.id = id;
        r.status = status;
        r
    }

    fn claim(id: u32, text: &str, status: ClaimStatus) -> ClaimRecord {
        ClaimRecord {
            id,
            text: text.into(),
            category: ClaimCategory::Other,
            originating_agents: vec!["a".into()],
            needs_external_evidence: true,
            cited_by_agents: vec![],
            status,
            evidence: vec![],
            supporting_independent_sources: 0,
            contradicting_independent_sources: 0,
            notes: vec![],
        }
    }

    fn report(sources: Vec<SourceRecord>, claims: Vec<ClaimRecord>) -> ResearchReport {
        ResearchReport {
            schema: 1,
            web_requested: WebMode::Auto,
            depth: Depth::Standard,
            max_sources: 6,
            freshness_days: None,
            discovery_agent: None,
            subquestions: vec![],
            search_queries: vec![],
            sources,
            claims,
            contradictions: vec![],
            citations: CitationSummary::default(),
            assessors: vec![],
            budget: ResearchBudget::default(),
            limitations: vec![],
        }
    }

    fn run(
        o: &DebateOutcome,
        r: Option<&ResearchReport>,
        req: &QualityRequirements,
        stop: Option<&BudgetStop>,
        single: bool,
    ) -> QualityReport {
        QualityGate.check(&QualityInput {
            outcome: o,
            research: r,
            budget_stop: stop,
            requirements: req,
            single_agent: single,
        })
    }

    fn failed_names(r: &QualityReport) -> Vec<String> {
        r.violations().iter().map(|c| c.name.clone()).collect()
    }

    #[test]
    fn a_clean_result_passes_and_the_gate_does_not_change_it() {
        let mut o = outcome(
            vec![step("a", true), step("b", true)],
            "## Answer\nfine",
            DebateStatus::Complete,
        );
        let rep = run(&o, None, &QualityRequirements::default(), None, false);
        assert!(rep.passed(), "{rep:?}");
        let before = o.clone();
        QualityGate.apply(&rep, &mut o);
        assert_eq!(o, before);
    }

    #[test]
    fn required_research_without_a_retrieved_source_blocks_complete() {
        let mut o = outcome(vec![step("a", true)], "answer", DebateStatus::Complete);
        let r = report(vec![source(1, SourceStatus::Inaccessible)], vec![]);
        let req = QualityRequirements {
            research_required: true,
            ..Default::default()
        };
        let rep = run(&o, Some(&r), &req, None, true);
        assert!(failed_names(&rep).contains(&"required_evidence_present".to_string()));
        QualityGate.apply(&rep, &mut o);
        assert_eq!(o.status, DebateStatus::Partial);
        assert!(
            o.warnings
                .iter()
                .any(|w| w.starts_with("quality gate: required_evidence_present"))
        );
        assert!(o.final_answer.contains("## Quality notes"));
    }

    #[test]
    fn citations_must_name_retrieved_sources_in_the_agents_text_only() {
        let r = report(
            vec![
                source(1, SourceStatus::Retrieved),
                source(2, SourceStatus::Inaccessible),
            ],
            vec![],
        );
        let body = "The spec says 100 MHz [1].\n\n## Sources\n\n[1] a\n\n[2] b (not retrieved)";
        let ok = outcome(vec![step("a", true)], body, DebateStatus::Complete);
        assert!(
            run(&ok, Some(&r), &QualityRequirements::default(), None, true).passed(),
            "the generated Sources section may list unretrieved ids"
        );
        let bad = outcome(
            vec![step("a", true)],
            "It is 100 MHz [2] and [9].",
            DebateStatus::Complete,
        );
        let rep = run(&bad, Some(&r), &QualityRequirements::default(), None, true);
        let v = rep.violations();
        assert_eq!(v.len(), 1);
        assert!(v[0].detail.contains("[2, 9]"), "{}", v[0].detail);
    }

    #[test]
    fn claims_that_were_never_checked_cannot_stand_as_verified() {
        let o = outcome(vec![step("a", true)], "answer", DebateStatus::Complete);
        let r = report(
            vec![source(1, SourceStatus::Retrieved)],
            vec![claim(1, "X is 5", ClaimStatus::NotChecked)],
        );
        let req = QualityRequirements {
            verification_expected: true,
            ..Default::default()
        };
        assert!(
            failed_names(&run(&o, Some(&r), &req, None, true))
                .contains(&"claims_checked".to_string())
        );
        // At a depth that does not judge, nothing was expected, so nothing fails.
        let quick = QualityRequirements {
            verification_expected: false,
            ..Default::default()
        };
        assert!(run(&o, Some(&r), &quick, None, true).passed());
    }

    #[test]
    fn contradicted_claims_must_be_visible_in_the_answer() {
        let r = report(
            vec![source(1, SourceStatus::Retrieved)],
            vec![claim(1, "X is 5", ClaimStatus::Contradicted)],
        );
        let hidden = outcome(
            vec![step("a", true)],
            "Everything is fine.",
            DebateStatus::Complete,
        );
        assert!(
            failed_names(&run(
                &hidden,
                Some(&r),
                &QualityRequirements::default(),
                None,
                true
            ))
            .contains(&"contradictions_visible".to_string())
        );
        let shown = outcome(
            vec![step("a", true)],
            "Body.\n\n## Claim verification\n\n- **CONTRADICTED** — X is 5 (claim 1)\n",
            DebateStatus::Complete,
        );
        assert!(
            run(
                &shown,
                Some(&r),
                &QualityRequirements::default(),
                None,
                true
            )
            .passed()
        );
    }

    #[test]
    fn the_review_the_user_asked_for_must_have_happened() {
        let meta = ConsultationMeta {
            rounds_run: 1,
            ..Default::default()
        };
        let mut o = outcome(
            vec![step("a", true), step("b", true)],
            "answer",
            DebateStatus::Complete,
        );
        o.consultation = Some(meta);
        let req = QualityRequirements {
            explicit_rounds: Some(2),
            planned_rounds: 2,
            ..Default::default()
        };
        let rep = run(&o, None, &req, None, false);
        let names = failed_names(&rep);
        assert!(
            names.contains(&"requested_review_satisfied".to_string()),
            "{names:?}"
        );
        assert!(
            names.contains(&"completeness_classified".to_string()),
            "complete needs every planned round: {names:?}"
        );
        QualityGate.apply(&rep, &mut o);
        assert_eq!(o.status, DebateStatus::Partial);
    }

    #[test]
    fn named_agents_that_were_never_called_must_be_reported_not_dropped() {
        let o = outcome(vec![step("a", true)], "answer", DebateStatus::Partial);
        let req = QualityRequirements {
            explicit_agents: vec!["a".into(), "b".into()],
            ..Default::default()
        };
        let rep = run(&o, None, &req, None, false);
        assert!(failed_names(&rep).contains(&"requested_agents_consulted".to_string()));
        let mut reported = o.clone();
        reported.skipped.push(crate::debate::Skipped {
            provider: ProviderId::new("b"),
            reason: "not installed".into(),
        });
        assert!(run(&reported, None, &req, None, false).passed());
    }

    #[test]
    fn a_failed_call_or_a_budget_stop_must_not_be_called_complete() {
        let mut o = outcome(
            vec![step("a", true), step("b", false)],
            "answer",
            DebateStatus::Complete,
        );
        let rep = run(&o, None, &QualityRequirements::default(), None, false);
        let names = failed_names(&rep);
        assert!(
            names.contains(&"limitations_disclosed".to_string())
                && names.contains(&"completeness_classified".to_string()),
            "{names:?}"
        );
        QualityGate.apply(&rep, &mut o);
        assert_eq!(o.status, DebateStatus::Partial);
        // Properly labelled partial results with warnings pass the disclosure check.
        let mut labelled = outcome(
            vec![step("a", true), step("b", false)],
            "answer",
            DebateStatus::Partial,
        );
        labelled.warnings.push("b failed".into());
        assert!(
            run(
                &labelled,
                None,
                &QualityRequirements::default(),
                None,
                false
            )
            .passed()
        );
        let stop = BudgetStop::Calls { limit: 3 };
        let stopped = outcome(vec![step("a", true)], "answer", DebateStatus::Complete);
        assert!(
            !run(
                &stopped,
                None,
                &QualityRequirements::default(),
                Some(&stop),
                false
            )
            .passed()
        );
    }

    #[test]
    fn a_single_agent_answer_is_flagged_as_unreviewed_without_changing_its_status() {
        let mut o = outcome(vec![step("a", true)], "answer", DebateStatus::Complete);
        let rep = run(&o, None, &QualityRequirements::default(), None, true);
        assert!(rep.passed());
        assert_eq!(rep.advisories().len(), 1);
        QualityGate.apply(&rep, &mut o);
        assert_eq!(o.status, DebateStatus::Complete);
    }
}
