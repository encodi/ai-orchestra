//! The [`AgentProvider`] abstraction and the shared invocation pipeline.
//!
//! An adapter only has to describe *how to build the command line* and *how to
//! parse the output* for its CLI. Spawning, limits, timeouts, error
//! classification and the recursion guard live here and are shared.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::Serialize;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::config::{Config, ProviderConfig, expand_tilde};
use crate::error::{OrchestraError, Result};
use crate::process::{BILLING_ENV_VARS, CommandSpec, ProcessError, ProcessManager, ProcessOutput};
use crate::types::{AgentRequest, AgentResponse, ProviderId};

/// Set in every agent process we start. If an agent were ever to run
/// `ai-orchestra` itself, the nested run refuses to start (see
/// `Orchestrator::check_not_nested`), which prevents orchestration loops.
pub const ORCHESTRA_ACTIVE_ENV: &str = "AI_ORCHESTRA_ACTIVE";

/// Reads the delegation depth from the environment. `AI_ORCHESTRA_ACTIVE`
/// alone (set by older builds) counts as depth 1.
fn current_depth() -> u32 {
    if let Some(d) = std::env::var(ORCHESTRA_DEPTH_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
    {
        return d;
    }
    u32::from(std::env::var_os(ORCHESTRA_ACTIVE_ENV).is_some())
}

/// Delegation depth of a process: 0 for one started by a person, and one more
/// than its parent for every agent ai-orchestra starts. A process that finds
/// the depth at or above `general.max_depth` refuses to start.
pub const ORCHESTRA_DEPTH_ENV: &str = "AI_ORCHESTRA_DEPTH";

/// How long the cheap diagnostic commands (`--version`, auth status) may take.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// Arguments and stdin for one invocation of a provider CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub args: Vec<String>,
    pub stdin: Option<String>,
}

/// What we could safely learn about the CLI's login state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AuthState {
    LoggedIn {
        /// Human readable login method, e.g. `claude.ai` or `ChatGPT`.
        method: String,
        /// True when the login is a usage-billed API key rather than a
        /// subscription. Violates this tool's cost model.
        uses_api_key: bool,
    },
    NotLoggedIn {
        hint: String,
    },
    /// The CLI offers no safe way to tell, or the check itself failed.
    Unknown {
        reason: String,
    },
}

/// Everything an invocation needs from its environment.
#[derive(Debug, Clone)]
pub struct InvokeContext {
    pub process: Arc<ProcessManager>,
    pub default_timeout: Duration,
    pub max_output_bytes: usize,
    pub max_prompt_bytes: usize,
    /// Environment variable names removed from every child.
    pub scrub_env: Vec<String>,
    pub allow_nested: bool,
    /// Delegation depth of this process (see [`ORCHESTRA_DEPTH_ENV`]).
    pub depth: u32,
    /// Refuse to start at this depth or deeper.
    pub max_depth: u32,
    /// Names (not values) of the variables present in our own environment.
    pub host_env: HashSet<String>,
}

impl InvokeContext {
    pub fn from_config(config: &Config) -> Self {
        let g = &config.general;
        Self {
            process: Arc::new(ProcessManager::new(g.max_concurrent_agents)),
            default_timeout: config.default_timeout(),
            max_output_bytes: g.max_output_bytes,
            max_prompt_bytes: g.max_prompt_bytes,
            scrub_env: if g.scrub_api_key_env {
                BILLING_ENV_VARS.iter().map(|s| s.to_string()).collect()
            } else {
                Vec::new()
            },
            allow_nested: g.allow_nested,
            depth: current_depth(),
            max_depth: g.max_depth,
            host_env: std::env::vars_os()
                .filter_map(|(k, _)| k.into_string().ok())
                .collect(),
        }
    }

    /// Why this process must not start, if its depth is too great.
    pub fn depth_violation(&self) -> Option<&'static str> {
        if self.allow_nested || self.depth < self.max_depth {
            None
        } else {
            Some(ORCHESTRA_DEPTH_ENV)
        }
    }

    /// Billing-related variables that are set in our environment.
    pub fn billing_vars_present(&self) -> Vec<&'static str> {
        BILLING_ENV_VARS
            .iter()
            .copied()
            .filter(|v| self.host_env.contains(*v))
            .collect()
    }
}

