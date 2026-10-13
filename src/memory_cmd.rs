//! `ai-orchestra memory ...` and `ai-orchestra session ...`: seeing, searching, adding, correcting,
//! exporting and deleting what AI Orchestra remembers. Nothing here calls an agent except
//! `memory summarize`, which makes exactly one call to the agent you name.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ai_orchestra::Config;
use ai_orchestra::error::OrchestraError;
use ai_orchestra::history::{DebateStore, format_utc, now_ms};
use ai_orchestra::memory::continuation;
use ai_orchestra::memory::freshness::assess;
use ai_orchestra::memory::model::{MemoryKind, MemoryRecord, MemoryScope};
use ai_orchestra::memory::service::{AddInput, MemoryService, MemorySettings, ProjectRef};
use clap::{Subcommand, ValueEnum};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Fmt {
    Text,
    Json,
}

#[derive(Subcommand)]
pub enum MemoryCommand {
    /// List what is remembered for this project (newest first)
    List {
        /// session, project or global
        #[arg(long)]
        scope: Option<String>,
        /// question, conclusion, finding, unresolved, evidence, preference, decision, requirement, interest or note
        #[arg(long)]
        kind: Option<String>,
        /// Also show records that a newer one replaced
        #[arg(long)]
        all_versions: bool,
        #[arg(long, default_value_t = 30)]
        limit: usize,
        #[arg(long, value_enum, default_value = "text")]
        output: Fmt,
    },
    /// Full-text search of what is remembered
    Search {
        query: String,
        #[arg(long)]
        scope: Option<String>,
        #[arg(long)]
        all_versions: bool,
        #[arg(long, default_value_t = 10)]
        limit: usize,
        #[arg(long, value_enum, default_value = "text")]
        output: Fmt,
    },
    /// Show one memory with its origin, status, dates, links and evidence
    Show {
        /// A memory id or a unique prefix of it
        id: String,
        /// For a conclusion: also print the saved full answer it came from
        #[arg(long)]
        original: bool,
        #[arg(long, value_enum, default_value = "text")]
        output: Fmt,
    },
    /// Remember something on purpose (this is the authorization to keep it)
    Add {
        text: String,
        /// project (default), global or session
        #[arg(long, default_value = "project")]
        scope: String,
        /// preference, decision, requirement, interest or note (default)
        #[arg(long, default_value = "note")]
        kind: String,
        /// A short name for what this is about; a newer statement on the same topic replaces the older
        #[arg(long)]
        topic: Option<String>,
        #[arg(long = "tag")]
        tags: Vec<String>,
        /// For --scope session: which session (default: the current one)
        #[arg(long)]
        session: Option<String>,
        /// Allow text that looks like contact details or a payment number (kept private, never sent to an agent)
        #[arg(long)]
        allow_sensitive: bool,
        /// Forget it automatically after this many days
        #[arg(long)]
        expires_days: Option<u32>,
    },
    /// Delete one memory for good
    Forget { id: String },
    /// Replace a memory by a corrected version (the old one is kept, marked as replaced)
    Correct { id: String, text: String },
    /// Copy a memory to project or global memory (a persistent copy; trust is not raised)
    Promote {
        id: String,
        #[arg(long, default_value = "project")]
        scope: String,
    },
    /// Write everything remembered for this project (and global) as JSON
    Export {
        /// Every project, not only this one
        #[arg(long)]
        all_projects: bool,
        /// Write to this file (owner-only) instead of stdout
        #[arg(long, value_name = "FILE")]
        file: Option<PathBuf>,
    },
    /// Delete a whole scope: the current session (default), or all project or global memories with --yes
    Clear {
        #[arg(long, default_value = "session")]
        scope: String,
        #[arg(long)]
        session: Option<String>,
        /// Required for --scope project or global
        #[arg(long)]
        yes: bool,
    },
    /// What is stored, how big it is, and what it saved or cost
    Stats {
        #[arg(long, value_enum, default_value = "text")]
        output: Fmt,
    },
    /// Where it lives, how it is configured, and what is and is not done about provider sessions
    Status {
        #[arg(long, value_enum, default_value = "text")]
        output: Fmt,
    },
    /// Verify the database; --quarantine sets a damaged file aside (never deletes it)
    Check {
        #[arg(long)]
        quarantine: bool,
        /// Rebuild the full-text index first
        #[arg(long)]
        rebuild: bool,
    },
    /// Remove what has expired
    Prune,
    /// Bring earlier saved consultations (the debate history) into this project's memory
    ImportHistory,
    /// Ask ONE agent (one call) to write a short summary of a session; stored labelled as AI-written
    Summarize {
        /// The agent that writes it: claude, codex or google
        #[arg(long)]
        agent: String,
        #[arg(long)]
        session: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum SessionCommand {
    /// List the sessions of this project
    List {
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long, value_enum, default_value = "text")]
        output: Fmt,
    },
    /// Show a session (default: the current one): its consultations and memories
    Show {
        id: Option<String>,
        #[arg(long, value_enum, default_value = "text")]
        output: Fmt,
    },
    /// Continue an AI Orchestra session with the next consultations (this does not resume any provider CLI session)
    Resume { id: String },
    /// Delete a session and everything in it
    Clear { id: Option<String> },
}

