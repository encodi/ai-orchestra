//! The evaluation system (Milestone 8): datasets, offline runs, the live runner with mock agents,
//! blind comparison, reports. No real model is contacted.

#![cfg(unix)]

use std::sync::Arc;

use ai_orchestra::Orchestrator;
use ai_orchestra::config::Config;
use ai_orchestra::eval::blind;
use ai_orchestra::eval::dataset::{Arm, Dataset, Provenance, builtin};
use ai_orchestra::eval::live::{self, LiveAuthorization, LiveLimits};
use ai_orchestra::eval::report;
use ai_orchestra::eval::rubric::Outcome;
use ai_orchestra::eval::run::{Choice, EvalStore, Evaluator, EvaluatorKind, RunKind, run_offline};
use ai_orchestra::provider::{AgentProvider, ProviderRegistry};
use ai_orchestra::providers::mock::{MockProvider, MockReply};
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn agents(reply: &'static str) -> Vec<Arc<MockProvider>> {
    ["claude", "codex", "google"]
        .iter()
        .map(|n| {
            Arc::new(
                MockProvider::new(n)
                    .with_reply(MockReply::Responder(Arc::new(move |_| reply.to_string()))),
            )
        })
        .collect()
}

fn orchestrator(agents: &[Arc<MockProvider>]) -> Orchestrator {
    let providers: Vec<Arc<dyn AgentProvider>> = agents
        .iter()
        .map(|a| a.clone() as Arc<dyn AgentProvider>)
        .collect();
    let mut config = Config::default();
    config.general.coordinator = "codex".into();
    Orchestrator::with_registry(config, ProviderRegistry::new(providers)).unwrap()
}

fn calls(agents: &[Arc<MockProvider>]) -> usize {
    agents.iter().map(|a| a.requests().len()).sum()
}

fn small_dataset() -> Dataset {
    Dataset::parse(
        &json!({
            "format": "ai-orchestra-eval-dataset", "format_version": 1, "id": "tiny", "title": "Tiny", "dataset_version": "1.0.0",
            "provenance": {"authors": ["tests"], "created": "2026-10-12", "description": "two cases", "synthetic": true},
            "cases": [
                {"id": "dc", "category": "simple_fact", "question": "What does DC stand for in electronics?", "instructions": "check", "objective": true,
                 "reference_basis": "standard terminology",
                 "expect": {"required": [{"id": "dc", "description": "direct current", "any_of": ["direct current"]}]},
                 "criteria": ["factual_correctness"]},
                {"id": "ac", "category": "simple_fact", "question": "What does AC stand for in electronics?", "instructions": "check", "objective": true,
                 "reference_basis": "standard terminology",
                 "expect": {"required": [{"id": "ac", "description": "alternating current", "any_of": ["alternating current"]}]},
                 "criteria": ["factual_correctness"]}
            ]
        })
        .to_string(),
    )
    .unwrap()
}

fn limits() -> LiveLimits {
    LiveLimits {
        max_calls_per_consultation: 10,
        max_total_calls: 100,
        timeout_secs: None,
    }
}

fn arms(names: &[&str]) -> Vec<Arm> {
    names.iter().map(|n| Arm::parse(n).unwrap()).collect()
}

// ------------------------------------------------------------------------- datasets and offline

