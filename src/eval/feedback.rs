//! Explicit user feedback on a consultation, stored locally.
//!
//! Feedback is a note attached to a consultation id. It is **not** a preference, it trains nothing,
//! and it changes no routing or provider ranking: using it that way would need a larger sample and a
//! separately validated step. The user can list, inspect and delete every entry.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{OrchestraError, Result};
use crate::history::{new_debate_id, now_ms};
use crate::store::redact_secrets;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rating {
    Helpful,
    NotHelpful,
    Inaccurate,
    Incomplete,
    BadCitation,
    Slow,
    Verbose,
    Unsatisfactory,
}

impl Rating {
    pub const NAMES: [&'static str; 8] = [
        "helpful",
        "not-helpful",
        "inaccurate",
        "incomplete",
        "bad-citation",
        "slow",
        "verbose",
        "unsatisfactory",
    ];

    pub fn parse(s: &str) -> Option<Rating> {
        Some(match s.trim().to_lowercase().replace('_', "-").as_str() {
            "helpful" => Rating::Helpful,
            "not-helpful" | "nothelpful" => Rating::NotHelpful,
            "inaccurate" => Rating::Inaccurate,
            "incomplete" | "missing-information" => Rating::Incomplete,
            "bad-citation" | "citation" | "unsupported-citation" => Rating::BadCitation,
            "slow" | "too-slow" => Rating::Slow,
            "verbose" | "too-verbose" => Rating::Verbose,
            "unsatisfactory" => Rating::Unsatisfactory,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Rating::Helpful => "helpful",
            Rating::NotHelpful => "not-helpful",
            Rating::Inaccurate => "inaccurate",
            Rating::Incomplete => "incomplete",
            Rating::BadCitation => "bad-citation",
            Rating::Slow => "slow",
            Rating::Verbose => "verbose",
            Rating::Unsatisfactory => "unsatisfactory",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Feedback {
    pub id: String,
    pub consultation_id: String,
    pub rating: Rating,
    pub note: String,
    pub created_ms: u64,
}

impl Feedback {
    pub fn new(consultation_id: String, rating: Rating, note: &str) -> Self {
        Self {
            id: String::new(),
            consultation_id,
            rating,
            note: redact_secrets(&note.chars().take(2000).collect::<String>()),
            created_ms: now_ms(),
        }
    }
}

/// One JSON file of feedback entries, owner-only, rewritten atomically.
#[derive(Debug, Clone)]
pub struct FeedbackStore {
    path: PathBuf,
}

impl FeedbackStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn list(&self) -> Result<Vec<Feedback>> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| {
                OrchestraError::Config(format!(
                    "the feedback file {} is damaged ({e})",
                    self.path.display()
                ))
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e.into()),
        }
    }

    fn write(&self, all: &[Feedback]) -> Result<()> {
        use std::io::Write as _;
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        let text =
            serde_json::to_string_pretty(all).map_err(|e| OrchestraError::Config(e.to_string()))?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    /// Stores one entry and returns its id.
    pub fn add(&self, mut entry: Feedback) -> Result<String> {
        if entry.consultation_id.trim().is_empty() {
            return Err(OrchestraError::Config(
                "feedback needs a consultation id".into(),
            ));
        }
        let mut all = self.list()?;
        entry.id = format!("fb-{}", &new_debate_id()[16..]);
        let id = entry.id.clone();
        all.push(entry);
        self.write(&all)?;
        Ok(id)
    }

    pub fn for_consultation(&self, consultation_id: &str) -> Result<Vec<Feedback>> {
        Ok(self
            .list()?
            .into_iter()
            .filter(|f| f.consultation_id.starts_with(consultation_id))
            .collect())
    }

    /// Deletes one entry by id (or unique prefix); returns whether one was removed.
    pub fn delete(&self, id: &str) -> Result<bool> {
        let all = self.list()?;
        let matching: Vec<&Feedback> = all.iter().filter(|f| f.id.starts_with(id)).collect();
        if matching.len() > 1 {
            return Err(OrchestraError::Config(format!(
                "`{id}` matches {} entries; use more characters",
                matching.len()
            )));
        }
        let before = all.len();
        let kept: Vec<Feedback> = all
            .iter()
            .filter(|f| !f.id.starts_with(id))
            .cloned()
            .collect();
        if kept.len() == before {
            return Ok(false);
        }
        self.write(&kept)?;
        Ok(true)
    }

    pub fn clear(&self) -> Result<usize> {
        let n = self.list()?.len();
        if n > 0 {
            self.write(&[])?;
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feedback_is_stored_listed_and_deleted_and_never_keeps_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let store = FeedbackStore::new(dir.path().join("sub").join("feedback.json"));
        assert!(store.list().unwrap().is_empty());
        let id = store
            .add(Feedback::new("20261012-000000-000001".into(), Rating::BadCitation, "the [2] link says nothing about this; key sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789"))
            .unwrap();
        let id2 = store
            .add(Feedback::new(
                "20261012-000000-000002".into(),
                Rating::Helpful,
                "",
            ))
            .unwrap();
        assert_ne!(id, id2);
        let all = store.list().unwrap();
        assert_eq!(all.len(), 2);
        assert!(
            !all[0].note.contains("abcdefghijklmnopqrstuvwxyz"),
            "{}",
            all[0].note
        );
        assert_eq!(
            store
                .for_consultation("20261012-000000-000001")
                .unwrap()
                .len(),
            1
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(store.path())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o077,
                0
            );
        }
        assert!(store.delete(&id).unwrap());
        assert!(!store.delete(&id).unwrap());
        assert_eq!(store.list().unwrap().len(), 1);
        assert_eq!(store.clear().unwrap(), 1);
        assert!(
            store
                .add(Feedback::new(" ".into(), Rating::Slow, ""))
                .is_err()
        );
    }

    #[test]
    fn ratings_parse_their_names_and_nothing_else() {
        for name in Rating::NAMES {
            assert_eq!(Rating::parse(name).unwrap().name(), name);
        }
        assert!(Rating::parse("great").is_none());
    }
}
