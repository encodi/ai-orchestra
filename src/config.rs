//! TOML configuration: global limits plus per-provider overrides.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{OrchestraError, Result};

pub const CONFIG_ENV_VAR: &str = "AI_ORCHESTRA_CONFIG";
pub const LOCAL_CONFIG_NAME: &str = "ai-orchestra.toml";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub general: GeneralConfig,
    #[serde(default)]
    pub debate: DebateConfig,
    #[serde(default)]
    pub mcp: McpConfig,
    #[serde(default)]
    pub research: ResearchConfig,
    #[serde(default)]
    pub smart: SmartConfig,
    #[serde(default)]
    pub memory: MemoryConfig,
    #[serde(default)]
    pub web: WebConfig,
    /// Per-provider overrides keyed by provider id (`claude`, `codex`, `google`).
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderConfig>,
}

/// The local web interface (`ai-orchestra web`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct WebConfig {
    /// Port of the local server (the address is always loopback unless `--allow-non-local` is given).
    pub port: u16,
    /// Consultations the browser may have running at once.
    pub max_concurrent_consultations: usize,
    /// Extra `Host` values accepted besides 127.0.0.1, localhost and [::1] (only for a deliberate
    /// non-local bind; an unlisted Host is refused to defeat DNS rebinding).
    pub extra_allowed_hosts: Vec<String>,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            port: 8765,
            max_concurrent_consultations: 2,
            extra_allowed_hosts: Vec::new(),
        }
    }
}

/// Web research (`consult` with sources): how pages are fetched and the time allowed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ResearchConfig {
    /// Time allowed for one page, in seconds.
    pub fetch_timeout_secs: u64,
    /// Time allowed for finding and retrieving all sources, in seconds.
    pub total_timeout_secs: u64,
    /// Largest page body read, in bytes.
    pub max_page_bytes: usize,
    /// Most extracted text kept from one page, in bytes.
    pub max_text_bytes: usize,
    /// Pages fetched at the same time.
    pub max_concurrent_fetches: usize,
    pub max_redirects: usize,
    /// Honor `robots.txt` (default true; a site that forbids fetching is not fetched).
    pub respect_robots: bool,
    /// Also fetch plain `http://` pages (default: HTTPS only).
    pub allow_http: bool,
    /// `User-Agent` sent to sites. Its first word is the name matched against `robots.txt`.
    pub user_agent: String,
}

impl Default for ResearchConfig {
    fn default() -> Self {
        let p = crate::research::net::FetchPolicy::default();
        Self {
            fetch_timeout_secs: p.timeout_secs,
            total_timeout_secs: 300,
            max_page_bytes: p.max_page_bytes,
            max_text_bytes: p.max_text_bytes,
            max_concurrent_fetches: p.max_concurrent,
            max_redirects: p.max_redirects,
            respect_robots: true,
            allow_http: false,
            user_agent: p.user_agent,
        }
    }
}

