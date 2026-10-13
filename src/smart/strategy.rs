//! The vocabulary of smart orchestration: strategies, execution flows and the plan that the
//! [`SmartStrategyEngine`](super::engine::SmartStrategyEngine) attaches to a routing decision.

use std::str::FromStr;

use serde::{Deserialize, Serialize};

use super::budget::BudgetLimits;
use crate::consult::Routed;
use crate::error::{OrchestraError, Result};
use crate::types::ProviderId;

/// How much AI work a consultation should use. Strategies set defaults for whatever the user did
/// not set explicitly; they never override an explicit choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Strategy {
    /// Choose one of the other three from the question, the available agents and the budgets.
    Auto,
    /// Lowest latency and fewest calls: one agent, no review. Essential evidence checks still run.
    Fast,
    /// Quality, speed and quota in balance: one agent answers, and an independent review or a
    /// further agent is added only when something in the result calls for it.
    Balanced,
    /// Comprehensive: several independent perspectives, evidence verification and up to two
    /// cross-review rounds, within explicit limits.
    Thorough,
}

impl Strategy {
    pub const ALL: [Strategy; 4] = [
        Strategy::Auto,
        Strategy::Fast,
        Strategy::Balanced,
        Strategy::Thorough,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Strategy::Auto => "auto",
            Strategy::Fast => "fast",
            Strategy::Balanced => "balanced",
            Strategy::Thorough => "thorough",
        }
    }
}

impl std::fmt::Display for Strategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Strategy {
    type Err = OrchestraError;

    fn from_str(s: &str) -> Result<Self> {
        let wanted = s.trim().to_lowercase();
        Strategy::ALL
            .into_iter()
            .find(|m| m.as_str() == wanted)
            .ok_or_else(|| {
                OrchestraError::Config(format!(
                    "unknown strategy `{s}` (use auto, fast, balanced or thorough)"
                ))
            })
    }
}

/// `--strategy`: nothing means "the configured default".
pub fn parse_strategy_arg(value: Option<&str>) -> Result<Option<Strategy>> {
    match value.map(str::trim) {
        None | Some("") => Ok(None),
        Some(v) => v.parse().map(Some),
    }
}

/// `[smart] default_strategy`: a strategy, or `off` for the fixed orchestration of Milestones 4
/// and 5 (the baseline the benchmark compares against).
pub fn parse_default_strategy(value: &str) -> Result<Option<Strategy>> {
    let v = value.trim();
    if v.eq_ignore_ascii_case("off") || v.eq_ignore_ascii_case("legacy") {
        Ok(None)
    } else {
        v.parse().map(Some).map_err(|_| {
            OrchestraError::Config(format!(
                "smart.default_strategy must be auto, fast, balanced, thorough or off, got `{v}`"
            ))
        })
    }
}

/// How the agents are scheduled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Flow {
    /// One agent writes the final answer (the evidence is verified if there is research).
    Single,
    /// One agent answers; an independent review, another independent perspective or a second
    /// round are added one step at a time, only while the results call for them.
    Adaptive,
    /// Every participant answers independently in parallel, then the review rounds, then the
    /// synthesis: the Milestone 4 workflow.
    Panel,
    /// The Milestone 2 debate engine.
    Debate,
}

impl Flow {
    pub fn as_str(self) -> &'static str {
        match self {
            Flow::Single => "single",
            Flow::Adaptive => "adaptive",
            Flow::Panel => "panel",
            Flow::Debate => "debate",
        }
    }
}

impl std::fmt::Display for Flow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Features of the question that the automatic strategy looks at. Wording features only: they
/// say how large and how contested the question looks, not whether anything is true.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionFeatures {
    pub words: usize,
    pub simple: bool,
    pub contested: bool,
    /// Long, or made of several separate questions.
    pub complex: bool,
    /// Mentions money, health, safety, law or a long-lived commitment.
    pub high_stakes: bool,
}

/// What the engine decided, and why. Saved with the consultation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmartPlan {
    /// `None` when no strategy is in force (only limits were given with the fixed Milestone 4/5
    /// orchestration).
    pub strategy: Option<Routed<Strategy>>,
    pub flow: Flow,
    /// Agents that may be added when the results call for it, best first (adaptive flow only).
    pub escalation_order: Vec<ProviderId>,
    /// The limits enforced while the consultation runs.
    pub limits: BudgetLimits,
    /// Most agent calls the plan can make, with research included.
    pub max_calls: usize,
    /// Calls when nothing triggers an escalation.
    pub typical_calls: usize,
    pub features: QuestionFeatures,
    /// One line per decision, in plain words.
    pub explanation: Vec<String>,
    /// What the provider history contributed (or why it was not used).
    pub profile_notes: Vec<String>,
}

impl SmartPlan {
    pub fn strategy_value(&self) -> Option<Strategy> {
        self.strategy.as_ref().map(|s| s.value)
    }

    pub fn render(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        let _ = match &self.strategy {
            Some(s) => writeln!(
                out,
                "strategy:     {} ({}: {})",
                s.value,
                match s.source {
                    crate::consult::Source::Auto => "auto",
                    crate::consult::Source::Manual => "manual",
                },
                s.reason
            ),
            None => writeln!(
                out,
                "strategy:     none (fixed Milestone 4/5 orchestration)"
            ),
        };
        let _ = writeln!(out, "flow:         {}", self.flow);
        let _ = writeln!(
            out,
            "calls:        {} expected, at most {}",
            self.typical_calls, self.max_calls
        );
        let _ = writeln!(out, "limits:       {}", self.limits.describe());
        for line in &self.explanation {
            let _ = writeln!(out, "why:          {line}");
        }
        for line in &self.profile_notes {
            let _ = writeln!(out, "history:      {line}");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strategies_parse_case_insensitively_and_reject_unknown_names() {
        assert_eq!("FAST".parse::<Strategy>().unwrap(), Strategy::Fast);
        assert_eq!(parse_strategy_arg(None).unwrap(), None);
        assert_eq!(
            parse_strategy_arg(Some(" thorough ")).unwrap(),
            Some(Strategy::Thorough)
        );
        assert!(parse_strategy_arg(Some("turbo")).is_err());
        assert!(
            parse_strategy_arg(Some("off")).is_err(),
            "`off` is config only"
        );
    }

    #[test]
    fn the_configured_default_can_switch_the_strategies_off() {
        assert_eq!(
            parse_default_strategy("auto").unwrap(),
            Some(Strategy::Auto)
        );
        assert_eq!(parse_default_strategy("OFF").unwrap(), None);
        assert_eq!(parse_default_strategy("legacy").unwrap(), None);
        assert!(parse_default_strategy("fastest").is_err());
    }
}
