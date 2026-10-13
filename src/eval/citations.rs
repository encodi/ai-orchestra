//! Citation and evidence checks for an answer.
//!
//! Two different questions are kept apart:
//!
//! * **Mechanical validity** (deterministic, certain): does `[n]` name a source this run actually has,
//!   was that source retrieved, are its dates recorded, does the answer link to pages that are not
//!   in the source list, does it say "verified" with no retrieved evidence behind it, does it make a
//!   required claim without citing the evidence for it?
//! * **Support** (a different question, and not answered here): does the cited passage actually say
//!   what the sentence says? A citation that points at a real page is not thereby evidence for the
//!   claim. This module only reports a *lexical overlap* between the sentence and the passage, labelled
//!   as such. It is a flag for a human or a verifier to look at, never "semantically verified".

use serde::{Deserialize, Serialize};

use super::dataset::{RequiredClaim, ResponseSource};

pub const METHOD_NOTE: &str = "Structural checks are exact. Support is a lexical overlap between the citing sentence and the passage held for the source: a flag to look at, not a semantic judgement.";

/// Fraction of the sentence's content words found in the passage.
pub const STRONG_OVERLAP: f64 = 0.5;
pub const WEAK_OVERLAP: f64 = 0.25;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Structural {
    Valid,
    /// The marker names no source of this answer.
    UnknownId,
    /// The source exists but the program never retrieved it, so it is not evidence.
    NotRetrieved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LexicalSupport {
    Strong,
    Weak,
    None,
    /// No passage, or no content words, to compare.
    NotAssessed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CitationCheck {
    pub marker: u32,
    /// The sentence that carries the marker.
    pub sentence: String,
    pub structural: Structural,
    pub lexical_overlap: Option<f64>,
    pub support: LexicalSupport,
    /// The passage the overlap was computed against (kept so a reviewer can read it).
    pub passage: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimEvidence {
    /// The answer does not make the claim at all.
    Absent,
    /// It makes the claim without citing the evidence for it.
    MadeUncited,
    MadeAndCited,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequiredClaimResult {
    pub description: String,
    pub result: ClaimEvidence,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CitationReport {
    pub checks: Vec<CitationCheck>,
    pub structurally_valid: usize,
    pub unknown_ids: Vec<u32>,
    pub not_retrieved: Vec<u32>,
    pub strong: usize,
    pub weak: usize,
    pub unsupported: usize,
    /// Links in the answer that are not in the source list.
    pub unlisted_urls: Vec<String>,
    /// Phrases that claim verification when nothing was retrieved to verify against.
    pub unsupported_labels: Vec<String>,
    pub sources_total: usize,
    pub sources_retrieved: usize,
    /// Sources with no publication date or no retrieval date recorded.
    pub sources_missing_dates: usize,
    pub required_claims: Vec<RequiredClaimResult>,
    pub method: String,
}

impl CitationReport {
    /// Share of citations whose marker is structurally valid; `None` when there are no citations.
    pub fn validity_rate(&self) -> Option<f64> {
        (!self.checks.is_empty()).then(|| self.structurally_valid as f64 / self.checks.len() as f64)
    }

    /// Mechanical problems only (support is a separate matter).
    pub fn mechanical_problems(&self) -> usize {
        self.checks.len() - self.structurally_valid
            + self.unlisted_urls.len()
            + self.unsupported_labels.len()
            + self
                .required_claims
                .iter()
                .filter(|r| r.result != ClaimEvidence::MadeAndCited)
                .count()
    }
}

const STOP: [&str; 40] = [
    "that", "this", "with", "from", "have", "has", "are", "was", "were", "the", "and", "for",
    "not", "but", "its", "which", "their", "there", "would", "could", "should", "about", "into",
    "than", "then", "also", "more", "most", "such", "only", "when", "what", "where", "will", "can",
    "may", "any", "all", "one", "each",
];

fn content_words(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in text.split(|c: char| !c.is_alphanumeric() && c != '.') {
        let w = w.trim_matches('.').to_lowercase();
        if w.is_empty() {
            continue;
        }
        let keep = w.chars().any(|c| c.is_ascii_digit())
            || (w.chars().count() >= 4 && !STOP.contains(&w.as_str()));
        if keep && !out.contains(&w) {
            out.push(w);
        }
    }
    out
}

/// `(marker ids, sentence without markers)` for each sentence that cites.
fn cited_sentences(answer: &str) -> Vec<(Vec<u32>, String)> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut ids: Vec<u32> = Vec::new();
    let chars: Vec<char> = answer.chars().collect();
    let mut i = 0;
    let flush = |current: &mut String, ids: &mut Vec<u32>, out: &mut Vec<(Vec<u32>, String)>| {
        if !ids.is_empty() {
            out.push((std::mem::take(ids), current.trim().to_string()));
        }
        current.clear();
    };
    while i < chars.len() {
        let c = chars[i];
        if c == '[' {
            // `[3]`, `[1, 2]` or `[1][2]`
            if let Some(close) = chars[i..].iter().position(|x| *x == ']') {
                let inner: String = chars[i + 1..i + close].iter().collect();
                let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
                if !parts.is_empty()
                    && parts
                        .iter()
                        .all(|p| !p.is_empty() && p.chars().all(|d| d.is_ascii_digit()))
                {
                    for p in parts {
                        if let Ok(n) = p.parse::<u32>() {
                            ids.push(n);
                        }
                    }
                    i += close + 1;
                    continue;
                }
            }
        }
        if matches!(c, '\n' | '?' | '!')
            || (c == '.' && chars.get(i + 1).is_none_or(|n| n.is_whitespace()))
        {
            // A marker right after the full stop belongs to the sentence that just ended.
            let mut j = i + 1;
            while j < chars.len() && chars[j] == '[' {
                match chars[j..].iter().position(|x| *x == ']') {
                    Some(close) => {
                        let inner: String = chars[j + 1..j + close].iter().collect();
                        if inner.split(',').all(|p| {
                            p.trim().chars().all(|d| d.is_ascii_digit()) && !p.trim().is_empty()
                        }) {
                            for p in inner.split(',') {
                                if let Ok(n) = p.trim().parse::<u32>() {
                                    ids.push(n);
                                }
                            }
                            j += close + 1;
                        } else {
                            break;
                        }
                    }
                    None => break,
                }
            }
            flush(&mut current, &mut ids, &mut out);
            i = j;
            continue;
        }
        current.push(c);
        i += 1;
    }
    flush(&mut current, &mut ids, &mut out);
    out
}

fn urls_in(answer: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in answer.split_whitespace() {
        let w = w.trim_matches(|c: char| {
            matches!(
                c,
                '(' | ')' | '[' | ']' | '<' | '>' | ',' | ';' | '"' | '\''
            )
        });
        if w.starts_with("http://") || w.starts_with("https://") {
            let w = w.trim_end_matches(['.', ')']);
            if !out.iter().any(|x| x == w) {
                out.push(w.to_string());
            }
        }
    }
    out
}

const CLAIM_PHRASES: [&str; 6] = [
    "has been verified",
    "verified against",
    "verified by",
    "independently verified",
    "confirmed by the sources",
    "supported by the evidence",
];

fn unsupported_labels(answer: &str, retrieved: usize) -> Vec<String> {
    if retrieved > 0 {
        return Vec::new();
    }
    let lower = answer.to_lowercase();
    let mut out = Vec::new();
    for phrase in CLAIM_PHRASES {
        let mut from = 0;
        while let Some(i) = lower[from..].find(phrase) {
            let at = from + i;
            // Negation anywhere earlier in the same sentence ("nothing here has been verified ...").
            let sentence_start = lower[..at]
                .rfind(['.', '\n', '!', '?'])
                .map_or(0, |i| i + 1);
            let negated = lower[sentence_start..at]
                .split(|c: char| !c.is_alphanumeric() && c != '\'')
                .any(|w| {
                    [
                        "not", "no", "nothing", "never", "none", "without", "cannot", "hasn't",
                        "isn't", "wasn't", "can't",
                    ]
                    .contains(&w)
                });
            if !negated && !out.iter().any(|o: &String| o == phrase) {
                out.push(phrase.to_string());
            }
            from = at + phrase.len();
        }
    }
    if answer.contains("SUPPORTED")
        && !answer.contains("NOT SUPPORTED")
        && !out.iter().any(|o| o == "SUPPORTED")
    {
        out.push("SUPPORTED".to_string());
    }
    out
}

/// Checks `answer` against the sources its run recorded, and against the claims the case requires.
pub fn check(
    answer: &str,
    sources: &[ResponseSource],
    required: &[RequiredClaim],
) -> CitationReport {
    let mut checks = Vec::new();
    for (ids, sentence) in cited_sentences(answer) {
        for id in ids {
            let src = sources.iter().find(|s| s.id == id);
            let structural = match src {
                None => Structural::UnknownId,
                Some(s) if !s.retrieved => Structural::NotRetrieved,
                Some(_) => Structural::Valid,
            };
            let (overlap, support, passage) =
                match src.filter(|s| s.retrieved && !s.passage.trim().is_empty()) {
                    Some(s) => {
                        let words = content_words(&sentence);
                        if words.is_empty() {
                            (None, LexicalSupport::NotAssessed, Some(s.passage.clone()))
                        } else {
                            let have = content_words(&s.passage);
                            let hit = words.iter().filter(|w| have.contains(w)).count();
                            let ratio = hit as f64 / words.len() as f64;
                            let support = if ratio >= STRONG_OVERLAP {
                                LexicalSupport::Strong
                            } else if ratio >= WEAK_OVERLAP {
                                LexicalSupport::Weak
                            } else {
                                LexicalSupport::None
                            };
                            (
                                Some((ratio * 1000.0).round() / 1000.0),
                                support,
                                Some(s.passage.clone()),
                            )
                        }
                    }
                    None => (None, LexicalSupport::NotAssessed, None),
                };
            checks.push(CitationCheck {
                marker: id,
                sentence: sentence.clone(),
                structural,
                lexical_overlap: overlap,
                support,
                passage,
            });
        }
    }
    let retrieved = sources.iter().filter(|s| s.retrieved).count();
    let listed: Vec<&str> = sources.iter().map(|s| s.url.as_str()).collect();
    let unlisted_urls = urls_in(answer)
        .into_iter()
        .filter(|u| {
            !listed
                .iter()
                .any(|l| l.trim_end_matches('/') == u.trim_end_matches('/'))
        })
        .collect();
    let cited_ids: Vec<u32> = checks.iter().map(|c| c.marker).collect();
    let lower = answer.to_lowercase();
    let required_claims = required
        .iter()
        .map(|rc| {
            let made = rc
                .any_of
                .iter()
                .any(|s| !s.trim().is_empty() && lower.contains(&s.to_lowercase()));
            let cited = rc.source_ids.iter().any(|id| {
                checks
                    .iter()
                    .any(|c| c.marker == *id && c.structural == Structural::Valid)
                    && cited_ids.contains(id)
            });
            RequiredClaimResult {
                description: rc.description.clone(),
                result: match (made, cited) {
                    (false, _) => ClaimEvidence::Absent,
                    (true, false) => ClaimEvidence::MadeUncited,
                    (true, true) => ClaimEvidence::MadeAndCited,
                },
            }
        })
        .collect();
    CitationReport {
        structurally_valid: checks
            .iter()
            .filter(|c| c.structural == Structural::Valid)
            .count(),
        unknown_ids: dedup(
            checks
                .iter()
                .filter(|c| c.structural == Structural::UnknownId)
                .map(|c| c.marker),
        ),
        not_retrieved: dedup(
            checks
                .iter()
                .filter(|c| c.structural == Structural::NotRetrieved)
                .map(|c| c.marker),
        ),
        strong: checks
            .iter()
            .filter(|c| c.support == LexicalSupport::Strong)
            .count(),
        weak: checks
            .iter()
            .filter(|c| c.support == LexicalSupport::Weak)
            .count(),
        unsupported: checks
            .iter()
            .filter(|c| c.support == LexicalSupport::None)
            .count(),
        checks,
        unlisted_urls,
        unsupported_labels: unsupported_labels(answer, retrieved),
        sources_total: sources.len(),
        sources_retrieved: retrieved,
        sources_missing_dates: sources
            .iter()
            .filter(|s| s.published.is_none() || s.retrieved_at.is_none())
            .count(),
        required_claims,
        method: METHOD_NOTE.to_string(),
    }
}

fn dedup(it: impl Iterator<Item = u32>) -> Vec<u32> {
    let mut v: Vec<u32> = it.collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// The sources of a research report, as the checks need them.
pub fn sources_from_report(r: &crate::research::engine::ResearchReport) -> Vec<ResponseSource> {
    r.sources
        .iter()
        .map(|s| ResponseSource {
            id: s.id,
            url: s.display_url().to_string(),
            retrieved: s.status == crate::research::source::SourceStatus::Retrieved,
            published: s.published.clone(),
            retrieved_at: s.retrieved_at_ms.map(crate::history::format_utc),
            passage: s.text_preview.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(id: u32, url: &str, retrieved: bool, passage: &str) -> ResponseSource {
        ResponseSource {
            id,
            url: url.into(),
            retrieved,
            published: Some("2026-01-01".into()),
            retrieved_at: Some("2026-10-12".into()),
            passage: passage.into(),
        }
    }

    const PASSAGE: &str =
        "The Model X oscilloscope offers 100 MHz of analog bandwidth and four channels.";

    #[test]
    fn a_valid_citation_to_a_supporting_passage_is_strong_but_only_lexically() {
        let r = check(
            "The Model X offers 100 MHz of analog bandwidth [1].",
            &[src(1, "https://a.example/x", true, PASSAGE)],
            &[],
        );
        assert_eq!((r.checks.len(), r.structurally_valid, r.strong), (1, 1, 1));
        assert_eq!(r.validity_rate(), Some(1.0));
        assert!(r.method.contains("not a semantic judgement"));
        assert_eq!(r.mechanical_problems(), 0);
    }

    #[test]
    fn a_real_looking_citation_to_an_unrelated_page_is_valid_mechanically_and_unsupported() {
        // The adversarial case: the source exists, was retrieved, has dates and a believable URL.
        let r = check(
            "The Model X has a 1 GHz bandwidth and wireless charging [1].",
            &[src(
                1,
                "https://reviews.example/model-x-review",
                true,
                "A general guide to choosing a multimeter for household wiring and batteries.",
            )],
            &[],
        );
        assert_eq!(r.structurally_valid, 1, "structurally it is fine");
        assert_eq!(
            (r.strong, r.weak, r.unsupported),
            (0, 0, 1),
            "{:?}",
            r.checks
        );
        assert_eq!(
            r.mechanical_problems(),
            0,
            "mechanical validity does not mean support"
        );
    }

    #[test]
    fn unknown_unretrieved_and_unlisted_sources_are_flagged() {
        let r = check(
            "It has 100 MHz [1]. It also has 4 channels [2][7]. See https://made-up.example/spec and https://a.example/x for more.",
            &[
                src(1, "https://a.example/x", true, PASSAGE),
                src(2, "https://b.example/y", false, ""),
            ],
            &[],
        );
        assert_eq!(r.unknown_ids, vec![7]);
        assert_eq!(r.not_retrieved, vec![2]);
        assert_eq!(
            r.unlisted_urls,
            vec!["https://made-up.example/spec".to_string()]
        );
        assert_eq!(r.structurally_valid, 1);
        assert!(r.validity_rate().unwrap() < 0.5);
        assert_eq!(
            r.checks.iter().find(|c| c.marker == 2).unwrap().support,
            LexicalSupport::NotAssessed
        );
    }

    #[test]
    fn verification_language_with_nothing_retrieved_is_flagged_but_honest_negations_are_not() {
        let none = check(
            "Nothing here has been verified against external sources.",
            &[],
            &[],
        );
        assert!(
            none.unsupported_labels.is_empty(),
            "{:?}",
            none.unsupported_labels
        );
        let bad = check(
            "The specification has been verified against the manufacturer. Status: SUPPORTED.",
            &[],
            &[],
        );
        assert!(
            bad.unsupported_labels.len() >= 2
                && bad.unsupported_labels.iter().any(|l| l == "SUPPORTED"),
            "{:?}",
            bad.unsupported_labels
        );
        let with = check(
            "The specification has been verified against the page [1].",
            &[src(1, "https://a.example/x", true, PASSAGE)],
            &[],
        );
        assert!(with.unsupported_labels.is_empty());
    }

    #[test]
    fn required_claims_need_their_evidence_cited() {
        let rc = vec![RequiredClaim {
            any_of: vec!["100 MHz".into()],
            source_ids: vec![1],
            description: "the bandwidth".into(),
        }];
        let s = [src(1, "https://a.example/x", true, PASSAGE)];
        assert_eq!(
            check("It has 100 MHz [1].", &s, &rc).required_claims[0].result,
            ClaimEvidence::MadeAndCited
        );
        assert_eq!(
            check("It has 100 MHz.", &s, &rc).required_claims[0].result,
            ClaimEvidence::MadeUncited
        );
        assert_eq!(
            check("It is fast.", &s, &rc).required_claims[0].result,
            ClaimEvidence::Absent
        );
        assert_eq!(
            check("It has 100 MHz [3].", &s, &rc).required_claims[0].result,
            ClaimEvidence::MadeUncited,
            "an unknown id is not evidence"
        );
    }

    #[test]
    fn multi_citations_and_missing_dates_are_handled() {
        let mut s2 = src(2, "https://b.example/y", true, PASSAGE);
        s2.published = None;
        let r = check(
            "Four channels and 100 MHz [1, 2].\nA second claim about channels [2]",
            &[src(1, "https://a.example/x", true, PASSAGE), s2],
            &[],
        );
        assert_eq!(r.checks.len(), 3);
        assert_eq!(r.sources_missing_dates, 1);
        assert_eq!(r.sources_retrieved, 2);
        let none = check("No citations at all.", &[], &[]);
        assert_eq!(none.validity_rate(), None);
    }
}