#[test]
fn a_dataset_file_is_parsed_validated_and_a_broken_one_lists_every_problem() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("starter.json");
    std::fs::write(&path, builtin("starter").unwrap()).unwrap();
    let parsed = Dataset::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(parsed.cases.len(), 12);
    assert_eq!(
        parsed.content_hash(),
        Dataset::parse(builtin("starter").unwrap())
            .unwrap()
            .content_hash()
    );

    let mut broken: serde_json::Value = serde_json::from_str(builtin("starter").unwrap()).unwrap();
    broken["cases"][0]["reference_basis"] = json!("");
    broken["cases"][1]["category"] = json!("astrology");
    assert!(
        Dataset::parse(&broken.to_string()).is_err(),
        "an unknown category does not parse"
    );
    broken["cases"][1]["category"] = json!("simple_fact");
    broken["cases"][2]["criteria"] = json!(["beauty"]);
    let err = Dataset::parse(&broken.to_string()).unwrap_err().to_string();
    assert!(
        err.contains("reference_basis") && err.contains("beauty"),
        "{err}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_offline_run_never_calls_a_provider_and_is_reproducible() {
    let mocks = agents("should never be asked");
    let _orch = orchestrator(&mocks);
    let ds = Dataset::parse(builtin("starter").unwrap()).unwrap();
    let a = run_offline(&ds, None).unwrap();
    let b = run_offline(&ds, None).unwrap();
    assert_eq!(calls(&mocks), 0);
    assert_eq!(a.kind, RunKind::Offline);
    assert_eq!(a.cases, b.cases);
    assert_eq!(report::summarize(&a).len(), report::summarize(&b).len());
    // The planted flaws are found and the good fixtures pass.
    let by = |arm: &str| {
        report::summarize(&a)
            .into_iter()
            .find(|s| s.arm.0 == arm)
            .unwrap()
    };
    assert!(by("thorough").objective_correct.k > by("single:claude").objective_correct.k);
    assert!(by("single:claude").unlisted_links >= 1);
}

#[test]
fn objective_rubric_results_follow_the_dataset_checks_and_judgement_criteria_stay_unscored() {
    let run = run_offline(&Dataset::parse(builtin("starter").unwrap()).unwrap(), None).unwrap();
    let ohm = run.cases.iter().find(|c| c.case_id == "fact-ohm").unwrap();
    let outcome = |arm: &str, id: &str| {
        ohm.arms
            .iter()
            .find(|a| a.arm.0 == arm)
            .unwrap()
            .evaluation
            .criteria
            .iter()
            .find(|c| c.id == id)
            .unwrap()
            .outcome
    };
    assert_eq!(
        outcome("single:codex", "factual_correctness"),
        Outcome::Pass
    );
    assert_eq!(
        outcome("single:claude", "factual_correctness"),
        Outcome::Fail
    );
    assert_eq!(outcome("single:codex", "clarity"), Outcome::NotAssessed);
    let uncertain = run
        .cases
        .iter()
        .find(|c| c.case_id == "uncertain-price")
        .unwrap();
    let cal = |arm: &str| {
        uncertain
            .arms
            .iter()
            .find(|a| a.arm.0 == arm)
            .unwrap()
            .evaluation
            .criteria
            .iter()
            .find(|c| c.id == "uncertainty_calibration")
            .unwrap()
            .outcome
    };
    assert_eq!(
        cal("single:codex"),
        Outcome::Fail,
        "asserting a 2030 price fails"
    );
    assert_eq!(cal("single:claude"), Outcome::Pass);
    assert_eq!(cal("thorough"), Outcome::Pass);
}

#[test]
fn citation_checks_separate_mechanical_validity_from_support() {
    let run = run_offline(&Dataset::parse(builtin("starter").unwrap()).unwrap(), None).unwrap();
    let case = run
        .cases
        .iter()
        .find(|c| c.case_id == "product-scopex")
        .unwrap();
    let claude = case
        .arms
        .iter()
        .find(|a| a.arm.0 == "single:claude")
        .unwrap()
        .evaluation
        .citation
        .as_ref()
        .unwrap();
    assert_eq!(claude.unknown_ids, vec![3], "[3] names no source");
    assert_eq!(
        claude.unlisted_urls.len(),
        1,
        "a link that is not in the source list"
    );
    assert!(claude.method.contains("not a semantic judgement"));
    let codex = case
        .arms
        .iter()
        .find(|a| a.arm.0 == "single:codex")
        .unwrap()
        .evaluation
        .citation
        .as_ref()
        .unwrap();
    assert_eq!(codex.structurally_valid, codex.checks.len());
    assert!(
        codex.checks.iter().all(|c| c.passage.is_some()),
        "the passage is kept next to each check"
    );
    let science = run
        .cases
        .iter()
        .find(|c| c.case_id == "science-claim")
        .unwrap();
    let thorough = science
        .arms
        .iter()
        .find(|a| a.arm.0 == "thorough")
        .unwrap()
        .evaluation
        .citation
        .as_ref()
        .unwrap();
    assert_eq!(thorough.unknown_ids, vec![4], "the invented [4] is caught");
    assert!(thorough.unlisted_urls.is_empty());
    // The wording "verified against the manufacturer's own site" is only a flag when nothing was retrieved;
    // here something was, so the structural problem is the unknown id.
    assert!(thorough.unsupported_labels.is_empty());
}

