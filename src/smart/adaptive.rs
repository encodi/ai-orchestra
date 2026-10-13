//! Adaptive deliberation: after each stage, decide whether the next stage is worth its cost.
//!
//! The controller is a pure function of structured facts, so every decision is explainable and
//! testable: it never calls a model and it never guesses a number. What it reads:
//!
//! * the question's features (contested, complex, high-stakes) and the mode;
//! * the evidence records of the research layer: how many retrieved sources there are and how the
//!   claims the answer relied on came out (`SUPPORTED`, `CONTRADICTED`, `MIXED_EVIDENCE`,
//!   `INSUFFICIENT_EVIDENCE`, `NOT_CHECKED`), and whether verification failed;
//! * the categorical verdict an independent reviewer ended its review with. That verdict is one
//!   model's judgement of another model's answer, so it is only ever used to decide whether to look
//!   closer, never as a measure of correctness or as a confidence;
//! * what is left of the budget.
//!
//! Agreement between models is not treated as proof: an answer is not declared sufficient because
//! two agents agree, only because the retrieved evidence supports what it relied on (or because the
//! question is simple and nothing in the results calls for a second look). When the evidence is
//! missing, more deliberation cannot create it, so the controller stops and says so instead of
//! forcing more rounds.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::consult::ConsultMode;
use crate::research::claims::{ClaimRecord, ClaimStatus};

/// What the independent reviewer concluded about the answer it read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewVerdict {
    Agree,
    MinorIssues,
    MaterialDisagreement,
    /// No machine-readable verdict (missing, malformed or the template echoed back).
    Unknown,
}

impl ReviewVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            ReviewVerdict::Agree => "AGREE",
            ReviewVerdict::MinorIssues => "MINOR_ISSUES",
            ReviewVerdict::MaterialDisagreement => "MATERIAL_DISAGREEMENT",
            ReviewVerdict::Unknown => "UNKNOWN",
        }
    }
}

/// Reads the last `VERDICT:` line of a review.
pub fn parse_verdict(text: &str) -> ReviewVerdict {
    for line in text.lines().rev() {
        let cleaned: String = line
            .trim()
            .trim_start_matches(['#', '>', '-', '*', '`', ' '])
            .to_string();
        let lower = cleaned.to_lowercase();
        let Some(rest) = lower.strip_prefix("verdict") else {
            continue;
        };
        let value = rest
            .trim_start_matches([' ', ':', '*', '`', '=', '-'])
            .trim_end_matches(['*', '`', '.', ' '])
            .replace(['_', '-'], " ");
        // The prompt's own template lists the three choices; echoing it back is no verdict.
        if value.contains('|') || value.contains(',') || value.contains(" or ") {
            return ReviewVerdict::Unknown;
        }
        return match value.trim() {
            "agree" => ReviewVerdict::Agree,
            "minor issues" | "minor" => ReviewVerdict::MinorIssues,
            "material disagreement" | "material" | "disagree" => {
                ReviewVerdict::MaterialDisagreement
            }
            _ => ReviewVerdict::Unknown,
        };
    }
    ReviewVerdict::Unknown
}

/// How the claims the answer relied on came out. Counts of evidence records, not opinions.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimTally {
    pub total: usize,
    pub supported: usize,
    pub contradicted: usize,
    pub mixed: usize,
    pub insufficient: usize,
    pub not_checked: usize,
    /// Claims that need outside evidence and ended `INSUFFICIENT_EVIDENCE`.
    pub unsupported_external: usize,
}

impl ClaimTally {
    pub fn from_claims(claims: &[ClaimRecord]) -> Self {
        let mut t = Self {
            total: claims.len(),
            ..Self::default()
        };
        for c in claims {
            match c.status {
                ClaimStatus::Supported => t.supported += 1,
                ClaimStatus::Contradicted => t.contradicted += 1,
                ClaimStatus::MixedEvidence => t.mixed += 1,
                ClaimStatus::InsufficientEvidence => {
                    t.insufficient += 1;
                    if c.needs_external_evidence {
                        t.unsupported_external += 1;
                    }
                }
                ClaimStatus::NotChecked => t.not_checked += 1,
            }
        }
        t
    }

