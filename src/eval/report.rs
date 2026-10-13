//! Reports. They state what was measured, on what, and what cannot be concluded.
//!
//! Rules this module keeps:
//! * results of different provenance classes (fixture, mock, recorded, live) are summarised
//!   separately and each class is described; a fixture report says in its first lines that it says
//!   nothing about any model;
//! * a comparison sentence is generated only from measurements that exist for both sides on the same
//!   cases, and it is worded as a count on this dataset, never as a general claim;
//! * below [`stats::MIN_FOR_INTERVAL`] shared cases no statement about "similar" or "better" is made;
//! * tokens and money are never estimated: only input tokens a CLI reported are shown;
//! * subjective preference rates are never presented as accuracy.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::Serialize;

use super::blind;
use super::citations::{ClaimEvidence, LexicalSupport};
use super::dataset::{Arm, Provenance};
use super::rubric::Outcome;
use super::run::{ArmResult, EvalRun};
use super::stats::{self, MIN_FOR_INTERVAL, Rate};

#[derive(Debug, Clone, Serialize)]
pub struct CriterionTally {
    pub pass: usize,
    pub fail: usize,
    pub not_applicable: usize,
    pub not_assessed: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArmSummary {
    pub arm: Arm,
    pub provenance: Provenance,
    pub cases: usize,
    pub failed_or_partial: Rate,
    pub objective_correct: Rate,
    /// Structurally valid citations over all citations the answers made.
    pub citation_validity: Rate,
    /// Required claims that were made and cited to the supplied evidence, over all required claims.
    pub evidence_coverage: Rate,
    pub unlisted_links: usize,
    pub unbacked_verification_wording: usize,
    /// Citations whose sentence shares little vocabulary with the passage (a flag, not a verdict).
    pub lexically_unsupported: usize,
    pub latency_median_ms: Option<f64>,
    pub invocations_total: Option<usize>,
    pub invocations_median: Option<f64>,
    pub context_chars_median: Option<f64>,
    pub input_tokens_median: Option<f64>,
    pub answer_words_median: Option<f64>,
    pub criteria: BTreeMap<String, CriterionTally>,
}

fn collect<'a>(
    run: &'a EvalRun,
    arm: &Arm,
    prov: Provenance,
) -> Vec<(&'a super::run::CaseResult, &'a ArmResult)> {
    run.cases
        .iter()
        .filter_map(|c| {
            c.arms
                .iter()
                .find(|a| &a.arm == arm && a.provenance == prov)
                .map(|a| (c, a))
        })
        .collect()
}

pub fn summarize(run: &EvalRun) -> Vec<ArmSummary> {
    let mut keys: Vec<(Arm, Provenance)> = run
        .cases
        .iter()
        .flat_map(|c| c.arms.iter().map(|a| (a.arm.clone(), a.provenance)))
        .collect();
    keys.sort();
    keys.dedup();
    keys.into_iter()
        .map(|(arm, prov)| {
            let rows = collect(run, &arm, prov);
            let obj: Vec<bool> = rows
                .iter()
                .filter_map(|(_, a)| a.evaluation.objective_correct)
                .collect();
            let mut cit_total = 0;
            let mut cit_valid = 0;
            let (mut unlisted, mut labels, mut lexical) = (0, 0, 0);
            let (mut rc_total, mut rc_ok) = (0, 0);
            for (_, a) in &rows {
                if let Some(c) = &a.evaluation.citation {
                    cit_total += c.checks.len();
                    cit_valid += c.structurally_valid;
                    unlisted += c.unlisted_urls.len();
                    labels += c.unsupported_labels.len();
                    lexical += c
                        .checks
                        .iter()
                        .filter(|x| x.support == LexicalSupport::None)
                        .count();
                    rc_total += c.required_claims.len();
                    rc_ok += c
                        .required_claims
                        .iter()
                        .filter(|r| r.result == ClaimEvidence::MadeAndCited)
                        .count();
                }
            }
            let nums = |f: &dyn Fn(&ArmResult) -> Option<f64>| -> Vec<f64> {
                rows.iter().filter_map(|(_, a)| f(a)).collect()
            };
            let inv = nums(&|a| a.invocations.map(|x| x as f64));
            let mut criteria: BTreeMap<String, CriterionTally> = BTreeMap::new();
            for (_, a) in &rows {
                for cr in &a.evaluation.criteria {
                    let t = criteria.entry(cr.id.clone()).or_insert(CriterionTally {
                        pass: 0,
                        fail: 0,
                        not_applicable: 0,
                        not_assessed: 0,
                    });
                    match cr.outcome {
                        Outcome::Pass => t.pass += 1,
                        Outcome::Fail => t.fail += 1,
                        Outcome::NotApplicable => t.not_applicable += 1,
                        Outcome::NotAssessed => t.not_assessed += 1,
                    }
                }
            }
            ArmSummary {
                arm,
                provenance: prov,
                cases: rows.len(),
                failed_or_partial: Rate::new(
                    rows.iter().filter(|(_, a)| a.status != "complete").count(),
                    rows.len(),
                ),
                objective_correct: Rate::new(obj.iter().filter(|b| **b).count(), obj.len()),
                citation_validity: Rate::new(cit_valid, cit_total),
                evidence_coverage: Rate::new(rc_ok, rc_total),
                unlisted_links: unlisted,
                unbacked_verification_wording: labels,
                lexically_unsupported: lexical,
                latency_median_ms: stats::median(&nums(&|a| a.latency_ms.map(|x| x as f64))),
                invocations_total: (inv.len() == rows.len() && !inv.is_empty())
                    .then(|| inv.iter().sum::<f64>() as usize),
                invocations_median: stats::median(&inv),
                context_chars_median: stats::median(&nums(&|a| a.context_chars.map(|x| x as f64))),
                input_tokens_median: stats::median(&nums(&|a| a.input_tokens.map(|x| x as f64))),
                answer_words_median: stats::median(&nums(&|a| {
                    Some(a.evaluation.answer_words as f64)
                })),
                criteria,
            }
        })
        .collect()
}