#[test]
fn small_samples_say_so_and_never_claim_a_difference() {
    let run = run_offline(&Dataset::parse(builtin("starter").unwrap()).unwrap(), None).unwrap();
    let mut r = run.clone();
    blind::prepare(
        &mut r,
        &[(Arm("single:codex".into()), Arm("thorough".into()))],
        1,
    )
    .unwrap();
    let ids: Vec<String> = r
        .blind
        .as_ref()
        .unwrap()
        .items
        .iter()
        .map(|i| i.item_id.clone())
        .collect();
    let human = Evaluator {
        kind: EvaluatorKind::Human,
        name: "me".into(),
    };
    for id in ids.iter().take(4) {
        blind::record(&mut r, id, Choice::First, human.clone(), "").unwrap();
    }
    let text = report::render(&r, 1);
    assert!(text.contains("too few to report an interval"), "{text}");
    assert!(!text.contains("excludes 50%"));
    assert!(text.contains("A preference is not an accuracy"));
}

#[test]
fn blind_comparison_never_pairs_different_kinds_of_results() {
    let mut run = run_offline(&Dataset::parse(builtin("starter").unwrap()).unwrap(), None).unwrap();
    for c in &mut run.cases {
        for a in &mut c.arms {
            if a.arm.0 == "thorough" {
                a.provenance = Provenance::Mock;
            }
        }
    }
    let err = blind::prepare(
        &mut run,
        &[(Arm("single:codex".into()), Arm("thorough".into()))],
        1,
    )
    .unwrap_err();
    assert!(err.to_string().contains("same provenance"), "{err}");
    let text = report::render(&run, 1);
    assert!(
        text.contains("== fixture results ==") && text.contains("== mock results =="),
        "kinds are reported apart"
    );
}

