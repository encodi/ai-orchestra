//! Finding the memories that bear on a question.
//!
//! The first backend is lexical and deterministic: SQLite FTS5 with BM25 ranking over the words of
//! the question, combined with metadata filters (the visible scopes, the project, the session),
//! the *recent turns of the current session* (a follow-up such as "of the three above" shares no
//! words with the answer it refers to, so continuity cannot depend on matching words), the evidence
//! of the consultations it selected, and a few explainable signals: recency, scope, who said it and
//! how well it was checked, exact entity matches. There is no embedding model and no external
//! service. The [`RetrievalBackend`] trait keeps the door open for a semantic backend later.
//!
//! The ranking never assumes that newer is better: recency is a small, decaying term.

use std::collections::HashSet;

use super::db::{MemoryDb, Visibility};
use super::freshness::{Freshness, FreshnessPolicy, assess};
use super::model::{EpistemicStatus, MemoryKind, MemoryMode, MemoryRecord, MemoryScope};
use crate::error::Result;

const STOPWORDS: &[&str] = &[
    // English
    "the",
    "a",
    "an",
    "and",
    "or",
    "of",
    "to",
    "in",
    "on",
    "for",
    "with",
    "is",
    "are",
    "was",
    "were",
    "be",
    "been",
    "it",
    "its",
    "that",
    "this",
    "these",
    "those",
    "as",
    "at",
    "by",
    "from",
    "has",
    "have",
    "had",
    "than",
    "then",
    "which",
    "who",
    "will",
    "can",
    "could",
    "would",
    "should",
    "do",
    "does",
    "did",
    "not",
    "no",
    "if",
    "i",
    "me",
    "my",
    "we",
    "you",
    "your",
    "what",
    "when",
    "where",
    "how",
    "why",
    "so",
    "but",
    "about",
    "into",
    "over",
    "also",
    "just",
    "more",
    "most",
    "any",
    "question",
    "questions",
    "answer",
    "answers",
    "conclusion",
    "conclusions",
    "one",
    "ones",
    "us",
    "our",
    "some",
    "there",
    "their",
    "them",
    "they",
    "he",
    "she",
    "his",
    "her",
    // Spanish
    "el",
    "la",
    "los",
    "las",
    "de",
    "del",
    "y",
    "o",
    "en",
    "un",
    "una",
    "unos",
    "unas",
    "es",
    "son",
    "que",
    "con",
    "por",
    "para",
    "al",
    "se",
    "su",
    "sus",
    "lo",
    "como",
    "pero",
    "mi",
    "tu",
    "si",
    "qué",
    "cuál",
    "cuáles",
    "cómo",
    "cuándo",
    "dónde",
    "más",
    "muy",
    "ya",
    "me",
    "te",
    "nos",
];

/// Words that say a question continues an earlier one.
const CONTINUATION_CUES: &[&str] = &[
    "above",
    "previous",
    "previously",
    "earlier",
    "before",
    "those",
    "these",
    "them",
    "same",
    "again",
    "now",
    "still",
    "instead",
    "anterior",
    "anteriores",
    "previo",
    "previa",
    "previos",
    "esos",
    "esas",
    "estos",
    "estas",
    "mismo",
    "misma",
    "ahora",
    "todavía",
    "sigue",
    "entonces",
    "recomendaste",
    "recomendó",
    "dijiste",
    "dijo",
];

pub fn tokenize_terms(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    text.split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|w| {
            !w.is_empty() && (w.chars().count() > 1 || w.chars().all(|c| c.is_ascii_digit()))
        })
        .filter(|w| !STOPWORDS.contains(&w.as_str()))
        .filter(|w| seen.insert(w.clone()))
        .collect()
}

/// An FTS5 expression of quoted terms only, so nothing the user typed can be read as syntax.
pub fn match_expression(terms: &[String], max_terms: usize) -> String {
    terms
        .iter()
        .take(max_terms)
        .map(|t| format!("\"{}\"", t.replace('"', "")))
        .collect::<Vec<_>>()
        .join(" OR ")
}

pub fn continues_earlier_question(question: &str) -> bool {
    let lower = question.to_lowercase();
    lower
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| CONTINUATION_CUES.contains(&w))
}

