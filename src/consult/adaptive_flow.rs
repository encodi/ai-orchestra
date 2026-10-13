//! The adaptive workflow (strategy `balanced`): one agent answers, and more work is added one
//! step at a time, only while the results call for it.
//!
//! 1. **Evidence** (when research is part of the task): sources are found and retrieved, exactly as
//!    in the fixed panel.
//! 2. **The lead answers** in the final format. If the question alone already calls for a review,
//!    the evidence check and the review run **at the same time** (neither depends on the other);
//!    otherwise the check runs first, because its result decides whether a review is worth a call.
//! 3. **The controller decides** ([`AdaptiveDeliberationController`]): finish, or have one other
//!    agent review the answer. The review ends with a categorical verdict that is only used to
//!    decide whether to look closer.
//! 4. **Bounded escalation**: a material disagreement adds one independent perspective from a third
//!    agent (that never sees the others' work), or, with no agent left, one reconsideration by the
//!    lead. Then the coordinator writes the final answer from everything gathered. A review that
//!    finds nothing to change ends the consultation without a synthesis.
//!
//! Calls: the lead, at most one review, at most one escalation call and at most one synthesis
//! (plus the research calls). Every decision is recorded in the decision trace with its reasons.

use super::*;
use crate::smart::adaptive::{
    AdaptiveDeliberationController, ClaimTally, NextStep, ReviewVerdict, Signals, StopReason,
    parse_verdict,
};

/// Sections of the lead's answer: the final format minus the two that need other assistants'
/// views, which only a synthesis (after a review) can fill in honestly.
fn lead_sections(mode: ConsultMode, brief: bool) -> &'static str {
    if brief {
        return "`## Answer` (exactly the length the question asks for, otherwise a short \
                paragraph at most), `## Assumptions` (one line) and `## Evidence limitations` \
                (one line)";
    }
    match mode {
        ConsultMode::Decision => {
            "`## Answer`, `## Recommendation`, `## Alternatives and criteria`, `## Objective \
             evidence vs. personal preferences`, `## Trade-offs, costs and risks`, `## How \
             priorities change the recommendation`, `## Assumptions` and `## Evidence limitations`"
        }
        ConsultMode::Research | ConsultMode::Debate => {
            "`## Answer`, `## Central question`, `## Established facts`, `## Hypotheses and open \
             questions`, `## Opinions and contested claims`, `## Contradictions`, `## Source \
             references` (only references that appear in the question or the evidence; otherwise \
             say none were provided), `## Assumptions` and `## Evidence limitations` (state that \
             nothing was verified externally, unless it was)"
        }
        ConsultMode::Review => {
            "`## Answer`, `## Recommendation`, `## Evaluation criteria`, `## Strengths and \
             weaknesses`, `## Alternatives compared`, `## Specifications vs. subjective \
             assessments`, `## Outdated or unverified information`, `## Assumptions` and \
             `## Evidence limitations`"
        }
        ConsultMode::Learn => {
            "`## Lesson` (one coherent lesson: a logical progression with analogies and \
             examples), `## Prerequisites`, `## Assumptions` and `## Evidence limitations`"
        }
    }
}

fn lead_prompt(mode: ConsultMode, brief: bool, question: &str) -> String {
    format!(
        "You are the lead assistant for this question. Write your complete answer in Markdown \
         with exactly these sections: {}. Another assistant may review your answer afterwards, so \
         state your reasoning and your assumptions explicitly.\n\n{}\n\n{}{}\n\n{SYNTHESIS_RULES}\n\n\
         {NO_FABRICATION}\n\n{READ_ONLY_NOTE}\n\n<question>\n{question}\n</question>",
        lead_sections(mode, brief),
        stage_a(brief),
        brevity(brief),
        mode_brief(mode)
    )
}

const VERDICT_RULE: &str = "End your review with exactly one final line: `VERDICT: AGREE` \
(nothing to change), `VERDICT: MINOR_ISSUES` (points worth integrating) or \
`VERDICT: MATERIAL_DISAGREEMENT` (you would reach a different conclusion, or the answer has an \
error that changes its recommendation). Choose one. The verdict is your judgement of this one \
answer; it is not a confidence level and agreement alone is not evidence.";

