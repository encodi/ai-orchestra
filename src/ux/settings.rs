//! The settings of an interactive session, in layers.
//!
//! Every setting is optional: `None` is AUTO, the engine chooses and explains. A value that is set is
//! never changed by the engine. Precedence, strongest first:
//!
//! 1. **Safety restrictions**: not settings. Agents are read-only, the host agent is never called
//!    through MCP, limits and the recursion guard apply. Nothing here can relax them.
//! 2. **Explicit per-request settings** (`/ask --mode decision ...`): this question only.
//! 3. **Explicit session settings** (`/mode decision`): until `/auto` or the end of the session.
//! 4. **Saved user preferences** (`/save`): the starting point of every new session.
//! 5. **Automatic defaults**: whatever the router, the strategy engine and the configuration decide.
//!
//! Values are validated with the same parsers as the command line, so a typo is refused when it is
//! typed, not when the question is asked.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::consult::{
    ConsultOptions, parse_agents_arg, parse_coordinator_arg, parse_length_arg, parse_mode_arg,
    parse_rounds_arg,
};
use crate::error::{OrchestraError, Result};
use crate::memory::model::{MemoryMode, parse_memory_arg};
use crate::memory::service::MemoryRequest;
use crate::research::plan::{Depth, ResearchRequest, WebMode};
use crate::smart::strategy::parse_strategy_arg;

/// What can never be relaxed from a setting.
pub const SAFETY_NOTES: [&str; 4] = [
    "agents are read-only: they can read files in the working directory but cannot change anything",
    "the agent that hosts an MCP server is never called through it",
    "limits (time, calls, depth of nesting) and the recursion guard always apply",
    "nothing an agent or a web page says is ever run",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Key {
    Mode,
    Coordinator,
    Agents,
    Rounds,
    Depth,
    Web,
    Strategy,
    Length,
    Timeout,
    MaxCalls,
    Memory,
    ContextBudget,
    View,
    MaxSources,
    FreshnessDays,
    MaxDuration,
    MaxParallel,
}

impl Key {
    pub const ALL: [Key; 17] = [
        Key::Mode,
        Key::Coordinator,
        Key::Agents,
        Key::Rounds,
        Key::Depth,
        Key::Web,
        Key::Strategy,
        Key::Length,
        Key::Timeout,
        Key::MaxCalls,
        Key::Memory,
        Key::ContextBudget,
        Key::View,
        Key::MaxSources,
        Key::FreshnessDays,
        Key::MaxDuration,
        Key::MaxParallel,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Key::Mode => "mode",
            Key::Coordinator => "coordinator",
            Key::Agents => "agents",
            Key::Rounds => "rounds",
            Key::Depth => "depth",
            Key::Web => "web",
            Key::Strategy => "strategy",
            Key::Length => "length",
            Key::Timeout => "timeout",
            Key::MaxCalls => "max-calls",
            Key::Memory => "memory",
            Key::ContextBudget => "context-budget",
            Key::View => "view",
            Key::MaxSources => "max-sources",
            Key::FreshnessDays => "freshness-days",
            Key::MaxDuration => "max-duration",
            Key::MaxParallel => "max-parallel",
        }
    }

    pub fn parse(name: &str) -> Option<Key> {
        let n = name.trim().trim_start_matches('-').to_lowercase();
        Key::ALL.into_iter().find(|k| {
            k.name() == n
                || (*k == Key::MaxCalls && (n == "max_calls" || n == "budget"))
                || (*k == Key::ContextBudget && n == "context_budget")
                || (*k == Key::Timeout && n == "timeout-secs")
                || (*k == Key::MaxSources && n == "max_sources")
                || (*k == Key::FreshnessDays && n == "freshness_days")
                || (*k == Key::MaxDuration && n == "max_duration")
                || (*k == Key::MaxParallel && n == "max_parallel")
        })
    }

    /// What the setting accepts, for `/help` and error messages.
    pub fn accepts(self) -> &'static str {
        match self {
            Key::Mode => "auto, decision, research, review, learn, debate",
            Key::Coordinator => "auto, claude, codex, google",
            Key::Agents => "auto, or a list such as claude,codex,google",
            Key::Rounds => "auto, 1, 2",
            Key::Depth => "auto, quick, standard, deep",
            Key::Web => "auto, on, off",
            Key::Strategy => "auto, fast, balanced, thorough",
            Key::Length => "auto, brief, full",
            Key::Timeout => "auto, or seconds per agent call",
            Key::MaxCalls => "auto, or the most agent calls one consultation may start",
            Key::Memory => "auto, off, session, project, global",
            Key::ContextBudget => "auto, or estimated tokens of recalled context",
            Key::View => "auto (plain), plain, detailed",
            Key::MaxSources => "auto, or the most sources to retrieve (1-20)",
            Key::FreshnessDays => "auto, or flag sources older than this many days (1-3650)",
            Key::MaxDuration => "auto, or stop starting new work after this many seconds (1-3600)",
            Key::MaxParallel => "auto, or the most agent calls running at once (1-8)",
        }
    }
}

