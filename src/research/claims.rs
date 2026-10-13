//! Claims: the factual statements that matter to an answer, and what the evidence says about each.
//!
//! A claim is *extracted* from what the agents wrote (they are asked to list the claims they rely
//! on in a fenced `claims` block), de-duplicated across agents, and ranked by importance. Its
//! *status* is never taken from an agent: it is computed from evidence links, each of which points
//! at a passage this program cut from a page it retrieved. Agents agreeing with each other does
//! not move a claim towards SUPPORTED, and neither does a source that merely exists.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::quality::{ClaimCategory, Freshness, SourceKind, Suitability};
use super::source::SourceSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ClaimStatus {
    /// At least one retrieved passage supports it and none contradicts it.
    Supported,
    /// Retrieved passages contradict it and none supports it.
    Contradicted,
    /// Some retrieved passages support it and others contradict it.
    MixedEvidence,
    /// Verification ran, but no retrieved passage supports or contradicts it.
    InsufficientEvidence,
    /// No verification was run (offline, no sources, or a quick run).
    NotChecked,
}

impl ClaimStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Supported => "SUPPORTED",
            Self::Contradicted => "CONTRADICTED",
            Self::MixedEvidence => "MIXED_EVIDENCE",
            Self::InsufficientEvidence => "INSUFFICIENT_EVIDENCE",
            Self::NotChecked => "NOT_CHECKED",
        }
    }
}

/// How a passage relates to a claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Relation {
    Supports,
    Contradicts,
    Unrelated,
    /// The passage is vague, partial, or the assessors disagreed.
    Unclear,
}

impl Relation {
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "supports" | "support" | "supported" | "supporting" => Some(Self::Supports),
            "contradicts" | "contradict" | "contradicted" | "contradicting" | "refutes" => {
                Some(Self::Contradicts)
            }
            "unrelated" | "irrelevant" | "not_related" | "neutral" => Some(Self::Unrelated),
            "unclear" | "uncertain" | "partial" | "mixed" | "insufficient" => Some(Self::Unclear),
            _ => None,
        }
    }
}

/// A passage of a retrieved page and how it relates to a claim. The passage text was cut by this
/// program from the extracted page text; the relation is a model's reading of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceLink {
    pub source_id: u32,
    pub relation: Relation,
    pub passage: String,
    /// Character offset of the passage in the source's extracted text.
    pub offset: usize,
    /// The agents that judged this passage. The judgement is semantic and model-dependent.
    pub assessed_by: Vec<String>,
    pub source_kind: SourceKind,
    pub suitability: Suitability,
    pub suitability_note: String,
    pub freshness: Freshness,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimRecord {
    pub id: u32,
    pub text: String,
    pub category: ClaimCategory,
    pub originating_agents: Vec<String>,
    pub needs_external_evidence: bool,
    /// Source ids the originating agents said they relied on. Their word only: a hint for where
    /// to look, not evidence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cited_by_agents: Vec<u32>,
    pub status: ClaimStatus,
    pub evidence: Vec<EvidenceLink>,
    /// Independent upstream sources that support / contradict it.
    pub supporting_independent_sources: usize,
    pub contradicting_independent_sources: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// A claim as written by one agent, before merging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawClaim {
    pub text: String,
    pub category: ClaimCategory,
    pub sources: Vec<u32>,
    pub needs_external_evidence: bool,
    pub agent: String,
}

fn fenced_blocks(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let t = line.trim();
        if let Some(info) = t.strip_prefix("```") {
            let mut body = String::new();
            for l in lines.by_ref() {
                if l.trim().starts_with("```") {
                    break;
                }
                body.push_str(l);
                body.push('\n');
            }
            out.push((info.trim().to_ascii_lowercase(), body));
        }
    }
    out
}

fn as_ids(v: &Value) -> Vec<u32> {
    let one = |x: &Value| -> Option<u32> {
        match x {
            Value::Number(n) => n.as_u64().and_then(|n| u32::try_from(n).ok()),
            Value::String(s) => s.trim().trim_matches(['[', ']']).parse().ok(),
            _ => None,
        }
    };
    match v {
        Value::Array(items) => items.iter().filter_map(one).collect(),
        other => one(other).into_iter().collect(),
    }
}