    /// Claims the retrieved evidence contradicts, wholly or in part.
    pub fn contested(&self) -> usize {
        self.contradicted + self.mixed
    }

    pub fn all_supported(&self) -> bool {
        self.total > 0 && self.supported == self.total
    }
}

/// Everything the controller looks at, gathered by the engine after each stage.
#[derive(Debug, Clone)]
pub struct Signals {
    pub mode: ConsultMode,
    pub contested_question: bool,
    pub complex_question: bool,
    pub high_stakes: bool,
    pub research_active: bool,
    pub retrieved_sources: usize,
    pub claims: ClaimTally,
    /// Verification was part of the plan and could not be completed.
    pub verification_failed: bool,
    /// The reviewer's verdict, once there is a review.
    pub review: Option<ReviewVerdict>,
    /// Agents that have not been asked anything yet and could still be used.
    pub agents_left: usize,
    pub rounds_done: u8,
    pub max_rounds: u8,
    pub escalations_done: u8,
    pub calls_left: Option<usize>,
    pub time_left: Option<Duration>,
}

impl Signals {
    fn fits(&self, calls: usize) -> bool {
        self.calls_left.is_none_or(|left| left >= calls)
            && self.time_left.is_none_or(|t| !t.is_zero())
    }
}

/// The next stage to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NextStep {
    /// Stop here: the answer already in hand is the result.
    Finish,
    /// One other agent reviews the answer.
    Review,
    /// Another agent answers independently, without seeing the others.
    IndependentPerspective,
    /// The lead agent reconsiders after reading the review.
    Reconsider,
    /// The coordinator writes the final answer from everything gathered.
    Synthesize,
}

impl NextStep {
    pub fn as_str(self) -> &'static str {
        match self {
            NextStep::Finish => "finish",
            NextStep::Review => "review",
            NextStep::IndependentPerspective => "independent_perspective",
            NextStep::Reconsider => "reconsider",
            NextStep::Synthesize => "synthesize",
        }
    }
}

/// Why deliberation stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum StopReason {
    /// Nothing in the results calls for more work.
    Sufficient(String),
    /// The independent review found no material issue.
    ReviewClean,
    /// More deliberation cannot supply what is missing.
    EvidenceCannotImprove(String),
    /// A limit was reached before the next stage.
    Budget(String),
    /// There is no other agent left to ask.
    NoAgentLeft,
    /// The bounded escalation was used; what remains is reported, not argued further.
    EscalationLimit(String),
}

impl std::fmt::Display for StopReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StopReason::Sufficient(why) => write!(f, "sufficient: {why}"),
            StopReason::ReviewClean => {
                f.write_str("the independent review found no material issue")
            }
            StopReason::EvidenceCannotImprove(why) => {
                write!(f, "more deliberation cannot help: {why}")
            }
            StopReason::Budget(why) => write!(f, "budget: {why}"),
            StopReason::NoAgentLeft => f.write_str("no other agent is left to ask"),
            StopReason::EscalationLimit(why) => write!(f, "escalation limit: {why}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    pub next: NextStep,
    /// Why this step (or why stopping), in plain words.
    pub reasons: Vec<String>,
    pub stop: Option<StopReason>,
}

impl Decision {
    fn go(next: NextStep, reasons: Vec<String>) -> Self {
        Self {
            next,
            reasons,
            stop: None,
        }
    }

    fn stop(next: NextStep, stop: StopReason, reasons: Vec<String>) -> Self {
        Self {
            next,
            reasons,
            stop: Some(stop),
        }
    }
}

/// Conservative, explainable escalation policy. Escalation is bounded: one review, at most one
/// further perspective (or one reconsideration), then the synthesis.
#[derive(Debug, Clone, Copy, Default)]
pub struct AdaptiveDeliberationController;

impl AdaptiveDeliberationController {
    /// The reasons, from the question alone, to have the answer reviewed by someone else.
    fn opinion_reasons(s: &Signals) -> Vec<String> {
        let mut opinion = Vec::new();
        if matches!(s.mode, ConsultMode::Decision | ConsultMode::Review) {
            opinion.push(format!(
                "a {} question: the criteria and trade-offs deserve an independent check",
                s.mode
            ));
        }
        if s.contested_question {
            opinion.push("the question is contested".to_string());
        }
        if s.complex_question {
            opinion.push("the question is long or has several parts".to_string());
        }
        if s.high_stakes {
            opinion.push(
                "the question touches money, health, safety or a lasting commitment".to_string(),
            );
        }
        opinion
    }

