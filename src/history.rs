//! Saved debates: unique ids, atomic JSON persistence, listing and lookup.
//!
//! One pretty-printed JSON file per debate (`<id>.json`) in a private
//! directory. Files hold prompts and replies, so they are created owner-only
//! and pass through [`redact_secrets`] first. A write goes to a temporary file
//! in the same directory, is synced, and is then renamed over the final name,
//! so a crash never leaves a half-written debate behind.

use std::collections::hash_map::RandomState;
use std::fs::{self, File, OpenOptions};
use std::hash::{BuildHasher, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::debate::{DebateOutcome, DebateStatus};
use crate::error::{OrchestraError, Result};
use crate::store::redact_secrets;

/// Refuse to parse saved files larger than this (they would not be ours).
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Converts days since 1970-01-01 to a (year, month, day) in the proleptic
/// Gregorian calendar (Howard Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

fn utc_parts(ms: u64) -> (i64, u32, u32, u32, u32, u32) {
    let secs = i64::try_from(ms / 1000).unwrap_or(i64::MAX / 2);
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400);
    (
        y,
        m,
        d,
        (rem / 3600) as u32,
        (rem % 3600 / 60) as u32,
        (rem % 60) as u32,
    )
}

/// `2026-10-11 09:46:00Z`
pub fn format_utc(ms: u64) -> String {
    let (y, mo, d, h, mi, s) = utc_parts(ms);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}Z")
}

/// A sortable, human-readable id such as `20261011-094600-0a1f3c`. The suffix
/// is a per-process counter (12 bits, so ids from one process never collide
/// within the same second) followed by 12 random bits from the standard
/// library's per-process hasher keys (so two processes starting in the same
/// second almost never collide). [`DebateStore::fresh_id`] additionally checks
/// the directory.
pub fn new_debate_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let (y, mo, d, h, mi, s) = utc_parts(now_ms());
    let count = COUNTER.fetch_add(1, Ordering::Relaxed) & 0xfff;
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u64(count);
    hasher.write_u32(std::process::id());
    format!(
        "{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}-{count:03x}{:03x}",
        hasher.finish() & 0xfff
    )
}