fn to_json<T: serde::Serialize>(v: &T) -> Result<String, OrchestraError> {
    serde_json::to_string_pretty(v).map_err(|e| OrchestraError::Config(e.to_string()))
}

pub fn service_for(config: &Config) -> Result<Arc<MemoryService>, OrchestraError> {
    let path = config.resolved_memory_path().ok_or_else(|| {
        OrchestraError::Config(
            "cannot determine where memory is kept (no home directory); set memory.path in the config".into(),
        )
    })?;
    Ok(MemoryService::new(
        path,
        MemorySettings::from_config(config),
    ))
}

/// A `--project` value: a directory (canonical path identity) or otherwise a plain name.
pub fn project_ref(arg: &str) -> ProjectRef {
    if arg.contains('/') || Path::new(arg).is_dir() {
        ProjectRef::from_path(Path::new(arg))
    } else {
        ProjectRef::named(arg)
    }
}

fn project_from(arg: Option<&str>, svc: &MemoryService) -> ProjectRef {
    match arg {
        Some(a) => project_ref(a),
        None => svc.default_project(None),
    }
}

fn day(ms: u64) -> String {
    format_utc(ms).chars().take(10).collect()
}

fn clip(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        format!("{}…", flat.chars().take(max).collect::<String>())
    }
}

fn row(r: &MemoryRecord) -> String {
    format!(
        "{:<24} {:<8} {:<11} {:<19} {}  {}{}",
        r.id,
        r.scope,
        r.kind,
        r.status.as_str(),
        day(r.created_ms),
        clip(&r.text, 90),
        if r.lifecycle == ai_orchestra::memory::model::Lifecycle::Superseded {
            "  [replaced]"
        } else {
            ""
        }
    )
}

fn parse_scope(s: Option<&str>) -> Result<Option<MemoryScope>, OrchestraError> {
    s.map(str::parse).transpose()
}