fn opt(v: Option<f64>, unit: &str) -> String {
    v.map_or("not recorded".to_string(), |x| format!("{x:.0}{unit}"))
}

/// Sentences comparing arms on the cases both answered, only where the measurements exist.
pub fn tradeoffs(run: &EvalRun) -> Vec<String> {
    let mut out = Vec::new();
    let mut classes = run.provenance_classes();
    classes.retain(|p| {
        run.arms
            .iter()
            .filter(|a| !collect(run, a, *p).is_empty())
            .count()
            >= 2
    });
    for prov in classes {
        let arms: Vec<&Arm> = run
            .arms
            .iter()
            .filter(|a| !collect(run, a, prov).is_empty())
            .collect();
        for (i, x) in arms.iter().enumerate() {
            for y in &arms[i + 1..] {
                let shared: Vec<&str> = run
                    .cases
                    .iter()
                    .filter(|c| {
                        c.arms.iter().any(|a| &a.arm == *x && a.provenance == prov)
                            && c.arms.iter().any(|a| &a.arm == *y && a.provenance == prov)
                    })
                    .map(|c| c.case_id.as_str())
                    .collect();
                if shared.is_empty() {
                    continue;
                }
                let get =
                    |arm: &Arm, id: &str| run.arm_result(id, arm).filter(|a| a.provenance == prov);
                let calls = |arm: &Arm| -> Option<usize> {
                    shared
                        .iter()
                        .map(|id| get(arm, id).and_then(|a| a.invocations))
                        .sum::<Option<usize>>()
                };
                let obj = |arm: &Arm| -> (usize, usize) {
                    let v: Vec<bool> = shared
                        .iter()
                        .filter_map(|id| get(arm, id).and_then(|a| a.evaluation.objective_correct))
                        .collect();
                    (v.iter().filter(|b| **b).count(), v.len())
                };
                let (ox, oy) = (obj(x), obj(y));
                let n = shared.len();
                let mut s = format!("[{}] {x} vs {y} on {n} shared case(s): ", prov.as_str());
                match (calls(x), calls(y)) {
                    (Some(cx), Some(cy)) => {
                        let _ = write!(s, "{x} made {cx} agent call(s), {y} made {cy}. ");
                    }
                    _ => s.push_str("agent calls were not recorded for both. "),
                }
                if ox.1 == oy.1 && ox.1 > 0 {
                    let _ = write!(
                        s,
                        "Objective checks passed: {} of {} for {x}, {} of {} for {y}. ",
                        ox.0, ox.1, oy.0, oy.1
                    );
                } else {
                    s.push_str("No objective check is available on the same cases for both. ");
                }
                if n < MIN_FOR_INTERVAL {
                    let _ = write!(
                        s,
                        "Only {n} shared case(s): too few to say whether the two are alike or one is better; these are counts on this dataset."
                    );
                } else if let (Some(cx), Some(cy)) = (calls(x), calls(y))
                    && cx < cy
                    && ox.1 == oy.1
                    && ox.0 >= oy.0
                {
                    let _ = write!(
                        s,
                        "On this dataset {x} used fewer agent calls than {y} while passing at least as many objective checks; this does not carry over to other questions."
                    );
                } else {
                    s.push_str("No saving can be claimed from these numbers.");
                }
                out.push(s);
            }
        }
    }
    out
}

