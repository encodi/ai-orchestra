//! How a finished consultation is shown.
//!
//! The coordinator's answer is Markdown with the sections its mode asks for. The default view puts
//! the useful ones first, in the order a reader needs them for that mode, and leaves out the
//! orchestration log; the detailed views (`sources`, `answers`, `disagreements`, `why`, `stats`)
//! show the rest. Nothing is invented here: a section the answer does not have is not shown, no
//! number is made up, and a partial, failed or cancelled run says so in its first line.

use std::fmt::Write as _;

use crate::consult::{ConsultMode, ConsultationMeta, section};
use crate::debate::{DebateOutcome, DebateStatus, Role};
use crate::history::format_utc;
use crate::research::source::SourceStatus;

/// The sections shown for a mode, in order: (heading shown, titles to look for in the answer).
fn plan(mode: Option<ConsultMode>) -> &'static [(&'static str, &'static [&'static str])] {
    match mode {
        Some(ConsultMode::Decision) => &[
            ("Recommendation", &["answer", "recommendation"]),
            ("Main reasons and criteria", &["alternatives and criteria"]),
            ("Trade-offs, costs and risks", &["trade-offs"]),
            ("Evidence and preferences", &["objective evidence"]),
            ("If your priorities change", &["how priorities change"]),
        ],
        Some(ConsultMode::Review) => &[
            ("Recommendation", &["answer", "recommendation"]),
            ("Evaluation criteria", &["evaluation criteria"]),
            ("Strengths and weaknesses", &["strengths and weaknesses"]),
            ("Alternatives compared", &["alternatives compared"]),
            ("Specifications vs. opinions", &["specifications vs"]),
            ("Outdated or unverified", &["outdated or unverified"]),
        ],
        Some(ConsultMode::Research) => &[
            ("Main findings", &["answer"]),
            ("Established facts", &["established facts"]),
            ("Contradictions", &["contradictions"]),
            ("Open questions", &["hypotheses and open questions"]),
            ("Contested claims", &["opinions and contested"]),
        ],
        Some(ConsultMode::Learn) => &[
            ("Explanation", &["lesson", "answer"]),
            ("Before you start", &["prerequisites"]),
            ("Corrections made after review", &["corrections made"]),
        ],
        Some(ConsultMode::Debate) => &[
            ("Conclusion", &["recommended solution", "answer"]),
            ("Reasoning", &["technical reasoning"]),
            ("Next steps", &["suggested next steps"]),
        ],
        None => &[("Answer", &["answer", "recommended solution", "lesson"])],
    }
}

/// Where the reader should be careful: always shown after the main sections.
const CAVEATS: [(&str, &[&str]); 3] = [
    ("Disagreements", &["significant disagreements"]),
    ("Assumptions", &["assumptions"]),
    (
        "Evidence limits",
        &["evidence limitations", "unresolved risks"],
    ),
];

fn banner(o: &DebateOutcome) -> Option<String> {
    match o.status {
        DebateStatus::Complete => None,
        DebateStatus::Partial => Some(
            "PARTIAL RESULT: something failed while this was produced. Treat it as incomplete; `/why` and `/answers` show what.".into(),
        ),
        DebateStatus::Failed => Some("FAILED: no answer was produced.".into()),
        DebateStatus::Cancelled => Some(
            "CANCELLED: you stopped it before it finished. What is shown (if anything) is incomplete and is not a result.".into(),
        ),
    }
}

fn not_none(text: &str) -> bool {
    let t = text.trim().trim_matches(['*', '.']).to_lowercase();
    !(t.is_empty() || t == "none" || t.starts_with("none:") || t == "n/a")
}

/// A one-line account of how it was produced.
pub fn footer(o: &DebateOutcome) -> String {
    let meta = o.consultation.as_ref();
    let strategy = meta
        .and_then(|m| m.routing.as_ref())
        .and_then(|r| r.smart.as_ref())
        .and_then(|p| p.strategy.as_ref())
        .map(|s| s.value.as_str().to_string())
        .or_else(|| meta.map(|m| m.strategy.clone()))
        .unwrap_or_else(|| "-".into());
    let mode = meta
        .and_then(|m| m.mode)
        .map_or("-".to_string(), |m| m.to_string());
    let agents = o
        .participants
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "[{} · {} · {} · {} call(s) · {:.0}s · agents: {} · id {}]",
        o.status,
        mode,
        strategy,
        o.calls,
        o.duration_ms as f64 / 1000.0,
        if agents.is_empty() {
            o.coordinator.to_string()
        } else {
            format!("{}, {}", o.coordinator, agents)
        },
        o.id
    )
}

