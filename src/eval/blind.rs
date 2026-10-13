//! Blind pairwise comparison.
//!
//! For each case and each requested pair of arms, the two responses are shown as "answer 1" and
//! "answer 2" in an order drawn from a seeded generator, so the order is reproducible but not guessable,
//! and the judge sees neither the provider nor the strategy. The same criteria are shown to both. The
//! judge records a preference, a tie or "not enough information to say", with a reason. Judgments are
//! appended to the run, never edited: a later judgment by the same evaluator supersedes the earlier
//! for the statistics, and both stay in the file for audit. The identity of each answer is revealed
//! only by `eval report`.
//!
//! What blinding does not do: answers may still betray their source by style, and a longer or more
//! polished answer tends to win regardless of quality. The report therefore shows how often the longer
//! answer won and how often the first-shown answer won, next to the preference rate.

use serde::{Deserialize, Serialize};

use super::dataset::Arm;
use super::run::{Choice, EvalRun, Evaluator, EvaluatorKind, Judgment};
use super::stats::{self, Preference, SplitMix64, hash64};
use crate::error::{OrchestraError, Result};
use crate::history::now_ms;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlindItem {
    pub item_id: String,
    pub case_id: String,
    /// The pair as it was requested; the order here is not the display order.
    pub x: Arm,
    pub y: Arm,
    /// Which of the two was shown first.
    pub first_shown: Arm,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlindState {
    pub seed: u64,
    pub pairs: Vec<(Arm, Arm)>,
    pub items: Vec<BlindItem>,
}

/// What the judge is shown. No arm, provider or strategy appears in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PacketItem {
    pub item_id: String,
    pub case_id: String,
    pub question: String,
    pub instructions: String,
    pub first: PacketAnswer,
    pub second: PacketAnswer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PacketAnswer {
    pub text: String,
    pub words: usize,
}

/// Builds the blind items for `pairs`. Reproducible for the same run, pairs and seed. Responses of
/// different provenance classes are never paired, and a failed response is not compared.
pub fn prepare(run: &mut EvalRun, pairs: &[(Arm, Arm)], seed: u64) -> Result<usize> {
    if !run.judgments.is_empty() {
        return Err(OrchestraError::Config(
            "this run already has judgments; prepare the comparison before judging, or evaluate again to start a new run".into(),
        ));
    }
    if pairs.is_empty() {
        return Err(OrchestraError::Config(
            "name at least one pair of arms to compare".into(),
        ));
    }
    let mut items: Vec<BlindItem> = Vec::new();
    for case in &run.cases {
        for (x, y) in pairs {
            if x == y {
                return Err(OrchestraError::Config(format!(
                    "`{x}` cannot be compared with itself"
                )));
            }
            let (Some(rx), Some(ry)) = (
                case.arms.iter().find(|a| &a.arm == x),
                case.arms.iter().find(|a| &a.arm == y),
            ) else {
                continue;
            };
            if rx.provenance != ry.provenance || rx.status == "failed" || ry.status == "failed" {
                continue;
            }
            let mut rng = SplitMix64::new(seed ^ hash64(&format!("{}|{}|{}", case.case_id, x, y)));
            let first_shown = if rng.below(2) == 0 {
                x.clone()
            } else {
                y.clone()
            };
            items.push(BlindItem {
                item_id: String::new(),
                case_id: case.case_id.clone(),
                x: x.clone(),
                y: y.clone(),
                first_shown,
            });
        }
    }
    if items.is_empty() {
        return Err(OrchestraError::Config(
            "no case has a usable response from both arms of a pair (same provenance, not failed)"
                .into(),
        ));
    }
    // The order the judge meets the items in is shuffled too, so it does not follow the dataset.
    SplitMix64::new(seed).shuffle(&mut items);
    for (i, item) in items.iter_mut().enumerate() {
        item.item_id = format!("item-{:03}", i + 1);
    }
    let n = items.len();
    run.blind = Some(BlindState {
        seed,
        pairs: pairs.to_vec(),
        items,
    });
    Ok(n)
}

pub fn packet(run: &EvalRun) -> Result<Vec<PacketItem>> {
    let state = run.blind.as_ref().ok_or_else(|| {
        OrchestraError::Config(
            "no blind comparison is prepared for this run (use `eval blind`)".into(),
        )
    })?;
    let mut out = Vec::new();
    for item in &state.items {
        let case = run
            .cases
            .iter()
            .find(|c| c.case_id == item.case_id)
            .ok_or_else(|| OrchestraError::Config("the run is inconsistent".into()))?;
        let second_arm = if item.first_shown == item.x {
            &item.y
        } else {
            &item.x
        };
        let get = |arm: &Arm| case.arms.iter().find(|a| &a.arm == arm);
        let (Some(first), Some(second)) = (get(&item.first_shown), get(second_arm)) else {
            continue;
        };
        out.push(PacketItem {
            item_id: item.item_id.clone(),
            case_id: item.case_id.clone(),
            question: case.question.clone(),
            instructions: case.instructions.clone(),
            first: PacketAnswer {
                text: first.answer.clone(),
                words: first.evaluation.answer_words,
            },
            second: PacketAnswer {
                text: second.answer.clone(),
                words: second.evaluation.answer_words,
            },
        });
    }
    Ok(out)
}

/// Records one judgment. An AI evaluator is accepted only when it is not one of the providers that
/// produced either answer, and its judgment is labelled as such in every report.
pub fn record(
    run: &mut EvalRun,
    item_id: &str,
    choice: Choice,
    evaluator: Evaluator,
    explanation: &str,
) -> Result<()> {
    let state = run.blind.as_ref().ok_or_else(|| {
        OrchestraError::Config("no blind comparison is prepared for this run".into())
    })?;
    let item = state
        .items
        .iter()
        .find(|i| i.item_id == item_id)
        .ok_or_else(|| {
            OrchestraError::Config(format!(
                "no item `{item_id}` in this run (see `eval blind`)"
            ))
        })?
        .clone();
    if evaluator.name.trim().is_empty() {
        return Err(OrchestraError::Config("the evaluator needs a name".into()));
    }
    if evaluator.kind == EvaluatorKind::Ai {
        let case = run.cases.iter().find(|c| c.case_id == item.case_id);
        let involved: Vec<String> = [&item.x, &item.y]
            .iter()
            .filter_map(|arm| case.and_then(|c| c.arms.iter().find(|a| &a.arm == *arm)))
            .flat_map(|a| {
                a.agents
                    .iter()
                    .cloned()
                    .chain(a.arm.single_agent().map(str::to_string))
            })
            .map(|a| a.to_lowercase())
            .collect();
        if involved.contains(&evaluator.name.trim().to_lowercase()) {
            return Err(OrchestraError::Config(format!(
                "`{}` produced one of these answers and cannot be the judge of it",
                evaluator.name
            )));
        }
    }
    run.judgments.push(Judgment {
        item_id: item.item_id,
        choice,
        evaluator: Evaluator {
            kind: evaluator.kind,
            name: evaluator.name.trim().to_string(),
        },
        explanation: explanation.trim().chars().take(2000).collect(),
        recorded_ms: now_ms(),
    });
    Ok(())
}

/// The effective judgment per (item, evaluator): the latest one.
fn effective(run: &EvalRun) -> Vec<&Judgment> {
    let mut out: Vec<&Judgment> = Vec::new();
    for j in &run.judgments {
        out.retain(|k| !(k.item_id == j.item_id && k.evaluator == j.evaluator));
        out.push(j);
    }
    out
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PairReport {
    pub x: Arm,
    pub y: Arm,
    pub items: usize,
    pub unjudged: usize,
    pub human: stats::PairwiseSummary,
    /// Present when an AI evaluator judged; never merged with the human summary.
    pub ai: Option<stats::PairwiseSummary>,
    /// Among decided human judgments where the answers differ in length by 10% or more: the longer won.
    pub longer_preferred: stats::Rate,
    /// Among decided human judgments (no ties): the answer shown first won.
    pub first_shown_preferred: stats::Rate,
}

fn relative_to_x(item: &BlindItem, choice: Choice) -> Preference {
    match choice {
        Choice::Tie => Preference::Tie,
        Choice::Insufficient => Preference::Insufficient,
        Choice::First | Choice::Second => {
            let chosen = if choice == Choice::First {
                &item.first_shown
            } else if item.first_shown == item.x {
                &item.y
            } else {
                &item.x
            };
            if chosen == &item.x {
                Preference::A
            } else {
                Preference::B
            }
        }
    }
}

pub fn results(run: &EvalRun, seed: u64) -> Vec<PairReport> {
    let Some(state) = &run.blind else {
        return Vec::new();
    };
    let judgments = effective(run);
    let mut reports = Vec::new();
    for (x, y) in &state.pairs {
        let items: Vec<&BlindItem> = state
            .items
            .iter()
            .filter(|i| &i.x == x && &i.y == y)
            .collect();
        let mut human = Vec::new();
        let mut ai = Vec::new();
        let mut longer = (0usize, 0usize);
        let mut first = (0usize, 0usize);
        let mut judged_items = std::collections::BTreeSet::new();
        for item in &items {
            for j in judgments.iter().filter(|j| j.item_id == item.item_id) {
                judged_items.insert(item.item_id.clone());
                let pref = relative_to_x(item, j.choice);
                match j.evaluator.kind {
                    EvaluatorKind::Human => {
                        human.push(pref);
                        if matches!(j.choice, Choice::First | Choice::Second) {
                            first.1 += 1;
                            if j.choice == Choice::First {
                                first.0 += 1;
                            }
                            let words = |arm: &Arm| {
                                run.arm_result(&item.case_id, arm)
                                    .map_or(0, |a| a.evaluation.answer_words)
                            };
                            let (wx, wy) = (words(&item.x), words(&item.y));
                            let (lo, hi) = (wx.min(wy) as f64, wx.max(wy) as f64);
                            if hi > 0.0 && (hi - lo) / hi >= 0.10 {
                                longer.1 += 1;
                                let winner_words = if pref == Preference::A { wx } else { wy };
                                if winner_words as f64 == hi {
                                    longer.0 += 1;
                                }
                            }
                        }
                    }
                    EvaluatorKind::Ai => ai.push(pref),
                }
            }
        }
        let pair_seed = seed ^ hash64(&format!("{x}|{y}"));
        reports.push(PairReport {
            x: x.clone(),
            y: y.clone(),
            items: items.len(),
            unjudged: items.len() - judged_items.len(),
            human: stats::pairwise_summary(&human, pair_seed),
            ai: (!ai.is_empty()).then(|| stats::pairwise_summary(&ai, pair_seed)),
            longer_preferred: stats::Rate::new(longer.0, longer.1),
            first_shown_preferred: stats::Rate::new(first.0, first.1),
        });
    }
    reports
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::dataset::{Dataset, builtin};
    use crate::eval::run::run_offline;

    fn run() -> EvalRun {
        run_offline(&Dataset::parse(builtin("starter").unwrap()).unwrap(), None).unwrap()
    }

    fn pair() -> Vec<(Arm, Arm)> {
        vec![(Arm("single:codex".into()), Arm("thorough".into()))]
    }

    fn human() -> Evaluator {
        Evaluator {
            kind: EvaluatorKind::Human,
            name: "ido".into(),
        }
    }

    #[test]
    fn the_same_seed_gives_the_same_order_and_another_seed_a_different_one() {
        let (mut a, mut b, mut c) = (run(), run(), run());
        let n = prepare(&mut a, &pair(), 7).unwrap();
        prepare(&mut b, &pair(), 7).unwrap();
        prepare(&mut c, &pair(), 8).unwrap();
        assert!(n >= 2);
        assert_eq!(a.blind, b.blind);
        assert_ne!(
            a.blind.as_ref().unwrap().items,
            c.blind.as_ref().unwrap().items
        );
        // The display order really varies across items (not always x first).
        let items = &a.blind.as_ref().unwrap().items;
        assert!(
            items.iter().any(|i| i.first_shown == i.x)
                && items.iter().any(|i| i.first_shown == i.y),
            "{items:?}"
        );
    }

    #[test]
    fn the_packet_hides_the_arms_and_lists_the_same_criteria_for_both_answers() {
        let mut r = run();
        prepare(&mut r, &pair(), 1).unwrap();
        let packet = packet(&r).unwrap();
        let text = serde_json::to_string(&packet).unwrap();
        for hidden in ["single:codex", "thorough", "\"arm\"", "provenance"] {
            assert!(!text.contains(hidden), "the packet leaks `{hidden}`");
        }
        assert!(packet.iter().all(|p| !p.question.is_empty()
            && !p.instructions.is_empty()
            && p.first.words > 0
            && p.second.words > 0));
        assert!(
            packet.iter().all(|p| p.item_id.starts_with("item-")),
            "ids do not reveal the pair"
        );
    }

    #[test]
    fn a_judgment_maps_back_to_the_right_arm_whatever_the_display_order() {
        let mut r = run();
        prepare(&mut r, &pair(), 3).unwrap();
        let items = r.blind.as_ref().unwrap().items.clone();
        // Always prefer the answer that is really `x`, however it was shown.
        for item in &items {
            let choice = if item.first_shown == item.x {
                Choice::First
            } else {
                Choice::Second
            };
            record(&mut r, &item.item_id, choice, human(), "x is better").unwrap();
        }
        let rep = &results(&r, 1)[0];
        assert_eq!((rep.human.wins_a, rep.human.wins_b), (items.len(), 0));
        assert_eq!(rep.unjudged, 0);
        // A tie and an "insufficient" are recorded as such.
        let mut r2 = run();
        prepare(&mut r2, &pair(), 3).unwrap();
        let ids: Vec<String> = r2
            .blind
            .as_ref()
            .unwrap()
            .items
            .iter()
            .map(|i| i.item_id.clone())
            .collect();
        record(&mut r2, &ids[0], Choice::Tie, human(), "equal").unwrap();
        record(
            &mut r2,
            &ids[1],
            Choice::Insufficient,
            human(),
            "cannot tell",
        )
        .unwrap();
        let rep = &results(&r2, 1)[0];
        assert_eq!(
            (
                rep.human.ties,
                rep.human.insufficient,
                rep.human.wins_a + rep.human.wins_b
            ),
            (1, 1, 0)
        );
    }

    #[test]
    fn a_later_judgment_supersedes_the_earlier_one_but_both_stay_in_the_record() {
        let mut r = run();
        prepare(&mut r, &pair(), 3).unwrap();
        let item = r.blind.as_ref().unwrap().items[0].clone();
        record(&mut r, &item.item_id, Choice::First, human(), "first").unwrap();
        record(
            &mut r,
            &item.item_id,
            Choice::Tie,
            human(),
            "on reflection, equal",
        )
        .unwrap();
        assert_eq!(r.judgments.len(), 2, "audit trail");
        assert_eq!(results(&r, 1)[0].human.ties, 1);
        assert_eq!(
            results(&r, 1)[0].human.wins_a + results(&r, 1)[0].human.wins_b,
            0
        );
    }

    #[test]
    fn a_model_cannot_judge_its_own_answer_and_ai_judgments_stay_separate() {
        let mut r = run();
        prepare(&mut r, &pair(), 3).unwrap();
        let id = r.blind.as_ref().unwrap().items[0].item_id.clone();
        let ai = |name: &str| Evaluator {
            kind: EvaluatorKind::Ai,
            name: name.into(),
        };
        assert!(
            record(&mut r, &id, Choice::First, ai("codex"), "mine")
                .unwrap_err()
                .to_string()
                .contains("cannot be the judge")
        );
        record(
            &mut r,
            &id,
            Choice::First,
            ai("outside-judge"),
            "by an outside model",
        )
        .unwrap();
        assert!(
            record(&mut r, &id, Choice::First, ai("Google"), "mine too").is_err(),
            "case-insensitive"
        );
        record(&mut r, &id, Choice::Second, human(), "by a person").unwrap();
        let rep = &results(&r, 1)[0];
        assert_eq!(rep.human.judged, 1);
        assert_eq!(
            rep.ai.as_ref().unwrap().judged,
            1,
            "never merged with the human summary"
        );
        assert!(record(&mut r, "item-999", Choice::Tie, human(), "").is_err());
        assert!(
            record(
                &mut r,
                &id,
                Choice::Tie,
                Evaluator {
                    kind: EvaluatorKind::Human,
                    name: " ".into()
                },
                ""
            )
            .is_err()
        );
    }

    #[test]
    fn length_and_position_bias_are_measured_next_to_the_preference() {
        let mut r = run();
        prepare(&mut r, &pair(), 5).unwrap();
        let items = r.blind.as_ref().unwrap().items.clone();
        for item in &items {
            // Always pick whichever answer is longer.
            let words = |arm: &Arm| {
                r.arm_result(&item.case_id, arm)
                    .unwrap()
                    .evaluation
                    .answer_words
            };
            let longer_is_x = words(&item.x) >= words(&item.y);
            let x_first = item.first_shown == item.x;
            let choice = if longer_is_x == x_first {
                Choice::First
            } else {
                Choice::Second
            };
            record(&mut r, &item.item_id, choice, human(), "longer").unwrap();
        }
        let rep = &results(&r, 1)[0];
        if rep.longer_preferred.n > 0 {
            assert_eq!(
                rep.longer_preferred.k, rep.longer_preferred.n,
                "{:?}",
                rep.longer_preferred
            );
        }
        assert!(rep.first_shown_preferred.n > 0);
    }

    #[test]
    fn preparing_is_refused_after_judging_and_without_a_usable_pair() {
        let mut r = run();
        assert!(prepare(&mut r, &[], 1).is_err());
        assert!(prepare(&mut r, &[(Arm("fast".into()), Arm("fast".into()))], 1).is_err());
        prepare(&mut r, &pair(), 1).unwrap();
        let id = r.blind.as_ref().unwrap().items[0].item_id.clone();
        record(&mut r, &id, Choice::Tie, human(), "").unwrap();
        assert!(prepare(&mut r, &pair(), 2).is_err());
        assert!(packet(&run()).is_err(), "no blind comparison prepared");
    }
}