    /// Whether the review will happen whatever the evidence check finds, so that the check and the
    /// review can safely run at the same time. True only when the question alone calls for a
    /// review, another agent is available and the budget allows the review and a synthesis.
    pub fn review_regardless_of_evidence(&self, s: &Signals) -> bool {
        !Self::opinion_reasons(s).is_empty() && s.agents_left > 0 && s.fits(2)
    }

    /// After the lead agent's answer (and its evidence check, if there is research).
    pub fn after_initial(&self, s: &Signals) -> Decision {
        let opinion = Self::opinion_reasons(s);
        let mut evidence = Vec::new();
        if s.claims.contested() > 0 {
            evidence.push(format!(
                "the retrieved evidence contradicts {} claim(s) the answer relied on",
                s.claims.contested()
            ));
        }
        if s.claims.unsupported_external > 0 && s.retrieved_sources > 0 {
            evidence.push(format!(
                "{} claim(s) that need outside evidence are not supported by the retrieved sources",
                s.claims.unsupported_external
            ));
        }
        if s.verification_failed {
            evidence.push("the evidence check could not be completed".to_string());
        }

        // Research was part of the task and produced no source at all: no agent can supply any.
        if opinion.is_empty() && s.research_active && s.retrieved_sources == 0 {
            let why = "no source could be retrieved, so another agent could not check the answer against any evidence";
            return Decision::stop(
                NextStep::Finish,
                StopReason::EvidenceCannotImprove(why.to_string()),
                vec![why.to_string()],
            );
        }
        if opinion.is_empty() && evidence.is_empty() {
            let why = if s.research_active && s.claims.all_supported() {
                "every claim the answer relied on is supported by retrieved sources and the question is not contested"
            } else {
                "the question is simple and uncontested, and nothing in the result calls for a second look"
            };
            return Decision::stop(
                NextStep::Finish,
                StopReason::Sufficient(why.to_string()),
                vec![why.to_string()],
            );
        }
        let mut reasons = opinion;
        reasons.extend(evidence);
        if s.agents_left == 0 {
            return Decision::stop(NextStep::Finish, StopReason::NoAgentLeft, reasons);
        }
        if !s.fits(1) {
            return Decision::stop(
                NextStep::Finish,
                StopReason::Budget("no call or time is left for a review".to_string()),
                reasons,
            );
        }
        Decision::go(NextStep::Review, reasons)
    }

    /// After the independent review.
    pub fn after_review(&self, s: &Signals) -> Decision {
        let verdict = s.review.unwrap_or(ReviewVerdict::Unknown);
        let contested = s.claims.contested();
        let synth_or_stop = |reasons: Vec<String>, why_stop: Option<StopReason>| {
            if s.fits(1) {
                Decision {
                    next: NextStep::Synthesize,
                    reasons,
                    stop: why_stop,
                }
            } else {
                Decision::stop(
                    NextStep::Finish,
                    StopReason::Budget(
                        "no call or time is left to integrate the review".to_string(),
                    ),
                    reasons,
                )
            }
        };
        match verdict {
            ReviewVerdict::Agree if contested == 0 => Decision::stop(
                NextStep::Finish,
                StopReason::ReviewClean,
                vec!["the reviewer found nothing to change and no evidence contradicts the answer".to_string()],
            ),
            ReviewVerdict::Agree => synth_or_stop(
                vec![format!(
                    "the reviewer agrees, but the evidence contradicts {contested} claim(s), so the answer is rewritten around them"
                )],
                None,
            ),
            ReviewVerdict::MinorIssues => synth_or_stop(
                vec!["the reviewer found points worth integrating".to_string()],
                None,
            ),
            ReviewVerdict::Unknown => synth_or_stop(
                vec!["the review gave no machine-readable verdict, so its points are integrated rather than ignored".to_string()],
                None,
            ),
            ReviewVerdict::MaterialDisagreement => {
                let mut reasons = vec!["the reviewer disagrees materially with the answer".to_string()];
                if s.escalations_done >= 1 {
                    return synth_or_stop(
                        reasons,
                        Some(StopReason::EscalationLimit(
                            "one escalation was already used; the remaining disagreement is reported, not argued further".to_string(),
                        )),
                    );
                }
                if s.agents_left > 0 && s.fits(2) {
                    reasons.push("a third, independent perspective helps tell whose view holds".to_string());
                    return Decision::go(NextStep::IndependentPerspective, reasons);
                }
                if s.agents_left == 0 && s.rounds_done < s.max_rounds && s.fits(2) {
                    reasons.push("no further agent is available, so the lead agent reconsiders after the review".to_string());
                    return Decision::go(NextStep::Reconsider, reasons);
                }
                let stop = if s.agents_left == 0 {
                    StopReason::NoAgentLeft
                } else {
                    StopReason::Budget(
                        "not enough calls or time left for a further perspective and the synthesis"
                            .to_string(),
                    )
                };
                synth_or_stop(reasons, Some(stop))
            }
        }
    }