/// The default view.
pub fn render_answer(o: &DebateOutcome) -> String {
    let mut out = String::new();
    if let Some(b) = banner(o) {
        let _ = writeln!(out, "{b}\n");
    }
    let meta = o.consultation.as_ref();
    let mode = meta.and_then(|m| m.mode);
    let answer = o.final_answer.trim();
    if answer.is_empty() {
        let _ = writeln!(out, "(no answer text)");
    } else {
        let mut shown = 0;
        for (heading, titles) in plan(mode) {
            let text = if *heading == "Recommendation" && mode != Some(ConsultMode::Research) {
                // The direct answer first, then the recommendation if it is a separate section.
                let a = section(answer, &["answer"]);
                let r = section(answer, &["recommendation"]);
                match (a, r) {
                    (Some(a), Some(r)) if a != r => Some(format!("{a}\n\n{r}")),
                    (Some(a), _) => Some(a),
                    (None, r) => r,
                }
            } else {
                section(answer, titles)
            };
            if let Some(text) = text {
                let _ = writeln!(out, "{heading}\n{}\n", text);
                shown += 1;
            }
        }
        for (heading, titles) in CAVEATS {
            if let Some(text) = section(answer, titles).filter(|t| not_none(t)) {
                let _ = writeln!(out, "{heading}\n{text}\n");
                shown += 1;
            }
        }
        if shown == 0 {
            // An answer without the expected headings (a single-agent reply, a brief one) is
            // shown as it is.
            let _ = writeln!(out, "{answer}\n");
        }
    }
    if let Some(r) = meta.and_then(|m| m.research.as_ref()) {
        let cited = r.citations.cited.len();
        let _ = writeln!(
            out,
            "Sources: {} retrieved of {} found, {} cited in the answer (`/sources` lists them).\n",
            r.retrieved(),
            r.sources.len(),
            cited
        );
    }
    for w in o.warnings.iter().take(3) {
        let _ = writeln!(out, "Note: {w}");
    }
    if o.warnings.len() > 3 {
        let _ = writeln!(out, "Note: {} more (see `/why`).", o.warnings.len() - 3);
    }
    let _ = writeln!(out, "{}", footer(o));
    out
}

/// Sources, citations and verification status.
pub fn render_sources(o: &DebateOutcome) -> String {
    let Some(r) = o.consultation.as_ref().and_then(|m| m.research.as_ref()) else {
        return "No web research was done for this answer, so there are no sources. Nothing in it was checked against external sources.\n".into();
    };
    let mut out = String::new();
    for s in &r.sources {
        let state = match s.status {
            SourceStatus::Retrieved => "retrieved",
            _ => "NOT retrieved (not used as evidence)",
        };
        let _ = writeln!(
            out,
            "[{}] {} - {}\n    {} · published {} · retrieved {} · {}",
            s.id,
            s.title.as_deref().unwrap_or("(untitled)"),
            s.display_url(),
            s.domain,
            s.published.as_deref().unwrap_or("date not stated"),
            s.retrieved_at_ms.map_or("-".to_string(), format_utc),
            state
        );
    }
    if !r.claims.is_empty() {
        let _ = writeln!(
            out,
            "\nClaims and their status (computed from retrieved passages; the judgement of each passage is model-dependent):"
        );
        for c in &r.claims {
            let ids: Vec<String> = c
                .evidence
                .iter()
                .map(|e| format!("[{}]", e.source_id))
                .collect();
            let _ = writeln!(
                out,
                "  {} - {}{}",
                c.status.as_str(),
                c.text,
                if ids.is_empty() {
                    String::new()
                } else {
                    format!(" {}", ids.join(""))
                }
            );
        }
    }
    if !r.citations.unknown_removed.is_empty() || !r.citations.unretrieved_removed.is_empty() {
        let _ = writeln!(
            out,
            "\nCitations removed from the answer because they named no retrieved source: {:?}",
            r.citations
                .unknown_removed
                .iter()
                .chain(&r.citations.unretrieved_removed)
                .collect::<Vec<_>>()
        );
    }
    for l in &r.limitations {
        let _ = writeln!(out, "Limit: {l}");
    }
    out
}

