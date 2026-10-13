//! Deciding whether and how to research: web access, depth and limits.
//!
//! Automatic by default, manual when asked: every setting can be given explicitly and then wins.
//! `--web off` guarantees no network use for research; `--web on` fails clearly when there is no
//! way to find or fetch sources. The automatic rules are fixed keyword rules (no model call) and
//! are shown to the user with the reason for each choice.

use serde::{Deserialize, Serialize};

use super::net::{FetchPolicy, parse_public_url};
use crate::consult::{ConsultMode, Routed};
use crate::error::{OrchestraError, Result};
use crate::types::ProviderId;

/// Hard limits, whatever the settings say.
pub const MAX_SOURCES_LIMIT: usize = 20;
pub const MAX_USER_URLS: usize = 20;
pub const MAX_FRESHNESS_DAYS: u32 = 3650;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebMode {
    Auto,
    On,
    Off,
}

impl WebMode {
    pub fn parse(value: Option<&str>) -> Result<Self> {
        match value.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
            None | Some("") | Some("auto") => Ok(Self::Auto),
            Some("on") => Ok(Self::On),
            Some("off") => Ok(Self::Off),
            Some(v) => Err(OrchestraError::Config(format!(
                "unknown --web value `{v}` (use auto, on or off)"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Depth {
    Quick,
    Standard,
    Deep,
}

impl Depth {
    pub fn parse(value: Option<&str>) -> Result<Option<Self>> {
        match value.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
            None | Some("") | Some("auto") => Ok(None),
            Some("quick") => Ok(Some(Self::Quick)),
            Some("standard") => Ok(Some(Self::Standard)),
            Some("deep") => Ok(Some(Self::Deep)),
            Some(v) => Err(OrchestraError::Config(format!(
                "unknown --depth value `{v}` (use auto, quick, standard or deep)"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Quick => "quick",
            Self::Standard => "standard",
            Self::Deep => "deep",
        }
    }

    pub fn profile(self) -> DepthProfile {
        match self {
            Self::Quick => DepthProfile {
                max_sources: 3,
                max_claims: 4,
                verifier_calls: 0,
                passages_per_claim: 2,
                evidence_chars_per_source: 1500,
            },
            Self::Standard => DepthProfile {
                max_sources: 6,
                max_claims: 8,
                verifier_calls: 1,
                passages_per_claim: 3,
                evidence_chars_per_source: 2500,
            },
            Self::Deep => DepthProfile {
                max_sources: 12,
                max_claims: 12,
                verifier_calls: 2,
                passages_per_claim: 4,
                evidence_chars_per_source: 3500,
            },
        }
    }
}

/// What a depth level means in concrete limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DepthProfile {
    /// Default number of sources to retrieve.
    pub max_sources: usize,
    /// Claims verified at most.
    pub max_claims: usize,
    /// Calls to judge claims against passages (0 = passages are found but not judged).
    pub verifier_calls: usize,
    pub passages_per_claim: usize,
    /// Characters of each source placed in the evidence given to the agents.
    pub evidence_chars_per_source: usize,
}

/// What the user asked for.
#[derive(Debug, Clone)]
pub struct ResearchRequest {
    pub web: WebMode,
    pub depth: Option<Depth>,
    pub max_sources: Option<usize>,
    /// HTTPS URLs to use as sources.
    pub urls: Vec<String>,
    /// Sources older than this many days are flagged as stale.
    pub freshness_days: Option<u32>,
}

impl Default for ResearchRequest {
    fn default() -> Self {
        Self {
            web: WebMode::Auto,
            depth: None,
            max_sources: None,
            urls: Vec::new(),
            freshness_days: None,
        }
    }
}

/// The routed research settings, saved with the consultation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResearchDecision {
    pub requested_web: WebMode,
    /// Whether the research layer runs at all (sources are gathered and checked).
    pub active: Routed<bool>,
    pub depth: Routed<Depth>,
    pub max_sources: Routed<usize>,
    pub urls: Vec<String>,
    pub freshness_days: Option<u32>,
    /// The agent asked to search the web, when discovery by search will happen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery_agent: Option<ProviderId>,
    /// What cannot be done and why (no search-capable agent, web off, ...).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitations: Vec<String>,
}

impl ResearchDecision {
    /// Research is not part of this consultation.
    pub fn inactive(web: WebMode, reason: &str) -> Self {
        Self {
            requested_web: web,
            active: Routed::auto(false, reason),
            depth: Routed::auto(Depth::Standard, "research is not active"),
            max_sources: Routed::auto(0, "research is not active"),
            urls: Vec::new(),
            freshness_days: None,
            discovery_agent: None,
            limitations: Vec::new(),
        }
    }

    pub fn is_active(&self) -> bool {
        self.active.value
    }

    /// Whether a planning call to a search-capable agent will happen.
    pub fn discovers_by_search(&self) -> bool {
        self.is_active() && self.discovery_agent.is_some()
    }

    /// Extra agent calls research adds at most: one planning call and the verification calls.
    pub fn extra_calls(&self) -> usize {
        if !self.is_active() {
            return 0;
        }
        usize::from(self.discovers_by_search()) + self.depth.value.profile().verifier_calls
    }
}

fn normalize(question: &str) -> String {
    let mut s = String::from(" ");
    for c in question.to_lowercase().chars() {
        s.push(if c.is_alphanumeric() || c == '\'' {
            c
        } else {
            ' '
        });
    }
    s.push(' ');
    s
}

fn has_any(haystack: &str, cues: &[&str]) -> Option<String> {
    cues.iter()
        .find(|c| match c.strip_suffix('*') {
            Some(prefix) => haystack.contains(&format!(" {prefix}")),
            None => haystack.contains(&format!(" {c} ")),
        })
        .map(|c| c.trim_end_matches('*').to_string())
}

/// Wording that needs facts from outside the model: recent events, prices, versions, studies,
/// claims to check, product specifications.
const NEEDS_WEB: &[&str] = &[
    "latest",
    "recent*",
    "current*",
    "today",
    "yesterday",
    "this week",
    "this year",
    "news",
    "announced",
    "announcement*",
    "release*",
    "released",
    "price*",
    "cost of",
    "how much",
    "version*",
    "update*",
    "2024",
    "2025",
    "2026",
    "2027",
    "specs",
    "specifications",
    "datasheet*",
    "is it true",
    "is this true",
    "actually true",
    "fact check*",
    "fact-check*",
    "verify",
    "claim*",
    "source*",
    "cite",
    "citation*",
    "reference*",
    "study",
    "studies",
    "paper*",
    "research*",
    "evidence",
    "statistic*",
    "review*",
    "benchmark*",
    "compare the specs",
    "últim*",
    "reciente*",
    "noticias",
    "novedad*",
    "actualidad",
    "precio*",
    "verifica*",
    "fuente*",
];

/// Wording that calls for a thorough check.
const DEEP_CUES: &[&str] = &[
    "verify",
    "fact check*",
    "fact-check*",
    "is it true",
    "is this true",
    "actually true",
    "contradict*",
    "controvers*",
    "conflicting",
    "myth*",
    "claim*",
    "debunk*",
    "peer review*",
    "verifica*",
];

/// Wording that asks for the newest information ("latest", "current", "today"): sources are then
/// ranked by how recent they are as well as by relevance.
const WANTS_FRESH: &[&str] = &[
    "latest",
    "newest",
    "recent*",
    "current*",
    "today",
    "yesterday",
    "this week",
    "this year",
    "now",
    "new release*",
    "just announced",
    "últim*",
    "reciente*",
    "actual*",
    "hoy",
];

pub fn wants_fresh(question: &str) -> bool {
    has_any(&normalize(question), WANTS_FRESH).is_some()
}

/// Whether the question calls for outside facts. Basic educational questions do not.
pub fn needs_web(question: &str, mode: ConsultMode) -> Option<String> {
    let n = normalize(question);
    if let Some(cue) = has_any(&n, NEEDS_WEB) {
        return Some(format!("the question asks about outside facts (\"{cue}\")"));
    }
    match mode {
        ConsultMode::Review => Some(
            "a product or service review depends on current specifications and prices".to_string(),
        ),
        _ => None,
    }
}

pub fn wants_deep(question: &str) -> Option<String> {
    has_any(&normalize(question), DEEP_CUES)
        .map(|c| format!("the question asks to verify or contrast claims (\"{c}\")"))
}

/// Everything the research decision looks at.
pub struct ResearchInput<'a> {
    pub question: &'a str,
    pub mode: ConsultMode,
    /// The question asks for a short answer.
    pub brief: bool,
    pub request: &'a ResearchRequest,
    /// The coordinator first, then the other participants, each with whether it can search.
    pub participants: &'a [(ProviderId, bool)],
    /// What may be fetched: used to reject unusable `--url` values before anything runs.
    pub policy: &'a FetchPolicy,
}

/// Decides the research settings. Errors are for requests that cannot be honoured: `--web on`
/// with no way to find sources, `--web off` together with URLs, or URLs that are not fetchable.
pub fn decide_research(input: &ResearchInput<'_>) -> Result<ResearchDecision> {
    let r = input.request;
    // ---- validate the manual values first, whatever the mode
    if r.urls.len() > MAX_USER_URLS {
        return Err(OrchestraError::Config(format!(
            "at most {MAX_USER_URLS} --url values are allowed"
        )));
    }
    if let Some(n) = r.max_sources
        && !(1..=MAX_SOURCES_LIMIT).contains(&n)
    {
        return Err(OrchestraError::Config(format!(
            "--max-sources must be between 1 and {MAX_SOURCES_LIMIT}"
        )));
    }
    if let Some(d) = r.freshness_days
        && !(1..=MAX_FRESHNESS_DAYS).contains(&d)
    {
        return Err(OrchestraError::Config(format!(
            "--freshness-days must be between 1 and {MAX_FRESHNESS_DAYS}"
        )));
    }
    let mut urls: Vec<String> = Vec::new();
    for raw in &r.urls {
        let url = parse_public_url(raw, input.policy)
            .map_err(|e| OrchestraError::Config(format!("--url `{raw}` cannot be used: {e}")))?;
        let s = url.to_string();
        if !urls.contains(&s) {
            urls.push(s);
        }
    }

    // ---- the debate engine has its own fixed workflow
    if input.mode == ConsultMode::Debate {
        let mut d = ResearchDecision::inactive(r.web, "the debate engine does not do research");
        if r.web == WebMode::On || !urls.is_empty() {
            d.limitations.push("web research was ignored: debate mode runs the fixed debate workflow (use another --mode for research)".to_string());
        }
        return Ok(d);
    }

    // ---- web off: no network research, ever
    if r.web == WebMode::Off {
        if !urls.is_empty() {
            return Err(OrchestraError::Config("--web off contradicts --url: URLs can only be used with the network. Drop one of them".to_string()));
        }
        let mut d = ResearchDecision::inactive(WebMode::Off, "--web off: no network research");
        d.active = Routed::manual(false, "requested with --web off");
        d.limitations.push(
            "web research is off: nothing in this answer was checked against external sources"
                .to_string(),
        );
        return Ok(d);
    }

    let discovery_agent = input
        .participants
        .iter()
        .find(|(_, can)| *can)
        .map(|(id, _)| id.clone());
    let need = needs_web(input.question, input.mode);

    // ---- is research active?
    let (active, mut limitations) = match (r.web, need.as_deref(), !urls.is_empty(), discovery_agent.is_some()) {
        (WebMode::On, _, false, false) => {
            return Err(OrchestraError::Config(
                "--web on needs a way to find or fetch sources, and there is none: no participating agent can search the web (see `ai-orchestra capabilities`) and no --url was given. Add --url, include a search-capable agent in --agents, or use --web off".to_string(),
            ));
        }
        (WebMode::On, _, _, _) => (Routed::manual(true, "requested with --web on"), Vec::new()),
        (_, _, true, _) => (Routed::auto(true, "URLs were provided with --url"), Vec::new()),
        (_, Some(why), false, true) => (Routed::auto(true, why), Vec::new()),
        (_, Some(why), false, false) => (
            Routed::auto(false, format!("{why}, but no participating agent can search the web")),
            vec!["live web discovery is unavailable (no participating agent has a usable web search) and no --url was given, so this answer is not checked against external sources".to_string()],
        ),
        (_, None, false, _) => (Routed::auto(false, "a basic question that does not need outside facts"), Vec::new()),
    };
    if !active.value {
        let mut d = ResearchDecision::inactive(r.web, &active.reason);
        d.active = active;
        d.limitations.append(&mut limitations);
        return Ok(d);
    }

    // ---- depth and limits
    let depth = match r.depth {
        Some(d) => Routed::manual(d, "requested with --depth"),
        None => match wants_deep(input.question) {
            Some(why) => Routed::auto(Depth::Deep, why),
            None if input.brief => {
                Routed::auto(Depth::Quick, "the question asks for a short answer")
            }
            None => Routed::auto(Depth::Standard, "the default"),
        },
    };
    let max_sources = match r.max_sources {
        Some(n) => Routed::manual(n, "requested with --max-sources"),
        None => Routed::auto(
            depth.value.profile().max_sources,
            format!("the default for {} depth", depth.value.as_str()),
        ),
    };
    // Discovery by search happens when the user asked for the web explicitly, or gave no URLs.
    let search_wanted = r.web == WebMode::On || urls.is_empty();
    let discovery_agent = if search_wanted { discovery_agent } else { None };
    if search_wanted && discovery_agent.is_none() {
        limitations.push(
            "no participating agent can search the web, so only the provided URLs are used"
                .to_string(),
        );
    }
    Ok(ResearchDecision {
        requested_web: r.web,
        active,
        depth,
        max_sources,
        urls,
        freshness_days: r.freshness_days,
        discovery_agent,
        limitations,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(items: &[(&str, bool)]) -> Vec<(ProviderId, bool)> {
        items
            .iter()
            .map(|(n, c)| (ProviderId::new(*n), *c))
            .collect()
    }

    fn decide(
        question: &str,
        mode: ConsultMode,
        request: ResearchRequest,
        parts: &[(&str, bool)],
    ) -> Result<ResearchDecision> {
        decide_research(&ResearchInput {
            question,
            mode,
            brief: false,
            request: &request,
            participants: &p(parts),
            policy: &FetchPolicy::default(),
        })
    }

    fn auto(question: &str, mode: ConsultMode, parts: &[(&str, bool)]) -> ResearchDecision {
        decide(question, mode, ResearchRequest::default(), parts).unwrap()
    }

    const CAPABLE: &[(&str, bool)] = &[("claude", true), ("codex", true), ("google", false)];

    #[test]
    fn basic_educational_questions_do_not_browse() {
        for q in [
            "Explain how a transistor works",
            "What is a resistor?",
            "Teach me Ohm's law from scratch",
        ] {
            let d = auto(q, ConsultMode::Learn, CAPABLE);
            assert!(!d.is_active(), "{q}");
            assert_eq!(d.requested_web, WebMode::Auto);
            assert!(
                d.limitations.is_empty(),
                "no limitation is reported when nothing outside is needed: {:?}",
                d.limitations
            );
        }
    }

    #[test]
    fn questions_about_outside_facts_activate_research_with_a_reason() {
        for (q, mode) in [
            (
                "What changed recently in quantum computing?",
                ConsultMode::Research,
            ),
            (
                "Is this scientific claim actually true?",
                ConsultMode::Research,
            ),
            (
                "What is the current price of the Rigol DS1054Z?",
                ConsultMode::Decision,
            ),
            (
                "Compare the specs of two oscilloscopes",
                ConsultMode::Review,
            ),
            (
                "Qué novedades hay en computación cuántica",
                ConsultMode::Research,
            ),
        ] {
            let d = auto(q, mode, CAPABLE);
            assert!(d.is_active(), "{q}: {}", d.active.reason);
            assert_eq!(d.active.source, crate::consult::Source::Auto);
            assert_eq!(
                d.discovery_agent.as_ref().map(ProviderId::as_str),
                Some("claude"),
                "the first capable participant searches"
            );
        }
        // A review always wants current specifications.
        assert!(auto("Which microscope is better?", ConsultMode::Review, CAPABLE).is_active());
    }

    #[test]
    fn depth_is_chosen_from_the_question_and_manual_depth_wins() {
        assert_eq!(
            auto(
                "Verify this claim about batteries",
                ConsultMode::Research,
                CAPABLE
            )
            .depth
            .value,
            Depth::Deep
        );
        assert_eq!(
            auto(
                "What is the latest firmware version of the Rigol DS1054Z?",
                ConsultMode::Research,
                CAPABLE
            )
            .depth
            .value,
            Depth::Standard
        );
        let brief = decide_research(&ResearchInput {
            question: "Latest Rust version in one line?",
            mode: ConsultMode::Research,
            brief: true,
            request: &ResearchRequest::default(),
            participants: &p(CAPABLE),
            policy: &FetchPolicy::default(),
        })
        .unwrap();
        assert_eq!(brief.depth.value, Depth::Quick);
        for manual in [Depth::Quick, Depth::Standard, Depth::Deep] {
            let d = decide(
                "Verify this claim about batteries",
                ConsultMode::Research,
                ResearchRequest {
                    depth: Some(manual),
                    ..Default::default()
                },
                CAPABLE,
            )
            .unwrap();
            assert_eq!(
                (d.depth.value, d.depth.source),
                (manual, crate::consult::Source::Manual)
            );
            assert_eq!(d.max_sources.value, manual.profile().max_sources);
        }
        assert!(Depth::Quick.profile().verifier_calls < Depth::Standard.profile().verifier_calls);
        assert!(Depth::Standard.profile().verifier_calls < Depth::Deep.profile().verifier_calls);
        assert!(Depth::Quick.profile().max_sources < Depth::Deep.profile().max_sources);
    }

    #[test]
    fn web_off_prevents_research_and_is_never_presented_as_verification() {
        let d = decide(
            "What is the latest news on quantum computing?",
            ConsultMode::Research,
            ResearchRequest {
                web: WebMode::Off,
                ..Default::default()
            },
            CAPABLE,
        )
        .unwrap();
        assert!(!d.is_active());
        assert_eq!(
            (d.active.source, d.requested_web),
            (crate::consult::Source::Manual, WebMode::Off)
        );
        assert!(
            d.limitations
                .iter()
                .any(|l| l.contains("nothing in this answer was checked against external sources")),
            "{:?}",
            d.limitations
        );
        assert!(d.discovery_agent.is_none() && d.extra_calls() == 0);
        // Contradictory: URLs need the network.
        let e = decide(
            "q",
            ConsultMode::Research,
            ResearchRequest {
                web: WebMode::Off,
                urls: vec!["https://example.com/a".into()],
                ..Default::default()
            },
            CAPABLE,
        )
        .unwrap_err();
        assert!(e.to_string().contains("--web off contradicts --url"), "{e}");
    }

    #[test]
    fn web_on_fails_clearly_when_there_is_no_way_to_find_sources() {
        let none: &[(&str, bool)] = &[("google", false)];
        let e = decide(
            "Explain batteries",
            ConsultMode::Learn,
            ResearchRequest {
                web: WebMode::On,
                ..Default::default()
            },
            none,
        )
        .unwrap_err()
        .to_string();
        assert!(
            e.contains("--web on needs a way to find or fetch sources") && e.contains("--url"),
            "{e}"
        );
        // With a URL it works, and with a capable agent it works.
        let d = decide(
            "Explain batteries",
            ConsultMode::Learn,
            ResearchRequest {
                web: WebMode::On,
                urls: vec!["https://example.com/a".into()],
                ..Default::default()
            },
            none,
        )
        .unwrap();
        assert!(d.is_active() && d.discovery_agent.is_none());
        let d = decide(
            "Explain batteries",
            ConsultMode::Learn,
            ResearchRequest {
                web: WebMode::On,
                ..Default::default()
            },
            CAPABLE,
        )
        .unwrap();
        assert!(
            d.is_active()
                && d.active.source == crate::consult::Source::Manual
                && d.discovery_agent.is_some()
        );
    }

    #[test]
    fn auto_mode_degrades_honestly_when_discovery_is_unavailable() {
        let none: &[(&str, bool)] = &[("google", false)];
        let d = auto(
            "What is the latest news on quantum computing?",
            ConsultMode::Research,
            none,
        );
        assert!(!d.is_active());
        assert!(
            d.active
                .reason
                .contains("no participating agent can search the web")
        );
        assert!(
            d.limitations
                .iter()
                .any(|l| l.contains("not checked against external sources")),
            "{:?}",
            d.limitations
        );
    }

    #[test]
    fn user_urls_are_validated_deduplicated_and_enough_to_research() {
        let d = decide(
            "Summarize and verify this study",
            ConsultMode::Research,
            ResearchRequest {
                urls: vec![
                    "https://example.org/study".into(),
                    "https://example.org/study".into(),
                    " https://example.org/other ".into(),
                ],
                ..Default::default()
            },
            &[("google", false)],
        )
        .unwrap();
        assert!(d.is_active());
        assert_eq!(
            d.urls,
            ["https://example.org/study", "https://example.org/other"]
        );
        assert!(
            d.discovery_agent.is_none(),
            "with URLs and web auto, only those URLs are used"
        );
        assert!(d.limitations.is_empty(), "{:?}", d.limitations);
        // With --web on, the search agent also looks for more.
        let d = decide(
            "Verify this study",
            ConsultMode::Research,
            ResearchRequest {
                web: WebMode::On,
                urls: vec!["https://example.org/study".into()],
                ..Default::default()
            },
            CAPABLE,
        )
        .unwrap();
        assert!(d.discovery_agent.is_some());
        for bad in [
            "http://example.org/x",
            "https://127.0.0.1/x",
            "https://localhost/x",
            "file:///etc/passwd",
            "https://user:pw@example.org/",
            "not a url",
            "https://169.254.169.254/",
        ] {
            let e = decide(
                "q",
                ConsultMode::Research,
                ResearchRequest {
                    urls: vec![bad.into()],
                    ..Default::default()
                },
                CAPABLE,
            )
            .unwrap_err()
            .to_string();
            assert!(e.contains("cannot be used"), "{bad}: {e}");
        }
    }

    #[test]
    fn limits_are_enforced() {
        for (req, needle) in [
            (
                ResearchRequest {
                    max_sources: Some(0),
                    ..Default::default()
                },
                "--max-sources",
            ),
            (
                ResearchRequest {
                    max_sources: Some(MAX_SOURCES_LIMIT + 1),
                    ..Default::default()
                },
                "--max-sources",
            ),
            (
                ResearchRequest {
                    freshness_days: Some(0),
                    ..Default::default()
                },
                "--freshness-days",
            ),
            (
                ResearchRequest {
                    freshness_days: Some(MAX_FRESHNESS_DAYS + 1),
                    ..Default::default()
                },
                "--freshness-days",
            ),
            (
                ResearchRequest {
                    urls: (0..=MAX_USER_URLS)
                        .map(|i| format!("https://example.org/{i}"))
                        .collect(),
                    ..Default::default()
                },
                "at most",
            ),
        ] {
            let e = decide("latest news", ConsultMode::Research, req, CAPABLE)
                .unwrap_err()
                .to_string();
            assert!(e.contains(needle), "{e}");
        }
        let ok = decide(
            "latest news",
            ConsultMode::Research,
            ResearchRequest {
                max_sources: Some(MAX_SOURCES_LIMIT),
                freshness_days: Some(30),
                ..Default::default()
            },
            CAPABLE,
        )
        .unwrap();
        assert_eq!(
            (ok.max_sources.value, ok.freshness_days),
            (MAX_SOURCES_LIMIT, Some(30))
        );
        assert_eq!(ok.max_sources.source, crate::consult::Source::Manual);
    }

    #[test]
    fn the_debate_engine_ignores_research_and_says_so() {
        let d = decide(
            "Latest news?",
            ConsultMode::Debate,
            ResearchRequest {
                web: WebMode::On,
                ..Default::default()
            },
            CAPABLE,
        )
        .unwrap();
        assert!(!d.is_active());
        assert!(
            d.limitations.iter().any(|l| l.contains("debate mode")),
            "{:?}",
            d.limitations
        );
        assert!(
            !decide(
                "Latest news?",
                ConsultMode::Debate,
                ResearchRequest::default(),
                CAPABLE
            )
            .unwrap()
            .limitations
            .iter()
            .any(|l| l.contains("ignored"))
        );
    }

    #[test]
    fn argument_parsers_accept_auto_and_reject_unknown_values() {
        assert_eq!(WebMode::parse(None).unwrap(), WebMode::Auto);
        assert_eq!(WebMode::parse(Some("ON")).unwrap(), WebMode::On);
        assert_eq!(WebMode::parse(Some("off")).unwrap(), WebMode::Off);
        assert!(WebMode::parse(Some("maybe")).is_err());
        assert_eq!(Depth::parse(Some("auto")).unwrap(), None);
        assert_eq!(Depth::parse(Some("Deep")).unwrap(), Some(Depth::Deep));
        assert!(Depth::parse(Some("infinite")).is_err());
    }

    #[test]
    fn extra_calls_follow_the_depth_and_the_discovery_agent() {
        let d = auto(
            "What is the latest news on quantum computing?",
            ConsultMode::Research,
            CAPABLE,
        );
        assert_eq!(
            d.extra_calls(),
            1 + Depth::Standard.profile().verifier_calls
        );
        let deep = decide(
            "Verify this claim",
            ConsultMode::Research,
            ResearchRequest {
                depth: Some(Depth::Deep),
                ..Default::default()
            },
            CAPABLE,
        )
        .unwrap();
        assert_eq!(deep.extra_calls(), 1 + 2);
        let quick = decide(
            "latest news",
            ConsultMode::Research,
            ResearchRequest {
                depth: Some(Depth::Quick),
                ..Default::default()
            },
            CAPABLE,
        )
        .unwrap();
        assert_eq!(quick.extra_calls(), 1);
    }
}
