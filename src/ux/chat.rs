//! The interactive chat: one engine that turns lines into consultations and settings changes.
//!
//! The engine does no terminal work. It reads a line, writes through a [`ChatIo`], and runs the same
//! `Orchestrator::consult` as every other entry point, so the tests drive it with a scripted
//! input and mock agents, and the binary drives it from the terminal. Nothing here decides how many
//! agents a question needs: that is the strategy engine, unless the user set it.
//!
//! A line that starts with `/` is a command; any other line is a question. `/ask --mode review ...`
//! overrides settings for one question only.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::debate::{DebateEvent, DebateOutcome, DebateStatus};
use crate::error::OrchestraError;
use crate::eval::feedback::{Feedback, FeedbackStore, Rating};
use crate::history::{format_utc, now_ms};
use crate::memory::model::MemoryMode;
use crate::memory::service::{MemoryRequest, ProjectRef};
use crate::orchestrator::Orchestrator;
use crate::progress::render_progress;

use super::prefs;
use super::present;
use super::settings::{Key, Layers, SAFETY_NOTES, Settings};

/// Where the engine writes. The terminal implements it with stdout/stderr and Ctrl-C.
pub trait ChatIo: Send {
    /// Normal output (answers, tables, help).
    fn out(&mut self, text: &str);
    /// Progress and notices.
    fn status(&mut self, text: &str);
    /// A token that is cancelled when the user interrupts the consultation about to start.
    fn begin_consultation(&mut self) -> CancellationToken;
    /// The consultation is over: stop listening for the interrupt.
    fn end_consultation(&mut self) {}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Exit,
}

pub struct ChatEngine {
    orch: Arc<Orchestrator>,
    config: Config,
    layers: Layers,
    prefs_path: Option<PathBuf>,
    project: Option<ProjectRef>,
    /// The memory session this chat is continuing, once there is one.
    session: Option<String>,
    /// The next question starts a new session (a fresh chat, or after `/new`).
    new_session_next: bool,
    last: Option<DebateOutcome>,
    feedback: Option<FeedbackStore>,
    asked: usize,
}

const HELP: &str = "\
Type a question to consult the agents. Follow-ups continue the same session.

Settings (each takes a value, or `auto` to let AI Orchestra choose; with no value it shows the current one):
  /mode  /coordinator  /agents  /rounds  /depth  /web  /strategy  /length
  /timeout  /max-calls  /memory  /context-budget  /view
  /settings            show every setting, where it came from, and what is always enforced
  /auto <setting|all>  return a setting (or all) to AUTO
  /save                keep this session's settings as your starting preferences  (/prefs, /prefs clear)
  /ask --mode review --strategy thorough <question>   override settings for one question only

Looking at the last answer:
  /sources  /answers [full]  /disagreements  /why  /stats  /last

Sessions and history:
  /new                 start a new session (settings stay)
  /sessions            list sessions of this project;   /resume <id>   continue one
  /history [words]     list recent consultations, or search them;   /open <id>   show one again
  /feedback <helpful|not-helpful|inaccurate|incomplete|bad-citation|slow|verbose|unsatisfactory> [note]

  /help   /exit
Ctrl-C while a consultation runs cancels it (children are terminated); at the prompt it leaves.";

impl ChatEngine {
    pub fn new(
        orch: Arc<Orchestrator>,
        config: Config,
        prefs_path: Option<PathBuf>,
        feedback: Option<FeedbackStore>,
        resume: Option<String>,
    ) -> crate::error::Result<Self> {
        let preferences = match &prefs_path {
            Some(p) => prefs::load(p)?,
            None => Settings::default(),
        };
        let project = orch.memory().map(|m| m.default_project(None));
        Ok(Self {
            orch,
            config,
            layers: Layers {
                preferences,
                session: Settings::default(),
            },
            prefs_path,
            project,
            new_session_next: resume.is_none(),
            session: resume,
            last: None,
            feedback,
            asked: 0,
        })
    }

