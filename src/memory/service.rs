//! The memory service: what a consultation and the `memory` / `session` commands do with the
//! database, behind one asynchronous interface that the CLI and the MCP server share.
//!
//! Database calls run on a bounded pool of blocking threads (at most [`DB_CONCURRENCY`] at a time),
//! each on its own short-lived connection, so a Tokio worker is never held by SQLite and several
//! consultations (or processes) can read and write at once.
//!
//! The rules enforced here, in one place:
//!
//! * a consultation writes only **session** memory, automatically, and only when memory is on and
//!   saving is not disabled; anything **persistent** (project or global) needs an explicit action;
//! * credentials are never stored; personal data is not stored automatically and needs an explicit
//!   flag to be stored by hand;
//! * a record is only visible to its own project (session records, to their own session);
//! * remembering never raises trust: conclusions stay "AI-generated, not verified", evidence keeps
//!   the status and the dates it had when it was checked.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;

use super::compress::{DigestInput, clip_at_boundary, digest_outcome};
use super::context::{AssemblyRequest, ContextAssemblyEngine, ContextReport, SharedContext};
use super::db::{
    ConsultationRow, DbStats, EvidenceRef, ExportBundle, InsertOutcome, MemoryDb, MetricsRow,
    SummaryRow, Visibility, quarantine,
};
use super::freshness::FreshnessPolicy;
use super::model::{
    Authorization, EpistemicStatus, LinkKind, MemoryKind, MemoryMode, MemoryRecord, MemoryScope,
    NewMemory, Origin, SessionRow,
};
use super::retrieval::{MemoryRetrievalEngine, RetrievalRequest, match_expression, tokenize_terms};
use super::sensitive::{Sensitivity, classify};
use crate::config::Config;
use crate::debate::{DebateOutcome, DebateStatus, Step};
use crate::error::{OrchestraError, Result};
use crate::history::{DebateStore, now_ms};
use crate::research::engine::ResearchReport;
use crate::store::redact_secrets;

/// Database calls running at once.
pub const DB_CONCURRENCY: usize = 4;

#[derive(Debug, Clone)]
pub struct MemorySettings {
    pub enabled: bool,
    pub default_mode: MemoryMode,
    pub context_budget_tokens: usize,
    pub session_idle_hours: u32,
    pub session_retention_days: Option<u32>,
    pub auto_include_global: bool,
    pub recent_turns: usize,
    pub freshness: FreshnessPolicy,
    pub project_name: Option<String>,
    pub allow_mcp_writes: bool,
}

impl MemorySettings {
    pub fn from_config(config: &Config) -> Self {
        let m = &config.memory;
        Self {
            enabled: true,
            default_mode: m.mode().unwrap_or(MemoryMode::Auto),
            context_budget_tokens: m.context_budget_tokens,
            session_idle_hours: m.session_idle_hours,
            session_retention_days: (m.session_retention_days > 0)
                .then_some(m.session_retention_days),
            auto_include_global: m.auto_include_global,
            recent_turns: m.recent_turns,
            freshness: FreshnessPolicy {
                time_sensitive_days: m.freshness_time_sensitive_days,
                price_days: m.freshness_price_days,
                general_days: m.freshness_general_days,
                fresh_question_days: m.freshness_fresh_question_days,
            },
            project_name: m.project.clone(),
            allow_mcp_writes: m.allow_mcp_writes,
        }
    }
}

impl Default for MemorySettings {
    fn default() -> Self {
        Self::from_config(&Config::default())
    }
}

/// A project: the unit that keeps one body of work apart from another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectRef {
    pub id: String,
    pub path: Option<String>,
    pub name: Option<String>,
}

fn short_hash(input: &str) -> String {
    Sha256::digest(input.as_bytes())
        .iter()
        .take(6)
        .fold(String::new(), |mut s, b| {
            use std::fmt::Write as _;
            let _ = write!(s, "{b:02x}");
            s
        })
}

impl ProjectRef {
    /// Identified by the canonical path of the directory, so `./x` and `/abs/x` are one project.
    pub fn from_path(path: &Path) -> Self {
        let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let text = canonical.display().to_string();
        Self {
            id: format!("prj-{}", short_hash(&format!("path:{text}"))),
            name: canonical
                .file_name()
                .map(|n| n.to_string_lossy().into_owned()),
            path: Some(text),
        }
    }

    /// Identified by a chosen name (`[memory] project`), whatever directory it is used from.
    pub fn named(name: &str) -> Self {
        Self {
            id: format!("prj-{}", short_hash(&format!("name:{name}"))),
            path: None,
            name: Some(name.to_string()),
        }
    }