/// Ids are used as file names, so only a safe alphabet is accepted.
fn valid_id_fragment(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[derive(Debug, Clone, Serialize)]
pub struct DebateSummary {
    pub id: String,
    pub created_at_ms: u64,
    pub status: DebateStatus,
    pub coordinator: String,
    pub question: String,
    pub calls: usize,
    pub duration_ms: u64,
    /// Consultation mode, for records made by `consult`; absent for plain debates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
}

#[derive(Debug, Default)]
pub struct Listing {
    /// Newest first.
    pub debates: Vec<DebateSummary>,
    /// Files in the directory that could not be read as a debate.
    pub unreadable: usize,
}

#[derive(Debug, Clone)]
pub struct DebateStore {
    dir: PathBuf,
}

impl DebateStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path_for(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    /// An id that no saved debate uses yet.
    pub fn fresh_id(&self) -> String {
        loop {
            let id = new_debate_id();
            if !self.path_for(&id).exists() {
                return id;
            }
        }
    }

    /// Atomically writes `outcome` and returns its path.
    pub fn save(&self, outcome: &DebateOutcome) -> Result<PathBuf> {
        if !valid_id_fragment(&outcome.id) {
            return Err(OrchestraError::History(format!(
                "refusing to save a debate with an unsafe id `{}`",
                outcome.id
            )));
        }
        create_private_dir(&self.dir)?;
        let json = serde_json::to_vec_pretty(&redacted(outcome))
            .map_err(|e| OrchestraError::History(format!("cannot serialize the debate: {e}")))?;

        static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);
        let tmp = self.dir.join(format!(
            ".{}.json.tmp-{}-{}",
            outcome.id,
            std::process::id(),
            TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let target = self.path_for(&outcome.id);
        let written = write_private_file(&tmp, &json).and_then(|()| fs::rename(&tmp, &target));
        if let Err(e) = written {
            let _ = fs::remove_file(&tmp);
            return Err(OrchestraError::Io(e));
        }
        // Make the rename itself durable; not every platform allows this.
        if let Ok(dir) = File::open(&self.dir) {
            let _ = dir.sync_all();
        }
        Ok(target)
    }

    fn read_file(path: &Path) -> Result<DebateOutcome> {
        let meta = fs::metadata(path)?;
        if meta.len() > MAX_FILE_BYTES {
            return Err(OrchestraError::History(format!(
                "{} is too large to be a saved debate",
                path.display()
            )));
        }
        let bytes = fs::read(path)?;
        serde_json::from_slice(&bytes).map_err(|e| {
            OrchestraError::History(format!(
                "{} is not a valid saved debate: {e}",
                path.display()
            ))
        })
    }

    /// Finds a saved debate by full id or unique prefix.
    pub fn load(&self, id_or_prefix: &str) -> Result<DebateOutcome> {
        if !valid_id_fragment(id_or_prefix) {
            return Err(OrchestraError::History(format!(
                "`{id_or_prefix}` is not a valid debate id (use letters, digits, `-` and `_`)"
            )));
        }
        let mut matches: Vec<String> = Vec::new();
        if let Ok(entries) = fs::read_dir(&self.dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if let Some(stem) = name.strip_suffix(".json")
                    && !stem.starts_with('.')
                    && stem.starts_with(id_or_prefix)
                {
                    matches.push(stem.to_string());
                }
            }
        }
        matches.sort();
        if let Some(exact) = matches.iter().find(|m| m.as_str() == id_or_prefix) {
            return Self::read_file(&self.path_for(exact));
        }
        match matches.as_slice() {
            [] => Err(OrchestraError::History(format!(
                "no saved debate matches `{id_or_prefix}` (see `ai-orchestra history`)"
            ))),
            [only] => Self::read_file(&self.path_for(only)),
            many => Err(OrchestraError::History(format!(
                "`{id_or_prefix}` matches {} debates ({}); use more characters",
                many.len(),
                many.iter().take(5).cloned().collect::<Vec<_>>().join(", ")
            ))),
        }
    }

    /// Lists saved debates, newest first. A missing directory is an empty list.
    pub fn list(&self) -> Listing {
        let mut listing = Listing::default();
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return listing;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || !name.ends_with(".json") {
                continue;
            }
            match Self::read_file(&entry.path()) {
                Ok(d) => listing.debates.push(DebateSummary {
                    id: d.id.clone(),
                    created_at_ms: d.created_at_ms,
                    status: d.status,
                    coordinator: d.coordinator.to_string(),
                    question: d.question.clone(),
                    calls: d.calls,
                    duration_ms: d.duration_ms,
                    mode: d
                        .consultation
                        .as_ref()
                        .and_then(|c| c.mode)
                        .map(|m| m.to_string()),
                }),
                Err(_) => listing.unreadable += 1,
            }
        }
        listing
            .debates
            .sort_by(|a, b| b.created_at_ms.cmp(&a.created_at_ms).then(b.id.cmp(&a.id)));
        listing
    }
}

/// One saved consultation that matched a search.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SearchHit {
    pub summary: DebateSummary,
    /// How many times the search words occur in the question and answer.
    pub matches: usize,
    /// A short passage around the first match in the answer, or the start of the question.
    pub snippet: String,
}