impl ResearchConfig {
    /// The fetch policy these settings describe. The test-only relaxations are never set here.
    pub fn fetch_policy(&self) -> crate::research::net::FetchPolicy {
        crate::research::net::FetchPolicy {
            allow_http: self.allow_http,
            timeout_secs: self.fetch_timeout_secs,
            max_page_bytes: self.max_page_bytes,
            max_text_bytes: self.max_text_bytes,
            max_concurrent: self.max_concurrent_fetches,
            max_redirects: self.max_redirects,
            respect_robots: self.respect_robots,
            user_agent: self.user_agent.clone(),
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct GeneralConfig {
    /// Provider that leads collaboration workflows (used from Milestone 2).
    pub coordinator: String,
    /// Default per-invocation timeout.
    pub timeout_secs: u64,
    /// Maximum number of agent processes running at the same time.
    pub max_concurrent_agents: usize,
    /// Cap on captured stdout per invocation.
    pub max_output_bytes: usize,
    /// Cap on prompt size.
    pub max_prompt_bytes: usize,
    /// Remove API-key style variables from the child environment so the CLIs
    /// use their subscription login instead of usage-billed API access.
    pub scrub_api_key_env: bool,
    /// Permit running while inside the same agent (recursive invocation).
    pub allow_nested: bool,
    /// Delegation depth at which ai-orchestra refuses to start. A process run
    /// by a person is depth 0; every agent ai-orchestra starts runs at one
    /// level deeper (`AI_ORCHESTRA_DEPTH`). With the default of 1, an agent
    /// started by ai-orchestra cannot start ai-orchestra again.
    pub max_depth: u32,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            coordinator: "claude".to_string(),
            timeout_secs: 300,
            max_concurrent_agents: 3,
            max_output_bytes: 4 * 1024 * 1024,
            max_prompt_bytes: 1024 * 1024,
            scrub_api_key_env: true,
            allow_nested: false,
            max_depth: 1,
        }
    }
}

/// Largest value accepted for `debate.rounds`; keeps workflows bounded.
pub const MAX_DEBATE_ROUNDS: u8 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DebateConfig {
    /// Review rounds (1 or 2). A round is one parallel pass in which every
    /// reviewer answers once. Round 1 reviews the coordinator's proposal
    /// independently; round 2 (optional) lets each reviewer reconsider after
    /// reading everyone's round-1 reviews. The coordinator then writes the
    /// final synthesis.
    pub rounds: u8,
    /// Wall-clock budget for the whole debate; on expiry all agents are stopped.
    pub total_timeout_secs: u64,
    /// Save every debate as JSON so `history` and `show` can inspect it.
    pub save: bool,
    /// Where debates are saved. Default: `$XDG_DATA_HOME/ai-orchestra/debates`,
    /// else `~/.local/share/ai-orchestra/debates`.
    pub history_dir: Option<String>,
}

impl Default for DebateConfig {
    fn default() -> Self {
        Self {
            rounds: 1,
            total_timeout_secs: 1800,
            save: true,
            history_dir: None,
        }
    }
}

/// Memory and context (Milestone 7): what consultations may recall, and how much of it is sent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct MemoryConfig {
    /// `auto` (the project's current session and its approved project memories), `off`, `session`,
    /// `project` or `global`. Used when a request names none (`--memory` overrides it).
    pub default: String,
    /// Where the database lives. Default: next to the saved debates (`.../ai-orchestra/memory.db`).
    pub path: Option<String>,
    /// A name that identifies this project instead of the working directory's path. Records of
    /// two directories with the same name here share their project memories.
    pub project: Option<String>,
    /// Most (estimated) tokens of recalled context placed before a question.
    pub context_budget_tokens: usize,
    /// A session unused for longer than this many hours is not continued automatically
    /// (`--session` and `session resume` still reach it).
    pub session_idle_hours: u32,
    /// Session memory older than this many days is deleted by `memory prune` and when a
    /// consultation writes. 0 keeps it for good.
    pub session_retention_days: u32,
    /// Let `auto` also read memories that apply to every project.
    pub auto_include_global: bool,
    /// How many of the latest consultations of the session are always candidates.
    pub recent_turns: usize,
    /// Days a time-sensitive fact stays current (specifications, versions, announcements).
    pub freshness_time_sensitive_days: u32,
    /// Days a price or availability stays current.
    pub freshness_price_days: u32,
    /// Days anything else that was checked against a source stays current.
    pub freshness_general_days: u32,
    /// Window used instead when the question asks for the latest information.
    pub freshness_fresh_question_days: u32,
    /// Let the MCP server save and forget memories when the host says the user asked for it.
    /// Off by default: reading is allowed, writing is not.
    pub allow_mcp_writes: bool,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            default: "auto".to_string(),
            path: None,
            project: None,
            context_budget_tokens: 2000,
            session_idle_hours: 24,
            session_retention_days: 90,
            auto_include_global: false,
            recent_turns: 2,
            freshness_time_sensitive_days: 30,
            freshness_price_days: 14,
            freshness_general_days: 365,
            freshness_fresh_question_days: 7,
            allow_mcp_writes: false,
        }
    }
}

impl MemoryConfig {
    pub fn mode(&self) -> Result<crate::memory::model::MemoryMode> {
        self.default.parse().map_err(|_| {
            OrchestraError::Config(format!(
                "memory.default must be auto, off, session, project or global, got `{}`",
                self.default
            ))
        })
    }
}

