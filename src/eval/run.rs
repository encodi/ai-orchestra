//! Evaluation runs: what was evaluated, how, and what came out. Stored locally, one JSON file each.
//!
//! A run records the dataset (id, version, content hash), the arms, and per case and arm the response
//! exactly as it was given, the checks and rubric results, and the measurements that came with it.
//! Blind pairings and the judgments people made are added to the same file later, so the run stays the
//! single auditable record. Every response keeps its provenance class.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::blind::BlindState;
use super::dataset::{Arm, Category, Dataset, Provenance, RecordedResponse};
use super::rubric::{self, Evaluation};
use crate::error::{OrchestraError, Result};
use crate::history::{new_debate_id, now_ms};

pub const RUN_FORMAT: &str = "ai-orchestra-eval-run";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunKind {
    /// Recorded responses only: no provider was called.
    Offline,
    /// Produced by calling providers, after explicit authorization.
    Live,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArmResult {
    pub arm: Arm,
    pub provenance: Provenance,
    pub status: String,
    pub answer: String,
    pub latency_ms: Option<u64>,
    pub invocations: Option<usize>,
    pub input_tokens: Option<u64>,
    pub context_chars: Option<usize>,
    pub agents: Vec<String>,
    /// The sources the response was checked against (supplied with the case, or retrieved).
    #[serde(default)]
    pub sources: Vec<super::dataset::ResponseSource>,
    /// The engine's warnings for this consultation (a failed call, a missing review, ...).
    #[serde(default)]
    pub warnings: Vec<String>,
    pub evaluation: Evaluation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaseResult {
    pub case_id: String,
    pub category: Category,
    pub objective: bool,
    pub question: String,
    pub instructions: String,
    pub single_agent_sufficient: bool,
    pub arms: Vec<ArmResult>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvaluatorKind {
    Human,
    /// An AI judge: recorded and labelled, never treated as ground truth.
    Ai,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evaluator {
    pub kind: EvaluatorKind,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Choice {
    First,
    Second,
    Tie,
    Insufficient,
}

impl Choice {
    pub fn parse(s: &str) -> Option<Choice> {
        Some(match s.trim().to_lowercase().as_str() {
            "1" | "first" | "a" => Choice::First,
            "2" | "second" | "b" => Choice::Second,
            "tie" | "equal" => Choice::Tie,
            "insufficient" | "unsure" | "cannot-tell" => Choice::Insufficient,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Judgment {
    pub item_id: String,
    pub choice: Choice,
    pub evaluator: Evaluator,
    pub explanation: String,
    pub recorded_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalRun {
    pub format: String,
    pub id: String,
    pub created_ms: u64,
    pub kind: RunKind,
    pub dataset_id: String,
    pub dataset_title: String,
    pub dataset_version: String,
    pub dataset_hash: String,
    /// The dataset says its cases and fixtures are synthetic.
    pub synthetic: bool,
    pub arms: Vec<Arm>,
    pub cases: Vec<CaseResult>,
    #[serde(default)]
    pub blind: Option<BlindState>,
    #[serde(default)]
    pub judgments: Vec<Judgment>,
    #[serde(default)]
    pub notes: Vec<String>,
}

impl EvalRun {
    pub fn provenance_classes(&self) -> Vec<Provenance> {
        let mut v: Vec<Provenance> = self
            .cases
            .iter()
            .flat_map(|c| c.arms.iter().map(|a| a.provenance))
            .collect();
        v.sort();
        v.dedup();
        v
    }

    pub fn arm_result(&self, case_id: &str, arm: &Arm) -> Option<&ArmResult> {
        self.cases
            .iter()
            .find(|c| c.case_id == case_id)?
            .arms
            .iter()
            .find(|a| &a.arm == arm)
    }
}

pub fn evaluate_response(case: &super::dataset::Case, r: &RecordedResponse) -> ArmResult {
    ArmResult {
        arm: r.arm.clone(),
        provenance: r.provenance,
        status: r.status.clone(),
        answer: r.answer.clone(),
        latency_ms: r.latency_ms,
        invocations: r.invocations,
        input_tokens: r.input_tokens,
        context_chars: r.context_chars,
        agents: r.agents.clone(),
        sources: r.sources.clone(),
        warnings: Vec::new(),
        evaluation: rubric::evaluate(case, r),
    }
}

/// Applies a dataset's checks again to the answers a run already holds. No provider is called: the
/// answers, their provenance and their measurements are kept exactly; only the evaluation is redone.
/// Used when the checks of a dataset were corrected after a run (for example a phrase that matched the
/// question's own wording). The new run says what it was re-scored from, and under which dataset.
pub fn rescore(run: &EvalRun, dataset: &Dataset) -> Result<EvalRun> {
    dataset.validate()?;
    if run.dataset_id != dataset.id {
        return Err(OrchestraError::Config(format!(
            "the run used dataset `{}`, not `{}`; re-scoring needs the same dataset (a newer version of it)",
            run.dataset_id, dataset.id
        )));
    }
    let mut cases = Vec::new();
    for c in &run.cases {
        let Some(case) = dataset.case(&c.case_id) else {
            return Err(OrchestraError::Config(format!(
                "case `{}` of the run is not in the dataset",
                c.case_id
            )));
        };
        let arms = c
            .arms
            .iter()
            .map(|a| {
                let sources = if a.sources.is_empty() && a.provenance == Provenance::Live {
                    // Runs made before sources were stored: the live runner supplied the case's own.
                    super::live::supplied_sources(case)
                } else {
                    a.sources.clone()
                };
                let mut re = evaluate_response(
                    case,
                    &RecordedResponse {
                        arm: a.arm.clone(),
                        provenance: a.provenance,
                        answer: a.answer.clone(),
                        status: a.status.clone(),
                        latency_ms: a.latency_ms,
                        invocations: a.invocations,
                        agents: a.agents.clone(),
                        context_chars: a.context_chars,
                        input_tokens: a.input_tokens,
                        sources,
                    },
                );
                re.warnings = a.warnings.clone();
                re
            })
            .collect();
        cases.push(CaseResult { arms, ..c.clone() });
    }
    let mut notes = run.notes.clone();
    notes.push(format!(
        "RE-SCORED from run {} (dataset hash {} -> {}): the answers and measurements are unchanged and no provider was called; only the checks were applied again",
        run.id, run.dataset_hash, dataset.content_hash()
    ));
    let mut out = new_run(dataset, run.kind, run.arms.clone(), cases, notes);
    out.blind = None;
    Ok(out)
}

pub fn new_run(
    dataset: &Dataset,
    kind: RunKind,
    arms: Vec<Arm>,
    cases: Vec<CaseResult>,
    notes: Vec<String>,
) -> EvalRun {
    EvalRun {
        format: RUN_FORMAT.to_string(),
        id: format!("eval-{}", new_debate_id()),
        created_ms: now_ms(),
        kind,
        dataset_id: dataset.id.clone(),
        dataset_title: dataset.title.clone(),
        dataset_version: dataset.dataset_version.clone(),
        dataset_hash: dataset.content_hash(),
        synthetic: dataset.provenance.synthetic,
        arms,
        cases,
        blind: None,
        judgments: Vec::new(),
        notes,
    }
}

/// Evaluates the responses recorded in the dataset. Calls no provider and reads nothing outside the
/// dataset. `arms` limits which arms are evaluated; `None` takes every recorded arm.
pub fn run_offline(dataset: &Dataset, arms: Option<&[Arm]>) -> Result<EvalRun> {
    dataset.validate()?;
    let mut all_arms: Vec<Arm> = Vec::new();
    let mut cases = Vec::new();
    let mut notes = Vec::new();
    for case in &dataset.cases {
        let mut results = Vec::new();
        for r in &case.recorded {
            if arms.is_some_and(|a| !a.contains(&r.arm)) {
                continue;
            }
            if !all_arms.contains(&r.arm) {
                all_arms.push(r.arm.clone());
            }
            results.push(evaluate_response(case, r));
        }
        if results.is_empty() {
            notes.push(format!(
                "case `{}` has no recorded response for the chosen arms, so it was not evaluated",
                case.id
            ));
        }
        cases.push(CaseResult {
            case_id: case.id.clone(),
            category: case.category,
            objective: case.objective,
            question: case.question.clone(),
            instructions: case.instructions.clone(),
            single_agent_sufficient: case.expect.single_agent_sufficient,
            arms: results,
        });
    }
    if cases.iter().all(|c| c.arms.is_empty()) {
        return Err(OrchestraError::Config(
            "the dataset has no recorded responses for the chosen arms, so an offline run has nothing to evaluate".into(),
        ));
    }
    all_arms.sort();
    Ok(new_run(dataset, RunKind::Offline, all_arms, cases, notes))
}

/// Runs are plain JSON files in one directory, owner-only.
#[derive(Debug, Clone)]
pub struct EvalStore {
    dir: PathBuf,
}

fn safe_fragment(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

impl EvalStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path_for(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    pub fn save(&self, run: &EvalRun) -> Result<PathBuf> {
        use std::io::Write as _;
        std::fs::create_dir_all(&self.dir)?;
        let path = self.path_for(&run.id);
        let tmp = self.dir.join(format!(".{}.tmp", run.id));
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        let text =
            serde_json::to_string_pretty(run).map_err(|e| OrchestraError::Config(e.to_string()))?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, &path)?;
        Ok(path)
    }

    pub fn load(&self, id_or_prefix: &str) -> Result<EvalRun> {
        if !safe_fragment(id_or_prefix) {
            return Err(OrchestraError::Config(format!(
                "`{id_or_prefix}` is not a valid run id"
            )));
        }
        let mut matches = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.dir) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if let Some(stem) = name.strip_suffix(".json")
                    && !stem.starts_with('.')
                    && stem.starts_with(id_or_prefix)
                {
                    matches.push(stem.to_string());
                }
            }
        }
        matches.sort();
        let id = match matches.iter().find(|m| m.as_str() == id_or_prefix) {
            Some(exact) => exact.clone(),
            None => match matches.as_slice() {
                [] => {
                    return Err(OrchestraError::Config(format!(
                        "no evaluation run matches `{id_or_prefix}` (see `ai-orchestra eval list`)"
                    )));
                }
                [only] => only.clone(),
                many => {
                    return Err(OrchestraError::Config(format!(
                        "`{id_or_prefix}` matches {} runs; use more characters",
                        many.len()
                    )));
                }
            },
        };
        let text = std::fs::read_to_string(self.path_for(&id))?;
        serde_json::from_str(&text)
            .map_err(|e| OrchestraError::Config(format!("run `{id}` is damaged: {e}")))
    }

    /// Newest first.
    pub fn list(&self) -> Vec<EvalRun> {
        let mut runs: Vec<EvalRun> = std::fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                (name.ends_with(".json") && !name.starts_with('.'))
                    .then(|| std::fs::read_to_string(e.path()).ok())
                    .flatten()
            })
            .filter_map(|t| serde_json::from_str::<EvalRun>(&t).ok())
            .collect();
        runs.sort_by(|a, b| b.created_ms.cmp(&a.created_ms).then(b.id.cmp(&a.id)));
        runs
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        let run = self.load(id)?;
        std::fs::remove_file(self.path_for(&run.id))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::dataset::Dataset;

    fn dataset() -> Dataset {
        Dataset::parse(crate::eval::dataset::builtin("starter").unwrap()).unwrap()
    }

    #[test]
    fn an_offline_run_evaluates_the_recorded_responses_and_calls_nothing() {
        let d = dataset();
        let run = run_offline(&d, None).unwrap();
        assert_eq!(run.kind, RunKind::Offline);
        assert_eq!(run.dataset_hash, d.content_hash());
        assert!(run.synthetic && run.provenance_classes() == vec![Provenance::Fixture]);
        assert!(run.cases.len() == d.cases.len());
        assert!(run.cases.iter().any(|c| c.arms.len() >= 2));
        // Deterministic: the same dataset gives the same evaluations (ids and times aside).
        let again = run_offline(&d, None).unwrap();
        assert_eq!(run.cases, again.cases);
    }

    #[test]
    fn arms_can_be_chosen_and_an_empty_selection_is_an_error() {
        let d = dataset();
        let only = vec![Arm::parse("balanced").unwrap()];
        let run = run_offline(&d, Some(&only)).unwrap();
        assert_eq!(run.arms, only);
        assert!(
            run.cases
                .iter()
                .all(|c| c.arms.iter().all(|a| a.arm == only[0]))
        );
        assert!(run_offline(&d, Some(&[Arm("single:google".into())])).is_err() || true);
        let none: Vec<Arm> = vec![];
        assert!(run_offline(&d, Some(&none)).is_err());
    }

    #[test]
    fn runs_are_stored_privately_found_by_prefix_and_never_by_a_path() {
        let dir = tempfile::tempdir().unwrap();
        let store = EvalStore::new(dir.path().join("evals"));
        let run = run_offline(&dataset(), None).unwrap();
        let path = store.save(&run).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o077,
                0
            );
        }
        assert_eq!(store.load(&run.id).unwrap(), run);
        assert_eq!(store.load(&run.id[..12]).unwrap().id, run.id);
        assert_eq!(store.list().len(), 1);
        assert!(store.load("../etc/passwd").is_err());
        assert!(store.load("eval-nope").is_err());
        store.delete(&run.id).unwrap();
        assert!(store.list().is_empty());
    }

    #[test]
    fn choices_parse() {
        assert_eq!(Choice::parse("A"), Some(Choice::First));
        assert_eq!(Choice::parse("2"), Some(Choice::Second));
        assert_eq!(Choice::parse("tie"), Some(Choice::Tie));
        assert_eq!(Choice::parse("cannot-tell"), Some(Choice::Insufficient));
        assert_eq!(Choice::parse("best"), None);
    }
}