fn targeted_review_prompt(
    mode: ConsultMode,
    brief: bool,
    question: &str,
    lead: &str,
    answer: &str,
) -> String {
    let cap = review_cap(1);
    format!(
        "You are an independent AI assistant asked to review another assistant's answer to a \
         question. Read the answer below and review it critically. (1) Identify mistakes. \
         (2) Challenge unsupported claims. (3) Point out criteria or considerations that are \
         missing. (4) Say specifically what you would change and why. If you find nothing \
         material, say so briefly. Do not rewrite the answer.{}\n\n{VERDICT_RULE}\n\n{}\n\n\
         {NO_FABRICATION}\n\n{READ_ONLY_NOTE}\n\n<question>\n{question}\n</question>\n\n\
         <answer_under_review from=\"{lead}\">\n{}\n</answer_under_review>",
        review_length(brief),
        mode_brief(mode),
        clip(answer, cap)
    )
}

fn reconsider_prompt(
    mode: ConsultMode,
    brief: bool,
    question: &str,
    reviewer: &str,
    own: &str,
    review: &str,
) -> String {
    let cap = review_cap(2);
    format!(
        "You answered the question below, and an independent reviewer ({reviewer}) disagreed \
         materially. Reconsider: which of its points change your mind and why, which of your own \
         points you withdraw, and which disagreements you still hold and why. Do not repeat what \
         is already settled. Agreement alone is not evidence.{}\n\n{NO_FABRICATION}\n\n\
         {READ_ONLY_NOTE}\n\n<question>\n{question}\n</question>\n\n\
         <your_answer>\n{}\n</your_answer>\n\n<review from=\"{reviewer}\">\n{}\n</review>\n\n{}",
        review_length(brief),
        clip(own, cap),
        clip(review, cap),
        mode_brief(mode)
    )
}

/// One input to the final synthesis.
struct Part {
    tag: &'static str,
    from: String,
    note: Option<&'static str>,
    text: String,
}

#[allow(clippy::too_many_arguments)]
fn adaptive_synthesis_prompt(
    mode: ConsultMode,
    brief: bool,
    question: &str,
    roster: &[String],
    parts: &[Part],
) -> String {
    let cap = review_cap(parts.len());
    let mut block = String::new();
    for part in parts {
        let _ = writeln!(
            block,
            "<{} from=\"{}\"{}>\n{}\n</{}>",
            part.tag,
            part.from,
            part.note.map_or(String::new(), |n| format!(" {n}")),
            clip(&part.text, cap),
            part.tag
        );
    }
    format!(
        "You are the coordinator of a small panel of AI assistants. You wrote the first answer; the \
         other input is below. The panel is exactly: {}. Do not mention or speculate about any \
         other assistant. Write the FINAL answer in Markdown with exactly these sections: {}. \
         {SYNTHESIS_RULES} Keep each substantive objection and say whether you resolved it.\n\n{}{}\n\n\
         {NO_FABRICATION}\n\n{READ_ONLY_NOTE}\n\n<question>\n{question}\n</question>\n\n\
         <panel>\n{block}</panel>",
        roster.join(", "),
        sections(mode, brief),
        brevity(brief),
        mode_brief(mode)
    )
}

/// Appended to the lead's answer when the review found nothing to change and no synthesis was
/// needed. Generated here; the reviewer's own words are quoted, never paraphrased as fact.
fn review_note(reviewer: &str, verdict: ReviewVerdict, review: &str) -> String {
    let mut out = format!(
        "\n\n## Independent review\n\n{reviewer} reviewed this answer independently and ended with `VERDICT: {}`. A verdict is one model's judgement of one answer, not proof.\n",
        verdict.as_str()
    );
    let quoted: String = clip(review.trim(), 3_000)
        .lines()
        .filter(|l| !l.trim().to_uppercase().starts_with("VERDICT"))
        .map(|l| format!("> {l}\n"))
        .collect();
    if !quoted.trim().is_empty() {
        out.push('\n');
        out.push_str(&quoted);
    }
    out
}

fn describe(d: &crate::smart::adaptive::Decision) -> String {
    format!(
        "{}{}: {}",
        d.next.as_str(),
        d.stop
            .as_ref()
            .map_or(String::new(), |s| format!(" (stop: {s})")),
        d.reasons.join("; ")
    )
}

