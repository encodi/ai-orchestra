//! Running consultations as jobs the browser can watch, reconnect to and cancel.
//!
//! A job wraps one `Orchestrator::consult`. Everything the browser sees comes from what the engine
//! reported: `DebateEvent`s are recorded in order, each with a sequence number, and replayed to any
//! number of subscribers (a refresh, another tab, a reconnect with `Last-Event-ID`). Reconnecting only
//! *reads* the recorded events: it never starts, repeats or restarts an agent call. Nothing is
//! invented to make it look busy - no percentages, no simulated typing.
//!
//! Memory is bounded: a job keeps at most [`MAX_EVENTS`] events, and the store keeps the
//! [`MAX_FINISHED`] most recent finished jobs; the saved history remains the long-term record.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::debate::{DebateEvent, DebateOutcome, DebateStatus, Role};
use crate::history::now_ms;
use crate::store::redact_secrets;

pub const MAX_EVENTS: usize = 4000;
pub const MAX_FINISHED: usize = 40;
const MAX_MESSAGE_CHARS: usize = 200_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Running,
    Completed,
    /// There is a result, but something failed while producing it.
    Partial,
    Failed,
    Cancelled,
}

impl JobStatus {
    pub fn is_finished(self) -> bool {
        self != JobStatus::Running
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct EventRecord {
    pub seq: u64,
    pub ts_ms: u64,
    pub kind: &'static str,
    pub data: Value,
}

#[derive(Default)]
struct JobState {
    status: Option<JobStatus>,
    events: Vec<EventRecord>,
    next_seq: u64,
    dropped: u64,
    outcome: Option<DebateOutcome>,
    error: Option<Value>,
    finished_ms: Option<u64>,
    consultation_id: Option<String>,
}

pub struct Job {
    pub id: String,
    pub created_ms: u64,
    pub question_preview: String,
    /// The settings in force, as resolved (value and where it came from), for the interface to show.
    pub settings_in_force: Value,
    pub cancel: CancellationToken,
    state: Mutex<JobState>,
    notify: Notify,
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        text.chars().take(max).collect()
    }
}

/// How a `DebateEvent` is reported to the browser. `None` for events that carry nothing to show.
pub fn map_event(event: &DebateEvent) -> Vec<(&'static str, Value)> {
    match event {
        DebateEvent::Started {
            debate_id,
            mode,
            question: _,
            coordinator,
            reviewers,
            skipped,
            rounds,
        } => vec![(
            "consultation_created",
            json!({
                "consultation_id": debate_id,
                "mode": mode,
                "coordinator": coordinator.to_string(),
                "participants": reviewers.iter().map(ToString::to_string).collect::<Vec<_>>(),
                "skipped": skipped.iter().map(|s| json!({"agent": s.provider.to_string(), "reason": s.reason})).collect::<Vec<_>>(),
                "rounds": rounds,
            }),
        )],
        DebateEvent::CallStarted {
            round,
            role,
            provider,
        } => vec![(
            "provider_started",
            json!({"agent": provider.to_string(), "role": role.as_str(), "round": round, "stage": stage_of(*role)}),
        )],
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
            let mut out = vec![(
                "provider_completed",
                json!({
                    "agent": provider.to_string(), "role": role.as_str(), "round": round, "stage": stage_of(*role),
                    "ok": ok, "duration_ms": duration_ms, "settings": settings,
                    "error": error.as_deref().map(|e| redact_secrets(&clip(e, 2000))),
                }),
            )];
            if let (true, Some(text)) = (*ok, text) {
                out.push((
                    "agent_message_recorded",
                    json!({
                        "agent": provider.to_string(), "role": role.as_str(), "round": round, "stage": stage_of(*role),
                        "technical": matches!(role, Role::Planning | Role::Verification),
                        "text": redact_secrets(&clip(text, MAX_MESSAGE_CHARS)),
                        "settings": settings, "duration_ms": duration_ms,
                    }),
                ));
            }
            out
        }
        DebateEvent::Research { message } => {
            vec![("research_note", json!({"message": redact_secrets(message)}))]
        }
        DebateEvent::Memory { message } => {
            vec![("memory_note", json!({"message": redact_secrets(message)}))]
        }
        // The terminal event is reported from the outcome, with the saved id and the real status.
        DebateEvent::Finished { .. } | DebateEvent::Failed { .. } => Vec::new(),
    }
}

/// The stage of the workflow a call belongs to (the browser groups messages by it).
pub fn stage_of(role: Role) -> &'static str {
    match role {
        Role::Planning | Role::Verification => "research",
        Role::Perspective | Role::Proposal => "independent_analysis",
        Role::Review => "review",
        Role::CrossReview => "cross_review",
        Role::Synthesis => "synthesis",
    }
}