/// Where the value in force came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// Nothing was set: the engine chooses.
    Auto,
    Preference,
    Session,
    Request,
}

impl Origin {
    pub fn label(self) -> &'static str {
        match self {
            Origin::Auto => "AUTO",
            Origin::Preference => "saved preference",
            Origin::Session => "this session",
            Origin::Request => "this question only",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub mode: Option<String>,
    pub coordinator: Option<String>,
    pub agents: Option<Vec<String>>,
    pub rounds: Option<String>,
    pub depth: Option<String>,
    pub web: Option<String>,
    pub strategy: Option<String>,
    pub length: Option<String>,
    pub timeout: Option<u64>,
    pub max_calls: Option<usize>,
    pub memory: Option<String>,
    pub context_budget: Option<usize>,
    pub view: Option<String>,
    pub max_sources: Option<usize>,
    pub freshness_days: Option<u32>,
    pub max_duration: Option<u64>,
    pub max_parallel: Option<usize>,
}

fn bad(key: Key, value: &str) -> OrchestraError {
    OrchestraError::Config(format!(
        "`{value}` is not valid for {}; it accepts: {}",
        key.name(),
        key.accepts()
    ))
}

fn valid_agent_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 32
        && n.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

fn is_auto(v: &str) -> bool {
    v.trim().eq_ignore_ascii_case("auto") || v.trim().is_empty()
}

impl Settings {
    pub fn is_empty(&self) -> bool {
        *self == Settings::default()
    }