/// Reads the claims an agent listed in a fenced block (` ```claims ` or ` ```json `). Malformed
/// blocks and entries are ignored: a missing list means "no claims", never an error.
pub fn parse_claims(reply: &str, agent: &str) -> Vec<RawClaim> {
    let blocks = fenced_blocks(reply);
    let chosen = blocks
        .iter()
        .find(|(info, _)| info == "claims")
        .or_else(|| {
            blocks
                .iter()
                .find(|(info, body)| info == "json" && body.contains("\"claim"))
        });
    let Some((_, body)) = chosen else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(body.trim()) else {
        return Vec::new();
    };
    let items = match &value {
        Value::Array(a) => a.clone(),
        Value::Object(o) => o
            .get("claims")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    items
        .iter()
        .filter_map(|item| {
            let text = item
                .get("claim")
                .or_else(|| item.get("text"))?
                .as_str()?
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            if text.chars().count() < 8 || text.chars().count() > 400 {
                return None;
            }
            Some(RawClaim {
                text,
                category: item
                    .get("category")
                    .and_then(Value::as_str)
                    .map(ClaimCategory::parse)
                    .unwrap_or(ClaimCategory::Other),
                sources: item
                    .get("sources")
                    .or_else(|| item.get("cited_sources"))
                    .map(as_ids)
                    .unwrap_or_default(),
                needs_external_evidence: item
                    .get("needs_external_evidence")
                    .and_then(Value::as_bool)
                    .unwrap_or(true),
                agent: agent.to_string(),
            })
        })
        .take(20)
        .collect()
}

/// The reply without its claims block, for showing and for passing on to other agents.
pub fn strip_claims_block(reply: &str) -> String {
    let mut out = Vec::new();
    let mut lines = reply.lines();
    while let Some(line) = lines.next() {
        let info = line
            .trim()
            .strip_prefix("```")
            .map(|i| i.trim().to_ascii_lowercase());
        if info.as_deref() == Some("claims") {
            for l in lines.by_ref() {
                if l.trim().starts_with("```") {
                    break;
                }
            }
            continue;
        }
        out.push(line);
    }
    out.join("\n").trim_end().to_string()
}

fn tokens(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '.')
        .map(|w| w.trim_matches('.').to_lowercase())
        .filter(|w| w.len() > 1)
        .collect()
}

pub(crate) fn similar(a: &str, b: &str) -> bool {
    let (ta, tb) = (tokens(a), tokens(b));
    if ta.is_empty() || tb.is_empty() {
        return false;
    }
    let inter = ta.intersection(&tb).count() as f64;
    inter / ta.union(&tb).count() as f64 >= 0.7
}

fn has_number(text: &str) -> bool {
    text.chars().any(|c| c.is_ascii_digit())
}

/// Merges claims from several agents (near-identical wording is one claim, and every agent that
/// made it is recorded), ranks them by importance and keeps the `max` most important. The same
/// claim being made by several agents raises its priority for checking, **not** its status.
pub fn merge_claims(raw: Vec<RawClaim>, max: usize) -> Vec<ClaimRecord> {
    let mut merged: Vec<ClaimRecord> = Vec::new();
    for r in raw {
        if let Some(existing) = merged.iter_mut().find(|c| similar(&c.text, &r.text)) {
            if !existing.originating_agents.contains(&r.agent) {
                existing.originating_agents.push(r.agent.clone());
            }
            for s in &r.sources {
                if !existing.cited_by_agents.contains(s) {
                    existing.cited_by_agents.push(*s);
                }
            }
            existing.needs_external_evidence |= r.needs_external_evidence;
            if existing.category == ClaimCategory::Other {
                existing.category = r.category;
            }
            continue;
        }
        merged.push(ClaimRecord {
            id: 0,
            text: r.text,
            category: r.category,
            originating_agents: vec![r.agent],
            needs_external_evidence: r.needs_external_evidence,
            cited_by_agents: r.sources,
            status: ClaimStatus::NotChecked,
            evidence: Vec::new(),
            supporting_independent_sources: 0,
            contradicting_independent_sources: 0,
            notes: Vec::new(),
        });
    }
    let score = |c: &ClaimRecord| {
        2 * c.originating_agents.len()
            + usize::from(has_number(&c.text))
            + usize::from(!c.cited_by_agents.is_empty())
            + usize::from(c.needs_external_evidence)
    };
    merged.sort_by_key(|c| std::cmp::Reverse(score(c)));
    merged.truncate(max);
    for (i, c) in merged.iter_mut().enumerate() {
        c.id = u32::try_from(i + 1).unwrap_or(u32::MAX);
    }
    merged
}