    pub fn banner(&self) -> String {
        let mut out = String::from("AI Orchestra 🎼  (/help for commands, /exit to leave)\n");
        let r = self.layers.resolve(&Settings::default());
        let set: Vec<String> = Key::ALL
            .into_iter()
            .filter_map(|k| r.settings.get(k).map(|v| format!("{}={v}", k.name())))
            .collect();
        if !set.is_empty() {
            out.push_str(&format!("Your settings: {}  (/settings)\n", set.join(", ")));
        }
        if self.orch.memory().is_none() {
            out.push_str(
                "Memory is not available: follow-up questions will not recall earlier answers.\n",
            );
        } else if let Some(id) = &self.session {
            out.push_str(&format!("Continuing session {id}.\n"));
        }
        out
    }

    /// Sets a session setting before the first question (for `chat --mode ...`).
    pub fn preset(&mut self, key: Key, value: &str) -> crate::error::Result<()> {
        self.layers.session.set(key, value)
    }

    pub fn last(&self) -> Option<&DebateOutcome> {
        self.last.as_ref()
    }

    pub fn session(&self) -> Option<&str> {
        self.session.as_deref()
    }

    pub fn layers(&self) -> &Layers {
        &self.layers
    }

    pub async fn handle(&mut self, line: &str, io: &mut dyn ChatIo) -> Flow {
        let line = line.trim();
        if line.is_empty() {
            return Flow::Continue;
        }
        let Some(command) = line.strip_prefix('/') else {
            self.ask(line, io).await;
            return Flow::Continue;
        };
        let (name, arg) = command
            .split_once(char::is_whitespace)
            .unwrap_or((command, ""));
        let arg = arg.trim();
        match name.to_lowercase().as_str() {
            "help" | "?" | "h" => io.out(HELP),
            "exit" | "quit" | "q" => return Flow::Exit,
            "settings" | "set" => self.show_settings(io),
            "auto" => self.auto(arg, io),
            "save" => self.save_prefs(io),
            "prefs" => self.prefs(arg, io),
            "ask" => self.ask(arg, io).await,
            "new" => {
                self.session = None;
                self.new_session_next = true;
                self.last = None;
                io.out("Started a new session. Your settings stay as they are.\n");
            }
            "sources" => self.view(io, present::render_sources),
            "answers" => match &self.last {
                Some(o) => io.out(&present::render_answers(
                    o,
                    arg.eq_ignore_ascii_case("full"),
                )),
                None => io.out("Nothing to show yet: ask a question first.\n"),
            },
            "disagreements" | "disagree" => self.view(io, present::render_disagreements),
            "why" | "plan" => self.view(io, present::render_why),
            "stats" => self.view(io, present::render_stats),
            "last" => self.view(io, present::render_answer),
            "sessions" => self.sessions(io).await,
            "resume" => self.resume(arg, io).await,
            "history" => self.history(arg, io),
            "open" => self.open(arg, io),
            "feedback" => self.feedback_cmd(arg, io),
            other => {
                // `/mode decision`, `/strategy thorough`, ...
                match Key::parse(other) {
                    Some(key) => self.set(key, arg, io),
                    None => io.out(&format!("Unknown command `/{other}`. /help lists them.\n")),
                }
            }
        }
        Flow::Continue
    }

    fn view(&self, io: &mut dyn ChatIo, f: fn(&DebateOutcome) -> String) {
        match &self.last {
            Some(o) => io.out(&f(o)),
            None => io.out("Nothing to show yet: ask a question first.\n"),
        }
    }

    // ------------------------------------------------------------------ settings

    fn show_settings(&self, io: &mut dyn ChatIo) {
        let r = self.layers.resolve(&Settings::default());
        let mut out = String::from("Settings (AUTO means AI Orchestra chooses and says why):\n");
        out.push_str(&r.render());
        out.push_str("\nPrecedence: safety rules > this question (/ask --x) > this session > saved preferences > AUTO.\nAlways enforced:\n");
        for n in SAFETY_NOTES {
            out.push_str(&format!("  - {n}\n"));
        }
        match &self.session {
            Some(s) => out.push_str(&format!("\nSession: {s}\n")),
            None => out.push_str("\nSession: a new one starts with your next question\n"),
        }
        io.out(&out);
    }