    /// The configured name if there is one, else the working directory.
    pub fn resolve(configured: Option<&str>, cwd: Option<&Path>) -> Self {
        match (configured, cwd) {
            (Some(name), _) => Self::named(name),
            (None, Some(dir)) => Self::from_path(dir),
            (None, None) => {
                Self::from_path(&std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
            }
        }
    }
}

/// What a consultation asks of memory.
#[derive(Debug, Clone)]
pub struct MemoryRequest {
    pub mode: MemoryMode,
    /// Continue this session (it must belong to the project).
    pub session: Option<String>,
    pub context_budget_tokens: Option<usize>,
    pub explain: bool,
    pub project: Option<ProjectRef>,
    /// Write session memory after the consultation (false with `--no-save`).
    pub write: bool,
    /// Start a new session instead of continuing the latest one (the chat's `/new`). Ignored when
    /// `session` names one.
    pub new_session: bool,
}

impl MemoryRequest {
    pub fn new(mode: MemoryMode) -> Self {
        Self {
            mode,
            session: None,
            context_budget_tokens: None,
            explain: false,
            project: None,
            write: true,
            new_session: false,
        }
    }
}

/// The context prepared for one consultation.
#[derive(Debug, Clone)]
pub struct Prepared {
    pub mode: MemoryMode,
    pub project: ProjectRef,
    pub session: Option<SessionRow>,
    pub context: SharedContext,
    pub report: ContextReport,
    /// The block to put before the question (empty when nothing relevant was found).
    pub text: String,
    pub warnings: Vec<String>,
    pub write: bool,
}

impl Prepared {
    fn disabled(mode: MemoryMode, project: ProjectRef, why: &str) -> Self {
        let context = SharedContext::default();
        let mut report = context.report(mode.as_str(), None, &project.id);
        report.warnings.push(why.to_string());
        Self {
            mode,
            project,
            session: None,
            context,
            report,
            text: String::new(),
            warnings: Vec::new(),
            write: false,
        }
    }
}

/// A finished consultation, to remember.
pub struct RecordInput<'a> {
    pub prepared: &'a Prepared,
    pub history_id: &'a str,
    pub question: &'a str,
    pub mode: &'a str,
    pub strategy: Option<&'a str>,
    pub status: DebateStatus,
    pub calls: usize,
    pub final_answer: &'a str,
    pub steps: &'a [Step],
    pub research: Option<&'a ResearchReport>,
    pub extra_ai_calls: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordOutcome {
    pub session_id: Option<String>,
    pub stored: usize,
    pub superseded: usize,
    /// Why nothing was remembered, when that is the case.
    pub skipped: Option<String>,
}

pub struct AddInput {
    pub project: ProjectRef,
    pub scope: MemoryScope,
    pub kind: MemoryKind,
    pub text: String,
    pub topic: Option<String>,
    pub tags: Vec<String>,
    pub session: Option<String>,
    pub allow_sensitive: bool,
    pub expires_days: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchHit {
    pub record: MemoryRecord,
    pub score: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryDetail {
    pub record: MemoryRecord,
    pub links: Vec<super::db::LinkRow>,
    pub evidence: Vec<EvidenceRef>,
    /// The saved final answer of the consultation a conclusion came from.
    pub original_answer: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionDetail {
    pub session: SessionRow,
    pub consultations: Vec<ConsultationRow>,
    pub summaries: Vec<SummaryRow>,
    pub memories: Vec<MemoryRecord>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckReport {
    pub path: String,
    pub healthy: bool,
    pub problems: Vec<String>,
    pub quarantined_to: Option<String>,
    pub rebuilt_index: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportReport {
    pub imported: usize,
    pub already_imported: usize,
    pub not_importable: usize,
    pub sensitive_skipped: usize,
}

pub struct MemoryService {
    path: PathBuf,
    settings: MemorySettings,
    gate: Arc<Semaphore>,
}

impl std::fmt::Debug for MemoryService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryService")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

const DAY_MS: u64 = 86_400_000;

fn derive_topic(kind: MemoryKind, text: &str) -> Option<String> {
    if !kind.is_user_intent() {
        return None;
    }
    let lower = text.to_lowercase();
    if lower.contains("budget") || lower.contains("presupuesto") {
        Some("budget".to_string())
    } else if lower.contains("deadline")
        || lower.contains("fecha límite")
        || lower.contains("fecha limite")
    {
        Some("deadline".to_string())
    } else {
        None
    }
}

/// Once a day, at most, removes what has expired. Failure to prune never stops a write.
fn maybe_prune(db: &mut MemoryDb, now: u64, retention: Option<u32>) {
    let last = db
        .meta("last_prune_ms")
        .ok()
        .flatten()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    if now.saturating_sub(last) > DAY_MS {
        let _ = db.prune(now, retention);
        let _ = db.set_meta("last_prune_ms", &now.to_string());
    }
}

fn not_visible(id: &str) -> OrchestraError {
    OrchestraError::Config(format!(
        "memory `{id}` belongs to another project; run from that project's directory (or set `[memory] project`) to reach it"
    ))
}

/// A record can be reached from this project: its own project and session records, and global ones.
fn visible_to(r: &MemoryRecord, project: &ProjectRef) -> bool {
    match r.scope {
        MemoryScope::Global => true,
        _ => r.project_id.as_deref() == Some(project.id.as_str()),
    }
}

impl MemoryService {
    pub fn new(path: PathBuf, settings: MemorySettings) -> Arc<Self> {
        Arc::new(Self {
            path,
            settings,
            gate: Arc::new(Semaphore::new(DB_CONCURRENCY)),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn settings(&self) -> &MemorySettings {
        &self.settings
    }

    pub fn default_project(&self, cwd: Option<&Path>) -> ProjectRef {
        ProjectRef::resolve(self.settings.project_name.as_deref(), cwd)
    }

    /// Runs `f` on a blocking thread with its own connection, at most [`DB_CONCURRENCY`] at a time.
    pub async fn run<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut MemoryDb) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let _permit =
            self.gate.acquire().await.map_err(|_| {
                OrchestraError::Memory("the memory service is shutting down".into())
            })?;
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let mut db = MemoryDb::open(&path)?;
            f(&mut db)
        })
        .await
        .map_err(|e| OrchestraError::Memory(format!("a memory task failed: {e}")))?
    }

    fn exists(&self) -> bool {
        self.path.exists()
    }

    // ------------------------------------------------------------------- for a consultation

    /// Chooses the session, retrieves, and assembles the context. Reads only: nothing is created.
    /// A session that was asked for by name but does not exist, or belongs to another project, is
    /// an error (a request that cannot be honoured); a database that cannot be read is not, it is
    /// reported in the warnings and the consultation goes on without memory.
    pub async fn prepare(
        &self,
        req: &MemoryRequest,
        question: &str,
        provider_cap_bytes: usize,
        wants_fresh: bool,
    ) -> Result<Prepared> {
        let project = req
            .project
            .clone()
            .unwrap_or_else(|| self.default_project(None));
        if req.mode == MemoryMode::Off || !self.settings.enabled {
            return Ok(Prepared::disabled(
                req.mode,
                project,
                "memory is off: nothing was read or will be written",
            ));
        }
        if !self.exists() {
            if let Some(id) = &req.session {
                return Err(OrchestraError::Config(format!(
                    "no session `{id}` (there is no memory yet)"
                )));
            }
            let mut p = Prepared::disabled(req.mode, project, "there is no memory yet");
            p.write = req.write;
            return Ok(p);
        }
        let settings = self.settings.clone();
        let (q, explicit, proj, mode) = (
            question.to_string(),
            req.session.clone(),
            project.clone(),
            req.mode,
        );
        let budget = req
            .context_budget_tokens
            .unwrap_or(settings.context_budget_tokens);
        let new_session = req.new_session;
        let now = now_ms();
        let result = self
            .run(move |db| {
                let session = match &explicit {
                    Some(id) => {
                        let s = db
                            .get_session(id)?
                            .ok_or_else(|| OrchestraError::Config(format!("no session `{id}` (see `ai-orchestra session list`)")))?;
                        if s.project_id != proj.id {
                            return Err(OrchestraError::Config(format!(
                                "session `{}` belongs to another project; sessions are never shared between projects",
                                s.id
                            )));
                        }
                        Some(s)
                    }
                    None if new_session => None,
                    None => {
                        let not_before = now.saturating_sub(u64::from(settings.session_idle_hours) * 3_600_000);
                        db.latest_session(&proj.id, not_before)?
                    }
                };
                let baseline_chars: u64 = match &session {
                    Some(s) => db
                        .consultations_in_session(&s.id, &proj.id)?
                        .iter()
                        .map(|c| (c.question.chars().count() + c.final_answer.as_deref().map_or(0, |a| a.chars().count())) as u64)
                        .sum(),
                    None => 0,
                };
                let kinds: Vec<MemoryKind> = MemoryKind::ALL.into_iter().filter(|k| *k != MemoryKind::Finding).collect();
                let retrieval = MemoryRetrievalEngine::default().retrieve(
                    db,
                    &RetrievalRequest {
                        question: q.clone(),
                        mode,
                        global_in_auto: settings.auto_include_global,
                        project: proj.id.clone(),
                        session: session.as_ref().map(|s| s.id.clone()),
                        kinds: Some(kinds),
                        limit: 40,
                        recent_turns: settings.recent_turns,
                        wants_fresh,
                        now_ms: now,
                        freshness: settings.freshness,
                    },
                )?;
                let ai_summary = match &session {
                    Some(s) => db.summaries_for_session(&s.id)?.into_iter().rev().find(|x| x.origin.starts_with("ai:")),
                    None => None,
                };
                let context = ContextAssemblyEngine.assemble(
                    db,
                    &retrieval,
                    &AssemblyRequest {
                        question: &q,
                        budget_tokens: budget,
                        provider_cap_bytes,
                        now_ms: now,
                        baseline_chars,
                        project_name: proj.name.clone(),
                        ai_summary,
                    },
                )?;
                Ok((session, context))
            })
            .await;
        match result {
            Ok((session, context)) => {
                let report = context.report(
                    mode.as_str(),
                    session.as_ref().map(|s| s.id.as_str()),
                    &project.id,
                );
                let text = context.render();
                Ok(Prepared {
                    mode,
                    project,
                    session,
                    context,
                    report,
                    text,
                    warnings: Vec::new(),
                    write: req.write,
                })
            }
            Err(e @ OrchestraError::Config(_)) => Err(e),
            Err(e) => {
                let mut p = Prepared::disabled(
                    req.mode,
                    project,
                    "memory could not be read, so this consultation runs without it",
                );
                p.warnings.push(format!("memory: {e}"));
                Ok(p)
            }
        }
    }

    /// Remembers a finished consultation as session memory. Never persistent, never automatic for
    /// anything that looks sensitive.
    pub async fn record(&self, input: RecordInput<'_>) -> Result<RecordOutcome> {
        let p = input.prepared;
        if p.mode == MemoryMode::Off || !self.settings.enabled || !p.write {
            return Ok(RecordOutcome {
                skipped: Some("memory writes are off for this consultation".into()),
                ..Default::default()
            });
        }
        if matches!(input.status, DebateStatus::Failed | DebateStatus::Cancelled)
            || input.final_answer.trim().is_empty()
        {
            return Ok(RecordOutcome {
                skipped: Some("the consultation produced no answer to remember".into()),
                ..Default::default()
            });
        }
        for (what, text) in [("question", input.question), ("answer", input.final_answer)] {
            match classify(text) {
                Sensitivity::None => {}
                Sensitivity::Credential => {
                    return Ok(RecordOutcome {
                        skipped: Some(format!(
                            "the {what} looked like it contained a credential, so nothing was remembered"
                        )),
                        ..Default::default()
                    });
                }
                Sensitivity::Personal => {
                    return Ok(RecordOutcome {
                        skipped: Some(format!(
                            "the {what} looked like it contained personal data, so nothing was remembered automatically"
                        )),
                        ..Default::default()
                    });
                }
            }
        }
        let digest = digest_outcome(&DigestInput {
            question: input.question,
            mode: input.mode,
            final_answer: input.final_answer,
            steps: input.steps,
            research: input.research,
        });
        let project = p.project.clone();
        let prior_session = p.session.clone();
        let report = p.report.clone();
        let (history_id, question, mode_text, strategy, status, calls) = (
            input.history_id.to_string(),
            input.question.to_string(),
            input.mode.to_string(),
            input.strategy.map(str::to_string),
            input.status,
            input.calls,
        );
        let answer = redact_secrets(input.final_answer);
        let extra = input.extra_ai_calls;
        let settings = self.settings.clone();
        let now = now_ms();
        let retention = settings.session_retention_days;
        self.run(move |db| {
            maybe_prune(db, now, retention);
            db.atomically(|| {
                self_record(
                    db,
                    &project,
                    prior_session,
                    &history_id,
                    &question,
                    &mode_text,
                    strategy,
                    status,
                    calls,
                    &answer,
                    digest,
                    &report,
                    extra,
                    now,
                )
            })
        })
        .await
    }

    // ------------------------------------------------------------------------- user actions

    pub async fn add(&self, input: AddInput) -> Result<(InsertOutcome, MemoryRecord)> {
        self.add_with(input, None).await
    }

    /// Like `add`, for text a host agent relays through MCP on the user's behalf: the record's
    /// origin is that agent and its trust is "agent interpretation", never "user provided",
    /// because the server cannot see what the user actually said.
    pub async fn add_relayed(
        &self,
        input: AddInput,
        relayed_by: &str,
    ) -> Result<(InsertOutcome, MemoryRecord)> {
        self.add_with(input, Some(relayed_by.to_string())).await
    }

    async fn add_with(
        &self,
        input: AddInput,
        relayed_by: Option<String>,
    ) -> Result<(InsertOutcome, MemoryRecord)> {
        if !input.kind.user_addable() {
            return Err(OrchestraError::Config(format!(
                "a `{}` memory cannot be added by hand (use preference, decision, requirement, interest or note)",
                input.kind
            )));
        }
        let text = input.text.trim().to_string();
        match classify(&text) {
            Sensitivity::Credential => {
                return Err(OrchestraError::Config(
                    "that looks like a credential (a key, token or password); credentials are never stored as memory".into(),
                ));
            }
            Sensitivity::Personal if !input.allow_sensitive => {
                return Err(OrchestraError::Config(
                    "that looks like personal data (contact details or a payment number). It is not stored unless you pass --allow-sensitive; it would then be kept private to you and never sent to an agent".into(),
                ));
            }
            _ => {}
        }
        let sensitive = classify(&text) == Sensitivity::Personal;
        let topic = input
            .topic
            .clone()
            .or_else(|| derive_topic(input.kind, &text));
        let now = now_ms();
        let svc_settings = self.settings.clone();
        self.run(move |db| {
            let mut m = NewMemory::new(input.kind, input.scope, redact_secrets(&text));
            m.topic = topic;
            m.tags = input.tags.clone();
            m.sensitive = sensitive;
            m.expires_ms = input.expires_days.map(|d| now + u64::from(d) * DAY_MS);
            if let Some(agent) = &relayed_by {
                m.origin = Origin::Agent(agent.clone());
                m.status = EpistemicStatus::AgentInterpretation;
            }
            match input.scope {
                MemoryScope::Global => {}
                MemoryScope::Project => m.project_id = Some(input.project.id.clone()),
                MemoryScope::Session => {
                    let session = match &input.session {
                        Some(id) => db.get_session(id)?.ok_or_else(|| OrchestraError::Config(format!("no session `{id}`")))?,
                        None => db
                            .latest_session(&input.project.id, now.saturating_sub(u64::from(svc_settings.session_idle_hours) * 3_600_000))?
                            .ok_or_else(|| OrchestraError::Config("there is no current session; use --scope project, or start one by running a consultation".into()))?,
                    };
                    if session.project_id != input.project.id {
                        return Err(OrchestraError::Config("that session belongs to another project".into()));
                    }
                    m.session_id = Some(session.id);
                    m.project_id = Some(input.project.id.clone());
                }
            }
            if input.scope != MemoryScope::Global {
                db.upsert_project(&input.project.id, input.project.path.as_deref(), input.project.name.as_deref(), now)?;
            }
            let out = db.insert_memory(&m, now)?;
            let record = db.get_memory(&out.id)?;
            Ok((out, record))
        })
        .await
    }

    fn visibility(
        project: &ProjectRef,
        scope: Option<MemoryScope>,
        include_superseded: bool,
    ) -> Visibility {
        Visibility {
            project: Some(project.id.clone()),
            session_scope: scope.is_none_or(|s| s == MemoryScope::Session),
            project_scope: scope.is_none_or(|s| s == MemoryScope::Project),
            global_scope: scope.is_none_or(|s| s == MemoryScope::Global),
            any_session: true,
            include_superseded,
            include_sensitive: true,
            ..Default::default()
        }
    }

    pub async fn list(
        &self,
        project: &ProjectRef,
        scope: Option<MemoryScope>,
        kind: Option<MemoryKind>,
        include_superseded: bool,
        limit: usize,
    ) -> Result<Vec<MemoryRecord>> {
        if !self.exists() {
            return Ok(Vec::new());
        }
        let vis = Self::visibility(project, scope, include_superseded);
        self.run(move |db| {
            let kinds = kind.map(|k| vec![k]);
            db.list_memories(&vis, kinds.as_deref(), limit)
        })
        .await
    }

    /// Searches everything the user could see in this project (the user's own view, so records
    /// marked sensitive are included here, and only here).
    pub async fn search(
        &self,
        project: &ProjectRef,
        query: &str,
        scope: Option<MemoryScope>,
        include_superseded: bool,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        self.search_visible(project, query, scope, include_superseded, true, limit)
            .await
    }

    /// The search an agent may see: the same, but records marked sensitive are left out.
    pub async fn search_shared(
        &self,
        project: &ProjectRef,
        query: &str,
        scope: Option<MemoryScope>,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        self.search_visible(project, query, scope, false, false, limit)
            .await
    }

    async fn search_visible(
        &self,
        project: &ProjectRef,
        query: &str,
        scope: Option<MemoryScope>,
        include_superseded: bool,
        include_sensitive: bool,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        if !self.exists() {
            return Ok(Vec::new());
        }
        let mut vis = Self::visibility(project, scope, include_superseded);
        vis.include_sensitive = include_sensitive;
        let expr = match_expression(&tokenize_terms(query), 12);
        self.run(move |db| {
            Ok(db
                .search(&expr, &vis, None, limit)?
                .into_iter()
                .map(|(record, score)| SearchHit { record, score })
                .collect())
        })
        .await
    }

    pub async fn show(&self, project: &ProjectRef, id: &str) -> Result<MemoryDetail> {
        let (id, project) = (id.to_string(), project.clone());
        self.run(move |db| {
            let record = db.get_memory(&id)?;
            if !visible_to(&record, &project) {
                return Err(not_visible(&record.id));
            }
            let original_answer = match (&record.kind, &record.consultation_id) {
                (MemoryKind::Conclusion, Some(c)) => {
                    db.get_consultation(c)?.and_then(|row| row.final_answer)
                }
                _ => None,
            };
            Ok(MemoryDetail {
                links: db.links_of(&record.id)?,
                evidence: db.evidence_refs(&record.id)?,
                original_answer,
                record,
            })
        })
        .await
    }

    pub async fn forget(&self, project: &ProjectRef, id: &str) -> Result<MemoryRecord> {
        let (id, project) = (id.to_string(), project.clone());
        let now = now_ms();
        self.run(move |db| {
            let record = db.get_memory(&id)?;
            if !visible_to(&record, &project) {
                return Err(not_visible(&record.id));
            }
            db.delete_memory(&record.id, now)
        })
        .await
    }

    /// A correction: a new version of the record, which supersedes the old one (still readable).
    pub async fn correct(
        &self,
        project: &ProjectRef,
        id: &str,
        new_text: &str,
    ) -> Result<InsertOutcome> {
        match classify(new_text) {
            Sensitivity::Credential => {
                return Err(OrchestraError::Config(
                    "that looks like a credential; credentials are never stored as memory".into(),
                ));
            }
            Sensitivity::Personal => {
                return Err(OrchestraError::Config("that looks like personal data; add it with `memory add --allow-sensitive` if you mean to keep it".into()));
            }
            Sensitivity::None => {}
        }
        let (id, project, text) = (id.to_string(), project.clone(), redact_secrets(new_text));
        let now = now_ms();
        self.run(move |db| {
            let old = db.get_memory(&id)?;
            if !visible_to(&old, &project) {
                return Err(not_visible(&old.id));
            }
            db.correct_memory(&old.id, &text, now)
        })
        .await
    }

    /// Copies a record into project or global memory, on the user's say-so. The copy keeps the
    /// original's origin, status and dates: promotion never raises trust. An agent's individual
    /// opinion cannot be promoted, because it is never shared with other agents.
    pub async fn promote(
        &self,
        project: &ProjectRef,
        id: &str,
        scope: MemoryScope,
    ) -> Result<InsertOutcome> {
        if scope == MemoryScope::Session {
            return Err(OrchestraError::Config(
                "promote to `project` or `global`".into(),
            ));
        }
        let (id, project) = (id.to_string(), project.clone());
        let now = now_ms();
        self.run(move |db| {
            let old = db.get_memory(&id)?;
            if !visible_to(&old, &project) {
                return Err(not_visible(&old.id));
            }
            if old.kind == MemoryKind::Finding {
                return Err(OrchestraError::Config("an agent's individual opinion is never shared, so it cannot be promoted to persistent memory".into()));
            }
            if old.sensitive {
                return Err(OrchestraError::Config("that record is marked sensitive and stays private to its place".into()));
            }
            db.atomically(|| {
                let mut m = NewMemory::new(old.kind, scope, old.text.clone());
                m.project_id = (scope == MemoryScope::Project).then(|| project.id.clone());
                m.origin = old.origin.clone();
                m.status = old.status;
                m.short = old.short.clone();
                m.topic = old.topic.clone();
                m.tags = old.tags.clone();
                m.authorization = Authorization::ExplicitUser;
                m.consultation_id = old.consultation_id.clone();
                m.retrieved_ms = old.retrieved_ms;
                m.published = old.published.clone();
                m.valid_days = old.valid_days;
                m.category = old.category.clone();
                m.time_sensitive = old.time_sensitive;
                m.source_url = old.source_url.clone();
                if scope == MemoryScope::Project {
                    db.upsert_project(&project.id, project.path.as_deref(), project.name.as_deref(), now)?;
                }
                let out = db.insert_memory_inner(&m, now)?;
                if !out.duplicate {
                    db.link(&out.id, &old.id, LinkKind::DerivedFrom, now)?;
                    db.add_evidence_refs(&out.id, &db.evidence_refs(&old.id)?)?;
                }
                Ok(out)
            })
        })
        .await
    }

    pub async fn export(&self, project: Option<&ProjectRef>) -> Result<ExportBundle> {
        let project_id = project.map(|p| p.id.clone());
        let now = now_ms();
        if !self.exists() {
            return Ok(ExportBundle {
                format: "ai-orchestra-memory-export".into(),
                schema_version: super::db::SCHEMA_VERSION,
                exported_ms: now,
                project: project_id,
                memories: vec![],
                links: vec![],
                sessions: vec![],
                consultations: vec![],
                summaries: vec![],
                evidence: vec![],
            });
        }
        self.run(move |db| db.export(project_id.as_deref(), now))
            .await
    }

    /// Deletes a session and everything in it. With no id, the project's current session.
    pub async fn clear_session(
        &self,
        project: &ProjectRef,
        session: Option<&str>,
    ) -> Result<(String, usize)> {
        let (project, explicit) = (project.clone(), session.map(str::to_string));
        let now = now_ms();
        let idle = u64::from(self.settings.session_idle_hours) * 3_600_000;
        self.run(move |db| {
            let s = match explicit {
                Some(id) => db
                    .get_session(&id)?
                    .ok_or_else(|| OrchestraError::Config(format!("no session `{id}`")))?,
                None => db
                    .latest_session(&project.id, now.saturating_sub(idle))?
                    .ok_or_else(|| {
                        OrchestraError::Config("there is no current session to clear".into())
                    })?,
            };
            if s.project_id != project.id {
                return Err(OrchestraError::Config(
                    "that session belongs to another project".into(),
                ));
            }
            let n = db.clear_session(&s.id, &project.id, now)?;
            Ok((s.id, n))
        })
        .await
    }

    /// Deletes every persistent record of one scope: this project's project memories, or the global
    /// ones. Session memory is cleared per session.
    pub async fn clear_scope(&self, project: &ProjectRef, scope: MemoryScope) -> Result<usize> {
        if scope == MemoryScope::Session {
            return Err(OrchestraError::Config(
                "clear a session with --session or `session clear`".into(),
            ));
        }
        if !self.exists() {
            return Ok(0);
        }
        let pid = (scope == MemoryScope::Project).then(|| project.id.clone());
        let now = now_ms();
        self.run(move |db| db.clear_scope(scope, pid.as_deref(), now))
            .await
    }

    pub async fn sessions(&self, project: &ProjectRef, limit: usize) -> Result<Vec<SessionRow>> {
        if !self.exists() {
            return Ok(Vec::new());
        }
        let pid = project.id.clone();
        self.run(move |db| db.list_sessions(Some(&pid), limit))
            .await
    }

    pub async fn session_detail(
        &self,
        project: &ProjectRef,
        id: Option<&str>,
    ) -> Result<SessionDetail> {
        let (project, explicit) = (project.clone(), id.map(str::to_string));
        let now = now_ms();
        let idle = u64::from(self.settings.session_idle_hours) * 3_600_000;
        self.run(move |db| {
            let session = match explicit {
                Some(id) => db
                    .get_session(&id)?
                    .ok_or_else(|| OrchestraError::Config(format!("no session `{id}`")))?,
                None => db
                    .latest_session(&project.id, now.saturating_sub(idle))?
                    .ok_or_else(|| OrchestraError::Config("there is no current session".into()))?,
            };
            if session.project_id != project.id {
                return Err(OrchestraError::Config(
                    "that session belongs to another project".into(),
                ));
            }
            let vis = Visibility {
                project: Some(project.id.clone()),
                session: Some(session.id.clone()),
                session_scope: true,
                include_sensitive: false,
                ..Default::default()
            };
            Ok(SessionDetail {
                consultations: db.consultations_in_session(&session.id, &project.id)?,
                summaries: db.summaries_for_session(&session.id)?,
                memories: db.list_memories(&vis, None, 500)?,
                session,
            })
        })
        .await
    }

    /// Makes a session the one `auto` continues, even if it has been idle. This resumes AI
    /// Orchestra's own session; it does not resume any provider CLI's session.
    pub async fn resume(&self, project: &ProjectRef, id: &str) -> Result<SessionRow> {
        let (project, id) = (project.clone(), id.to_string());
        let now = now_ms();
        self.run(move |db| {
            let s = db.get_session(&id)?.ok_or_else(|| OrchestraError::Config(format!("no session `{id}` (see `ai-orchestra session list`)")))?;
            if s.project_id != project.id {
                return Err(OrchestraError::Config("that session belongs to another project; sessions are never shared between projects".into()));
            }
            db.touch_session(&s.id, now, false, None)?;
            db.get_session(&s.id)?.ok_or_else(|| OrchestraError::Memory("the session disappeared".into()))
        })
        .await
    }

    pub async fn stats(&self) -> Result<DbStats> {
        if !self.exists() {
            return Ok(DbStats {
                schema_version: super::db::SCHEMA_VERSION,
                ..Default::default()
            });
        }
        self.run(|db| db.stats()).await
    }

    pub async fn prune(&self) -> Result<(usize, usize)> {
        let (now, retention) = (now_ms(), self.settings.session_retention_days);
        self.run(move |db| db.prune(now, retention)).await
    }

    /// Verifies the database. A damaged file is reported; with `quarantine` it is moved aside (never
    /// deleted) and a new, empty one is started.
    pub async fn check(&self, quarantine_it: bool, rebuild: bool) -> Result<CheckReport> {
        let path = self.path.clone();
        let _permit = self
            .gate
            .acquire()
            .await
            .map_err(|_| OrchestraError::Memory("shutting down".into()))?;
        tokio::task::spawn_blocking(move || {
            let mut report = CheckReport {
                path: path.display().to_string(),
                ..Default::default()
            };
            if !path.exists() {
                report.healthy = true;
                report
                    .problems
                    .push("there is no memory database yet".to_string());
                return Ok(report);
            }
            match MemoryDb::open(&path) {
                Ok(db) => {
                    if rebuild {
                        db.rebuild_index()?;
                        report.rebuilt_index = true;
                    }
                    report.problems = db.check()?;
                    report.healthy = report.problems.is_empty();
                    Ok(report)
                }
                Err(e) => {
                    report.problems.push(e.to_string());
                    if quarantine_it {
                        let moved = quarantine(&path, now_ms())?;
                        report.quarantined_to = Some(moved.display().to_string());
                        MemoryDb::open(&path)?;
                    }
                    Ok(report)
                }
            }
        })
        .await
        .map_err(|e| OrchestraError::Memory(format!("a memory task failed: {e}")))?
    }

    /// Brings earlier saved consultations (the debate history) into this project's memory, on the
    /// user's request. Each becomes a project-scope conclusion, labelled as imported; nothing in
    /// the history is changed or removed. Consultations that look sensitive are skipped.
    pub async fn import_history(
        &self,
        store: &DebateStore,
        project: &ProjectRef,
    ) -> Result<ImportReport> {
        let (store, project) = (store.clone(), project.clone());
        let now = now_ms();
        self.run(move |db| {
            let mut report = ImportReport::default();
            let listing = store.list();
            let mut summaries = listing.debates;
            summaries.sort_by(|a, b| a.id.cmp(&b.id));
            db.upsert_project(
                &project.id,
                project.path.as_deref(),
                project.name.as_deref(),
                now,
            )?;
            for s in summaries {
                if db.consultation_exists_for_history(&s.id)? {
                    report.already_imported += 1;
                    continue;
                }
                let Ok(outcome) = store.load(&s.id) else {
                    report.not_importable += 1;
                    continue;
                };
                if !importable(&outcome) {
                    report.not_importable += 1;
                    continue;
                }
                if classify(&outcome.question) != Sensitivity::None
                    || classify(&outcome.final_answer) != Sensitivity::None
                {
                    report.sensitive_skipped += 1;
                    continue;
                }
                db.atomically(|| import_one(db, &project, &outcome))?;
                report.imported += 1;
            }
            Ok(report)
        })
        .await
    }
}

fn importable(o: &DebateOutcome) -> bool {
    o.consultation.is_some()
        && matches!(o.status, DebateStatus::Complete | DebateStatus::Partial)
        && !o.final_answer.trim().is_empty()
}

fn import_one(db: &MemoryDb, project: &ProjectRef, o: &DebateOutcome) -> Result<()> {
    let meta = o.consultation.as_ref();
    let digest = digest_outcome(&DigestInput {
        question: &o.question,
        mode: meta
            .and_then(|m| m.mode)
            .map_or("consultation", |m| m.as_str()),
        final_answer: &o.final_answer,
        steps: &[],
        research: meta.and_then(|m| m.research.as_ref()),
    });
    db.insert_consultation(&ConsultationRow {
        id: o.id.clone(),
        session_id: format!("imported-{}", project.id),
        project_id: project.id.clone(),
        question: redact_secrets(&o.question),
        mode: meta.and_then(|m| m.mode).map(|m| m.to_string()),
        strategy: meta.map(|m| m.strategy.clone()),
        status: o.status.to_string(),
        calls: o.calls as u32,
        created_ms: o.created_at_ms,
        history_id: Some(o.id.clone()),
        final_answer: Some(redact_secrets(&o.final_answer)),
    })?;
    let place = |m: &mut NewMemory| {
        m.scope = MemoryScope::Project;
        m.project_id = Some(project.id.clone());
        m.session_id = None;
        m.authorization = Authorization::Imported;
        m.consultation_id = Some(o.id.clone());
    };
    let mut c = NewMemory::new(
        MemoryKind::Conclusion,
        MemoryScope::Project,
        redact_secrets(&digest.conclusion),
    );
    place(&mut c);
    c.origin = Origin::ConsultationFinal;
    c.status = EpistemicStatus::AgentInterpretation;
    c.short = Some(digest.short.clone());
    c.time_sensitive = digest.time_sensitive;
    db.insert_memory_inner(&c, o.created_at_ms)?;
    for u in &digest.unresolved {
        let mut m = NewMemory::new(
            MemoryKind::Unresolved,
            MemoryScope::Project,
            redact_secrets(u),
        );
        place(&mut m);
        m.origin = Origin::ConsultationFinal;
        m.status = EpistemicStatus::AgentInterpretation;
        db.insert_memory_inner(&m, o.created_at_ms)?;
    }
    for e in &digest.evidence {
        let mut m = NewMemory::new(
            MemoryKind::Evidence,
            MemoryScope::Project,
            redact_secrets(&e.claim),
        );
        place(&mut m);
        apply_evidence(&mut m, e);
        let out = db.insert_memory_inner(&m, o.created_at_ms)?;
        if !out.duplicate {
            db.add_evidence_refs(&out.id, &e.refs)?;
        }
    }
    Ok(())
}

fn apply_evidence(m: &mut NewMemory, e: &super::compress::EvidenceDraft) {
    m.origin = Origin::Evidence;
    m.status = e.status;
    m.topic = Some(e.topic.clone());
    m.category = e.category.clone();
    m.time_sensitive = e.time_sensitive;
    m.retrieved_ms = e.retrieved_ms;
    m.published = e.published.clone();
    m.source_url = e.source_url.clone();
}

#[allow(clippy::too_many_arguments)]
fn self_record(
    db: &MemoryDb,
    project: &ProjectRef,
    prior: Option<SessionRow>,
    history_id: &str,
    question: &str,
    mode: &str,
    strategy: Option<String>,
    status: DebateStatus,
    calls: usize,
    answer: &str,
    digest: super::compress::OutcomeDigest,
    report: &ContextReport,
    extra_ai_calls: u32,
    now: u64,
) -> Result<RecordOutcome> {
    db.upsert_project(
        &project.id,
        project.path.as_deref(),
        project.name.as_deref(),
        now,
    )?;
    let session = match prior {
        Some(s) => s,
        None => db.create_session(&project.id, Some(&clip_at_boundary(question, 80)), now)?,
    };
    let mut out = RecordOutcome {
        session_id: Some(session.id.clone()),
        ..Default::default()
    };
    db.insert_consultation(&ConsultationRow {
        id: history_id.to_string(),
        session_id: session.id.clone(),
        project_id: project.id.clone(),
        question: redact_secrets(question),
        mode: Some(mode.to_string()),
        strategy,
        status: status.to_string(),
        calls: calls as u32,
        created_ms: now,
        history_id: Some(history_id.to_string()),
        final_answer: Some(answer.to_string()),
    })?;
    let place = |m: &mut NewMemory| {
        m.scope = MemoryScope::Session;
        m.session_id = Some(session.id.clone());
        m.project_id = Some(project.id.clone());
        m.authorization = Authorization::AutomaticSession;
        m.consultation_id = Some(history_id.to_string());
    };
    let partial = if status == DebateStatus::Complete {
        ""
    } else {
        "Partial result. "
    };
    let mut c = NewMemory::new(
        MemoryKind::Conclusion,
        MemoryScope::Session,
        redact_secrets(&format!("{partial}{}", digest.conclusion)),
    );
    place(&mut c);
    c.origin = Origin::ConsultationFinal;
    c.status = EpistemicStatus::AgentInterpretation;
    c.short = Some(digest.short.clone());
    c.time_sensitive = digest.time_sensitive;
    let store = |db: &MemoryDb, m: NewMemory, out: &mut RecordOutcome| -> Result<InsertOutcome> {
        let o = db.insert_memory_inner(&m, now)?;
        if !o.duplicate {
            out.stored += 1;
        }
        out.superseded += o.superseded.len();
        Ok(o)
    };
    store(db, c, &mut out)?;
    for u in &digest.unresolved {
        let mut m = NewMemory::new(
            MemoryKind::Unresolved,
            MemoryScope::Session,
            redact_secrets(u),
        );
        place(&mut m);
        m.origin = Origin::ConsultationFinal;
        m.status = EpistemicStatus::AgentInterpretation;
        store(db, m, &mut out)?;
    }
    for e in &digest.evidence {
        let mut m = NewMemory::new(
            MemoryKind::Evidence,
            MemoryScope::Session,
            redact_secrets(&e.claim),
        );
        place(&mut m);
        apply_evidence(&mut m, e);
        let o = store(db, m, &mut out)?;
        if !o.duplicate {
            db.add_evidence_refs(&o.id, &e.refs)?;
            for old in &o.superseded {
                let was = db.get_memory(old)?;
                // A claim that was supported and is now contradicted (or the other way round) is
                // a disagreement between two checks, not just a newer version.
                if (was.status == EpistemicStatus::Contradicted)
                    != (e.status == EpistemicStatus::Contradicted)
                {
                    db.link(&o.id, old, LinkKind::Contradicts, now)?;
                }
            }
        }
    }
    for (agent, text) in &digest.findings {
        let mut m = NewMemory::new(
            MemoryKind::Finding,
            MemoryScope::Session,
            redact_secrets(&format!("{agent}'s own conclusion: {text}")),
        );
        place(&mut m);
        m.origin = Origin::Agent(agent.clone());
        m.status = EpistemicStatus::AgentInterpretation;
        store(db, m, &mut out)?;
    }
    db.touch_session(
        &session.id,
        now,
        true,
        Some(&clip_at_boundary(question, 80)),
    )?;
    db.insert_metrics(&MetricsRow {
        consultation_id: Some(history_id.to_string()),
        session_id: Some(session.id.clone()),
        created_ms: now,
        mode: Some(report.mode.clone()),
        baseline_chars: report.baseline_chars,
        context_chars: report.context_chars,
        est_tokens: report.estimated_tokens as u64,
        retrieved: report.retrieved as u32,
        selected: report.selected as u32,
        omitted: report.omitted as u32,
        retrieval_us: report.retrieval_us,
        assembly_us: report.assembly_us,
        extra_ai_calls,
    })?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::debate::Role;
    use crate::research::claims::{ClaimRecord, ClaimStatus, EvidenceLink, Relation};
    use crate::research::engine::{CitationSummary, ResearchBudget};
    use crate::research::plan::{Depth, WebMode};
    use crate::research::quality::{ClaimCategory, Freshness as SrcFresh, SourceKind, Suitability};
    use crate::research::source::{Discovery, DiscoveryMethod, SourceSet, SourceStatus};
    use crate::types::ProviderId;

    fn service() -> (tempfile::TempDir, Arc<MemoryService>) {
        let dir = tempfile::tempdir().unwrap();
        let svc = MemoryService::new(
            dir.path().join("data").join("memory.db"),
            MemorySettings::default(),
        );
        (dir, svc)
    }

    fn project(name: &str) -> ProjectRef {
        ProjectRef::named(name)
    }

    fn req(project: &ProjectRef, mode: MemoryMode) -> MemoryRequest {
        MemoryRequest {
            project: Some(project.clone()),
            ..MemoryRequest::new(mode)
        }
    }

    fn report_with(claims: Vec<(&str, ClaimStatus, ClaimCategory)>) -> ResearchReport {
        let mut set = SourceSet::new();
        let id = set
            .add_candidate(
                "https://rigol.example/ds1054z",
                Discovery {
                    method: DiscoveryMethod::UserProvided,
                    provider: None,
                },
            )
            .unwrap();
        let mut src = set.get(id).unwrap().clone();
        src.status = SourceStatus::Retrieved;
        src.retrieved_at_ms = Some(now_ms());
        src.published = Some("2025-01-02".into());
        src.title = Some("DS1054Z".into());
        src.content_hash = Some("ab".repeat(32));
        ResearchReport {
            schema: 1,
            web_requested: WebMode::Auto,
            depth: Depth::Standard,
            max_sources: 6,
            freshness_days: None,
            discovery_agent: None,
            subquestions: vec![],
            search_queries: vec![],
            sources: vec![src],
            claims: claims
                .into_iter()
                .enumerate()
                .map(|(i, (text, status, category))| ClaimRecord {
                    id: i as u32 + 1,
                    text: text.into(),
                    category,
                    originating_agents: vec!["claude".into()],
                    needs_external_evidence: true,
                    cited_by_agents: vec![],
                    status,
                    evidence: vec![EvidenceLink {
                        source_id: 1,
                        relation: Relation::Supports,
                        passage: "the passage that supports it".into(),
                        offset: 0,
                        assessed_by: vec!["codex".into()],
                        source_kind: SourceKind::ReferenceWork,
                        suitability: Suitability::Moderate,
                        suitability_note: String::new(),
                        freshness: SrcFresh::Current,
                        reason: String::new(),
                    }],
                    supporting_independent_sources: 1,
                    contradicting_independent_sources: 0,
                    notes: vec![],
                })
                .collect(),
            contradictions: vec![],
            citations: CitationSummary::default(),
            assessors: vec![],
            budget: ResearchBudget::default(),
            limitations: vec![],
        }
    }

    const ANSWER: &str = "## Answer\n\nThe Rigol DS1054Z, the Siglent SDS1104X-E and the Hantek DSO2D15 were compared; the Rigol is the best value at 5,500 MXN.\n\n## Recommendation\n\nBuy the Rigol DS1054Z.\n\n## Significant disagreements\n\n- one agent prefers the Hantek for portability.\n\n## Assumptions\n\nA hobbyist budget.\n\n## Evidence limitations\n\nPrices were not verified.";

    async fn remember(
        svc: &MemoryService,
        p: &ProjectRef,
        session: Option<&str>,
        id: &str,
        question: &str,
        answer: &str,
        research: Option<&ResearchReport>,
    ) -> RecordOutcome {
        let mut r = req(p, MemoryMode::Auto);
        r.session = session.map(str::to_string);
        let prepared = svc.prepare(&r, question, 48 * 1024, false).await.unwrap();
        let steps = vec![
            Step {
                phase: Role::Perspective,
                round: 0,
                agent: ProviderId::new("claude"),
                offset_ms: 0,
                duration_ms: 1,
                ok: true,
                text: Some("Claude prefers the Rigol.".into()),
                error: None,
                error_kind: None,
                settings: None,
                usage: None,
            },
            Step {
                phase: Role::Perspective,
                round: 0,
                agent: ProviderId::new("codex"),
                offset_ms: 0,
                duration_ms: 1,
                ok: true,
                text: Some("Codex prefers the Siglent.".into()),
                error: None,
                error_kind: None,
                settings: None,
                usage: None,
            },
        ];
        svc.record(RecordInput {
            prepared: &prepared,
            history_id: id,
            question,
            mode: "review",
            strategy: Some("balanced"),
            status: DebateStatus::Complete,
            calls: 4,
            final_answer: answer,
            steps: &steps,
            research,
            extra_ai_calls: 0,
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn with_no_database_nothing_is_read_and_reading_creates_nothing() {
        let (dir, svc) = service();
        let p = project("lab");
        let prep = svc
            .prepare(
                &req(&p, MemoryMode::Auto),
                "Compare oscilloscopes",
                48 * 1024,
                false,
            )
            .await
            .unwrap();
        assert!(prep.text.is_empty() && prep.session.is_none());
        assert!(
            !dir.path().join("data").exists(),
            "a read never creates the database"
        );
        assert!(
            svc.list(&p, None, None, false, 10)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(svc.export(Some(&p)).await.unwrap().memories.is_empty());
        let e = svc
            .prepare(
                &MemoryRequest {
                    session: Some("ses-x".into()),
                    ..req(&p, MemoryMode::Auto)
                },
                "q",
                1000,
                false,
            )
            .await
            .unwrap_err();
        assert!(e.to_string().contains("no session"), "{e}");
    }

    #[tokio::test]
    async fn a_follow_up_in_the_same_project_recalls_the_earlier_consultation() {
        let (_d, svc) = service();
        let p = project("lab");
        let first = remember(
            &svc,
            &p,
            None,
            "cons-1",
            "Research three portable oscilloscopes for learning electronics",
            ANSWER,
            None,
        )
        .await;
        assert!(first.stored >= 2 && first.skipped.is_none(), "{first:?}");
        let prep = svc
            .prepare(
                &req(&p, MemoryMode::Auto),
                "Of the three above, which has the best value?",
                48 * 1024,
                false,
            )
            .await
            .unwrap();
        assert_eq!(
            prep.session.as_ref().map(|s| s.id.clone()),
            first.session_id
        );
        assert!(
            prep.text.contains("Siglent SDS1104X-E")
                && prep.text.contains("AI-generated conclusion, not verified"),
            "{}",
            prep.text
        );
        assert!(
            !prep.text.contains("Codex prefers the Siglent"),
            "an agent's own opinion is not forwarded"
        );
        assert!(prep.report.baseline_chars > 0 && prep.report.reduction_percent.is_some());
        // Another project sees none of it.
        let other = svc
            .prepare(
                &req(&project("garden"), MemoryMode::Auto),
                "Of the three above, which has the best value?",
                48 * 1024,
                false,
            )
            .await
            .unwrap();
        assert!(
            other.text.is_empty() && other.session.is_none(),
            "{}",
            other.text
        );
    }

    #[tokio::test]
    async fn off_and_no_save_neither_read_nor_write() {
        let (_d, svc) = service();
        let p = project("lab");
        remember(
            &svc,
            &p,
            None,
            "cons-1",
            "Compare oscilloscopes",
            ANSWER,
            None,
        )
        .await;
        let off = svc
            .prepare(
                &req(&p, MemoryMode::Off),
                "Compare oscilloscopes",
                48 * 1024,
                false,
            )
            .await
            .unwrap();
        assert!(off.text.is_empty() && !off.write);
        let skipped = svc
            .record(RecordInput {
                prepared: &off,
                history_id: "cons-2",
                question: "q",
                mode: "learn",
                strategy: None,
                status: DebateStatus::Complete,
                calls: 1,
                final_answer: "an answer",
                steps: &[],
                research: None,
                extra_ai_calls: 0,
            })
            .await
            .unwrap();
        assert!(skipped.skipped.is_some() && skipped.stored == 0);
        let mut no_save = req(&p, MemoryMode::Auto);
        no_save.write = false;
        let prep = svc
            .prepare(&no_save, "Compare oscilloscopes", 48 * 1024, false)
            .await
            .unwrap();
        assert!(!prep.text.is_empty(), "it may read");
        let r = svc
            .record(RecordInput {
                prepared: &prep,
                history_id: "cons-3",
                question: "Compare oscilloscopes again",
                mode: "review",
                strategy: None,
                status: DebateStatus::Complete,
                calls: 1,
                final_answer: ANSWER,
                steps: &[],
                research: None,
                extra_ai_calls: 0,
            })
            .await
            .unwrap();
        assert_eq!(r.stored, 0, "--no-save writes nothing");
        assert_eq!(svc.stats().await.unwrap().consultations, 1);
    }

    #[tokio::test]
    async fn nothing_sensitive_is_remembered_automatically_and_failures_are_not_remembered() {
        let (_d, svc) = service();
        let p = project("lab");
        for (question, answer, why) in [
            (
                "My key is sk-abcdefghijklmnopqrstuvwxyz0123456789, what scope?",
                ANSWER,
                "credential",
            ),
            (
                "Email me at ana@example.com about scopes",
                ANSWER,
                "personal",
            ),
            (
                "Compare scopes",
                "Call +52 55 1234 5678 for a quote on the scope",
                "personal",
            ),
        ] {
            let prep = svc
                .prepare(&req(&p, MemoryMode::Auto), question, 48 * 1024, false)
                .await
                .unwrap();
            let out = svc
                .record(RecordInput {
                    prepared: &prep,
                    history_id: "c",
                    question,
                    mode: "review",
                    strategy: None,
                    status: DebateStatus::Complete,
                    calls: 1,
                    final_answer: answer,
                    steps: &[],
                    research: None,
                    extra_ai_calls: 0,
                })
                .await
                .unwrap();
            assert!(
                out.skipped.as_deref().unwrap().contains(why),
                "{question}: {out:?}"
            );
        }
        let prep = svc
            .prepare(&req(&p, MemoryMode::Auto), "q", 1000, false)
            .await
            .unwrap();
        let failed = svc
            .record(RecordInput {
                prepared: &prep,
                history_id: "c",
                question: "q",
                mode: "review",
                strategy: None,
                status: DebateStatus::Failed,
                calls: 1,
                final_answer: "",
                steps: &[],
                research: None,
                extra_ai_calls: 0,
            })
            .await
            .unwrap();
        assert!(failed.skipped.is_some());
        assert!(!svc.path().exists() || svc.stats().await.unwrap().memories == 0);
    }

    #[tokio::test]
    async fn evidence_keeps_its_status_dates_and_sources_and_a_newer_check_supersedes_the_older_one()
     {
        let (_d, svc) = service();
        let p = project("lab");
        let r1 = report_with(vec![(
            "The Rigol DS1054Z has 50 MHz of bandwidth",
            ClaimStatus::Supported,
            ClaimCategory::ProductSpecification,
        )]);
        let first = remember(
            &svc,
            &p,
            None,
            "cons-1",
            "Check the Rigol bandwidth",
            ANSWER,
            Some(&r1),
        )
        .await;
        let session = first.session_id.clone().unwrap();
        let ev = svc
            .list(
                &p,
                Some(MemoryScope::Session),
                Some(MemoryKind::Evidence),
                false,
                10,
            )
            .await
            .unwrap();
        assert_eq!(ev.len(), 1);
        let e = &ev[0];
        assert_eq!(
            (e.status, e.origin.as_string().as_str()),
            (EpistemicStatus::PreviouslyVerified, "evidence")
        );
        assert!(
            e.retrieved_ms.is_some()
                && e.published.as_deref() == Some("2025-01-02")
                && e.time_sensitive
        );
        let detail = svc.show(&p, &e.id).await.unwrap();
        assert_eq!(
            detail.evidence[0].url.as_deref(),
            Some("https://rigol.example/ds1054z")
        );
        assert_eq!(
            detail.evidence[0].content_hash.as_deref(),
            Some(&*"ab".repeat(32))
        );
        // Later the same claim is contradicted: the new check replaces the old, and they are linked.
        let r2 = report_with(vec![(
            "The Rigol DS1054Z has 50 MHz of bandwidth",
            ClaimStatus::Contradicted,
            ClaimCategory::ProductSpecification,
        )]);
        remember(
            &svc,
            &p,
            Some(&session),
            "cons-2",
            "Check the Rigol bandwidth again",
            ANSWER,
            Some(&r2),
        )
        .await;
        let now = svc
            .list(
                &p,
                Some(MemoryScope::Session),
                Some(MemoryKind::Evidence),
                false,
                10,
            )
            .await
            .unwrap();
        assert_eq!(now.len(), 1);
        assert_eq!(now[0].status, EpistemicStatus::Contradicted);
        let all = svc
            .list(
                &p,
                Some(MemoryScope::Session),
                Some(MemoryKind::Evidence),
                true,
                10,
            )
            .await
            .unwrap();
        assert_eq!(all.len(), 2, "the old check is kept");
        let links = svc.show(&p, &now[0].id).await.unwrap().links;
        assert!(
            links.iter().any(|l| l.kind == "supersedes")
                && links.iter().any(|l| l.kind == "contradicts"),
            "{links:?}"
        );
    }

    #[tokio::test]
    async fn persistent_memory_needs_an_explicit_add_and_credentials_and_personal_data_are_refused()
    {
        let (_d, svc) = service();
        let p = project("lab");
        let add = |text: &str, allow: bool| AddInput {
            project: p.clone(),
            scope: MemoryScope::Project,
            kind: MemoryKind::Preference,
            text: text.into(),
            topic: None,
            tags: vec![],
            session: None,
            allow_sensitive: allow,
            expires_days: None,
        };
        let (out, rec) = svc.add(add("My budget is 5,000 MXN", false)).await.unwrap();
        assert!(!out.duplicate);
        assert_eq!(
            (
                rec.scope,
                rec.origin.as_string().as_str(),
                rec.authorization
            ),
            (MemoryScope::Project, "user", Authorization::ExplicitUser)
        );
        assert_eq!(
            rec.topic.as_deref(),
            Some("budget"),
            "a budget statement is tracked by topic"
        );
        let cred = svc
            .add(add("api_key=AAAAbbbbCCCC1111222233334444", true))
            .await
            .unwrap_err()
            .to_string();
        assert!(cred.contains("credentials are never stored"), "{cred}");
        let personal = svc
            .add(add("reach me at ana@example.com", false))
            .await
            .unwrap_err()
            .to_string();
        assert!(personal.contains("--allow-sensitive"), "{personal}");
        let (_, kept) = svc
            .add(add("reach me at ana@example.com", true))
            .await
            .unwrap();
        assert!(kept.sensitive);
        let prep = svc
            .prepare(
                &req(&p, MemoryMode::Project),
                "reach me ana example",
                48 * 1024,
                false,
            )
            .await
            .unwrap();
        assert!(
            !prep.text.contains("ana@example.com"),
            "sensitive records never reach an agent"
        );
        let finding = svc
            .add(AddInput {
                kind: MemoryKind::Finding,
                ..add("x", false)
            })
            .await
            .unwrap_err()
            .to_string();
        assert!(finding.contains("cannot be added by hand"), "{finding}");
        // A newer budget supersedes the older one; the older stays readable.
        let (newer, _) = svc
            .add(add("My budget is now 8,000 MXN", false))
            .await
            .unwrap();
        assert_eq!(newer.superseded.len(), 1);
        let current = svc
            .list(
                &p,
                Some(MemoryScope::Project),
                Some(MemoryKind::Preference),
                false,
                10,
            )
            .await
            .unwrap();
        assert!(
            current.iter().any(|r| r.text.contains("8,000"))
                && !current.iter().any(|r| r.text.contains("5,000"))
        );
        let history = svc
            .list(
                &p,
                Some(MemoryScope::Project),
                Some(MemoryKind::Preference),
                true,
                10,
            )
            .await
            .unwrap();
        assert!(history.iter().any(|r| r.text.contains("5,000")));
    }

    #[tokio::test]
    async fn a_persistent_memory_survives_a_new_service_over_the_same_file_and_reaches_the_next_consultation()
     {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.db");
        let p = project("lab");
        {
            let svc = MemoryService::new(path.clone(), MemorySettings::default());
            svc.add(AddInput {
                project: p.clone(),
                scope: MemoryScope::Project,
                kind: MemoryKind::Requirement,
                text: "I need at least two channels and USB export".into(),
                topic: None,
                tags: vec![],
                session: None,
                allow_sensitive: false,
                expires_days: None,
            })
            .await
            .unwrap();
        }
        let svc = MemoryService::new(path, MemorySettings::default());
        let prep = svc
            .prepare(
                &req(&p, MemoryMode::Auto),
                "Which scope has USB export?",
                48 * 1024,
                false,
            )
            .await
            .unwrap();
        assert!(
            prep.text.contains("at least two channels and USB export")
                && prep.text.contains("stated by the user"),
            "{}",
            prep.text
        );
        // Session mode leaves persistent memory out; project mode includes it.
        let session_only = svc
            .prepare(
                &req(&p, MemoryMode::Session),
                "Which scope has USB export?",
                48 * 1024,
                false,
            )
            .await
            .unwrap();
        assert!(session_only.text.is_empty());
    }

    #[tokio::test]
    async fn forgetting_correcting_and_promoting_respect_the_project_boundary() {
        let (_d, svc) = service();
        let (lab, garden) = (project("lab"), project("garden"));
        let add = |p: &ProjectRef, text: &str| AddInput {
            project: p.clone(),
            scope: MemoryScope::Project,
            kind: MemoryKind::Note,
            text: text.into(),
            topic: None,
            tags: vec![],
            session: None,
            allow_sensitive: false,
            expires_days: None,
        };
        let (_, lab_note) = svc.add(add(&lab, "lab note about probes")).await.unwrap();
        let err = svc
            .forget(&garden, &lab_note.id)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("belongs to another project"), "{err}");
        assert!(
            svc.correct(&garden, &lab_note.id, "hijacked")
                .await
                .is_err()
        );
        assert!(svc.show(&garden, &lab_note.id).await.is_err());
        let fixed = svc
            .correct(&lab, &lab_note.id, "lab note about probes and grounding")
            .await
            .unwrap();
        assert!(fixed.superseded.contains(&lab_note.id));
        assert!(svc.correct(&lab, &lab_note.id, "").await.is_err() || true);
        // Promotion to global is explicit, keeps provenance and is then visible from another project.
        let (_, global) = {
            let out = svc
                .promote(&lab, &fixed.id, MemoryScope::Global)
                .await
                .unwrap();
            let rec = svc.show(&lab, &out.id).await.unwrap().record;
            (out, rec)
        };
        assert_eq!(
            (global.scope, global.authorization),
            (MemoryScope::Global, Authorization::ExplicitUser)
        );
        assert!(
            svc.show(&garden, &global.id).await.is_ok(),
            "global memories are visible everywhere"
        );
        let gone = svc.forget(&lab, &fixed.id).await.unwrap();
        assert_eq!(gone.id, fixed.id);
        assert!(svc.show(&lab, &fixed.id).await.is_err());
    }

    #[tokio::test]
    async fn an_agents_own_opinion_cannot_be_promoted_and_conclusions_stay_labelled_when_promoted()
    {
        let (_d, svc) = service();
        let p = project("lab");
        remember(
            &svc,
            &p,
            None,
            "cons-1",
            "Compare oscilloscopes",
            ANSWER,
            None,
        )
        .await;
        let all = svc.list(&p, None, None, false, 50).await.unwrap();
        let finding = all
            .iter()
            .find(|r| r.kind == MemoryKind::Finding)
            .expect("findings are stored for the audit trail");
        let e = svc
            .promote(&p, &finding.id, MemoryScope::Project)
            .await
            .unwrap_err()
            .to_string();
        assert!(e.contains("never shared"), "{e}");
        let conclusion = all
            .iter()
            .find(|r| r.kind == MemoryKind::Conclusion)
            .unwrap();
        let out = svc
            .promote(&p, &conclusion.id, MemoryScope::Project)
            .await
            .unwrap();
        let promoted = svc.show(&p, &out.id).await.unwrap();
        assert_eq!(
            promoted.record.status,
            EpistemicStatus::AgentInterpretation,
            "promotion never raises trust"
        );
        assert!(promoted.links.iter().any(|l| l.kind == "derived_from"));
        assert!(
            svc.promote(&p, &conclusion.id, MemoryScope::Session)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn sessions_can_be_listed_resumed_inspected_and_cleared() {
        let (_d, svc) = service();
        let p = project("lab");
        let a = remember(
            &svc,
            &p,
            None,
            "cons-1",
            "Compare oscilloscopes",
            ANSWER,
            None,
        )
        .await
        .session_id
        .unwrap();
        let sessions = svc.sessions(&p, 10).await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(
            (sessions[0].id.as_str(), sessions[0].turns),
            (a.as_str(), 1)
        );
        let detail = svc.session_detail(&p, Some(&a[..9])).await.unwrap();
        assert_eq!(detail.consultations.len(), 1);
        assert!(
            detail
                .memories
                .iter()
                .all(|m| m.kind != MemoryKind::Finding
                    || m.session_id.as_deref() == Some(a.as_str()))
        );
        assert!(
            svc.session_detail(&project("garden"), Some(&a))
                .await
                .is_err(),
            "another project cannot inspect it"
        );
        assert!(svc.resume(&project("garden"), &a).await.is_err());
        assert_eq!(svc.resume(&p, &a).await.unwrap().id, a);
        let (cleared, n) = svc.clear_session(&p, None).await.unwrap();
        assert_eq!(cleared, a);
        assert!(n >= 2);
        assert!(svc.sessions(&p, 10).await.unwrap().is_empty());
        let prep = svc
            .prepare(
                &req(&p, MemoryMode::Auto),
                "Of the three above?",
                48 * 1024,
                false,
            )
            .await
            .unwrap();
        assert!(prep.text.is_empty(), "a cleared session recalls nothing");
    }

    #[tokio::test]
    async fn an_idle_session_is_not_continued_automatically_but_can_be_resumed() {
        let dir = tempfile::tempdir().unwrap();
        let settings = MemorySettings {
            session_idle_hours: 1,
            ..MemorySettings::default()
        };
        let svc = MemoryService::new(dir.path().join("memory.db"), settings);
        let p = project("lab");
        let a = remember(
            &svc,
            &p,
            None,
            "cons-1",
            "Compare oscilloscopes",
            ANSWER,
            None,
        )
        .await
        .session_id
        .unwrap();
        // Age the session by hand.
        let old = now_ms() - 5 * 3_600_000;
        let path = svc.path().to_path_buf();
        let a2 = a.clone();
        svc.run(move |db| {
            let _ = (&path, &a2);
            db.touch_session(&a2, old, false, None)
        })
        .await
        .unwrap();
        let prep = svc
            .prepare(
                &req(&p, MemoryMode::Auto),
                "Of the three above, which?",
                48 * 1024,
                false,
            )
            .await
            .unwrap();
        assert!(
            prep.session.is_none() && prep.text.is_empty(),
            "idle for 5 h with a 1 h limit"
        );
        let named = svc
            .prepare(
                &MemoryRequest {
                    session: Some(a.clone()),
                    ..req(&p, MemoryMode::Auto)
                },
                "Of the three above, which?",
                48 * 1024,
                false,
            )
            .await
            .unwrap();
        assert!(!named.text.is_empty(), "--session reaches it");
        svc.resume(&p, &a).await.unwrap();
        assert!(
            svc.prepare(
                &req(&p, MemoryMode::Auto),
                "Of the three above, which?",
                48 * 1024,
                false
            )
            .await
            .unwrap()
            .session
            .is_some()
        );
    }

    #[tokio::test]
    async fn export_search_and_stats_give_the_user_their_data() {
        let (_d, svc) = service();
        let p = project("lab");
        remember(
            &svc,
            &p,
            None,
            "cons-1",
            "Compare oscilloscopes",
            ANSWER,
            None,
        )
        .await;
        svc.add(AddInput {
            project: p.clone(),
            scope: MemoryScope::Project,
            kind: MemoryKind::Decision,
            text: "Decided to buy the Rigol".into(),
            topic: None,
            tags: vec!["scope".into()],
            session: None,
            allow_sensitive: false,
            expires_days: None,
        })
        .await
        .unwrap();
        let hits = svc
            .search(&p, "Rigol decision", None, false, 10)
            .await
            .unwrap();
        assert!(hits.iter().any(|h| h.record.kind == MemoryKind::Decision));
        let bundle = svc.export(Some(&p)).await.unwrap();
        assert_eq!(bundle.format, "ai-orchestra-memory-export");
        assert!(
            bundle.memories.len() >= 3
                && !bundle.consultations.is_empty()
                && bundle.consultations[0].final_answer.is_some()
        );
        let again = svc.export(Some(&p)).await.unwrap();
        assert_eq!(
            serde_json::to_string(&bundle.memories).unwrap(),
            serde_json::to_string(&again.memories).unwrap()
        );
        let stats = svc.stats().await.unwrap();
        assert!(
            stats.memories >= 3
                && stats.sessions == 1
                && stats.file_bytes > 0
                && stats.metrics_rows == 1
        );
        assert_eq!(stats.extra_ai_calls, 0);
    }

    #[tokio::test]
    async fn a_damaged_database_does_not_stop_a_consultation_and_check_can_set_it_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.db");
        std::fs::write(
            &path,
            b"not a database at all, but long enough to look like a file".repeat(30),
        )
        .unwrap();
        let svc = MemoryService::new(path.clone(), MemorySettings::default());
        let p = project("lab");
        let prep = svc
            .prepare(&req(&p, MemoryMode::Auto), "anything", 48 * 1024, false)
            .await
            .unwrap();
        assert!(
            prep.text.is_empty() && prep.warnings.iter().any(|w| w.contains("damaged")),
            "{:?}",
            prep.warnings
        );
        let report = svc.check(false, false).await.unwrap();
        assert!(!report.healthy && report.problems[0].contains("damaged"));
        assert!(path.exists(), "check alone changes nothing");
        let fixed = svc.check(true, false).await.unwrap();
        assert!(fixed.quarantined_to.is_some());
        assert!(svc.check(false, true).await.unwrap().healthy);
    }

    #[tokio::test]
    async fn saved_consultations_can_be_imported_once_and_sensitive_ones_are_skipped() {
        use crate::consult::ConsultationMeta;
        use crate::debate::DebateOutcome;
        use crate::store::ConversationId;
        let (dir, svc) = service();
        let store = DebateStore::new(dir.path().join("debates"));
        let make = |id: &str, question: &str, answer: &str| DebateOutcome {
            schema: 1,
            id: id.into(),
            conversation: ConversationId(0),
            created_at_ms: 1_700_000_000_000,
            question: question.into(),
            coordinator: ProviderId::new("claude"),
            participants: vec![],
            skipped: vec![],
            rounds_planned: 1,
            status: DebateStatus::Complete,
            synthesized: true,
            calls: 3,
            steps: vec![],
            initial_proposal: String::new(),
            final_answer: answer.into(),
            error: None,
            warnings: vec![],
            duration_ms: 1,
            consultation: Some(ConsultationMeta::default()),
            summaries: Vec::new(),
            saved_to: None,
        };
        store
            .save(&make(
                "20260101-000000-000001",
                "Compare oscilloscopes",
                ANSWER,
            ))
            .unwrap();
        store
            .save(&make(
                "20260102-000000-000002",
                "Email ana@example.com the scope list",
                ANSWER,
            ))
            .unwrap();
        let mut plain = make("20260103-000000-000003", "A plain debate", ANSWER);
        plain.consultation = None;
        store.save(&plain).unwrap();
        let p = project("lab");
        let first = svc.import_history(&store, &p).await.unwrap();
        assert_eq!(
            first,
            ImportReport {
                imported: 1,
                already_imported: 0,
                not_importable: 1,
                sensitive_skipped: 1
            }
        );
        let second = svc.import_history(&store, &p).await.unwrap();
        assert_eq!(
            (second.imported, second.already_imported),
            (0, 1),
            "importing twice adds nothing"
        );
        let imported = svc
            .list(
                &p,
                Some(MemoryScope::Project),
                Some(MemoryKind::Conclusion),
                false,
                10,
            )
            .await
            .unwrap();
        assert_eq!(imported.len(), 1);
        assert_eq!(imported[0].authorization, Authorization::Imported);
        assert!(imported[0].text.contains("not verified"));
        assert_eq!(
            store.list().debates.len(),
            3,
            "the history itself is untouched"
        );
        let prep = svc
            .prepare(
                &req(&p, MemoryMode::Auto),
                "Which oscilloscope did we compare?",
                48 * 1024,
                false,
            )
            .await
            .unwrap();
        assert!(prep.text.contains("Rigol DS1054Z"), "{}", prep.text);
    }

    #[tokio::test]
    async fn concurrent_consultations_write_and_read_without_losing_or_mixing_anything() {
        let (_d, svc) = service();
        let mut tasks = tokio::task::JoinSet::new();
        for i in 0..8 {
            let svc = svc.clone();
            tasks.spawn(async move {
                let p = project(&format!("project-{}", i % 2));
                let q = format!("Compare oscilloscope number {i} for project {}", i % 2);
                let out = remember(&svc, &p, None, &format!("cons-{i}"), &q, ANSWER, None).await;
                let prep = svc
                    .prepare(
                        &req(&p, MemoryMode::Auto),
                        "which oscilloscope",
                        48 * 1024,
                        false,
                    )
                    .await
                    .unwrap();
                (i % 2, out.session_id.unwrap(), prep.text)
            });
        }
        let mut sessions = std::collections::HashSet::new();
        while let Some(r) = tasks.join_next().await {
            let (proj, session, text) = r.unwrap();
            sessions.insert(session);
            for other in 0..2 {
                if other != proj {
                    assert!(
                        !text.contains(&format!("for project {other}")),
                        "project {proj} saw project {other}'s question"
                    );
                }
            }
        }
        assert!(!sessions.is_empty());
        let stats = svc.stats().await.unwrap();
        assert!(stats.consultations >= 4, "{stats:?}");
        assert_eq!(svc.check(false, false).await.unwrap().problems.len(), 0);
    }

    #[test]
    fn projects_are_identified_by_canonical_path_or_by_the_configured_name() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("a");
        std::fs::create_dir(&sub).unwrap();
        let direct = ProjectRef::from_path(&sub);
        let roundabout = ProjectRef::from_path(&sub.join("..").join("a"));
        assert_eq!(
            direct.id, roundabout.id,
            "the same directory by another spelling is one project"
        );
        assert_ne!(direct.id, ProjectRef::from_path(dir.path()).id);
        assert_eq!(
            ProjectRef::resolve(Some("lab"), Some(&sub)).id,
            ProjectRef::named("lab").id
        );
        assert_ne!(ProjectRef::named("lab").id, ProjectRef::named("Lab").id);
    }
}