impl Job {
    pub fn push(&self, kind: &'static str, data: Value) -> u64 {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let seq = st.next_seq;
        st.next_seq += 1;
        if st.events.len() >= MAX_EVENTS {
            st.events.remove(0);
            st.dropped += 1;
        }
        st.events.push(EventRecord {
            seq,
            ts_ms: now_ms(),
            kind,
            data,
        });
        drop(st);
        self.notify.notify_waiters();
        seq
    }

    /// Events with `seq` greater than `after` (all of them when `None`), and whether the job is over.
    pub fn events_after(&self, after: Option<u64>) -> (Vec<EventRecord>, bool) {
        let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let from = after.map_or(0, |a| a + 1);
        (
            st.events
                .iter()
                .filter(|e| e.seq >= from)
                .cloned()
                .collect(),
            st.status.is_some_and(JobStatus::is_finished),
        )
    }

    /// A future that completes when something is pushed. Create it *before* reading the events, so a
    /// push between the read and the wait is not missed.
    pub fn notified(&self) -> tokio::sync::futures::Notified<'_> {
        self.notify.notified()
    }

    pub fn status(&self) -> JobStatus {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .status
            .unwrap_or(JobStatus::Running)
    }

    pub fn consultation_id(&self) -> Option<String> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .consultation_id
            .clone()
    }

    pub fn set_consultation_id(&self, id: &str) {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .consultation_id = Some(id.to_string());
    }

    pub fn outcome(&self) -> Option<DebateOutcome> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .outcome
            .clone()
    }

    pub fn error(&self) -> Option<Value> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .error
            .clone()
    }

    pub fn finished_ms(&self) -> Option<u64> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .finished_ms
    }

    pub fn event_count(&self) -> (usize, u64) {
        let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        (st.events.len(), st.dropped)
    }

    /// Ends the job with the engine's outcome. The status is the outcome's own: a cancelled run is
    /// `cancelled`, a run with a failed call is `partial`, and neither is ever called completed.
    pub fn finish_with_outcome(&self, outcome: DebateOutcome) {
        let status = match outcome.status {
            DebateStatus::Complete => JobStatus::Completed,
            DebateStatus::Partial => JobStatus::Partial,
            DebateStatus::Failed => JobStatus::Failed,
            DebateStatus::Cancelled => JobStatus::Cancelled,
        };
        let kind = match status {
            JobStatus::Completed | JobStatus::Partial => "consultation_completed",
            JobStatus::Failed => "consultation_failed",
            _ => "consultation_cancelled",
        };
        let data = json!({
            "status": status, "engine_status": outcome.status.as_str(), "consultation_id": outcome.id,
            "calls": outcome.calls, "duration_ms": outcome.duration_ms,
            "warnings": outcome.warnings.iter().take(10).map(|w| redact_secrets(w)).collect::<Vec<_>>(),
        });
        {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            st.consultation_id = Some(outcome.id.clone());
            st.outcome = Some(outcome);
            st.status = Some(status);
            st.finished_ms = Some(now_ms());
        }
        self.push(kind, data);
    }

    pub fn finish_with_error(&self, kind: &'static str, message: &str, cancelled: bool) {
        let status = if cancelled {
            JobStatus::Cancelled
        } else {
            JobStatus::Failed
        };
        let error = json!({"kind": kind, "message": redact_secrets(message)});
        {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            st.error = Some(error.clone());
            st.status = Some(status);
            st.finished_ms = Some(now_ms());
        }
        self.push(
            if cancelled {
                "consultation_cancelled"
            } else {
                "consultation_failed"
            },
            json!({"status": status, "error": error}),
        );
    }
}

#[derive(Default)]
pub struct JobStore {
    jobs: Mutex<BTreeMap<String, Arc<Job>>>,
}

impl JobStore {
    pub fn running(&self) -> usize {
        self.jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .filter(|j| !j.status().is_finished())
            .count()
    }

    /// Registers a new job unless `max_running` are already running.
    pub fn create(
        &self,
        question: &str,
        settings_in_force: Value,
        max_running: usize,
    ) -> Option<Arc<Job>> {
        let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        if jobs.values().filter(|j| !j.status().is_finished()).count() >= max_running {
            return None;
        }
        let id = format!("job-{}", crate::history::new_debate_id());
        let job = Arc::new(Job {
            id: id.clone(),
            created_ms: now_ms(),
            question_preview: clip(question.trim(), 200),
            settings_in_force,
            cancel: CancellationToken::new(),
            state: Mutex::new(JobState::default()),
            notify: Notify::new(),
        });
        jobs.insert(id, job.clone());
        // Evict the oldest finished jobs beyond the bound.
        let mut finished: Vec<(u64, String)> = jobs
            .values()
            .filter(|j| j.status().is_finished())
            .map(|j| (j.created_ms, j.id.clone()))
            .collect();
        if finished.len() > MAX_FINISHED {
            finished.sort();
            for (_, id) in finished.iter().take(finished.len() - MAX_FINISHED) {
                jobs.remove(id);
            }
        }
        Some(job)
    }

