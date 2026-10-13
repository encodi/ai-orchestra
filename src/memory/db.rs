//! The SQLite store behind memory: schema, migrations, and every query.
//!
//! One file, `memory.db`, created owner-only. It holds the memory records and an FTS5 index over
//! them (kept in step by triggers), sessions, the consultations that fed them, evidence references,
//! links between records (supersedes, contradicts, derived from), structured summaries and the
//! context metrics. Writes are transactions; readers do not block writers (WAL); a damaged file is
//! reported and set aside on request, never deleted.
//!
//! This layer is synchronous. [`super::service::MemoryService`] runs it on a bounded pool of
//! blocking threads so a Tokio worker is never held by a database call.
//!
//! Deleting is real: `secure_delete` overwrites the freed content, the index entry is removed in the
//! same transaction, and the write-ahead log is checkpointed afterwards. A copy of the database file
//! that the user made is, of course, not touched. Nothing is encrypted at rest.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter, types::Value};
use serde::{Deserialize, Serialize};

use super::model::{
    Authorization, EpistemicStatus, Lifecycle, LinkKind, MemoryKind, MemoryRecord, MemoryScope,
    NewMemory, Origin, SessionRow, content_hash,
};
use crate::error::{OrchestraError, Result};

pub const SCHEMA_VERSION: i64 = 2;
/// Longest text one record may hold.
pub const MAX_TEXT_BYTES: usize = 16 * 1024;
/// Longest saved final answer kept with a consultation.
pub const MAX_ANSWER_BYTES: usize = 64 * 1024;