impl DebateStore {
    /// Plain-text search of saved consultations: every word of `query` must occur (case-insensitive)
    /// in the question or the final answer. Newest first among equal matches. No model is involved.
    pub fn search(&self, query: &str, limit: usize) -> Vec<SearchHit> {
        let words: Vec<String> = query
            .split_whitespace()
            .map(|w| w.to_lowercase())
            .filter(|w| !w.is_empty())
            .collect();
        if words.is_empty() {
            return Vec::new();
        }
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut hits = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || !name.ends_with(".json") {
                continue;
            }
            let Ok(d) = Self::read_file(&entry.path()) else {
                continue;
            };
            let hay = format!("{}\n{}", d.question, d.final_answer).to_lowercase();
            if !words.iter().all(|w| hay.contains(w.as_str())) {
                continue;
            }
            let matches = words.iter().map(|w| hay.matches(w.as_str()).count()).sum();
            let answer_lower = d.final_answer.to_lowercase();
            let snippet = match answer_lower.find(words[0].as_str()) {
                Some(i) => {
                    let start = d.final_answer[..i.min(d.final_answer.len())]
                        .char_indices()
                        .rev()
                        .nth(60)
                        .map_or(0, |(b, _)| b);
                    d.final_answer[start..]
                        .chars()
                        .take(160)
                        .collect::<String>()
                        .replace('\n', " ")
                }
                None => d.question.chars().take(160).collect(),
            };
            hits.push(SearchHit {
                summary: DebateSummary {
                    id: d.id.clone(),
                    created_at_ms: d.created_at_ms,
                    status: d.status,
                    coordinator: d.coordinator.to_string(),
                    question: d.question.clone(),
                    calls: d.calls,
                    duration_ms: d.duration_ms,
                    mode: d
                        .consultation
                        .as_ref()
                        .and_then(|c| c.mode)
                        .map(|m| m.to_string()),
                },
                matches,
                snippet,
            });
        }
        hits.sort_by(|a, b| {
            b.matches
                .cmp(&a.matches)
                .then(b.summary.created_at_ms.cmp(&a.summary.created_at_ms))
        });
        hits.truncate(limit);
        hits
    }
}

/// A saved consultation with the facts the web interface filters and shows, read without the transcript.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DebateDetail {
    #[serde(flatten)]
    pub summary: DebateSummary,
    pub strategy: Option<String>,
    pub participants: Vec<String>,
    pub has_research: bool,
    pub retrieved_sources: usize,
    pub answer_preview: String,
}

impl DebateStore {
    /// Every saved consultation with its filterable facts, newest first. Reads each file once.
    pub fn list_details(&self) -> Vec<DebateDetail> {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || !name.ends_with(".json") {
                continue;
            }
            let Ok(d) = Self::read_file(&entry.path()) else {
                continue;
            };
            let meta = d.consultation.as_ref();
            out.push(DebateDetail {
                summary: DebateSummary {
                    id: d.id.clone(),
                    created_at_ms: d.created_at_ms,
                    status: d.status,
                    coordinator: d.coordinator.to_string(),
                    question: d.question.clone(),
                    calls: d.calls,
                    duration_ms: d.duration_ms,
                    mode: meta.and_then(|c| c.mode).map(|m| m.to_string()),
                },
                strategy: meta.map(|c| c.strategy.clone()),
                participants: d.participants.iter().map(ToString::to_string).collect(),
                has_research: meta.is_some_and(|c| c.research.is_some()),
                retrieved_sources: meta
                    .and_then(|c| c.research.as_ref())
                    .map_or(0, |r| r.retrieved()),
                answer_preview: d
                    .final_answer
                    .chars()
                    .take(240)
                    .collect::<String>()
                    .replace('\n', " "),
            });
        }
        out.sort_by(|a, b| {
            b.summary
                .created_at_ms
                .cmp(&a.summary.created_at_ms)
                .then(b.summary.id.cmp(&a.summary.id))
        });
        out
    }

    /// Calls `f` with every readable saved consultation (each file is read once).
    pub fn for_each(&self, mut f: impl FnMut(&DebateOutcome)) {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || !name.ends_with(".json") {
                continue;
            }
            if let Ok(d) = Self::read_file(&entry.path()) {
                f(&d);
            }
        }
    }

    /// Deletes one saved consultation (full id only). The only thing removed is that record's file.
    pub fn delete(&self, id: &str) -> Result<()> {
        if !valid_id_fragment(id) {
            return Err(OrchestraError::History(format!(
                "`{id}` is not a valid consultation id"
            )));
        }
        let path = self.path_for(id);
        if !path.is_file() {
            return Err(OrchestraError::History(format!(
                "no saved consultation `{id}`"
            )));
        }
        fs::remove_file(path)?;
        Ok(())
    }
}

fn create_private_dir(dir: &Path) -> Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)?;
    Ok(())
}

