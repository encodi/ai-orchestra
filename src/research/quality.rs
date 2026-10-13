//! Qualitative evidence assessment: what kind of source this is, whether that kind is a good
//! basis for a given kind of claim, whether it is current, and which sources are really one.
//!
//! Nothing here produces a number that pretends to be a probability. The labels are advisory,
//! heuristic (based on the domain and path only) and say so. Quality depends on the claim: a
//! manufacturer's page is the right source for a *specification* and a poor one for *real-world
//! performance*; a forum thread is good evidence of *user experience* and weak for a scientific
//! finding. No kind is "always right".

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::source::SourceSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    GovernmentOrRegulatory,
    ScholarlyPublication,
    /// A preprint server: not peer reviewed.
    Preprint,
    /// An encyclopedia or reference work (secondary).
    ReferenceWork,
    /// Documentation, datasheets or specification pages (by URL shape); whether it is the
    /// manufacturer's own cannot be told from the URL alone.
    VendorOrProjectDocumentation,
    NewsMedia,
    CommunityDiscussion,
    /// Nothing in the domain or path says what this is.
    Unclassified,
}

impl SourceKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::GovernmentOrRegulatory => "government or regulatory publication",
            Self::ScholarlyPublication => "scholarly publication",
            Self::Preprint => "preprint (not peer reviewed)",
            Self::ReferenceWork => "reference work (secondary)",
            Self::VendorOrProjectDocumentation => "documentation or specification page",
            Self::NewsMedia => "news media",
            Self::CommunityDiscussion => "community discussion or review",
            Self::Unclassified => "unclassified",
        }
    }
}

fn ends_with_any(host: &str, suffixes: &[&str]) -> bool {
    suffixes
        .iter()
        .any(|s| host == *s || host.ends_with(&format!(".{s}")))
}

/// Classifies a source from its domain and path. The note says how sure that is.
pub fn classify_source(domain: &str, path: &str) -> (SourceKind, String) {
    let host = domain.trim_start_matches("www.").to_ascii_lowercase();
    let p = path.to_ascii_lowercase();
    let heuristic = |what: &str| format!("{what} (judged from the domain only)");
    if host.ends_with(".gov")
        || host.contains(".gov.")
        || host.ends_with(".mil")
        || ends_with_any(
            &host,
            &[
                "europa.eu",
                "who.int",
                "un.org",
                "nih.gov",
                "nasa.gov",
                "ec.europa.eu",
                "oecd.org",
            ],
        )
    {
        return (
            SourceKind::GovernmentOrRegulatory,
            heuristic("government or intergovernmental site"),
        );
    }
    if ends_with_any(
        &host,
        &[
            "arxiv.org",
            "biorxiv.org",
            "medrxiv.org",
            "ssrn.com",
            "preprints.org",
            "chemrxiv.org",
        ],
    ) {
        return (
            SourceKind::Preprint,
            "preprint server: the paper has not necessarily been peer reviewed".to_string(),
        );
    }
    if ends_with_any(
        &host,
        &[
            "nature.com",
            "science.org",
            "sciencedirect.com",
            "springer.com",
            "link.springer.com",
            "ieee.org",
            "acm.org",
            "wiley.com",
            "pubmed.ncbi.nlm.nih.gov",
            "ncbi.nlm.nih.gov",
            "doi.org",
            "jstor.org",
            "cell.com",
            "thelancet.com",
            "nejm.org",
            "plos.org",
            "pnas.org",
            "aps.org",
            "iop.org",
        ],
    ) || host.ends_with(".edu")
        || host.contains(".ac.")
    {
        return (
            SourceKind::ScholarlyPublication,
            heuristic(
                "academic or journal site; peer-review status of the specific article is not checked",
            ),
        );
    }
    if ends_with_any(&host, &["wikipedia.org", "britannica.com", "wikidata.org"]) {
        return (
            SourceKind::ReferenceWork,
            "encyclopedia: a secondary source that summarizes others".to_string(),
        );
    }
    if ends_with_any(
        &host,
        &[
            "reddit.com",
            "news.ycombinator.com",
            "stackoverflow.com",
            "stackexchange.com",
            "quora.com",
            "medium.com",
            "substack.com",
        ],
    ) || host.starts_with("forum.")
        || host.starts_with("forums.")
        || p.contains("/forum")
    {
        return (
            SourceKind::CommunityDiscussion,
            heuristic("community or user-generated content"),
        );
    }
    if ends_with_any(
        &host,
        &[
            "bbc.com",
            "bbc.co.uk",
            "reuters.com",
            "apnews.com",
            "nytimes.com",
            "theguardian.com",
            "washingtonpost.com",
            "bloomberg.com",
            "wsj.com",
            "ft.com",
            "elpais.com",
            "lemonde.fr",
            "theverge.com",
            "arstechnica.com",
            "wired.com",
        ],
    ) {
        return (SourceKind::NewsMedia, heuristic("news organization"));
    }
    if host.starts_with("docs.")
        || host.starts_with("developer.")
        || host.starts_with("support.")
        || host.starts_with("help.")
        || p.contains("/docs/")
        || p.contains("/documentation")
        || p.contains("/datasheet")
        || p.contains("/specifications")
        || p.contains("/specs")
        || p.contains("/manual")
    {
        return (
            SourceKind::VendorOrProjectDocumentation,
            heuristic("documentation or specification page; the URL does not show whose it is"),
        );
    }
    (
        SourceKind::Unclassified,
        "nothing in the address says what kind of source this is".to_string(),
    )
}

