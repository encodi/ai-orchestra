//! Provider-native session continuation: what each CLI documents, and what AI Orchestra does about it.
//!
//! The three CLIs document ways to continue an earlier conversation (`claude --resume <id>` and
//! `--continue`, `codex exec resume <id>`, `agy --continue` and `--conversation <id>`), found in
//! their local `--help` output. **AI Orchestra uses none of them.** It starts every call as a fresh,
//! stateless session (`--no-session-persistence`, `--ephemeral`), so it holds no provider session
//! id, and it has not verified, live, that continuing a session works non-interactively, is safe
//! through MCP, preserves context, or lowers what a subscription counts. Provider sessions are
//! therefore not the source of truth for anything: memory is local, in AI Orchestra's own database,
//! and every call receives only the bounded context package assembled from it.
//!
//! This table is shown by `ai-orchestra memory status` so the position is explicit.

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContinuationRow {
    pub provider: &'static str,
    /// How the CLI's own `--help` says a session is continued.
    pub documented: &'static str,
    /// How AI Orchestra starts its calls instead.
    pub orchestra_uses: &'static str,
    /// Whether continuation was run and checked live by this project. Always false so far.
    pub verified: bool,
    /// Whether AI Orchestra uses it. Always false so far.
    pub used: bool,
}

pub fn matrix() -> Vec<ContinuationRow> {
    vec![
        ContinuationRow {
            provider: "claude",
            documented: "`claude --resume <session-id>`, `claude --continue` (from `claude --help`)",
            orchestra_uses: "`--no-session-persistence`: every call is a fresh, unsaved session",
            verified: false,
            used: false,
        },
        ContinuationRow {
            provider: "codex",
            documented: "`codex exec resume <id>` or `--last`, `codex resume`, `codex fork` (from `codex exec --help`)",
            orchestra_uses: "`--ephemeral`: no session files are written",
            verified: false,
            used: false,
        },
        ContinuationRow {
            provider: "google",
            documented: "`agy --continue`, `agy --conversation <ID>` (from `agy --help`)",
            orchestra_uses: "a fresh `agy -p` call each time; the CLI has no flag to disable its own history",
            verified: false,
            used: false,
        },
    ]
}

pub const POSITION: &str = "Provider-native sessions are not used. Every call is stateless and receives a bounded context package built from AI Orchestra's own local memory. A provider session would be an optional optimisation, never the source of truth, and nothing here assumes it would reduce quota.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_provider_session_is_claimed_to_be_verified_or_used() {
        let rows = matrix();
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|r| !r.verified && !r.used));
        assert!(rows.iter().any(|r| r.documented.contains("--resume")));
        assert!(POSITION.contains("never the source of truth"));
    }
}
