//! Core value types shared by every layer: provider identity, requests and
//! the normalized response.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Stable identifier of an agent provider (`claude`, `codex`, `google`, ...).
///
/// A string newtype rather than an enum so new providers can be added without
/// touching the orchestration core.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProviderId(String);

impl ProviderId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProviderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for ProviderId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

/// What an agent is allowed to do to the local machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AccessMode {
    /// The agent may read the workspace but not modify it. The default.
    #[default]
    ReadOnly,
    /// The agent may edit files in the workspace. Must be requested explicitly.
    AllowWrite,
}

/// A single prompt to a single agent.
#[derive(Debug, Clone)]
pub struct AgentRequest {
    pub prompt: String,
    /// Working directory for the agent. `None` means the current directory.
    pub cwd: Option<PathBuf>,
    pub access: AccessMode,
    /// Overrides the model configured for the provider.
    pub model: Option<String>,
    /// Overrides the reasoning effort configured for the provider.
    pub effort: Option<String>,
    /// Overrides the configured timeout.
    pub timeout: Option<Duration>,
    /// Start the agent so it does not load MCP servers or user configuration,
    /// where its CLI supports that. Used when ai-orchestra itself runs as an
    /// MCP server, so a child agent cannot load ai-orchestra again.
    pub isolate: bool,
    /// Let the agent use its own web search / web fetch tools, where its CLI supports them.
    /// Used only for research; the agent stays read-only on the machine either way.
    pub web: bool,
}

impl AgentRequest {
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            cwd: None,
            access: AccessMode::ReadOnly,
            model: None,
            effort: None,
            timeout: None,
            isolate: false,
            web: false,
        }
    }

    pub fn with_web(mut self, web: bool) -> Self {
        self.web = web;
        self
    }

    pub fn with_access(mut self, access: AccessMode) -> Self {
        self.access = access;
        self
    }

    pub fn with_cwd(mut self, cwd: PathBuf) -> Self {
        self.cwd = Some(cwd);
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    pub fn with_effort(mut self, effort: impl Into<String>) -> Self {
        self.effort = Some(effort.into());
        self
    }

    pub fn with_isolation(mut self, isolate: bool) -> Self {
        self.isolate = isolate;
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }
}

/// Token accounting, when the provider reports it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
}

impl Usage {
    pub fn is_empty(&self) -> bool {
        self.input_tokens.is_none()
            && self.output_tokens.is_none()
            && self.cached_input_tokens.is_none()
    }
}

/// Provider-independent result of one agent invocation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentResponse {
    pub provider: ProviderId,
    pub text: String,
    /// Provider session / conversation / thread id, if reported.
    pub session_id: Option<String>,
    pub usage: Option<Usage>,
    /// Cost figure as *reported by the CLI*. On subscription plans this is
    /// informational only; nothing is billed per request by this tool.
    pub reported_cost_usd: Option<f64>,
    /// Wall-clock time measured by the orchestrator.
    pub duration_ms: u64,
    /// True when captured output hit the size limit and was cut off.
    pub truncated: bool,
    /// Model we asked the CLI to use (config or flag). `None` means we passed
    /// nothing and the CLI's own default applied.
    pub model_requested: Option<String>,
    pub effort_requested: Option<String>,
    /// Model the CLI itself says it used, when its output includes that
    /// (Claude only so far). The other CLIs do not report it.
    pub model_reported: Option<String>,
    /// URLs that appeared in the output of the agent's own web tools (a search result, a page it
    /// opened), when the CLI's output exposes them. Candidates only: they count as evidence
    /// when this program retrieves them, not because an agent saw them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reported_urls: Vec<ReportedUrl>,
}

/// A URL seen in a provider's web-tool output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportedUrl {
    pub url: String,
    /// The tool that produced it (`WebSearch`, `WebFetch`, `web_search`, ...).
    pub tool: String,
}

impl AgentResponse {
    pub fn new(provider: ProviderId, text: impl Into<String>) -> Self {
        Self {
            provider,
            text: text.into(),
            session_id: None,
            usage: None,
            reported_cost_usd: None,
            duration_ms: 0,
            truncated: false,
            model_requested: None,
            effort_requested: None,
            model_reported: None,
            reported_urls: Vec::new(),
        }
    }

    /// One-line description of the model and effort behind this response.
    pub fn settings_label(&self) -> String {
        let model = match (&self.model_reported, &self.model_requested) {
            (Some(used), Some(asked)) if used != asked => format!("{used} (requested {asked})"),
            (Some(used), _) => used.clone(),
            (None, Some(asked)) => asked.clone(),
            (None, None) => "CLI-default model".to_string(),
        };
        let effort = match &self.effort_requested {
            Some(e) => format!("effort {e}"),
            None => "CLI-default effort".to_string(),
        };
        format!("{model} · {effort}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_label_never_guesses() {
        let mut r = AgentResponse::new(ProviderId::new("x"), "t");
        assert_eq!(r.settings_label(), "CLI-default model · CLI-default effort");
        r.model_requested = Some("sonnet".into());
        r.effort_requested = Some("medium".into());
        assert_eq!(r.settings_label(), "sonnet · effort medium");
        r.model_reported = Some("claude-sonnet-5-5".into());
        assert_eq!(
            r.settings_label(),
            "claude-sonnet-5-5 (requested sonnet) · effort medium"
        );
        r.model_requested = Some("claude-sonnet-5-5".into());
        assert_eq!(r.settings_label(), "claude-sonnet-5-5 · effort medium");
        r.model_requested = None;
        assert_eq!(r.settings_label(), "claude-sonnet-5-5 · effort medium");
    }
}