fn banner(run: &EvalRun) -> String {
    let mut out = String::new();
    for p in run.provenance_classes() {
        let _ = writeln!(out, "  * {}: {}", p.as_str(), p.describe());
    }
    if run.synthetic {
        out.push_str("  * the dataset is synthetic: its products, studies and pages are invented for testing.\n");
    }
    out
}

pub fn render(run: &EvalRun, seed: u64) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "Evaluation run {}  ({})",
        run.id,
        match run.kind {
            super::run::RunKind::Offline => "offline: no provider was called",
            super::run::RunKind::Live => "live: providers were called",
        }
    );
    let _ = writeln!(
        out,
        "dataset: {} v{} [{}]  hash {}",
        run.dataset_title, run.dataset_version, run.dataset_id, run.dataset_hash
    );
    let _ = writeln!(out, "what these results are:\n{}", banner(run));
    let summaries = summarize(run);
    if run.provenance_classes().contains(&Provenance::Fixture) {
        out.push_str("READ THIS FIRST: fixture responses were written by hand to exercise the checks. A number below describes the fixtures, not a model.\n\n");
    }
    for prov in run.provenance_classes() {
        let _ = writeln!(out, "== {} results ==", prov.as_str());
        let _ = writeln!(
            out,
            "{:<16} {:>5} {:>26} {:>26} {:>9} {:>10} {:>8}",
            "arm",
            "cases",
            "objective checks passed",
            "citations structurally ok",
            "median s",
            "calls",
            "words"
        );
        for s in summaries.iter().filter(|s| s.provenance == prov) {
            let _ = writeln!(
                out,
                "{:<16} {:>5} {:>26} {:>26} {:>9} {:>10} {:>8}",
                s.arm.0,
                s.cases,
                s.objective_correct.rate.map_or("n/a".into(), |r| format!(
                    "{}/{} ({:.0}%)",
                    s.objective_correct.k,
                    s.objective_correct.n,
                    r * 100.0
                )),
                s.citation_validity.rate.map_or("n/a".into(), |r| format!(
                    "{}/{} ({:.0}%)",
                    s.citation_validity.k,
                    s.citation_validity.n,
                    r * 100.0
                )),
                s.latency_median_ms
                    .map_or("n/r".into(), |l| format!("{:.1}", l / 1000.0)),
                s.invocations_total.map_or("n/r".into(), |c| c.to_string()),
                opt(s.answer_words_median, "")
            );
        }
        out.push('\n');
        for s in summaries.iter().filter(|s| s.provenance == prov) {
            let _ = writeln!(
                out,
                "{}: evidence coverage {} · incomplete runs {} · links not in the source list {} · verification wording with nothing retrieved {} · citations whose sentence shares little vocabulary with the passage {} (a flag to read, not a verdict){}{}",
                s.arm,
                s.evidence_coverage.render(),
                s.failed_or_partial.render(),
                s.unlisted_links,
                s.unbacked_verification_wording,
                s.lexically_unsupported,
                s.input_tokens_median.map_or(String::new(), |t| format!(
                    " · median input tokens {t:.0} (as reported by the CLIs)"
                )),
                s.context_chars_median.map_or(String::new(), |t| format!(
                    " · median recalled context {t:.0} chars"
                )),
            );
        }
        out.push('\n');
    }
    out.push_str("Rubric (deterministic criteria are decided by the dataset's checks; judgement criteria are left to people and are not scored):\n");
    for s in &summaries {
        let tally: Vec<String> = s
            .criteria
            .iter()
            .filter(|(_, t)| t.pass + t.fail > 0)
            .map(|(k, t)| format!("{k} {}/{}", t.pass, t.pass + t.fail))
            .collect();
        let pending = s.criteria.values().map(|t| t.not_assessed).sum::<usize>();
        let _ = writeln!(
            out,
            "  {} [{}]: {}{}",
            s.arm,
            s.provenance.as_str(),
            if tally.is_empty() {
                "no deterministic criterion applied".to_string()
            } else {
                tally.join(", ")
            },
            if pending > 0 {
                format!("; {pending} judgement criterion result(s) awaiting a person")
            } else {
                String::new()
            }
        );
    }
    out.push('\n');
    let trade = tradeoffs(run);
    if !trade.is_empty() {
        out.push_str(
            "Quality against efficiency (counts on this dataset; nothing is extrapolated):\n",
        );
        for t in trade {
            let _ = writeln!(out, "  - {t}");
        }
        out.push('\n');
    }
    let pairs = blind::results(run, seed);
    if !pairs.is_empty() {
        out.push_str("Blind pairwise comparison (revealed here; judges did not see the arms). A preference is not an accuracy:\n");
        for p in &pairs {
            let h = &p.human;
            let _ = writeln!(
                out,
                "  {} vs {}: {} item(s), {} unjudged. Human: {} prefer {}, {} prefer {}, {} tie, {} not enough information. {}{}",
                p.x,
                p.y,
                p.items,
                p.unjudged,
                h.wins_a,
                p.x,
                h.wins_b,
                p.y,
                h.ties,
                h.insufficient,
                h.preference_a.map_or(String::new(), |r| format!(
                    "Preference for {}: {:.0}%{}. ",
                    p.x,
                    r * 100.0,
                    h.interval.map_or(String::new(), |(lo, hi)| format!(
                        " (bootstrap 95% interval {:.0}%-{:.0}%, seeded)",
                        lo * 100.0,
                        hi * 100.0
                    ))
                )),
                h.note
            );
            let _ = writeln!(
                out,
                "      bias checks: the longer answer won {}; the answer shown first won {}",
                p.longer_preferred.render(),
                p.first_shown_preferred.render()
            );
            if let Some(ai) = &p.ai {
                let _ = writeln!(
                    out,
                    "      AI-assisted judgments (labelled, not ground truth, not merged with the above): {} prefer {}, {} prefer {}, {} tie. {}",
                    ai.wins_a, p.x, ai.wins_b, p.y, ai.ties, ai.note
                );
            }
        }
        out.push('\n');
    }
    for n in &run.notes {
        let _ = writeln!(out, "note: {n}");
    }
    out.push_str("Limits: checks are keyword-based and can miss a correct paraphrase; citation support is shown only as a vocabulary overlap; nothing here shows that one approach gives better answers in general, and no cost, token or money saving is claimed.\n");
    out
}

