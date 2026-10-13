//! The evaluation dataset format.
//!
//! A dataset is one JSON file of cases. A case carries its own question, the instructions for whoever
//! judges it, the constraints an answer must respect, and - for cases that can be checked - the
//! reference evidence that makes it checkable. **Ground truth is never invented**: an objective case
//! must say where its reference comes from (a supplied source passage, or a fact that follows from the
//! question itself, such as arithmetic), and a case without one is a subjective scenario that people
//! judge. Datasets are versioned and carry their provenance; private conversations are never turned
//! into cases automatically.
//!
//! Cases may include *recorded responses*: answers per arm (a single agent, or a strategy) used by the
//! offline evaluator. Every recorded response says where it came from (`fixture` written by hand to
//! exercise the engine, `mock`, `recorded_live`, `live`) and reports never merge those classes.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{OrchestraError, Result};

pub const FORMAT: &str = "ai-orchestra-eval-dataset";
pub const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    SimpleFact,
    MultiStepReasoning,
    TechnicalExplanation,
    ScientificClaim,
    ProductComparison,
    PersonalDecision,
    MultiSourceResearch,
    ConflictingEvidence,
    UncertaintyExpected,
    MisleadingInformation,
    FollowUpMemory,
    FewerCallsSufficient,
}

impl Category {
    pub const ALL: [Category; 12] = [
        Category::SimpleFact,
        Category::MultiStepReasoning,
        Category::TechnicalExplanation,
        Category::ScientificClaim,
        Category::ProductComparison,
        Category::PersonalDecision,
        Category::MultiSourceResearch,
        Category::ConflictingEvidence,
        Category::UncertaintyExpected,
        Category::MisleadingInformation,
        Category::FollowUpMemory,
        Category::FewerCallsSufficient,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Category::SimpleFact => "simple_fact",
            Category::MultiStepReasoning => "multi_step_reasoning",
            Category::TechnicalExplanation => "technical_explanation",
            Category::ScientificClaim => "scientific_claim",
            Category::ProductComparison => "product_comparison",
            Category::PersonalDecision => "personal_decision",
            Category::MultiSourceResearch => "multi_source_research",
            Category::ConflictingEvidence => "conflicting_evidence",
            Category::UncertaintyExpected => "uncertainty_expected",
            Category::MisleadingInformation => "misleading_information",
            Category::FollowUpMemory => "follow_up_memory",
            Category::FewerCallsSufficient => "fewer_calls_sufficient",
        }
    }
}

/// Where a recorded response came from. Results of different classes are never merged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    /// Written by hand to exercise the evaluation. Says nothing about any model.
    Fixture,
    /// Produced by a scripted in-process agent.
    Mock,
    /// Real provider output recorded in an earlier, authorized run.
    RecordedLive,
    /// Produced now, by an authorized live run.
    Live,
}

impl Provenance {
    pub fn as_str(self) -> &'static str {
        match self {
            Provenance::Fixture => "fixture",
            Provenance::Mock => "mock",
            Provenance::RecordedLive => "recorded_live",
            Provenance::Live => "live",
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Provenance::Fixture => {
                "hand-written fixture responses: they exercise the evaluation and say nothing about any model"
            }
            Provenance::Mock => "scripted mock agents: a test of the machinery, not of any model",
            Provenance::RecordedLive => {
                "real provider output recorded in an earlier authorized run"
            }
            Provenance::Live => "real provider output from an authorized live run",
        }
    }
}

/// One way of answering: a single agent, or an orchestration strategy.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Arm(pub String);