fn write_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// A copy of `outcome` with credential-shaped strings masked.
fn redacted(outcome: &DebateOutcome) -> DebateOutcome {
    let mut o = outcome.clone();
    o.question = redact_secrets(&o.question);
    o.initial_proposal = redact_secrets(&o.initial_proposal);
    o.final_answer = redact_secrets(&o.final_answer);
    o.error = o.error.as_deref().map(redact_secrets);
    for w in &mut o.warnings {
        *w = redact_secrets(w);
    }
    for step in &mut o.steps {
        step.text = step.text.as_deref().map(redact_secrets);
        step.error = step.error.as_deref().map(redact_secrets);
    }
    if let Some(meta) = &mut o.consultation {
        for field in [
            &mut meta.recommendation,
            &mut meta.agreements,
            &mut meta.disagreements,
            &mut meta.assumptions,
            &mut meta.evidence_limitations,
        ] {
            *field = field.as_deref().map(redact_secrets);
        }
    }
    o
}

/// Table for `ai-orchestra history`.
pub fn render_history(listing: &Listing, limit: usize) -> String {
    if listing.debates.is_empty() {
        let mut out = String::from("No saved debates yet. Run `ai-orchestra debate \"...\"`.\n");
        if listing.unreadable > 0 {
            out.push_str(&format!(
                "({} unreadable file(s) skipped)\n",
                listing.unreadable
            ));
        }
        return out;
    }
    let id_width = listing
        .debates
        .iter()
        .take(limit)
        .map(|d| d.id.len())
        .max()
        .unwrap_or(2)
        .max(2);
    // Consultations add a KIND column; a list of plain debates looks as it always did.
    let kinds = listing.debates.iter().take(limit).any(|d| d.mode.is_some());
    let mut out = format!(
        "{:<id_width$} {}{:<9} {:<9} {:>5} {:>7}  {}\n",
        "ID",
        if kinds { "KIND      " } else { "" },
        "STATUS",
        "COORD",
        "CALLS",
        "TIME",
        "QUESTION"
    );
    for d in listing.debates.iter().take(limit) {
        let question: String = d.question.split_whitespace().collect::<Vec<_>>().join(" ");
        let question = if question.chars().count() > 60 {
            format!("{}...", question.chars().take(57).collect::<String>())
        } else {
            question
        };
        out.push_str(&format!(
            "{:<id_width$} {}{:<9} {:<9} {:>5} {:>6.1}s  {}\n",
            d.id,
            if kinds {
                format!("{:<10}", d.mode.as_deref().unwrap_or("debate"))
            } else {
                String::new()
            },
            d.status.as_str(),
            d.coordinator,
            d.calls,
            d.duration_ms as f64 / 1000.0,
            question
        ));
    }
    if listing.debates.len() > limit {
        out.push_str(&format!(
            "... {} more (use --limit)\n",
            listing.debates.len() - limit
        ));
    }
    if listing.unreadable > 0 {
        out.push_str(&format!(
            "({} unreadable file(s) skipped)\n",
            listing.unreadable
        ));
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::debate::{DebateOutcome, Role, Step};
    use crate::store::ConversationId;
    use crate::types::ProviderId;

    pub(crate) fn sample(id: &str, created_at_ms: u64, status: DebateStatus) -> DebateOutcome {
        DebateOutcome {
            schema: 1,
            id: id.to_string(),
            conversation: ConversationId(1),
            created_at_ms,
            question: "What is best?".to_string(),
            coordinator: ProviderId::new("claude"),
            participants: vec![ProviderId::new("codex")],
            skipped: vec![],
            rounds_planned: 1,
            status,
            synthesized: true,
            steps: vec![Step {
                phase: Role::Proposal,
                round: 0,
                agent: ProviderId::new("claude"),
                offset_ms: 0,
                duration_ms: 1200,
                ok: true,
                text: Some("proposal".into()),
                error: None,
                error_kind: None,
                settings: None,
                usage: None,
            }],
            initial_proposal: "proposal".to_string(),
            final_answer: "final".to_string(),
            error: None,
            calls: 3,
            warnings: vec![],
            duration_ms: 5000,
            consultation: None,
            summaries: Vec::new(),
            saved_to: None,
        }
    }

    #[test]
    fn utc_formatting_matches_known_instants() {
        assert_eq!(format_utc(0), "1970-01-01 00:00:00Z");
        assert_eq!(format_utc(951_782_400_000), "2000-02-29 00:00:00Z"); // leap day
        assert_eq!(format_utc(4_102_444_799_000), "2099-12-31 23:59:59Z");
        // Cross-checked against Python's datetime.
        assert_eq!(format_utc(1_791_732_135_161), "2026-10-11 15:22:15Z");
    }

    #[test]
    fn ids_are_sortable_unique_and_filename_safe() {
        // Far more than one process would make in a second; none may collide.
        let ids: Vec<String> = (0..4000).map(|_| new_debate_id()).collect();
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len(), "ids must not collide");
        for id in &ids {
            assert!(valid_id_fragment(id), "{id}");
            assert_eq!(id.len(), "20261011-094600-0a1f3c".len(), "{id}");
        }
    }

    #[test]
    fn fresh_ids_skip_ones_already_saved() {
        let dir = tempfile::tempdir().unwrap();
        let store = DebateStore::new(dir.path().to_path_buf());
        let first = store.fresh_id();
        store
            .save(&sample(&first, 1, DebateStatus::Complete))
            .unwrap();
        let second = store.fresh_id();
        assert_ne!(first, second);
        assert!(!store.path_for(&second).exists());
    }

    #[test]
    fn save_then_load_round_trips_and_is_private() {
        let dir = tempfile::tempdir().unwrap();
        let store = DebateStore::new(dir.path().join("nested").join("debates"));
        let d = sample("20261011-094600-a1b2", 1000, DebateStatus::Complete);
        let path = store.save(&d).unwrap();
        assert!(path.ends_with("20261011-094600-a1b2.json"));
        let back = store.load("20261011-094600-a1b2").unwrap();
        assert_eq!(back.final_answer, "final");
        assert_eq!(back.steps.len(), 1);
        assert_eq!(back.status, DebateStatus::Complete);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(store.dir()).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
    }

    #[test]
    fn writes_are_atomic_and_leave_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let store = DebateStore::new(dir.path().to_path_buf());
        let mut d = sample("20261011-000000-0001", 1, DebateStatus::Partial);
        store.save(&d).unwrap();
        d.final_answer = "second version".to_string();
        store.save(&d).unwrap(); // overwrite replaces the file whole
        assert_eq!(
            store.load("20261011-000000-0001").unwrap().final_answer,
            "second version"
        );
        let names: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec!["20261011-000000-0001.json"],
            "no temp files: {names:?}"
        );
        // The file is always complete JSON.
        let text = fs::read_to_string(dir.path().join("20261011-000000-0001.json")).unwrap();
        serde_json::from_str::<serde_json::Value>(&text).unwrap();
    }

    #[test]
    fn a_failed_write_cleans_up_and_keeps_the_previous_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = DebateStore::new(dir.path().to_path_buf());
        let d = sample("20261011-000000-0002", 1, DebateStatus::Complete);
        store.save(&d).unwrap();
        // Make the rename target a directory so the final step fails.
        let blocked = sample("20261011-000000-0003", 2, DebateStatus::Complete);
        fs::create_dir(dir.path().join("20261011-000000-0003.json")).unwrap();
        assert!(store.save(&blocked).is_err());
        let stray: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(stray.is_empty(), "temp file left behind");
        assert_eq!(
            store.load("20261011-000000-0002").unwrap().final_answer,
            "final"
        );
    }

    #[test]
    fn secrets_are_masked_before_anything_reaches_disk() {
        let dir = tempfile::tempdir().unwrap();
        let store = DebateStore::new(dir.path().to_path_buf());
        let mut d = sample("20261011-000000-0004", 1, DebateStatus::Complete);
        d.question = "use OPENAI_API_KEY=hunter2".to_string();
        d.final_answer = "token sk-abcdefghijklmnopqrstuvwx leaked".to_string();
        d.steps[0].text = Some("ANTHROPIC_API_KEY=topsecret".to_string());
        let path = store.save(&d).unwrap();
        let text = fs::read_to_string(path).unwrap();
        for secret in ["hunter2", "sk-abcdefghijklmnopqrstuvwx", "topsecret"] {
            assert!(!text.contains(secret), "{secret} reached the file");
        }
    }

    #[test]
    fn list_is_newest_first_and_skips_junk() {
        let dir = tempfile::tempdir().unwrap();
        let store = DebateStore::new(dir.path().to_path_buf());
        store
            .save(&sample(
                "20261011-000001-aaaa",
                1000,
                DebateStatus::Complete,
            ))
            .unwrap();
        store
            .save(&sample("20261011-000003-cccc", 3000, DebateStatus::Partial))
            .unwrap();
        store
            .save(&sample("20261011-000002-bbbb", 2000, DebateStatus::Failed))
            .unwrap();
        fs::write(dir.path().join("broken.json"), "{ not json").unwrap();
        fs::write(dir.path().join("notes.txt"), "ignored").unwrap();
        fs::write(dir.path().join(".hidden.json.tmp-1"), "ignored").unwrap();
        let listing = store.list();
        let ids: Vec<&str> = listing.debates.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "20261011-000003-cccc",
                "20261011-000002-bbbb",
                "20261011-000001-aaaa"
            ]
        );
        assert_eq!(listing.unreadable, 1);
        let text = render_history(&listing, 2);
        assert!(
            text.contains("partial")
                && text.contains("failed")
                && !text.contains("20261011-000001-aaaa")
        );
        assert!(text.contains("1 more") && text.contains("unreadable"));
        assert!(
            DebateStore::new(dir.path().join("missing"))
                .list()
                .debates
                .is_empty()
        );
        assert!(render_history(&Listing::default(), 5).contains("No saved debates"));
    }

    #[test]
    fn history_columns_line_up_whatever_the_id_length() {
        for id in ["20261011-000001-aaaa", "20261011-000001-aaaaaa"] {
            let dir = tempfile::tempdir().unwrap();
            let store = DebateStore::new(dir.path().to_path_buf());
            store.save(&sample(id, 1, DebateStatus::Partial)).unwrap();
            let text = render_history(&store.list(), 5);
            let mut lines = text.lines();
            let (header, row) = (lines.next().unwrap(), lines.next().unwrap());
            assert_eq!(header.find("STATUS"), row.find("partial"), "{text}");
            assert_eq!(header.find("QUESTION"), row.find("What is best?"), "{text}");
        }
    }

    #[test]
    fn lookup_by_unique_prefix_and_rejects_unsafe_or_ambiguous_ids() {
        let dir = tempfile::tempdir().unwrap();
        let store = DebateStore::new(dir.path().to_path_buf());
        store
            .save(&sample("20261011-000001-aaaa", 1, DebateStatus::Complete))
            .unwrap();
        store
            .save(&sample("20261011-000001-aabb", 2, DebateStatus::Complete))
            .unwrap();
        assert_eq!(
            store.load("20261011-000001-aaaa").unwrap().id,
            "20261011-000001-aaaa"
        );
        assert_eq!(
            store.load("20261011-000001-aab").unwrap().id,
            "20261011-000001-aabb"
        );
        for bad in ["../etc/passwd", "a/b", "", "x y", ".."] {
            assert!(
                matches!(store.load(bad).err().unwrap(), OrchestraError::History(_)),
                "{bad}"
            );
        }
        let ambiguous = store.load("20261011-000001").err().unwrap().to_string();
        assert!(ambiguous.contains("matches 2 debates"), "{ambiguous}");
        let missing = store.load("2099").err().unwrap().to_string();
        assert!(missing.contains("no saved debate"), "{missing}");
        // Unsafe ids are never written either.
        let mut evil = sample("../escape", 1, DebateStatus::Complete);
        assert!(store.save(&evil).is_err());
        evil.id = "ok-id".into();
        assert!(store.save(&evil).is_ok());
    }
}
