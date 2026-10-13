//! Quality rubrics: structured criteria per consultation type, scored only where that can be done
//! honestly.
//!
//! A criterion is judged one of two ways. **Deterministic** criteria are decided by checks the
//! dataset defines (required facts present, forbidden claims absent, constraints respected, required
//! claims cited, citations structurally valid). **Judgement** criteria (fairness of a comparison,
//! clarity, usefulness, whether a citation really supports a sentence) cannot be settled by a
//! program: they come out `NotAssessed` and are meant for people, through the blind comparison.
//! A criterion that does not apply to a response is `NotApplicable`; nothing is scored 0 or 5 to fill
//! a cell. There is no total: a rubric result is a set of judgments, not a probability that the
//! answer is true.

use serde::{Deserialize, Serialize};

use super::citations::{self, CitationReport, ClaimEvidence};
use super::dataset::{Case, Check, RecordedResponse};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    /// Decided by checks the dataset defines.
    Deterministic,
    /// Needs a person (or a clearly labelled AI-assisted judgment).
    Judgement,
}

pub struct CriterionDef {
    pub id: &'static str,
    /// `general` or the consultation mode it belongs to.
    pub group: &'static str,
    pub description: &'static str,
    pub method: Method,
}

const fn c(
    id: &'static str,
    group: &'static str,
    method: Method,
    description: &'static str,
) -> CriterionDef {
    CriterionDef {
        id,
        group,
        description,
        method,
    }
}

use Method::{Deterministic as D, Judgement as J};

pub const CRITERIA: &[CriterionDef] = &[
    c(
        "factual_correctness",
        "general",
        D,
        "required facts present and no forbidden claim made (objective cases only)",
    ),
    c(
        "evidence_quality",
        "general",
        D,
        "claims the case requires to be evidenced are made and cite the supplied evidence (coverage only, not support)",
    ),
    c(
        "citation_accuracy",
        "general",
        D,
        "citations are structurally valid: known retrieved sources, no unlisted links, no unbacked verification language",
    ),
    c(
        "instruction_adherence",
        "general",
        D,
        "the constraints stated by the question are respected",
    ),
    c(
        "completeness",
        "general",
        D,
        "every required element is present",
    ),
    c(
        "logical_consistency",
        "general",
        J,
        "the answer does not contradict itself and its conclusion follows",
    ),
    c(
        "uncertainty_calibration",
        "general",
        D,
        "when the right answer is uncertainty, the answer says so instead of asserting",
    ),
    c(
        "decision_usefulness",
        "general",
        J,
        "a person could act on the answer",
    ),
    c("clarity", "general", J, "organised and understandable"),
    c(
        "alternatives_compared_fairly",
        "decision",
        J,
        "alternatives are compared on the same criteria",
    ),
    c(
        "user_priorities_respected",
        "decision",
        D,
        "the constraints and priorities the user stated are respected",
    ),
    c(
        "costs_and_risks",
        "decision",
        J,
        "important costs and risks are considered",
    ),
    c(
        "recommendation_follows_evidence",
        "decision",
        J,
        "the recommendation follows from the evidence given",
    ),
    c(
        "missing_information_acknowledged",
        "decision",
        J,
        "what is missing or unknown is said",
    ),
    c(
        "factual_correctness_checkable",
        "research",
        D,
        "independently checkable facts are correct",
    ),
    c(
        "source_provenance",
        "research",
        D,
        "every source has a link, a publication date and a retrieval date recorded",
    ),
    c(
        "citation_support",
        "research",
        J,
        "the cited passage really says what the sentence says (lexical overlap is only a flag)",
    ),
    c(
        "contradiction_handling",
        "research",
        J,
        "conflicting evidence is shown and weighed, not hidden",
    ),
    c(
        "freshness",
        "research",
        J,
        "dates are given and old information is not presented as current",
    ),
    c(
        "explicit_uncertainty",
        "research",
        J,
        "limits and unknowns are stated",
    ),
    c(
        "comparison_criteria",
        "review",
        J,
        "the criteria used to compare are stated",
    ),
    c(
        "verified_specifications",
        "review",
        D,
        "specifications the case requires are stated and cited to the supplied evidence",
    ),
    c(
        "relevant_alternatives",
        "review",
        J,
        "relevant alternatives are named",
    ),
    c(
        "advantages_and_disadvantages",
        "review",
        J,
        "both sides are given",
    ),
    c(
        "recommendation_value",
        "review",
        J,
        "the recommendation is justified by value",
    ),
    c(
        "conceptual_accuracy",
        "learn",
        D,
        "required concepts are stated correctly and no forbidden misconception is asserted",
    ),
    c(
        "clear_progression",
        "learn",
        J,
        "ideas build in a sensible order",
    ),
    c(
        "appropriate_difficulty",
        "learn",
        J,
        "suited to the stated level",
    ),
    c(
        "examples_and_analogies",
        "learn",
        J,
        "examples or analogies help",
    ),
    c(
        "misconceptions_identified",
        "learn",
        D,
        "the common misunderstandings the case lists are addressed",
    ),
    c(
        "independence_of_perspectives",
        "debate",
        J,
        "initial perspectives were formed independently",
    ),
    c(
        "constructive_criticism",
        "debate",
        J,
        "reviews are specific and useful",
    ),
    c(
        "disagreement_handling",
        "debate",
        J,
        "disagreements are kept and explained, not smoothed over",
    ),
    c(
        "correct_evidence_use",
        "debate",
        D,
        "evidence is used as the case's checks require",
    ),
    c(
        "synthesis_quality",
        "debate",
        J,
        "the final synthesis is faithful to the discussion",
    ),
];