/// Capitalised words and tokens with digits: names, models and numbers worth matching exactly.
fn entities(question: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (i, raw) in question.split_whitespace().enumerate() {
        let w = raw.trim_matches(|c: char| !c.is_alphanumeric());
        if w.chars().count() < 2 {
            continue;
        }
        let has_digit = w.chars().any(|c| c.is_ascii_digit());
        let capitalised = w.chars().next().is_some_and(char::is_uppercase) && i > 0;
        let inner_upper = w.chars().skip(1).any(char::is_uppercase);
        if (has_digit || capitalised || inner_upper)
            && !out.iter().any(|e| e.eq_ignore_ascii_case(w))
        {
            out.push(w.to_lowercase());
        }
    }
    out
}

#[derive(Debug, Clone)]
pub struct RetrievalRequest {
    pub question: String,
    pub mode: MemoryMode,
    pub global_in_auto: bool,
    pub project: String,
    /// The session whose records may be read. `None`: no session records.
    pub session: Option<String>,
    pub kinds: Option<Vec<MemoryKind>>,
    /// Most lexical candidates to fetch.
    pub limit: usize,
    /// How many recent consultations of the session are always candidates.
    pub recent_turns: usize,
    pub wants_fresh: bool,
    pub now_ms: u64,
    pub freshness: FreshnessPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Via {
    /// Matched the words of the question.
    Lexical,
    /// One of the latest consultations of the session, for a question that reads as a continuation
    /// of earlier work: continuity, not a word match.
    Recent,
    /// The very latest consultation, included in case the question is a follow-up phrased without
    /// any pointer. It stays in its shortest form: it costs one line if it turns out unrelated.
    Latest,
    /// Evidence of a consultation that was selected.
    Linked,
    /// A statement the user made about the same subject as a selected conclusion (a budget or a
    /// requirement that the follow-up question does not repeat).
    Related,
}

#[derive(Debug, Clone)]
pub struct Candidate {
    pub record: MemoryRecord,
    /// BM25 relevance scaled to 0..=1 within this query (0 when it did not match the words).
    pub relevance: f64,
    pub score: f64,
    pub via: Via,
    pub reasons: Vec<String>,
    pub freshness: Freshness,
}

#[derive(Debug, Clone, Default)]
pub struct RetrievalResult {
    pub candidates: Vec<Candidate>,
    pub terms: Vec<String>,
    pub match_expr: String,
    pub lexical_hits: usize,
    pub elapsed_us: u64,
    pub notes: Vec<String>,
}

pub trait RetrievalBackend: Send + Sync {
    fn retrieve(&self, db: &MemoryDb, req: &RetrievalRequest) -> Result<RetrievalResult>;
}

#[derive(Debug, Clone, Copy)]
pub struct LexicalBackend;

/// The weights of the ranking, in one place. They are documented, fixed and testable; nothing is
/// learned.
mod weight {
    pub const RELEVANCE: f64 = 0.55;
    pub const RECENCY: f64 = 0.12;
    pub const SESSION: f64 = 0.08;
    pub const PROJECT: f64 = 0.04;
    pub const USER_STATED: f64 = 0.10;
    pub const VERIFIED: f64 = 0.06;
    pub const CONTRADICTED: f64 = 0.03;
    pub const AGENT: f64 = -0.02;
    pub const ENTITY: f64 = 0.04;
    pub const ENTITY_CAP: f64 = 0.12;
    /// A recent turn without a word match starts here and loses `RECENT_STEP` per turn back.
    pub const RECENT_BASE: f64 = 0.40;
    pub const RECENT_STEP: f64 = 0.06;
    pub const LINKED: f64 = 0.8;
    /// A related user statement starts here: above a stale lexical match, below a direct one.
    pub const RELATED_BASE: f64 = 0.38;
    pub const HALF_LIFE_DAYS: f64 = 30.0;
}

fn recency(record: &MemoryRecord, now_ms: u64) -> f64 {
    let age_days = now_ms.saturating_sub(record.created_ms) as f64 / 86_400_000.0;
    (-age_days * std::f64::consts::LN_2 / weight::HALF_LIFE_DAYS).exp()
}

fn quality(record: &MemoryRecord) -> f64 {
    match record.status {
        EpistemicStatus::UserProvided => weight::USER_STATED,
        EpistemicStatus::PreviouslyVerified | EpistemicStatus::Historical => weight::VERIFIED,
        EpistemicStatus::Contradicted => weight::CONTRADICTED,
        EpistemicStatus::Unverified => 0.0,
        EpistemicStatus::AgentInterpretation => weight::AGENT,
    }
}

fn scope_bonus(scope: MemoryScope) -> f64 {
    match scope {
        MemoryScope::Session => weight::SESSION,
        MemoryScope::Project => weight::PROJECT,
        MemoryScope::Global => 0.0,
    }
}

impl RetrievalBackend for LexicalBackend {
    fn retrieve(&self, db: &MemoryDb, req: &RetrievalRequest) -> Result<RetrievalResult> {
        let started = std::time::Instant::now();
        let mut out = RetrievalResult::default();
        if req.mode == MemoryMode::Off {
            out.notes
                .push("memory is off: nothing was read".to_string());
            return Ok(out);
        }
        let vis = Visibility {
            project: Some(req.project.clone()),
            session: req.session.clone(),
            session_scope: req.session.is_some()
                && req.mode.reads(MemoryScope::Session, req.global_in_auto),
            project_scope: req.mode.reads(MemoryScope::Project, req.global_in_auto),
            global_scope: req.mode.reads(MemoryScope::Global, req.global_in_auto),
            ..Default::default()
        };
        out.terms = tokenize_terms(&req.question);
        out.match_expr = match_expression(&out.terms, 12);
        let ents = entities(&req.question);
        let mut by_id: std::collections::HashMap<String, Candidate> =
            std::collections::HashMap::new();

        // 1. Lexical candidates.
        let hits = db.search(&out.match_expr, &vis, req.kinds.as_deref(), req.limit)?;
        out.lexical_hits = hits.len();
        let max = hits.iter().map(|(_, s)| *s).fold(0.0_f64, f64::max);
        for (record, raw) in hits {
            let relevance = if max > 0.0 {
                (raw / max).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let matched: Vec<&String> = out
                .terms
                .iter()
                .filter(|t| record.text.to_lowercase().contains(t.as_str()))
                .collect();
            let lower = record.text.to_lowercase();
            let entity_hits = ents.iter().filter(|e| lower.contains(e.as_str())).count();
            let entity = (entity_hits as f64 * weight::ENTITY).min(weight::ENTITY_CAP);
            let rec = recency(&record, req.now_ms);
            let score = weight::RELEVANCE * relevance
                + weight::RECENCY * rec
                + scope_bonus(record.scope)
                + quality(&record)
                + entity;
            let mut reasons = vec![format!(
                "matched {}",
                if matched.is_empty() {
                    "the question's words".to_string()
                } else {
                    matched
                        .iter()
                        .take(5)
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            )];
            if entity_hits > 0 {
                reasons.push(format!("{entity_hits} exact name or number match(es)"));
            }
            reasons.push(format!("{} memory", record.scope));
            let freshness = assess(&record, req.now_ms, req.wants_fresh, &req.freshness);
            by_id.insert(
                record.id.clone(),
                Candidate {
                    record,
                    relevance,
                    score,
                    via: Via::Lexical,
                    reasons,
                    freshness,
                },
            );
        }

        // 2. The latest consultations of the session, whatever words they share.
        let mut selected_consultations: Vec<String> = Vec::new();
        if let (Some(session), true) = (&req.session, vis.session_scope) {
            let continuation = continues_earlier_question(&req.question);
            // A question that names its own subject and matches an earlier conclusion strongly does
            // not also need the latest turns for continuity (they may be about something else);
            // one that reads as a continuation gets the most; any other gets only the latest.
            let strong_match = by_id.values().any(|c| {
                matches!(
                    c.record.kind,
                    MemoryKind::Conclusion | MemoryKind::Unresolved
                ) && c.relevance >= 0.5
            });
            let turns = if continuation {
                req.recent_turns + 1
            } else if strong_match {
                0
            } else {
                // No shared words and no cue: only the very latest turn, in case it is a follow-up
                // phrased without any pointer. It costs one short line when it is not.
                1
            };
            let recent = db.recent_in_session(
                session,
                &req.project,
                &[MemoryKind::Conclusion, MemoryKind::Unresolved],
                turns * 3,
            )?;
            let mut seen_consultations: Vec<String> = Vec::new();
            for record in recent {
                let cid = record
                    .consultation_id
                    .clone()
                    .unwrap_or_else(|| record.id.clone());
                if !seen_consultations.contains(&cid) {
                    if seen_consultations.len() >= turns {
                        continue;
                    }
                    seen_consultations.push(cid.clone());
                }
                let back = seen_consultations
                    .iter()
                    .position(|c| *c == cid)
                    .unwrap_or(0);
                let base = weight::RECENT_BASE - weight::RECENT_STEP * back as f64
                    + if continuation { 0.10 } else { 0.0 };
                let freshness = assess(&record, req.now_ms, req.wants_fresh, &req.freshness);
                let reason = format!(
                    "{} of this session{}",
                    if back == 0 {
                        "the latest consultation".to_string()
                    } else {
                        format!("consultation {} back", back + 1)
                    },
                    if continuation {
                        " (the question continues an earlier one)"
                    } else {
                        ""
                    }
                );
                match by_id.get_mut(&record.id) {
                    Some(existing) => {
                        existing.score = existing.score.max(base + quality(&record));
                        existing.reasons.push(reason);
                    }
                    None => {
                        let score = base + quality(&record) + weight::SESSION;
                        by_id.insert(
                            record.id.clone(),
                            Candidate {
                                record,
                                relevance: 0.0,
                                score,
                                via: if continuation {
                                    Via::Recent
                                } else {
                                    Via::Latest
                                },
                                reasons: vec![reason],
                                freshness,
                            },
                        );
                    }
                }
            }
            selected_consultations = seen_consultations;
        }

        // 3. Evidence of the consultations whose conclusions were selected.
        let mut parents: Vec<(String, f64)> = by_id
            .values()
            .filter(|c| c.record.kind == MemoryKind::Conclusion)
            .filter_map(|c| c.record.consultation_id.clone().map(|id| (id, c.score)))
            .collect();
        for cid in selected_consultations {
            if !parents.iter().any(|(p, _)| *p == cid) {
                parents.push((cid, weight::RECENT_BASE));
            }
        }
        parents.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        for (cid, parent_score) in parents.into_iter().take(3) {
            let linked =
                db.memories_of_consultation(&cid, &[MemoryKind::Evidence, MemoryKind::Unresolved])?;
            for record in linked.into_iter().take(8) {
                if !vis_allows(&vis, &record) || by_id.contains_key(&record.id) {
                    continue;
                }
                let freshness = assess(&record, req.now_ms, req.wants_fresh, &req.freshness);
                let score = parent_score * weight::LINKED + quality(&record);
                by_id.insert(
                    record.id.clone(),
                    Candidate {
                        record,
                        relevance: 0.0,
                        score,
                        via: Via::Linked,
                        reasons: vec![format!("evidence of consultation {cid}")],
                        freshness,
                    },
                );
            }
        }

        // 4. What the user stated about the same subject as the selected conclusions. A follow-up
        // such as "which is the best value?" does not repeat "my budget is 5000 MXN", but the budget
        // bears on the answer; the shared subject ("oscilloscope") is what connects them.
        let subject: Vec<String> = {
            let mut terms: Vec<String> = Vec::new();
            for c in by_id
                .values()
                .filter(|c| c.record.kind == MemoryKind::Conclusion)
            {
                let first = c.record.text.lines().next().unwrap_or("");
                for t in tokenize_terms(first.strip_prefix("Question:").unwrap_or(first)) {
                    if !terms.contains(&t) {
                        terms.push(t);
                    }
                }
            }
            terms.sort();
            terms.truncate(10);
            terms
        };
        if !subject.is_empty() {
            let kinds = [
                MemoryKind::Preference,
                MemoryKind::Requirement,
                MemoryKind::Decision,
                MemoryKind::Interest,
            ];
            let allowed = req
                .kinds
                .as_deref()
                .is_none_or(|k| kinds.iter().any(|x| k.contains(x)));
            if allowed {
                let related = db.search(&match_expression(&subject, 10), &vis, Some(&kinds), 4)?;
                for (record, _) in related {
                    if by_id.contains_key(&record.id) {
                        continue;
                    }
                    let freshness = assess(&record, req.now_ms, req.wants_fresh, &req.freshness);
                    let score = weight::RELATED_BASE + quality(&record) + scope_bonus(record.scope);
                    by_id.insert(
                        record.id.clone(),
                        Candidate {
                            record,
                            relevance: 0.0,
                            score,
                            via: Via::Related,
                            reasons: vec!["a statement of yours about the same subject as a recalled conclusion".to_string()],
                            freshness,
                        },
                    );
                }
            }
        }

        let mut candidates: Vec<Candidate> = by_id.into_values().collect();
        candidates.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.record.id.cmp(&b.record.id))
        });
        out.candidates = candidates;
        out.elapsed_us = started.elapsed().as_micros() as u64;
        Ok(out)
    }
}