#[async_trait]
pub trait AgentProvider: Send + Sync {
    fn id(&self) -> &ProviderId;
    fn display_name(&self) -> &str;
    /// Extra names accepted on the command line (`agy` for `google`).
    fn aliases(&self) -> &[&str] {
        &[]
    }
    fn config(&self) -> &ProviderConfig;
    /// Executable used when the config does not name one.
    fn default_executable(&self) -> &str;
    /// What this CLI can do for web research, and how well that is known. The default is nothing.
    fn research_capabilities(&self) -> crate::research::capabilities::ResearchCapabilities {
        crate::research::capabilities::ResearchCapabilities::none(
            "no web research capability is known for this provider",
        )
    }
    /// Environment variables that are set when this process runs inside the
    /// provider's own interactive/agent session.
    fn host_env_markers(&self) -> &[&str] {
        &[]
    }
    fn version_args(&self) -> Vec<String> {
        vec!["--version".to_string()]
    }

    /// Validates a reasoning-effort level for this CLI. The default accepts
    /// anything; adapters restrict it to what their CLI documents.
    fn check_effort(&self, _effort: &str) -> Result<()> {
        Ok(())
    }

    /// Model and effort for a request: the request wins over the provider's
    /// config, and `None` leaves the choice to the CLI's own settings.
    fn resolve_settings(&self, req: &AgentRequest) -> Result<(Option<String>, Option<String>)> {
        let model = req.model.clone().or_else(|| self.config().model.clone());
        if let Some(model) = &model {
            check_model(self.id(), model)?;
        }
        let effort = req.effort.clone().or_else(|| self.config().effort.clone());
        if let Some(effort) = &effort {
            self.check_effort(effort)?;
        }
        Ok((model, effort))
    }

    /// Builds the command line for a request. Pure; does not spawn anything.
    fn build_invocation(&self, req: &AgentRequest) -> Result<Invocation>;

    /// Turns the stdout of a *successful run* into a normalized response.
    fn parse_output(&self, stdout: &str) -> Result<AgentResponse>;

    /// Checks login state using only local, non-AI commands.
    async fn auth_status(&self, _ctx: &InvokeContext, _executable: &Path) -> AuthState {
        AuthState::Unknown {
            reason: "this CLI has no safe auth-status command".to_string(),
        }
    }

    fn executable_name(&self) -> String {
        expand_tilde(
            self.config()
                .executable
                .as_deref()
                .unwrap_or_else(|| self.default_executable()),
        )
    }

    fn resolve_executable(&self) -> Result<PathBuf> {
        let name = self.executable_name();
        which::which(&name).map_err(|_| OrchestraError::BinaryNotFound {
            provider: self.id().clone(),
            executable: name,
        })
    }

    /// Fails if this process is running inside the provider's own session.
    fn check_not_nested(&self, ctx: &InvokeContext) -> Result<()> {
        if ctx.allow_nested {
            return Ok(());
        }
        match self
            .host_env_markers()
            .iter()
            .find(|m| ctx.host_env.contains(**m))
        {
            Some(marker) => Err(OrchestraError::NestedInvocation {
                provider: self.id().clone(),
                marker: (*marker).to_string(),
            }),
            None => Ok(()),
        }
    }

    /// Whether a multi-agent workflow can use this provider right now. The
    /// error is a short human-readable reason for skipping it.
    fn availability(&self, ctx: &InvokeContext) -> std::result::Result<(), String> {
        if !self.config().enabled {
            return Err("disabled in config".to_string());
        }
        if self.resolve_executable().is_err() {
            return Err(format!("`{}` not found", self.executable_name()));
        }
        self.check_not_nested(ctx)
            .map_err(|_| "this process runs inside its own session".to_string())
    }