pub async fn run_memory(
    cmd: MemoryCommand,
    project: Option<String>,
    config: &Config,
) -> Result<u8, OrchestraError> {
    let svc = service_for(config)?;
    let project = project_from(project.as_deref(), &svc);
    match cmd {
        MemoryCommand::List {
            scope,
            kind,
            all_versions,
            limit,
            output,
        } => {
            let kind = kind.map(|k| k.parse::<MemoryKind>()).transpose()?;
            let rows = svc
                .list(
                    &project,
                    parse_scope(scope.as_deref())?,
                    kind,
                    all_versions,
                    limit,
                )
                .await?;
            match output {
                Fmt::Json => println!("{}", to_json(&rows)?),
                Fmt::Text if rows.is_empty() => {
                    println!("Nothing is remembered for this project yet.")
                }
                Fmt::Text => {
                    println!(
                        "{:<24} {:<8} {:<11} {:<19} SAVED       TEXT",
                        "ID", "SCOPE", "KIND", "STATUS"
                    );
                    for r in &rows {
                        println!("{}", row(r));
                    }
                }
            }
        }
        MemoryCommand::Search {
            query,
            scope,
            all_versions,
            limit,
            output,
        } => {
            let hits = svc
                .search(
                    &project,
                    &query,
                    parse_scope(scope.as_deref())?,
                    all_versions,
                    limit,
                )
                .await?;
            match output {
                Fmt::Json => println!("{}", to_json(&hits)?),
                Fmt::Text if hits.is_empty() => println!("Nothing matches."),
                Fmt::Text => {
                    for h in &hits {
                        println!("bm25 {:>6.3}  {}", h.score, row(&h.record));
                    }
                }
            }
        }
        MemoryCommand::Show {
            id,
            original,
            output,
        } => {
            let d = svc.show(&project, &id).await?;
            match output {
                Fmt::Json => println!("{}", to_json(&d)?),
                Fmt::Text => {
                    let r = &d.record;
                    println!("id:            {}", r.id);
                    println!(
                        "kind / scope:  {} / {}{}",
                        r.kind,
                        r.scope,
                        r.session_id
                            .as_ref()
                            .map_or(String::new(), |s| format!(" (session {s})"))
                    );
                    println!("said by:       {}", r.origin.as_string());
                    println!(
                        "trust:         {} [{}]",
                        r.status.label(),
                        r.status.as_str()
                    );
                    println!(
                        "stored:        {} · {:?} · saved {} · updated {}",
                        r.lifecycle.as_str(),
                        r.authorization,
                        day(r.created_ms),
                        day(r.updated_ms)
                    );
                    if let Some(t) = &r.topic {
                        println!("topic:         {t}");
                    }
                    if r.sensitive {
                        println!("sensitive:     yes (never sent to an agent)");
                    }
                    if let Some(rt) = r.retrieved_ms {
                        println!("retrieved:     {}", day(rt));
                    }
                    if let Some(p) = &r.published {
                        println!("published:     {p}");
                    }
                    let fresh = assess(r, now_ms(), false, &svc.settings().freshness).label();
                    if !fresh.is_empty() {
                        println!("freshness:     {fresh}");
                    }
                    if let Some(c) = &r.consultation_id {
                        println!("consultation:  {c}  (ai-orchestra show {c})");
                    }
                    for l in &d.links {
                        println!("link:          {} {} {}", l.from_id, l.kind, l.to_id);
                    }
                    for e in &d.evidence {
                        println!(
                            "evidence:      {} ({}) retrieved {}{}",
                            e.url.as_deref().unwrap_or("(no url)"),
                            e.relation.as_deref().unwrap_or("?"),
                            e.retrieved_ms.map_or("unknown".to_string(), day),
                            e.content_hash.as_ref().map_or(String::new(), |h| format!(
                                " sha256 {}",
                                &h[..h.len().min(12)]
                            ))
                        );
                    }
                    println!("\n{}", r.text);
                    if original && let Some(a) = &d.original_answer {
                        println!("\n--- the saved full answer ---\n{a}");
                    }
                }
            }
        }
        MemoryCommand::Add {
            text,
            scope,
            kind,
            topic,
            tags,
            session,
            allow_sensitive,
            expires_days,
        } => {
            let scope: MemoryScope = scope.parse()?;
            let (outcome, record) = svc
                .add(AddInput {
                    project: project.clone(),
                    scope,
                    kind: kind.parse()?,
                    text,
                    topic,
                    tags,
                    session,
                    allow_sensitive,
                    expires_days,
                })
                .await?;
            if outcome.duplicate {
                println!("Already remembered as {} (nothing added).", outcome.id);
            } else {
                println!(
                    "Remembered as {} ({} memory, {}).",
                    record.id, record.scope, record.kind
                );
                for old in &outcome.superseded {
                    println!(
                        "It replaces {old}, which is kept (see `memory list --all-versions`)."
                    );
                }
            }
        }
        MemoryCommand::Forget { id } => {
            let r = svc.forget(&project, &id).await?;
            println!(
                "Forgot {} ({} {}). Its content, index entry and links were deleted.",
                r.id, r.scope, r.kind
            );
        }
        MemoryCommand::Correct { id, text } => {
            let out = svc.correct(&project, &id, &text).await?;
            println!(
                "Corrected: {} now holds the new text; {} is kept as the replaced version.",
                out.id, id
            );
        }
        MemoryCommand::Promote { id, scope } => {
            let out = svc.promote(&project, &id, scope.parse()?).await?;
            if out.duplicate {
                println!("Already there as {}.", out.id);
            } else {
                println!(
                    "Copied to {scope} memory as {} (the original is unchanged; origin, trust and dates were kept).",
                    out.id
                );
            }
        }
        MemoryCommand::Export { all_projects, file } => {
            let bundle = svc
                .export(if all_projects { None } else { Some(&project) })
                .await?;
            let json = to_json(&bundle)?;
            match file {
                Some(path) => {
                    write_private(&path, &json)?;
                    println!(
                        "Wrote {} memories to {} (owner-only).",
                        bundle.memories.len(),
                        path.display()
                    );
                }
                None => println!("{json}"),
            }
        }
        MemoryCommand::Clear {
            scope,
            session,
            yes,
        } => {
            let scope: MemoryScope = scope.parse()?;
            if scope == MemoryScope::Session {
                let (id, n) = svc.clear_session(&project, session.as_deref()).await?;
                println!("Cleared session {id}: {n} memories deleted.");
            } else {
                if !yes {
                    return Err(OrchestraError::Config(format!(
                        "this deletes every {scope} memory{}; add --yes to confirm",
                        if scope == MemoryScope::Project {
                            " of this project"
                        } else {
                            ""
                        }
                    )));
                }
                let n = svc.clear_scope(&project, scope).await?;
                println!("Deleted {n} {scope} memories.");
            }
        }
        MemoryCommand::Stats { output } => {
            let s = svc.stats().await?;
            match output {
                Fmt::Json => println!("{}", to_json(&s)?),
                Fmt::Text => {
                    println!(
                        "database:       {} ({} bytes, schema {})",
                        svc.path().display(),
                        s.file_bytes,
                        s.schema_version
                    );
                    println!(
                        "memories:       {} current, {} replaced; {} session(s), {} consultation(s), {} summary(ies)",
                        s.memories, s.superseded, s.sessions, s.consultations, s.summaries
                    );
                    println!(
                        "by scope:       {}",
                        s.by_scope
                            .iter()
                            .map(|(k, v)| format!("{k} {v}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                    println!(
                        "by kind:        {}",
                        s.by_kind
                            .iter()
                            .map(|(k, v)| format!("{k} {v}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                    if s.metrics_rows > 0 {
                        println!(
                            "per consultation (average of {}): context {:.0} characters against {:.0} for the full session history before it (a reference figure; smaller is not automatically better); retrieval {:.0} µs, assembly {:.0} µs",
                            s.metrics_rows,
                            s.avg_context_chars,
                            s.avg_baseline_chars,
                            s.avg_retrieval_us,
                            s.avg_assembly_us
                        );
                        println!(
                            "extra agent calls made only to condense context: {}",
                            s.extra_ai_calls
                        );
                    }
                    println!(
                        "token figures in this program are estimates (bytes / 3, conservative), never provider counts"
                    );
                }
            }
        }
        MemoryCommand::Status { output } => {
            let st = svc.settings();
            let matrix = continuation::matrix();
            match output {
                Fmt::Json => println!(
                    "{}",
                    to_json(&serde_json::json!({
                        "path": svc.path().display().to_string(),
                        "exists": svc.path().exists(),
                        "project": project,
                        "default_mode": st.default_mode,
                        "context_budget_tokens": st.context_budget_tokens,
                        "session_idle_hours": st.session_idle_hours,
                        "encrypted_at_rest": false,
                        "provider_session_continuation": matrix,
                        "position": continuation::POSITION,
                    }))?
                ),
                Fmt::Text => {
                    println!(
                        "database:   {} ({})",
                        svc.path().display(),
                        if svc.path().exists() {
                            "exists"
                        } else {
                            "not created yet"
                        }
                    );
                    println!(
                        "project:    {} ({})",
                        project.id,
                        project
                            .path
                            .as_deref()
                            .or(project.name.as_deref())
                            .unwrap_or("-")
                    );
                    println!(
                        "default:    --memory {} · context budget ~{} tokens (estimated) · a session idle for {} h is not continued automatically",
                        st.default_mode, st.context_budget_tokens, st.session_idle_hours
                    );
                    println!(
                        "privacy:    local only; owner-only file; NOT encrypted at rest; deleting a memory overwrites its content in the file (copies you made of the file are not touched); no telemetry"
                    );
                    println!("\nprovider-native sessions:");
                    for m in &matrix {
                        println!(
                            "  {:<7} documented: {}\n          AI Orchestra: {} (verified: {}, used: {})",
                            m.provider, m.documented, m.orchestra_uses, m.verified, m.used
                        );
                    }
                    println!("  {}", continuation::POSITION);
                }
            }
        }
        MemoryCommand::Check {
            quarantine,
            rebuild,
        } => {
            let r = svc.check(quarantine, rebuild).await?;
            println!(
                "{}: {}",
                r.path,
                if r.healthy {
                    "healthy"
                } else {
                    "PROBLEMS FOUND"
                }
            );
            for p in &r.problems {
                println!("  {p}");
            }
            if let Some(q) = &r.quarantined_to {
                println!(
                    "The damaged file was moved to {q} (not deleted) and a new, empty database was started."
                );
            }
            if r.rebuilt_index {
                println!("The full-text index was rebuilt.");
            }
            if !r.healthy && r.quarantined_to.is_none() {
                return Ok(1);
            }
        }
        MemoryCommand::Prune => {
            let (m, s) = svc.prune().await?;
            println!("Removed {m} expired memories and {s} idle session(s).");
        }
        MemoryCommand::ImportHistory => {
            let dir = config
                .debate
                .resolved_history_dir()
                .ok_or_else(|| OrchestraError::Config("cannot find the debate history".into()))?;
            let r = svc.import_history(&DebateStore::new(dir), &project).await?;
            println!(
                "Imported {} consultation(s) into this project's memory ({} already imported, {} not importable, {} skipped because they looked sensitive). The history itself is unchanged.",
                r.imported, r.already_imported, r.not_importable, r.sensitive_skipped
            );
        }
        MemoryCommand::Summarize { agent, session } => {
            let orchestrator =
                ai_orchestra::Orchestrator::new(config.clone())?.with_memory(svc.clone());
            let cancel = CancellationToken::new();
            let row = orchestrator
                .summarize_session(&project, session.as_deref(), &agent, &cancel)
                .await?;
            println!(
                "{}\n\n[stored as {} · written by {} · UNVERIFIED · it condenses {} consultation(s) and replaces none of them]",
                row.body,
                row.id,
                row.origin,
                row.source_ids.len()
            );
        }
    }
    Ok(0)
}

fn write_private(path: &Path, text: &str) -> Result<(), OrchestraError> {
    use std::io::Write as _;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(text.as_bytes())?;
    Ok(())
}

pub async fn run_session(
    cmd: SessionCommand,
    project: Option<String>,
    config: &Config,
) -> Result<u8, OrchestraError> {
    let svc = service_for(config)?;
    let project = project_from(project.as_deref(), &svc);
    match cmd {
        SessionCommand::List { limit, output } => {
            let rows = svc.sessions(&project, limit).await?;
            match output {
                Fmt::Json => println!("{}", to_json(&rows)?),
                Fmt::Text if rows.is_empty() => println!("No sessions in this project yet."),
                Fmt::Text => {
                    println!("{:<24} {:>5}  {:<10}  TITLE", "ID", "TURNS", "LAST USED");
                    for s in rows {
                        println!(
                            "{:<24} {:>5}  {:<10}  {}",
                            s.id,
                            s.turns,
                            day(s.last_used_ms),
                            s.title.as_deref().unwrap_or("-")
                        );
                    }
                }
            }
        }
        SessionCommand::Show { id, output } => {
            let d = svc.session_detail(&project, id.as_deref()).await?;
            match output {
                Fmt::Json => println!("{}", to_json(&d)?),
                Fmt::Text => {
                    println!(
                        "session {} · {} turn(s) · started {} · last used {}",
                        d.session.id,
                        d.session.turns,
                        day(d.session.created_ms),
                        day(d.session.last_used_ms)
                    );
                    for c in &d.consultations {
                        println!(
                            "  {} {} [{}] {}",
                            day(c.created_ms),
                            c.id,
                            c.status,
                            clip(&c.question, 80)
                        );
                    }
                    println!("{} memory record(s) in this session:", d.memories.len());
                    for m in d.memories.iter().take(20) {
                        println!("  {}", row(m));
                    }
                    for s in &d.summaries {
                        println!("summary {} ({}): {}", s.id, s.origin, clip(&s.body, 120));
                    }
                }
            }
        }
        SessionCommand::Resume { id } => {
            let s = svc.resume(&project, &id).await?;
            println!(
                "Session {} will be continued by the next consultations in this project (memory auto/session/project/global). This is AI Orchestra's own session: no provider CLI session is resumed.",
                s.id
            );
        }
        SessionCommand::Clear { id } => {
            let (id, n) = svc.clear_session(&project, id.as_deref()).await?;
            println!("Cleared session {id}: {n} memories deleted.");
        }
    }
    Ok(0)
}