fn vis_allows(vis: &Visibility, r: &MemoryRecord) -> bool {
    if r.sensitive {
        return false;
    }
    match r.scope {
        MemoryScope::Session => {
            vis.session_scope && r.session_id == vis.session && r.project_id == vis.project
        }
        MemoryScope::Project => vis.project_scope && r.project_id == vis.project,
        MemoryScope::Global => vis.global_scope,
    }
}

/// The retrieval engine: a backend behind a stable interface.
pub struct MemoryRetrievalEngine {
    backend: Box<dyn RetrievalBackend>,
}

impl Default for MemoryRetrievalEngine {
    fn default() -> Self {
        Self {
            backend: Box::new(LexicalBackend),
        }
    }
}

impl MemoryRetrievalEngine {
    pub fn with_backend(backend: Box<dyn RetrievalBackend>) -> Self {
        Self { backend }
    }

    pub fn retrieve(&self, db: &MemoryDb, req: &RetrievalRequest) -> Result<RetrievalResult> {
        self.backend.retrieve(db, req)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::model::{Authorization, NewMemory, Origin};

    const NOW: u64 = 100 * 86_400_000;

    fn db() -> (tempfile::TempDir, MemoryDb) {
        let dir = tempfile::tempdir().unwrap();
        let db = MemoryDb::open(&dir.path().join("m.db")).unwrap();
        (dir, db)
    }

    fn req(question: &str, mode: MemoryMode, session: Option<&str>) -> RetrievalRequest {
        RetrievalRequest {
            question: question.into(),
            mode,
            global_in_auto: false,
            project: "p".into(),
            session: session.map(str::to_string),
            kinds: None,
            limit: 20,
            recent_turns: 2,
            wants_fresh: false,
            now_ms: NOW,
            freshness: FreshnessPolicy::default(),
        }
    }

    fn put(
        db: &mut MemoryDb,
        kind: MemoryKind,
        scope: MemoryScope,
        session: Option<&str>,
        text: &str,
        at_days_ago: u64,
    ) -> String {
        let mut m = NewMemory::new(kind, scope, text);
        m.project_id = Some("p".into());
        m.session_id = session.map(str::to_string);
        if kind == MemoryKind::Conclusion {
            m.origin = Origin::ConsultationFinal;
            m.status = EpistemicStatus::AgentInterpretation;
            m.authorization = Authorization::AutomaticSession;
        }
        db.insert_memory(&m, NOW - at_days_ago * 86_400_000)
            .unwrap()
            .id
    }

    fn ids(r: &RetrievalResult) -> Vec<String> {
        r.candidates.iter().map(|c| c.record.text.clone()).collect()
    }

    #[test]
    fn terms_drop_stopwords_keep_numbers_and_never_become_search_syntax() {
        let t = tokenize_terms(
            "What is the BEST oscilloscope for $8,000 MXN? And what about \"NEAR\" (a OR b)",
        );
        assert!(t.contains(&"best".to_string()) && t.contains(&"oscilloscope".to_string()));
        assert!(
            t.contains(&"8".to_string())
                && t.contains(&"000".to_string())
                && t.contains(&"mxn".to_string())
        );
        assert!(!t.contains(&"the".to_string()) && !t.contains(&"what".to_string()));
        let expr = match_expression(&t, 12);
        assert!(
            expr.starts_with('"') && expr.matches('"').count().is_multiple_of(2),
            "{expr}"
        );
        assert!(!expr.contains('('), "{expr}");
        assert_eq!(match_expression(&["a\"b".to_string()], 3), "\"ab\"");
    }

    #[test]
    fn a_question_is_matched_to_stored_words_with_the_best_match_first() {
        let (_d, mut db) = db();
        put(
            &mut db,
            MemoryKind::Note,
            MemoryScope::Project,
            None,
            "The Rigol DS1054Z oscilloscope has 50 MHz of bandwidth and four channels",
            5,
        );
        put(
            &mut db,
            MemoryKind::Note,
            MemoryScope::Project,
            None,
            "The microscope uses a 10x objective lens",
            5,
        );
        put(
            &mut db,
            MemoryKind::Note,
            MemoryScope::Project,
            None,
            "Oscilloscope probes need compensation",
            5,
        );
        let r = MemoryRetrievalEngine::default()
            .retrieve(
                &db,
                &req(
                    "Which oscilloscope has more bandwidth?",
                    MemoryMode::Project,
                    None,
                ),
            )
            .unwrap();
        let texts = ids(&r);
        assert!(texts[0].contains("bandwidth"), "{texts:?}");
        assert!(
            !texts.iter().any(|t| t.contains("microscope uses")),
            "{texts:?}"
        );
        assert!(
            r.candidates[0]
                .reasons
                .iter()
                .any(|x| x.starts_with("matched")),
            "{:?}",
            r.candidates[0].reasons
        );
        assert!(r.candidates[0].relevance > r.candidates[1].relevance - 1e-9);
    }

    #[test]
    fn stemming_and_accents_do_not_hide_a_match() {
        let (_d, mut db) = db();
        put(
            &mut db,
            MemoryKind::Note,
            MemoryScope::Project,
            None,
            "Los osciloscopios portátiles son útiles para aprender",
            1,
        );
        put(
            &mut db,
            MemoryKind::Note,
            MemoryScope::Project,
            None,
            "Compared two oscilloscopes for learning",
            1,
        );
        let r = MemoryRetrievalEngine::default()
            .retrieve(
                &db,
                &req("portatiles osciloscopios", MemoryMode::Project, None),
            )
            .unwrap();
        assert!(
            ids(&r).iter().any(|t| t.contains("portátiles")),
            "diacritics are ignored: {:?}",
            ids(&r)
        );
        let r = MemoryRetrievalEngine::default()
            .retrieve(
                &db,
                &req("oscilloscope learning", MemoryMode::Project, None),
            )
            .unwrap();
        assert!(
            ids(&r).iter().any(|t| t.contains("oscilloscopes")),
            "plural matches singular: {:?}",
            ids(&r)
        );
    }

    #[test]
    fn a_follow_up_with_no_shared_words_still_finds_the_latest_turns_of_its_session() {
        let (_d, mut db) = db();
        put(
            &mut db,
            MemoryKind::Conclusion,
            MemoryScope::Session,
            Some("s1"),
            "Q: compare three portable oscilloscopes. Result: Rigol DS1054Z, Siglent SDS1104X-E, Hantek DSO2D15",
            1,
        );
        put(
            &mut db,
            MemoryKind::Conclusion,
            MemoryScope::Session,
            Some("other"),
            "Q: unrelated cooking. Result: soup",
            1,
        );
        let r = MemoryRetrievalEngine::default()
            .retrieve(
                &db,
                &req(
                    "Of those, which one is the best value?",
                    MemoryMode::Auto,
                    Some("s1"),
                ),
            )
            .unwrap();
        assert_eq!(r.lexical_hits, 0, "no word in common");
        assert_eq!(r.candidates.len(), 1);
        assert_eq!(r.candidates[0].via, Via::Recent);
        assert!(
            r.candidates[0].reasons[0].contains("the latest consultation"),
            "{:?}",
            r.candidates[0].reasons
        );
        assert!(r.candidates[0].reasons[0].contains("continues an earlier one"));
        assert!(
            !ids(&r).iter().any(|t| t.contains("soup")),
            "another session is not visible"
        );
    }

    #[test]
    fn a_question_that_names_its_subject_does_not_drag_in_unrelated_latest_turns() {
        let (_d, mut db) = db();
        put(
            &mut db,
            MemoryKind::Conclusion,
            MemoryScope::Session,
            Some("s1"),
            "Question: intermittent fasting and longevity\nConclusion: evidence in humans is limited",
            20,
        );
        put(
            &mut db,
            MemoryKind::Conclusion,
            MemoryScope::Session,
            Some("s1"),
            "Question: sauces\nConclusion: reduce slowly",
            1,
        );
        let r = MemoryRetrievalEngine::default()
            .retrieve(
                &db,
                &req(
                    "What did we conclude about intermittent fasting and longevity?",
                    MemoryMode::Auto,
                    Some("s1"),
                ),
            )
            .unwrap();
        assert!(ids(&r).iter().any(|t| t.contains("fasting")));
        assert!(
            !ids(&r).iter().any(|t| t.contains("sauces")),
            "{:?}",
            ids(&r)
        );
        // A continuation always brings the latest turn.
        let r = MemoryRetrievalEngine::default()
            .retrieve(
                &db,
                &req(
                    "Now, about intermittent fasting and longevity, and the previous answer?",
                    MemoryMode::Auto,
                    Some("s1"),
                ),
            )
            .unwrap();
        assert!(
            ids(&r).iter().any(|t| t.contains("sauces")),
            "{:?}",
            ids(&r)
        );
    }

    #[test]
    fn a_user_statement_about_the_same_subject_comes_along_with_a_follow_up_that_does_not_repeat_it()
     {
        let (_d, mut db) = db();
        put(
            &mut db,
            MemoryKind::Conclusion,
            MemoryScope::Session,
            Some("s1"),
            "Question: compare three portable oscilloscopes\nConclusion: Rigol, Siglent, Hantek",
            1,
        );
        put(
            &mut db,
            MemoryKind::Preference,
            MemoryScope::Project,
            None,
            "My oscilloscope budget is 5000 MXN",
            3,
        );
        put(
            &mut db,
            MemoryKind::Preference,
            MemoryScope::Project,
            None,
            "I like tomatoes on the balcony",
            3,
        );
        let r = MemoryRetrievalEngine::default()
            .retrieve(
                &db,
                &req(
                    "Of those, which one is the best value for money?",
                    MemoryMode::Auto,
                    Some("s1"),
                ),
            )
            .unwrap();
        let related: Vec<_> = r
            .candidates
            .iter()
            .filter(|c| c.via == Via::Related)
            .collect();
        assert_eq!(related.len(), 1, "{:?}", ids(&r));
        assert!(related[0].record.text.contains("5000 MXN"));
        assert!(
            !ids(&r).iter().any(|t| t.contains("tomatoes")),
            "{:?}",
            r.candidates
                .iter()
                .map(|c| (c.record.text.clone(), c.via, c.reasons.clone()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn evidence_of_a_selected_consultation_comes_along_and_is_marked_as_linked() {
        let (_d, mut db) = db();
        let mut c = NewMemory::new(
            MemoryKind::Conclusion,
            MemoryScope::Session,
            "Q: oscilloscopes. Result: the Rigol",
        );
        c.project_id = Some("p".into());
        c.session_id = Some("s1".into());
        c.consultation_id = Some("cons-1".into());
        c.origin = Origin::ConsultationFinal;
        c.status = EpistemicStatus::AgentInterpretation;
        db.insert_memory(&c, NOW - 86_400_000).unwrap();
        let mut e = NewMemory::new(
            MemoryKind::Evidence,
            MemoryScope::Session,
            "The DS1054Z has 50 MHz of bandwidth",
        );
        e.project_id = Some("p".into());
        e.session_id = Some("s1".into());
        e.consultation_id = Some("cons-1".into());
        e.origin = Origin::Evidence;
        e.status = EpistemicStatus::PreviouslyVerified;
        e.retrieved_ms = Some(NOW - 86_400_000);
        db.insert_memory(&e, NOW - 86_400_000).unwrap();
        let r = MemoryRetrievalEngine::default()
            .retrieve(
                &db,
                &req("and the one you picked?", MemoryMode::Auto, Some("s1")),
            )
            .unwrap();
        let linked: Vec<_> = r
            .candidates
            .iter()
            .filter(|c| c.via == Via::Linked)
            .collect();
        assert_eq!(linked.len(), 1, "{:?}", ids(&r));
        assert!(linked[0].reasons[0].contains("cons-1"));
        assert!(matches!(
            linked[0].freshness,
            Freshness::NotApplicable
                | Freshness::Current { .. }
                | Freshness::Stale { .. }
                | Freshness::Unknown
        ));
    }

    #[test]
    fn user_statements_beat_agent_conclusions_with_the_same_relevance_and_recency_is_only_a_nudge()
    {
        let (_d, mut db) = db();
        put(
            &mut db,
            MemoryKind::Conclusion,
            MemoryScope::Project,
            None,
            "budget recommendation for the lab scope",
            0,
        );
        let mut user = NewMemory::new(
            MemoryKind::Preference,
            MemoryScope::Project,
            "budget limit for the lab scope",
        );
        user.project_id = Some("p".into());
        db.insert_memory(&user, NOW - 60 * 86_400_000).unwrap();
        let r = MemoryRetrievalEngine::default()
            .retrieve(&db, &req("budget lab scope", MemoryMode::Project, None))
            .unwrap();
        assert_eq!(
            r.candidates[0].record.kind,
            MemoryKind::Preference,
            "an old user statement outranks a brand-new AI conclusion"
        );
        let ages: Vec<f64> = r.candidates.iter().map(|c| c.score).collect();
        assert!(ages[0] > ages[1]);
    }

    #[test]
    fn the_mode_decides_which_scopes_are_searched_and_off_reads_nothing() {
        let (_d, mut db) = db();
        put(
            &mut db,
            MemoryKind::Note,
            MemoryScope::Session,
            Some("s1"),
            "alpha session fact",
            1,
        );
        put(
            &mut db,
            MemoryKind::Note,
            MemoryScope::Project,
            None,
            "alpha project fact",
            1,
        );
        let mut g = NewMemory::new(
            MemoryKind::Preference,
            MemoryScope::Global,
            "alpha global fact",
        );
        g.project_id = None;
        db.insert_memory(&g, NOW - 86_400_000).unwrap();
        let e = MemoryRetrievalEngine::default();
        let texts = |mode| {
            let mut v = ids(&e
                .retrieve(&db, &req("alpha fact", mode, Some("s1")))
                .unwrap());
            v.sort();
            v
        };
        assert_eq!(texts(MemoryMode::Session), ["alpha session fact"]);
        assert_eq!(
            texts(MemoryMode::Auto),
            ["alpha project fact", "alpha session fact"]
        );
        assert_eq!(
            texts(MemoryMode::Project),
            ["alpha project fact", "alpha session fact"]
        );
        assert_eq!(
            texts(MemoryMode::Global),
            [
                "alpha global fact",
                "alpha project fact",
                "alpha session fact"
            ]
        );
        let off = e
            .retrieve(&db, &req("alpha fact", MemoryMode::Off, Some("s1")))
            .unwrap();
        assert!(off.candidates.is_empty() && off.notes[0].contains("memory is off"));
        let mut cfg = req("alpha fact", MemoryMode::Auto, Some("s1"));
        cfg.global_in_auto = true;
        assert_eq!(
            e.retrieve(&db, &cfg).unwrap().candidates.len(),
            3,
            "auto includes global only when configured"
        );
    }

    #[test]
    fn another_project_and_sensitive_records_are_never_returned() {
        let (_d, mut db) = db();
        let mut other = NewMemory::new(
            MemoryKind::Note,
            MemoryScope::Project,
            "zeta other project secret",
        );
        other.project_id = Some("other".into());
        db.insert_memory(&other, NOW).unwrap();
        let mut sensitive = NewMemory::new(
            MemoryKind::Note,
            MemoryScope::Project,
            "zeta contact ana@example.com",
        );
        sensitive.project_id = Some("p".into());
        sensitive.sensitive = true;
        db.insert_memory(&sensitive, NOW).unwrap();
        put(
            &mut db,
            MemoryKind::Note,
            MemoryScope::Project,
            None,
            "zeta ordinary note",
            1,
        );
        let r = MemoryRetrievalEngine::default()
            .retrieve(&db, &req("zeta", MemoryMode::Global, None))
            .unwrap();
        assert_eq!(ids(&r), ["zeta ordinary note"]);
    }

    #[test]
    fn retrieval_is_deterministic() {
        let (_d, mut db) = db();
        for i in 0..8 {
            put(
                &mut db,
                MemoryKind::Note,
                MemoryScope::Project,
                None,
                &format!("shared topic words item {i}"),
                i,
            );
        }
        let e = MemoryRetrievalEngine::default();
        let first = ids(&e
            .retrieve(&db, &req("shared topic words", MemoryMode::Project, None))
            .unwrap());
        for _ in 0..10 {
            assert_eq!(
                ids(&e
                    .retrieve(&db, &req("shared topic words", MemoryMode::Project, None))
                    .unwrap()),
                first
            );
        }
    }

    #[test]
    fn exact_names_and_numbers_get_a_small_boost() {
        assert_eq!(
            entities("Compare the DS1054Z with the Siglent SDS1104X-E please"),
            vec!["ds1054z", "siglent", "sds1104x-e"]
        );
        let (_d, mut db) = db();
        put(
            &mut db,
            MemoryKind::Note,
            MemoryScope::Project,
            None,
            "scope notes mention the DS1054Z often",
            1,
        );
        put(
            &mut db,
            MemoryKind::Note,
            MemoryScope::Project,
            None,
            "scope notes mention another model often",
            1,
        );
        let r = MemoryRetrievalEngine::default()
            .retrieve(
                &db,
                &req("scope notes about the DS1054Z", MemoryMode::Project, None),
            )
            .unwrap();
        assert!(r.candidates[0].record.text.contains("DS1054Z"));
        assert!(
            r.candidates[0]
                .reasons
                .iter()
                .any(|x| x.contains("exact name or number"))
        );
    }

    #[test]
    fn continuation_wording_is_recognised_in_english_and_spanish() {
        for q in [
            "Of the three above, which?",
            "Ahora aumenta el presupuesto",
            "Is the same model still good?",
            "los anteriores",
        ] {
            assert!(continues_earlier_question(q), "{q}");
        }
        assert!(!continues_earlier_question("Explain quantum computing"));
    }
}