impl Arm {
    pub const STRATEGIES: [&'static str; 4] = ["fast", "balanced", "thorough", "auto"];

    pub fn parse(s: &str) -> Result<Arm> {
        let s = s.trim().to_lowercase();
        let ok = Arm::STRATEGIES.contains(&s.as_str())
            || s.strip_prefix("single:")
                .is_some_and(|a| ["claude", "codex", "google"].contains(&a));
        if ok {
            Ok(Arm(s))
        } else {
            Err(OrchestraError::Config(format!(
                "`{s}` is not an arm (use single:claude, single:codex, single:google, fast, balanced, thorough or auto)"
            )))
        }
    }

    /// The agent of a single-agent arm.
    pub fn single_agent(&self) -> Option<&str> {
        self.0.strip_prefix("single:")
    }

    pub fn is_multi(&self) -> bool {
        self.single_agent().is_none()
    }
}

impl std::fmt::Display for Arm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A check passes when the answer contains any one of the alternatives (case-insensitive). A keyword
/// check is crude: it can miss a correct paraphrase, so alternatives should list the usual ones, and a
/// failing check is evidence to look at, not proof that the answer is wrong.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    pub id: String,
    pub description: String,
    pub any_of: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequiredClaim {
    /// The claim is made if the answer contains any of these.
    pub any_of: Vec<String>,
    /// And it must cite one of these source ids.
    pub source_ids: Vec<u32>,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Expectations {
    /// Essential facts (objective cases): each must be present.
    pub required: Vec<Check>,
    /// Statements a correct answer must not make (misleading premises, contradicted claims).
    pub forbidden: Vec<Check>,
    /// The right response is to say it cannot be known (or is not established).
    pub expects_uncertainty: bool,
    /// Constraints from the question that the answer must respect.
    pub constraints: Vec<Check>,
    pub must_cite: bool,
    pub min_citations: usize,
    pub required_claims: Vec<RequiredClaim>,
    /// A single agent should be enough: calls beyond one are cost, not quality.
    pub single_agent_sufficient: bool,
}

/// Reference evidence supplied by the dataset for a case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferenceSource {
    pub id: u32,
    pub title: String,
    pub url: String,
    pub published: Option<String>,
    pub passage: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Turn {
    pub question: String,
    pub answer: String,
}

/// What a response listed as its sources (from the research report of the run).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseSource {
    pub id: u32,
    pub url: String,
    /// The page was retrieved by the program (only retrieved pages count as evidence).
    pub retrieved: bool,
    pub published: Option<String>,
    pub retrieved_at: Option<String>,
    /// The passage the program holds for it (a preview of the extracted text).
    pub passage: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedResponse {
    pub arm: Arm,
    pub provenance: Provenance,
    pub answer: String,
    /// `complete`, `partial` or `failed`.
    pub status: String,
    pub latency_ms: Option<u64>,
    pub invocations: Option<usize>,
    /// The providers that took part (used to keep a model from judging its own answer).
    #[serde(default)]
    pub agents: Vec<String>,
    pub context_chars: Option<usize>,
    /// Input tokens as the CLIs reported them (exact), when they did.
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub sources: Vec<ResponseSource>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Case {
    pub id: String,
    pub category: Category,
    /// `decision`, `research`, `review`, `learn` or `debate`; absent means the router chooses.
    pub mode: Option<String>,
    pub question: String,
    /// What the person judging this case should look at.
    pub instructions: String,
    /// True when correctness can be checked mechanically against `expect` and `evidence`.
    pub objective: bool,
    /// Why the reference is trustworthy (a supplied passage, arithmetic, a defined rule).
    #[serde(default)]
    pub reference_basis: String,
    /// Whether the case is meant to be run with web research.
    #[serde(default)]
    pub web: bool,
    /// Earlier turns of the conversation (follow-up cases).
    #[serde(default)]
    pub history: Vec<Turn>,
    #[serde(default)]
    pub expect: Expectations,
    #[serde(default)]
    pub evidence: Vec<ReferenceSource>,
    /// The rubric criteria that apply; the others are not applicable and are not scored.
    #[serde(default)]
    pub criteria: Vec<String>,
    #[serde(default)]
    pub recorded: Vec<RecordedResponse>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatasetProvenance {
    pub authors: Vec<String>,
    pub created: String,
    pub description: String,
    /// The cases and fixtures were written for testing (no real-world claim is made by them).
    pub synthetic: bool,
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dataset {
    pub format: String,
    pub format_version: u32,
    pub id: String,
    pub title: String,
    pub dataset_version: String,
    pub provenance: DatasetProvenance,
    pub cases: Vec<Case>,
}

fn id_ok(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

impl Dataset {
    pub fn parse(text: &str) -> Result<Dataset> {
        let ds: Dataset = serde_json::from_str(text)
            .map_err(|e| OrchestraError::Config(format!("the dataset is not valid: {e}")))?;
        ds.validate()?;
        Ok(ds)
    }

    /// Every problem, not only the first.
    pub fn problems(&self) -> Vec<String> {
        let mut p = Vec::new();
        if self.format != FORMAT {
            p.push(format!("format must be `{FORMAT}`, got `{}`", self.format));
        }
        if self.format_version != FORMAT_VERSION {
            p.push(format!(
                "format_version {} is not supported (this program reads {FORMAT_VERSION})",
                self.format_version
            ));
        }
        if !id_ok(&self.id) {
            p.push(format!("dataset id `{}` is not a valid id", self.id));
        }
        if self.dataset_version.trim().is_empty() {
            p.push("dataset_version is empty".into());
        }
        if self.provenance.authors.is_empty() || self.provenance.description.trim().is_empty() {
            p.push("provenance needs authors and a description".into());
        }
        if self.cases.is_empty() {
            p.push("the dataset has no cases".into());
        }
        let mut seen = BTreeSet::new();
        for c in &self.cases {
            let at = format!("case `{}`", c.id);
            if !id_ok(&c.id) {
                p.push(format!("{at}: not a valid id"));
            }
            if !seen.insert(c.id.clone()) {
                p.push(format!("{at}: duplicate id"));
            }
            if c.question.trim().is_empty() {
                p.push(format!("{at}: empty question"));
            }
            if c.instructions.trim().is_empty() {
                p.push(format!("{at}: no evaluation instructions"));
            }
            if let Some(m) = &c.mode
                && !["decision", "research", "review", "learn", "debate"].contains(&m.as_str())
            {
                p.push(format!("{at}: unknown mode `{m}`"));
            }
            for k in &c.criteria {
                if !super::rubric::is_criterion(k) {
                    p.push(format!("{at}: unknown criterion `{k}`"));
                }
            }
            if c.objective {
                if c.reference_basis.trim().is_empty() {
                    p.push(format!("{at}: an objective case must say where its reference comes from (reference_basis)"));
                }
                if c.expect.required.is_empty()
                    && c.expect.forbidden.is_empty()
                    && !c.expect.expects_uncertainty
                {
                    p.push(format!("{at}: an objective case needs required or forbidden checks, or expects_uncertainty"));
                }
            }
            for ck in c
                .expect
                .required
                .iter()
                .chain(&c.expect.forbidden)
                .chain(&c.expect.constraints)
            {
                if ck.any_of.iter().all(|s| s.trim().is_empty()) {
                    p.push(format!("{at}: check `{}` has nothing to look for", ck.id));
                }
            }
            if c.expect.must_cite && c.evidence.is_empty() {
                p.push(format!("{at}: must_cite needs reference evidence"));
            }
            let mut ev_ids = BTreeSet::new();
            for e in &c.evidence {
                if !ev_ids.insert(e.id) {
                    p.push(format!("{at}: duplicate evidence id {}", e.id));
                }
                if e.passage.trim().is_empty() {
                    p.push(format!("{at}: evidence {} has no passage", e.id));
                }
            }
            for rc in &c.expect.required_claims {
                for id in &rc.source_ids {
                    if !ev_ids.contains(id) {
                        p.push(format!("{at}: a required claim names evidence {id}, which the case does not supply"));
                    }
                }
            }
            let mut arms = BTreeSet::new();
            for r in &c.recorded {
                if Arm::parse(&r.arm.0).is_err() {
                    p.push(format!(
                        "{at}: recorded response for unknown arm `{}`",
                        r.arm
                    ));
                }
                if !arms.insert((r.arm.clone(), r.provenance)) {
                    p.push(format!(
                        "{at}: two recorded responses for arm `{}` ({})",
                        r.arm,
                        r.provenance.as_str()
                    ));
                }
                if !["complete", "partial", "failed"].contains(&r.status.as_str()) {
                    p.push(format!(
                        "{at}: recorded status `{}` is not complete, partial or failed",
                        r.status
                    ));
                }
            }
        }
        p
    }

    pub fn validate(&self) -> Result<()> {
        let problems = self.problems();
        if problems.is_empty() {
            Ok(())
        } else {
            Err(OrchestraError::Config(format!(
                "the dataset has {} problem(s):\n  - {}",
                problems.len(),
                problems.join("\n  - ")
            )))
        }
    }

    pub fn case(&self, id: &str) -> Option<&Case> {
        self.cases.iter().find(|c| c.id == id)
    }

    /// A hash of the content, so a run says exactly which dataset it used.
    pub fn content_hash(&self) -> String {
        let canonical = serde_json::to_string(self).unwrap_or_default();
        let digest = Sha256::digest(canonical.as_bytes());
        digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
    }

    /// The categories present, for coverage statements.
    pub fn categories(&self) -> BTreeSet<Category> {
        self.cases.iter().map(|c| c.category).collect()
    }
}

/// The dataset shipped with the program (synthetic: see its provenance).
pub fn builtin(name: &str) -> Option<&'static str> {
    match name {
        "starter" | "starter-v1" => Some(include_str!("../../eval/datasets/starter-v1.json")),
        "comparison" | "comparison-v1" => {
            Some(include_str!("../../eval/datasets/comparison-v1.json"))
        }
        "hard" | "hard-v1" => Some(include_str!("../../eval/datasets/hard-v1.json")),
        _ => None,
    }
}

pub fn builtin_names() -> [&'static str; 3] {
    ["starter", "comparison", "hard"]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal() -> Dataset {
        serde_json::from_value(serde_json::json!({
            "format": FORMAT, "format_version": 1, "id": "t", "title": "t", "dataset_version": "1.0.0",
            "provenance": {"authors": ["a"], "created": "2026-10-12", "description": "d", "synthetic": true},
            "cases": [{
                "id": "c1", "category": "simple_fact", "question": "What is 2+2?", "instructions": "check the number",
                "objective": true, "reference_basis": "arithmetic",
                "expect": {"required": [{"id": "r", "description": "four", "any_of": ["4", "four"]}]},
                "criteria": ["factual_correctness"]
            }]
        }))
        .unwrap()
    }

    #[test]
    fn a_valid_dataset_parses_and_hashes_stably() {
        let d = minimal();
        d.validate().unwrap();
        assert_eq!(d.content_hash(), minimal().content_hash());
        let mut e = minimal();
        e.dataset_version = "1.0.1".into();
        assert_ne!(d.content_hash(), e.content_hash());
        assert_eq!(
            Dataset::parse(&serde_json::to_string(&d).unwrap()).unwrap(),
            d
        );
    }

    #[test]
    fn every_problem_is_reported_and_invented_ground_truth_is_refused() {
        let mut d = minimal();
        d.format = "other".into();
        d.cases[0].reference_basis = String::new();
        d.cases[0].criteria.push("beauty".into());
        d.cases[0].expect.must_cite = true;
        d.cases.push(d.cases[0].clone());
        let problems = d.problems();
        let text = problems.join(" | ");
        for needle in [
            "format must be",
            "must say where its reference comes from",
            "unknown criterion `beauty`",
            "must_cite needs reference evidence",
            "duplicate id",
        ] {
            assert!(text.contains(needle), "missing `{needle}` in {text}");
        }
        assert!(d.validate().unwrap_err().to_string().contains("problem(s)"));
        assert!(Dataset::parse("{").is_err());
    }

    #[test]
    fn a_required_claim_must_name_supplied_evidence_and_recorded_arms_are_checked() {
        let mut d = minimal();
        d.cases[0].expect.required_claims.push(RequiredClaim {
            any_of: vec!["4".into()],
            source_ids: vec![9],
            description: "x".into(),
        });
        d.cases[0].recorded.push(RecordedResponse {
            arm: Arm("turbo".into()),
            provenance: Provenance::Fixture,
            answer: "4".into(),
            status: "done".into(),
            latency_ms: None,
            invocations: None,
            agents: vec![],
            context_chars: None,
            input_tokens: None,
            sources: vec![],
        });
        let text = d.problems().join(" | ");
        assert!(
            text.contains("supplies") || text.contains("does not supply"),
            "{text}"
        );
        assert!(
            text.contains("unknown arm `turbo`")
                && text.contains("is not complete, partial or failed"),
            "{text}"
        );
    }

    #[test]
    fn arms_parse_strictly() {
        assert!(Arm::parse("Single:Codex").unwrap().single_agent() == Some("codex"));
        assert!(Arm::parse("balanced").unwrap().is_multi());
        assert!(Arm::parse("single:gemini").is_err() && Arm::parse("slow").is_err());
    }

    #[test]
    fn the_comparison_dataset_is_valid_has_eighteen_cases_and_no_recorded_responses() {
        let d = Dataset::parse(builtin("comparison").unwrap()).unwrap();
        assert_eq!(d.cases.len(), 18);
        assert_eq!(d.categories().len(), Category::ALL.len());
        assert!(
            d.cases.iter().all(|c| c.recorded.is_empty()),
            "a live dataset carries no answers"
        );
        assert!(
            d.cases.iter().all(|c| !c.web),
            "every reference is supplied, none depends on the web"
        );
        assert!(d.provenance.synthetic);
    }

    #[test]
    fn the_starter_dataset_is_valid_and_covers_every_category() {
        let d = Dataset::parse(builtin("starter").unwrap()).unwrap();
        assert_eq!(
            d.categories().len(),
            Category::ALL.len(),
            "{:?}",
            d.categories()
        );
        assert!(d.provenance.synthetic);
    }
}
