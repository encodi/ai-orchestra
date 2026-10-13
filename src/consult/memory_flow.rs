//! How a consultation uses memory: recall before it runs, remember after it finishes.
//!
//! Recall is read-only and bounded: a block of labelled background data placed before the question,
//! sized to the context budget. Remembering writes session memory only (never persistent memory),
//! only for a consultation that produced an answer, and never for text that looks sensitive. A
//! memory problem is a warning on the consultation, never a reason for it to fail; only a request
//! that cannot be honoured (a session that does not exist, or belongs to another project) is
//! refused, before any agent is called.

use super::*;
use crate::memory::context::provider_context_cap;
use crate::memory::service::{MemoryRequest, Prepared, RecordInput};

impl Orchestrator {
    /// The (estimated) token budget for recalled context, when memory is in use for this request.
    pub(super) fn memory_budget(&self, options: &ConsultOptions) -> Option<usize> {
        let (svc, req) = (self.memory()?, options.memory.as_ref()?);
        (req.mode != crate::memory::model::MemoryMode::Off).then(|| {
            req.context_budget_tokens
                .unwrap_or(svc.settings().context_budget_tokens)
        })
    }

    fn memory_request(&self, options: &ConsultOptions) -> Option<MemoryRequest> {
        let (svc, req) = (self.memory()?, options.memory.as_ref()?);
        let mut req = req.clone();
        if req.project.is_none() {
            req.project = Some(svc.default_project(options.cwd.as_deref()));
        }
        Some(req)
    }

    /// Recalls what bears on the question. `None` when memory is not in use for this request.
    pub(super) async fn prepare_memory(
        &self,
        options: &ConsultOptions,
        decision: &RoutingDecision,
        smart: Option<&Arc<SmartRun>>,
    ) -> Result<Option<Prepared>> {
        let (Some(svc), Some(req)) = (self.memory(), self.memory_request(options)) else {
            return Ok(None);
        };
        let cap = decision
            .participants
            .value
            .iter()
            .map(|p| provider_context_cap(p.as_str()))
            .min()
            .unwrap_or(48 * 1024);
        let prepared = svc
            .prepare(
                &req,
                &options.question,
                cap,
                crate::research::plan::wants_fresh(&options.question),
            )
            .await?;
        if let Some(run) = smart {
            run.budget.note_context(
                prepared.report.context_chars,
                prepared.report.estimated_tokens as u64,
            );
            run.trace.event(
                TraceKind::Stage,
                format!(
                    "memory ({}): {} of {} recalled item(s) used, ~{} of {} estimated tokens; {} left out",
                    prepared.mode,
                    prepared.report.selected,
                    prepared.report.retrieved,
                    prepared.report.estimated_tokens,
                    prepared.report.budget_tokens,
                    prepared.report.omitted
                ),
            );
        }
        Ok(Some(prepared))
    }

    /// What a consultation with these options would recall, without running it: the plan and the
    /// retrieval only. Reads the memory database; calls no agent and writes nothing.
    pub async fn preview_context(&self, options: &ConsultOptions) -> Result<Option<Prepared>> {
        let decision = self.plan_consultation(options)?;
        self.prepare_memory(options, &decision, None).await
    }

    /// Remembers a finished consultation as session memory and updates its context report with the
    /// session it belongs to.
    pub(super) async fn remember_outcome(
        &self,
        prepared: &Prepared,
        outcome: &mut DebateOutcome,
        strategy: Option<&str>,
    ) {
        let Some(svc) = self.memory() else { return };
        let meta = outcome.consultation.as_ref();
        let mode = meta
            .and_then(|m| m.mode)
            .map_or("consultation".to_string(), |m| m.to_string());
        let research = meta.and_then(|m| m.research.clone());
        let result = svc
            .record(RecordInput {
                prepared,
                history_id: &outcome.id,
                question: &outcome.question,
                mode: &mode,
                strategy,
                status: outcome.status,
                calls: outcome.calls,
                final_answer: &outcome.final_answer,
                steps: &outcome.steps,
                research: research.as_ref(),
                extra_ai_calls: 0,
            })
            .await;
        match result {
            Ok(rec) => {
                if let (Some(session), Some(meta)) =
                    (&rec.session_id, outcome.consultation.as_mut())
                    && let Some(ctx) = meta.context.as_mut()
                {
                    ctx.session = Some(session.clone());
                }
                if let Some(why) = rec.skipped
                    && prepared.write
                    && prepared.mode != crate::memory::model::MemoryMode::Off
                {
                    outcome
                        .warnings
                        .push(format!("memory: not remembered: {why}"));
                }
            }
            Err(e) => outcome.warnings.push(format!(
                "memory: this consultation could not be remembered: {e}"
            )),
        }
    }
}