/// Two runs side by side, per arm. Refuses to blend runs of different datasets or provenance.
pub fn compare(a: &EvalRun, b: &EvalRun) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "A: {} ({} v{}, {:?})\nB: {} ({} v{}, {:?})",
        a.id,
        a.dataset_id,
        a.dataset_version,
        a.provenance_classes()
            .iter()
            .map(|p| p.as_str())
            .collect::<Vec<_>>(),
        b.id,
        b.dataset_id,
        b.dataset_version,
        b.provenance_classes()
            .iter()
            .map(|p| p.as_str())
            .collect::<Vec<_>>()
    );
    if a.dataset_hash != b.dataset_hash {
        let _ = writeln!(
            out,
            "WARNING: the two runs used different datasets or versions ({} vs {}); differences below may come from the cases, not the arms.",
            a.dataset_hash, b.dataset_hash
        );
    }
    if a.provenance_classes() != b.provenance_classes() {
        out.push_str("WARNING: the two runs contain different kinds of results (fixture / mock / recorded / live). They are shown side by side and must not be read as one measurement.\n");
    }
    let (sa, sb) = (summarize(a), summarize(b));
    let mut arms: Vec<(Arm, Provenance)> = sa
        .iter()
        .chain(&sb)
        .map(|s| (s.arm.clone(), s.provenance))
        .collect();
    arms.sort();
    arms.dedup();
    let _ = writeln!(
        out,
        "\n{:<16} {:<13} {:>26} {:>26}",
        "arm", "kind", "A: objective checks", "B: objective checks"
    );
    for (arm, prov) in arms {
        let cell = |ss: &[ArmSummary]| {
            ss.iter()
                .find(|s| s.arm == arm && s.provenance == prov)
                .map_or("-".to_string(), |s| s.objective_correct.render())
        };
        let _ = writeln!(
            out,
            "{:<16} {:<13} {:>26} {:>26}",
            arm.0,
            prov.as_str(),
            cell(&sa),
            cell(&sb)
        );
    }
    out.push_str("\nA difference between two runs of a stochastic system includes run-to-run variation; with one run each it cannot be separated from a real difference.\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::dataset::{Dataset, builtin};
    use crate::eval::run::run_offline;

    fn run() -> EvalRun {
        run_offline(&Dataset::parse(builtin("starter").unwrap()).unwrap(), None).unwrap()
    }

    #[test]
    fn a_fixture_report_says_up_front_that_it_describes_the_fixtures() {
        let text = render(&run(), 1);
        let first = text.lines().take(10).collect::<Vec<_>>().join("\n");
        assert!(first.contains("fixture"), "{first}");
        assert!(text.contains("READ THIS FIRST") && text.contains("not a model"));
        assert!(text.contains("no cost, token or money saving is claimed"));
    }

    #[test]
    fn summaries_count_what_the_checks_found() {
        let summaries = summarize(&run());
        let codex = summaries
            .iter()
            .find(|s| s.arm.0 == "single:codex")
            .unwrap();
        let thorough = summaries.iter().find(|s| s.arm.0 == "thorough").unwrap();
        assert!(
            codex.objective_correct.n >= 8
                && thorough.objective_correct.n == codex.objective_correct.n
        );
        assert!(
            thorough.objective_correct.k >= codex.objective_correct.k,
            "{:?} {:?}",
            thorough.objective_correct,
            codex.objective_correct
        );
        // The planted flaws are found: a link outside the source list, verification wording, an unknown id.
        let claude = summaries
            .iter()
            .find(|s| s.arm.0 == "single:claude")
            .unwrap();
        assert!(claude.unlisted_links >= 1, "{claude:?}");
        assert!(
            thorough.unbacked_verification_wording >= 1
                || thorough.citation_validity.rate.unwrap_or(1.0) < 1.0,
            "{thorough:?}"
        );
        assert!(codex.failed_or_partial.k == 0);
    }

    #[test]
    fn no_efficiency_claim_is_made_without_recorded_calls_or_enough_cases() {
        let trade = tradeoffs(&run());
        assert!(!trade.is_empty());
        for t in &trade {
            assert!(
                t.contains("not recorded for both") || t.contains("Only"),
                "{t}"
            );
            assert!(!t.contains("used fewer agent calls"), "{t}");
        }
    }

    #[test]
    fn a_saving_is_stated_only_with_recorded_calls_enough_cases_and_no_loss_of_checks() {
        // Build a run with 12 shared cases where `balanced` is as correct as `thorough` and cheaper.
        let mut r = run();
        r.cases = (0..12)
            .map(|i| {
                let mut c = r.cases[0].clone();
                c.case_id = format!("synthetic-{i}");
                for a in &mut c.arms {
                    a.invocations = Some(match a.arm.0.as_str() {
                        "balanced" => 2,
                        "thorough" => 10,
                        _ => 1,
                    });
                    a.evaluation.objective_correct = Some(true);
                }
                c
            })
            .collect();
        let text = tradeoffs(&r).join("\n");
        assert!(
            text.contains("balanced used fewer agent calls than thorough")
                || text.contains("thorough") && text.contains("fewer"),
            "{text}"
        );
        assert!(text.contains("does not carry over"));
        // Make the cheaper one worse: the sentence must disappear.
        for c in &mut r.cases {
            for a in &mut c.arms {
                if a.arm.0 == "balanced" {
                    a.evaluation.objective_correct = Some(false);
                }
            }
        }
        let worse = tradeoffs(&r).join("\n");
        assert!(
            !worse.contains("balanced used fewer agent calls than thorough"),
            "{worse}"
        );
    }

    #[test]
    fn comparing_runs_warns_about_different_datasets_and_kinds() {
        let a = run();
        let mut b = run();
        b.dataset_hash = "other".into();
        b.cases[0].arms[0].provenance = Provenance::Mock;
        let text = compare(&a, &b);
        assert!(
            text.contains("different datasets") && text.contains("different kinds of results"),
            "{text}"
        );
        assert!(text.contains("run-to-run variation"));
    }
}