    /// After an escalation (a further perspective or a reconsideration): always synthesize, once.
    pub fn after_escalation(&self, s: &Signals) -> Decision {
        let reasons = vec![
            "the bounded escalation is complete; everything gathered is integrated".to_string(),
        ];
        if s.fits(1) {
            Decision::stop(
                NextStep::Synthesize,
                StopReason::EscalationLimit("one escalation is the maximum".to_string()),
                reasons,
            )
        } else {
            Decision::stop(
                NextStep::Finish,
                StopReason::Budget("no call or time is left for the synthesis".to_string()),
                reasons,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::research::quality::ClaimCategory;

    fn base() -> Signals {
        Signals {
            mode: ConsultMode::Learn,
            contested_question: false,
            complex_question: false,
            high_stakes: false,
            research_active: false,
            retrieved_sources: 0,
            claims: ClaimTally::default(),
            verification_failed: false,
            review: None,
            agents_left: 2,
            rounds_done: 0,
            max_rounds: 2,
            escalations_done: 0,
            calls_left: None,
            time_left: None,
        }
    }

    fn claim(status: ClaimStatus, external: bool) -> ClaimRecord {
        ClaimRecord {
            id: 1,
            text: "x".into(),
            category: ClaimCategory::Other,
            originating_agents: vec![],
            needs_external_evidence: external,
            cited_by_agents: vec![],
            status,
            evidence: vec![],
            supporting_independent_sources: 0,
            contradicting_independent_sources: 0,
            notes: vec![],
        }
    }

    const C: AdaptiveDeliberationController = AdaptiveDeliberationController;

    #[test]
    fn verdicts_are_read_from_the_last_verdict_line_and_the_template_is_not_a_verdict() {
        let cases = [
            ("notes\nVERDICT: AGREE", ReviewVerdict::Agree),
            ("**Verdict:** MINOR_ISSUES", ReviewVerdict::MinorIssues),
            (
                "- verdict: material disagreement.",
                ReviewVerdict::MaterialDisagreement,
            ),
            (
                "VERDICT: AGREE\nlater thoughts\nVERDICT: MATERIAL_DISAGREEMENT",
                ReviewVerdict::MaterialDisagreement,
            ),
            (
                "VERDICT: AGREE | MINOR_ISSUES | MATERIAL_DISAGREEMENT",
                ReviewVerdict::Unknown,
            ),
            ("VERDICT: unsure", ReviewVerdict::Unknown),
            ("no verdict at all", ReviewVerdict::Unknown),
            ("", ReviewVerdict::Unknown),
        ];
        for (text, expected) in cases {
            assert_eq!(parse_verdict(text), expected, "{text:?}");
        }
    }

    #[test]
    fn claims_are_tallied_from_evidence_records() {
        let t = ClaimTally::from_claims(&[
            claim(ClaimStatus::Supported, true),
            claim(ClaimStatus::Contradicted, true),
            claim(ClaimStatus::MixedEvidence, true),
            claim(ClaimStatus::InsufficientEvidence, true),
            claim(ClaimStatus::InsufficientEvidence, false),
            claim(ClaimStatus::NotChecked, true),
        ]);
        assert_eq!(
            (
                t.total,
                t.supported,
                t.contested(),
                t.insufficient,
                t.unsupported_external,
                t.not_checked
            ),
            (6, 1, 2, 2, 1, 1)
        );
        assert!(!t.all_supported());
        assert!(ClaimTally::from_claims(&[claim(ClaimStatus::Supported, true)]).all_supported());
        assert!(
            !ClaimTally::default().all_supported(),
            "no claims is not 'all supported'"
        );
    }

    #[test]
    fn a_simple_uncontested_question_stops_after_the_first_answer_and_says_why() {
        let d = C.after_initial(&base());
        assert_eq!(d.next, NextStep::Finish);
        assert!(matches!(d.stop, Some(StopReason::Sufficient(_))));
        assert!(!d.reasons.is_empty());
    }

    #[test]
    fn decision_and_review_questions_get_one_independent_review() {
        for mode in [ConsultMode::Decision, ConsultMode::Review] {
            let d = C.after_initial(&Signals { mode, ..base() });
            assert_eq!(d.next, NextStep::Review, "{mode}");
            assert!(d.reasons[0].contains("independent check"));
        }
    }

    #[test]
    fn contested_complex_or_high_stakes_questions_are_reviewed_even_when_teaching() {
        for s in [
            Signals {
                contested_question: true,
                ..base()
            },
            Signals {
                complex_question: true,
                ..base()
            },
            Signals {
                high_stakes: true,
                ..base()
            },
        ] {
            assert_eq!(C.after_initial(&s).next, NextStep::Review);
        }
    }

    #[test]
    fn contradicted_or_unsupported_claims_trigger_a_review_but_supported_ones_do_not() {
        let research = Signals {
            mode: ConsultMode::Research,
            research_active: true,
            retrieved_sources: 3,
            ..base()
        };
        let supported = Signals {
            claims: ClaimTally::from_claims(&[claim(ClaimStatus::Supported, true)]),
            ..research.clone()
        };
        let d = C.after_initial(&supported);
        assert_eq!(d.next, NextStep::Finish);
        assert!(d.reasons[0].contains("supported by retrieved sources"));
        let contradicted = Signals {
            claims: ClaimTally::from_claims(&[claim(ClaimStatus::Contradicted, true)]),
            ..research.clone()
        };
        assert_eq!(C.after_initial(&contradicted).next, NextStep::Review);
        let unsupported = Signals {
            claims: ClaimTally::from_claims(&[claim(ClaimStatus::InsufficientEvidence, true)]),
            ..research.clone()
        };
        assert_eq!(C.after_initial(&unsupported).next, NextStep::Review);
        let failed = Signals {
            verification_failed: true,
            ..research
        };
        assert_eq!(C.after_initial(&failed).next, NextStep::Review);
    }

    #[test]
    fn missing_evidence_alone_never_forces_more_agents() {
        // Nothing retrieved: another agent cannot supply evidence, so the controller stops and says so.
        let s = Signals {
            mode: ConsultMode::Research,
            research_active: true,
            retrieved_sources: 0,
            verification_failed: true,
            ..base()
        };
        let d = C.after_initial(&s);
        assert_eq!(d.next, NextStep::Finish);
        assert!(matches!(d.stop, Some(StopReason::EvidenceCannotImprove(_))));
    }

    #[test]
    fn no_other_agent_or_no_budget_stops_before_the_review() {
        let wants_review = Signals {
            mode: ConsultMode::Decision,
            ..base()
        };
        let d = C.after_initial(&Signals {
            agents_left: 0,
            ..wants_review.clone()
        });
        assert_eq!(
            (d.next, d.stop),
            (NextStep::Finish, Some(StopReason::NoAgentLeft))
        );
        let d = C.after_initial(&Signals {
            calls_left: Some(0),
            ..wants_review.clone()
        });
        assert!(matches!(d.stop, Some(StopReason::Budget(_))));
        let d = C.after_initial(&Signals {
            time_left: Some(Duration::ZERO),
            ..wants_review
        });
        assert!(matches!(d.stop, Some(StopReason::Budget(_))));
    }

    #[test]
    fn a_review_that_agrees_ends_the_consultation_unless_the_evidence_disagrees() {
        let reviewed = |verdict, claims: ClaimTally| Signals {
            review: Some(verdict),
            claims,
            ..base()
        };
        let d = C.after_review(&reviewed(ReviewVerdict::Agree, ClaimTally::default()));
        assert_eq!(
            (d.next, d.stop),
            (NextStep::Finish, Some(StopReason::ReviewClean))
        );
        let contradicted = ClaimTally::from_claims(&[claim(ClaimStatus::MixedEvidence, true)]);
        let d = C.after_review(&reviewed(ReviewVerdict::Agree, contradicted));
        assert_eq!(
            d.next,
            NextStep::Synthesize,
            "agreement does not settle contradicting evidence"
        );
    }

    #[test]
    fn minor_issues_and_missing_verdicts_are_integrated_by_a_synthesis() {
        for v in [ReviewVerdict::MinorIssues, ReviewVerdict::Unknown] {
            let d = C.after_review(&Signals {
                review: Some(v),
                ..base()
            });
            assert_eq!(d.next, NextStep::Synthesize);
            assert!(d.stop.is_none());
        }
        // A review that never produced a verdict at all reads as unknown.
        assert_eq!(C.after_review(&base()).next, NextStep::Synthesize);
    }

    #[test]
    fn a_material_disagreement_escalates_once_to_a_third_perspective() {
        let s = Signals {
            review: Some(ReviewVerdict::MaterialDisagreement),
            ..base()
        };
        let d = C.after_review(&s);
        assert_eq!(d.next, NextStep::IndependentPerspective);
        assert!(
            d.reasons
                .iter()
                .any(|r| r.contains("third, independent perspective"))
        );
        // Already escalated: synthesize and report the remainder.
        let d = C.after_review(&Signals {
            escalations_done: 1,
            ..s.clone()
        });
        assert_eq!(d.next, NextStep::Synthesize);
        assert!(matches!(d.stop, Some(StopReason::EscalationLimit(_))));
        // The escalation is followed by a synthesis, and nothing else.
        let d = C.after_escalation(&Signals {
            escalations_done: 1,
            ..s
        });
        assert_eq!(d.next, NextStep::Synthesize);
    }

    #[test]
    fn without_a_third_agent_the_lead_reconsiders_if_rounds_allow() {
        let s = Signals {
            review: Some(ReviewVerdict::MaterialDisagreement),
            agents_left: 0,
            ..base()
        };
        assert_eq!(C.after_review(&s).next, NextStep::Reconsider);
        let d = C.after_review(&Signals { max_rounds: 0, ..s });
        assert_eq!(d.next, NextStep::Synthesize);
        assert_eq!(d.stop, Some(StopReason::NoAgentLeft));
    }

    #[test]
    fn escalation_needs_two_calls_and_a_synthesis_is_dropped_when_none_is_left() {
        let s = Signals {
            review: Some(ReviewVerdict::MaterialDisagreement),
            ..base()
        };
        let one_call = C.after_review(&Signals {
            calls_left: Some(1),
            ..s.clone()
        });
        assert_eq!(
            one_call.next,
            NextStep::Synthesize,
            "no room for a third perspective and a synthesis"
        );
        assert!(matches!(one_call.stop, Some(StopReason::Budget(_))));
        let none = C.after_review(&Signals {
            calls_left: Some(0),
            ..s.clone()
        });
        assert_eq!(none.next, NextStep::Finish);
        assert!(matches!(none.stop, Some(StopReason::Budget(_))));
        let after = C.after_escalation(&Signals {
            calls_left: Some(0),
            ..s
        });
        assert_eq!(after.next, NextStep::Finish);
    }

    #[test]
    fn the_same_signals_always_give_the_same_decision() {
        let s = Signals {
            mode: ConsultMode::Decision,
            high_stakes: true,
            ..base()
        };
        let first = C.after_initial(&s);
        for _ in 0..20 {
            assert_eq!(C.after_initial(&s), first);
        }
    }
}