// -------------------------------------------------------------------------------- the live runner

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_live_plan_is_free_and_says_what_a_run_would_cost() {
    let mocks = agents("DC stands for direct current. AC stands for alternating current.");
    let orch = orchestrator(&mocks);
    let ds = small_dataset();
    let plan = live::plan(
        &orch,
        &ds,
        None,
        &arms(&["single:codex", "thorough"]),
        &limits(),
    )
    .unwrap();
    assert_eq!(calls(&mocks), 0, "planning calls nothing");
    assert_eq!(plan.items.len(), 4);
    assert!(
        plan.items
            .iter()
            .filter(|i| i.arm.0 == "single:codex")
            .all(|i| i.max_calls == 1)
    );
    assert!(
        plan.items
            .iter()
            .filter(|i| i.arm.0 == "thorough")
            .all(|i| i.max_calls > 1),
        "{:?}",
        plan.items
    );
    assert!(plan.max_total >= plan.typical_total && plan.max_total > 4);
    let text = plan.render();
    assert!(
        text.contains("subscription quota") && text.contains("at most") && text.contains("Ctrl-C"),
        "{text}"
    );
    // An arm that cannot run here is announced, not hidden.
    let only_codex = orchestrator(&[Arc::new(
        MockProvider::new("codex").with_reply(MockReply::Text("x".into())),
    )]);
    let blocked = live::plan(&only_codex, &ds, None, &arms(&["single:claude"]), &limits()).unwrap();
    assert!(blocked.items.iter().all(|i| i.blocked.is_some()));
    assert!(blocked.render().contains("NOT RUNNABLE HERE"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_live_run_records_provenance_calls_and_time_and_keeps_its_kind_apart_from_fixtures() {
    let mocks = agents("DC stands for direct current. AC stands for alternating current.");
    let orch = orchestrator(&mocks);
    let ds = small_dataset();
    let run = live::run(
        &orch,
        &ds,
        None,
        &arms(&["single:codex", "fast"]),
        &limits(),
        &LiveAuthorization::explicitly_granted_by_the_user(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(run.kind, RunKind::Live);
    assert_eq!(run.provenance_classes(), vec![Provenance::Live]);
    let single = run.arm_result("dc", &Arm("single:codex".into())).unwrap();
    assert_eq!(
        (single.invocations, single.status.as_str()),
        (Some(1), "complete")
    );
    assert!(single.latency_ms.is_some());
    assert_eq!(single.evaluation.objective_correct, Some(true));
    assert!(calls(&mocks) >= 4, "two cases x two arms");
    let text = report::render(&run, 1);
    assert!(
        text.contains("live: providers were called") && text.contains("== live results =="),
        "{text}"
    );
    assert!(
        !text.contains("READ THIS FIRST"),
        "that warning is for fixtures"
    );
    // Stored apart from a fixture run: comparing them warns.
    let fixture = run_offline(&Dataset::parse(builtin("starter").unwrap()).unwrap(), None).unwrap();
    let cmp = report::compare(&fixture, &run);
    assert!(
        cmp.contains("different kinds of results") && cmp.contains("different datasets"),
        "{cmp}"
    );
    let dir = tempfile::tempdir().unwrap();
    let store = EvalStore::new(dir.path().join("evals"));
    store.save(&run).unwrap();
    store.save(&fixture).unwrap();
    assert_eq!(store.list().len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quality_and_efficiency_are_compared_only_from_recorded_calls_on_shared_cases() {
    let mocks = agents("DC stands for direct current. AC stands for alternating current.");
    let orch = orchestrator(&mocks);
    let run = live::run(
        &orch,
        &small_dataset(),
        None,
        &arms(&["single:codex", "thorough"]),
        &limits(),
        &LiveAuthorization::explicitly_granted_by_the_user(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    let text = report::tradeoffs(&run).join("\n");
    assert!(
        text.contains("single:codex made 2 agent call(s), thorough made"),
        "{text}"
    );
    assert!(text.contains("Objective checks passed: 2 of 2"), "{text}");
    assert!(
        text.contains("Only 2 shared case(s)") && text.contains("too few"),
        "two cases are never enough to call anything alike: {text}"
    );
    assert!(!text.contains("used fewer agent calls"), "{text}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_run_above_the_call_ceiling_is_refused_before_any_call() {
    let mocks = agents("DC stands for direct current.");
    let orch = orchestrator(&mocks);
    let tight = LiveLimits {
        max_total_calls: 2,
        ..limits()
    };
    let err = live::run(
        &orch,
        &small_dataset(),
        None,
        &arms(&["thorough"]),
        &tight,
        &LiveAuthorization::explicitly_granted_by_the_user(),
        &CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("ceiling"), "{err}");
    assert_eq!(calls(&mocks), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancelled_live_run_stops_and_says_it_is_partial() {
    let mocks = agents("DC stands for direct current.");
    let orch = orchestrator(&mocks);
    let cancel = CancellationToken::new();
    cancel.cancel();
    let run = live::run(
        &orch,
        &small_dataset(),
        None,
        &arms(&["single:codex"]),
        &limits(),
        &LiveAuthorization::explicitly_granted_by_the_user(),
        &cancel,
    )
    .await
    .unwrap();
    assert_eq!(calls(&mocks), 0);
    assert!(
        run.notes
            .iter()
            .any(|n| n.contains("CANCELLED") && n.contains("partial")),
        "{:?}",
        run.notes
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failing_provider_is_recorded_as_failed_and_never_retried() {
    let mut failing = MockProvider::new("codex");
    for _ in 0..20 {
        failing = failing.with_reply(MockReply::Failure("scripted failure".into()));
    }
    let mocks = vec![Arc::new(failing)];
    let orch = orchestrator(&mocks);
    let run = live::run(
        &orch,
        &small_dataset(),
        None,
        &arms(&["single:codex"]),
        &limits(),
        &LiveAuthorization::explicitly_granted_by_the_user(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    let r = run.arm_result("dc", &Arm("single:codex".into())).unwrap();
    assert_ne!(r.status, "complete");
    assert_eq!(r.evaluation.objective_correct, Some(false));
    assert_eq!(calls(&mocks), 2, "one call per consultation, none retried");
    let s = &report::summarize(&run)[0];
    assert_eq!(s.failed_or_partial.k, 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn follow_up_cases_carry_their_history_into_a_live_prompt() {
    let mocks = agents("Scopex B.");
    let orch = orchestrator(&mocks);
    let ds = Dataset::parse(builtin("starter").unwrap()).unwrap();
    live::run(
        &orch,
        &ds,
        Some(&["followup-channels".to_string()]),
        &arms(&["single:codex"]),
        &limits(),
        &LiveAuthorization::explicitly_granted_by_the_user(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    let prompt = mocks[1].requests().pop().unwrap().prompt;
    assert!(
        prompt.contains("Earlier in this conversation")
            && prompt.contains("Scopex B has 4 channels"),
        "{prompt}"
    );
    assert!(prompt.contains("Of the two cheapest ones"));
    assert!(
        !prompt.contains("<memory_context"),
        "live evaluation runs are isolated from memory"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn supplied_sources_reach_the_prompt_and_count_as_evidence_for_the_citation_checks() {
    let mocks = agents("The trial found no significant difference from placebo (p = 0.41) [1].");
    let orch = orchestrator(&mocks);
    let ds = Dataset::parse(builtin("comparison").unwrap()).unwrap();
    let run = live::run(
        &orch,
        &ds,
        Some(&["science-claim".to_string()]),
        &arms(&["single:codex"]),
        &limits(),
        &LiveAuthorization::explicitly_granted_by_the_user(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    let prompt = mocks[1].requests().remove(0).prompt;
    assert!(
        prompt.contains("Sources supplied with this question") && prompt.contains("p = 0.41"),
        "{prompt}"
    );
    let cit = run
        .arm_result("science-claim", &Arm("single:codex".into()))
        .unwrap()
        .evaluation
        .citation
        .as_ref()
        .unwrap();
    assert_eq!(
        (cit.checks.len(), cit.structurally_valid),
        (1, 1),
        "[1] names the supplied source"
    );
    assert_eq!(
        run.arm_result("science-claim", &Arm("single:codex".into()))
            .unwrap()
            .evaluation
            .objective_correct,
        Some(true)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_comparison_dataset_plans_a_bounded_live_experiment_without_calling_anything() {
    let mocks = agents("x");
    let orch = orchestrator(&mocks);
    let ds = Dataset::parse(builtin("comparison").unwrap()).unwrap();
    let plan = live::plan(
        &orch,
        &ds,
        None,
        &arms(&["single:codex", "balanced", "thorough"]),
        &LiveLimits::default(),
    )
    .unwrap();
    assert_eq!(calls(&mocks), 0);
    assert_eq!(plan.items.len(), 54, "18 cases x 3 arms");
    assert!(
        plan.max_total <= LiveLimits::default().max_total_calls * 3,
        "{}",
        plan.max_total
    );
    assert!(plan.typical_total >= 54 && plan.max_total >= plan.typical_total);
}

// The answers below are real Codex answers from the first live run (2026-10-12). The first versions of
// three checks matched the question's own wording or missed a paraphrase and failed them wrongly.
#[test]
fn checks_do_not_fail_answers_that_restate_the_question_or_paraphrase_the_fact() {
    use ai_orchestra::eval::dataset::RecordedResponse;
    use ai_orchestra::eval::rubric::evaluate;
    let ds = Dataset::parse(builtin("comparison").unwrap()).unwrap();
    let resp = |answer: &str| RecordedResponse {
        arm: Arm("single:codex".into()),
        provenance: Provenance::Live,
        answer: answer.into(),
        status: "complete".into(),
        latency_ms: None,
        invocations: Some(1),
        agents: vec!["codex".into()],
        context_chars: None,
        input_tokens: None,
        sources: ai_orchestra::eval::live::supplied_sources_for_tests(
            ds.case("science-claim").unwrap(),
        ),
    };
    let check =
        |id: &str, answer: &str| evaluate(ds.case(id).unwrap(), &resp(answer)).objective_correct;
    assert_eq!(
        check(
            "uncertain-price",
            "There isn\u{2019}t enough information to determine what a Scopex B will cost in 2030. Any specific price would be a guess."
        ),
        Some(true),
        "restating the question is not asserting a price"
    );
    assert_eq!(
        check(
            "uncertain-price",
            "A Scopex B will cost around 6,500 MXN in 2030."
        ),
        Some(false),
        "asserting one still fails"
    );
    assert_eq!(
        check(
            "followup-channels",
            "Scopex B has more channels: 4, compared with Scopex A\u{2019}s 2."
        ),
        Some(true)
    );
    assert_eq!(
        check("followup-channels", "I do not have the earlier comparison."),
        Some(false)
    );
    let science = |a: &str| {
        let case = ds.case("science-claim").unwrap();
        let mut r = resp(a);
        r.sources = ai_orchestra::eval::live::supplied_sources_for_tests(case);
        evaluate(case, &r).objective_correct
    };
    assert_eq!(
        science(
            "No. The supplied summary does not support the website\u{2019}s claim that supplement X improves memory: the difference was not statistically significant (p = 0.41). [1]"
        ),
        Some(true)
    );
    assert_eq!(
        science("Yes, supplement X clearly improves memory [1]."),
        Some(false)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn re_scoring_applies_corrected_checks_to_stored_answers_without_calling_anyone() {
    let mocks = agents("DC stands for direct current. AC stands for alternating current.");
    let orch = orchestrator(&mocks);
    let ds = small_dataset();
    let run = live::run(
        &orch,
        &ds,
        None,
        &arms(&["single:codex"]),
        &limits(),
        &LiveAuthorization::explicitly_granted_by_the_user(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    let calls_before = calls(&mocks);
    // A stricter check (as if the first one had been wrong the other way) changes only the evaluation.
    let mut v2 = ds.clone();
    v2.dataset_version = "1.0.1".into();
    v2.cases[0].expect.required[0].any_of = vec!["pulsating current".into()];
    let again = ai_orchestra::eval::run::rescore(&run, &v2).unwrap();
    assert_eq!(calls(&mocks), calls_before, "no provider was called");
    assert_ne!(again.id, run.id);
    assert_eq!(again.kind, RunKind::Live);
    let (a, b) = (
        run.arm_result("dc", &Arm("single:codex".into())).unwrap(),
        again.arm_result("dc", &Arm("single:codex".into())).unwrap(),
    );
    assert_eq!(
        (a.answer.as_str(), a.latency_ms, a.invocations),
        (b.answer.as_str(), b.latency_ms, b.invocations),
        "answers and measurements are kept"
    );
    assert_eq!(
        (
            a.evaluation.objective_correct,
            b.evaluation.objective_correct
        ),
        (Some(true), Some(false))
    );
    assert!(
        again
            .notes
            .iter()
            .any(|n| n.contains("RE-SCORED") && n.contains("no provider was called")),
        "{:?}",
        again.notes
    );
    assert_ne!(run.dataset_hash, again.dataset_hash);
    // The wrong dataset is refused.
    let other = Dataset::parse(builtin("starter").unwrap()).unwrap();
    assert!(ai_orchestra::eval::run::rescore(&run, &other).is_err());
}

// ------------------------------------------------------------------------------ the `hard` dataset

#[test]
fn the_hard_dataset_is_valid_unanswered_and_every_case_is_objective() {
    let d = Dataset::parse(builtin("hard").unwrap()).unwrap();
    assert_eq!(d.cases.len(), 12);
    assert!(
        d.cases
            .iter()
            .all(|c| c.recorded.is_empty() && c.objective && !c.web)
    );
    assert!(
        d.cases.iter().all(|c| !c.reference_basis.is_empty()),
        "every reference is stated"
    );
    assert!(d.provenance.notes.contains("BEFORE any answer"));
}

/// The first dataset's checks had to be corrected after a live run, so these are tested against plausible
/// correct answers (including ones that restate the question or paraphrase) and plausible wrong ones
/// BEFORE any model answers them.
#[test]
fn the_checks_of_the_hard_cases_accept_correct_answers_however_phrased_and_reject_the_classic_slips()
 {
    use ai_orchestra::eval::dataset::RecordedResponse;
    use ai_orchestra::eval::rubric::{Outcome, evaluate};
    let ds = Dataset::parse(builtin("hard").unwrap()).unwrap();
    let passes = |id: &str, answer: &str| -> bool {
        let case = ds.case(id).unwrap();
        let r = RecordedResponse {
            arm: Arm("single:codex".into()),
            provenance: Provenance::Live,
            answer: answer.into(),
            status: "complete".into(),
            latency_ms: None,
            invocations: Some(1),
            agents: vec![],
            context_chars: None,
            input_tokens: None,
            sources: ai_orchestra::eval::live::supplied_sources_for_tests(case),
        };
        let e = evaluate(case, &r);
        let calibrated = e
            .criteria
            .iter()
            .find(|c| c.id == "uncertainty_calibration")
            .is_none_or(|c| c.outcome != Outcome::Fail);
        e.objective_correct == Some(true) && calibrated
    };
    let table: &[(&str, &[&str], &[&str])] = &[
        (
            "hard-rc-cutoff",
            &[
                "f_c = 1/(2*pi*R*C) = 1/(2*pi*1600*100e-9) = 994.7 Hz, so about 995 Hz.",
                "The cutoff frequency of this RC low-pass filter is roughly 0.995 kHz (995 Hz).",
                "Using f = 1/(2πRC): 1/(2π × 1.6 kΩ × 100 nF) ≈ 994.7 Hz.",
            ],
            &[
                "f = 1/(RC) = 1/(1600 x 100e-9) = 6,250 Hz.",
                "The -3 dB cutoff is about 1.6 kHz.",
                "Roughly 6.25 kHz.",
            ],
        ),
        (
            "hard-battery-runtime",
            &[
                "Series cells add voltage, not capacity: 14.8 V x 2.5 Ah = 37 Wh, and 37 / 12 = 3.08 hours.",
                "About 3.1 hours (roughly 3 h 5 min).",
                "The pack stores 37 Wh, so at 12 W it lasts approximately 3.083 hours.",
            ],
            &[
                "The pack is 10 Ah at 14.8 V, 148 Wh, so 148 / 12 = 12.3 hours.",
                "It runs for about 1 hour.",
                "12.33 h.",
            ],
        ),
        (
            "hard-decibel-gain",
            &[
                "40 dB of voltage gain is a factor of 100, so the output is 0.5 V rms (500 mV).",
                "Gain of 100 x 5 mV = 500 mV.",
                "The output is half a volt rms.",
            ],
            &[
                "The output is 50 V rms.",
                "40 dB is 10,000 times, so 50 V.",
                "About 5 mV.",
            ],
        ),
        (
            "hard-led-current",
            &[
                "The resistor drops 3.3 - 2.0 = 1.3 V, so I = 1.3 / 220 = 5.9 mA.",
                "I is about 5.91 mA.",
                "Roughly 0.0059 A.",
            ],
            &["I = 3.3 V / 220 ohm = 15 mA.", "About 9 mA.", "10 mA."],
        ),
        (
            "hard-resistor-rating",
            &[
                "P = V^2/R = 25/100 = 0.25 W, which is exactly the resistor's rating, so there is no margin; derate and use a higher-rated part.",
                "It dissipates 250 mW, right at its rating, with no headroom, so I would not rely on it.",
            ],
            &[
                "It dissipates 0.25 W, well within its rating, so there is plenty of margin.",
                "Yes, it is safe.",
                "P = 2.5 W, so no.",
            ],
        ),
        (
            "hard-premise-portable",
            &[
                "Not quite: according to the datasheet the Scopex C is a benchtop unit and is not portable. It has four channels and costs 11,800 MXN [1].",
                "The premise is wrong - the Scopex C is not portable (it is benchtop). Four channels, 11,800 MXN [1].",
            ],
            &[
                "Yes, the Scopex C is portable and has four channels. It costs 11,800 MXN [1].",
                "It has four channels and costs 11,800 MXN [1].",
            ],
        ),
        (
            "hard-missing-input-voltage",
            &[
                "The supplied datasheet does not state a maximum input voltage for the Scopex B.",
                "That is not specified in the datasheet, so I cannot give a value.",
                "Not given: the datasheet lists channels, bandwidth, price and portability only.",
            ],
            &[
                "The Scopex B accepts up to 300 V.",
                "The datasheet does not state it, but similar scopes usually take 300 V CAT II.",
                "It is 400 V.",
            ],
        ),
        (
            "hard-no-feasible-option",
            &[
                "None of the three meets all your constraints: A fits the budget but has 2 channels, B has 4 channels and is handheld but costs 7,400 MXN [1], and C is not portable. You would have to raise the budget.",
                "No model satisfies all three. Scopex B comes closest (4 channels, handheld) but at 7,400 MXN it is over your 5,000 MXN budget [1].",
            ],
            &[
                "Buy the Scopex B: it has 4 channels, is handheld and costs 7,400 MXN [1].",
                "Scopex B fits your budget and has four channels. None of the others do. 7,400 MXN.",
            ],
        ),
        (
            "hard-changed-preference",
            &[
                "With your budget now at 8,000 MXN, buy the Scopex B: 4 channels, handheld, 7,400 MXN [1].",
                "Scopex B. It costs 7,400 MXN, within your 8,000 MXN budget, and has four channels.",
            ],
            &[
                "None of the three fits your budget and needs, so none.",
                "Scopex A, at 3,900 MXN.",
            ],
        ),
        (
            "hard-newer-source",
            &[
                "The latest firmware is 2.5 [2]; version 2.4 [1] was superseded in September.",
                "Firmware 2.5 (September) replaces 2.4, which is no longer offered [2].",
            ],
            &[
                "The latest firmware version is 2.4 [1].",
                "The Kestrel K1 runs firmware 3.0.",
            ],
        ),
        (
            "hard-mixed-studies",
            &[
                "No. The small study (20 volunteers, p = 0.03) [1] found an improvement, but the larger study of 400 volunteers (p = 0.60) [2] did not, so the evidence is mixed and does not establish that compound Q improves reaction time.",
                "The studies are inconsistent: study 1 [1] is positive but small, study 2 [2] is larger and null; this is inconclusive.",
            ],
            &[
                "Yes, compound Q clearly improves reaction time [1].",
                "Study 1 is positive [1], so compound Q works.",
            ],
        ),
        (
            "hard-parallel",
            &[
                "1/(1/6k + 1/3k) = 2 kilohm.",
                "The equivalent resistance is 2000 ohms.",
                "(6k x 3k)/(6k + 3k) = 2 kΩ.",
            ],
            &["Adding them gives 9 kilohm.", "It is 4.5 kilohm."],
        ),
    ];
    assert_eq!(table.len(), ds.cases.len(), "every case is covered");
    for (id, good, bad) in table {
        for a in *good {
            assert!(passes(id, a), "{id}: a correct answer was rejected: {a}");
        }
        for a in *bad {
            assert!(!passes(id, a), "{id}: a wrong answer was accepted: {a}");
        }
    }
}
