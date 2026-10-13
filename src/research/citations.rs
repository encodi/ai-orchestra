//! Citations: `[1]` markers in an answer must point at sources that exist in the run, and the
//! list of sources shown to the user is generated here from the recorded sources, never written by
//! a model. A citation to a source that was never discovered or supplied is removed, a citation to
//! a source that could not be retrieved is flagged, and a source that was not retrieved is never
//! presented as checked.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::claims::{ClaimRecord, ClaimStatus, Relation};
use super::source::{SourceRecord, SourceSet, SourceStatus};
use crate::history::format_utc;

/// The citation markers found in a text, split by whether they can be honored.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CitationCheck {
    /// Valid markers: the source exists and was retrieved.
    pub cited: BTreeSet<u32>,
    /// Markers that name no source of this run (never discovered or supplied).
    pub unknown: BTreeSet<u32>,
    /// Markers that name a source that exists but could not be retrieved.
    pub not_retrieved: BTreeSet<u32>,
}

impl CitationCheck {
    pub fn is_clean(&self) -> bool {
        self.unknown.is_empty() && self.not_retrieved.is_empty()
    }
}

/// A `[n]` / `[n, m]` marker: its byte range and the numbers inside.
fn markers(text: &str) -> Vec<(usize, usize, Vec<u32>)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'['
            && let Some(rel) = text[i + 1..].find(']')
        {
            let inner = &text[i + 1..i + 1 + rel];
            let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
            if !inner.is_empty()
                && inner.len() <= 24
                && parts
                    .iter()
                    .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) && p.len() <= 6)
            {
                let nums: Vec<u32> = parts.iter().filter_map(|p| p.parse().ok()).collect();
                out.push((i, i + rel + 2, nums));
                i += rel + 2;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Every citation number that appears in `text` as a marker such as `[1]` or `[2, 3]`.
pub fn citation_numbers(text: &str) -> BTreeSet<u32> {
    markers(text)
        .into_iter()
        .flat_map(|(_, _, nums)| nums)
        .collect()
}

pub fn check_citations(text: &str, set: &SourceSet) -> CitationCheck {
    let mut check = CitationCheck::default();
    for (_, _, nums) in markers(text) {
        for n in nums {
            match set.get(n) {
                None => {
                    check.unknown.insert(n);
                }
                Some(r) if !r.is_retrieved() => {
                    check.not_retrieved.insert(n);
                }
                Some(_) => {
                    check.cited.insert(n);
                }
            }
        }
    }
    check
}

/// Rewrites the text so that no marker points at a missing or unretrieved source: invalid numbers
/// are dropped from a marker, and a marker left with none becomes `[unsupported citation removed]`.
pub fn sanitize_citations(text: &str, set: &SourceSet) -> String {
    let mut out = String::new();
    let mut last = 0;
    for (start, end, nums) in markers(text) {
        out.push_str(&text[last..start]);
        let valid: Vec<u32> = nums
            .iter()
            .copied()
            .filter(|n| set.get(*n).is_some_and(SourceRecord::is_retrieved))
            .collect();
        if valid.is_empty() {
            out.push_str("[unsupported citation removed]");
        } else {
            let _ = write!(
                out,
                "[{}]",
                valid
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        last = end;
    }
    out.push_str(&text[last..]);
    out
}

fn date_of(ms: u64) -> String {
    format_utc(ms).chars().take(10).collect()
}

fn describe(r: &SourceRecord) -> String {
    let mut line = format!(
        "[{}] {} — <{}>", // `<url>` is a Markdown autolink and keeps the full URL visible in a terminal
        r.id,
        r.title.as_deref().unwrap_or("(no title found)"),
        r.display_url()
    );
    let mut dates = Vec::new();
    match &r.published {
        Some(d) => dates.push(format!("published {d}")),
        None => dates.push("publication date not stated".to_string()),
    }
    if let Some(d) = &r.updated {
        dates.push(format!("updated {d}"));
    }
    if let Some(ms) = r.retrieved_at_ms {
        dates.push(format!("retrieved {}", date_of(ms)));
    }
    let _ = write!(line, " ({})", dates.join(", "));
    line
}

/// The "Sources" part of the final answer, generated from the records. `cited` marks which ones the
/// answer actually cites; sources that were not retrieved are listed apart and described as
/// not checked.
pub fn render_sources(set: &SourceSet, cited: &BTreeSet<u32>) -> String {
    let mut out = String::new();
    let retrieved: Vec<&SourceRecord> = set.records().iter().filter(|r| r.is_retrieved()).collect();
    if retrieved.is_empty() {
        out.push_str("## Sources\n\nNo source could be retrieved, so nothing in this answer is backed by a retrieved source.\n");
    } else {
        out.push_str("## Sources\n\n");
        for r in &retrieved {
            let _ = writeln!(
                out,
                "{}{}",
                describe(r),
                if cited.contains(&r.id) {
                    ""
                } else {
                    " — retrieved, not cited above"
                }
            );
        }
    }
    let others: Vec<&SourceRecord> = set
        .records()
        .iter()
        .filter(|r| {
            !r.is_retrieved()
                && !matches!(
                    r.status,
                    SourceStatus::NotAttempted | SourceStatus::NotSelected
                )
        })
        .collect();
    let unused: Vec<&SourceRecord> = set
        .records()
        .iter()
        .filter(|r| r.status == SourceStatus::NotSelected)
        .collect();
    if !others.is_empty() {
        out.push_str("\n## Consulted but not retrieved (these were NOT checked)\n\n");
        for r in others {
            let _ = writeln!(
                out,
                "[{}] {} — {}: {}",
                r.id,
                r.original_url,
                match r.status {
                    SourceStatus::Blocked => "refused",
                    SourceStatus::Unsupported => "unsupported format",
                    _ => "could not be reached",
                },
                r.error.as_deref().unwrap_or("no details")
            );
        }
    }
    if !unused.is_empty() {
        out.push_str("\n## Retrieved but not used as evidence (over the source limit; see `--max-sources`)\n\n");
        for r in unused {
            let _ = writeln!(
                out,
                "[{}] {} — <{}>",
                r.id,
                r.title.as_deref().unwrap_or("(no title found)"),
                r.display_url()
            );
        }
    }
    out
}

/// A short table of the claims and their status, generated from the claim records.
pub fn render_claims(claims: &[ClaimRecord]) -> String {
    if claims.is_empty() {
        return String::new();
    }
    let mut out = String::from("## Claim verification\n\n");
    out.push_str("Status is computed from passages retrieved from the sources, not from how many assistants repeated a claim. The assessment of whether a passage supports a claim is made by an AI model and remains model-dependent.\n\n");
    for c in claims {
        let _ = writeln!(
            out,
            "- **{}** — {} (claim {}; made by {})",
            c.status.as_str(),
            c.text,
            c.id,
            c.originating_agents.join(", ")
        );
        let by = |rel: Relation| -> Vec<String> {
            let mut v: Vec<u32> = c
                .evidence
                .iter()
                .filter(|e| e.relation == rel)
                .map(|e| e.source_id)
                .collect();
            v.sort_unstable();
            v.dedup();
            v.iter().map(|n| format!("[{n}]")).collect()
        };
        let (sup, con) = (by(Relation::Supports), by(Relation::Contradicts));
        if !sup.is_empty() {
            let _ = writeln!(
                out,
                "  - supported by {} ({} independent source(s))",
                sup.join(" "),
                c.supporting_independent_sources
            );
        }
        if !con.is_empty() {
            let _ = writeln!(
                out,
                "  - contradicted by {} ({} independent source(s))",
                con.join(" "),
                c.contradicting_independent_sources
            );
        }
        for n in &c.notes {
            let _ = writeln!(out, "  - note: {}", n.trim_start_matches("auto: "));
        }
        if c.status == ClaimStatus::NotChecked {
            let _ = writeln!(out, "  - not verified against any retrieved source");
        }
    }
    out
}

/// Machine-readable citation metadata: one entry per source with its retrieval status and the
/// claims it supports or contradicts.
pub fn citations_json(set: &SourceSet, claims: &[ClaimRecord], cited: &BTreeSet<u32>) -> Value {
    Value::Array(
        set.records()
            .iter()
            .map(|r| {
                let supports: Vec<u32> = claims
                    .iter()
                    .filter(|c| {
                        c.evidence
                            .iter()
                            .any(|e| e.source_id == r.id && e.relation == Relation::Supports)
                    })
                    .map(|c| c.id)
                    .collect();
                let contradicts: Vec<u32> = claims
                    .iter()
                    .filter(|c| {
                        c.evidence
                            .iter()
                            .any(|e| e.source_id == r.id && e.relation == Relation::Contradicts)
                    })
                    .map(|c| c.id)
                    .collect();
                json!({
                    "id": r.id,
                    "url": r.display_url(),
                    "original_url": r.original_url,
                    "title": r.title,
                    "published": r.published,
                    "updated": r.updated,
                    "retrieved": r.retrieved_at_ms.map(date_of),
                    "status": r.status,
                    "cited_in_answer": cited.contains(&r.id),
                    "supports_claims": supports,
                    "contradicts_claims": contradicts,
                })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::research::fetch::FetchedPage;
    use crate::research::html::Extracted;
    use crate::research::net::FetchError;
    use crate::research::source::{Discovery, DiscoveryMethod};

    fn set() -> SourceSet {
        let mut set = SourceSet::new();
        let d = || Discovery {
            method: DiscoveryMethod::UserProvided,
            provider: None,
        };
        let a = set
            .add_candidate("https://maker.example/spec", d())
            .unwrap();
        let b = set
            .add_candidate("https://lab.example/report", d())
            .unwrap();
        let c = set.add_candidate("https://down.example/page", d()).unwrap();
        for (id, url, title, published) in [
            (
                a,
                "https://maker.example/spec",
                "Model X specs",
                Some("2025-03-04"),
            ),
            (b, "https://lab.example/report", "Bench report", None),
        ] {
            set.apply_fetch(
                id,
                Ok(FetchedPage {
                    requested_url: url.into(),
                    final_url: url.into(),
                    redirects: vec![],
                    status: 200,
                    content_type: "text/html".into(),
                    bytes: 10,
                    content_hash: "0".repeat(64),
                    retrieved_at_ms: 1_791_676_800_000,
                    extracted: Extracted {
                        title: Some(title.into()),
                        published: published.map(String::from),
                        text: "text ".repeat(100),
                        ..Default::default()
                    },
                }),
            );
        }
        set.apply_fetch(c, Err(FetchError::HttpStatus(503)));
        set
    }

    #[test]
    fn markers_are_validated_against_the_recorded_sources() {
        let s = set();
        let text = "The bandwidth is 100 MHz [1]. A test found 70 MHz [2]. Also see [3] and [9, 1]. Not a citation: [x], [1.5], [a, 2], and [].";
        let check = check_citations(text, &s);
        assert_eq!(check.cited, BTreeSet::from([1, 2]));
        assert_eq!(
            check.not_retrieved,
            BTreeSet::from([3]),
            "source 3 exists but was not retrieved"
        );
        assert_eq!(
            check.unknown,
            BTreeSet::from([9]),
            "source 9 was never discovered"
        );
        assert!(!check.is_clean());
        assert!(check_citations("Plain text without citations.", &s).is_clean());
    }

    #[test]
    fn fabricated_and_unretrieved_citations_are_removed_from_the_text() {
        let s = set();
        let out = sanitize_citations("Fact [1]. Invented [9]. Mixed [1, 9, 3]. Down [3].", &s);
        assert_eq!(
            out,
            "Fact [1]. Invented [unsupported citation removed]. Mixed [1]. Down [unsupported citation removed]."
        );
        // Text without markers, and non-citation brackets, is untouched.
        assert_eq!(sanitize_citations("a [x] b [1.5] c", &s), "a [x] b [1.5] c");
    }

    #[test]
    fn the_sources_list_is_generated_from_records_with_full_urls_and_honest_dates() {
        let s = set();
        let text = render_sources(&s, &BTreeSet::from([1]));
        assert!(text.contains("[1] Model X specs — <https://maker.example/spec> (published 2025-03-04, retrieved 2026-10-11)"), "{text}");
        assert!(text.contains("[2] Bench report — <https://lab.example/report> (publication date not stated, retrieved 2026-10-11) — retrieved, not cited above"), "{text}");
        assert!(text.contains("## Consulted but not retrieved (these were NOT checked)"));
        assert!(
            text.contains("[3] https://down.example/page — could not be reached: HTTP status 503"),
            "{text}"
        );
        // Nothing retrieved: say so, do not imply anything was checked.
        let mut none = SourceSet::new();
        let id = none
            .add_candidate(
                "https://x.example/a",
                Discovery {
                    method: DiscoveryMethod::UserProvided,
                    provider: None,
                },
            )
            .unwrap();
        none.apply_fetch(id, Err(FetchError::Timeout(5)));
        let t = render_sources(&none, &BTreeSet::new());
        assert!(
            t.contains("No source could be retrieved") && t.contains("NOT checked"),
            "{t}"
        );
    }

    #[test]
    fn citation_metadata_json_links_sources_to_claims() {
        let s = set();
        let mut claims = crate::research::claims::merge_claims(
            vec![crate::research::claims::RawClaim {
                text: "Model X has 100 MHz of bandwidth".into(),
                category: crate::research::quality::ClaimCategory::ProductSpecification,
                sources: vec![],
                needs_external_evidence: true,
                agent: "claude".into(),
            }],
            3,
        );
        claims[0]
            .evidence
            .push(crate::research::claims::EvidenceLink {
                source_id: 1,
                relation: Relation::Supports,
                passage: "p".into(),
                offset: 0,
                assessed_by: vec!["codex".into()],
                source_kind: crate::research::quality::SourceKind::Unclassified,
                suitability: crate::research::quality::Suitability::Moderate,
                suitability_note: "n".into(),
                freshness: crate::research::quality::Freshness::Unknown,
                reason: String::new(),
            });
        let v = citations_json(&s, &claims, &BTreeSet::from([1]));
        assert_eq!(v[0]["id"], 1);
        assert_eq!(v[0]["supports_claims"], json!([1]));
        assert_eq!(v[0]["cited_in_answer"], true);
        assert_eq!(v[0]["retrieved"], "2026-10-11");
        assert_eq!(
            v[1]["published"],
            Value::Null,
            "an unstated date is null, not invented"
        );
        assert_eq!(v[2]["status"], "inaccessible");
    }

    #[test]
    fn the_claim_table_never_calls_an_unchecked_claim_verified() {
        let mut c = crate::research::claims::merge_claims(
            vec![crate::research::claims::RawClaim {
                text: "Something that was never checked at all".into(),
                category: crate::research::quality::ClaimCategory::Other,
                sources: vec![],
                needs_external_evidence: true,
                agent: "codex".into(),
            }],
            3,
        );
        let text = render_claims(&c);
        assert!(
            text.contains("**NOT_CHECKED**")
                && text.contains("not verified against any retrieved source")
                && text.contains("model-dependent"),
            "{text}"
        );
        c.clear();
        assert_eq!(render_claims(&c), "");
    }
}