/// How many independent origins stand behind the evidence links of one relation. Two sources are
/// one origin when their pages are near-copies (the page-level groups), **or** when the passages
/// that were actually used as evidence are near-copies of each other: a news site that repeats the
/// maker's specification paragraph is not a second confirmation, even if the rest of its page differs.
fn independent_origins(set: &SourceSet, links: &[&EvidenceLink]) -> usize {
    let mut ids: Vec<u32> = links.iter().map(|l| l.source_id).collect();
    ids.sort_unstable();
    ids.dedup();
    let mut parent: std::collections::HashMap<u32, u32> = ids
        .iter()
        .map(|&id| {
            let root = set.get(id).and_then(|r| r.upstream_group).unwrap_or(id);
            (id, root)
        })
        .collect();
    fn find(parent: &mut std::collections::HashMap<u32, u32>, x: u32) -> u32 {
        let p = parent.get(&x).copied().unwrap_or(x);
        if p == x {
            return x;
        }
        let root = find(parent, p);
        parent.insert(x, root);
        root
    }
    // Page-level groups name a representative id that may not be among the links: make sure it
    // is a node.
    for root in parent.values().copied().collect::<Vec<_>>() {
        parent.entry(root).or_insert(root);
    }
    for (i, a) in links.iter().enumerate() {
        for b in &links[i + 1..] {
            if a.source_id != b.source_id && passages_are_copies(&a.passage, &b.passage) {
                let (ra, rb) = (
                    find(&mut parent, a.source_id),
                    find(&mut parent, b.source_id),
                );
                if ra != rb {
                    parent.insert(ra.max(rb), ra.min(rb));
                }
            }
        }
    }
    let mut roots = std::collections::HashSet::new();
    for id in ids {
        roots.insert(find(&mut parent, id));
    }
    roots.len()
}

/// Whether two passages are the same text with at most small changes.
fn passages_are_copies(a: &str, b: &str) -> bool {
    let (ta, tb): (Vec<String>, Vec<String>) = (
        a.split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_lowercase)
            .collect(),
        b.split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_lowercase)
            .collect(),
    );
    if ta.len() < 8 || tb.len() < 8 {
        return false;
    }
    let (sa, sb): (HashSet<&[String]>, HashSet<&[String]>) =
        (ta.windows(4).collect(), tb.windows(4).collect());
    let inter = sa.intersection(&sb).count() as f64;
    inter / sa.len().min(sb.len()) as f64 >= 0.7
}