/// What each agent answered (clipped unless `full`).
pub fn render_answers(o: &DebateOutcome, full: bool) -> String {
    let mut out = String::new();
    for s in &o.steps {
        if !matches!(
            s.phase,
            Role::Perspective | Role::Proposal | Role::Review | Role::CrossReview | Role::Synthesis
        ) {
            continue;
        }
        let what = match s.phase {
            Role::Perspective => "independent answer".to_string(),
            Role::Proposal => "initial proposal".to_string(),
            Role::Review => "review".to_string(),
            Role::CrossReview => format!("cross-review (round {})", s.round),
            _ => "final synthesis".to_string(),
        };
        let _ = writeln!(
            out,
            "--- {} · {} · {:.1}s · {}",
            s.agent,
            what,
            s.duration_ms as f64 / 1000.0,
            if s.ok { "ok" } else { "FAILED" }
        );
        match (&s.text, &s.error) {
            (Some(t), _) if full => {
                let _ = writeln!(out, "{}", t.trim());
            }
            (Some(t), _) => {
                let t = t.trim();
                let cut: String = t.chars().take(700).collect();
                let _ = writeln!(
                    out,
                    "{cut}{}",
                    if t.chars().count() > 700 {
                        "\n[...clipped; `/answers full` shows all]"
                    } else {
                        ""
                    }
                );
            }
            (None, Some(e)) => {
                let _ = writeln!(out, "error: {e}");
            }
            _ => {}
        }
        out.push('\n');
    }
    if out.is_empty() {
        out.push_str("No agent answers were recorded for this consultation.\n");
    }
    out
}

pub fn render_disagreements(o: &DebateOutcome) -> String {
    let meta = o.consultation.as_ref();
    let from_meta = |t: Option<&String>| t.filter(|t| not_none(t)).cloned();
    let agreements = from_meta(meta.and_then(|m| m.agreements.as_ref()));
    let disagreements = from_meta(meta.and_then(|m| m.disagreements.as_ref()));
    let mut out = String::new();
    match &disagreements {
        Some(d) => {
            let _ = writeln!(
                out,
                "Disagreements{}:\n{d}\n",
                if meta.is_some_and(|m| m.unresolved_disagreement) {
                    " (not resolved)"
                } else {
                    ""
                }
            );
        }
        None => out.push_str("No significant disagreement was recorded.\n\n"),
    }
    if let Some(a) = agreements {
        let _ = writeln!(out, "Where they agree (agreement is not proof):\n{a}");
    }
    if let Some(r) = meta.and_then(|m| m.research.as_ref())
        && !r.contradictions.is_empty()
    {
        let _ = writeln!(out, "\nSources that disagree about a claim:");
        for c in &r.contradictions {
            let text = r
                .claims
                .iter()
                .find(|x| x.id == c.claim_id)
                .map_or("(claim)", |x| x.text.as_str());
            let _ = writeln!(
                out,
                "  {text}: supported by {:?}, contradicted by {:?}",
                c.supporting_sources, c.contradicting_sources
            );
        }
    }
    out
}

/// Why it ran the way it did: the routing and the strategy, in plain words.
pub fn render_why(o: &DebateOutcome) -> String {
    let Some(meta) = o.consultation.as_ref() else {
        return "This record is a plain debate; it has no routing explanation.\n".into();
    };
    let mut out = String::new();
    if let Some(r) = &meta.routing {
        out.push_str(&r.render());
    }
    if let Some(orch) = &meta.orchestration {
        out.push('\n');
        out.push_str(&orch.render());
    }
    if let Some(c) = &meta.context {
        out.push('\n');
        out.push_str(&c.render_text());
    }
    for w in &o.warnings {
        let _ = writeln!(out, "warning: {w}");
    }
    out
}

pub fn render_stats(o: &DebateOutcome) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "status:         {}", o.status);
    let _ = writeln!(out, "agent calls:    {}", o.calls);
    let _ = writeln!(out, "wall time:      {:.1}s", o.duration_ms as f64 / 1000.0);
    if let Some(m) = &o.consultation {
        render_meta_stats(&mut out, m);
    }
    out
}

