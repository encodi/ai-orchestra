//! Source records: where a piece of evidence came from, how it was found, and what happened when
//! it was retrieved. A [`SourceSet`] owns the numbering (`[1]`, `[2]`, ...) that citations use.
//!
//! The record never contains a field that was not observed: a missing author, date or title stays
//! `None`. The *published*, *updated* and *retrieved* dates are separate fields and are never
//! substituted for one another. Only a short preview of the text is stored with the record (the
//! full extracted text lives in memory for the run), so saved records do not hold whole pages.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use url::Url;

use super::fetch::FetchedPage;
use super::net::FetchError;
use super::quality::{SourceKind, classify_source};

/// How a URL came to be considered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryMethod {
    /// The user passed it (`--url`) or it appears in the question.
    UserProvided,
    /// It appeared in the output of a provider's own web tool (for example a search result).
    /// The provider reported it; it counts as evidence only after this program retrieves it.
    ProviderToolReported,
    /// A provider wrote it in its answer. Weakest: a model can invent URLs.
    ProviderTextReported,
}

impl DiscoveryMethod {
    pub fn label(self) -> &'static str {
        match self {
            Self::UserProvided => "provided by the user",
            Self::ProviderToolReported => "reported by a provider's web tool",
            Self::ProviderTextReported => "written by a provider in its answer",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Discovery {
    pub method: DiscoveryMethod,
    /// The provider that reported it, for the provider-reported methods.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceStatus {
    /// This program fetched the page and extracted its text.
    Retrieved,
    /// The page could not be reached or answered with an error (not found, timeout, ...).
    Inaccessible,
    /// A safety rule refused it (private address, robots.txt, bad scheme, ...).
    Blocked,
    /// It answered, but in a format that is not read (PDF, JSON, ...).
    Unsupported,
    /// Not attempted (for example the source limit was reached).
    NotAttempted,
    /// Retrieved, but not among the most relevant sources; its text is dropped and it is not
    /// used as evidence.
    NotSelected,
}

/// A short passage kept as the visible basis of a judgement. Always cut from the extracted text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Excerpt {
    pub text: String,
    /// Character offset in the extracted text.
    pub offset: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRecord {
    /// The citation number: `[id]`.
    pub id: u32,
    pub original_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub domain: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// Publication date as the page states it. `None` if it does not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published: Option<String>,
    /// Last-updated date as the page states it. `None` if it does not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>,
    /// When this program retrieved the page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieved_at_ms: Option<u64>,
    pub discovery: Vec<Discovery>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// SHA-256 of the retrieved bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    pub status: SourceStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Characters of extracted text (the text itself is not stored).
    pub text_chars: usize,
    pub text_preview: String,
    pub kind: SourceKind,
    pub kind_note: String,
    /// Sources whose texts are near-identical (syndication, copies, quotes of each other) share
    /// a group number: they are one independent source, not several.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_group: Option<u32>,
    /// Short passages used as evidence for claims.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excerpts: Vec<Excerpt>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitations: Vec<String>,
}

impl SourceRecord {
    pub fn is_retrieved(&self) -> bool {
        self.status == SourceStatus::Retrieved
    }

    /// The best URL to show: the canonical one when the page states it, else where it ended up.
    pub fn display_url(&self) -> &str {
        self.final_url.as_deref().unwrap_or(&self.original_url)
    }
}

const PREVIEW_CHARS: usize = 500;

fn preview(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    flat.chars().take(PREVIEW_CHARS).collect()
}

/// The URL with everything that does not change the document removed, for detecting duplicates.
pub fn normalize_url_key(url: &Url) -> String {
    let mut u = url.clone();
    u.set_fragment(None);
    let kept: Vec<(String, String)> = u
        .query_pairs()
        .filter(|(k, _)| {
            let k = k.to_ascii_lowercase();
            !(k.starts_with("utm_")
                || matches!(
                    k.as_str(),
                    "fbclid" | "gclid" | "mc_cid" | "mc_eid" | "ref" | "ref_src" | "igshid"
                ))
        })
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    if kept.is_empty() {
        u.set_query(None);
    } else {
        u.query_pairs_mut().clear().extend_pairs(kept);
    }
    // `/a/` and `/a` name the same page for any site that serves one of them as the other.
    if u.path().len() > 1 && u.path().ends_with('/') {
        let trimmed = u.path().trim_end_matches('/').to_string();
        u.set_path(&trimmed);
    }
    let mut s = u.to_string();
    if u.path() == "/" && u.query().is_none() {
        s = s.trim_end_matches('/').to_string();
    }
    s.to_ascii_lowercase()
}

/// The numbered collection of sources for one research run.
#[derive(Debug, Default)]
pub struct SourceSet {
    records: Vec<SourceRecord>,
    keys: HashMap<String, u32>,
    texts: HashMap<u32, String>,
    /// Times a candidate was offered that was already recorded (the same page under another
    /// spelling, or reported by several agents): each was merged instead of fetched again.
    merged: usize,
}

impl SourceSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Candidates that were merged into one already recorded instead of being fetched again.
    pub fn duplicates_merged(&self) -> usize {
        self.merged
    }

    pub fn records(&self) -> &[SourceRecord] {
        &self.records
    }

    pub fn into_records(self) -> Vec<SourceRecord> {
        self.records
    }

    pub fn get(&self, id: u32) -> Option<&SourceRecord> {
        self.records.iter().find(|r| r.id == id)
    }

    pub fn get_mut(&mut self, id: u32) -> Option<&mut SourceRecord> {
        self.records.iter_mut().find(|r| r.id == id)
    }

    pub fn text(&self, id: u32) -> Option<&str> {
        self.texts.get(&id).map(String::as_str)
    }

    /// Ids of the sources that were actually retrieved.
    pub fn retrieved_ids(&self) -> Vec<u32> {
        self.records
            .iter()
            .filter(|r| r.is_retrieved())
            .map(|r| r.id)
            .collect()
    }

    /// Registers a URL (or adds another way it was found to a URL already registered) and
    /// returns its citation id. Returns `None` if the text is not a URL at all.
    pub fn add_candidate(&mut self, raw_url: &str, discovery: Discovery) -> Option<u32> {
        let parsed = Url::parse(raw_url.trim()).ok()?;
        let key = normalize_url_key(&parsed);
        if let Some(&id) = self.keys.get(&key) {
            self.merged += 1;
            if let Some(rec) = self.get_mut(id)
                && !rec.discovery.contains(&discovery)
            {
                rec.discovery.push(discovery);
            }
            return Some(id);
        }
        let id = u32::try_from(self.records.len() + 1).ok()?;
        let domain = parsed.host_str().unwrap_or("").to_ascii_lowercase();
        let (kind, kind_note) = classify_source(&domain, parsed.path());
        self.records.push(SourceRecord {
            id,
            original_url: parsed.to_string(),
            final_url: None,
            canonical_url: None,
            title: None,
            domain,
            author: None,
            published: None,
            updated: None,
            retrieved_at_ms: None,
            discovery: vec![discovery],
            http_status: None,
            content_type: None,
            content_hash: None,
            status: SourceStatus::NotAttempted,
            error_kind: None,
            error: None,
            text_chars: 0,
            text_preview: String::new(),
            kind,
            kind_note,
            upstream_group: None,
            excerpts: Vec::new(),
            limitations: Vec::new(),
        });
        self.keys.insert(key, id);
        Some(id)
    }

    /// Records the outcome of fetching source `id`.
    pub fn apply_fetch(&mut self, id: u32, result: Result<FetchedPage, FetchError>) {
        match result {
            Ok(page) => {
                let text = page.extracted.text.clone();
                let final_domain = Url::parse(&page.final_url)
                    .ok()
                    .and_then(|u| u.host_str().map(str::to_ascii_lowercase));
                let reclass = final_domain.map(|d| {
                    (
                        d,
                        Url::parse(&page.final_url)
                            .map(|u| u.path().to_string())
                            .unwrap_or_default(),
                    )
                });
                if let Some(rec) = self.get_mut(id) {
                    rec.final_url =
                        (page.final_url != rec.original_url).then(|| page.final_url.clone());
                    rec.canonical_url = page.extracted.canonical_url.clone();
                    rec.title = page.extracted.title.clone();
                    rec.author = page.extracted.author.clone();
                    rec.published = page.extracted.published.clone();
                    rec.updated = page.extracted.updated.clone();
                    rec.retrieved_at_ms = Some(page.retrieved_at_ms);
                    rec.http_status = Some(page.status);
                    rec.content_type = Some(page.content_type.clone());
                    rec.content_hash = Some(page.content_hash.clone());
                    rec.status = SourceStatus::Retrieved;
                    rec.text_chars = text.chars().count();
                    rec.text_preview = preview(&text);
                    if let Some((domain, path)) = reclass {
                        // A redirect can lead to another site: describe the page that answered.
                        if domain != rec.domain {
                            rec.domain = domain.clone();
                            let (kind, note) = classify_source(&domain, &path);
                            rec.kind = kind;
                            rec.kind_note = note;
                        }
                    }
                    if page.extracted.text_truncated {
                        rec.limitations
                            .push("only the first part of a long page was read".to_string());
                    }
                    if text.chars().count() < 200 {
                        rec.limitations.push("very little text could be extracted (the page may rely on scripts, or be a login/consent page)".to_string());
                    }
                    if rec.published.is_none() && rec.updated.is_none() {
                        rec.limitations
                            .push("the page states no publication or update date".to_string());
                    }
                }
                self.texts.insert(id, text);
            }
            Err(e) => {
                if let Some(rec) = self.get_mut(id) {
                    rec.status = match &e {
                        FetchError::UnsupportedContentType(_) => SourceStatus::Unsupported,
                        FetchError::InvalidUrl(_)
                        | FetchError::SchemeNotAllowed(..)
                        | FetchError::Credentials
                        | FetchError::PortNotAllowed(_)
                        | FetchError::BlockedAddress { .. }
                        | FetchError::RedirectRefused(..)
                        | FetchError::RobotsDisallowed
                        | FetchError::RobotsUnavailable(_) => SourceStatus::Blocked,
                        _ => SourceStatus::Inaccessible,
                    };
                    rec.error_kind = Some(e.kind().to_string());
                    rec.error = Some(e.to_string());
                    if let FetchError::HttpStatus(code) = e {
                        rec.http_status = Some(code);
                    }
                }
            }
        }
    }

    /// Folds in sources that turned out to be the same document (same final or canonical URL),
    /// keeping the first. Ids stay as they were; the later record is marked as a duplicate.
    pub fn mark_duplicates(&mut self) {
        let mut seen: HashMap<String, u32> = HashMap::new();
        let mut dupes: Vec<(u32, u32)> = Vec::new();
        for rec in &self.records {
            if !rec.is_retrieved() {
                continue;
            }
            for u in [
                rec.canonical_url.as_deref(),
                Some(rec.final_url.as_deref().unwrap_or(&rec.original_url)),
            ]
            .into_iter()
            .flatten()
            {
                let Ok(parsed) = Url::parse(u) else { continue };
                let key = normalize_url_key(&parsed);
                match seen.get(&key) {
                    Some(&first) if first != rec.id => {
                        dupes.push((rec.id, first));
                        break;
                    }
                    _ => {
                        seen.insert(key, rec.id);
                    }
                }
            }
        }
        for (dup, first) in dupes {
            if let Some(rec) = self.get_mut(dup) {
                rec.status = SourceStatus::NotAttempted;
                rec.limitations.push(format!(
                    "same document as [{first}]; not counted separately"
                ));
            }
            self.texts.remove(&dup);
        }
    }

    /// Marks a retrieved source as not used as evidence (over the source limit).
    pub fn demote(&mut self, id: u32, why: &str) {
        self.texts.remove(&id);
        if let Some(rec) = self.get_mut(id) {
            rec.status = SourceStatus::NotSelected;
            rec.excerpts.clear();
            rec.limitations.push(why.to_string());
        }
    }

    pub fn set_group(&mut self, id: u32, group: u32) {
        if let Some(rec) = self.get_mut(id) {
            rec.upstream_group = Some(group);
        }
    }

    pub fn add_excerpt(&mut self, id: u32, excerpt: Excerpt) {
        if let Some(rec) = self.get_mut(id)
            && !rec.excerpts.contains(&excerpt)
            && rec.excerpts.len() < 12
        {
            rec.excerpts.push(excerpt);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::research::html::Extracted;

    fn disc(m: DiscoveryMethod) -> Discovery {
        Discovery {
            method: m,
            provider: None,
        }
    }

    fn page(url: &str, text: &str) -> FetchedPage {
        FetchedPage {
            requested_url: url.to_string(),
            final_url: url.to_string(),
            redirects: vec![],
            status: 200,
            content_type: "text/html".to_string(),
            bytes: text.len(),
            content_hash: "ab".repeat(32),
            retrieved_at_ms: 1_800_000_000_000,
            extracted: Extracted {
                title: Some("T".into()),
                text: text.to_string(),
                published: Some("2025-01-02".into()),
                ..Default::default()
            },
        }
    }

    #[test]
    fn the_same_page_under_other_spellings_is_one_source_and_the_merges_are_counted() {
        let mut set = SourceSet::new();
        let spellings = [
            "https://blog.example.org/2026/10/01/post/",
            "https://blog.example.org/2026/10/01/post",
            "https://BLOG.example.org/2026/10/01/post#section",
            "https://blog.example.org/2026/10/01/post/?utm_campaign=a&fbclid=b",
        ];
        let ids: Vec<u32> = spellings
            .iter()
            .map(|u| {
                set.add_candidate(u, disc(DiscoveryMethod::ProviderToolReported))
                    .unwrap()
            })
            .collect();
        assert!(ids.iter().all(|i| *i == ids[0]), "{ids:?}");
        assert_eq!(set.records().len(), 1);
        assert_eq!(set.duplicates_merged(), 3);
        // A genuinely different path stays a different source.
        let other = set
            .add_candidate(
                "https://blog.example.org/2026/10/02/post/",
                disc(DiscoveryMethod::UserProvided),
            )
            .unwrap();
        assert_ne!(other, ids[0]);
        assert_eq!(set.duplicates_merged(), 3);
    }

    #[test]
    fn ids_are_stable_numbers_and_duplicate_urls_share_one_id() {
        let mut set = SourceSet::new();
        let a = set
            .add_candidate(
                "https://Example.com/spec?utm_source=x#top",
                disc(DiscoveryMethod::ProviderTextReported),
            )
            .unwrap();
        let b = set
            .add_candidate(
                "https://example.com/spec",
                disc(DiscoveryMethod::UserProvided),
            )
            .unwrap();
        let c = set
            .add_candidate(
                "https://example.com/other",
                disc(DiscoveryMethod::UserProvided),
            )
            .unwrap();
        assert_eq!((a, b, c), (1, 1, 2));
        assert_eq!(set.records().len(), 2);
        let methods: Vec<_> = set
            .get(1)
            .unwrap()
            .discovery
            .iter()
            .map(|d| d.method)
            .collect();
        assert_eq!(
            methods,
            [
                DiscoveryMethod::ProviderTextReported,
                DiscoveryMethod::UserProvided
            ],
            "both ways it was found are kept"
        );
        assert_eq!(
            set.add_candidate("not a url", disc(DiscoveryMethod::UserProvided)),
            None
        );
        // A different query is a different document.
        assert_eq!(
            set.add_candidate(
                "https://example.com/spec?id=7",
                disc(DiscoveryMethod::UserProvided)
            ),
            Some(3)
        );
        assert_eq!(
            set.add_candidate("https://example.com", disc(DiscoveryMethod::UserProvided)),
            set.add_candidate("https://example.com/", disc(DiscoveryMethod::UserProvided))
        );
    }

    #[test]
    fn a_retrieved_page_fills_the_record_without_inventing_anything() {
        let mut set = SourceSet::new();
        let id = set
            .add_candidate("https://example.com/p", disc(DiscoveryMethod::UserProvided))
            .unwrap();
        assert_eq!(set.get(id).unwrap().status, SourceStatus::NotAttempted);
        set.apply_fetch(
            id,
            Ok(page(
                "https://example.com/p",
                &"The bandwidth is 100 MHz. ".repeat(20),
            )),
        );
        let r = set.get(id).unwrap();
        assert!(r.is_retrieved() && r.http_status == Some(200));
        assert_eq!(r.published.as_deref(), Some("2025-01-02"));
        assert_eq!(
            r.updated, None,
            "the updated date was not stated, so it is not filled in"
        );
        assert_eq!(r.author, None);
        assert!(r.text_preview.chars().count() <= 500 && r.text_chars > 400);
        assert!(set.text(id).unwrap().contains("100 MHz"));
        assert_eq!(r.content_hash.as_deref().map(str::len), Some(64));
        assert!(r.limitations.is_empty(), "{:?}", r.limitations);

        let id2 = set
            .add_candidate(
                "https://example.org/undated",
                disc(DiscoveryMethod::UserProvided),
            )
            .unwrap();
        let mut p = page("https://example.org/undated", "short");
        p.extracted.published = None;
        set.apply_fetch(id2, Ok(p));
        let lim = &set.get(id2).unwrap().limitations;
        assert!(
            lim.iter()
                .any(|l| l.contains("no publication or update date"))
                && lim.iter().any(|l| l.contains("very little text")),
            "{lim:?}"
        );
    }

    #[test]
    fn failures_are_classified_and_never_count_as_retrieved() {
        let mut set = SourceSet::new();
        let cases = [
            (FetchError::HttpStatus(404), SourceStatus::Inaccessible),
            (FetchError::Timeout(5), SourceStatus::Inaccessible),
            (
                FetchError::Dns {
                    host: "x".into(),
                    detail: "nxdomain".into(),
                },
                SourceStatus::Inaccessible,
            ),
            (
                FetchError::UnsupportedContentType("application/pdf".into()),
                SourceStatus::Unsupported,
            ),
            (FetchError::RobotsDisallowed, SourceStatus::Blocked),
            (
                FetchError::BlockedAddress {
                    host: "h".into(),
                    reason: "r".into(),
                },
                SourceStatus::Blocked,
            ),
            (
                FetchError::SchemeNotAllowed("ftp".into(), ""),
                SourceStatus::Blocked,
            ),
        ];
        for (i, (err, want)) in cases.into_iter().enumerate() {
            let id = set
                .add_candidate(
                    &format!("https://example.com/{i}"),
                    disc(DiscoveryMethod::UserProvided),
                )
                .unwrap();
            let kind = err.kind();
            set.apply_fetch(id, Err(err));
            let r = set.get(id).unwrap();
            assert_eq!(r.status, want);
            assert!(!r.is_retrieved() && set.text(id).is_none());
            assert_eq!(r.error_kind.as_deref(), Some(kind));
        }
        assert_eq!(set.get(1).unwrap().http_status, Some(404));
        assert!(set.retrieved_ids().is_empty());
    }

    #[test]
    fn two_urls_for_one_document_are_folded_by_final_and_canonical_url() {
        let mut set = SourceSet::new();
        let a = set
            .add_candidate("https://example.com/a", disc(DiscoveryMethod::UserProvided))
            .unwrap();
        let b = set
            .add_candidate("https://example.com/b", disc(DiscoveryMethod::UserProvided))
            .unwrap();
        set.apply_fetch(a, Ok(page("https://example.com/a", &"alpha ".repeat(60))));
        let mut pb = page("https://example.com/b", &"alpha ".repeat(60));
        pb.extracted.canonical_url = Some("https://example.com/a".into());
        set.apply_fetch(b, Ok(pb));
        set.mark_duplicates();
        assert!(set.get(a).unwrap().is_retrieved());
        assert!(!set.get(b).unwrap().is_retrieved());
        assert!(set.get(b).unwrap().limitations[0].contains("same document as [1]"));
        assert_eq!(set.retrieved_ids(), vec![1]);
    }

    #[test]
    fn a_redirect_to_another_site_is_described_by_the_site_that_answered() {
        let mut set = SourceSet::new();
        let id = set
            .add_candidate(
                "https://short.example/x",
                disc(DiscoveryMethod::UserProvided),
            )
            .unwrap();
        let mut p = page("https://www.nasa.gov/news/item", &"text ".repeat(100));
        p.requested_url = "https://short.example/x".into();
        set.apply_fetch(id, Ok(p));
        let r = set.get(id).unwrap();
        assert_eq!(r.domain, "www.nasa.gov");
        assert_eq!(r.kind, SourceKind::GovernmentOrRegulatory);
        assert_eq!(r.original_url, "https://short.example/x");
        assert_eq!(r.display_url(), "https://www.nasa.gov/news/item");
    }

    #[test]
    fn records_round_trip_through_json_and_omit_empty_fields() {
        let mut set = SourceSet::new();
        let id = set
            .add_candidate("https://example.com/p", disc(DiscoveryMethod::UserProvided))
            .unwrap();
        set.apply_fetch(id, Ok(page("https://example.com/p", &"word ".repeat(100))));
        let rec = set.get(id).unwrap().clone();
        let json = serde_json::to_value(&rec).unwrap();
        assert!(
            json.get("author").is_none() && json.get("updated").is_none(),
            "unknown fields are absent, not null or invented"
        );
        let back: SourceRecord = serde_json::from_value(json).unwrap();
        assert_eq!(back, rec);
    }
}