/// Smart orchestration (Milestone 6): the default strategy, default limits and the local
/// provider performance history. Every value can be overridden per request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SmartConfig {
    /// `auto`, `fast`, `balanced` or `thorough`; or `off` for the fixed Milestone 4/5
    /// orchestration (every usable agent answers, then the review rounds, then the synthesis).
    pub default_strategy: String,
    /// Most agent calls one consultation may start. Unset: no limit beyond the plan's own.
    pub max_agent_calls: Option<usize>,
    /// Wall-clock limit for one consultation, in seconds. Never above
    /// `debate.total_timeout_secs`, which stays the absolute ceiling.
    pub max_duration_secs: Option<u64>,
    /// Most of one consultation's agent calls running at the same time. Unset: only the global
    /// `general.max_concurrent_agents` applies.
    pub max_parallel: Option<usize>,
    /// Most characters of agent output one consultation accepts before it stops launching work.
    pub max_response_chars: u64,
    /// Keep a small local file with how each provider performed (durations, outcomes). It holds
    /// no prompts, answers or credentials.
    pub profiles: bool,
    /// Where that file lives. Default: next to the saved debates (`.../ai-orchestra/profiles.json`).
    pub profiles_path: Option<String>,
    /// History older than this many days is ignored.
    pub profile_max_age_days: u32,
    /// Successful calls needed before a provider's latency is used.
    pub profile_min_samples: u32,
    /// Weight of the newest call in the moving average, in (0, 1].
    pub profile_ewma_alpha: f64,
    /// A provider that failed this many calls in a row is tried last for a day.
    pub profile_degraded_after_failures: u32,
}

impl Default for SmartConfig {
    fn default() -> Self {
        Self {
            default_strategy: "auto".to_string(),
            max_agent_calls: None,
            max_duration_secs: None,
            max_parallel: None,
            max_response_chars: 2_000_000,
            profiles: true,
            profiles_path: None,
            profile_max_age_days: 14,
            profile_min_samples: 3,
            profile_ewma_alpha: 0.3,
            profile_degraded_after_failures: 3,
        }
    }
}

impl SmartConfig {
    /// The default strategy: `None` means the fixed Milestone 4/5 orchestration.
    pub fn strategy(&self) -> Result<Option<crate::smart::strategy::Strategy>> {
        crate::smart::strategy::parse_default_strategy(&self.default_strategy)
    }

    pub fn profile_settings(&self) -> crate::smart::profiles::ProfileSettings {
        crate::smart::profiles::ProfileSettings {
            ewma_alpha: self.profile_ewma_alpha,
            max_age_days: self.profile_max_age_days,
            min_samples: self.profile_min_samples,
            degraded_after_failures: self.profile_degraded_after_failures,
            degraded_window_hours: 24,
        }
    }
}

