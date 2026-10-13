//! A scripted in-process provider. It never spawns anything or contacts any
//! service, so tests can exercise the orchestrator without consuming a real
//! subscription.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::Barrier;
use tokio_util::sync::CancellationToken;

use crate::config::ProviderConfig;
use crate::error::{OrchestraError, Result};
use crate::provider::{AgentProvider, AuthState, Invocation, InvokeContext};
use crate::research::capabilities::{ResearchCapabilities, Support};
use crate::types::{AgentRequest, AgentResponse, ProviderId, ReportedUrl};

/// One scripted outcome.
#[derive(Clone)]
pub enum MockReply {
    Text(String),
    /// The agent reports an error.
    Failure(String),
    Auth(String),
    Quota(String),
    /// The call exceeded its timeout and was terminated.
    Timeout,
    /// The CLI produced output the adapter could not parse.
    Malformed(String),
    /// A reply that also reports URLs seen in the agent's web tool (as a real CLI's output would).
    WebText {
        text: String,
        urls: Vec<String>,
    },
    /// A reply computed from the request (for example a fake verifier that reads the passages in
    /// its prompt). Not consumed: it answers every call from then on.
    Responder(std::sync::Arc<dyn Fn(&AgentRequest) -> String + Send + Sync>),
}

impl std::fmt::Debug for MockReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text(t) => write!(f, "Text({t:?})"),
            Self::Failure(t) => write!(f, "Failure({t:?})"),
            Self::Auth(t) => write!(f, "Auth({t:?})"),
            Self::Quota(t) => write!(f, "Quota({t:?})"),
            Self::Timeout => write!(f, "Timeout"),
            Self::Malformed(t) => write!(f, "Malformed({t:?})"),
            Self::WebText { text, urls } => write!(f, "WebText({text:?}, {urls:?})"),
            Self::Responder(_) => write!(f, "Responder(..)"),
        }
    }
}

pub struct MockProvider {
    id: ProviderId,
    config: ProviderConfig,
    script: Mutex<VecDeque<MockReply>>,
    seen: Mutex<Vec<AgentRequest>>,
    delay: Duration,
    gate: Option<Arc<Barrier>>,
    missing: bool,
    web: bool,
}

impl MockProvider {
    pub fn new(id: &str) -> Self {
        Self {
            id: ProviderId::new(id),
            config: ProviderConfig::default(),
            script: Mutex::new(VecDeque::new()),
            seen: Mutex::new(Vec::new()),
            delay: Duration::ZERO,
            gate: None,
            missing: false,
            web: false,
        }
    }

    /// Behaves like a provider whose CLI can search the web.
    pub fn with_web_search(mut self) -> Self {
        self.web = true;
        self
    }

    /// Queues a reply. Once the script is exhausted the mock echoes the prompt.
    pub fn with_reply(self, reply: MockReply) -> Self {
        self.script
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_back(reply);
        self
    }

    pub fn disabled(mut self) -> Self {
        self.config.enabled = false;
        self
    }

    /// Behaves like an agent whose CLI is not installed.
    pub fn missing(mut self) -> Self {
        self.missing = true;
        self
    }

    /// Waits on `gate` before replying. With a barrier sized for N agents,
    /// every one of them replies only once all N are inside `invoke` at the
    /// same time, which proves parallel execution without any timing
    /// assumption (sequential execution would never release the barrier).
    pub fn with_gate(mut self, gate: Arc<Barrier>) -> Self {
        self.gate = Some(gate);
        self
    }

    pub fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    /// Requests received so far.
    pub fn requests(&self) -> Vec<AgentRequest> {
        self.seen.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

#[async_trait]
impl AgentProvider for MockProvider {
    fn id(&self) -> &ProviderId {
        &self.id
    }

    fn display_name(&self) -> &str {
        "Mock provider"
    }

    fn config(&self) -> &ProviderConfig {
        &self.config
    }

    fn default_executable(&self) -> &str {
        "mock"
    }

    fn research_capabilities(&self) -> ResearchCapabilities {
        if self.web {
            ResearchCapabilities {
                web_search: Support::Documented,
                web_fetch: Support::Unsupported,
                exposes_source_urls: Support::Documented,
                how: "mock".to_string(),
                notes: "scripted mock provider".to_string(),
            }
        } else {
            ResearchCapabilities::none("mock provider without web search")
        }
    }

    fn build_invocation(&self, req: &AgentRequest) -> Result<Invocation> {
        Ok(Invocation {
            args: Vec::new(),
            stdin: Some(req.prompt.clone()),
        })
    }

    fn parse_output(&self, stdout: &str) -> Result<AgentResponse> {
        Ok(AgentResponse::new(self.id.clone(), stdout))
    }

    fn availability(&self, _ctx: &InvokeContext) -> std::result::Result<(), String> {
        if self.missing {
            Err("`mock` not found".to_string())
        } else if self.config.enabled {
            Ok(())
        } else {
            Err("disabled in config".to_string())
        }
    }

    async fn auth_status(&self, _ctx: &InvokeContext, _exe: &std::path::Path) -> AuthState {
        AuthState::LoggedIn {
            method: "mock".to_string(),
            uses_api_key: false,
        }
    }

    async fn invoke(
        &self,
        _ctx: &InvokeContext,
        req: AgentRequest,
        cancel: &CancellationToken,
    ) -> Result<AgentResponse> {
        self.seen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(req.clone());
        if self.missing {
            return Err(OrchestraError::BinaryNotFound {
                provider: self.id.clone(),
                executable: "mock".to_string(),
            });
        }
        if let Some(gate) = &self.gate {
            tokio::select! {
                _ = gate.wait() => {}
                _ = cancel.cancelled() => {
                    return Err(OrchestraError::Cancelled { provider: self.id.clone() });
                }
            }
        }
        if !self.delay.is_zero() {
            tokio::select! {
                _ = tokio::time::sleep(self.delay) => {}
                _ = cancel.cancelled() => {
                    return Err(OrchestraError::Cancelled { provider: self.id.clone() });
                }
            }
        }
        let reply = {
            let mut script = self.script.lock().unwrap_or_else(|e| e.into_inner());
            // A responder stays at the front of the script and answers every later call.
            match script.front() {
                Some(MockReply::Responder(_)) => script.front().cloned(),
                _ => script.pop_front(),
            }
            .unwrap_or_else(|| MockReply::Text(format!("mock reply to: {}", req.prompt)))
        };
        let provider = self.id.clone();
        match reply {
            MockReply::Text(text) => {
                let mut r = AgentResponse::new(provider, text);
                r.duration_ms = u64::try_from(self.delay.as_millis()).unwrap_or(u64::MAX);
                Ok(r)
            }
            MockReply::Failure(message) => Err(OrchestraError::AgentError { provider, message }),
            MockReply::Auth(detail) => Err(OrchestraError::Auth { provider, detail }),
            MockReply::Quota(detail) => Err(OrchestraError::Quota { provider, detail }),
            MockReply::Timeout => Err(OrchestraError::Timeout {
                provider,
                after: Duration::from_secs(300),
            }),
            MockReply::Malformed(detail) => {
                Err(OrchestraError::MalformedOutput { provider, detail })
            }
            MockReply::Responder(f) => Ok(AgentResponse::new(provider, f(&req))),
            MockReply::WebText { text, urls } => {
                let mut r = AgentResponse::new(provider, text);
                r.reported_urls = urls
                    .into_iter()
                    .map(|url| ReportedUrl {
                        url,
                        tool: "mock_search".to_string(),
                    })
                    .collect();
                Ok(r)
            }
        }
    }
}
