//! The decision trace of a consultation: what the engine planned before anything ran (stored in
//! the routing decision) and what it decided while the consultation ran (here), together with the
//! measured budget use and run metrics.
//!
//! Entries are short sentences built by this program from counts, names and reasons. They never
//! contain a prompt, an answer, an argument list, an environment value or a credential.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use super::budget::BudgetUsage;
use super::metrics::RunMetrics;
use super::quality::QualityReport;

/// Most events kept per consultation, and the longest event text.
const MAX_EVENTS: usize = 200;
const MAX_MESSAGE_CHARS: usize = 400;
const SCHEMA: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceKind {
    /// A stage started or finished.
    Stage,
    /// Research: sources found, retrieved, claims checked.
    Research,
    /// The controller chose the next step.
    Decision,
    /// An agent was added or a round was repeated because of a signal.
    Escalation,
    /// Deliberation stopped, and why.
    Stop,
    /// A limit was reached or a call was refused.
    Budget,
    /// Work was avoided because it had already been done.
    Reuse,
    Note,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceEvent {
    pub at_ms: u64,
    pub kind: TraceKind,
    pub message: String,
}

/// Collects events from anywhere in a running consultation.
#[derive(Debug)]
pub struct TraceRecorder {
    started: Instant,
    events: Mutex<Vec<TraceEvent>>,
}

impl TraceRecorder {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            started: Instant::now(),
            events: Mutex::new(Vec::new()),
        })
    }

    pub fn event(&self, kind: TraceKind, message: impl Into<String>) {
        let mut message: String = message.into();
        if message.chars().count() > MAX_MESSAGE_CHARS {
            message = message.chars().take(MAX_MESSAGE_CHARS).collect::<String>() + "...";
        }
        let at_ms = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let mut events = self.events.lock().unwrap_or_else(|e| e.into_inner());
        if events.len() < MAX_EVENTS {
            events.push(TraceEvent {
                at_ms,
                kind,
                message,
            });
        }
    }

    pub fn events(&self) -> Vec<TraceEvent> {
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

/// Saved with the consultation record.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrchestrationRecord {
    pub schema: u32,
    /// What happened while it ran, in order. The plan made before it ran is `routing.smart`.
    pub events: Vec<TraceEvent>,
    /// The reason deliberation ended, in plain words.
    pub stop_reason: Option<String>,
    /// Agents that were called, in the order they were first called.
    pub agents_used: Vec<String>,
    pub budget: BudgetUsage,
    pub metrics: RunMetrics,
    pub wall_ms: u64,
    /// Wall-clock time outside any agent call (fetching pages, preparing prompts, waiting).
    pub overhead_ms: u64,
    pub quality: Option<QualityReport>,
    /// Notes about the provider history, or why none was used.
    pub profile_notes: Vec<String>,
}

impl OrchestrationRecord {
    pub fn new() -> Self {
        Self {
            schema: SCHEMA,
            ..Self::default()
        }
    }

    /// A short readable account, for `show` and `--explain-plan`.
    pub fn render(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        let _ = writeln!(
            out,
            "execution:    {} call(s) in {} ms (agents busy {} ms, outside agent calls {} ms), at most {} at once",
            self.metrics.calls,
            self.wall_ms,
            self.metrics.provider_busy_ms,
            self.overhead_ms,
            self.metrics.peak_concurrency
        );
        let _ = writeln!(
            out,
            "budget:       {} call(s) started, {} reused, {} ms waiting for a slot{}",
            self.budget.calls_started,
            self.budget.calls_reused,
            self.budget.queue_ms,
            self.budget
                .stopped_by
                .as_ref()
                .map_or(String::new(), |s| format!("; stopped: {s}"))
        );
        if let Some(stop) = &self.stop_reason {
            let _ = writeln!(out, "stopped:      {stop}");
        }
        if let Some(q) = &self.quality {
            let failed = q.violations();
            if failed.is_empty() {
                let _ = writeln!(out, "quality gate: {} check(s) passed", q.checks.len());
            } else {
                for v in failed {
                    let _ = writeln!(out, "quality gate: FAILED {}: {}", v.name, v.detail);
                }
            }
        }
        for e in &self.events {
            let _ = writeln!(out, "  +{:>6} ms  {}", e.at_ms, e.message);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_are_kept_in_order_and_bounded() {
        let t = TraceRecorder::new();
        t.event(TraceKind::Stage, "first");
        t.event(TraceKind::Stop, "second");
        for i in 0..500 {
            t.event(TraceKind::Note, format!("n{i}"));
        }
        let events = t.events();
        assert_eq!(events.len(), MAX_EVENTS);
        assert_eq!(
            (events[0].message.as_str(), events[1].kind),
            ("first", TraceKind::Stop)
        );
        assert!(events.windows(2).all(|w| w[0].at_ms <= w[1].at_ms));
    }

    #[test]
    fn long_messages_are_truncated() {
        let t = TraceRecorder::new();
        t.event(TraceKind::Note, "x".repeat(5_000));
        assert!(t.events()[0].message.chars().count() <= MAX_MESSAGE_CHARS + 3);
    }

    #[test]
    fn the_rendered_record_reports_budget_stops_and_gate_failures() {
        let mut r = OrchestrationRecord::new();
        r.wall_ms = 100;
        r.stop_reason = Some("sufficient: nothing contested".into());
        r.budget.stopped_by = Some(super::super::budget::BudgetStop::Calls { limit: 3 });
        r.events.push(TraceEvent {
            at_ms: 5,
            kind: TraceKind::Decision,
            message: "review chosen".into(),
        });
        let text = r.render();
        assert!(
            text.contains("stopped: the limit of 3 agent call(s) was reached"),
            "{text}"
        );
        assert!(text.contains("sufficient: nothing contested"));
        assert!(text.contains("review chosen"));
    }
}