    fn set(&mut self, key: Key, value: &str, io: &mut dyn ChatIo) {
        if value.is_empty() {
            let r = self.layers.resolve(&Settings::default());
            io.out(&format!(
                "{} = {}   (accepts: {})\n",
                key.name(),
                r.settings.get(key).unwrap_or_else(|| "AUTO".into()),
                key.accepts()
            ));
            return;
        }
        let previous = self.layers.session.get(key);
        let outcome = self
            .layers
            .session
            .set(key, value)
            .and_then(|()| self.check_agents(key));
        if outcome.is_err() {
            // A refused value changes nothing.
            match previous {
                Some(p) => {
                    let _ = self.layers.session.set(key, &p);
                }
                None => self.layers.session.clear(key),
            }
        }
        match outcome {
            Ok(()) => {
                let now = self.layers.session.get(key);
                io.out(&match now {
                    Some(v) => format!("{} set to {v} for this session.\n", key.name()),
                    None => {
                        let inherited = self.layers.preferences.get(key);
                        match inherited {
                            // Clearing the session layer reveals the saved preference below it.
                            Some(p) => format!("{} cleared for this session; your saved preference ({p}) applies.\n", key.name()),
                            None => format!("{} is AUTO.\n", key.name()),
                        }
                    }
                });
            }
            Err(e) => io.out(&format!("{e}\n")),
        }
    }

    /// Agent names must be agents this installation knows (the settings only check their shape).
    fn check_agents(&self, key: Key) -> crate::error::Result<()> {
        let names: Vec<String> = match key {
            Key::Coordinator => self.layers.session.coordinator.iter().cloned().collect(),
            Key::Agents => self.layers.session.agents.clone().unwrap_or_default(),
            _ => return Ok(()),
        };
        for n in names {
            self.orch.registry().resolve(&n)?;
        }
        Ok(())
    }

    fn auto(&mut self, arg: &str, io: &mut dyn ChatIo) {
        if arg.eq_ignore_ascii_case("all") {
            self.layers.session = Settings::default();
            io.out(
                "Every session setting is back to AUTO (saved preferences, if any, still apply).\n",
            );
        } else if let Some(key) = Key::parse(arg) {
            self.layers.session.clear(key);
            io.out(&format!(
                "{} is back to AUTO for this session.\n",
                key.name()
            ));
        } else {
            io.out("Usage: /auto <setting>  or  /auto all\n");
        }
    }

    fn save_prefs(&mut self, io: &mut dyn ChatIo) {
        let Some(path) = &self.prefs_path else {
            io.out("Cannot save: the preferences location is unknown (no home directory).\n");
            return;
        };
        let merged = self.layers.preferences.overlay(&self.layers.session);
        match prefs::save(path, &merged) {
            Ok(()) => {
                self.layers.preferences = merged;
                self.layers.session = Settings::default();
                io.out(&format!(
                    "Saved as your starting preferences ({}).\n",
                    path.display()
                ));
            }
            Err(e) => io.out(&format!("Could not save: {e}\n")),
        }
    }

    fn prefs(&mut self, arg: &str, io: &mut dyn ChatIo) {
        if arg.eq_ignore_ascii_case("clear") {
            if let Some(path) = &self.prefs_path {
                let _ = prefs::clear(path);
            }
            self.layers.preferences = Settings::default();
            io.out("Saved preferences removed.\n");
            return;
        }
        let shown = Key::ALL
            .into_iter()
            .filter_map(|k| {
                self.layers
                    .preferences
                    .get(k)
                    .map(|v| format!("  {} = {v}\n", k.name()))
            })
            .collect::<String>();
        io.out(&if shown.is_empty() {
            "No saved preferences. /save keeps this session's settings.\n".to_string()
        } else {
            format!("Saved preferences:\n{shown}")
        });
    }

    // ------------------------------------------------------------------ asking