impl DebateConfig {
    /// Directory debates are saved to, or `None` if no home directory can be found.
    pub fn resolved_history_dir(&self) -> Option<PathBuf> {
        if let Some(dir) = self.history_dir.as_deref() {
            return Some(PathBuf::from(expand_tilde(dir)));
        }
        let base = std::env::var_os("XDG_DATA_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| home_dir().map(|h| h.join(".local").join("share")))?;
        Some(base.join("ai-orchestra").join("debates"))
    }
}

/// Settings for `ai-orchestra mcp serve`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct McpConfig {
    /// The agent that hosts (launches) this MCP server: `claude`, `codex` or
    /// `google`. It is never called through the server, to avoid starting a
    /// second copy of the agent the user is talking to. Also set with
    /// `ai-orchestra mcp serve --host`.
    pub host: Option<String>,
    /// Allow `ask_agent` / `debate` to call the host agent anyway.
    pub allow_host_agent: bool,
    /// Tasks (ask / debate) running at the same time.
    pub max_concurrent_tasks: usize,
    /// Tasks waiting for a slot; more are rejected.
    pub max_queued_tasks: usize,
    /// Hard cap on one task's total run time, whatever the caller asks for.
    pub max_task_secs: u64,
    /// How long a tool call waits for its task before answering "running" with
    /// a task id (clients have request timeouts of their own).
    pub inline_wait_secs: u64,
    /// Longest wait a caller may request, for tool calls and `get_task_status`.
    pub max_wait_secs: u64,
    /// Finished tasks kept for `get_task_status`; the oldest are dropped.
    pub task_retention: usize,
    /// Cap on the text returned in one tool result.
    pub max_result_bytes: usize,
    /// Start agents in a mode that keeps them from loading MCP servers or
    /// other user configuration where their CLI supports it (Claude: always;
    /// Codex: `--ignore-user-config`; Google has no such option).
    pub isolate_children: bool,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            host: None,
            allow_host_agent: false,
            max_concurrent_tasks: 4,
            max_queued_tasks: 16,
            max_task_secs: 1800,
            inline_wait_secs: 30,
            max_wait_secs: 55,
            task_retention: 100,
            max_result_bytes: 64 * 1024,
            isolate_children: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ProviderConfig {
    pub enabled: bool,
    /// Executable name (looked up on `PATH`) or a path. `~/` is expanded.
    pub executable: Option<String>,
    /// Model passed to the CLI. Unset means the CLI's own default.
    pub model: Option<String>,
    /// Reasoning effort passed to the CLI (valid levels differ per CLI; see
    /// the example config). Unset means the CLI's own default.
    pub effort: Option<String>,
    /// Extra arguments appended verbatim. Written by you, never by an agent.
    pub extra_args: Vec<String>,
    pub timeout_secs: Option<u64>,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            executable: None,
            model: None,
            effort: None,
            extra_args: Vec::new(),
            timeout_secs: None,
        }
    }
}

impl ProviderConfig {
    pub fn timeout(&self) -> Option<Duration> {
        self.timeout_secs.map(Duration::from_secs)
    }
}

impl Config {
    /// Where the provider performance history is kept, or `None` if it is switched off or no
    /// home directory can be found.
    pub fn resolved_profiles_path(&self) -> Option<PathBuf> {
        if !self.smart.profiles {
            return None;
        }
        if let Some(p) = self.smart.profiles_path.as_deref() {
            return Some(PathBuf::from(expand_tilde(p)));
        }
        let dir = self.debate.resolved_history_dir()?;
        Some(dir.parent()?.join("profiles.json"))
    }

    /// Where the memory database lives, or `None` if no home directory can be found.
    pub fn resolved_memory_path(&self) -> Option<PathBuf> {
        if let Some(p) = self.memory.path.as_deref() {
            return Some(PathBuf::from(expand_tilde(p)));
        }
        let dir = self.debate.resolved_history_dir()?;
        Some(dir.parent()?.join("memory.db"))
    }