    pub fn get(&self, id: &str) -> Option<Arc<Job>> {
        self.jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
    }

    /// A job by its own id, or by the id of the consultation it produced.
    pub fn find(&self, id: &str) -> Option<Arc<Job>> {
        let jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        jobs.get(id).cloned().or_else(|| {
            jobs.values()
                .find(|j| j.consultation_id().as_deref() == Some(id))
                .cloned()
        })
    }

    pub fn all(&self) -> Vec<Arc<Job>> {
        let mut v: Vec<Arc<Job>> = self
            .jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect();
        v.sort_by_key(|j| std::cmp::Reverse(j.created_ms));
        v
    }

    /// Drops the finished jobs that produced this consultation, so deleting a record also removes what the
    /// server still holds in memory (its events and outcome). A running job is never dropped.
    pub fn forget_consultation(&self, consultation_id: &str) {
        let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        jobs.retain(|_, j| {
            !(j.status().is_finished() && j.consultation_id().as_deref() == Some(consultation_id))
        });
    }

    pub fn cancel_all(&self) {
        for j in self.all() {
            j.cancel.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ProviderId;

    fn finished(provider: &str, role: Role, ok: bool, text: Option<&str>) -> DebateEvent {
        DebateEvent::CallFinished {
            round: 0,
            role,
            provider: ProviderId::new(provider),
            ok,
            text: text.map(str::to_string),
            error: (!ok).then(|| "boom".to_string()),
            settings: None,
            duration_ms: 12,
        }
    }

    #[test]
    fn only_recorded_text_becomes_a_message_and_failures_become_no_message() {
        let ev = map_event(&finished(
            "claude",
            Role::Perspective,
            true,
            Some("My view."),
        ));
        assert_eq!(
            ev.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
            ["provider_completed", "agent_message_recorded"]
        );
        assert_eq!(ev[1].1["text"], "My view.");
        assert_eq!(ev[1].1["stage"], "independent_analysis");
        let failed = map_event(&finished("codex", Role::Review, false, None));
        assert_eq!(
            failed.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
            ["provider_completed"],
            "no invented reply for a failed call"
        );
        assert_eq!(failed[0].1["ok"], false);
        let tech = map_event(&finished("codex", Role::Verification, true, Some("[]")));
        assert_eq!(tech[1].1["technical"], true);
        assert!(
            map_event(&DebateEvent::Finished {
                status: DebateStatus::Complete,
                calls: 1,
                duration_ms: 1
            })
            .is_empty()
        );
    }

    #[test]
    fn events_are_numbered_replayed_after_a_point_and_bounded() {
        let store = JobStore::default();
        let job = store.create("q", json!({}), 2).unwrap();
        for i in 0..5 {
            job.push("research_note", json!({"i": i}));
        }
        let (all, over) = job.events_after(None);
        assert_eq!((all.len(), over), (5, false));
        assert_eq!(
            all.iter().map(|e| e.seq).collect::<Vec<_>>(),
            [0, 1, 2, 3, 4]
        );
        let (tail, _) = job.events_after(Some(2));
        assert_eq!(
            tail.iter().map(|e| e.seq).collect::<Vec<_>>(),
            [3, 4],
            "a reconnect gets only what it missed"
        );
        for _ in 0..MAX_EVENTS + 10 {
            job.push("memory_note", json!({}));
        }
        let (n, dropped) = job.event_count();
        assert_eq!(n, MAX_EVENTS);
        assert!(dropped >= 15);
    }

    #[test]
    fn the_store_limits_running_jobs_and_keeps_a_bounded_history() {
        let store = JobStore::default();
        let a = store.create("a", json!({}), 2).unwrap();
        let _b = store.create("b", json!({}), 2).unwrap();
        assert!(
            store.create("c", json!({}), 2).is_none(),
            "a third running job is refused"
        );
        a.finish_with_error("provider_failure", "x", false);
        assert!(
            store.create("c", json!({}), 2).is_some(),
            "finishing one frees a slot"
        );
        assert_eq!(a.status(), JobStatus::Failed);
        assert!(store.find(&a.id).is_some());
        for i in 0..MAX_FINISHED + 5 {
            let j = store.create(&format!("q{i}"), json!({}), 100).unwrap();
            j.finish_with_error("provider_failure", "x", true);
        }
        assert!(
            store
                .all()
                .iter()
                .filter(|j| j.status().is_finished())
                .count()
                <= MAX_FINISHED + 1
        );
    }
}