    /// Sets (or, for `auto`, clears) one setting after validating it.
    pub fn set(&mut self, key: Key, value: &str) -> Result<()> {
        let value = value.trim();
        if is_auto(value) {
            self.clear(key);
            return Ok(());
        }
        match key {
            Key::Mode => {
                parse_mode_arg(Some(value)).map_err(|_| bad(key, value))?;
                self.mode = Some(value.to_lowercase());
            }
            // Whether the names are agents that exist is checked against the registry by whoever
            // holds one (the chat engine, the router); here only their shape is.
            Key::Coordinator => {
                let name = value.to_lowercase();
                if !valid_agent_name(&name) {
                    return Err(bad(key, value));
                }
                self.coordinator = Some(name);
            }
            Key::Agents => {
                let list = parse_agents_arg(&[value.to_string()]).map_err(|_| bad(key, value))?;
                match list {
                    Some(list) if list.iter().all(|a| valid_agent_name(&a.to_lowercase())) => {
                        self.agents = Some(list.into_iter().map(|a| a.to_lowercase()).collect());
                    }
                    _ => return Err(bad(key, value)),
                }
            }
            Key::Rounds => {
                parse_rounds_arg(Some(value)).map_err(|_| bad(key, value))?;
                self.rounds = Some(value.to_string());
            }
            Key::Depth => {
                Depth::parse(Some(value)).map_err(|_| bad(key, value))?;
                self.depth = Some(value.to_lowercase());
            }
            Key::Web => {
                WebMode::parse(Some(value)).map_err(|_| bad(key, value))?;
                self.web = Some(value.to_lowercase());
            }
            Key::Strategy => {
                parse_strategy_arg(Some(value)).map_err(|_| bad(key, value))?;
                self.strategy = Some(value.to_lowercase());
            }
            Key::Length => {
                parse_length_arg(Some(value)).map_err(|_| bad(key, value))?;
                self.length = Some(value.to_lowercase());
            }
            Key::Timeout => {
                let n: u64 = value.parse().map_err(|_| bad(key, value))?;
                if n == 0 || n > 3600 {
                    return Err(bad(key, value));
                }
                self.timeout = Some(n);
            }
            Key::MaxCalls => {
                let n: usize = value.parse().map_err(|_| bad(key, value))?;
                if n == 0 || n > 50 {
                    return Err(bad(key, value));
                }
                self.max_calls = Some(n);
            }
            Key::Memory => {
                parse_memory_arg(Some(value)).map_err(|_| bad(key, value))?;
                self.memory = Some(value.to_lowercase());
            }
            Key::ContextBudget => {
                let n: usize = value.parse().map_err(|_| bad(key, value))?;
                if n == 0 || n > 100_000 {
                    return Err(bad(key, value));
                }
                self.context_budget = Some(n);
            }
            Key::MaxSources => {
                let n: usize = value.parse().map_err(|_| bad(key, value))?;
                if !(1..=20).contains(&n) {
                    return Err(bad(key, value));
                }
                self.max_sources = Some(n);
            }
            Key::FreshnessDays => {
                let n: u32 = value.parse().map_err(|_| bad(key, value))?;
                if !(1..=3650).contains(&n) {
                    return Err(bad(key, value));
                }
                self.freshness_days = Some(n);
            }
            Key::MaxDuration => {
                let n: u64 = value.parse().map_err(|_| bad(key, value))?;
                if !(1..=3600).contains(&n) {
                    return Err(bad(key, value));
                }
                self.max_duration = Some(n);
            }
            Key::MaxParallel => {
                let n: usize = value.parse().map_err(|_| bad(key, value))?;
                if !(1..=8).contains(&n) {
                    return Err(bad(key, value));
                }
                self.max_parallel = Some(n);
            }
            Key::View => match value.to_lowercase().as_str() {
                "plain" | "detailed" => self.view = Some(value.to_lowercase()),
                _ => return Err(bad(key, value)),
            },
        }
        Ok(())
    }

    pub fn clear(&mut self, key: Key) {
        match key {
            Key::Mode => self.mode = None,
            Key::Coordinator => self.coordinator = None,
            Key::Agents => self.agents = None,
            Key::Rounds => self.rounds = None,
            Key::Depth => self.depth = None,
            Key::Web => self.web = None,
            Key::Strategy => self.strategy = None,
            Key::Length => self.length = None,
            Key::Timeout => self.timeout = None,
            Key::MaxCalls => self.max_calls = None,
            Key::Memory => self.memory = None,
            Key::ContextBudget => self.context_budget = None,
            Key::View => self.view = None,
            Key::MaxSources => self.max_sources = None,
            Key::FreshnessDays => self.freshness_days = None,
            Key::MaxDuration => self.max_duration = None,
            Key::MaxParallel => self.max_parallel = None,
        }
    }

    pub fn get(&self, key: Key) -> Option<String> {
        match key {
            Key::Mode => self.mode.clone(),
            Key::Coordinator => self.coordinator.clone(),
            Key::Agents => self.agents.as_ref().map(|a| a.join(",")),
            Key::Rounds => self.rounds.clone(),
            Key::Depth => self.depth.clone(),
            Key::Web => self.web.clone(),
            Key::Strategy => self.strategy.clone(),
            Key::Length => self.length.clone(),
            Key::Timeout => self.timeout.map(|t| t.to_string()),
            Key::MaxCalls => self.max_calls.map(|t| t.to_string()),
            Key::Memory => self.memory.clone(),
            Key::ContextBudget => self.context_budget.map(|t| t.to_string()),
            Key::View => self.view.clone(),
            Key::MaxSources => self.max_sources.map(|t| t.to_string()),
            Key::FreshnessDays => self.freshness_days.map(|t| t.to_string()),
            Key::MaxDuration => self.max_duration.map(|t| t.to_string()),
            Key::MaxParallel => self.max_parallel.map(|t| t.to_string()),
        }
    }