    /// The full pipeline: guard, build, run, classify failures, parse.
    async fn invoke(
        &self,
        ctx: &InvokeContext,
        req: AgentRequest,
        cancel: &CancellationToken,
    ) -> Result<AgentResponse> {
        let id = self.id().clone();
        self.check_not_nested(ctx)?;
        if req.prompt.len() > ctx.max_prompt_bytes {
            return Err(OrchestraError::PromptTooLarge {
                len: req.prompt.len(),
                max: ctx.max_prompt_bytes,
                target: id.to_string(),
            });
        }
        let program = self.resolve_executable()?;
        let invocation = self.build_invocation(&req)?;
        let (model_requested, effort_requested) = self.resolve_settings(&req)?;

        let timeout = req
            .timeout
            .or_else(|| self.config().timeout())
            .unwrap_or(ctx.default_timeout);
        let spec = CommandSpec {
            program: program.clone(),
            args: invocation.args,
            stdin: invocation.stdin,
            cwd: req.cwd.clone(),
            env_remove: ctx.scrub_env.clone(),
            env_set: vec![
                (ORCHESTRA_ACTIVE_ENV.to_string(), "1".to_string()),
                (
                    ORCHESTRA_DEPTH_ENV.to_string(),
                    ctx.depth.saturating_add(1).to_string(),
                ),
            ],
            timeout,
            max_stdout_bytes: ctx.max_output_bytes,
        };
        let started = Instant::now();
        let output = ctx
            .process
            .run(spec, cancel)
            .await
            .map_err(|e| map_process_error(&id, &program, e))?;
        debug!(provider = %id, exit = ?output.exit_code, "process finished");

        if !output.success {
            // Prefer the CLI's own structured error over raw stderr.
            let detail = match self.parse_output(&output.stdout) {
                Err(OrchestraError::AgentError { message, .. }) => message,
                _ => failure_detail(&output),
            };
            return Err(classify_failure(&id, output.exit_code, detail));
        }

        let mut response = self.parse_output(&output.stdout).map_err(|e| match e {
            OrchestraError::AgentError { provider, message } => {
                classify_failure_message(provider, message)
            }
            OrchestraError::MalformedOutput { provider, detail }
                if output.stdout_truncated || output.stdout_held_open =>
            {
                let why = match (output.stdout_truncated, output.stdout_held_open) {
                    (true, true) => format!(
                        "output was truncated at {} bytes (raise general.max_output_bytes) and \
                         the CLI left a process holding its output open",
                        ctx.max_output_bytes
                    ),
                    (true, false) => format!(
                        "output was truncated at {} bytes; raise general.max_output_bytes",
                        ctx.max_output_bytes
                    ),
                    _ => "the CLI exited but a process it started kept its output open, so the \
                          output may be incomplete"
                        .to_string(),
                };
                OrchestraError::MalformedOutput {
                    provider,
                    detail: format!("{detail} ({why})"),
                }
            }
            other => other,
        })?;
        response.duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        response.truncated = output.stdout_truncated;
        response.model_requested = model_requested;
        response.effort_requested = effort_requested;
        Ok(response)
    }
}

/// Rejects model names that could be mistaken for a flag or are not a single
/// token. Arguments are never shell-interpreted; this guards the CLI's own parser.
pub fn check_model(provider: &ProviderId, model: &str) -> Result<()> {
    if model.is_empty()
        || model.starts_with('-')
        || model.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(OrchestraError::Config(format!(
            "invalid model `{model}` for {provider}: it must be one word and not start with `-`"
        )));
    }
    Ok(())
}

/// Checks `effort` against the levels a CLI documents.
pub fn check_effort_in(provider: &ProviderId, effort: &str, allowed: &[&str]) -> Result<()> {
    if allowed.contains(&effort) {
        Ok(())
    } else {
        Err(OrchestraError::Config(format!(
            "invalid effort `{effort}` for {provider} (valid: {})",
            allowed.join(", ")
        )))
    }
}

fn map_process_error(
    id: &ProviderId,
    program: &std::path::Path,
    e: ProcessError,
) -> OrchestraError {
    match e {
        ProcessError::Spawn(source) => OrchestraError::Spawn {
            provider: id.clone(),
            program: program.display().to_string(),
            source,
        },
        ProcessError::Timeout(after) => OrchestraError::Timeout {
            provider: id.clone(),
            after,
        },
        ProcessError::Cancelled => OrchestraError::Cancelled {
            provider: id.clone(),
        },
        ProcessError::Io(e) => OrchestraError::Io(e),
    }
}

/// Last lines of stderr (or stdout when stderr is empty), bounded in size.
fn failure_detail(out: &ProcessOutput) -> String {
    let text = if out.stderr.trim().is_empty() {
        out.stdout.as_str()
    } else {
        out.stderr.as_str()
    };
    let text = text.trim();
    if text.is_empty() {
        return "no output".to_string();
    }
    tail_chars(text, 800)
}