    async fn ask(&mut self, text: &str, io: &mut dyn ChatIo) {
        let (request, question) = match Settings::parse_request_prefix(text) {
            Ok(x) => x,
            Err(e) => {
                io.out(&format!("{e}\n"));
                return;
            }
        };
        if question.trim().is_empty() {
            io.out("Type the question after the settings.\n");
            return;
        }
        let resolved = self.layers.resolve(&request);
        let mode = match resolved.memory_mode(&self.config) {
            Ok(m) => m,
            Err(e) => {
                io.out(&format!("{e}\n"));
                return;
            }
        };
        let memory = match (mode, self.orch.memory()) {
            (MemoryMode::Off, _) | (_, None) => None,
            (mode, Some(_)) => {
                let mut req = MemoryRequest::new(mode);
                req.session = self.session.clone();
                req.new_session = self.new_session_next && self.session.is_none();
                req.context_budget_tokens = resolved.settings.context_budget;
                req.project = self.project.clone();
                Some(req)
            }
        };
        let mut options = match resolved.to_options(question, &self.config, memory) {
            Ok(o) => o,
            Err(e) => {
                io.out(&format!("{e}\n"));
                return;
            }
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<DebateEvent>();
        options.events = Some(tx);
        // Routing is deterministic and free: show it before any agent runs.
        match self.orch.plan_consultation(&options) {
            Ok(d) => io.status(&format!(
                "Mode: {} · Coordinator: {} · Agents: {}{}",
                d.mode.value,
                d.coordinator.value,
                d.participants
                    .value
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", "),
                d.smart
                    .as_ref()
                    .and_then(|p| p.strategy.as_ref())
                    .map_or(String::new(), |s| format!(" · Strategy: {}", s.value))
            )),
            Err(e) => {
                io.out(&format!("{e}\n"));
                return;
            }
        }
        let cancel = io.begin_consultation();
        let started = Instant::now();
        let orch = self.orch.clone();
        let result = {
            let fut = orch.consult(options, &cancel);
            tokio::pin!(fut);
            loop {
                tokio::select! {
                    res = &mut fut => break res,
                    Some(event) = rx.recv() => {
                        if let Some(line) = render_progress(&event) {
                            io.status(&format!("  {line}  [{}s]", started.elapsed().as_secs()));
                        }
                    }
                }
            }
        };
        io.end_consultation();
        while let Ok(event) = rx.try_recv() {
            if let Some(line) = render_progress(&event) {
                io.status(&format!("  {line}"));
            }
        }
        self.asked += 1;
        match result {
            Ok(outcome) => {
                if let Some(s) = outcome
                    .consultation
                    .as_ref()
                    .and_then(|m| m.context.as_ref())
                    .and_then(|c| c.session.clone())
                {
                    self.session = Some(s);
                    self.new_session_next = false;
                }
                io.out(&format!("\n{}", present::render_answer(&outcome)));
                if resolved.detailed() {
                    io.out(&format!(
                        "\n{}\n{}",
                        present::render_why(&outcome),
                        present::render_stats(&outcome)
                    ));
                }
                self.last = Some(outcome);
            }
            Err(OrchestraError::Cancelled { .. }) => {
                io.out("\nCANCELLED: you stopped the consultation. No result was produced.\n");
            }
            Err(e) => io.out(&format!("\n{e}\n")),
        }
    }

    // ------------------------------------------------------------------ sessions and history

    async fn sessions(&mut self, io: &mut dyn ChatIo) {
        let (Some(svc), Some(project)) = (self.orch.memory(), self.project.clone()) else {
            io.out("Memory is not available, so there are no sessions.\n");
            return;
        };
        match svc.sessions(&project, 15).await {
            Ok(rows) if rows.is_empty() => io.out("No sessions in this project yet.\n"),
            Ok(rows) => {
                let mut out = String::new();
                for s in rows {
                    out.push_str(&format!(
                        "{}{:<24} {:>3} turn(s)  {}  {}\n",
                        if self.session.as_deref() == Some(s.id.as_str()) {
                            "* "
                        } else {
                            "  "
                        },
                        s.id,
                        s.turns,
                        format_utc(s.last_used_ms)
                            .chars()
                            .take(10)
                            .collect::<String>(),
                        s.title.as_deref().unwrap_or("-")
                    ));
                }
                io.out(&out);
            }
            Err(e) => io.out(&format!("{e}\n")),
        }
    }

