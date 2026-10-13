//! An explicit, AI-written summary of a session (`ai-orchestra memory summarize`).
//!
//! This is the only place memory asks an agent for anything, and it only does so when the user runs
//! the command: one call, to the agent the user names. The summary is stored next to its sources,
//! labelled with the agent that wrote it, never replaces them, and is used in a context only when
//! older conclusions would not fit, always labelled "AI-written, unverified".

use super::db::{MetricsRow, SummaryRow, new_id};
use super::model::MemoryKind;
use super::service::ProjectRef;
use crate::error::{OrchestraError, Result};
use crate::history::now_ms;
use crate::orchestrator::Orchestrator;
use crate::types::AgentRequest;
use tokio_util::sync::CancellationToken;

const MAX_INPUT_BYTES: usize = 20 * 1024;

fn prompt(material: &str) -> String {
    format!(
        "You are condensing a user's earlier consultations into a short summary that will be given to AI assistants as background. Rules: use only what is in the material below; keep every number, name, date and URL exactly as written; keep the qualifications (\"not verified\", \"unresolved\", prices that may be out of date); say which statements are the user's and which are AI conclusions; do not add advice, opinions or new facts; at most 250 words. Do not run commands or use tools; answer in plain text.\n\n<material>\n{material}\n</material>"
    )
}

impl Orchestrator {
    /// Asks `agent` (one call) to condense a session, and stores the result with its provenance.
    pub async fn summarize_session(
        &self,
        project: &ProjectRef,
        session: Option<&str>,
        agent: &str,
        cancel: &CancellationToken,
    ) -> Result<SummaryRow> {
        let svc = self
            .memory()
            .ok_or_else(|| OrchestraError::Config("memory is not enabled".into()))?;
        let detail = svc.session_detail(project, session).await?;
        let mut material = String::new();
        let mut sources = Vec::new();
        for c in &detail.consultations {
            let conclusion = detail
                .memories
                .iter()
                .find(|m| {
                    m.kind == MemoryKind::Conclusion
                        && m.consultation_id.as_deref() == Some(c.id.as_str())
                })
                .map(|m| m.text.clone())
                .unwrap_or_else(|| format!("Question: {}", c.question));
            if material.len() + conclusion.len() > MAX_INPUT_BYTES {
                break;
            }
            material.push_str(&format!(
                "--- consultation {} ---\n{}\n\n",
                c.id, conclusion
            ));
            sources.push(c.id.clone());
        }
        if sources.is_empty() {
            return Err(OrchestraError::Config(
                "that session has nothing to summarize yet".into(),
            ));
        }
        let resolved = self.registry().resolve(agent)?;
        let (_, response) = self
            .ask(agent, AgentRequest::new(prompt(&material)), cancel)
            .await?;
        let text = crate::store::redact_secrets(response.text.trim());
        if text.is_empty() {
            return Err(OrchestraError::Memory(
                "the agent returned an empty summary; nothing was stored".into(),
            ));
        }
        let now = now_ms();
        let row = SummaryRow {
            id: new_id("sum"),
            session_id: detail.session.id.clone(),
            kind: "session".to_string(),
            origin: format!("ai:{}", resolved.id()),
            body: text,
            source_ids: sources,
            created_ms: now,
        };
        let (stored, session_id) = (row.clone(), detail.session.id.clone());
        svc.run(move |db| {
            db.insert_summary(&stored)?;
            db.insert_metrics(&MetricsRow {
                consultation_id: None,
                session_id: Some(session_id),
                created_ms: now,
                mode: Some("summary".to_string()),
                baseline_chars: 0,
                context_chars: 0,
                est_tokens: 0,
                retrieved: 0,
                selected: 0,
                omitted: 0,
                retrieval_us: 0,
                assembly_us: 0,
                extra_ai_calls: 1,
            })
        })
        .await?;
        Ok(row)
    }
}