    /// `over` wins wherever it has a value.
    pub fn overlay(&self, over: &Settings) -> Settings {
        let mut out = self.clone();
        for key in Key::ALL {
            if let Some(v) = over.get(key) {
                // Values in `over` were validated when they were set.
                let _ = out.set(key, &v);
            }
        }
        out
    }

    /// Parses `--key value` pairs at the start of `text` (a per-request override) and returns them
    /// with the rest of the text.
    pub fn parse_request_prefix(text: &str) -> Result<(Settings, String)> {
        let mut over = Settings::default();
        let mut rest = text.trim_start();
        while let Some(after) = rest.strip_prefix("--") {
            let (name, tail) = after.split_once(char::is_whitespace).unwrap_or((after, ""));
            let key = Key::parse(name).ok_or_else(|| {
                OrchestraError::Config(format!(
                    "unknown setting `--{name}` (try /settings for the list)"
                ))
            })?;
            let tail = tail.trim_start();
            let (value, tail) = tail.split_once(char::is_whitespace).unwrap_or((tail, ""));
            if value.is_empty() {
                return Err(OrchestraError::Config(format!(
                    "--{name} needs a value ({})",
                    key.accepts()
                )));
            }
            over.set(key, value)?;
            rest = tail.trim_start();
        }
        Ok((over, rest.to_string()))
    }
}

/// The three layers an interactive session keeps.
#[derive(Debug, Clone, Default)]
pub struct Layers {
    pub preferences: Settings,
    pub session: Settings,
}

/// The settings in force for one request, and where each came from.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub settings: Settings,
    pub origins: BTreeMap<Key, Origin>,
}

impl Layers {
    pub fn resolve(&self, request: &Settings) -> Resolved {
        let mut settings = Settings::default();
        let mut origins = BTreeMap::new();
        for key in Key::ALL {
            let layered = [
                (Origin::Request, request),
                (Origin::Session, &self.session),
                (Origin::Preference, &self.preferences),
            ]
            .into_iter()
            .find_map(|(origin, s)| s.get(key).map(|v| (origin, v)));
            match layered {
                Some((origin, v)) => {
                    let _ = settings.set(key, &v);
                    origins.insert(key, origin);
                }
                None => {
                    origins.insert(key, Origin::Auto);
                }
            }
        }
        Resolved { settings, origins }
    }
}

