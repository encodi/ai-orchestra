//! Presentation of [`DebateEvent`]s: a human-readable live view and an
//! append-only JSON Lines log that can be followed from another terminal.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::debate::{DebateEvent, Role};
use crate::store::redact_secrets;
use crate::types::ProviderId;

fn secs(ms: u64) -> String {
    format!("{:.1}s", ms as f64 / 1000.0)
}

/// Formats one event for the terminal (`--live`): the full text of each step.
/// No trailing newline.
pub fn render_live(event: &DebateEvent) -> String {
    match event {
        DebateEvent::Started {
            debate_id,
            mode,
            coordinator,
            reviewers,
            skipped,
            rounds,
            ..
        } => {
            let mut out = match mode {
                Some(mode) => format!(
                    "== consultation {debate_id} ({mode}): {coordinator} coordinates · with: {} · {rounds} review round(s) ==",
                    if reviewers.is_empty() {
                        "nobody else".to_string()
                    } else {
                        names(reviewers)
                    }
                ),
                None => format!(
                    "== debate {debate_id}: {coordinator} leads · reviewers: {} · {rounds} round(s) ==",
                    names(reviewers)
                ),
            };
            for s in skipped {
                out.push_str(&format!(
                    "\n   (not consulted: {} - {})",
                    s.provider, s.reason
                ));
            }
            out
        }
        DebateEvent::CallStarted {
            round,
            role,
            provider,
        } => match role {
            Role::Planning => format!("   ... {provider} is searching the web for sources"),
            Role::Verification => {
                format!("   ... {provider} is checking claims against the retrieved passages")
            }
            Role::Perspective => format!("   ... {provider} is forming its own answer"),
            Role::Proposal => format!("   ... {provider} is drafting the initial proposal"),
            Role::Review => format!("   ... {provider} is reviewing (round {round})"),
            Role::CrossReview => format!("   ... {provider} is reconsidering (round {round})"),
            Role::Synthesis => format!("   ... {provider} is writing the final synthesis"),
        },
        DebateEvent::CallFinished {
            round,
            role,
            provider,
            ok,
            text,
            error,
            settings,
            duration_ms,
        } => {
            let place = if matches!(
                role,
                Role::Proposal | Role::Perspective | Role::Planning | Role::Verification
            ) {
                String::new()
            } else {
                format!(" · round {round}")
            };
            if *ok {
                let settings = settings
                    .as_ref()
                    .map(|s| format!(" · {s}"))
                    .unwrap_or_default();
                format!(
                    "\n---- {} · {provider}{place} · {}{settings} ----\n{}\n",
                    role.label(),
                    secs(*duration_ms),
                    text.as_deref().unwrap_or("").trim_end()
                )
            } else {
                format!(
                    "\n---- {} · {provider}{place} · FAILED after {} ----\n{}\n",
                    role.label(),
                    secs(*duration_ms),
                    error.as_deref().unwrap_or("no details")
                )
            }
        }
        DebateEvent::Finished {
            status,
            calls,
            duration_ms,
        } => format!(
            "== finished ({status}): {calls} calls, {} ==",
            secs(*duration_ms)
        ),
        DebateEvent::Failed { error } => format!("== debate failed: {error} =="),
        DebateEvent::Research { message } => format!("   [research] {message}"),
        DebateEvent::Memory { message } => format!("   [memory] {message}"),
    }
}