impl Orchestrator {
    pub(super) async fn run_adaptive(
        &self,
        p: &Panel<'_>,
        cancel: &CancellationToken,
    ) -> Result<PanelResult> {
        let (Some(smart), Some(plan)) = (p.smart.as_ref(), p.plan.as_ref()) else {
            return Err(OrchestraError::Config(
                "internal error: the adaptive workflow needs a smart plan".into(),
            ));
        };
        let controller = AdaptiveDeliberationController;
        let features = &plan.features;
        let lead = p.providers[0].clone();
        let cancelled = || OrchestraError::Cancelled {
            provider: lead.id().clone(),
        };
        let request = |prompt: String| p.request(prompt);
        let mut warnings: Vec<String> = Vec::new();
        let mut any_failure = false;

        // ---- 1. Evidence.
        let research_on = p.research.lock().await.is_some();
        let (mut evidence, nothing_retrieved) = self.gather_research(p, cancel).await?;
        evidence.memory.clone_from(&p.memory_text);
        any_failure |= nothing_retrieved;

        let signals = |claims: ClaimTally,
                       verification_failed: bool,
                       retrieved: usize,
                       review: Option<ReviewVerdict>,
                       rounds_done: u8,
                       escalations_done: u8,
                       agents_left: usize| Signals {
            mode: p.mode,
            contested_question: features.contested,
            complex_question: features.complex,
            high_stakes: features.high_stakes,
            research_active: research_on,
            retrieved_sources: retrieved,
            claims,
            verification_failed,
            review,
            agents_left,
            rounds_done,
            max_rounds: 2,
            escalations_done,
            calls_left: smart.budget.remaining_calls(),
            time_left: smart.budget.remaining_time(),
        };
        let retrieved_now = |run: &ResearchRun| run.set.retrieved_ids().len();

        // ---- 2. The lead answers.
        smart.trace.event(
            TraceKind::Stage,
            format!("the lead {} answers first", lead.id()),
        );
        let (lead_result, _) = p
            .runner
            .call(
                lead.clone(),
                request(with_evidence(
                    lead_prompt(p.mode, p.brief, p.question),
                    &evidence.for_answer(),
                )),
                0,
                Role::Perspective,
                cancel,
            )
            .await;
        let raw_lead = match lead_result {
            Ok(r) => r.text,
            Err(OrchestraError::Cancelled { .. }) => return Err(cancelled()),
            Err(e) => return Err(e),
        };
        let lead_text = strip_claims_block(&raw_lead);
        let lead_name = lead.id().to_string();

        // ---- 3. The evidence check, alongside the review when the review is certain.
        let evidence_check = !evidence.block.is_empty();
        let other_agents = p.providers.len().saturating_sub(1);
        let mut review_in_flight: Option<Result<crate::types::AgentResponse>> = None;
        if evidence_check {
            let certain = controller.review_regardless_of_evidence(&signals(
                ClaimTally::default(),
                false,
                0,
                None,
                0,
                0,
                other_agents,
            ));
            let analyses = vec![(lead_name.clone(), raw_lead.clone())];
            let mut guard = p.research.lock().await;
            if let Some(run) = guard.as_mut() {
                if certain {
                    smart.trace.event(
                        TraceKind::Stage,
                        "the evidence check and the independent review run at the same time: the question alone already calls for a review, so neither depends on the other's result",
                    );
                    let reviewer = p.providers[1].clone();
                    let review_request = request(with_evidence(
                        targeted_review_prompt(p.mode, p.brief, p.question, &lead_name, &lead_text),
                        &evidence.for_review(),
                    ));
                    let (verified, reviewed) = tokio::join!(
                        run.verify(
                            &p.runner,
                            &analyses,
                            &p.providers,
                            &request,
                            p.question,
                            now_ms(),
                            cancel,
                        ),
                        p.runner
                            .call(reviewer, review_request, 1, Role::Review, cancel),
                    );
                    verified?;
                    review_in_flight = Some(reviewed.0);
                } else {
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
                any_failure |= run.had_failure;
                evidence.table = claims_table(&run.claims, CLAIM_TABLE_MAX_BYTES);
                smart.trace.event(
                    TraceKind::Research,
                    format!(
                        "{} claim(s) checked: {}",
                        run.claims.len(),
                        run.claims
                            .iter()
                            .map(|c| c.status.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                );
            }
        }
        let (tally, verification_failed, retrieved) = {
            let guard = p.research.lock().await;
            match guard.as_ref() {
                Some(run) => (
                    ClaimTally::from_claims(&run.claims),
                    run.had_failure && evidence_check,
                    retrieved_now(run),
                ),
                None => (ClaimTally::default(), false, 0),
            }
        };

        // ---- 4. The first decision.
        let first = controller.after_initial(&signals(
            tally.clone(),
            verification_failed,
            retrieved,
            None,
            0,
            0,
            other_agents,
        ));
        smart.trace.event(
            TraceKind::Decision,
            format!("after the lead's answer: {}", describe(&first)),
        );
        if first.next != NextStep::Review && review_in_flight.is_none() {
            let stop = first.stop.clone().unwrap_or(StopReason::Sufficient(
                "nothing called for more".to_string(),
            ));
            smart.set_stop(stop.to_string());
            warnings.push(format!("no independent review took place: {stop}"));
            let budget_stop = matches!(stop, StopReason::Budget(_));
            let mut final_answer = lead_text.clone();
            if research_on {
                let mut guard = p.research.lock().await;
                if let Some(run) = guard.as_mut() {
                    final_answer = run.finalize_answer(&raw_lead);
                    any_failure |= run.had_failure;
                }
            }
            return Ok(PanelResult {
                synthesized: true,
                initial: lead_text,
                final_answer,
                warnings,
                any_failure,
                plan_met: !budget_stop,
                single_agent: true,
            });
        }

        // ---- 5. The independent review.
        let reviewer = p.providers[1].clone();
        let reviewer_name = reviewer.id().to_string();
        let review_result = match review_in_flight {
            Some(r) => r,
            None => {
                smart.trace.event(
                    TraceKind::Escalation,
                    format!("{reviewer_name} reviews the answer independently"),
                );
                p.runner
                    .call(
                        reviewer.clone(),
                        request(with_evidence(
                            targeted_review_prompt(
                                p.mode, p.brief, p.question, &lead_name, &lead_text,
                            ),
                            &evidence.for_review(),
                        )),
                        1,
                        Role::Review,
                        cancel,
                    )
                    .await
                    .0
            }
        };
        let review_text = match review_result {
            Ok(r) => r.text,
            Err(OrchestraError::Cancelled { .. }) => return Err(cancelled()),
            Err(e) => {
                any_failure = true;
                warnings.push(format!(
                    "the independent review by {reviewer_name} failed: {}; the answer shown is the lead's own and has not been reviewed",
                    first_line(&e.to_string())
                ));
                smart.set_stop("the independent review could not be obtained");
                let mut final_answer = lead_text.clone();
                if research_on {
                    let mut guard = p.research.lock().await;
                    if let Some(run) = guard.as_mut() {
                        final_answer = run.finalize_answer(&raw_lead);
                    }
                }
                return Ok(PanelResult {
                    synthesized: true,
                    initial: lead_text,
                    final_answer,
                    warnings,
                    any_failure,
                    plan_met: false,
                    single_agent: true,
                });
            }
        };
        let verdict = parse_verdict(&review_text);
        let mut rounds_run = 1u8;
        let mut agents_asked = 2usize; // the lead and the reviewer
        let mut escalations = 0u8;
        smart.trace.event(
            TraceKind::Decision,
            format!("{reviewer_name} reviewed the answer: {}", verdict.as_str()),
        );
        let mut decision = controller.after_review(&signals(
            tally.clone(),
            verification_failed,
            retrieved,
            Some(verdict),
            rounds_run,
            escalations,
            p.providers.len().saturating_sub(agents_asked),
        ));
        smart.trace.event(
            TraceKind::Decision,
            format!("after the review: {}", describe(&decision)),
        );

        let mut parts = vec![
            Part {
                tag: "perspective",
                from: lead_name.clone(),
                note: Some("role=\"lead answer\""),
                text: lead_text.clone(),
            },
            Part {
                tag: "review",
                from: reviewer_name.clone(),
                note: None,
                text: review_text.clone(),
            },
        ];

        // ---- 6. Bounded escalation.
        let mut stop_note = decision.stop.clone();
        match decision.next {
            NextStep::IndependentPerspective => {
                let third = p.providers[agents_asked].clone();
                let third_name = third.id().to_string();
                agents_asked += 1;
                escalations = 1;
                smart.trace.event(
                    TraceKind::Escalation,
                    format!("{third_name} gives an independent perspective, without seeing the others' work"),
                );
                let (res, _) = p
                    .runner
                    .call(
                        third,
                        request(with_evidence(
                            perspective_prompt(p.mode, p.brief, p.question),
                            &evidence.for_answer(),
                        )),
                        1,
                        Role::Perspective,
                        cancel,
                    )
                    .await;
                match res {
                    Ok(r) => {
                        if evidence_check {
                            let mut guard = p.research.lock().await;
                            if let Some(run) = guard.as_mut() {
                                if smart.budget.can_afford(1) {
                                    run.verify_additional(
                                        &p.runner,
                                        &[(third_name.clone(), r.text.clone())],
                                        &p.providers,
                                        &request,
                                        p.question,
                                        now_ms(),
                                        cancel,
                                    )
                                    .await?;
                                    any_failure |= run.had_failure;
                                    evidence.table =
                                        claims_table(&run.claims, CLAIM_TABLE_MAX_BYTES);
                                    if run.budget.claims_reused > 0 {
                                        smart.trace.event(
                                            TraceKind::Reuse,
                                            format!(
                                                "{} claim(s) the third agent shares with the first were not judged again",
                                                run.budget.claims_reused
                                            ),
                                        );
                                    }
                                } else {
                                    run.limitations.push(format!(
                                        "the claims {third_name} relied on were not checked: no call or time was left"
                                    ));
                                }
                            }
                        }
                        parts.push(Part {
                            tag: "perspective",
                            from: third_name,
                            note: Some("role=\"independent perspective\""),
                            text: strip_claims_block(&r.text),
                        });
                    }
                    Err(OrchestraError::Cancelled { .. }) => return Err(cancelled()),
                    Err(e) => {
                        any_failure = true;
                        warnings.push(format!(
                            "the third perspective by {third_name} failed: {}",
                            first_line(&e.to_string())
                        ));
                    }
                }
            }
            NextStep::Reconsider => {
                escalations = 1;
                rounds_run = 2;
                smart.trace.event(
                    TraceKind::Escalation,
                    format!("{lead_name} reconsiders after reading the review"),
                );
                let (res, _) = p
                    .runner
                    .call(
                        lead.clone(),
                        request(reconsider_prompt(
                            p.mode,
                            p.brief,
                            p.question,
                            &reviewer_name,
                            &lead_text,
                            &review_text,
                        )),
                        2,
                        Role::CrossReview,
                        cancel,
                    )
                    .await;
                match res {
                    Ok(r) => parts.push(Part {
                        tag: "reconsideration",
                        from: lead_name.clone(),
                        note: None,
                        text: r.text,
                    }),
                    Err(OrchestraError::Cancelled { .. }) => return Err(cancelled()),
                    Err(e) => {
                        any_failure = true;
                        warnings.push(format!(
                            "the reconsideration by {lead_name} failed: {}",
                            first_line(&e.to_string())
                        ));
                    }
                }
            }
            _ => {}
        }
        if escalations > 0 {
            let (tally_now, retrieved_after) = {
                let guard = p.research.lock().await;
                guard.as_ref().map_or((tally.clone(), retrieved), |run| {
                    (ClaimTally::from_claims(&run.claims), retrieved_now(run))
                })
            };
            decision = controller.after_escalation(&signals(
                tally_now,
                verification_failed,
                retrieved_after,
                Some(verdict),
                rounds_run,
                escalations,
                p.providers.len().saturating_sub(agents_asked),
            ));
            stop_note = decision.stop.clone();
            smart.trace.event(
                TraceKind::Decision,
                format!("after the escalation: {}", describe(&decision)),
            );
        }

        // ---- 7. The final answer.
        let mut synthesized = true;
        let mut final_text = lead_text.clone();
        let mut plan_met = true;
        match decision.next {
            NextStep::Finish => {
                final_text.push_str(&review_note(&reviewer_name, verdict, &review_text));
                if let Some(stop) = &stop_note {
                    plan_met = !matches!(stop, StopReason::Budget(_));
                    smart.set_stop(stop.to_string());
                    if !plan_met {
                        warnings.push(format!("the review could not be integrated: {stop}"));
                    }
                }
            }
            _ => {
                let roster: Vec<String> = p
                    .providers
                    .iter()
                    .take(agents_asked)
                    .map(|a| a.id().to_string())
                    .collect();
                let prompt = with_evidence(
                    adaptive_synthesis_prompt(p.mode, p.brief, p.question, &roster, &parts),
                    &evidence.for_synthesis(),
                );
                smart.trace.event(
                    TraceKind::Stage,
                    format!(
                        "{lead_name} writes the final answer from {} input(s)",
                        parts.len()
                    ),
                );
                let (res, _) = p
                    .runner
                    .call(
                        lead.clone(),
                        request(prompt),
                        rounds_run,
                        Role::Synthesis,
                        cancel,
                    )
                    .await;
                match res {
                    Ok(r) => final_text = r.text,
                    Err(OrchestraError::Cancelled { .. }) => return Err(cancelled()),
                    Err(e) => {
                        any_failure = true;
                        synthesized = false;
                        warnings.push(format!(
                            "the coordinator {lead_name} failed to write the synthesis: {}; the answer shown is its own first answer, and the review is in the transcript",
                            first_line(&e.to_string())
                        ));
                    }
                }
                let why = stop_note.as_ref().map_or_else(
                    || {
                        "the review was integrated; nothing called for a further perspective"
                            .to_string()
                    },
                    ToString::to_string,
                );
                smart.set_stop(why);
            }
        }
        let mut final_answer = final_text;
        if research_on {
            let mut guard = p.research.lock().await;
            if let Some(run) = guard.as_mut() {
                final_answer = run.finalize_answer(&final_answer);
                any_failure |= run.had_failure;
            }
        }
        Ok(PanelResult {
            synthesized,
            initial: lead_text,
            final_answer,
            warnings,
            any_failure,
            plan_met,
            single_agent: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lead_prompt_asks_for_the_final_format_without_the_review_dependent_sections() {
        for mode in [
            ConsultMode::Decision,
            ConsultMode::Research,
            ConsultMode::Review,
            ConsultMode::Learn,
        ] {
            for brief in [false, true] {
                let s = lead_sections(mode, brief);
                assert!(
                    !s.contains("Areas of agreement") && !s.contains("Significant disagreements"),
                    "{mode} {brief}: {s}"
                );
                assert!(!s.contains("Corrections made after review"));
                assert!(s.contains("## Assumptions") && s.contains("## Evidence limitations"));
            }
        }
        let p = lead_prompt(ConsultMode::Decision, false, "Q?");
        assert!(
            p.contains("You are the lead assistant") && p.contains("<question>\nQ?\n</question>")
        );
        assert!(
            !p.contains("Write the FINAL answer"),
            "that wording belongs to the synthesis"
        );
    }

    #[test]
    fn the_review_prompt_demands_a_verdict_and_wraps_the_answer_as_data() {
        let p = targeted_review_prompt(ConsultMode::Review, false, "Q?", "lead", "the answer");
        assert!(
            p.contains("VERDICT: AGREE")
                && p.contains("VERDICT: MINOR_ISSUES")
                && p.contains("VERDICT: MATERIAL_DISAGREEMENT")
        );
        assert!(
            p.contains("<answer_under_review from=\"lead\">\nthe answer\n</answer_under_review>")
        );
        assert!(p.contains("not a confidence level"));
    }

    #[test]
    fn the_synthesis_prompt_names_exactly_the_agents_that_took_part() {
        let parts = vec![
            Part {
                tag: "perspective",
                from: "lead".into(),
                note: Some("role=\"lead answer\""),
                text: "A".into(),
            },
            Part {
                tag: "review",
                from: "beta".into(),
                note: None,
                text: "R".into(),
            },
        ];
        let p = adaptive_synthesis_prompt(
            ConsultMode::Decision,
            false,
            "Q?",
            &["lead".into(), "beta".into()],
            &parts,
        );
        assert!(p.contains("The panel is exactly: lead, beta."));
        assert!(p.contains("Write the FINAL answer"));
        assert!(p.contains("<perspective from=\"lead\" role=\"lead answer\">\nA\n</perspective>"));
        assert!(p.contains("<review from=\"beta\">\nR\n</review>"));
        assert!(
            p.contains("## Areas of agreement"),
            "the synthesis has the full section list"
        );
    }

    #[test]
    fn the_review_note_quotes_the_reviewer_and_drops_the_verdict_line() {
        let note = review_note(
            "beta",
            ReviewVerdict::Agree,
            "Looks right.\nOne nit: units.\nVERDICT: AGREE",
        );
        assert!(note.starts_with("\n\n## Independent review"));
        assert!(note.contains("`VERDICT: AGREE`") && note.contains("not proof"));
        assert!(note.contains("> Looks right.") && note.contains("> One nit: units."));
        assert_eq!(
            note.matches("VERDICT").count(),
            1,
            "only the generated mention remains"
        );
    }
}