    async fn resume(&mut self, id: &str, io: &mut dyn ChatIo) {
        let (Some(svc), Some(project)) = (self.orch.memory(), self.project.clone()) else {
            io.out("Memory is not available.\n");
            return;
        };
        if id.is_empty() {
            io.out("Usage: /resume <session id>  (/sessions lists them)\n");
            return;
        }
        match svc.resume(&project, id).await {
            Ok(s) => {
                self.session = Some(s.id.clone());
                self.new_session_next = false;
                self.last = None;
                io.out(&format!(
                    "Continuing session {} ({} turn(s)). This is AI Orchestra's own session; no provider session is resumed, and earlier sources are not re-checked.\n",
                    s.id, s.turns
                ));
            }
            Err(e) => io.out(&format!("{e}\n")),
        }
    }

    fn history(&self, query: &str, io: &mut dyn ChatIo) {
        let Some(store) = self.orch.history() else {
            io.out("Consultations are not being saved, so there is no history.\n");
            return;
        };
        if query.is_empty() {
            let listing = store.list();
            io.out(&crate::history::render_history(&listing, 10));
            return;
        }
        let hits = store.search(query, 10);
        if hits.is_empty() {
            io.out("Nothing matches.\n");
            return;
        }
        let mut out = String::new();
        for h in hits {
            out.push_str(&format!(
                "{}  {}  [{}]  {}\n    ...{}...\n",
                h.summary.id,
                format_utc(h.summary.created_at_ms)
                    .chars()
                    .take(10)
                    .collect::<String>(),
                h.summary.status,
                h.summary.question.chars().take(80).collect::<String>(),
                h.snippet
            ));
        }
        out.push_str("/open <id> shows one again.\n");
        io.out(&out);
    }

    fn open(&mut self, id: &str, io: &mut dyn ChatIo) {
        let Some(store) = self.orch.history() else {
            io.out("Consultations are not being saved.\n");
            return;
        };
        if id.is_empty() {
            io.out("Usage: /open <consultation id>  (/history lists them)\n");
            return;
        }
        match store.load(id) {
            Ok(o) => {
                io.out(&present::render_answer(&o));
                if let Some(note) = staleness_note(
                    &o,
                    now_ms(),
                    self.config.memory.freshness_time_sensitive_days,
                ) {
                    io.out(&format!("\n{note}\n"));
                }
                self.last = Some(o);
            }
            Err(e) => io.out(&format!("{e}\n")),
        }
    }

    fn feedback_cmd(&self, arg: &str, io: &mut dyn ChatIo) {
        let Some(store) = &self.feedback else {
            io.out("Feedback cannot be stored: the location is unknown.\n");
            return;
        };
        let Some(last) = &self.last else {
            io.out("Nothing to give feedback on yet: ask a question first.\n");
            return;
        };
        let (rating, note) = arg.split_once(char::is_whitespace).unwrap_or((arg, ""));
        let Some(rating) = Rating::parse(rating) else {
            io.out(&format!(
                "Usage: /feedback <{}> [note]\n",
                Rating::NAMES.join("|")
            ));
            return;
        };
        match store.add(Feedback::new(last.id.clone(), rating, note.trim())) {
            Ok(id) => io.out(&format!("Thanks. Stored locally as {id} for consultation {}. It changes nothing by itself; `ai-orchestra eval feedback list` shows it and `... delete {id}` removes it.\n", last.id)),
            Err(e) => io.out(&format!("{e}\n")),
        }
    }
}

/// Said when an old consultation is shown again: reopening does not re-check anything.
pub fn staleness_note(o: &DebateOutcome, now: u64, window_days: u32) -> Option<String> {
    let r = o.consultation.as_ref()?.research.as_ref()?;
    let oldest = r.sources.iter().filter_map(|s| s.retrieved_at_ms).min()?;
    let age_days = now.saturating_sub(oldest) / 86_400_000;
    let mut note = format!(
        "These sources were retrieved {} ({age_days} day(s) ago). Reopening a consultation does not re-check them.",
        format_utc(oldest).chars().take(10).collect::<String>()
    );
    if age_days > u64::from(window_days) {
        note.push_str(&format!(" That is older than {window_days} days: prices, versions and availability may have changed. Ask again with web research to refresh."));
    }
    Some(note)
}

/// The status of the last run, for tests and scripts.
pub fn is_complete(o: &DebateOutcome) -> bool {
    o.status == DebateStatus::Complete
}