impl Resolved {
    /// The settings as a table: what is set, by whom, and what AUTO leaves to the engine.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for key in Key::ALL {
            let origin = self.origins.get(&key).copied().unwrap_or(Origin::Auto);
            let value = self.settings.get(key);
            out.push_str(&format!(
                "  {:<15} {:<28} {}\n",
                key.name(),
                value.unwrap_or_else(|| "AUTO".to_string()),
                match origin {
                    Origin::Auto => String::new(),
                    o => format!("({})", o.label()),
                }
            ));
        }
        out
    }

    /// Builds the options of a consultation. Everything left AUTO stays `None` for the router.
    pub fn to_options(
        &self,
        question: String,
        config: &Config,
        memory: Option<MemoryRequest>,
    ) -> Result<ConsultOptions> {
        let s = &self.settings;
        let strategy = match parse_strategy_arg(s.strategy.as_deref())? {
            Some(x) => Some(x),
            None => config.smart.strategy()?,
        };
        Ok(ConsultOptions {
            strategy,
            max_agent_calls: s.max_calls,
            max_duration: s.max_duration.map(Duration::from_secs),
            max_parallel: s.max_parallel,
            question,
            mode: parse_mode_arg(s.mode.as_deref())?,
            coordinator: parse_coordinator_arg(s.coordinator.as_deref()),
            agents: match &s.agents {
                Some(a) => parse_agents_arg(a)?,
                None => None,
            },
            rounds: parse_rounds_arg(s.rounds.as_deref())?,
            length: parse_length_arg(s.length.as_deref())?,
            research: ResearchRequest {
                web: WebMode::parse(s.web.as_deref())?,
                depth: Depth::parse(s.depth.as_deref())?,
                max_sources: s.max_sources,
                freshness_days: s.freshness_days,
                ..Default::default()
            },
            timeout: s.timeout.map(Duration::from_secs),
            memory,
            ..Default::default()
        })
    }

    /// The memory mode in force (the configured default when AUTO).
    pub fn memory_mode(&self, config: &Config) -> Result<MemoryMode> {
        match parse_memory_arg(self.settings.memory.as_deref())? {
            Some(m) => Ok(m),
            None => config.memory.mode(),
        }
    }

    pub fn detailed(&self) -> bool {
        self.settings.view.as_deref() == Some("detailed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_validated_when_they_are_typed_and_auto_clears() {
        let mut s = Settings::default();
        s.set(Key::Mode, "Decision").unwrap();
        s.set(Key::Agents, "claude, codex").unwrap();
        s.set(Key::Rounds, "2").unwrap();
        assert_eq!(s.get(Key::Mode).as_deref(), Some("decision"));
        assert_eq!(s.get(Key::Agents).as_deref(), Some("claude,codex"));
        for (k, v) in [
            (Key::Mode, "debates"),
            (Key::Rounds, "3"),
            (Key::Agents, "claude,gp t"),
            (Key::Timeout, "0"),
            (Key::MaxCalls, "100"),
            (Key::Strategy, "turbo"),
            (Key::Memory, "sometimes"),
            (Key::View, "fancy"),
            (Key::Coordinator, "not an agent!"),
        ] {
            let before = s.clone();
            assert!(s.set(k, v).is_err(), "{} {v}", k.name());
            assert_eq!(s, before, "a refused value changes nothing");
        }
        s.set(Key::Mode, "auto").unwrap();
        assert_eq!(s.get(Key::Mode), None);
    }

    #[test]
    fn precedence_is_request_then_session_then_preference_then_auto() {
        let layers = Layers {
            preferences: Settings {
                mode: Some("learn".into()),
                strategy: Some("fast".into()),
                web: Some("off".into()),
                ..Default::default()
            },
            session: Settings {
                mode: Some("decision".into()),
                strategy: Some("balanced".into()),
                ..Default::default()
            },
        };
        let request = Settings {
            strategy: Some("thorough".into()),
            ..Default::default()
        };
        let r = layers.resolve(&request);
        assert_eq!(r.settings.strategy.as_deref(), Some("thorough"));
        assert_eq!(r.origins[&Key::Strategy], Origin::Request);
        assert_eq!(r.settings.mode.as_deref(), Some("decision"));
        assert_eq!(r.origins[&Key::Mode], Origin::Session);
        assert_eq!(r.settings.web.as_deref(), Some("off"));
        assert_eq!(r.origins[&Key::Web], Origin::Preference);
        assert_eq!(r.origins[&Key::Coordinator], Origin::Auto);
        assert!(r.render().contains("AUTO"));
        assert!(r.render().contains("(this question only)"));
    }

    #[test]
    fn a_request_prefix_is_parsed_and_the_rest_is_the_question() {
        let (over, rest) =
            Settings::parse_request_prefix("--mode review --strategy thorough Which scope?")
                .unwrap();
        assert_eq!(over.mode.as_deref(), Some("review"));
        assert_eq!(over.strategy.as_deref(), Some("thorough"));
        assert_eq!(rest, "Which scope?");
        assert!(Settings::parse_request_prefix("--mood review x").is_err());
        assert!(Settings::parse_request_prefix("--mode").is_err());
        let (none, q) = Settings::parse_request_prefix("a plain question").unwrap();
        assert!(none.is_empty() && q == "a plain question");
    }

    #[test]
    fn options_carry_what_was_set_and_leave_the_rest_to_the_router() {
        let mut session = Settings::default();
        session.set(Key::Mode, "decision").unwrap();
        session.set(Key::Web, "off").unwrap();
        session.set(Key::MaxCalls, "4").unwrap();
        let layers = Layers {
            session,
            ..Default::default()
        };
        let r = layers.resolve(&Settings::default());
        let o = r.to_options("q".into(), &Config::default(), None).unwrap();
        assert!(
            o.mode.is_some() && o.coordinator.is_none() && o.agents.is_none() && o.rounds.is_none()
        );
        assert_eq!(o.max_agent_calls, Some(4));
        assert_eq!(o.research.web, WebMode::Off);
    }
}