pub fn is_criterion(id: &str) -> bool {
    CRITERIA.iter().any(|d| d.id == id)
}

pub fn def(id: &str) -> Option<&'static CriterionDef> {
    CRITERIA.iter().find(|d| d.id == id)
}

/// The rubric of a consultation type: the general criteria plus those of its mode.
pub fn rubric_for(mode: Option<&str>) -> Vec<&'static CriterionDef> {
    CRITERIA
        .iter()
        .filter(|d| d.group == "general" || Some(d.group) == mode)
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Pass,
    Fail,
    /// The criterion does not apply to this response.
    NotApplicable,
    /// It needs a person; no automatic judgment was made.
    NotAssessed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CriterionResult {
    pub id: String,
    pub outcome: Outcome,
    pub method: Method,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckKind {
    Required,
    Forbidden,
    Constraint,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckResult {
    pub id: String,
    pub kind: CheckKind,
    pub description: String,
    /// `true` when the check came out as the case wants (required present; forbidden absent).
    pub ok: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Evaluation {
    pub checks: Vec<CheckResult>,
    pub citation: Option<CitationReport>,
    pub criteria: Vec<CriterionResult>,
    /// For objective cases: did the response pass every factual check. `None` when not objective.
    pub objective_correct: Option<bool>,
    pub answer_words: usize,
}

fn present(answer_lower: &str, c: &Check) -> bool {
    c.any_of
        .iter()
        .any(|s| !s.trim().is_empty() && answer_lower.contains(&s.to_lowercase()))
}

const UNCERTAINTY: &[&str] = &[
    "cannot be determined",
    "can't be determined",
    "cannot determine",
    "cannot know",
    "no way to know",
    "not established",
    "insufficient",
    "uncertain",
    "unknown",
    "not possible to",
    "no evidence",
    "not enough",
    "cannot be known",
    "no se puede",
    "no es posible determinar",
    "incierto",
    "no hay evidencia",
    "no está establecido",
    "no reliable way",
    "would be a guess",
    "any figure would",
    "not stated",
    "does not state",
    "doesn't state",
    "not specified",
    "does not specify",
    "doesn't specify",
    "not provided",
    "not given",
    "not listed",
    "no information",
];

fn expresses_uncertainty(answer_lower: &str) -> bool {
    UNCERTAINTY.iter().any(|m| answer_lower.contains(m))
}

pub fn words(text: &str) -> usize {
    text.split_whitespace().count()
}

/// Evaluates one response against one case. Deterministic: the same inputs give the same result.
pub fn evaluate(case: &Case, resp: &RecordedResponse) -> Evaluation {
    let lower = resp.answer.to_lowercase();
    let mut checks = Vec::new();
    for ck in &case.expect.required {
        checks.push(CheckResult {
            id: ck.id.clone(),
            kind: CheckKind::Required,
            description: ck.description.clone(),
            ok: present(&lower, ck),
        });
    }
    for ck in &case.expect.forbidden {
        checks.push(CheckResult {
            id: ck.id.clone(),
            kind: CheckKind::Forbidden,
            description: ck.description.clone(),
            ok: !present(&lower, ck),
        });
    }
    for ck in &case.expect.constraints {
        checks.push(CheckResult {
            id: ck.id.clone(),
            kind: CheckKind::Constraint,
            description: ck.description.clone(),
            ok: present(&lower, ck),
        });
    }
    let kind_ok = |k: CheckKind| checks.iter().filter(|c| c.kind == k).all(|c| c.ok);
    let kind_any = |k: CheckKind| checks.iter().any(|c| c.kind == k);
    let failed = |k: CheckKind| {
        checks
            .iter()
            .filter(|c| c.kind == k && !c.ok)
            .map(|c| c.id.clone())
            .collect::<Vec<_>>()
            .join(", ")
    };

    let wants_citations =
        case.expect.must_cite || !resp.sources.is_empty() || resp.answer.contains('[');
    let citation = wants_citations
        .then(|| citations::check(&resp.answer, &resp.sources, &case.expect.required_claims));

    let criteria_ids: Vec<&str> = if case.criteria.is_empty() {
        rubric_for(case.mode.as_deref())
            .iter()
            .map(|d| d.id)
            .collect()
    } else {
        case.criteria.iter().map(String::as_str).collect()
    };

    let mut criteria = Vec::new();
    for id in criteria_ids {
        let Some(d) = def(id) else { continue };
        let (outcome, detail) = match id {
            "factual_correctness" | "factual_correctness_checkable" | "conceptual_accuracy" => {
                if !case.objective {
                    (Outcome::NotApplicable, "not an objective case".to_string())
                } else if !kind_any(CheckKind::Required) && !kind_any(CheckKind::Forbidden) {
                    (
                        Outcome::NotApplicable,
                        "the case defines no factual checks".to_string(),
                    )
                } else if kind_ok(CheckKind::Required) && kind_ok(CheckKind::Forbidden) {
                    (
                        Outcome::Pass,
                        "every required fact is present and no forbidden claim is made".to_string(),
                    )
                } else {
                    (
                        Outcome::Fail,
                        format!(
                            "missing: [{}]; forbidden claim(s) made: [{}]",
                            failed(CheckKind::Required),
                            failed(CheckKind::Forbidden)
                        ),
                    )
                }
            }
            "completeness" => {
                if !kind_any(CheckKind::Required) && !kind_any(CheckKind::Constraint) {
                    (
                        Outcome::NotApplicable,
                        "the case defines no required elements".to_string(),
                    )
                } else if kind_ok(CheckKind::Required) && kind_ok(CheckKind::Constraint) {
                    (
                        Outcome::Pass,
                        "every required element is present".to_string(),
                    )
                } else {
                    (
                        Outcome::Fail,
                        format!(
                            "missing: [{}, {}]",
                            failed(CheckKind::Required),
                            failed(CheckKind::Constraint)
                        ),
                    )
                }
            }
            "instruction_adherence" | "user_priorities_respected" => {
                if !kind_any(CheckKind::Constraint) {
                    (
                        Outcome::NotApplicable,
                        "the question states no checkable constraint".to_string(),
                    )
                } else if kind_ok(CheckKind::Constraint) {
                    (
                        Outcome::Pass,
                        "every stated constraint is respected".to_string(),
                    )
                } else {
                    (
                        Outcome::Fail,
                        format!(
                            "constraint(s) not respected: [{}]",
                            failed(CheckKind::Constraint)
                        ),
                    )
                }
            }
            "uncertainty_calibration" => {
                if !case.expect.expects_uncertainty {
                    (
                        Outcome::NotApplicable,
                        "the case expects a definite answer".to_string(),
                    )
                } else if expresses_uncertainty(&lower) && kind_ok(CheckKind::Forbidden) {
                    (
                        Outcome::Pass,
                        "says it cannot be established and makes no forbidden claim".to_string(),
                    )
                } else if !expresses_uncertainty(&lower) {
                    (
                        Outcome::Fail,
                        "asserts an answer where none can be established".to_string(),
                    )
                } else {
                    (
                        Outcome::Fail,
                        format!(
                            "hedges but also makes forbidden claim(s): [{}]",
                            failed(CheckKind::Forbidden)
                        ),
                    )
                }
            }
            "citation_accuracy" => match &citation {
                None => (
                    Outcome::NotApplicable,
                    "no citations and none required".to_string(),
                ),
                Some(r) => {
                    let short = case.expect.min_citations.saturating_sub(r.checks.len());
                    if r.mechanical_problems() == 0 && short == 0 {
                        (
                            Outcome::Pass,
                            format!("{} citation(s), all structurally valid", r.checks.len()),
                        )
                    } else {
                        (
                            Outcome::Fail,
                            format!(
                                "unknown ids {:?}, unretrieved {:?}, unlisted links {}, unbacked verification wording {:?}, {} short of the {} required",
                                r.unknown_ids,
                                r.not_retrieved,
                                r.unlisted_urls.len(),
                                r.unsupported_labels,
                                short,
                                case.expect.min_citations
                            ),
                        )
                    }
                }
            },
            "evidence_quality" | "verified_specifications" | "correct_evidence_use" => {
                match &citation {
                    Some(r) if !r.required_claims.is_empty() => {
                        let bad: Vec<&str> = r
                            .required_claims
                            .iter()
                            .filter(|x| x.result != ClaimEvidence::MadeAndCited)
                            .map(|x| x.description.as_str())
                            .collect();
                        if bad.is_empty() {
                            (Outcome::Pass, "every required claim is made and cites the supplied evidence (coverage; not a judgement of support)".to_string())
                        } else {
                            (
                                Outcome::Fail,
                                format!("required claim(s) absent or uncited: {bad:?}"),
                            )
                        }
                    }
                    _ => (
                        Outcome::NotApplicable,
                        "the case requires no evidenced claim".to_string(),
                    ),
                }
            }
            "source_provenance" => match &citation {
                Some(r) if r.sources_total > 0 => {
                    if r.sources_missing_dates == 0 {
                        (
                            Outcome::Pass,
                            "every source records its publication and retrieval dates".to_string(),
                        )
                    } else {
                        (
                            Outcome::Fail,
                            format!(
                                "{} source(s) lack a publication or retrieval date",
                                r.sources_missing_dates
                            ),
                        )
                    }
                }
                _ => (
                    Outcome::NotApplicable,
                    "the response lists no sources".to_string(),
                ),
            },
            "misconceptions_identified" => {
                if kind_any(CheckKind::Required) {
                    let named: Vec<_> = checks
                        .iter()
                        .filter(|c| c.kind == CheckKind::Required && !c.ok)
                        .collect();
                    if named.is_empty() {
                        (
                            Outcome::Pass,
                            "the listed misunderstandings are addressed".to_string(),
                        )
                    } else {
                        (
                            Outcome::Fail,
                            format!(
                                "not addressed: {}",
                                named
                                    .iter()
                                    .map(|c| c.id.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ),
                        )
                    }
                } else {
                    (Outcome::NotApplicable, "the case lists none".to_string())
                }
            }
            _ if d.method == Method::Judgement => (
                Outcome::NotAssessed,
                "needs a person: use the blind comparison".to_string(),
            ),
            _ => (
                Outcome::NotAssessed,
                "no automatic check is defined for this criterion".to_string(),
            ),
        };
        criteria.push(CriterionResult {
            id: id.to_string(),
            outcome,
            method: d.method,
            detail,
        });
    }

    let objective_correct = if case.objective {
        criteria
            .iter()
            .find(|c| {
                c.id == "factual_correctness"
                    || c.id == "factual_correctness_checkable"
                    || c.id == "conceptual_accuracy"
            })
            .map(|c| c.outcome == Outcome::Pass)
            .or_else(|| {
                (kind_any(CheckKind::Required) || kind_any(CheckKind::Forbidden))
                    .then(|| kind_ok(CheckKind::Required) && kind_ok(CheckKind::Forbidden))
            })
    } else {
        None
    };
    Evaluation {
        checks,
        citation,
        criteria,
        objective_correct,
        answer_words: words(&resp.answer),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::dataset::{Arm, Case, Category, Expectations, Provenance};

    fn case(expect: Expectations, objective: bool) -> Case {
        Case {
            id: "c".into(),
            category: Category::SimpleFact,
            mode: None,
            question: "q".into(),
            instructions: "i".into(),
            objective,
            reference_basis: "arithmetic".into(),
            web: false,
            history: vec![],
            expect,
            evidence: vec![],
            criteria: vec![],
            recorded: vec![],
        }
    }

    fn resp(answer: &str) -> RecordedResponse {
        RecordedResponse {
            arm: Arm("fast".into()),
            provenance: Provenance::Fixture,
            answer: answer.into(),
            status: "complete".into(),
            latency_ms: Some(1000),
            invocations: Some(1),
            agents: vec![],
            context_chars: None,
            input_tokens: None,
            sources: vec![],
        }
    }

    fn ck(id: &str, any: &[&str]) -> Check {
        Check {
            id: id.into(),
            description: id.into(),
            any_of: any.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn outcome(e: &Evaluation, id: &str) -> Outcome {
        e.criteria
            .iter()
            .find(|c| c.id == id)
            .unwrap_or_else(|| panic!("no {id}"))
            .outcome
    }

    #[test]
    fn every_criterion_is_defined_once_and_each_mode_has_a_rubric() {
        let mut ids: Vec<&str> = CRITERIA.iter().map(|d| d.id).collect();
        ids.sort_unstable();
        let n = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), n);
        for mode in ["decision", "research", "review", "learn", "debate"] {
            assert!(
                rubric_for(Some(mode)).iter().any(|d| d.group == mode),
                "{mode}"
            );
        }
        assert!(rubric_for(None).iter().all(|d| d.group == "general"));
    }

    #[test]
    fn objective_correctness_needs_every_fact_and_no_forbidden_claim() {
        let expect = Expectations {
            required: vec![ck("amps", &["3 A", "3 amps"])],
            forbidden: vec![ck("wrong", &["12 A"])],
            ..Default::default()
        };
        let c = case(expect, true);
        let ok = evaluate(&c, &resp("The current is 3 A."));
        assert_eq!(outcome(&ok, "factual_correctness"), Outcome::Pass);
        assert_eq!(ok.objective_correct, Some(true));
        let missing = evaluate(&c, &resp("About twelve."));
        assert_eq!(outcome(&missing, "factual_correctness"), Outcome::Fail);
        assert_eq!(missing.objective_correct, Some(false));
        let forbidden = evaluate(&c, &resp("3 A, or maybe 12 A."));
        assert_eq!(outcome(&forbidden, "factual_correctness"), Outcome::Fail);
        assert!(
            forbidden
                .criteria
                .iter()
                .find(|c| c.id == "factual_correctness")
                .unwrap()
                .detail
                .contains("wrong")
        );
    }

    #[test]
    fn judgement_criteria_are_never_scored_by_the_program() {
        let c = case(Expectations::default(), false);
        let e = evaluate(&c, &resp("A fine answer."));
        for id in ["clarity", "decision_usefulness", "logical_consistency"] {
            assert_eq!(outcome(&e, id), Outcome::NotAssessed, "{id}");
        }
        assert_eq!(e.objective_correct, None);
        assert_eq!(outcome(&e, "factual_correctness"), Outcome::NotApplicable);
        assert_eq!(
            outcome(&e, "uncertainty_calibration"),
            Outcome::NotApplicable
        );
    }

    #[test]
    fn when_the_right_answer_is_uncertainty_asserting_one_fails() {
        let expect = Expectations {
            expects_uncertainty: true,
            forbidden: vec![ck("date", &["will be released on"])],
            ..Default::default()
        };
        let c = case(expect, true);
        assert_eq!(
            outcome(
                &evaluate(&c, &resp("It will be released on 5 May.")),
                "uncertainty_calibration"
            ),
            Outcome::Fail
        );
        assert_eq!(
            outcome(
                &evaluate(&c, &resp("That cannot be determined from what is public.")),
                "uncertainty_calibration"
            ),
            Outcome::Pass
        );
        assert_eq!(
            outcome(
                &evaluate(&c, &resp("No se puede saber todavía.")),
                "uncertainty_calibration"
            ),
            Outcome::Pass
        );
        assert_eq!(
            outcome(
                &evaluate(&c, &resp("Unknown, but it will be released on 5 May.")),
                "uncertainty_calibration"
            ),
            Outcome::Fail
        );
    }

    #[test]
    fn constraints_and_completeness_are_separate_from_correctness() {
        let expect = Expectations {
            constraints: vec![ck("budget", &["8000 MXN", "$8,000"])],
            required: vec![ck("winner", &["Siglent"])],
            ..Default::default()
        };
        let c = case(expect, false);
        let e = evaluate(&c, &resp("Within 8000 MXN the Siglent is best."));
        assert_eq!(outcome(&e, "instruction_adherence"), Outcome::Pass);
        assert_eq!(outcome(&e, "completeness"), Outcome::Pass);
        let e = evaluate(&c, &resp("The Siglent is best at any price."));
        assert_eq!(outcome(&e, "instruction_adherence"), Outcome::Fail);
        assert_eq!(outcome(&e, "completeness"), Outcome::Fail);
    }

    #[test]
    fn evaluation_is_deterministic() {
        let c = case(
            Expectations {
                required: vec![ck("a", &["x"])],
                ..Default::default()
            },
            true,
        );
        let r = resp("x");
        assert_eq!(evaluate(&c, &r), evaluate(&c, &r));
    }
}