fn names(ids: &[ProviderId]) -> String {
    ids.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// `claude` -> `Claude`
fn title(id: &ProviderId) -> String {
    let s = id.as_str();
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// One short status line per step (default progress display). `None` for
/// events that need no line.
pub fn render_progress(event: &DebateEvent) -> Option<String> {
    Some(match event {
        DebateEvent::Started {
            debate_id,
            mode,
            coordinator,
            reviewers,
            rounds,
            skipped,
            ..
        } => {
            let label = match mode {
                Some(mode) => format!("Consult:{mode}"),
                None => "Debate".to_string(),
            };
            let mut out = match mode {
                Some(_) => format!(
                    "[{label}] {debate_id} · {} coordinates · with: {} · {rounds} review round(s)",
                    title(coordinator),
                    if reviewers.is_empty() {
                        "nobody else".to_string()
                    } else {
                        reviewers.iter().map(title).collect::<Vec<_>>().join(", ")
                    }
                ),
                None => format!(
                    "[Debate] {debate_id} · {} leads · reviewers: {} · {rounds} round(s)",
                    title(coordinator),
                    reviewers.iter().map(title).collect::<Vec<_>>().join(", ")
                ),
            };
            for s in skipped {
                out.push_str(&format!(
                    "\n[{label}] not consulted: {} ({})",
                    title(&s.provider),
                    s.reason
                ));
            }
            out
        }
        DebateEvent::CallStarted {
            round,
            role,
            provider,
        } => match role {
            Role::Planning => format!("[{}] Searching the web for sources...", title(provider)),
            Role::Verification => format!(
                "[{}] Checking claims against the retrieved passages...",
                title(provider)
            ),
            Role::Perspective => format!("[{}] Forming its own answer...", title(provider)),
            Role::Proposal => "[Coordinator] Preparing initial proposal...".to_string(),
            Role::Review => format!("[{}] Reviewing...", title(provider)),
            Role::CrossReview => format!(
                "[{}] Reconsidering after cross-review (round {round})...",
                title(provider)
            ),
            Role::Synthesis => "[Coordinator] Producing final synthesis...".to_string(),
        },
        DebateEvent::CallFinished {
            role,
            provider,
            ok,
            error,
            duration_ms,
            ..
        } => {
            let who = if matches!(role, Role::Proposal | Role::Synthesis) {
                "Coordinator".to_string()
            } else {
                title(provider)
            };
            let what = match role {
                Role::Planning => "Source search",
                Role::Verification => "Claim check",
                Role::Perspective => "Independent answer",
                Role::Proposal => "Initial proposal",
                Role::Review => "Review",
                Role::CrossReview => "Cross-review",
                Role::Synthesis => "Final synthesis",
            };
            if *ok {
                format!("[{who}] {what} ready ({})", secs(*duration_ms))
            } else {
                let reason = error.as_deref().unwrap_or("failed");
                let reason = reason.lines().next().unwrap_or(reason);
                format!(
                    "[{who}] {what} FAILED after {}: {reason}",
                    secs(*duration_ms)
                )
            }
        }
        DebateEvent::Finished {
            status,
            calls,
            duration_ms,
        } => format!(
            "[Debate] {} · {calls} calls · {}",
            status.as_str().to_uppercase(),
            secs(*duration_ms)
        ),
        DebateEvent::Failed { error } => {
            format!("[Debate] FAILED: {}", error.lines().next().unwrap_or(error))
        }
        DebateEvent::Research { message } => format!("[Research] {message}"),
        DebateEvent::Memory { message } => format!("[Memory] {message}"),
    })
}

#[derive(Serialize)]
struct LogLine<'a> {
    ts_ms: u64,
    #[serde(flatten)]
    event: &'a DebateEvent,
}

/// Append-only JSON Lines file, one event per line, created owner-only
/// (0600) because it contains prompts and replies. Credential-shaped
/// strings are masked like everything else the tool retains.
pub struct EventLog {
    file: File,
}

impl EventLog {
    pub fn create(path: &Path) -> std::io::Result<Self> {
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        Ok(Self {
            file: options.open(path)?,
        })
    }

    pub fn write(&mut self, event: &DebateEvent) -> std::io::Result<()> {
        let redacted = redact_event(event);
        let ts_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or(0);
        let mut line = serde_json::to_string(&LogLine {
            ts_ms,
            event: &redacted,
        })
        .map_err(std::io::Error::other)?;
        line.push('\n');
        // One write per line keeps lines intact for `tail -f` readers.
        self.file.write_all(line.as_bytes())
    }
}

fn redact_event(event: &DebateEvent) -> DebateEvent {
    let mut event = event.clone();
    match &mut event {
        DebateEvent::Started { question, .. } => *question = redact_secrets(question),
        DebateEvent::CallFinished { text, error, .. } => {
            if let Some(t) = text {
                *t = redact_secrets(t);
            }
            if let Some(e) = error {
                *e = redact_secrets(e);
            }
        }
        DebateEvent::Failed { error } => *error = redact_secrets(error),
        DebateEvent::Research { message } => *message = redact_secrets(message),
        DebateEvent::Memory { message } => *message = redact_secrets(message),
        _ => {}
    }
    event
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::debate::{DebateStatus, Skipped};

    fn finished(ok: bool, role: Role, provider: &str) -> DebateEvent {
        DebateEvent::CallFinished {
            round: 2,
            role,
            provider: ProviderId::new(provider),
            ok,
            text: ok.then(|| "Looks fine.\n".to_string()),
            error: (!ok)
                .then(|| "codex: authentication problem\n  Sign in with the CLI".to_string()),
            settings: ok.then(|| "gpt-6-luna · effort medium".to_string()),
            duration_ms: 7421,
        }
    }

    fn started() -> DebateEvent {
        DebateEvent::Started {
            debate_id: "20261011-094600-a1b2".into(),
            mode: None,
            question: "q".into(),
            coordinator: ProviderId::new("claude"),
            reviewers: vec![ProviderId::new("codex"), ProviderId::new("google")],
            skipped: vec![Skipped {
                provider: ProviderId::new("x"),
                reason: "disabled in config".into(),
            }],
            rounds: 2,
        }
    }

    #[test]
    fn live_rendering_covers_every_event() {
        let header = render_live(&started());
        assert!(header.contains(
            "debate 20261011-094600-a1b2: claude leads · reviewers: codex, google · 2 round(s)"
        ));
        assert!(header.contains("not consulted: x - disabled in config"));

        for (role, expect) in [
            (Role::Proposal, "claude is drafting the initial proposal"),
            (Role::Review, "claude is reviewing (round 1)"),
            (Role::CrossReview, "claude is reconsidering (round 1)"),
            (Role::Synthesis, "claude is writing the final synthesis"),
        ] {
            let line = render_live(&DebateEvent::CallStarted {
                round: 1,
                role,
                provider: ProviderId::new("claude"),
            });
            assert!(line.contains(expect), "{line}");
        }

        let ok = render_live(&finished(true, Role::CrossReview, "codex"));
        assert!(
            ok.contains("cross-review · codex · round 2 · 7.4s · gpt-6-luna · effort medium"),
            "{ok}"
        );
        assert!(ok.contains("Looks fine."));
        let bad = render_live(&finished(false, Role::Review, "codex"));
        assert!(bad.contains("FAILED after 7.4s") && bad.contains("authentication problem"));
        let proposal = render_live(&finished(true, Role::Proposal, "claude"));
        assert!(
            proposal.contains("initial proposal · claude · 7.4s"),
            "no round for the proposal: {proposal}"
        );

        let done = render_live(&DebateEvent::Finished {
            status: DebateStatus::Partial,
            calls: 3,
            duration_ms: 2000,
        });
        assert!(done.contains("finished (partial): 3 calls, 2.0s"));
        assert!(
            render_live(&DebateEvent::Failed {
                error: "boom".into()
            })
            .contains("failed: boom")
        );
    }

    #[test]
    fn progress_lines_follow_the_documented_format() {
        let line = |e: DebateEvent| render_progress(&e).unwrap();
        assert!(line(started()).contains(
            "[Debate] 20261011-094600-a1b2 · Claude leads · reviewers: Codex, Google · 2 round(s)"
        ));
        assert!(line(started()).contains("not consulted: X (disabled in config)"));
        let at = |role, provider: &str| DebateEvent::CallStarted {
            round: 1,
            role,
            provider: ProviderId::new(provider),
        };
        assert_eq!(
            line(at(Role::Proposal, "claude")),
            "[Coordinator] Preparing initial proposal..."
        );
        assert_eq!(line(at(Role::Review, "codex")), "[Codex] Reviewing...");
        assert_eq!(line(at(Role::Review, "google")), "[Google] Reviewing...");
        assert_eq!(
            line(at(Role::CrossReview, "codex")),
            "[Codex] Reconsidering after cross-review (round 1)..."
        );
        assert_eq!(
            line(at(Role::Synthesis, "claude")),
            "[Coordinator] Producing final synthesis..."
        );

        assert_eq!(
            line(finished(true, Role::Review, "codex")),
            "[Codex] Review ready (7.4s)"
        );
        assert_eq!(
            line(finished(true, Role::Synthesis, "claude")),
            "[Coordinator] Final synthesis ready (7.4s)"
        );
        // A failure shows the first line of the reason only.
        let failed = line(finished(false, Role::Review, "codex"));
        assert_eq!(
            failed,
            "[Codex] Review FAILED after 7.4s: codex: authentication problem"
        );
        assert_eq!(
            line(DebateEvent::Finished {
                status: DebateStatus::Complete,
                calls: 4,
                duration_ms: 31_000
            }),
            "[Debate] COMPLETE · 4 calls · 31.0s"
        );
        assert!(
            line(DebateEvent::Finished {
                status: DebateStatus::Partial,
                calls: 4,
                duration_ms: 1
            })
            .contains("PARTIAL")
        );
        assert_eq!(
            line(DebateEvent::Failed {
                error: "boom\nmore".into()
            }),
            "[Debate] FAILED: boom"
        );
    }

    #[test]
    fn log_is_jsonl_appended_redacted_and_private() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("debate.jsonl");
        {
            let mut log = EventLog::create(&path).unwrap();
            log.write(&DebateEvent::Started {
                debate_id: "id".into(),
                mode: None,
                question: "use OPENAI_API_KEY=hunter2".into(),
                coordinator: ProviderId::new("claude"),
                reviewers: vec![],
                skipped: vec![],
                rounds: 1,
            })
            .unwrap();
            log.write(&finished(true, Role::Review, "codex")).unwrap();
        }
        // A second run appends rather than truncating.
        EventLog::create(&path)
            .unwrap()
            .write(&DebateEvent::Failed { error: "x".into() })
            .unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0]["event"], "started");
        assert_eq!(lines[0]["debate_id"], "id");
        assert!(lines[0]["ts_ms"].as_u64().unwrap() > 0);
        assert_eq!(lines[1]["event"], "call_finished");
        assert_eq!(lines[1]["role"], "review");
        assert_eq!(lines[1]["provider"], "codex");
        assert_eq!(lines[2]["event"], "failed");
        assert!(!text.contains("hunter2"), "secrets must be masked: {text}");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
