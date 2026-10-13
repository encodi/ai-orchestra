//! An explicit, AI-written summary of a finished consultation (Milestone 9 follow-up).
//!
//! The user asks for it (a button in the web interface); nothing starts it. It is exactly **one** call to one
//! agent (by default the coordinator that wrote the report), made through `Orchestrator::ask` like the
//! memory summary. The text is labelled as written by that agent, stored next to the consultation and never
//! replaces the report. The model is told to use only what the report says; this is a model's condensation,
//! not a verification, and the interface says so.

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::debate::{DebateOutcome, DebateStatus, Role};
use crate::error::{OrchestraError, Result};
use crate::history::now_ms;
use crate::orchestrator::Orchestrator;
use crate::store::redact_secrets;
use crate::style::PromptStyle;
use crate::types::{AgentRequest, ProviderId};

/// Longest report placed in the prompt (bytes); longer reports are cut and the summary says so.
const MAX_REPORT_BYTES: usize = 24 * 1024;
const MAX_KEPT: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiSummary {
    pub id: String,
    /// The agent that wrote it.
    pub agent: String,
    pub text: String,
    pub created_ms: u64,
    pub duration_ms: u64,
    /// Whether the report was cut to fit the prompt.
    #[serde(default)]
    pub report_truncated: bool,
    /// Always `ai`: the text was written by a model.
    pub origin: String,
}

fn prompt(outcome: &DebateOutcome, report: &str) -> String {
    let status = match outcome.status {
        DebateStatus::Complete => "complete",
        DebateStatus::Partial => "PARTIAL: something failed while it was produced",
        DebateStatus::Failed => "failed",
        DebateStatus::Cancelled => "cancelled",
    };
    format!(
        "You are the coordinator of a group consultation. Write a short summary of the report below for the person who asked.\n\
         Rules: use only what the report says; keep numbers, names, dates, [n] citations and every caveat exactly as written; \
         say what is still uncertain or disputed; if the report is PARTIAL, say that; add no facts, advice or opinions; \
         at most 180 words. Format: one line with the bottom line, then at most 5 bullet points. Plain Markdown, no headings. \
         Do not run commands or use tools.\n\n\
         <question>\n{}\n</question>\n\n<report status=\"{status}\">\n{report}\n</report>",
        outcome.question
    )
}

impl Orchestrator {
    /// Asks `agent` (the coordinator when `None`) for one summary of the saved consultation `id`, stores it
    /// with the consultation and returns it. Fails before any call when there is nothing to summarise.
    pub async fn summarize_consultation(
        &self,
        id: &str,
        agent: Option<&str>,
        style: Option<&PromptStyle>,
        cancel: &CancellationToken,
    ) -> Result<AiSummary> {
        let store = self.history().ok_or_else(|| {
            OrchestraError::History(
                "consultations are not being saved, so there is nothing to summarise".into(),
            )
        })?;
        let mut outcome = store.load(id)?;
        let report = outcome.final_answer.trim();
        if report.is_empty()
            || matches!(
                outcome.status,
                DebateStatus::Failed | DebateStatus::Cancelled
            )
        {
            return Err(OrchestraError::Config(
                "that consultation produced no report to summarise".into(),
            ));
        }
        let (report, truncated) = if report.len() > MAX_REPORT_BYTES {
            let cut = (0..=MAX_REPORT_BYTES)
                .rev()
                .find(|i| report.is_char_boundary(*i))
                .unwrap_or(0);
            (
                format!(
                    "{}\n[... the report continues; it was cut here ...]",
                    &report[..cut]
                ),
                true,
            )
        } else {
            (report.to_string(), false)
        };
        let who = agent.map_or_else(|| outcome.coordinator.to_string(), str::to_string);
        let provider = self.registry().resolve(&who)?;
        let provider_id: ProviderId = provider.id().clone();
        let mut text = prompt(&outcome, &report);
        if let Some(preamble) = style.and_then(|s| s.preamble(&provider_id, Role::Synthesis)) {
            text = format!("{preamble}\n\n{text}");
        }
        let started = std::time::Instant::now();
        let (_, response) = self
            .ask(provider_id.as_str(), AgentRequest::new(text), cancel)
            .await?;
        let body = redact_secrets(response.text.trim());
        if body.is_empty() {
            return Err(OrchestraError::EmptyResponse {
                provider: provider_id.clone(),
                detail: None,
            });
        }
        let now = now_ms();
        let summary = AiSummary {
            id: format!("sum-{now}"),
            agent: provider_id.to_string(),
            text: body,
            created_ms: now,
            duration_ms: started.elapsed().as_millis() as u64,
            report_truncated: truncated,
            origin: "ai".into(),
        };
        outcome.summaries.push(summary.clone());
        if outcome.summaries.len() > MAX_KEPT {
            let extra = outcome.summaries.len() - MAX_KEPT;
            outcome.summaries.drain(..extra);
        }
        store.save(&outcome)?;
        Ok(summary)
    }
}