/// Computes a claim's status from its evidence links. Only retrieved sources can be linked, and
/// only passages judged `Supports` / `Contradicts` count; each independent upstream source counts
/// once however many pages repeat it.
pub fn decide_status(claim: &mut ClaimRecord, set: &SourceSet, verification_ran: bool) {
    claim.notes.retain(|n| !n.starts_with("auto: "));
    if !verification_ran {
        claim.status = ClaimStatus::NotChecked;
        claim.supporting_independent_sources = 0;
        claim.contradicting_independent_sources = 0;
        return;
    }
    let ids = |rel: Relation| -> Vec<u32> {
        let mut v: Vec<u32> = claim
            .evidence
            .iter()
            .filter(|e| e.relation == rel)
            .map(|e| e.source_id)
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    let sup = ids(Relation::Supports);
    let links = |rel: Relation| -> Vec<&EvidenceLink> {
        claim
            .evidence
            .iter()
            .filter(|e| e.relation == rel)
            .collect()
    };
    let (gs, gc) = (
        independent_origins(set, &links(Relation::Supports)),
        independent_origins(set, &links(Relation::Contradicts)),
    );
    claim.supporting_independent_sources = gs;
    claim.contradicting_independent_sources = gc;
    claim.status = match (gs > 0, gc > 0) {
        (true, true) => ClaimStatus::MixedEvidence,
        (true, false) => ClaimStatus::Supported,
        (false, true) => ClaimStatus::Contradicted,
        (false, false) => ClaimStatus::InsufficientEvidence,
    };
    if sup.len() > gs {
        claim.notes.push(format!("auto: {} supporting pages are copies or near-copies of {} original(s), so they count once", sup.len(), gs));
    }
    if gs == 1 && claim.status == ClaimStatus::Supported {
        claim
            .notes
            .push("auto: supported by a single independent source".to_string());
    }
    let weak_only = claim.status == ClaimStatus::Supported
        && claim
            .evidence
            .iter()
            .filter(|e| e.relation == Relation::Supports)
            .all(|e| e.suitability == Suitability::Weak);
    if weak_only {
        claim.notes.push(
            "auto: the supporting sources are weak evidence for this kind of claim".to_string(),
        );
    }
    if claim
        .evidence
        .iter()
        .any(|e| e.relation == Relation::Supports && e.freshness == Freshness::Stale)
        && claim.status == ClaimStatus::Supported
    {
        claim.notes.push(
            "auto: some supporting sources are older than the requested freshness".to_string(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::research::fetch::FetchedPage;
    use crate::research::html::Extracted;
    use crate::research::quality::compute_upstream_groups;
    use crate::research::source::{Discovery, DiscoveryMethod};

    fn raw(text: &str, agent: &str) -> RawClaim {
        RawClaim {
            text: text.into(),
            category: ClaimCategory::ProductSpecification,
            sources: vec![],
            needs_external_evidence: true,
            agent: agent.into(),
        }
    }

    #[test]
    fn claims_are_read_from_a_fenced_block_and_malformed_ones_are_ignored() {
        let reply = "My answer [1].\n\n```claims\n[{\"claim\":\"Model X has 100 MHz bandwidth\",\"category\":\"product_specification\",\"sources\":[1,\"[2]\"],\"needs_external_evidence\":true},\n {\"text\":\"Short\"},{\"nope\":1},{\"claim\":\"The firmware was released in 2025\",\"category\":\"event\"}]\n```\nTrailing text";
        let claims = parse_claims(reply, "codex");
        assert_eq!(claims.len(), 2, "{claims:?}");
        assert_eq!(claims[0].sources, vec![1, 2]);
        assert_eq!(claims[0].category, ClaimCategory::ProductSpecification);
        assert_eq!(claims[1].category, ClaimCategory::EventOrAnnouncement);
        assert_eq!(claims[1].agent, "codex");
        assert!(parse_claims("no block here", "a").is_empty());
        assert!(parse_claims("```claims\nnot json\n```", "a").is_empty());
        // An object with a `claims` array, and a plain json fence, are tolerated.
        assert_eq!(
            parse_claims(
                "```json\n{\"claims\":[{\"claim\":\"Something specific happened in 2024\"}]}\n```",
                "a"
            )
            .len(),
            1
        );
        assert!(parse_claims("```json\n{\"unrelated\": true}\n```", "a").is_empty());
    }

    #[test]
    fn the_claims_block_is_removed_from_the_visible_text_only() {
        let reply = "Answer text.\n\n```claims\n[]\n```\n\n```rust\nlet x = 1;\n```\nEnd";
        let shown = strip_claims_block(reply);
        assert!(
            !shown.contains("claims") && shown.contains("let x = 1;") && shown.contains("End"),
            "{shown}"
        );
    }

    #[test]
    fn the_same_claim_from_several_agents_is_merged_and_ranked_but_not_promoted() {
        let merged = merge_claims(
            vec![
                raw("The Model X has 100 MHz of analog bandwidth", "claude"),
                raw("Model X has 100 MHz analog bandwidth", "codex"),
                raw("The vendor ships a carrying case with it", "google"),
                raw("Model X has 100 MHz analog bandwidth.", "google"),
            ],
            10,
        );
        assert_eq!(merged.len(), 2);
        assert_eq!(
            merged[0].originating_agents,
            ["claude", "codex", "google"],
            "everyone who made it is recorded"
        );
        assert_eq!(merged[0].id, 1);
        assert_eq!(
            merged[0].status,
            ClaimStatus::NotChecked,
            "agreement does not make a claim supported"
        );
        assert!(merged[0].evidence.is_empty());
        assert_eq!(
            merge_claims(
                (0..30)
                    .map(|i| raw(
                        &format!("distinct statement number {i} about topic {i}{i}"),
                        "a"
                    ))
                    .collect(),
                5
            )
            .len(),
            5
        );
    }

    fn page_set(texts: &[(&str, &str)]) -> SourceSet {
        let mut set = SourceSet::new();
        for (url, text) in texts {
            let id = set
                .add_candidate(
                    url,
                    Discovery {
                        method: DiscoveryMethod::UserProvided,
                        provider: None,
                    },
                )
                .unwrap();
            set.apply_fetch(
                id,
                Ok(FetchedPage {
                    requested_url: (*url).into(),
                    final_url: (*url).into(),
                    redirects: vec![],
                    status: 200,
                    content_type: "text/html".into(),
                    bytes: text.len(),
                    content_hash: "0".repeat(64),
                    retrieved_at_ms: 1,
                    extracted: Extracted {
                        text: (*text).into(),
                        ..Default::default()
                    },
                }),
            );
        }
        compute_upstream_groups(&mut set, 0.6);
        set
    }

    fn link(source_id: u32, relation: Relation) -> EvidenceLink {
        EvidenceLink {
            source_id,
            relation,
            passage: "passage".into(),
            offset: 0,
            assessed_by: vec!["codex".into()],
            source_kind: SourceKind::Unclassified,
            suitability: Suitability::Moderate,
            suitability_note: "n".into(),
            freshness: Freshness::Unknown,
            reason: String::new(),
        }
    }

    fn claim_with(evidence: Vec<EvidenceLink>) -> ClaimRecord {
        let mut c = merge_claims(
            vec![raw("Model X has 100 MHz analog bandwidth", "claude")],
            5,
        )
        .remove(0);
        c.evidence = evidence;
        c
    }

    const A: &str = "The Model X offers 100 MHz of analog bandwidth, four channels, one gigasample per second and a seven inch display with twelve million points of memory per channel.";
    const B: &str = "Independent bench test of the unit found the minus three decibel point at ninety six megahertz, below the advertised figure, on every channel we tried in the lab.";

    #[test]
    fn status_follows_the_evidence_links_and_nothing_else() {
        let set = page_set(&[
            ("https://maker.example/spec", A),
            ("https://lab.example/test", B),
        ]);
        let status = |ev: Vec<EvidenceLink>, ran: bool| {
            let mut c = claim_with(ev);
            decide_status(&mut c, &set, ran);
            c.status
        };
        assert_eq!(
            status(vec![link(1, Relation::Supports)], true),
            ClaimStatus::Supported
        );
        assert_eq!(
            status(vec![link(2, Relation::Contradicts)], true),
            ClaimStatus::Contradicted
        );
        assert_eq!(
            status(
                vec![link(1, Relation::Supports), link(2, Relation::Contradicts)],
                true
            ),
            ClaimStatus::MixedEvidence
        );
        assert_eq!(status(vec![], true), ClaimStatus::InsufficientEvidence);
        assert_eq!(
            status(
                vec![link(1, Relation::Unrelated), link(2, Relation::Unclear)],
                true
            ),
            ClaimStatus::InsufficientEvidence,
            "a page that exists but says nothing relevant is not support"
        );
        assert_eq!(
            status(vec![link(1, Relation::Supports)], false),
            ClaimStatus::NotChecked,
            "no verification, no verdict"
        );
    }

    #[test]
    fn many_pages_repeating_one_original_count_once() {
        let copy1 = format!("Reposted: {A} Thanks for reading.");
        let copy2 = format!("{A} (source: the manufacturer)");
        let set = page_set(&[
            ("https://maker.example/spec", A),
            ("https://news.example/a", &copy1),
            ("https://blog.example/b", &copy2),
        ]);
        let mut c = claim_with(vec![
            link(1, Relation::Supports),
            link(2, Relation::Supports),
            link(3, Relation::Supports),
        ]);
        decide_status(&mut c, &set, true);
        assert_eq!(c.status, ClaimStatus::Supported);
        assert_eq!(
            c.supporting_independent_sources, 1,
            "three copies, one origin"
        );
        assert!(
            c.notes.iter().any(|n| n.contains("count once"))
                && c.notes
                    .iter()
                    .any(|n| n.contains("single independent source")),
            "{:?}",
            c.notes
        );
    }

    #[test]
    fn status_notes_flag_weak_and_stale_support_and_are_recomputed_not_accumulated() {
        let set = page_set(&[("https://forum.example/t", A)]);
        let mut weak = link(1, Relation::Supports);
        weak.suitability = Suitability::Weak;
        weak.freshness = Freshness::Stale;
        let mut c = claim_with(vec![weak]);
        decide_status(&mut c, &set, true);
        decide_status(&mut c, &set, true);
        assert!(
            c.notes.iter().any(|n| n.contains("weak evidence"))
                && c.notes
                    .iter()
                    .any(|n| n.contains("older than the requested freshness"))
        );
        assert_eq!(
            c.notes.len(),
            3,
            "the automatic notes are replaced on each computation: {:?}",
            c.notes
        );
    }

    #[test]
    fn statuses_serialize_with_the_documented_names() {
        for (s, name) in [
            (ClaimStatus::Supported, "SUPPORTED"),
            (ClaimStatus::Contradicted, "CONTRADICTED"),
            (ClaimStatus::MixedEvidence, "MIXED_EVIDENCE"),
            (ClaimStatus::InsufficientEvidence, "INSUFFICIENT_EVIDENCE"),
            (ClaimStatus::NotChecked, "NOT_CHECKED"),
        ] {
            assert_eq!(serde_json::to_value(s).unwrap(), name);
            assert_eq!(s.as_str(), name);
        }
        assert_eq!(Relation::parse(" Supports "), Some(Relation::Supports));
        assert_eq!(Relation::parse("refutes"), Some(Relation::Contradicts));
        assert_eq!(Relation::parse("banana"), None);
    }
}