const V1: &str = r#"
CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE projects(id TEXT PRIMARY KEY, path TEXT, name TEXT, created_ms INTEGER NOT NULL);
CREATE TABLE sessions(
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL,
  title TEXT,
  created_ms INTEGER NOT NULL,
  last_used_ms INTEGER NOT NULL,
  turns INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX sessions_project ON sessions(project_id, last_used_ms DESC);
CREATE TABLE memories(
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  scope TEXT NOT NULL,
  session_id TEXT,
  project_id TEXT,
  origin TEXT NOT NULL,
  status TEXT NOT NULL,
  lifecycle TEXT NOT NULL DEFAULT 'active',
  text TEXT NOT NULL,
  short TEXT,
  topic TEXT,
  tags TEXT NOT NULL DEFAULT '',
  authorization TEXT NOT NULL,
  sensitive INTEGER NOT NULL DEFAULT 0,
  consultation_id TEXT,
  content_hash TEXT NOT NULL,
  created_ms INTEGER NOT NULL,
  updated_ms INTEGER NOT NULL,
  retrieved_ms INTEGER,
  published TEXT,
  valid_days INTEGER,
  category TEXT,
  time_sensitive INTEGER NOT NULL DEFAULT 0,
  source_url TEXT,
  expires_ms INTEGER
);
CREATE INDEX memories_place ON memories(scope, project_id, session_id, lifecycle);
CREATE INDEX memories_hash ON memories(content_hash, lifecycle);
CREATE INDEX memories_topic ON memories(topic, scope, project_id);
CREATE INDEX memories_consultation ON memories(consultation_id);
CREATE INDEX memories_created ON memories(session_id, created_ms DESC);
CREATE VIRTUAL TABLE memory_fts USING fts5(
  text, tags, topic,
  content='memories', content_rowid='rowid',
  tokenize='porter unicode61 remove_diacritics 2'
);
CREATE TRIGGER memories_ai AFTER INSERT ON memories BEGIN
  INSERT INTO memory_fts(rowid, text, tags, topic)
  VALUES (new.rowid, new.text, new.tags, coalesce(new.topic, ''));
END;
CREATE TRIGGER memories_ad AFTER DELETE ON memories BEGIN
  INSERT INTO memory_fts(memory_fts, rowid, text, tags, topic)
  VALUES ('delete', old.rowid, old.text, old.tags, coalesce(old.topic, ''));
END;
CREATE TRIGGER memories_au AFTER UPDATE OF text, tags, topic ON memories BEGIN
  INSERT INTO memory_fts(memory_fts, rowid, text, tags, topic)
  VALUES ('delete', old.rowid, old.text, old.tags, coalesce(old.topic, ''));
  INSERT INTO memory_fts(rowid, text, tags, topic)
  VALUES (new.rowid, new.text, new.tags, coalesce(new.topic, ''));
END;
CREATE TABLE memory_links(
  from_id TEXT NOT NULL,
  to_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  created_ms INTEGER NOT NULL,
  PRIMARY KEY(from_id, to_id, kind)
);
CREATE INDEX memory_links_to ON memory_links(to_id);
CREATE TABLE evidence_refs(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  memory_id TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
  url TEXT,
  title TEXT,
  published TEXT,
  updated TEXT,
  retrieved_ms INTEGER,
  content_hash TEXT,
  relation TEXT,
  quote TEXT
);
CREATE INDEX evidence_refs_memory ON evidence_refs(memory_id);
CREATE TABLE consultations(
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL,
  project_id TEXT NOT NULL,
  question TEXT NOT NULL,
  mode TEXT,
  strategy TEXT,
  status TEXT NOT NULL,
  calls INTEGER NOT NULL DEFAULT 0,
  created_ms INTEGER NOT NULL,
  history_id TEXT,
  final_answer TEXT
);
CREATE INDEX consultations_session ON consultations(session_id, created_ms);
CREATE TABLE tombstones(id TEXT PRIMARY KEY, kind TEXT NOT NULL, deleted_ms INTEGER NOT NULL);
"#;

const V2: &str = r#"
CREATE TABLE summaries(
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  origin TEXT NOT NULL,
  body TEXT NOT NULL,
  source_ids TEXT NOT NULL,
  created_ms INTEGER NOT NULL
);
CREATE INDEX summaries_session ON summaries(session_id, created_ms);
CREATE TABLE context_metrics(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  consultation_id TEXT,
  session_id TEXT,
  created_ms INTEGER NOT NULL,
  mode TEXT,
  baseline_chars INTEGER NOT NULL,
  context_chars INTEGER NOT NULL,
  est_tokens INTEGER NOT NULL,
  retrieved INTEGER NOT NULL,
  selected INTEGER NOT NULL,
  omitted INTEGER NOT NULL,
  retrieval_us INTEGER NOT NULL,
  assembly_us INTEGER NOT NULL,
  extra_ai_calls INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX context_metrics_created ON context_metrics(created_ms);
"#;

const MIGRATIONS: [&str; 2] = [V1, V2];

fn err(context: &str, e: rusqlite::Error) -> OrchestraError {
    use rusqlite::ErrorCode::{DatabaseCorrupt, NotADatabase};
    if let rusqlite::Error::SqliteFailure(f, _) = &e
        && matches!(f.code, DatabaseCorrupt | NotADatabase)
    {
        return OrchestraError::Memory(format!(
            "the memory database is damaged ({context}: {e}). Nothing was deleted: run `ai-orchestra memory check` to see what is wrong, or `ai-orchestra memory check --quarantine` to set the file aside and start a new, empty one"
        ));
    }
    OrchestraError::Memory(format!("{context}: {e}"))
}

/// Retries an operation that hit a busy database, for up to about two seconds. SQLite does not
/// apply its busy timeout in a few situations (switching to WAL mode, upgrading a read to a write),
/// and opening a database that other connections are using must still succeed.
fn retry_busy<T>(mut op: impl FnMut() -> rusqlite::Result<T>) -> rusqlite::Result<T> {
    use rusqlite::ErrorCode::{DatabaseBusy, DatabaseLocked};
    let mut attempt = 0;
    loop {
        match op() {
            Err(rusqlite::Error::SqliteFailure(f, _))
                if matches!(f.code, DatabaseBusy | DatabaseLocked) && attempt < 100 =>
            {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(5 + (attempt % 7) * 4));
            }
            other => return other,
        }
    }
}

pub fn new_id(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let ms = crate::history::now_ms();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mix =
        (u64::from(std::process::id()) ^ (ms.rotate_left(13))) as u32 ^ n.wrapping_mul(2654435761);
    format!("{prefix}-{:04x}-{}", mix & 0xffff, to_base36(ms))
}

fn to_base36(mut n: u64) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if n == 0 {
        return "0".to_string();
    }
    let mut out = Vec::new();
    while n > 0 {
        out.push(DIGITS[(n % 36) as usize]);
        n /= 36;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// Which records a query may see. Every clause also checks the project, so a record of one project
/// is never returned for another.
#[derive(Debug, Clone, Default)]
pub struct Visibility {
    pub project: Option<String>,
    pub session: Option<String>,
    pub session_scope: bool,
    pub project_scope: bool,
    pub global_scope: bool,
    /// Session records of *any* session of the project (for the user's own listing and search).
    pub any_session: bool,
    pub include_superseded: bool,
    /// Records marked sensitive (never for context sent to an agent).
    pub include_sensitive: bool,
}

impl Visibility {
    fn clause(&self, args: &mut Vec<Value>) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.session_scope {
            if self.any_session {
                if let Some(p) = &self.project {
                    args.push(Value::Text(p.clone()));
                    parts.push(format!(
                        "(m.scope = 'session' AND m.project_id = ?{})",
                        args.len()
                    ));
                }
            } else if let (Some(s), Some(p)) = (&self.session, &self.project) {
                args.push(Value::Text(s.clone()));
                let a = args.len();
                args.push(Value::Text(p.clone()));
                parts.push(format!(
                    "(m.scope = 'session' AND m.session_id = ?{a} AND m.project_id = ?{})",
                    args.len()
                ));
            }
        }
        if self.project_scope
            && let Some(p) = &self.project
        {
            args.push(Value::Text(p.clone()));
            parts.push(format!(
                "(m.scope = 'project' AND m.project_id = ?{})",
                args.len()
            ));
        }
        if self.global_scope {
            parts.push("(m.scope = 'global')".to_string());
        }
        let mut sql = if parts.is_empty() {
            "0".to_string()
        } else {
            format!("({})", parts.join(" OR "))
        };
        if !self.include_superseded {
            sql.push_str(" AND m.lifecycle = 'active'");
        }
        if !self.include_sensitive {
            sql.push_str(" AND m.sensitive = 0");
        }
        // An expired record is gone for every reader at once; `prune` only reclaims the space.
        sql.push_str(" AND (m.expires_ms IS NULL OR m.expires_ms > CAST(strftime('%s','now') AS INTEGER) * 1000)");
        sql
    }
}

const COLS: &str = "m.id, m.kind, m.scope, m.session_id, m.project_id, m.origin, m.status, m.lifecycle, m.text, m.short, m.topic, m.tags, m.authorization, m.sensitive, m.consultation_id, m.content_hash, m.created_ms, m.updated_ms, m.retrieved_ms, m.published, m.valid_days, m.category, m.time_sensitive, m.source_url, m.expires_ms";

fn kind_of(s: &str) -> MemoryKind {
    s.parse().unwrap_or(MemoryKind::Note)
}

fn scope_of(s: &str) -> MemoryScope {
    s.parse().unwrap_or(MemoryScope::Session)
}

fn row_to_record(r: &Row<'_>) -> rusqlite::Result<MemoryRecord> {
    let tags: String = r.get(11)?;
    Ok(MemoryRecord {
        id: r.get(0)?,
        kind: kind_of(&r.get::<_, String>(1)?),
        scope: scope_of(&r.get::<_, String>(2)?),
        session_id: r.get(3)?,
        project_id: r.get(4)?,
        origin: Origin::parse(&r.get::<_, String>(5)?),
        status: EpistemicStatus::parse(&r.get::<_, String>(6)?),
        lifecycle: if r.get::<_, String>(7)? == "superseded" {
            Lifecycle::Superseded
        } else {
            Lifecycle::Active
        },
        text: r.get(8)?,
        short: r.get(9)?,
        topic: r.get(10)?,
        tags: tags.split_whitespace().map(str::to_string).collect(),
        authorization: Authorization::parse(&r.get::<_, String>(12)?),
        sensitive: r.get::<_, i64>(13)? != 0,
        consultation_id: r.get(14)?,
        content_hash: r.get(15)?,
        created_ms: r.get::<_, i64>(16)? as u64,
        updated_ms: r.get::<_, i64>(17)? as u64,
        retrieved_ms: r.get::<_, Option<i64>>(18)?.map(|v| v as u64),
        published: r.get(19)?,
        valid_days: r.get::<_, Option<i64>>(20)?.map(|v| v as u32),
        category: r.get(21)?,
        time_sensitive: r.get::<_, i64>(22)? != 0,
        source_url: r.get(23)?,
        expires_ms: r.get::<_, Option<i64>>(24)?.map(|v| v as u64),
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRef {
    pub url: Option<String>,
    pub title: Option<String>,
    pub published: Option<String>,
    pub updated: Option<String>,
    pub retrieved_ms: Option<u64>,
    pub content_hash: Option<String>,
    pub relation: Option<String>,
    pub quote: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsultationRow {
    pub id: String,
    pub session_id: String,
    pub project_id: String,
    pub question: String,
    pub mode: Option<String>,
    pub strategy: Option<String>,
    pub status: String,
    pub calls: u32,
    pub created_ms: u64,
    pub history_id: Option<String>,
    pub final_answer: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkRow {
    pub from_id: String,
    pub to_id: String,
    pub kind: String,
    pub created_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SummaryRow {
    pub id: String,
    pub session_id: String,
    pub kind: String,
    /// `deterministic`, or `ai:<agent>` for a summary an agent wrote on request.
    pub origin: String,
    pub body: String,
    /// The consultation ids it condenses. The originals are never removed.
    pub source_ids: Vec<String>,
    pub created_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricsRow {
    pub consultation_id: Option<String>,
    pub session_id: Option<String>,
    pub created_ms: u64,
    pub mode: Option<String>,
    pub baseline_chars: u64,
    pub context_chars: u64,
    pub est_tokens: u64,
    pub retrieved: u32,
    pub selected: u32,
    pub omitted: u32,
    pub retrieval_us: u64,
    pub assembly_us: u64,
    pub extra_ai_calls: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InsertOutcome {
    pub id: String,
    /// The same content already existed in the same place; nothing was added.
    pub duplicate: bool,
    /// Records this one replaced (a newer statement on the same topic).
    pub superseded: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DbStats {
    pub schema_version: i64,
    pub file_bytes: u64,
    pub memories: u64,
    pub by_scope: Vec<(String, u64)>,
    pub by_kind: Vec<(String, u64)>,
    pub superseded: u64,
    pub sessions: u64,
    pub consultations: u64,
    pub summaries: u64,
    pub metrics_rows: u64,
    pub avg_baseline_chars: f64,
    pub avg_context_chars: f64,
    pub avg_retrieval_us: f64,
    pub avg_assembly_us: f64,
    pub extra_ai_calls: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportBundle {
    pub format: String,
    pub schema_version: i64,
    pub exported_ms: u64,
    pub project: Option<String>,
    pub memories: Vec<MemoryRecord>,
    pub links: Vec<LinkRow>,
    pub sessions: Vec<SessionRow>,
    pub consultations: Vec<ConsultationRow>,
    pub summaries: Vec<SummaryRow>,
    pub evidence: Vec<(String, EvidenceRef)>,
}

pub struct MemoryDb {
    conn: Connection,
    path: PathBuf,
    /// How many `atomically` calls are open (the outermost is a transaction, the rest savepoints).
    depth: std::cell::Cell<u32>,
}

impl std::fmt::Debug for MemoryDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryDb")
            .field("path", &self.path)
            .finish()
    }
}

fn create_private_file(path: &Path) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| OrchestraError::Memory(format!("cannot create {}: {e}", dir.display())))?;
    }
    if !path.exists() {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        match opts.open(path) {
            Ok(_) => {}
            // Another process created it first: fine.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => {
                return Err(OrchestraError::Memory(format!(
                    "cannot create {}: {e}",
                    path.display()
                )));
            }
        }
    }
    Ok(())
}

impl MemoryDb {
    /// Opens the database, creating and migrating it if needed.
    pub fn open(path: &Path) -> Result<Self> {
        create_private_file(path)?;
        let conn = Connection::open(path).map_err(|e| err("cannot open the database", e))?;
        conn.busy_timeout(Duration::from_secs(10))
            .map_err(|e| err("cannot set the busy timeout", e))?;
        // WAL lets readers run while one writer writes; secure_delete overwrites freed content.
        // Several processes (or threads) may open a brand-new file at once, and switching a file to
        // WAL mode does not wait on the busy timeout: retry for a short while instead.
        retry_busy(|| {
            let mode: String = conn.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
            if !mode.eq_ignore_ascii_case("wal") {
                conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))?;
            }
            conn.execute_batch(
                "PRAGMA synchronous = NORMAL; PRAGMA secure_delete = ON; PRAGMA foreign_keys = ON;",
            )
        })
        .map_err(|e| err("cannot configure the database", e))?;
        let mut db = Self {
            conn,
            path: path.to_path_buf(),
            depth: std::cell::Cell::new(0),
        };
        db.migrate(SCHEMA_VERSION)?;
        Ok(db)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn schema_version(&self) -> Result<i64> {
        self.conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(|e| err("cannot read the schema version", e))
    }

    /// Applies the migrations up to `target`, each in its own transaction.
    pub(crate) fn migrate(&mut self, target: i64) -> Result<()> {
        let current = self.schema_version()?;
        if current > SCHEMA_VERSION {
            return Err(OrchestraError::Memory(format!(
                "the memory database has schema version {current}, newer than this program understands ({SCHEMA_VERSION}); update ai-orchestra, or move the file aside"
            )));
        }
        for version in (current + 1)..=target.min(SCHEMA_VERSION) {
            let sql = MIGRATIONS[(version - 1) as usize];
            let tx = retry_busy(|| {
                rusqlite::Transaction::new_unchecked(
                    &self.conn,
                    rusqlite::TransactionBehavior::Immediate,
                )
            })
            .map_err(|e| err("cannot start a migration", e))?;
            // Another process may have migrated while this one waited for the write lock.
            let now_at: i64 = tx
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .map_err(|e| err("cannot read the schema version", e))?;
            if now_at >= version {
                continue;
            }
            tx.execute_batch(sql)
                .map_err(|e| err(&format!("migration to version {version} failed"), e))?;
            tx.execute_batch(&format!("PRAGMA user_version = {version}"))
                .map_err(|e| err("cannot record the schema version", e))?;
            if version == 1 {
                tx.execute(
                    "INSERT OR IGNORE INTO meta(key, value) VALUES ('created_by', ?1)",
                    params![format!("ai-orchestra {}", env!("CARGO_PKG_VERSION"))],
                )
                .map_err(|e| err("cannot write the metadata", e))?;
            }
            tx.commit()
                .map_err(|e| err("cannot commit a migration", e))?;
        }
        Ok(())
    }

    /// `PRAGMA integrity_check` plus a check of the full-text index. Empty means healthy.
    pub fn check(&self) -> Result<Vec<String>> {
        let mut problems = Vec::new();
        let mut stmt = self
            .conn
            .prepare("PRAGMA integrity_check")
            .map_err(|e| err("integrity check", e))?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| err("integrity check", e))?;
        for row in rows {
            let line = row.map_err(|e| err("integrity check", e))?;
            if line != "ok" {
                problems.push(line);
            }
        }
        if self
            .conn
            .execute(
                "INSERT INTO memory_fts(memory_fts, rank) VALUES ('integrity-check', 1)",
                [],
            )
            .is_err()
        {
            problems.push(
                "the full-text index does not match the records (run `memory check --rebuild`)"
                    .to_string(),
            );
        }
        Ok(problems)
    }

    /// Rebuilds the full-text index from the records.
    pub fn rebuild_index(&self) -> Result<()> {
        self.conn
            .execute("INSERT INTO memory_fts(memory_fts) VALUES ('rebuild')", [])
            .map(|_| ())
            .map_err(|e| err("cannot rebuild the index", e))
    }

    /// Runs `f` all or nothing. The outermost call is a transaction (taking the write lock at once);
    /// calls inside it are savepoints, so a multi-step write such as recording a whole consultation
    /// either happens completely or not at all.
    pub fn atomically<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        let depth = self.depth.get();
        let (begin, commit, rollback) = if depth == 0 {
            (
                "BEGIN IMMEDIATE".to_string(),
                "COMMIT".to_string(),
                "ROLLBACK".to_string(),
            )
        } else {
            (
                format!("SAVEPOINT sp{depth}"),
                format!("RELEASE sp{depth}"),
                format!("ROLLBACK TO sp{depth}; RELEASE sp{depth}"),
            )
        };
        self.conn
            .execute_batch(&begin)
            .map_err(|e| err("cannot start a write", e))?;
        self.depth.set(depth + 1);
        let result = f();
        self.depth.set(depth);
        match result {
            Ok(v) => {
                self.conn
                    .execute_batch(&commit)
                    .map_err(|e| err("cannot commit a write", e))?;
                Ok(v)
            }
            Err(e) => {
                let _ = self.conn.execute_batch(&rollback);
                Err(e)
            }
        }
    }

    // ------------------------------------------------------------------ projects and sessions

    pub fn upsert_project(
        &self,
        id: &str,
        path: Option<&str>,
        name: Option<&str>,
        now: u64,
    ) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO projects(id, path, name, created_ms) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(id) DO UPDATE SET path = coalesce(excluded.path, path), name = coalesce(excluded.name, name)",
                params![id, path, name, now as i64],
            )
            .map(|_| ())
            .map_err(|e| err("cannot save the project", e))
    }

    pub fn create_session(
        &self,
        project: &str,
        title: Option<&str>,
        now: u64,
    ) -> Result<SessionRow> {
        let id = new_id("ses");
        self.conn
            .execute(
                "INSERT INTO sessions(id, project_id, title, created_ms, last_used_ms, turns) VALUES (?1, ?2, ?3, ?4, ?4, 0)",
                params![id, project, title, now as i64],
            )
            .map_err(|e| err("cannot create the session", e))?;
        Ok(SessionRow {
            id,
            project_id: project.to_string(),
            title: title.map(str::to_string),
            created_ms: now,
            last_used_ms: now,
            turns: 0,
        })
    }

    fn session_from(r: &Row<'_>) -> rusqlite::Result<SessionRow> {
        Ok(SessionRow {
            id: r.get(0)?,
            project_id: r.get(1)?,
            title: r.get(2)?,
            created_ms: r.get::<_, i64>(3)? as u64,
            last_used_ms: r.get::<_, i64>(4)? as u64,
            turns: r.get::<_, i64>(5)? as u32,
        })
    }

    /// A session by id or unique id prefix.
    pub fn get_session(&self, id_or_prefix: &str) -> Result<Option<SessionRow>> {
        let like = format!("{}%", escape_like(id_or_prefix));
        let mut stmt = self
            .conn
            .prepare("SELECT id, project_id, title, created_ms, last_used_ms, turns FROM sessions WHERE id = ?1 OR id LIKE ?2 ESCAPE '\\' ORDER BY id LIMIT 3")
            .map_err(|e| err("cannot read sessions", e))?;
        let rows: Vec<SessionRow> = stmt
            .query_map(params![id_or_prefix, like], Self::session_from)
            .map_err(|e| err("cannot read sessions", e))?
            .filter_map(std::result::Result::ok)
            .collect();
        if let Some(exact) = rows.iter().find(|s| s.id == id_or_prefix) {
            return Ok(Some(exact.clone()));
        }
        match rows.len() {
            0 => Ok(None),
            1 => Ok(rows.into_iter().next()),
            _ => Err(OrchestraError::Config(format!(
                "`{id_or_prefix}` matches more than one session; use a longer prefix"
            ))),
        }
    }

    pub fn list_sessions(&self, project: Option<&str>, limit: usize) -> Result<Vec<SessionRow>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, project_id, title, created_ms, last_used_ms, turns FROM sessions WHERE (?1 IS NULL OR project_id = ?1) ORDER BY last_used_ms DESC, id LIMIT ?2")
            .map_err(|e| err("cannot list sessions", e))?;
        let rows = stmt
            .query_map(params![project, limit as i64], Self::session_from)
            .map_err(|e| err("cannot list sessions", e))?
            .filter_map(std::result::Result::ok)
            .collect();
        Ok(rows)
    }

    /// The most recently used session of a project, if it was used at or after `not_before_ms`.
    pub fn latest_session(&self, project: &str, not_before_ms: u64) -> Result<Option<SessionRow>> {
        self.conn
            .query_row(
                "SELECT id, project_id, title, created_ms, last_used_ms, turns FROM sessions WHERE project_id = ?1 AND last_used_ms >= ?2 ORDER BY last_used_ms DESC LIMIT 1",
                params![project, not_before_ms as i64],
                Self::session_from,
            )
            .optional()
            .map_err(|e| err("cannot read sessions", e))
    }

    pub fn touch_session(
        &self,
        id: &str,
        now: u64,
        add_turn: bool,
        title: Option<&str>,
    ) -> Result<()> {
        self.conn
            .execute(
                "UPDATE sessions SET last_used_ms = ?2, turns = turns + ?3, title = coalesce(title, ?4) WHERE id = ?1",
                params![id, now as i64, i64::from(add_turn), title],
            )
            .map(|_| ())
            .map_err(|e| err("cannot update the session", e))
    }

    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        self.conn
            .query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| {
                r.get(0)
            })
            .optional()
            .map_err(|e| err("cannot read the metadata", e))
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO meta(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .map(|_| ())
            .map_err(|e| err("cannot write the metadata", e))
    }

    // ------------------------------------------------------------------------------ memories

    /// Stores a record (see `insert_memory_inner`), all or nothing.
    pub fn insert_memory(&mut self, m: &NewMemory, now: u64) -> Result<InsertOutcome> {
        self.atomically(|| self.insert_memory_inner(m, now))
    }

    /// Stores a record in one transaction. The same content in the same place is not stored twice,
    /// and a newer statement of the user's intent on the same topic supersedes the older one.
    pub(crate) fn insert_memory_inner(&self, m: &NewMemory, now: u64) -> Result<InsertOutcome> {
        if m.text.trim().is_empty() {
            return Err(OrchestraError::Config("a memory cannot be empty".into()));
        }
        if m.text.len() > MAX_TEXT_BYTES {
            return Err(OrchestraError::Config(format!(
                "a memory is limited to {MAX_TEXT_BYTES} bytes; this one has {}",
                m.text.len()
            )));
        }
        // The same claim with a different verification status is a different record.
        let hashed_text = if m.kind == MemoryKind::Evidence {
            format!("{} [{}]", m.text, m.status.as_str())
        } else {
            m.text.clone()
        };
        let hash = content_hash(
            m.kind,
            m.scope,
            m.project_id.as_deref(),
            m.session_id.as_deref(),
            &hashed_text,
        );
        let tx = &self.conn;
        let existing: Option<String> = tx
            .query_row(
                "SELECT id FROM memories WHERE content_hash = ?1 AND lifecycle = 'active' LIMIT 1",
                params![hash],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| err("cannot look for duplicates", e))?;
        if let Some(id) = existing {
            return Ok(InsertOutcome {
                id,
                duplicate: true,
                superseded: Vec::new(),
            });
        }
        let id = new_id("mem");
        let mut superseded = Vec::new();
        let replaces_older = (m.kind.is_user_intent() && matches!(m.origin, Origin::User))
            || m.kind == MemoryKind::Evidence;
        if replaces_older && let Some(topic) = &m.topic {
            let kinds = if m.kind == MemoryKind::Evidence {
                "('evidence')"
            } else {
                "('preference','requirement','decision')"
            };
            let origin_filter = if m.kind == MemoryKind::Evidence {
                ""
            } else {
                " AND origin = 'user'"
            };
            let mut stmt = tx
                .prepare(&format!("SELECT id FROM memories WHERE topic = ?1 AND scope = ?2 AND coalesce(project_id,'') = ?3 AND coalesce(session_id,'') = ?4 AND lifecycle = 'active'{origin_filter} AND kind IN {kinds}"))
                .map_err(|e| err("cannot look for an older statement", e))?;
            let older: Vec<String> = stmt
                .query_map(
                    params![
                        topic,
                        m.scope.as_str(),
                        m.project_id.as_deref().unwrap_or(""),
                        m.session_id.as_deref().unwrap_or("")
                    ],
                    |r| r.get(0),
                )
                .map_err(|e| err("cannot look for an older statement", e))?
                .filter_map(std::result::Result::ok)
                .collect();
            drop(stmt);
            superseded = older;
        }
        tx.execute(
            "INSERT INTO memories(id, kind, scope, session_id, project_id, origin, status, lifecycle, text, short, topic, tags, authorization, sensitive, consultation_id, content_hash, created_ms, updated_ms, retrieved_ms, published, valid_days, category, time_sensitive, source_url, expires_ms) VALUES (?1,?2,?3,?4,?5,?6,?7,'active',?8,?9,?10,?11,?12,?13,?14,?15,?16,?16,?17,?18,?19,?20,?21,?22,?23)",
            params![
                id,
                m.kind.as_str(),
                m.scope.as_str(),
                m.session_id,
                m.project_id,
                m.origin.as_string(),
                m.status.as_str(),
                m.text,
                m.short,
                m.topic,
                m.tags.join(" "),
                m.authorization.as_str(),
                i64::from(m.sensitive),
                m.consultation_id,
                hash,
                now as i64,
                m.retrieved_ms.map(|v| v as i64),
                m.published,
                m.valid_days.map(i64::from),
                m.category,
                i64::from(m.time_sensitive),
                m.source_url,
                m.expires_ms.map(|v| v as i64),
            ],
        )
        .map_err(|e| err("cannot store the memory", e))?;
        for old in &superseded {
            tx.execute(
                "UPDATE memories SET lifecycle = 'superseded', updated_ms = ?2 WHERE id = ?1",
                params![old, now as i64],
            )
            .map_err(|e| err("cannot supersede the older record", e))?;
            tx.execute(
                "INSERT OR IGNORE INTO memory_links(from_id, to_id, kind, created_ms) VALUES (?1, ?2, 'supersedes', ?3)",
                params![id, old, now as i64],
            )
            .map_err(|e| err("cannot link the records", e))?;
        }
        Ok(InsertOutcome {
            id,
            duplicate: false,
            superseded,
        })
    }

    /// Replaces the meaning of a record by a new version that supersedes it, all or nothing.
    pub fn correct_memory(
        &mut self,
        old_id: &str,
        new_text: &str,
        now: u64,
    ) -> Result<InsertOutcome> {
        self.atomically(|| self.correct_memory_inner(old_id, new_text, now))
    }

    /// Replaces the *meaning* of a record without rewriting it: the new text becomes a new record
    /// that supersedes the old one. Returns the new id.
    fn correct_memory_inner(
        &self,
        old_id: &str,
        new_text: &str,
        now: u64,
    ) -> Result<InsertOutcome> {
        let old = self.get_memory(old_id)?;
        if old.lifecycle == Lifecycle::Superseded {
            return Err(OrchestraError::Config(format!(
                "{} was already replaced; correct the newer record instead",
                old.id
            )));
        }
        let mut next = NewMemory::new(old.kind, old.scope, new_text);
        next.session_id = old.session_id.clone();
        next.project_id = old.project_id.clone();
        next.origin = Origin::User;
        next.status = EpistemicStatus::UserProvided;
        next.topic = old.topic.clone();
        next.tags = old.tags.clone();
        next.authorization = Authorization::ExplicitUser;
        next.consultation_id = old.consultation_id.clone();
        let outcome = self.insert_memory_inner(&next, now)?;
        if outcome.duplicate {
            return Ok(outcome);
        }
        let tx = &self.conn;
        tx.execute(
            "UPDATE memories SET lifecycle = 'superseded', updated_ms = ?2 WHERE id = ?1",
            params![old.id, now as i64],
        )
        .map_err(|e| err("cannot supersede the record", e))?;
        tx.execute(
            "INSERT OR IGNORE INTO memory_links(from_id, to_id, kind, created_ms) VALUES (?1, ?2, 'supersedes', ?3)",
            params![outcome.id, old.id, now as i64],
        )
        .map_err(|e| err("cannot link the records", e))?;
        let mut out = outcome;
        if !out.superseded.contains(&old.id) {
            out.superseded.push(old.id);
        }
        Ok(out)
    }

    /// A record by id or unique prefix, whatever its lifecycle.
    pub fn get_memory(&self, id_or_prefix: &str) -> Result<MemoryRecord> {
        let like = format!("{}%", escape_like(id_or_prefix));
        let sql = format!(
            "SELECT {COLS} FROM memories m WHERE m.id = ?1 OR m.id LIKE ?2 ESCAPE '\\' ORDER BY m.id LIMIT 3"
        );
        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(|e| err("cannot read memories", e))?;
        let rows: Vec<MemoryRecord> = stmt
            .query_map(params![id_or_prefix, like], row_to_record)
            .map_err(|e| err("cannot read memories", e))?
            .filter_map(std::result::Result::ok)
            .collect();
        if let Some(exact) = rows.iter().find(|r| r.id == id_or_prefix) {
            return Ok(exact.clone());
        }
        match rows.len() {
            0 => Err(OrchestraError::Config(format!(
                "no memory `{id_or_prefix}` (see `ai-orchestra memory list`)"
            ))),
            1 => Ok(rows.into_iter().next().expect("one row")),
            _ => Err(OrchestraError::Config(format!(
                "`{id_or_prefix}` matches more than one memory; use a longer prefix"
            ))),
        }
    }

    pub fn list_memories(
        &self,
        vis: &Visibility,
        kinds: Option<&[MemoryKind]>,
        limit: usize,
    ) -> Result<Vec<MemoryRecord>> {
        let mut args: Vec<Value> = Vec::new();
        let mut sql = format!(
            "SELECT {COLS} FROM memories m WHERE {}",
            vis.clause(&mut args)
        );
        if let Some(kinds) = kinds {
            let list = kinds
                .iter()
                .map(|k| format!("'{}'", k.as_str()))
                .collect::<Vec<_>>()
                .join(",");
            sql.push_str(&format!(" AND m.kind IN ({list})"));
        }
        args.push(Value::Integer(limit as i64));
        sql.push_str(&format!(
            " ORDER BY m.created_ms DESC, m.id LIMIT ?{}",
            args.len()
        ));
        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(|e| err("cannot list memories", e))?;
        let rows = stmt
            .query_map(params_from_iter(args.iter()), row_to_record)
            .map_err(|e| err("cannot list memories", e))?
            .filter_map(std::result::Result::ok)
            .collect();
        Ok(rows)
    }

    /// Full-text search, best first. `match_expr` is an FTS5 expression built by the caller from
    /// quoted terms only. Returns each record with its BM25 score (lower is better in SQLite; the
    /// value returned here is negated, so higher is better).
    pub fn search(
        &self,
        match_expr: &str,
        vis: &Visibility,
        kinds: Option<&[MemoryKind]>,
        limit: usize,
    ) -> Result<Vec<(MemoryRecord, f64)>> {
        if match_expr.trim().is_empty() {
            return Ok(Vec::new());
        }
        let mut args: Vec<Value> = vec![Value::Text(match_expr.to_string())];
        let clause = vis.clause(&mut args);
        let mut sql = format!(
            "SELECT {COLS}, -bm25(memory_fts, 1.0, 0.6, 1.4) AS score FROM memory_fts JOIN memories m ON m.rowid = memory_fts.rowid WHERE memory_fts MATCH ?1 AND {clause}"
        );
        if let Some(kinds) = kinds {
            let list = kinds
                .iter()
                .map(|k| format!("'{}'", k.as_str()))
                .collect::<Vec<_>>()
                .join(",");
            sql.push_str(&format!(" AND m.kind IN ({list})"));
        }
        args.push(Value::Integer(limit as i64));
        sql.push_str(&format!(" ORDER BY score DESC, m.id LIMIT ?{}", args.len()));
        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(|e| err("cannot search memories", e))?;
        let rows = stmt
            .query_map(params_from_iter(args.iter()), |r| {
                Ok((row_to_record(r)?, r.get::<_, f64>(25)?))
            })
            .map_err(|e| err("cannot search memories", e))?
            .filter_map(std::result::Result::ok)
            .collect();
        Ok(rows)
    }

    /// The most recent records of a session (newest first), of the given kinds.
    pub fn recent_in_session(
        &self,
        session: &str,
        project: &str,
        kinds: &[MemoryKind],
        limit: usize,
    ) -> Result<Vec<MemoryRecord>> {
        let vis = Visibility {
            project: Some(project.to_string()),
            session: Some(session.to_string()),
            session_scope: true,
            ..Default::default()
        };
        self.list_memories(&vis, Some(kinds), limit)
    }

    pub fn memories_of_consultation(
        &self,
        consultation_id: &str,
        kinds: &[MemoryKind],
    ) -> Result<Vec<MemoryRecord>> {
        let list = kinds
            .iter()
            .map(|k| format!("'{}'", k.as_str()))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT {COLS} FROM memories m WHERE m.consultation_id = ?1 AND m.lifecycle = 'active' AND m.sensitive = 0 AND m.kind IN ({list}) ORDER BY m.created_ms, m.id"
        );
        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(|e| err("cannot read memories", e))?;
        let rows = stmt
            .query_map(params![consultation_id], row_to_record)
            .map_err(|e| err("cannot read memories", e))?
            .filter_map(std::result::Result::ok)
            .collect();
        Ok(rows)
    }

    /// Active records on the same topic and place as `m` that disagree with it, for conflict
    /// reporting. Only records written by the user or checked against sources are compared.
    pub fn active_with_topic(&self, topic: &str, vis: &Visibility) -> Result<Vec<MemoryRecord>> {
        let mut args: Vec<Value> = vec![Value::Text(topic.to_string())];
        let clause = vis.clause(&mut args);
        let sql = format!(
            "SELECT {COLS} FROM memories m WHERE m.topic = ?1 AND {clause} ORDER BY m.created_ms DESC, m.id"
        );
        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(|e| err("cannot read memories", e))?;
        let rows = stmt
            .query_map(params_from_iter(args.iter()), row_to_record)
            .map_err(|e| err("cannot read memories", e))?
            .filter_map(std::result::Result::ok)
            .collect();
        Ok(rows)
    }

    pub fn link(&self, from: &str, to: &str, kind: LinkKind, now: u64) -> Result<()> {
        self.conn
            .execute(
                "INSERT OR IGNORE INTO memory_links(from_id, to_id, kind, created_ms) VALUES (?1, ?2, ?3, ?4)",
                params![from, to, kind.as_str(), now as i64],
            )
            .map(|_| ())
            .map_err(|e| err("cannot link the records", e))
    }

    pub fn mark_superseded(&self, id: &str, now: u64) -> Result<()> {
        self.conn
            .execute(
                "UPDATE memories SET lifecycle = 'superseded', updated_ms = ?2 WHERE id = ?1",
                params![id, now as i64],
            )
            .map(|_| ())
            .map_err(|e| err("cannot supersede the record", e))
    }

    pub fn links_of(&self, id: &str) -> Result<Vec<LinkRow>> {
        let mut stmt = self
            .conn
            .prepare("SELECT from_id, to_id, kind, created_ms FROM memory_links WHERE from_id = ?1 OR to_id = ?1 ORDER BY created_ms, from_id, to_id")
            .map_err(|e| err("cannot read links", e))?;
        let rows = stmt
            .query_map(params![id], |r| {
                Ok(LinkRow {
                    from_id: r.get(0)?,
                    to_id: r.get(1)?,
                    kind: r.get(2)?,
                    created_ms: r.get::<_, i64>(3)? as u64,
                })
            })
            .map_err(|e| err("cannot read links", e))?
            .filter_map(std::result::Result::ok)
            .collect();
        Ok(rows)
    }

    pub fn add_evidence_refs(&self, memory_id: &str, refs: &[EvidenceRef]) -> Result<()> {
        for r in refs {
            self.conn
                .execute(
                    "INSERT INTO evidence_refs(memory_id, url, title, published, updated, retrieved_ms, content_hash, relation, quote) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                    params![
                        memory_id,
                        r.url,
                        r.title,
                        r.published,
                        r.updated,
                        r.retrieved_ms.map(|v| v as i64),
                        r.content_hash,
                        r.relation,
                        r.quote
                    ],
                )
                .map_err(|e| err("cannot store the evidence reference", e))?;
        }
        Ok(())
    }

    pub fn evidence_refs(&self, memory_id: &str) -> Result<Vec<EvidenceRef>> {
        let mut stmt = self
            .conn
            .prepare("SELECT url, title, published, updated, retrieved_ms, content_hash, relation, quote FROM evidence_refs WHERE memory_id = ?1 ORDER BY id")
            .map_err(|e| err("cannot read evidence", e))?;
        let rows = stmt
            .query_map(params![memory_id], |r| {
                Ok(EvidenceRef {
                    url: r.get(0)?,
                    title: r.get(1)?,
                    published: r.get(2)?,
                    updated: r.get(3)?,
                    retrieved_ms: r.get::<_, Option<i64>>(4)?.map(|v| v as u64),
                    content_hash: r.get(5)?,
                    relation: r.get(6)?,
                    quote: r.get(7)?,
                })
            })
            .map_err(|e| err("cannot read evidence", e))?
            .filter_map(std::result::Result::ok)
            .collect();
        Ok(rows)
    }

    /// Deletes one record for good, all or nothing, then scrubs the index and the log.
    pub fn delete_memory(&mut self, id_or_prefix: &str, now: u64) -> Result<MemoryRecord> {
        let record = self.atomically(|| self.delete_memory_inner(id_or_prefix, now))?;
        self.checkpoint();
        Ok(record)
    }

    /// Deletes one record for good, with its evidence references and links. Leaves a tombstone that
    /// holds no content (only the id, the kind and the time).
    fn delete_memory_inner(&self, id_or_prefix: &str, now: u64) -> Result<MemoryRecord> {
        let record = self.get_memory(id_or_prefix)?;
        let tx = &self.conn;
        tx.execute("DELETE FROM memories WHERE id = ?1", params![record.id])
            .map_err(|e| err("cannot delete the memory", e))?;
        tx.execute(
            "DELETE FROM memory_links WHERE from_id = ?1 OR to_id = ?1",
            params![record.id],
        )
        .map_err(|e| err("cannot delete the links", e))?;
        tx.execute(
            "INSERT OR REPLACE INTO tombstones(id, kind, deleted_ms) VALUES (?1, ?2, ?3)",
            params![record.id, record.kind.as_str(), now as i64],
        )
        .map_err(|e| err("cannot record the deletion", e))?;
        Ok(record)
    }

    /// Deletes a session with everything that belongs to it, all or nothing, then scrubs.
    pub fn clear_session(&mut self, session: &str, project: &str, now: u64) -> Result<usize> {
        let removed = self.atomically(|| self.clear_session_inner(session, project, now))?;
        self.checkpoint();
        Ok(removed)
    }

    /// Deletes a session with everything that belongs to it. Returns the memories removed.
    /// Deletes every record of a persistent scope (all of a project's, or all global ones).
    pub fn clear_scope(
        &mut self,
        scope: MemoryScope,
        project: Option<&str>,
        now: u64,
    ) -> Result<usize> {
        let removed = self.atomically(|| {
            self.conn
                .execute(
                    "INSERT OR REPLACE INTO tombstones(id, kind, deleted_ms) SELECT id, kind, ?3 FROM memories WHERE scope = ?1 AND coalesce(project_id,'') = ?2",
                    params![scope.as_str(), project.unwrap_or(""), now as i64],
                )
                .map_err(|e| err("cannot record the deletion", e))?;
            let n = self
                .conn
                .execute(
                    "DELETE FROM memories WHERE scope = ?1 AND coalesce(project_id,'') = ?2",
                    params![scope.as_str(), project.unwrap_or("")],
                )
                .map_err(|e| err("cannot delete the memories", e))?;
            self.conn
                .execute(
                    "DELETE FROM memory_links WHERE from_id NOT IN (SELECT id FROM memories) OR to_id NOT IN (SELECT id FROM memories)",
                    [],
                )
                .map_err(|e| err("cannot clean the links", e))?;
            Ok(n)
        })?;
        self.checkpoint();
        Ok(removed)
    }

    fn clear_session_inner(&self, session: &str, project: &str, now: u64) -> Result<usize> {
        let tx = &self.conn;
        tx.execute(
            "INSERT OR REPLACE INTO tombstones(id, kind, deleted_ms) SELECT id, kind, ?3 FROM memories WHERE session_id = ?1 AND project_id = ?2 AND scope = 'session'",
            params![session, project, now as i64],
        )
        .map_err(|e| err("cannot record the deletion", e))?;
        let removed = tx
            .execute(
                "DELETE FROM memories WHERE session_id = ?1 AND project_id = ?2 AND scope = 'session'",
                params![session, project],
            )
            .map_err(|e| err("cannot delete the session memories", e))?;
        tx.execute(
            "DELETE FROM consultations WHERE session_id = ?1 AND project_id = ?2",
            params![session, project],
        )
        .map_err(|e| err("cannot delete the consultations", e))?;
        tx.execute(
            "DELETE FROM summaries WHERE session_id = ?1",
            params![session],
        )
        .map_err(|e| err("cannot delete the summaries", e))?;
        tx.execute(
            "DELETE FROM context_metrics WHERE session_id = ?1",
            params![session],
        )
        .map_err(|e| err("cannot delete the metrics", e))?;
        tx.execute(
            "DELETE FROM sessions WHERE id = ?1 AND project_id = ?2",
            params![session, project],
        )
        .map_err(|e| err("cannot delete the session", e))?;
        tx.execute(
            "DELETE FROM memory_links WHERE from_id NOT IN (SELECT id FROM memories) OR to_id NOT IN (SELECT id FROM memories)",
            [],
        )
        .map_err(|e| err("cannot clean the links", e))?;
        Ok(removed)
    }

    /// Removes what has expired, all or nothing. Returns (memories, sessions) removed.
    pub fn prune(
        &mut self,
        now: u64,
        session_retention_days: Option<u32>,
    ) -> Result<(usize, usize)> {
        let out = self.atomically(|| self.prune_inner(now, session_retention_days))?;
        if out.0 + out.1 > 0 {
            self.checkpoint();
        }
        Ok(out)
    }

    /// Removes what has expired. Returns (memories, sessions) removed.
    fn prune_inner(&self, now: u64, session_retention_days: Option<u32>) -> Result<(usize, usize)> {
        let tx = &self.conn;
        let mut memories = tx
            .execute(
                "DELETE FROM memories WHERE expires_ms IS NOT NULL AND expires_ms < ?1",
                params![now as i64],
            )
            .map_err(|e| err("cannot prune", e))?;
        let mut sessions = 0;
        if let Some(days) = session_retention_days {
            let cutoff = now.saturating_sub(u64::from(days) * 86_400_000) as i64;
            let old: Vec<String> = {
                let mut stmt = tx
                    .prepare("SELECT id FROM sessions WHERE last_used_ms < ?1")
                    .map_err(|e| err("cannot prune", e))?;
                stmt.query_map(params![cutoff], |r| r.get(0))
                    .map_err(|e| err("cannot prune", e))?
                    .filter_map(std::result::Result::ok)
                    .collect()
            };
            for s in &old {
                memories += tx
                    .execute(
                        "DELETE FROM memories WHERE session_id = ?1 AND scope = 'session'",
                        params![s],
                    )
                    .map_err(|e| err("cannot prune", e))?;
                for table in ["consultations", "summaries", "context_metrics"] {
                    tx.execute(
                        &format!("DELETE FROM {table} WHERE session_id = ?1"),
                        params![s],
                    )
                    .map_err(|e| err("cannot prune", e))?;
                }
                sessions += tx
                    .execute("DELETE FROM sessions WHERE id = ?1", params![s])
                    .map_err(|e| err("cannot prune", e))?;
            }
        }
        tx.execute(
            "DELETE FROM memory_links WHERE from_id NOT IN (SELECT id FROM memories) OR to_id NOT IN (SELECT id FROM memories)",
            [],
        )
        .map_err(|e| err("cannot clean the links", e))?;
        Ok((memories, sessions))
    }

    /// After a delete: merge the full-text index (its segments keep deleted terms until merged),
    /// and fold the write-ahead log into the main file, so removed content does not linger.
    fn checkpoint(&self) {
        let _ = self
            .conn
            .execute("INSERT INTO memory_fts(memory_fts) VALUES ('optimize')", []);
        let _ = self
            .conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
    }

    // ------------------------------------------------------- consultations, summaries, metrics

    pub fn insert_consultation(&self, c: &ConsultationRow) -> Result<()> {
        let answer = c
            .final_answer
            .as_deref()
            .map(|a| clip_bytes(a, MAX_ANSWER_BYTES));
        self.conn
            .execute(
                "INSERT OR REPLACE INTO consultations(id, session_id, project_id, question, mode, strategy, status, calls, created_ms, history_id, final_answer) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                params![
                    c.id,
                    c.session_id,
                    c.project_id,
                    c.question,
                    c.mode,
                    c.strategy,
                    c.status,
                    i64::from(c.calls),
                    c.created_ms as i64,
                    c.history_id,
                    answer
                ],
            )
            .map(|_| ())
            .map_err(|e| err("cannot store the consultation", e))
    }

    fn consultation_from(r: &Row<'_>) -> rusqlite::Result<ConsultationRow> {
        Ok(ConsultationRow {
            id: r.get(0)?,
            session_id: r.get(1)?,
            project_id: r.get(2)?,
            question: r.get(3)?,
            mode: r.get(4)?,
            strategy: r.get(5)?,
            status: r.get(6)?,
            calls: r.get::<_, i64>(7)? as u32,
            created_ms: r.get::<_, i64>(8)? as u64,
            history_id: r.get(9)?,
            final_answer: r.get(10)?,
        })
    }

    pub fn get_consultation(&self, id: &str) -> Result<Option<ConsultationRow>> {
        self.conn
            .query_row(
                "SELECT id, session_id, project_id, question, mode, strategy, status, calls, created_ms, history_id, final_answer FROM consultations WHERE id = ?1",
                params![id],
                Self::consultation_from,
            )
            .optional()
            .map_err(|e| err("cannot read the consultation", e))
    }

    pub fn consultations_in_session(
        &self,
        session: &str,
        project: &str,
    ) -> Result<Vec<ConsultationRow>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, session_id, project_id, question, mode, strategy, status, calls, created_ms, history_id, final_answer FROM consultations WHERE session_id = ?1 AND project_id = ?2 ORDER BY created_ms, id")
            .map_err(|e| err("cannot read consultations", e))?;
        let rows = stmt
            .query_map(params![session, project], Self::consultation_from)
            .map_err(|e| err("cannot read consultations", e))?
            .filter_map(std::result::Result::ok)
            .collect();
        Ok(rows)
    }

    pub fn consultation_exists_for_history(&self, history_id: &str) -> Result<bool> {
        self.conn
            .query_row(
                "SELECT 1 FROM consultations WHERE history_id = ?1 LIMIT 1",
                params![history_id],
                |_| Ok(()),
            )
            .optional()
            .map(|o| o.is_some())
            .map_err(|e| err("cannot read consultations", e))
    }

    pub fn insert_summary(&self, s: &SummaryRow) -> Result<()> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO summaries(id, session_id, kind, origin, body, source_ids, created_ms) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![s.id, s.session_id, s.kind, s.origin, s.body, s.source_ids.join(","), s.created_ms as i64],
            )
            .map(|_| ())
            .map_err(|e| err("cannot store the summary", e))
    }

    pub fn summaries_for_session(&self, session: &str) -> Result<Vec<SummaryRow>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, session_id, kind, origin, body, source_ids, created_ms FROM summaries WHERE session_id = ?1 ORDER BY created_ms, id")
            .map_err(|e| err("cannot read summaries", e))?;
        let rows = stmt
            .query_map(params![session], |r| {
                let ids: String = r.get(5)?;
                Ok(SummaryRow {
                    id: r.get(0)?,
                    session_id: r.get(1)?,
                    kind: r.get(2)?,
                    origin: r.get(3)?,
                    body: r.get(4)?,
                    source_ids: ids
                        .split(',')
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .collect(),
                    created_ms: r.get::<_, i64>(6)? as u64,
                })
            })
            .map_err(|e| err("cannot read summaries", e))?
            .filter_map(std::result::Result::ok)
            .collect();
        Ok(rows)
    }

    pub fn insert_metrics(&self, m: &MetricsRow) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO context_metrics(consultation_id, session_id, created_ms, mode, baseline_chars, context_chars, est_tokens, retrieved, selected, omitted, retrieval_us, assembly_us, extra_ai_calls) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
                params![
                    m.consultation_id,
                    m.session_id,
                    m.created_ms as i64,
                    m.mode,
                    m.baseline_chars as i64,
                    m.context_chars as i64,
                    m.est_tokens as i64,
                    i64::from(m.retrieved),
                    i64::from(m.selected),
                    i64::from(m.omitted),
                    m.retrieval_us as i64,
                    m.assembly_us as i64,
                    i64::from(m.extra_ai_calls)
                ],
            )
            .map(|_| ())
            .map_err(|e| err("cannot store the metrics", e))
    }

    // ------------------------------------------------------------------- export and statistics

    pub fn stats(&self) -> Result<DbStats> {
        let count = |sql: &str| -> Result<u64> {
            self.conn
                .query_row(sql, [], |r| r.get::<_, i64>(0))
                .map(|v| v as u64)
                .map_err(|e| err("cannot count", e))
        };
        let grouped = |col: &str| -> Result<Vec<(String, u64)>> {
            let mut stmt = self
                .conn
                .prepare(&format!("SELECT {col}, count(*) FROM memories WHERE lifecycle = 'active' GROUP BY {col} ORDER BY {col}"))
                .map_err(|e| err("cannot count", e))?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64))
                })
                .map_err(|e| err("cannot count", e))?
                .filter_map(std::result::Result::ok)
                .collect();
            Ok(rows)
        };
        let (n, b, c, rt, at, extra): (i64, f64, f64, f64, f64, i64) = self
            .conn
            .query_row(
                "SELECT count(*), coalesce(avg(baseline_chars),0), coalesce(avg(context_chars),0), coalesce(avg(retrieval_us),0), coalesce(avg(assembly_us),0), coalesce(sum(extra_ai_calls),0) FROM context_metrics",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )
            .map_err(|e| err("cannot read the metrics", e))?;
        Ok(DbStats {
            schema_version: self.schema_version()?,
            file_bytes: std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0),
            memories: count("SELECT count(*) FROM memories WHERE lifecycle = 'active'")?,
            by_scope: grouped("scope")?,
            by_kind: grouped("kind")?,
            superseded: count("SELECT count(*) FROM memories WHERE lifecycle = 'superseded'")?,
            sessions: count("SELECT count(*) FROM sessions")?,
            consultations: count("SELECT count(*) FROM consultations")?,
            summaries: count("SELECT count(*) FROM summaries")?,
            metrics_rows: n as u64,
            avg_baseline_chars: b,
            avg_context_chars: c,
            avg_retrieval_us: rt,
            avg_assembly_us: at,
            extra_ai_calls: extra as u64,
        })
    }

    /// Everything stored that the user may see, in a deterministic order. Sensitive records are
    /// included: it is the user's own data.
    pub fn export(&self, project: Option<&str>, now: u64) -> Result<ExportBundle> {
        let vis = Visibility {
            project: project.map(str::to_string),
            session_scope: true,
            project_scope: true,
            global_scope: true,
            any_session: true,
            include_superseded: true,
            include_sensitive: true,
            ..Default::default()
        };
        let mut memories = self.list_memories(&vis, None, 1_000_000)?;
        memories.sort_by(|a, b| (a.created_ms, &a.id).cmp(&(b.created_ms, &b.id)));
        let ids: std::collections::HashSet<&str> = memories.iter().map(|m| m.id.as_str()).collect();
        let mut stmt = self
            .conn
            .prepare("SELECT from_id, to_id, kind, created_ms FROM memory_links ORDER BY created_ms, from_id, to_id, kind")
            .map_err(|e| err("cannot export", e))?;
        let links: Vec<LinkRow> = stmt
            .query_map([], |r| {
                Ok(LinkRow {
                    from_id: r.get(0)?,
                    to_id: r.get(1)?,
                    kind: r.get(2)?,
                    created_ms: r.get::<_, i64>(3)? as u64,
                })
            })
            .map_err(|e| err("cannot export", e))?
            .filter_map(std::result::Result::ok)
            .filter(|l| ids.contains(l.from_id.as_str()) || ids.contains(l.to_id.as_str()))
            .collect();
        drop(stmt);
        let sessions = self.list_sessions(project, 1_000_000)?;
        let mut consultations = Vec::new();
        let mut summaries = Vec::new();
        for s in &sessions {
            consultations.extend(self.consultations_in_session(&s.id, &s.project_id)?);
            summaries.extend(self.summaries_for_session(&s.id)?);
        }
        let mut evidence = Vec::new();
        for m in memories.iter().filter(|m| m.kind == MemoryKind::Evidence) {
            for r in self.evidence_refs(&m.id)? {
                evidence.push((m.id.clone(), r));
            }
        }
        Ok(ExportBundle {
            format: "ai-orchestra-memory-export".to_string(),
            schema_version: SCHEMA_VERSION,
            exported_ms: now,
            project: project.map(str::to_string),
            memories,
            links,
            sessions,
            consultations,
            summaries,
            evidence,
        })
    }
}

fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

pub fn clip_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// Moves a damaged database aside (never deletes it). Returns where it went.
pub fn quarantine(path: &Path, now: u64) -> Result<PathBuf> {
    let target = path.with_extension(format!("db.corrupt-{now}"));
    std::fs::rename(path, &target).map_err(|e| {
        OrchestraError::Memory(format!("cannot move {} aside: {e}", path.display()))
    })?;
    for ext in ["db-wal", "db-shm"] {
        let side = path.with_extension(ext);
        if side.exists() {
            let _ = std::fs::rename(&side, target.with_extension(format!("{ext}-{now}")));
        }
    }
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> (tempfile::TempDir, MemoryDb) {
        let dir = tempfile::tempdir().unwrap();
        let db = MemoryDb::open(&dir.path().join("sub").join("memory.db")).unwrap();
        (dir, db)
    }

    fn note(scope: MemoryScope, project: &str, text: &str) -> NewMemory {
        let mut m = NewMemory::new(MemoryKind::Note, scope, text);
        m.project_id = Some(project.to_string());
        m
    }

    fn session_note(project: &str, session: &str, text: &str) -> NewMemory {
        let mut m = note(MemoryScope::Session, project, text);
        m.session_id = Some(session.to_string());
        m
    }

    fn vis(project: &str) -> Visibility {
        Visibility {
            project: Some(project.to_string()),
            session_scope: true,
            project_scope: true,
            global_scope: true,
            any_session: true,
            ..Default::default()
        }
    }

    #[test]
    fn the_database_is_created_private_at_the_current_schema_and_full_text_search_works() {
        let (dir, mut db) = db();
        assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
        assert!(db.check().unwrap().is_empty());
        db.insert_memory(
            &note(
                MemoryScope::Project,
                "p",
                "The Rigol DS1054Z has 50 MHz of bandwidth",
            ),
            1,
        )
        .unwrap();
        let hits = db.search("\"bandwidth\"", &vis("p"), None, 5).unwrap();
        assert_eq!(hits.len(), 1, "FTS5 is available in the bundled SQLite");
        assert!(hits[0].1 > 0.0, "the score is higher-is-better");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(db.path()).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "owner-only");
            let dir_mode = std::fs::metadata(db.path().parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_ne!(dir_mode & 0o002, 0o002, "not world-writable");
        }
        drop(dir);
    }

    #[test]
    fn migrations_run_in_order_keep_the_data_and_refuse_a_newer_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.db");
        // A database as version 1 left it, with data in it.
        {
            create_private_file(&path).unwrap();
            let conn = Connection::open(&path).unwrap();
            let mut old = MemoryDb {
                conn,
                path: path.clone(),
                depth: std::cell::Cell::new(0),
            };
            old.migrate(1).unwrap();
            assert_eq!(old.schema_version().unwrap(), 1);
            old.insert_memory(
                &note(MemoryScope::Project, "p", "kept across the migration"),
                7,
            )
            .unwrap();
        }
        let db = MemoryDb::open(&path).unwrap();
        assert_eq!(db.schema_version().unwrap(), 2);
        assert_eq!(
            db.search("\"migration\"", &vis("p"), None, 5)
                .unwrap()
                .len(),
            1
        );
        db.insert_metrics(&MetricsRow {
            consultation_id: None,
            session_id: None,
            created_ms: 1,
            mode: None,
            baseline_chars: 10,
            context_chars: 5,
            est_tokens: 2,
            retrieved: 1,
            selected: 1,
            omitted: 0,
            retrieval_us: 3,
            assembly_us: 4,
            extra_ai_calls: 0,
        })
        .unwrap();
        // A file made by a newer version is refused, untouched.
        db.conn.execute_batch("PRAGMA user_version = 99").unwrap();
        drop(db);
        let err = MemoryDb::open(&path).unwrap_err().to_string();
        assert!(err.contains("newer than this program"), "{err}");
    }

    #[test]
    fn the_same_content_in_the_same_place_is_stored_once_but_other_places_are_separate() {
        let (_d, mut db) = db();
        let a = db
            .insert_memory(&note(MemoryScope::Project, "p", "Budget is 8,000 MXN"), 1)
            .unwrap();
        let b = db
            .insert_memory(
                &note(MemoryScope::Project, "p", "  budget is 8 000 mxn "),
                2,
            )
            .unwrap();
        assert!(!a.duplicate && b.duplicate);
        assert_eq!(a.id, b.id);
        let other = db
            .insert_memory(&note(MemoryScope::Project, "q", "Budget is 8,000 MXN"), 3)
            .unwrap();
        assert!(!other.duplicate, "another project is another place");
        assert_eq!(db.stats().unwrap().memories, 2);
    }

    #[test]
    fn scopes_never_leak_between_projects_or_sessions() {
        let (_d, mut db) = db();
        db.insert_memory(&session_note("a", "s1", "alpha session secret plan"), 1)
            .unwrap();
        db.insert_memory(&session_note("a", "s2", "alpha other session plan"), 2)
            .unwrap();
        db.insert_memory(&note(MemoryScope::Project, "a", "alpha project plan"), 3)
            .unwrap();
        db.insert_memory(&note(MemoryScope::Project, "b", "beta project plan"), 4)
            .unwrap();
        let mut global = NewMemory::new(
            MemoryKind::Preference,
            MemoryScope::Global,
            "global plan preference",
        );
        global.project_id = None;
        db.insert_memory(&global, 5).unwrap();
        let texts = |v: &Visibility| -> Vec<String> {
            let mut t: Vec<String> = db
                .search("\"plan\"", v, None, 20)
                .unwrap()
                .into_iter()
                .map(|(r, _)| r.text)
                .collect();
            t.sort();
            t
        };
        // The current session of project a, its project memories and the global ones.
        let in_session = Visibility {
            project: Some("a".into()),
            session: Some("s1".into()),
            session_scope: true,
            project_scope: true,
            global_scope: true,
            ..Default::default()
        };
        assert_eq!(
            texts(&in_session),
            [
                "alpha project plan",
                "alpha session secret plan",
                "global plan preference"
            ]
        );
        // Session only: nothing else.
        let only = Visibility {
            project_scope: false,
            global_scope: false,
            ..in_session.clone()
        };
        assert_eq!(texts(&only), ["alpha session secret plan"]);
        // Another project sees only its own and the global ones; a session id of another project is no key.
        let other = Visibility {
            project: Some("b".into()),
            session: Some("s1".into()),
            ..in_session.clone()
        };
        assert_eq!(
            texts(&other),
            ["beta project plan", "global plan preference"]
        );
        // Nothing is visible with every scope off.
        let none = Visibility {
            project: Some("a".into()),
            ..Default::default()
        };
        assert!(texts(&none).is_empty());
    }

    #[test]
    fn a_newer_statement_of_intent_on_the_same_topic_supersedes_the_older_one_without_erasing_it() {
        let (_d, mut db) = db();
        let mut old = NewMemory::new(
            MemoryKind::Preference,
            MemoryScope::Project,
            "Budget is 5,000 MXN",
        );
        old.project_id = Some("p".into());
        old.topic = Some("budget".into());
        let first = db.insert_memory(&old, 1).unwrap();
        let mut new = old.clone();
        new.text = "Budget is now 8,000 MXN".into();
        let second = db.insert_memory(&new, 2).unwrap();
        assert_eq!(second.superseded, vec![first.id.clone()]);
        let retrieved: Vec<String> = db
            .search("\"budget\"", &vis("p"), None, 10)
            .unwrap()
            .into_iter()
            .map(|(r, _)| r.text)
            .collect();
        assert_eq!(
            retrieved,
            ["Budget is now 8,000 MXN"],
            "retrieval sees only the current statement"
        );
        let old_rec = db.get_memory(&first.id).unwrap();
        assert_eq!(old_rec.lifecycle, Lifecycle::Superseded);
        assert_eq!(
            old_rec.text, "Budget is 5,000 MXN",
            "the old record is intact"
        );
        let links = db.links_of(&second.id).unwrap();
        assert!(
            links
                .iter()
                .any(|l| l.kind == "supersedes" && l.from_id == second.id && l.to_id == first.id)
        );
        let all = db
            .list_memories(
                &Visibility {
                    include_superseded: true,
                    ..vis("p")
                },
                None,
                10,
            )
            .unwrap();
        assert_eq!(all.len(), 2, "history stays inspectable");
    }

    #[test]
    fn a_correction_is_a_new_version_linked_to_the_old_one() {
        let (_d, mut db) = db();
        let first = db
            .insert_memory(
                &note(MemoryScope::Project, "p", "The scope costs 4000 MXN"),
                1,
            )
            .unwrap();
        let fixed = db
            .correct_memory(&first.id, "The scope costs 4500 MXN", 2)
            .unwrap();
        assert!(fixed.superseded.contains(&first.id));
        assert_eq!(
            db.get_memory(&first.id).unwrap().lifecycle,
            Lifecycle::Superseded
        );
        let current = db.get_memory(&fixed.id).unwrap();
        assert_eq!(
            (current.text.as_str(), current.lifecycle),
            ("The scope costs 4500 MXN", Lifecycle::Active)
        );
        assert_eq!(current.authorization, Authorization::ExplicitUser);
        // A replaced record cannot be corrected again; the newer one can.
        assert!(db.correct_memory(&first.id, "x", 3).is_err());
        assert!(
            db.correct_memory(&fixed.id, "The scope costs 4600 MXN", 4)
                .is_ok()
        );
    }

    #[test]
    fn forgetting_removes_the_content_the_index_entry_and_the_links_and_leaves_a_blank_tombstone() {
        let (_d, mut db) = db();
        let a = db
            .insert_memory(&note(MemoryScope::Project, "p", "kept zebra fact"), 1)
            .unwrap();
        let b = db
            .insert_memory(
                &note(MemoryScope::Project, "p", "forget this unicorn fact"),
                2,
            )
            .unwrap();
        db.link(&a.id, &b.id, LinkKind::Contradicts, 3).unwrap();
        db.add_evidence_refs(
            &b.id,
            &[EvidenceRef {
                url: Some("https://e.org".into()),
                title: None,
                published: None,
                updated: None,
                retrieved_ms: Some(1),
                content_hash: None,
                relation: None,
                quote: Some("unicorn quote".into()),
            }],
        )
        .unwrap();
        let removed = db.delete_memory(&b.id[..9], 9).unwrap();
        assert_eq!(removed.id, b.id);
        assert!(
            db.search("\"unicorn\"", &vis("p"), None, 5)
                .unwrap()
                .is_empty(),
            "gone from the index"
        );
        assert_eq!(
            db.search("\"zebra\"", &vis("p"), None, 5).unwrap().len(),
            1,
            "the other record is unaffected"
        );
        assert!(db.links_of(&a.id).unwrap().is_empty());
        assert!(db.evidence_refs(&b.id).unwrap().is_empty());
        assert!(db.get_memory(&b.id).is_err());
        let tomb: (String, String) = db
            .conn
            .query_row("SELECT id, kind FROM tombstones", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(
            tomb,
            (b.id.clone(), "note".to_string()),
            "the tombstone holds no text"
        );
        // The bytes are gone from the file, not just from the table.
        let bytes = std::fs::read(db.path()).unwrap();
        assert!(
            !String::from_utf8_lossy(&bytes).contains("unicorn"),
            "secure_delete overwrote the content"
        );
        assert!(
            db.check().unwrap().is_empty(),
            "the index is consistent after the delete"
        );
    }

    #[test]
    fn clearing_a_session_removes_its_records_and_nothing_else() {
        let (_d, mut db) = db();
        let s1 = db.create_session("p", Some("one"), 1).unwrap();
        let s2 = db.create_session("p", Some("two"), 2).unwrap();
        db.insert_memory(&session_note("p", &s1.id, "first session wombat"), 3)
            .unwrap();
        db.insert_memory(&session_note("p", &s2.id, "second session wombat"), 4)
            .unwrap();
        db.insert_memory(&note(MemoryScope::Project, "p", "project wombat"), 5)
            .unwrap();
        let n = db.clear_session(&s1.id, "p", 9).unwrap();
        assert_eq!(n, 1);
        let left: Vec<String> = db
            .search("\"wombat\"", &vis("p"), None, 10)
            .unwrap()
            .into_iter()
            .map(|(r, _)| r.text)
            .collect();
        assert_eq!(left.len(), 2);
        assert!(!left.iter().any(|t| t.contains("first")));
        assert!(
            db.get_session(&s1.id).unwrap().is_none() && db.get_session(&s2.id).unwrap().is_some()
        );
        // The wrong project cannot clear it.
        assert_eq!(db.clear_session(&s2.id, "other", 10).unwrap(), 0);
    }

    #[test]
    fn an_expired_record_is_invisible_before_the_next_prune() {
        let (_d, mut db) = db();
        let mut gone = note(MemoryScope::Project, "p", "oscilloscope loan expires");
        gone.expires_ms = Some(5);
        db.insert_memory(&gone, 1).unwrap();
        let mut later = note(MemoryScope::Project, "p", "oscilloscope loan lasts");
        later.expires_ms = Some(u64::MAX / 4);
        db.insert_memory(&later, 2).unwrap();
        let vis = Visibility {
            project: Some("p".into()),
            project_scope: true,
            ..Default::default()
        };
        let hits = db.search("\"oscilloscope\"", &vis, None, 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].0.text.contains("lasts"));
        assert_eq!(db.list_memories(&vis, None, 10).unwrap().len(), 1);
    }

    #[test]
    fn expired_records_and_idle_sessions_are_pruned() {
        let (_d, mut db) = db();
        let mut short = note(MemoryScope::Project, "p", "expires soon");
        short.expires_ms = Some(100);
        db.insert_memory(&short, 1).unwrap();
        db.insert_memory(&note(MemoryScope::Project, "p", "stays"), 2)
            .unwrap();
        let old = db.create_session("p", None, 1_000).unwrap();
        db.insert_memory(&session_note("p", &old.id, "old session text"), 3)
            .unwrap();
        let day = 86_400_000u64;
        let (memories, sessions) = db.prune(200 * day, Some(90)).unwrap();
        assert_eq!((memories, sessions), (2, 1));
        assert_eq!(db.stats().unwrap().memories, 1);
    }

    #[test]
    fn a_multi_step_write_is_all_or_nothing_and_nested_writes_join_it() {
        let (_d, mut db) = db();
        let failed: Result<()> = db.atomically(|| {
            db.insert_memory_inner(&note(MemoryScope::Project, "p", "first step"), 1)?;
            db.insert_memory_inner(&note(MemoryScope::Project, "p", "second step"), 2)?;
            // The third step is refused (empty), so nothing before it may remain.
            db.insert_memory_inner(&note(MemoryScope::Project, "p", "  "), 3)?;
            Ok(())
        });
        assert!(failed.is_err());
        assert_eq!(
            db.stats().unwrap().memories,
            0,
            "the failed write left nothing behind"
        );
        let ok: Result<()> = db.atomically(|| {
            db.atomically(|| {
                db.insert_memory_inner(&note(MemoryScope::Project, "p", "nested"), 1)
                    .map(|_| ())
            })?;
            db.insert_memory_inner(&note(MemoryScope::Project, "p", "outer"), 2)?;
            Ok(())
        });
        assert!(ok.is_ok());
        assert_eq!(db.stats().unwrap().memories, 2);
        // A failure in an inner step rolls back only that step when the outer one carries on.
        let partial: Result<()> = db.atomically(|| {
            db.insert_memory_inner(&note(MemoryScope::Project, "p", "kept"), 3)?;
            let _ = db.atomically(|| {
                db.insert_memory_inner(&note(MemoryScope::Project, "p", ""), 4)
                    .map(|_| ())
            });
            Ok(())
        });
        assert!(partial.is_ok());
        assert_eq!(db.stats().unwrap().memories, 3);
        assert!(
            db.insert_memory(&note(MemoryScope::Project, "p", "after"), 5)
                .is_ok(),
            "the connection is usable afterwards"
        );
    }

    #[test]
    fn ids_and_prefixes_are_unambiguous() {
        let (_d, mut db) = db();
        let a = db
            .insert_memory(&note(MemoryScope::Project, "p", "one"), 1)
            .unwrap();
        let b = db
            .insert_memory(&note(MemoryScope::Project, "p", "two"), 2)
            .unwrap();
        assert_ne!(a.id, b.id);
        assert_eq!(db.get_memory(&a.id).unwrap().text, "one");
        assert!(
            db.get_memory("mem-")
                .unwrap_err()
                .to_string()
                .contains("more than one")
        );
        assert!(
            db.get_memory("nope")
                .unwrap_err()
                .to_string()
                .contains("no memory")
        );
        assert!(
            db.get_memory("%").is_err(),
            "LIKE wildcards in an id are not wildcards"
        );
    }

    #[test]
    fn oversized_or_empty_text_is_refused() {
        let (_d, mut db) = db();
        assert!(
            db.insert_memory(&note(MemoryScope::Project, "p", "   "), 1)
                .is_err()
        );
        let big = "x".repeat(MAX_TEXT_BYTES + 1);
        assert!(
            db.insert_memory(&note(MemoryScope::Project, "p", &big), 1)
                .is_err()
        );
    }

    #[test]
    fn export_is_deterministic_and_complete() {
        let (_d, mut db) = db();
        let s = db.create_session("p", Some("t"), 1).unwrap();
        db.insert_memory(&session_note("p", &s.id, "session thing"), 2)
            .unwrap();
        let mut sensitive = note(MemoryScope::Project, "p", "contact ana@example.com");
        sensitive.sensitive = true;
        db.insert_memory(&sensitive, 3).unwrap();
        db.insert_memory(&note(MemoryScope::Project, "q", "other project"), 4)
            .unwrap();
        let a = serde_json::to_string(&db.export(Some("p"), 10).unwrap()).unwrap();
        let b = serde_json::to_string(&db.export(Some("p"), 10).unwrap()).unwrap();
        assert_eq!(a, b, "same data, same bytes");
        assert!(
            a.contains("session thing")
                && a.contains("ana@example.com")
                && !a.contains("other project")
        );
    }

    #[test]
    fn concurrent_connections_read_while_another_writes_and_nothing_is_lost() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.db");
        MemoryDb::open(&path).unwrap();
        let writers: Vec<_> = (0..4)
            .map(|w| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let mut db = MemoryDb::open(&path).unwrap();
                    for i in 0..25 {
                        db.insert_memory(
                            &note(
                                MemoryScope::Project,
                                "p",
                                &format!("writer {w} item {i} concurrent"),
                            ),
                            (w * 100 + i) as u64,
                        )
                        .unwrap();
                    }
                })
            })
            .collect();
        let reader = {
            let path = path.clone();
            std::thread::spawn(move || {
                let db = MemoryDb::open(&path).unwrap();
                for _ in 0..50 {
                    let _ = db.search("\"concurrent\"", &vis("p"), None, 10).unwrap();
                }
            })
        };
        for w in writers {
            w.join().unwrap();
        }
        reader.join().unwrap();
        let db = MemoryDb::open(&path).unwrap();
        assert_eq!(db.stats().unwrap().memories, 100);
        assert!(db.check().unwrap().is_empty());
    }

    #[test]
    fn a_file_that_is_not_a_database_is_reported_and_can_be_set_aside_without_deleting_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.db");
        std::fs::write(
            &path,
            b"this is definitely not a sqlite database, but long enough to look like a file"
                .repeat(20),
        )
        .unwrap();
        let e = MemoryDb::open(&path).unwrap_err().to_string();
        assert!(
            e.contains("damaged")
                && e.contains("Nothing was deleted")
                && e.contains("memory check"),
            "{e}"
        );
        let moved = quarantine(&path, 42).unwrap();
        assert!(moved.exists() && !path.exists(), "moved, not deleted");
        let fresh = MemoryDb::open(&path).unwrap();
        assert_eq!(fresh.stats().unwrap().memories, 0);
        assert!(
            std::fs::read(&moved)
                .unwrap()
                .starts_with(b"this is definitely")
        );
    }

    #[test]
    fn a_broken_full_text_index_is_detected_and_rebuilt() {
        let (_d, mut db) = db();
        db.insert_memory(
            &note(MemoryScope::Project, "p", "indexed text about narwhals"),
            1,
        )
        .unwrap();
        db.conn
            .execute("DELETE FROM memory_fts_docsize", [])
            .unwrap();
        let problems = db.check().unwrap();
        assert!(!problems.is_empty(), "{problems:?}");
        db.rebuild_index().unwrap();
        assert!(db.check().unwrap().is_empty());
        assert_eq!(
            db.search("\"narwhals\"", &vis("p"), None, 5).unwrap().len(),
            1
        );
    }
}
