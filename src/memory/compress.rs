//! Deterministic reduction of context: what to keep of a finished consultation, and how to drop
//! what repeats.
//!
//! Nothing here calls a model. A consultation is condensed by *selecting* parts of it (the answer,
//! the recommendation, the assumptions, the limitations, the disagreements it left open, the claims
//! that were checked and where they came from), never by rewording it, so a figure or a
//! qualification cannot be changed by the compression. The original answer is stored next to the
//! condensed form and is what the "full" representation expands to; a condensed form never replaces
//! its source.

use std::collections::HashSet;

use sha2::{Digest, Sha256};

use super::db::EvidenceRef;
use super::freshness::{is_time_sensitive, mentions_price};
use super::model::{EpistemicStatus, normalize_for_hash};
use super::retrieval::{Candidate, tokenize_terms};
use crate::consult::section;
use crate::debate::{Role, Step};
use crate::research::claims::{ClaimRecord, ClaimStatus};
use crate::research::engine::ResearchReport;
use crate::research::quality::ClaimCategory;
use crate::research::source::{SourceRecord, SourceStatus};

/// Cuts at the end of a paragraph or sentence (or a space) before `max` characters, never inside a
/// word, and says so with an ellipsis.
pub fn clip_at_boundary(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max).collect();
    let cut = ["\n\n", ". ", "! ", "? ", "\n", "; ", ", ", " "]
        .iter()
        .filter_map(|sep| {
            head.rfind(sep).map(|i| {
                i + if sep.starts_with(['.', '!', '?']) {
                    1
                } else {
                    0
                }
            })
        })
        .find(|i| *i >= max / 2)
        .unwrap_or(head.len());
    format!("{}…", head[..cut].trim_end())
}

fn first_sentence(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let end = flat
        .char_indices()
        .find(|(i, c)| matches!(c, '.' | '!' | '?') && flat[i + 1..].starts_with(' '))
        .map_or(flat.len(), |(i, _)| i + 1);
    clip_at_boundary(&flat[..end], max)
}

fn meaningful(section_text: Option<String>) -> Option<String> {
    let t = section_text?;
    let flat = t.trim().trim_start_matches(['-', '*', ' ']).to_lowercase();
    if flat.is_empty()
        || flat.starts_with("none")
        || flat.starts_with("no significant")
        || flat.starts_with("n/a")
    {
        None
    } else {
        Some(t.trim().to_string())
    }
}

/// One claim that was checked, ready to store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceDraft {
    pub claim: String,
    pub status: EpistemicStatus,
    pub category: Option<String>,
    pub time_sensitive: bool,
    pub retrieved_ms: Option<u64>,
    pub published: Option<String>,
    pub source_url: Option<String>,
    pub refs: Vec<EvidenceRef>,
    /// Identifies the claim across consultations, so a newer check can replace an older one.
    pub topic: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OutcomeDigest {
    /// The condensed result: question, answer, recommendation, assumptions, limitations.
    pub conclusion: String,
    /// One line that keeps the label and the qualification.
    pub short: String,
    pub unresolved: Vec<String>,
    pub evidence: Vec<EvidenceDraft>,
    /// Each agent's own words (condensed). Stored for the audit trail; never sent as context.
    pub findings: Vec<(String, String)>,
    pub time_sensitive: bool,
}

pub struct DigestInput<'a> {
    pub question: &'a str,
    pub mode: &'a str,
    pub final_answer: &'a str,
    pub steps: &'a [Step],
    pub research: Option<&'a ResearchReport>,
}

fn claim_topic(claim: &str) -> String {
    let mut h = Sha256::new();
    h.update(normalize_for_hash(claim).as_bytes());
    let hex = h.finalize().iter().take(8).fold(String::new(), |mut s, b| {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
        s
    });
    format!("claim:{hex}")
}

fn status_of(c: &ClaimRecord) -> EpistemicStatus {
    match c.status {
        ClaimStatus::Supported if c.category == ClaimCategory::HistoricalFact => {
            EpistemicStatus::Historical
        }
        ClaimStatus::Supported => EpistemicStatus::PreviouslyVerified,
        ClaimStatus::Contradicted | ClaimStatus::MixedEvidence => EpistemicStatus::Contradicted,
        ClaimStatus::InsufficientEvidence | ClaimStatus::NotChecked => EpistemicStatus::Unverified,
    }
}

