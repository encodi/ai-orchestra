//! In-memory conversation log: prompts, responses, decisions and errors with
//! agent identity and timestamps. Everything stored passes through
//! [`redact_secrets`] first.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::types::ProviderId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct ConversationId(pub u64);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", content = "id", rename_all = "lowercase")]
pub enum Author {
    User,
    System,
    Agent(ProviderId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    Prompt,
    Response,
    /// A conclusion reached by the orchestrator (used by collaboration flows).
    Decision,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub seq: u64,
    pub conversation: ConversationId,
    /// Milliseconds since the Unix epoch.
    pub timestamp_ms: u64,
    pub author: Author,
    pub kind: EntryKind,
    pub content: String,
}

#[derive(Debug)]
struct Inner {
    entries: VecDeque<Entry>,
    next_seq: u64,
    next_conversation: u64,
}

/// Thread-safe, bounded log. The oldest entries are dropped once `capacity`
/// is exceeded so a long session cannot grow without limit.
#[derive(Debug)]
pub struct ConversationStore {
    inner: Mutex<Inner>,
    capacity: usize,
}

impl ConversationStore {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                entries: VecDeque::new(),
                next_seq: 1,
                next_conversation: 1,
            }),
            capacity: capacity.max(1),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // A panic elsewhere must not make the log unusable.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn new_conversation(&self) -> ConversationId {
        let mut inner = self.lock();
        let id = ConversationId(inner.next_conversation);
        inner.next_conversation += 1;
        id
    }

    pub fn record(
        &self,
        conversation: ConversationId,
        author: Author,
        kind: EntryKind,
        content: &str,
    ) {
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or(0);
        let mut inner = self.lock();
        let seq = inner.next_seq;
        inner.next_seq += 1;
        inner.entries.push_back(Entry {
            seq,
            conversation,
            timestamp_ms,
            author,
            kind,
            content: redact_secrets(content),
        });
        while inner.entries.len() > self.capacity {
            inner.entries.pop_front();
        }
    }

    pub fn entries(&self, conversation: ConversationId) -> Vec<Entry> {
        self.lock()
            .entries
            .iter()
            .filter(|e| e.conversation == conversation)
            .cloned()
            .collect()
    }

    pub fn len(&self) -> usize {
        self.lock().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Masks things that look like credentials. Best effort: it exists so a
/// leaked token in an error message or echoed prompt does not get retained,
/// not as a security boundary.
pub fn redact_secrets(input: &str) -> String {
    const PREFIXES: &[&str] = &[
        "sk-",
        "ghp_",
        "gho_",
        "github_pat_",
        "xoxb-",
        "xoxp-",
        "AIza",
    ];
    const SENSITIVE_KEYS: &[&str] = &["KEY", "TOKEN", "SECRET", "PASSWORD", "PASSWD", "CREDENTIAL"];

    let mut out = String::with_capacity(input.len());
    let mut redact_next = false;
    for piece in input.split_inclusive(char::is_whitespace) {
        let word = piece.trim_end_matches(char::is_whitespace);
        let tail = &piece[word.len()..];
        let core = word.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_');

        let masked = (redact_next && !word.is_empty())
            || (PREFIXES.iter().any(|p| core.starts_with(p)) && core.len() >= 16)
            || (core.starts_with("eyJ") && core.len() >= 40);
        redact_next = word.eq_ignore_ascii_case("bearer");

        if masked {
            out.push_str("[REDACTED]");
            out.push_str(tail);
        } else if let Some((key, value)) = word.split_once('=')
            && !value.is_empty()
            && key.len() <= 64
            && key
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            && SENSITIVE_KEYS.iter().any(|k| key.contains(k))
        {
            out.push_str(key);
            out.push_str("=[REDACTED]");
            out.push_str(tail);
        } else {
            out.push_str(piece);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_in_order_with_identity_and_timestamps() {
        let store = ConversationStore::new(100);
        let c = store.new_conversation();
        store.record(c, Author::User, EntryKind::Prompt, "hello");
        store.record(
            c,
            Author::Agent(ProviderId::new("claude")),
            EntryKind::Response,
            "hi",
        );
        let e = store.entries(c);
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].seq + 1, e[1].seq);
        assert_eq!(e[1].author, Author::Agent(ProviderId::new("claude")));
        assert!(e[0].timestamp_ms > 0);
    }

    #[test]
    fn conversations_are_separate_and_capacity_is_enforced() {
        let store = ConversationStore::new(3);
        let a = store.new_conversation();
        let b = store.new_conversation();
        assert_ne!(a, b);
        for i in 0..5 {
            store.record(a, Author::System, EntryKind::Decision, &format!("m{i}"));
        }
        store.record(b, Author::System, EntryKind::Error, "boom");
        assert_eq!(store.len(), 3);
        assert_eq!(store.entries(b).len(), 1);
        assert_eq!(store.entries(a)[0].content, "m3");
    }

    #[test]
    fn redacts_common_credential_shapes() {
        let r = redact_secrets("key sk-abcdefghijklmnopqrstuvwx used");
        assert_eq!(r, "key [REDACTED] used");
        let r = redact_secrets("Authorization: Bearer abc123def456 end");
        assert!(!r.contains("abc123def456"), "{r}");
        let r = redact_secrets("ANTHROPIC_API_KEY=hunter2 and PATH=/bin");
        assert_eq!(r, "ANTHROPIC_API_KEY=[REDACTED] and PATH=/bin");
        let jwt = format!("eyJ{}", "a".repeat(60));
        assert!(!redact_secrets(&format!("t {jwt}")).contains(&jwt));
    }

    #[test]
    fn leaves_ordinary_text_untouched() {
        let text = "Explain ownership in Rust.\nA task-list: sk is short, key=value ok.\n";
        assert_eq!(redact_secrets(text), text);
    }

    #[test]
    fn store_applies_redaction() {
        let store = ConversationStore::new(10);
        let c = store.new_conversation();
        store.record(
            c,
            Author::User,
            EntryKind::Prompt,
            "use OPENAI_API_KEY=abc123",
        );
        assert_eq!(store.entries(c)[0].content, "use OPENAI_API_KEY=[REDACTED]");
    }
}