    pub fn from_toml(text: &str) -> Result<Self> {
        let config: Config =
            toml::from_str(text).map_err(|e| OrchestraError::Config(e.to_string()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        let g = &self.general;
        if g.max_concurrent_agents == 0 {
            return Err(OrchestraError::Config(
                "general.max_concurrent_agents must be at least 1".into(),
            ));
        }
        if g.timeout_secs == 0 {
            return Err(OrchestraError::Config(
                "general.timeout_secs must be greater than 0".into(),
            ));
        }
        if g.max_output_bytes == 0 || g.max_prompt_bytes == 0 {
            return Err(OrchestraError::Config(
                "general.max_output_bytes and general.max_prompt_bytes must be greater than 0"
                    .into(),
            ));
        }
        if self.web.port == 0
            || self.web.max_concurrent_consultations == 0
            || self.web.max_concurrent_consultations > 8
        {
            return Err(OrchestraError::Config(
                "web.port must be set and web.max_concurrent_consultations must be between 1 and 8"
                    .into(),
            ));
        }
        let sm = &self.smart;
        sm.strategy()?;
        if sm.max_agent_calls == Some(0)
            || sm.max_duration_secs == Some(0)
            || sm.max_parallel == Some(0)
            || sm.max_response_chars < 1024
        {
            return Err(OrchestraError::Config(
                "smart.max_agent_calls, max_duration_secs and max_parallel must be at least 1, and smart.max_response_chars at least 1024"
                    .into(),
            ));
        }
        if sm
            .max_duration_secs
            .is_some_and(|d| d > self.debate.total_timeout_secs)
        {
            return Err(OrchestraError::Config(
                "smart.max_duration_secs must not exceed debate.total_timeout_secs, which is the absolute ceiling"
                    .into(),
            ));
        }
        if !(sm.profile_ewma_alpha > 0.0 && sm.profile_ewma_alpha <= 1.0)
            || sm.profile_max_age_days == 0
            || sm.profile_min_samples == 0
            || sm.profile_degraded_after_failures == 0
        {
            return Err(OrchestraError::Config(
                "smart.profile_ewma_alpha must be in (0, 1], and profile_max_age_days, profile_min_samples and profile_degraded_after_failures at least 1"
                    .into(),
            ));
        }
        let mem = &self.memory;
        mem.mode()?;
        if mem.context_budget_tokens < 200
            || mem.context_budget_tokens > 100_000
            || mem.session_idle_hours == 0
            || mem.recent_turns > 10
            || mem.freshness_time_sensitive_days == 0
            || mem.freshness_price_days == 0
            || mem.freshness_general_days == 0
            || mem.freshness_fresh_question_days == 0
        {
            return Err(OrchestraError::Config(
                "memory.context_budget_tokens must be 200-100000, memory.recent_turns at most 10, and memory.session_idle_hours and the memory.freshness_* values at least 1"
                    .into(),
            ));
        }
        let m = &self.mcp;
        if m.max_concurrent_tasks == 0
            || m.max_task_secs == 0
            || m.max_wait_secs == 0
            || m.max_result_bytes < 1024
            || m.task_retention == 0
        {
            return Err(OrchestraError::Config(
                "mcp.max_concurrent_tasks, max_task_secs, max_wait_secs and task_retention must \
                 be at least 1, and mcp.max_result_bytes at least 1024"
                    .into(),
            ));
        }
        if m.inline_wait_secs > m.max_wait_secs {
            return Err(OrchestraError::Config(
                "mcp.inline_wait_secs must not exceed mcp.max_wait_secs".into(),
            ));
        }
        let r = &self.research;
        if !(1..=120).contains(&r.fetch_timeout_secs)
            || !(1..=3600).contains(&r.total_timeout_secs)
            || !(1024..=16 * 1024 * 1024).contains(&r.max_page_bytes)
            || !(1024..=2 * 1024 * 1024).contains(&r.max_text_bytes)
            || !(1..=8).contains(&r.max_concurrent_fetches)
            || r.max_redirects > 10
            || r.user_agent.trim().is_empty()
        {
            return Err(OrchestraError::Config(
                "research: fetch_timeout_secs must be 1-120, total_timeout_secs 1-3600, max_page_bytes 1 KiB-16 MiB, \
                 max_text_bytes 1 KiB-2 MiB, max_concurrent_fetches 1-8, max_redirects 0-10, and user_agent must not be empty"
                    .into(),
            ));
        }
        let d = &self.debate;
        if d.rounds == 0 || d.rounds > MAX_DEBATE_ROUNDS {
            return Err(OrchestraError::Config(format!(
                "debate.rounds must be between 1 and {MAX_DEBATE_ROUNDS}"
            )));
        }
        if d.total_timeout_secs == 0 {
            return Err(OrchestraError::Config(
                "debate.total_timeout_secs must be greater than 0".into(),
            ));
        }
        for (name, p) in &self.providers {
            if p.timeout_secs == Some(0) {
                return Err(OrchestraError::Config(format!(
                    "providers.{name}.timeout_secs must be greater than 0"
                )));
            }
        }
        Ok(())
    }

    /// Settings for `name`, falling back to defaults when not configured.
    pub fn provider(&self, name: &str) -> ProviderConfig {
        self.providers.get(name).cloned().unwrap_or_default()
    }

    pub fn default_timeout(&self) -> Duration {
        Duration::from_secs(self.general.timeout_secs)
    }

    /// Loads configuration. Order: explicit path, `$AI_ORCHESTRA_CONFIG`,
    /// `./ai-orchestra.toml`, `$XDG_CONFIG_HOME` or `~/.config`
    /// `/ai-orchestra/config.toml`. Falls back to defaults when none exist.
    /// An explicitly requested file that is missing is an error.
    pub fn load(explicit: Option<&Path>) -> Result<(Self, Option<PathBuf>)> {
        let path = match explicit {
            Some(p) => Some(p.to_path_buf()),
            None => discover_config_path(),
        };
        match path {
            None => Ok((Config::default(), None)),
            Some(p) => {
                let text = std::fs::read_to_string(&p).map_err(|e| {
                    OrchestraError::Config(format!("cannot read {}: {e}", p.display()))
                })?;
                let config = Config::from_toml(&text).map_err(|e| match e {
                    OrchestraError::Config(m) => {
                        OrchestraError::Config(format!("{}: {m}", p.display()))
                    }
                    other => other,
                })?;
                Ok((config, Some(p)))
            }
        }
    }
}

fn discover_config_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(CONFIG_ENV_VAR).filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(p));
    }
    let local = PathBuf::from(LOCAL_CONFIG_NAME);
    if local.is_file() {
        return Some(local);
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|h| h.join(".config")))?;
    let user = base.join("ai-orchestra").join("config.toml");
    user.is_file().then_some(user)
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Expands a leading `~/` using the home directory.
pub fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = home_dir()
    {
        return home.join(rest).to_string_lossy().into_owned();
    }
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_research_section_is_validated_and_never_relaxes_the_network_rules() {
        let c: Config = toml::from_str(
            "[research]\nfetch_timeout_secs = 5\ntotal_timeout_secs = 60\nmax_page_bytes = 100000\nmax_text_bytes = 20000\nmax_concurrent_fetches = 2\nmax_redirects = 2\nrespect_robots = true\nallow_http = true\nuser_agent = \"my-bot/1\"\n",
        )
        .unwrap();
        c.validate().unwrap();
        let p = c.research.fetch_policy();
        assert_eq!(
            (
                p.timeout_secs,
                p.max_concurrent,
                p.max_redirects,
                p.allow_http
            ),
            (5, 2, 2, true)
        );
        assert_eq!(p.user_agent, "my-bot/1");
        // There is no configuration that allows local or private addresses or any port.
        assert!(p.test_allow_ip.is_none() && !p.allow_any_port);
        assert!(
            toml::from_str::<Config>("[research]\nallow_private_addresses = true\n").is_err(),
            "no such setting exists"
        );
        for bad in [
            "fetch_timeout_secs = 0",
            "fetch_timeout_secs = 500",
            "total_timeout_secs = 0",
            "max_page_bytes = 10",
            "max_text_bytes = 99999999",
            "max_concurrent_fetches = 0",
            "max_concurrent_fetches = 100",
            "max_redirects = 50",
            "user_agent = \" \"",
        ] {
            let c: Config = toml::from_str(&format!("[research]\n{bad}\n")).unwrap();
            let e = c.validate().unwrap_err().to_string();
            assert!(e.contains("research:"), "{bad}: {e}");
        }
        let d = Config::default();
        d.validate().unwrap();
        assert!(!d.research.allow_http && d.research.respect_robots);
    }

    #[test]
    fn defaults_are_valid() {
        Config::default().validate().unwrap();
    }

    #[test]
    fn parses_full_example() {
        let c = Config::from_toml(
            r#"
            [general]
            coordinator = "codex"
            timeout_secs = 60
            max_concurrent_agents = 2

            [providers.claude]
            executable = "/opt/bin/claude"
            model = "sonnet"
            extra_args = ["--effort", "low"]
            timeout_secs = 120

            [providers.google]
            enabled = false
            "#,
        )
        .unwrap();
        assert_eq!(c.general.coordinator, "codex");
        assert_eq!(c.general.max_concurrent_agents, 2);
        assert!(c.general.scrub_api_key_env, "unset fields keep defaults");
        let claude = c.provider("claude");
        assert_eq!(claude.executable.as_deref(), Some("/opt/bin/claude"));
        assert_eq!(claude.timeout(), Some(Duration::from_secs(120)));
        assert!(!c.provider("google").enabled);
        assert!(
            c.provider("codex").enabled,
            "unlisted providers use defaults"
        );
    }

    #[test]
    fn shipped_example_config_stays_valid() {
        let c = Config::from_toml(include_str!("../ai-orchestra.example.toml")).unwrap();
        assert_eq!(
            c.general.timeout_secs,
            GeneralConfig::default().timeout_secs
        );
        assert!(c.general.scrub_api_key_env);
        assert!(!c.general.allow_nested);
        assert_eq!(c.debate.rounds, DebateConfig::default().rounds);
    }

    #[test]
    fn history_location_comes_from_config_or_the_data_dir() {
        let c = Config::from_toml("[debate]\nhistory_dir = \"/tmp/orchestra-debates\"").unwrap();
        assert_eq!(
            c.debate.resolved_history_dir(),
            Some(PathBuf::from("/tmp/orchestra-debates"))
        );
        assert!(c.debate.save);
        let c = Config::from_toml("[debate]\nsave = false").unwrap();
        assert!(!c.debate.save);
        // Without an override the path ends in the app's own directory.
        if let Some(dir) = Config::default().debate.resolved_history_dir() {
            assert!(dir.ends_with("ai-orchestra/debates"), "{}", dir.display());
        }
    }

    #[test]
    fn mcp_and_depth_settings_have_safe_defaults_and_are_validated() {
        let c = Config::default();
        assert_eq!(c.general.max_depth, 1);
        assert!(c.mcp.host.is_none() && !c.mcp.allow_host_agent && c.mcp.isolate_children);
        assert!(c.mcp.inline_wait_secs <= c.mcp.max_wait_secs);
        let c = Config::from_toml(
            "[mcp]\nhost = \"claude\"\nmax_concurrent_tasks = 2\n[general]\nmax_depth = 2",
        )
        .unwrap();
        assert_eq!(
            (
                c.mcp.host.as_deref(),
                c.mcp.max_concurrent_tasks,
                c.general.max_depth
            ),
            (Some("claude"), 2, 2)
        );
        for bad in [
            "[mcp]\nmax_concurrent_tasks = 0",
            "[mcp]\nmax_task_secs = 0",
            "[mcp]\nmax_result_bytes = 10",
            "[mcp]\ninline_wait_secs = 99\nmax_wait_secs = 10",
            "[mcp]\nbogus = 1",
        ] {
            assert!(Config::from_toml(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn rejects_unknown_keys_and_bad_limits() {
        assert!(Config::from_toml("[general]\nbogus = 1").is_err());
        assert!(Config::from_toml("[general]\nmax_concurrent_agents = 0").is_err());
        assert!(Config::from_toml("[general]\ntimeout_secs = 0").is_err());
        assert!(Config::from_toml("[providers.claude]\ntimeout_secs = 0").is_err());
        assert!(Config::from_toml("[debate]\nrounds = 0").is_err());
        assert!(Config::from_toml("[debate]\nrounds = 3").is_err());
        assert!(Config::from_toml("[debate]\ntotal_timeout_secs = 0").is_err());
        assert_eq!(
            Config::from_toml(
                "[debate]
rounds = 2"
            )
            .unwrap()
            .debate
            .rounds,
            2
        );
        assert_eq!(
            Config::default().debate.rounds,
            1,
            "one review round by default"
        );
        assert!(Config::from_toml("not toml ===").is_err());
    }

    #[test]
    fn missing_explicit_file_is_an_error() {
        let err = Config::load(Some(Path::new("/definitely/not/here.toml"))).unwrap_err();
        assert!(matches!(err, OrchestraError::Config(_)));
    }

    #[test]
    fn tilde_expansion_leaves_other_paths_alone() {
        assert_eq!(expand_tilde("/usr/bin/claude"), "/usr/bin/claude");
        assert_eq!(expand_tilde("claude"), "claude");
    }

    #[test]
    fn provider_effort_parses() {
        let c = Config::from_toml("[providers.codex]\nmodel = \"gpt-6-luna\"\neffort = \"low\"")
            .unwrap();
        assert_eq!(c.provider("codex").effort.as_deref(), Some("low"));
        assert_eq!(c.provider("claude").effort, None);
    }
}