fn category_text(c: ClaimCategory) -> String {
    serde_json::to_string(&c)
        .map(|s| s.trim_matches('"').to_string())
        .unwrap_or_default()
}

fn draft_for(claim: &ClaimRecord, sources: &[SourceRecord]) -> EvidenceDraft {
    let find = |id: u32| {
        sources
            .iter()
            .find(|s| s.id == id && s.status == SourceStatus::Retrieved)
    };
    let mut refs = Vec::new();
    let mut retrieved: Option<u64> = None;
    let mut published: Option<String> = None;
    let mut url: Option<String> = None;
    for link in &claim.evidence {
        let Some(src) = find(link.source_id) else {
            continue;
        };
        retrieved = retrieved.max(src.retrieved_at_ms);
        if src.published > published {
            published.clone_from(&src.published);
        }
        if url.is_none()
            && format!("{:?}", link.relation)
                .to_lowercase()
                .contains("support")
        {
            url = Some(src.display_url().to_string());
        }
        refs.push(EvidenceRef {
            url: Some(src.display_url().to_string()),
            title: src.title.clone(),
            published: src.published.clone(),
            updated: src.updated.clone(),
            retrieved_ms: src.retrieved_at_ms,
            content_hash: src.content_hash.clone(),
            relation: Some(format!("{:?}", link.relation).to_lowercase()),
            quote: Some(clip_at_boundary(&link.passage, 300)),
        });
    }
    let category = category_text(claim.category);
    let time_sensitive = is_time_sensitive(Some(&category), &claim.text);
    EvidenceDraft {
        claim: claim.text.clone(),
        status: status_of(claim),
        category: Some(category),
        time_sensitive,
        retrieved_ms: retrieved,
        published,
        source_url: url.or_else(|| refs.first().and_then(|r| r.url.clone())),
        refs,
        topic: claim_topic(&claim.text),
    }
}

/// Condenses a finished consultation, deterministically.
pub fn digest_outcome(input: &DigestInput<'_>) -> OutcomeDigest {
    let answer = input.final_answer;
    let body =
        section(answer, &["answer", "lesson", "recommended solution"]).unwrap_or_else(|| {
            // No section headings (a single-agent reply): the start of the text.
            answer.to_string()
        });
    let recommendation = section(answer, &["recommendation"]);
    let assumptions = section(answer, &["assumptions"]);
    let limits = section(
        answer,
        &["evidence limitations", "limitations", "unresolved risks"],
    );
    let disagreements = meaningful(section(answer, &["significant disagreements"]));

    let mut lines = vec![
        format!("Question: {}", clip_at_boundary(input.question, 300)),
        format!(
            "Conclusion (written by AI agents in a {} consultation; not verified): {}",
            input.mode,
            clip_at_boundary(&body, 1200)
        ),
    ];
    if let Some(r) = &recommendation {
        lines.push(format!("Recommendation: {}", clip_at_boundary(r, 700)));
    }
    if let Some(a) = &assumptions {
        lines.push(format!("Assumptions: {}", clip_at_boundary(a, 300)));
    }
    if let Some(l) = &limits {
        lines.push(format!(
            "Evidence limitations: {}",
            clip_at_boundary(l, 300)
        ));
    }
    if let Some(d) = &disagreements {
        lines.push(format!("Open disagreements: {}", clip_at_boundary(d, 400)));
    }
    let conclusion = lines.join("\n");

    let headline = recommendation
        .as_deref()
        .map(|r| first_sentence(r, 200))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| first_sentence(&body, 200));
    let short = format!(
        "Q: {} → {} (AI-generated conclusion, not verified)",
        clip_at_boundary(input.question, 100),
        headline
    );

    let evidence: Vec<EvidenceDraft> = input
        .research
        .map(|r| r.claims.iter().map(|c| draft_for(c, &r.sources)).collect())
        .unwrap_or_default();

    let mut unresolved = Vec::new();
    if let Some(d) = &disagreements {
        unresolved.push(format!(
            "After \"{}\" the agents left this disagreement open: {}",
            clip_at_boundary(input.question, 80),
            clip_at_boundary(d, 400)
        ));
    }
    if let Some(r) = input.research {
        for c in r.claims.iter().filter(|c| {
            matches!(
                c.status,
                ClaimStatus::Contradicted | ClaimStatus::MixedEvidence
            )
        }) {
            unresolved.push(format!(
                "The sources disagree about: {} ({})",
                clip_at_boundary(&c.text, 200),
                c.status.as_str()
            ));
        }
    }

    let mut findings = Vec::new();
    let panel = input
        .steps
        .iter()
        .filter(|s| s.ok && matches!(s.phase, Role::Perspective | Role::Proposal))
        .count();
    if panel >= 2 {
        for s in input
            .steps
            .iter()
            .filter(|s| s.ok && matches!(s.phase, Role::Perspective | Role::Proposal))
        {
            if let Some(t) = &s.text {
                let cleaned = crate::research::claims::strip_claims_block(t);
                findings.push((s.agent.to_string(), clip_at_boundary(&cleaned, 600)));
            }
        }
    }

    let time_sensitive =
        evidence.iter().any(|e| e.time_sensitive) || mentions_price(&clip_at_boundary(&body, 1200));
    OutcomeDigest {
        conclusion,
        short,
        unresolved,
        evidence,
        findings,
        time_sensitive,
    }
}

fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let inter = a.intersection(b).count() as f64;
    let union = a.union(b).count() as f64;
    inter / union
}

/// Why a candidate was dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dropped {
    pub id: String,
    pub duplicate_of: String,
    pub reason: String,
}

/// Removes records that say the same thing twice: identical text (for example a session copy and a
/// project copy of the same note) and near-identical text of the same kind. Of two duplicates the
/// better-trusted one is kept, then the more specific scope, then the newer. Candidates are assumed
/// to be ordered best-first, and the order of the survivors is kept.
pub fn dedupe(candidates: Vec<Candidate>) -> (Vec<Candidate>, Vec<Dropped>) {
    let mut kept: Vec<(Candidate, HashSet<String>, String)> = Vec::new();
    let mut dropped = Vec::new();
    for cand in candidates {
        let set: HashSet<String> = tokenize_terms(&cand.record.text).into_iter().collect();
        let norm = normalize_for_hash(&cand.record.text);
        let same = kept.iter().position(|(other, other_set, other_norm)| {
            other.record.kind == cand.record.kind
                && (*other_norm == norm
                    || (other_set.len() >= 4 && set.len() >= 4 && jaccard(other_set, &set) >= 0.85))
        });
        let Some(i) = same else {
            kept.push((cand, set, norm));
            continue;
        };
        let old_rank = (kept[i].0.record.status.rank(), scope_rank(&kept[i].0));
        let new_rank = (cand.record.status.rank(), scope_rank(&cand));
        if new_rank > old_rank {
            dropped.push(Dropped {
                id: kept[i].0.record.id.clone(),
                duplicate_of: cand.record.id.clone(),
                reason: "the same statement is stored in a better-trusted or more specific place"
                    .to_string(),
            });
            kept[i] = (cand, set, norm);
        } else {
            dropped.push(Dropped {
                id: cand.record.id.clone(),
                duplicate_of: kept[i].0.record.id.clone(),
                reason: "says the same as a record that is already included".to_string(),
            });
        }
    }
    (kept.into_iter().map(|(c, _, _)| c).collect(), dropped)
}