fn render_meta_stats(out: &mut String, m: &ConsultationMeta) {
    if !m.invocations.is_empty() {
        let _ = writeln!(
            out,
            "per agent:      {}",
            m.invocations
                .iter()
                .map(|(k, v)| format!("{k} {v}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let _ = writeln!(out, "failed calls:   {}", m.provider_failures);
    if !m.stage_ms.is_empty() {
        let _ = writeln!(
            out,
            "stages (ms):    {}",
            m.stage_ms
                .iter()
                .map(|(k, v)| format!("{k} {v}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    match &m.token_usage {
        Some(t) => {
            let _ = writeln!(
                out,
                "tokens:         input {} / output {} as reported by the CLIs ({} of {} calls reported)",
                t.input_tokens, t.output_tokens, t.calls_reporting, t.calls_total
            );
        }
        None => {
            let _ = writeln!(
                out,
                "tokens:         not reported by the CLIs (none are estimated here)"
            );
        }
    }
    if let Some(c) = &m.context {
        let _ = writeln!(
            out,
            "memory context: {} item(s), ~{} tokens ({}), {} characters",
            c.selected, c.estimated_tokens, c.token_basis, c.context_chars
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::debate::DebateOutcome;

    fn outcome(mode: ConsultMode, answer: &str, status: DebateStatus) -> DebateOutcome {
        let json = serde_json::json!({
            "schema": 1, "id": "20261012-000000-000001", "conversation": 1, "created_at_ms": 1,
            "question": "q", "coordinator": "claude", "participants": ["codex"], "skipped": [],
            "rounds_planned": 1, "status": status, "synthesized": true, "steps": [],
            "initial_proposal": "", "final_answer": answer, "error": null, "calls": 3,
            "warnings": [], "duration_ms": 4200
        });
        let mut o: DebateOutcome = serde_json::from_value(json).unwrap();
        o.consultation = Some(ConsultationMeta {
            mode: Some(mode),
            strategy: "panel".into(),
            ..Default::default()
        });
        o
    }

    const DECISION: &str = "## Answer\nBuy the B.\n\n## Recommendation\nChoose B unless you need USB.\n\n## Alternatives and criteria\nA, B, C compared on bandwidth and price.\n\n## Trade-offs, costs and risks\nB costs more.\n\n## Areas of agreement\nAll like B.\n\n## Significant disagreements\nThe agents disagree on whether 2 channels are enough.\n\n## Assumptions\nBudget is fixed.\n\n## Evidence limitations\nNothing was verified externally.";

    #[test]
    fn a_decision_leads_with_the_recommendation_and_keeps_the_caveats() {
        let text = render_answer(&outcome(
            ConsultMode::Decision,
            DECISION,
            DebateStatus::Complete,
        ));
        let rec = text.find("Recommendation").unwrap();
        let reasons = text.find("Main reasons").unwrap();
        let trade = text.find("Trade-offs").unwrap();
        let dis = text.find("Disagreements").unwrap();
        assert!(rec < reasons && reasons < trade && trade < dis, "{text}");
        assert!(text.contains("Buy the B.") && text.contains("Choose B unless"));
        assert!(
            text.contains("2 channels are enough")
                && text.contains("Nothing was verified externally")
        );
        assert!(
            !text.contains("Areas of agreement"),
            "agreement is not a headline"
        );
        assert!(text.contains("[complete · decision"), "{text}");
    }

    #[test]
    fn a_learning_answer_is_not_forced_into_the_decision_shape() {
        let text = render_answer(&outcome(
            ConsultMode::Learn,
            "## Lesson\nResistance opposes current.\n\n## Prerequisites\nBasic algebra.\n\n## Significant disagreements\nNone.\n\n## Evidence limitations\nNo sources.",
            DebateStatus::Complete,
        ));
        assert!(text.contains("Explanation") && text.contains("Before you start"));
        assert!(!text.contains("Recommendation") && !text.contains("Trade-offs"));
        assert!(
            !text.contains("Disagreements"),
            "`None.` is not shown as a disagreement"
        );
    }

    #[test]
    fn partial_failed_and_cancelled_runs_never_look_complete() {
        for (status, word) in [
            (DebateStatus::Partial, "PARTIAL"),
            (DebateStatus::Cancelled, "CANCELLED"),
            (DebateStatus::Failed, "FAILED"),
        ] {
            let text = render_answer(&outcome(ConsultMode::Decision, DECISION, status));
            assert!(text.starts_with(word), "{text}");
            assert!(!text.contains("[complete"), "{text}");
        }
    }

    #[test]
    fn an_answer_without_headings_is_shown_whole_and_nothing_is_invented() {
        let text = render_answer(&outcome(
            ConsultMode::Research,
            "Just a short reply.",
            DebateStatus::Complete,
        ));
        assert!(text.contains("Just a short reply."));
        assert!(!text.contains("Sources:"), "no research, no sources line");
        assert!(
            render_sources(&outcome(ConsultMode::Research, "x", DebateStatus::Complete))
                .contains("No web research")
        );
    }

    #[test]
    fn disagreements_are_listed_with_the_agreement_caveat() {
        let mut o = outcome(ConsultMode::Decision, DECISION, DebateStatus::Complete);
        let m = o.consultation.as_mut().unwrap();
        m.disagreements = Some("The agents disagree on channels.".into());
        m.agreements = Some("All like B.".into());
        m.unresolved_disagreement = true;
        let text = render_disagreements(&o);
        assert!(text.contains("(not resolved)") && text.contains("agreement is not proof"));
    }
}