fn tail_chars(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_string();
    }
    let tail: String = text.chars().skip(count - max_chars).collect();
    format!("...{tail}")
}

const AUTH_PATTERNS: &[&str] = &[
    "not logged in",
    "please run /login",
    "codex login",
    "invalid api key",
    "invalid authentication",
    "authentication_error",
    "authentication failed",
    "unauthorized",
    "please log in",
    "please sign in",
    "not authenticated",
    "login required",
    "token expired",
    "expired token",
];

const QUOTA_PATTERNS: &[&str] = &[
    "usage limit",
    "rate limit",
    "rate_limit",
    "quota",
    "too many requests",
    "resource_exhausted",
    "limit reached",
    "credit balance",
];

/// Maps an error message from a CLI to a typed error (auth, quota or the
/// generic fallback).
pub fn classify_failure_message(provider: ProviderId, message: String) -> OrchestraError {
    let lower = message.to_lowercase();
    if AUTH_PATTERNS.iter().any(|p| lower.contains(p)) {
        OrchestraError::Auth {
            provider,
            detail: message,
        }
    } else if QUOTA_PATTERNS.iter().any(|p| lower.contains(p)) {
        OrchestraError::Quota {
            provider,
            detail: message,
        }
    } else {
        OrchestraError::AgentError { provider, message }
    }
}

fn classify_failure(provider: &ProviderId, code: Option<i32>, detail: String) -> OrchestraError {
    match classify_failure_message(provider.clone(), detail) {
        OrchestraError::AgentError { provider, message } => OrchestraError::NonZeroExit {
            provider,
            code,
            detail: message,
        },
        typed => typed,
    }
}

/// Lookup table of available providers.
#[derive(Clone, Default)]
pub struct ProviderRegistry {
    providers: Vec<Arc<dyn AgentProvider>>,
}

impl ProviderRegistry {
    pub fn new(providers: Vec<Arc<dyn AgentProvider>>) -> Self {
        Self { providers }
    }

    pub fn all(&self) -> &[Arc<dyn AgentProvider>] {
        &self.providers
    }

    /// Resolves an id or alias, case-insensitively.
    pub fn resolve(&self, name: &str) -> Result<Arc<dyn AgentProvider>> {
        let wanted = name.trim().to_lowercase();
        self.providers
            .iter()
            .find(|p| {
                p.id().as_str() == wanted || p.aliases().iter().any(|a| a.to_lowercase() == wanted)
            })
            .cloned()
            .ok_or_else(|| OrchestraError::UnknownProvider {
                name: name.to_string(),
                known: self.names().join(", "),
            })
    }

    pub fn names(&self) -> Vec<String> {
        self.providers.iter().map(|p| p.id().to_string()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id() -> ProviderId {
        ProviderId::new("x")
    }

    #[test]
    fn classifies_auth_quota_and_generic_errors() {
        assert!(matches!(
            classify_failure_message(id(), "Invalid API key · Please run /login".into()),
            OrchestraError::Auth { .. }
        ));
        assert!(matches!(
            classify_failure_message(id(), "Claude AI usage limit reached".into()),
            OrchestraError::Quota { .. }
        ));
        assert!(matches!(
            classify_failure_message(id(), "429 Too Many Requests".into()),
            OrchestraError::Quota { .. }
        ));
        assert!(matches!(
            classify_failure_message(id(), "something odd".into()),
            OrchestraError::AgentError { .. }
        ));
    }

    #[test]
    fn nonzero_exit_keeps_code_unless_typed() {
        match classify_failure(&id(), Some(7), "kaboom".into()) {
            OrchestraError::NonZeroExit { code, detail, .. } => {
                assert_eq!(code, Some(7));
                assert_eq!(detail, "kaboom");
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(matches!(
            classify_failure(&id(), Some(1), "Not logged in".into()),
            OrchestraError::Auth { .. }
        ));
    }

    #[test]
    fn tail_chars_is_unicode_safe_and_bounded() {
        let s = "é".repeat(2000);
        let t = tail_chars(&s, 800);
        assert_eq!(t.chars().count(), 803);
        assert!(t.starts_with("..."));
        assert_eq!(tail_chars("short", 800), "short");
    }
}