fn scope_rank(c: &Candidate) -> u8 {
    match c.record.scope {
        super::model::MemoryScope::Project => 3,
        super::model::MemoryScope::Session => 2,
        super::model::MemoryScope::Global => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::debate::Step;
    use crate::memory::freshness::Freshness;
    use crate::memory::model::{
        Authorization, Lifecycle, MemoryKind, MemoryRecord, MemoryScope, Origin,
    };
    use crate::memory::retrieval::Via;
    use crate::types::ProviderId;

    const ANSWER: &str = "## Answer\n\nBuy the Rigol DS1054Z: it has 50 MHz of bandwidth and four channels, which suits learning. The Siglent costs more. The Hantek is portable but has two channels.\n\n## Recommendation\n\nGet the Rigol DS1054Z unless portability matters more than channel count.\n\n## Areas of agreement\n\nAll agree on the Rigol.\n\n## Significant disagreements\n\n- gamma prefers the Hantek for portability; unresolved.\n\n## Assumptions\n\nA budget under 5,000 MXN.\n\n## Evidence limitations\n\nPrices were not verified.";

    fn step(agent: &str, text: &str) -> Step {
        Step {
            phase: Role::Perspective,
            round: 0,
            agent: ProviderId::new(agent),
            offset_ms: 0,
            duration_ms: 1,
            ok: true,
            text: Some(text.into()),
            error: None,
            error_kind: None,
            settings: None,
            usage: None,
        }
    }

    fn cand(
        id: &str,
        kind: MemoryKind,
        scope: MemoryScope,
        status: EpistemicStatus,
        text: &str,
    ) -> Candidate {
        Candidate {
            record: MemoryRecord {
                id: id.into(),
                kind,
                scope,
                session_id: None,
                project_id: None,
                origin: Origin::User,
                status,
                lifecycle: Lifecycle::Active,
                text: text.into(),
                short: None,
                topic: None,
                tags: vec![],
                authorization: Authorization::ExplicitUser,
                sensitive: false,
                consultation_id: None,
                content_hash: id.into(),
                created_ms: 1,
                updated_ms: 1,
                retrieved_ms: None,
                published: None,
                valid_days: None,
                category: None,
                time_sensitive: false,
                source_url: None,
                expires_ms: None,
            },
            relevance: 1.0,
            score: 1.0,
            via: Via::Lexical,
            reasons: vec![],
            freshness: Freshness::NotApplicable,
        }
    }

    #[test]
    fn clipping_stops_at_a_boundary_and_never_inside_a_word() {
        let text = "First sentence is here. Second sentence follows right after it. Third one ends the paragraph.";
        let c = clip_at_boundary(text, 40);
        assert_eq!(c, "First sentence is here.…");
        let words = clip_at_boundary("alpha beta gamma delta epsilon", 14);
        assert!(words.ends_with('…') && !words.contains("gam"), "{words}");
        assert_eq!(clip_at_boundary("short", 40), "short");
    }

    #[test]
    fn a_consultation_is_condensed_by_selecting_its_parts_and_keeping_their_qualifications() {
        let steps = vec![
            step("claude", "Rigol is best.\n```claims\n[]\n```"),
            step("codex", "Siglent is best."),
        ];
        let d = digest_outcome(&DigestInput {
            question: "Compare three portable oscilloscopes",
            mode: "review",
            final_answer: ANSWER,
            steps: &steps,
            research: None,
        });
        assert!(
            d.conclusion
                .starts_with("Question: Compare three portable oscilloscopes")
        );
        assert!(
            d.conclusion.contains("not verified"),
            "the label survives: {}",
            d.conclusion
        );
        assert!(
            d.conclusion
                .contains("50 MHz of bandwidth and four channels"),
            "figures are copied, not reworded"
        );
        assert!(
            d.conclusion
                .contains("Recommendation: Get the Rigol DS1054Z unless portability")
        );
        assert!(
            d.conclusion
                .contains("Assumptions: A budget under 5,000 MXN.")
                && d.conclusion.contains("Prices were not verified")
        );
        assert!(
            d.conclusion
                .contains("Open disagreements: - gamma prefers the Hantek")
        );
        assert!(
            !d.conclusion.contains("All agree"),
            "the agreement section is not repeated"
        );
        assert!(
            d.short.contains("AI-generated conclusion, not verified")
                && d.short.contains("Get the Rigol DS1054Z")
        );
        assert_eq!(d.unresolved.len(), 1);
        assert!(d.unresolved[0].contains("gamma prefers the Hantek"));
        assert_eq!(
            d.findings.len(),
            2,
            "each agent's own words are kept for the audit trail"
        );
        assert!(
            !d.findings[0].1.contains("```claims"),
            "claims blocks are machine text"
        );
        assert!(d.conclusion.len() < ANSWER.len() + 200);
    }

    #[test]
    fn an_answer_without_headings_is_kept_from_its_start_and_none_disagreements_are_not_unresolved()
    {
        let d = digest_outcome(&DigestInput {
            question: "q",
            mode: "learn",
            final_answer: "Plain reply with no headings at all.",
            steps: &[],
            research: None,
        });
        assert!(
            d.conclusion
                .contains("Plain reply with no headings at all.")
        );
        assert!(d.unresolved.is_empty() && d.findings.is_empty());
        let none = "## Answer\n\nx\n\n## Significant disagreements\n\nNone.\n";
        assert!(
            digest_outcome(&DigestInput {
                question: "q",
                mode: "learn",
                final_answer: none,
                steps: &[],
                research: None
            })
            .unresolved
            .is_empty()
        );
    }

    #[test]
    fn prices_in_an_answer_make_the_conclusion_time_sensitive() {
        let a = "## Answer\n\nThe scope costs $3,500 MXN at the moment.";
        assert!(
            digest_outcome(&DigestInput {
                question: "q",
                mode: "review",
                final_answer: a,
                steps: &[],
                research: None
            })
            .time_sensitive
        );
        assert!(
            !digest_outcome(&DigestInput {
                question: "q",
                mode: "learn",
                final_answer: "## Answer\n\nOhm's law.",
                steps: &[],
                research: None
            })
            .time_sensitive
        );
    }

    #[test]
    fn duplicates_are_dropped_keeping_the_better_trusted_and_more_specific_copy() {
        let a = cand(
            "a",
            MemoryKind::Preference,
            MemoryScope::Session,
            EpistemicStatus::AgentInterpretation,
            "Budget is under 5000 MXN for the oscilloscope",
        );
        let b = cand(
            "b",
            MemoryKind::Preference,
            MemoryScope::Project,
            EpistemicStatus::UserProvided,
            "budget is under 5000 mxn for the oscilloscope",
        );
        let c = cand(
            "c",
            MemoryKind::Note,
            MemoryScope::Project,
            EpistemicStatus::UserProvided,
            "Something entirely different about microscopes and lenses",
        );
        let (kept, dropped) = dedupe(vec![a, b, c]);
        let ids: Vec<&str> = kept.iter().map(|k| k.record.id.as_str()).collect();
        assert_eq!(
            ids,
            ["b", "c"],
            "the user-stated project copy replaces the AI session copy"
        );
        assert_eq!(dropped.len(), 1);
        assert_eq!(
            (dropped[0].id.as_str(), dropped[0].duplicate_of.as_str()),
            ("a", "b")
        );
    }

    #[test]
    fn near_duplicates_of_the_same_kind_collapse_but_the_same_words_in_another_kind_do_not() {
        let a = cand(
            "a",
            MemoryKind::Note,
            MemoryScope::Project,
            EpistemicStatus::UserProvided,
            "oscilloscope bandwidth matters most for audio work around five megahertz",
        );
        let b = cand(
            "b",
            MemoryKind::Note,
            MemoryScope::Project,
            EpistemicStatus::UserProvided,
            "oscilloscope bandwidth matters most for audio work around five megahertz really",
        );
        let c = cand(
            "c",
            MemoryKind::Decision,
            MemoryScope::Project,
            EpistemicStatus::UserProvided,
            "oscilloscope bandwidth matters most for audio work around five megahertz",
        );
        let (kept, dropped) = dedupe(vec![a, b, c]);
        assert_eq!(kept.len(), 2);
        assert_eq!(dropped[0].id, "b");
        assert!(kept.iter().any(|k| k.record.kind == MemoryKind::Decision));
    }

    #[test]
    fn claim_topics_are_stable_across_spelling() {
        assert_eq!(
            claim_topic("The Model X has 100 MHz."),
            claim_topic("the model x has 100 mhz")
        );
        assert_ne!(
            claim_topic("The Model X has 100 MHz"),
            claim_topic("The Model X has 70 MHz")
        );
    }
}
