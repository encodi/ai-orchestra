//! Typed application errors. Nothing in the crate panics on bad input from an
//! external CLI; everything surfaces as an [`OrchestraError`].

use std::time::Duration;

use thiserror::Error;

use crate::types::ProviderId;

pub type Result<T, E = OrchestraError> = std::result::Result<T, E>;

#[derive(Debug, Error)]
pub enum OrchestraError {
    #[error(
        "{provider}: executable `{executable}` was not found. Install the CLI, or set \
         `executable` for this provider in the config file"
    )]
    BinaryNotFound {
        provider: ProviderId,
        executable: String,
    },

    #[error("{provider}: failed to start `{program}`: {source}")]
    Spawn {
        provider: ProviderId,
        program: String,
        #[source]
        source: std::io::Error,
    },

    #[error("{provider}: timed out after {}s and was terminated", .after.as_secs())]
    Timeout {
        provider: ProviderId,
        after: Duration,
    },

    #[error("{provider}: cancelled")]
    Cancelled { provider: ProviderId },

    #[error("{provider}: exited with {}: {detail}", fmt_exit(*.code))]
    NonZeroExit {
        provider: ProviderId,
        code: Option<i32>,
        detail: String,
    },

    #[error(
        "{provider}: authentication problem: {detail}\n  Sign in with the official CLI \
         (see `ai-orchestra doctor`); this tool never handles credentials itself"
    )]
    Auth {
        provider: ProviderId,
        detail: String,
    },

    #[error(
        "{provider}: usage limit or rate limit reached: {detail}\n  This is a subscription \
         limit enforced by the provider; wait for it to reset or use another agent"
    )]
    Quota {
        provider: ProviderId,
        detail: String,
    },

    /// The CLI ran to completion but reported that the request failed.
    #[error("{provider}: agent reported an error: {message}")]
    AgentError {
        provider: ProviderId,
        message: String,
    },

    #[error("{provider}: could not understand the CLI output: {detail}")]
    MalformedOutput {
        provider: ProviderId,
        detail: String,
    },

    #[error(
        "{provider}: the agent returned an empty response{} (some CLIs end their turn without \
         text after a denied tool call; retrying, or asking for a direct plain-text answer, \
         usually helps)",
        .detail.as_deref().map(|d| format!(" [{d}]")).unwrap_or_default()
    )]
    EmptyResponse {
        provider: ProviderId,
        /// What the CLI's reply did contain (status, field names, token counts), never its text.
        detail: Option<String>,
    },

    #[error(
        "{provider}: refusing to start: this process appears to be running inside {provider} \
         (environment variable `{marker}` is set), which would be a recursive agent call. \
         Run ai-orchestra from a plain terminal, or set `allow_nested = true` in the config"
    )]
    NestedInvocation {
        provider: ProviderId,
        marker: String,
    },

    #[error("prompt is {len} bytes, which exceeds the limit of {max} bytes for {target}")]
    PromptTooLarge {
        len: usize,
        max: usize,
        target: String,
    },

    #[error(
        "debate exceeded its total time budget of {}s; all agents were stopped",
        .after.as_secs()
    )]
    DebateTimeout { after: Duration },

    /// A budget limit (agent calls, time, output size) stopped a call before it started, or the
    /// deadline ended it. The consultation keeps what had already finished.
    #[error("{provider}: not run: {detail}")]
    BudgetExhausted {
        provider: ProviderId,
        detail: String,
    },

    #[error("provider `{provider}` is disabled in the configuration")]
    ProviderDisabled { provider: ProviderId },

    #[error("unknown provider `{name}` (known providers: {known})")]
    UnknownProvider { name: String, known: String },

    #[error("configuration error: {0}")]
    Config(String),

    /// Saved-debate lookup or persistence problems (unknown or ambiguous id, unreadable file).
    #[error("{0}")]
    History(String),

    /// The memory database could not be opened, read or written, or it is damaged.
    #[error("memory: {0}")]
    Memory(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

fn fmt_exit(code: Option<i32>) -> String {
    match code {
        Some(c) => format!("status {c}"),
        None => "a signal".to_string(),
    }
}

impl OrchestraError {
    /// Process exit code for the CLI front end.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Config(_)
            | Self::History(_)
            | Self::UnknownProvider { .. }
            | Self::PromptTooLarge { .. } => 2,
            Self::Auth { .. } => 3,
            Self::Quota { .. } => 4,
            Self::Timeout { .. } | Self::DebateTimeout { .. } => 124,
            Self::Cancelled { .. } => 130,
            _ => 1,
        }
    }
}
