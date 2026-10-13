//! The Agent Theater's data: the real, recorded steps of a consultation, in the order they finished.
//!
//! Every message comes from a `Step` the engine recorded (the text an agent actually returned, the
//! agent that returned it, when). Nothing is written, completed, summarised or reordered to make a
//! better story. A step that failed is shown as a failure with its recorded error, never as a reply.
//! Only text the CLI returned is shown: there is no hidden reasoning here, because none is recorded.
//! The same function feeds the live view (after the fact, from the finished outcome) and the replay of
//! an old consultation, so what you watch live and what you replay later are the same record.

use serde::Serialize;

use super::jobs::stage_of;
use crate::debate::{DebateOutcome, Role, Step};
use crate::store::redact_secrets;

#[derive(Debug, Clone, Serialize)]
pub struct TheaterMessage {
    pub index: usize,
    pub agent: String,
    pub role: &'static str,
    pub stage: &'static str,
    pub round: u8,
    /// When the call started and when it finished, in ms from the start of the consultation.
    pub started_offset_ms: u64,
    pub finished_offset_ms: u64,
    pub duration_ms: u64,
    pub ok: bool,
    /// The agent's own returned text (absent when the call failed).
    pub text: Option<String>,
    pub error: Option<String>,
    pub error_kind: Option<String>,
    pub settings: Option<String>,
    /// A search plan or a claim-checking reply: machine-oriented, shown folded by default.
    pub technical: bool,
    /// Tokens as the CLI reported them for this call (absent when it does not report them).
    pub usage: Option<crate::types::Usage>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TheaterTimeline {
    pub consultation_id: String,
    pub status: &'static str,
    pub coordinator: String,
    pub participants: Vec<String>,
    pub skipped: Vec<(String, String)>,
    pub duration_ms: u64,
    pub messages: Vec<TheaterMessage>,
    /// Where the messages come from, shown in the interface.
    pub source: &'static str,
    pub note: &'static str,
}

fn message(index: usize, s: &Step) -> TheaterMessage {
    TheaterMessage {
        index,
        agent: s.agent.to_string(),
        role: s.phase.as_str(),
        stage: stage_of(s.phase),
        round: s.round,
        started_offset_ms: s.offset_ms,
        finished_offset_ms: s.offset_ms + s.duration_ms,
        duration_ms: s.duration_ms,
        ok: s.ok,
        text: s.text.as_deref().map(redact_secrets),
        error: s.error.as_deref().map(redact_secrets),
        error_kind: s.error_kind.clone(),
        settings: s.settings.clone(),
        technical: matches!(s.phase, Role::Planning | Role::Verification),
        usage: s.usage.clone(),
    }
}

pub fn timeline(o: &DebateOutcome) -> TheaterTimeline {
    TheaterTimeline {
        consultation_id: o.id.clone(),
        status: o.status.as_str(),
        coordinator: o.coordinator.to_string(),
        participants: o.participants.iter().map(ToString::to_string).collect(),
        skipped: o
            .skipped
            .iter()
            .map(|s| (s.provider.to_string(), s.reason.clone()))
            .collect(),
        duration_ms: o.duration_ms,
        messages: o
            .steps
            .iter()
            .enumerate()
            .map(|(i, s)| message(i, s))
            .collect(),
        source: "the steps recorded by AI Orchestra for this consultation",
        note: "Each message is the text an agent actually returned, in the order the calls finished. Nothing is simulated or added.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome() -> DebateOutcome {
        let json = serde_json::json!({
            "schema": 1, "id": "20261012-000000-000001", "conversation": 1, "created_at_ms": 1,
            "question": "q", "coordinator": "claude", "participants": ["codex"], "skipped": [{"provider": "google", "reason": "not needed"}],
            "rounds_planned": 1, "status": "partial", "synthesized": true,
            "steps": [
                {"phase": "perspective", "round": 0, "agent": "claude", "offset_ms": 0, "duration_ms": 900, "ok": true, "text": "I recommend A.", "error": null, "error_kind": null, "settings": null},
                {"phase": "perspective", "round": 0, "agent": "codex", "offset_ms": 0, "duration_ms": 400, "ok": false, "text": null, "error": "timed out", "error_kind": "timeout", "settings": null},
                {"phase": "synthesis", "round": 1, "agent": "claude", "offset_ms": 1000, "duration_ms": 300, "ok": true, "text": "Final answer.", "error": null, "error_kind": null, "settings": "sonnet"}
            ],
            "initial_proposal": "", "final_answer": "Final answer.", "error": null, "calls": 3, "warnings": [], "duration_ms": 1400
        });
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn the_timeline_is_exactly_the_recorded_steps_with_failures_shown_as_failures() {
        let t = timeline(&outcome());
        assert_eq!(
            t.messages.len(),
            3,
            "one message per recorded step, none added"
        );
        assert_eq!(t.messages[0].text.as_deref(), Some("I recommend A."));
        assert_eq!(
            (t.messages[0].agent.as_str(), t.messages[0].stage),
            ("claude", "independent_analysis")
        );
        assert!(
            !t.messages[1].ok && t.messages[1].text.is_none(),
            "a failed call has no invented reply"
        );
        assert_eq!(t.messages[1].error.as_deref(), Some("timed out"));
        assert_eq!(
            (t.messages[2].stage, t.messages[2].finished_offset_ms),
            ("synthesis", 1300)
        );
        assert_eq!(t.status, "partial");
        assert_eq!(
            t.skipped,
            vec![("google".to_string(), "not needed".to_string())]
        );
        assert!(t.note.contains("Nothing is simulated"));
    }
}