/// What a claim is about. The same source can be strong evidence for one category and weak for
/// another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimCategory {
    ProductSpecification,
    RealWorldPerformance,
    ScientificFinding,
    EventOrAnnouncement,
    Statistic,
    HistoricalFact,
    UserExperience,
    Definition,
    Other,
}

impl ClaimCategory {
    pub fn parse(text: &str) -> Self {
        match text
            .trim()
            .to_ascii_lowercase()
            .replace([' ', '-'], "_")
            .as_str()
        {
            "product_specification" | "specification" | "spec" | "product_spec" => {
                Self::ProductSpecification
            }
            "real_world_performance" | "performance" | "measurement" => Self::RealWorldPerformance,
            "scientific_finding" | "scientific" | "study" | "science" => Self::ScientificFinding,
            "event_or_announcement" | "event" | "announcement" | "news" => {
                Self::EventOrAnnouncement
            }
            "statistic" | "statistics" | "number" => Self::Statistic,
            "historical_fact" | "historical" | "history" => Self::HistoricalFact,
            "user_experience" | "experience" | "review" => Self::UserExperience,
            "definition" | "concept" => Self::Definition,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Suitability {
    Strong,
    Moderate,
    Weak,
}

/// How suitable a kind of source is for a category of claim, with the reason.
pub fn suitability(kind: SourceKind, category: ClaimCategory) -> (Suitability, &'static str) {
    use ClaimCategory as C;
    use SourceKind as K;
    use Suitability::{Moderate as M, Strong as S, Weak as W};
    match (category, kind) {
        (C::ProductSpecification, K::VendorOrProjectDocumentation) => {
            (S, "specification pages state what the maker advertises")
        }
        (C::ProductSpecification, K::GovernmentOrRegulatory) => {
            (S, "regulatory filings state certified specifications")
        }
        (C::ProductSpecification, K::CommunityDiscussion) => {
            (W, "users repeat specifications second-hand")
        }
        (C::ProductSpecification, K::ScholarlyPublication | K::Preprint) => {
            (W, "papers rarely document a product's specification")
        }
        (C::RealWorldPerformance, K::VendorOrProjectDocumentation) => (
            W,
            "maker documentation is an advertised figure, not an independent measurement",
        ),
        (C::RealWorldPerformance, K::ScholarlyPublication) => {
            (S, "a published measurement or study")
        }
        (C::RealWorldPerformance, K::Preprint) => (M, "a measurement, but not peer reviewed"),
        (C::RealWorldPerformance, K::CommunityDiscussion) => (M, "reports from users: anecdotal"),
        (C::ScientificFinding, K::ScholarlyPublication | K::GovernmentOrRegulatory) => {
            (S, "a primary or institutional scientific source")
        }
        (C::ScientificFinding, K::Preprint) => (
            M,
            "a preprint: the finding has not necessarily passed peer review",
        ),
        (
            C::ScientificFinding,
            K::NewsMedia | K::CommunityDiscussion | K::VendorOrProjectDocumentation,
        ) => (W, "secondary reporting or opinion, not the study itself"),
        (
            C::EventOrAnnouncement,
            K::NewsMedia | K::GovernmentOrRegulatory | K::VendorOrProjectDocumentation,
        ) => (S, "an announcement or a report of it"),
        (C::EventOrAnnouncement, K::CommunityDiscussion) => (W, "second-hand discussion"),
        (C::Statistic, K::GovernmentOrRegulatory | K::ScholarlyPublication) => {
            (S, "official or published statistics")
        }
        (C::Statistic, K::CommunityDiscussion) => (W, "statistics repeated without a method"),
        (C::HistoricalFact, K::ScholarlyPublication | K::GovernmentOrRegulatory) => {
            (S, "scholarly or official record")
        }
        (C::UserExperience, K::CommunityDiscussion) => {
            (S, "first-hand user reports (still anecdotal)")
        }
        (C::UserExperience, _) => (W, "not a record of user experiences"),
        (
            C::Definition,
            K::ReferenceWork | K::ScholarlyPublication | K::VendorOrProjectDocumentation,
        ) => (S, "a reference or documentation source"),
        (_, K::Unclassified) => (M, "the kind of source is not known"),
        (_, K::ReferenceWork) => (M, "a secondary summary of other sources"),
        (_, K::NewsMedia) => (M, "journalism: depends on its own sourcing"),
        _ => (M, "no strong reason either way"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Freshness {
    Current,
    Stale,
    /// The page states no date, so currency is not assumed.
    Unknown,
}

/// Days from 1970-01-01 to a civil date (Howard Hinnant's `days_from_civil`).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Age in days of an ISO-style date relative to `now_ms`, or `None` if it is not parseable.
pub fn age_days(date: &str, now_ms: u64) -> Option<i64> {
    let y: i64 = date.get(0..4)?.parse().ok()?;
    let m: i64 = date.get(5..7)?.parse().ok()?;
    let d: i64 = date.get(8..10)?.parse().ok()?;
    let today = i64::try_from(now_ms / 1000 / 86_400).ok()?;
    Some(today - days_from_civil(y, m, d))
}

/// Judges a source against a freshness requirement (in days). The most recent date the page
/// states (updated, else published) is used; with no date the answer is `Unknown`.
pub fn freshness(
    published: Option<&str>,
    updated: Option<&str>,
    max_age_days: Option<u32>,
    now_ms: u64,
) -> Freshness {
    let newest = [updated, published]
        .into_iter()
        .flatten()
        .filter_map(|d| age_days(d, now_ms))
        .min();
    match (newest, max_age_days) {
        (None, _) => Freshness::Unknown,
        (Some(age), Some(max)) if age > i64::from(max) => Freshness::Stale,
        _ => Freshness::Current,
    }
}

fn shingles(text: &str, k: usize) -> HashSet<u64> {
    let words: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect();
    if words.len() < k {
        return HashSet::new();
    }
    words
        .windows(k)
        .map(|w| {
            // FNV-1a over the words: stable and dependency free.
            let mut h: u64 = 0xcbf29ce484222325;
            for word in w {
                for b in word.bytes().chain(std::iter::once(b' ')) {
                    h ^= u64::from(b);
                    h = h.wrapping_mul(0x100000001b3);
                }
            }
            h
        })
        .collect()
}

/// Groups retrieved sources whose texts are largely the same (copies, syndication, a page that
/// quotes another at length). The first source of a group (lowest id) names it. Several articles
/// repeating one original are *one* independent source, not several.
pub fn compute_upstream_groups(set: &mut SourceSet, similarity_threshold: f64) {
    let ids = set.retrieved_ids();
    let sets: HashMap<u32, HashSet<u64>> = ids
        .iter()
        .filter_map(|&id| set.text(id).map(|t| (id, shingles(t, 6))))
        .collect();
    let canon: HashMap<u32, Option<String>> = ids
        .iter()
        .filter_map(|&id| set.get(id).map(|r| (id, r.canonical_url.clone())))
        .collect();
    let finals: HashMap<u32, String> = ids
        .iter()
        .filter_map(|&id| set.get(id).map(|r| (id, r.display_url().to_string())))
        .collect();
    // Union-find over source ids.
    let mut parent: HashMap<u32, u32> = ids.iter().map(|&i| (i, i)).collect();
    fn find(parent: &mut HashMap<u32, u32>, x: u32) -> u32 {
        let p = parent[&x];
        if p == x {
            return x;
        }
        let root = find(parent, p);
        parent.insert(x, root);
        root
    }
    for (i, &a) in ids.iter().enumerate() {
        for &b in &ids[i + 1..] {
            let same_canonical = matches!((&canon[&a], &canon[&b]), (Some(x), Some(y)) if x == y)
                || canon[&a].as_deref() == Some(finals[&b].as_str())
                || canon[&b].as_deref() == Some(finals[&a].as_str());
            let similar = match (sets.get(&a), sets.get(&b)) {
                (Some(x), Some(y)) if x.len() >= 20 && y.len() >= 20 => {
                    // Containment: how much of the smaller page appears in the other.
                    let inter = x.intersection(y).count() as f64;
                    inter / x.len().min(y.len()) as f64 >= similarity_threshold
                }
                _ => false,
            };
            if same_canonical || similar {
                let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
                if ra != rb {
                    let (lo, hi) = (ra.min(rb), ra.max(rb));
                    parent.insert(hi, lo);
                }
            }
        }
    }
    for &id in &ids {
        let root = find(&mut parent, id);
        set.set_group(id, root);
    }
}

/// How many independent upstream sources the given source ids amount to.
pub fn independent_groups(set: &SourceSet, ids: &[u32]) -> usize {
    ids.iter()
        .filter_map(|&id| set.get(id).and_then(|r| r.upstream_group.or(Some(r.id))))
        .collect::<HashSet<_>>()
        .len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::research::fetch::FetchedPage;
    use crate::research::html::Extracted;
    use crate::research::source::{Discovery, DiscoveryMethod};

    #[test]
    fn sources_are_classified_by_domain_and_path_with_an_honest_note() {
        let cases = [
            ("www.nasa.gov", "/x", SourceKind::GovernmentOrRegulatory),
            ("data.gov.uk", "/", SourceKind::GovernmentOrRegulatory),
            ("arxiv.org", "/abs/1", SourceKind::Preprint),
            (
                "www.nature.com",
                "/articles/x",
                SourceKind::ScholarlyPublication,
            ),
            ("cs.mit.edu", "/", SourceKind::ScholarlyPublication),
            ("en.wikipedia.org", "/wiki/X", SourceKind::ReferenceWork),
            ("www.reddit.com", "/r/x", SourceKind::CommunityDiscussion),
            ("www.reuters.com", "/x", SourceKind::NewsMedia),
            (
                "docs.example.com",
                "/",
                SourceKind::VendorOrProjectDocumentation,
            ),
            (
                "example.com",
                "/products/x/specifications",
                SourceKind::VendorOrProjectDocumentation,
            ),
            ("example.com", "/blog/post", SourceKind::Unclassified),
        ];
        for (domain, path, want) in cases {
            let (kind, note) = classify_source(domain, path);
            assert_eq!(kind, want, "{domain}{path}");
            assert!(!note.is_empty());
        }
        assert!(
            classify_source("arxiv.org", "/")
                .1
                .contains("not necessarily been peer reviewed")
        );
        assert!(
            classify_source("docs.example.com", "/")
                .1
                .contains("does not show whose")
        );
    }

    #[test]
    fn suitability_depends_on_the_claim_not_on_a_universal_ranking() {
        use ClaimCategory as C;
        use SourceKind as K;
        use Suitability::{Strong, Weak};
        // The same vendor page is strong for the advertised spec and weak for measured performance.
        assert_eq!(
            suitability(K::VendorOrProjectDocumentation, C::ProductSpecification).0,
            Strong
        );
        assert_eq!(
            suitability(K::VendorOrProjectDocumentation, C::RealWorldPerformance).0,
            Weak
        );
        // A forum is strong for experiences and weak for scientific findings.
        assert_eq!(
            suitability(K::CommunityDiscussion, C::UserExperience).0,
            Strong
        );
        assert_eq!(
            suitability(K::CommunityDiscussion, C::ScientificFinding).0,
            Weak
        );
        assert_eq!(
            suitability(K::ScholarlyPublication, C::ScientificFinding).0,
            Strong
        );
        assert_eq!(
            suitability(K::Preprint, C::ScientificFinding).0,
            Suitability::Moderate
        );
        // Every pair has a reason.
        for kind in [
            K::GovernmentOrRegulatory,
            K::ScholarlyPublication,
            K::Preprint,
            K::ReferenceWork,
            K::VendorOrProjectDocumentation,
            K::NewsMedia,
            K::CommunityDiscussion,
            K::Unclassified,
        ] {
            for cat in [
                C::ProductSpecification,
                C::RealWorldPerformance,
                C::ScientificFinding,
                C::EventOrAnnouncement,
                C::Statistic,
                C::HistoricalFact,
                C::UserExperience,
                C::Definition,
                C::Other,
            ] {
                assert!(!suitability(kind, cat).1.is_empty());
            }
        }
        assert_eq!(
            ClaimCategory::parse("Product Specification"),
            C::ProductSpecification
        );
        assert_eq!(ClaimCategory::parse("nonsense"), C::Other);
    }

    #[test]
    fn age_and_freshness_use_the_stated_dates_and_never_assume() {
        // 2026-10-11 00:00:00 UTC
        let now: u64 = 1_791_676_800_000;
        assert_eq!(age_days("2026-10-11", now), Some(0));
        assert_eq!(age_days("2026-10-01T10:00:00Z", now), Some(10));
        assert_eq!(age_days("2025-10-11", now), Some(365));
        assert_eq!(age_days("garbage", now), None);
        assert_eq!(
            freshness(Some("2026-10-01"), None, Some(30), now),
            Freshness::Current
        );
        assert_eq!(
            freshness(Some("2026-01-01"), None, Some(30), now),
            Freshness::Stale
        );
        // The update date wins over the publication date.
        assert_eq!(
            freshness(Some("2020-01-01"), Some("2026-10-05"), Some(30), now),
            Freshness::Current
        );
        assert_eq!(freshness(None, None, Some(30), now), Freshness::Unknown);
        assert_eq!(
            freshness(Some("2020-01-01"), None, None, now),
            Freshness::Current,
            "with no requirement an old date is not 'stale'"
        );
    }

    fn add(set: &mut SourceSet, url: &str, text: &str, canonical: Option<&str>) -> u32 {
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
                requested_url: url.into(),
                final_url: url.into(),
                redirects: vec![],
                status: 200,
                content_type: "text/html".into(),
                bytes: text.len(),
                content_hash: "0".repeat(64),
                retrieved_at_ms: 1,
                extracted: Extracted {
                    text: text.to_string(),
                    canonical_url: canonical.map(String::from),
                    ..Default::default()
                },
            }),
        );
        id
    }

    const ORIGINAL: &str = "The Model X oscilloscope offers 100 MHz of analog bandwidth, four channels, a one gigasample per second sampling rate and a seven inch display with a deep memory of twelve million points per channel.";

    #[test]
    fn articles_repeating_one_original_are_one_independent_source() {
        let mut set = SourceSet::new();
        let a = add(&mut set, "https://maker.example/spec", ORIGINAL, None);
        let b = add(
            &mut set,
            "https://news.example/story",
            &format!("Breaking: {ORIGINAL} Analysts say it is a good value."),
            None,
        );
        let c = add(
            &mut set,
            "https://blog.example/post",
            &format!("Copied from the maker: {ORIGINAL}"),
            None,
        );
        let d = add(
            &mut set,
            "https://lab.example/test",
            "We measured the unit on our bench with a calibrated source and found the -3 dB point at 96 MHz, which is close to but below the advertised figure.",
            None,
        );
        compute_upstream_groups(&mut set, 0.6);
        let g = |id| set.get(id).unwrap().upstream_group;
        assert_eq!(
            (g(a), g(b), g(c)),
            (Some(1), Some(1), Some(1)),
            "the copies share the original's group"
        );
        assert_eq!(g(d), Some(d), "an independent measurement is its own group");
        assert_eq!(independent_groups(&set, &[a, b, c]), 1);
        assert_eq!(independent_groups(&set, &[a, b, c, d]), 2);
    }

    #[test]
    fn a_canonical_link_to_another_retrieved_source_joins_their_groups() {
        let mut set = SourceSet::new();
        let a = add(
            &mut set,
            "https://example.com/a",
            "first page with its own distinct words about batteries and charging cycles in detail",
            None,
        );
        let b = add(
            &mut set,
            "https://mirror.example/a",
            "an entirely different sentence about gardens, tomatoes, watering schedules and soil",
            Some("https://example.com/a"),
        );
        compute_upstream_groups(&mut set, 0.6);
        assert_eq!(independent_groups(&set, &[a, b]), 1);
    }

    #[test]
    fn short_or_unrelated_texts_are_not_grouped() {
        let mut set = SourceSet::new();
        let a = add(&mut set, "https://a.example/x", "short", None);
        let b = add(&mut set, "https://b.example/x", "short", None);
        compute_upstream_groups(&mut set, 0.6);
        assert_eq!(
            independent_groups(&set, &[a, b]),
            2,
            "too little text to call them copies"
        );
    }
}
