//! The per-consultation state of smart orchestration: the budget, the decision trace, the reuse
//! cache and the facts the quality gate needs, shared by every agent call of one consultation.

use std::sync::{Arc, Mutex};

use super::budget::{BudgetLimits, ResourceBudgetManager};
use super::metrics::RunMetrics;
use super::quality::{QualityGate, QualityInput, QualityRequirements};
use super::reuse::CallCache;
use super::trace::{OrchestrationRecord, TraceKind, TraceRecorder};
use crate::debate::{DebateOutcome, DebateStatus};
use crate::research::engine::ResearchReport;

pub struct SmartRun {
    pub budget: Arc<ResourceBudgetManager>,
    pub trace: Arc<TraceRecorder>,
    pub reuse: Arc<CallCache>,
    requirements: QualityRequirements,
    /// Apply the quality gate (a strategy is in force).
    gate: bool,
    profile_notes: Vec<String>,
    stop_reason: Mutex<Option<String>>,
    once: Mutex<std::collections::HashSet<String>>,
}

impl std::fmt::Debug for SmartRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SmartRun").finish_non_exhaustive()
    }
}

impl SmartRun {
    pub fn new(
        limits: BudgetLimits,
        requirements: QualityRequirements,
        gate: bool,
        profile_notes: Vec<String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            budget: ResourceBudgetManager::new(limits),
            trace: TraceRecorder::new(),
            reuse: CallCache::new(),
            requirements,
            gate,
            profile_notes,
            stop_reason: Mutex::new(None),
            once: Mutex::new(Default::default()),
        })
    }

    /// Records why deliberation ended. The first reason wins.
    pub fn set_stop(&self, reason: impl Into<String>) {
        let reason = reason.into();
        let mut slot = self.stop_reason.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_none() {
            self.trace.event(TraceKind::Stop, reason.clone());
            *slot = Some(reason);
        }
    }

    /// Records an event once per `key` (a limit that refuses many calls is one event).
    pub fn event_once(&self, key: &str, kind: TraceKind, message: impl Into<String>) {
        let first = self
            .once
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.to_string());
        if first {
            self.trace.event(kind, message);
        }
    }

    /// Completes a finished consultation: runs the quality gate (which may make the result
    /// honestly partial) and stores the decision trace, the measured budget use and the run
    /// metrics in the consultation record. Call after the record's own metadata is complete.
    pub fn finish(
        &self,
        outcome: &mut DebateOutcome,
        research: Option<&ResearchReport>,
        single_agent: bool,
    ) {
        let budget = self.budget.usage();
        let mut quality = None;
        if self.gate
            && matches!(
                outcome.status,
                DebateStatus::Complete | DebateStatus::Partial
            )
        {
            let stop = budget.stopped_by.clone();
            let report = QualityGate.check(&QualityInput {
                outcome,
                research,
                budget_stop: stop.as_ref(),
                requirements: &self.requirements,
                single_agent,
            });
            QualityGate.apply(&report, outcome);
            for v in report.violations() {
                self.trace.event(
                    TraceKind::Note,
                    format!("quality gate: {} failed: {}", v.name, v.detail),
                );
            }
            quality = Some(report);
        }
        if let Some(stop) = &budget.stopped_by {
            self.set_stop(format!("budget: {stop}"));
        }
        if let Some(meta) = outcome.consultation.as_ref() {
            let rounds = meta.rounds_run;
            self.set_stop(format!(
                "the planned workflow ({}) ran to the end{}",
                meta.strategy,
                if rounds > 0 {
                    format!(" after {rounds} review round(s)")
                } else {
                    String::new()
                }
            ));
        }
        let metrics = RunMetrics::from_steps(&outcome.steps);
        let mut agents_used: Vec<String> = Vec::new();
        for s in &outcome.steps {
            let a = s.agent.to_string();
            if !agents_used.contains(&a) {
                agents_used.push(a);
            }
        }
        let record = OrchestrationRecord {
            schema: 1,
            events: self.trace.events(),
            stop_reason: self
                .stop_reason
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            agents_used,
            overhead_ms: metrics.overhead_ms(outcome.duration_ms),
            wall_ms: outcome.duration_ms,
            metrics,
            budget,
            quality,
            profile_notes: self.profile_notes.clone(),
        };
        if let Some(meta) = outcome.consultation.as_mut() {
            meta.orchestration = Some(record);
        }
    }
}
