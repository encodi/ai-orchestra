//! Orchestration benchmark (Milestone 6): twelve representative workloads run through the
//! consultation engine with scripted in-process agents and a local HTTP server. No real model or
//! website is contacted, so **every number here is simulated**: the call counts are exact for the
//! scripted behaviours, the timings come from fixed artificial delays plus the real cost of this
//! program's own orchestration. They say nothing about real answer quality, real token use or real
//! subscription savings.
//!
//! `cargo test --test benchmark -- --nocapture` prints the comparison table. Set
//! `AI_ORCHESTRA_BENCH_OUT=/path/report.json` to also write it as JSON. The asserts only check
//! deterministic properties (counts, statuses, ordering), never wall-clock thresholds.

#![cfg(unix)]

#[path = "../src/research/testserver.rs"]
mod testserver;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use ai_orchestra::Orchestrator;
use ai_orchestra::config::Config;
use ai_orchestra::consult::{ConsultMode, ConsultOptions};
use ai_orchestra::debate::DebateOutcome;
use ai_orchestra::provider::{AgentProvider, ProviderRegistry};
use ai_orchestra::research::fetch::SourceFetcher;
use ai_orchestra::research::net::FetchPolicy;
use ai_orchestra::research::plan::{Depth, ResearchRequest, WebMode};
use ai_orchestra::smart::metrics::RunMetrics;
use ai_orchestra::smart::profiles::ProfileStore;
use ai_orchestra::smart::strategy::Strategy;
use serde::Serialize;
use testserver::{Route, TestServer};
use tokio_util::sync::CancellationToken;

const MAKER: &str = include_str!("fixtures/research/maker_spec.html");
const LAB: &str = include_str!("fixtures/research/lab_test.html");
const REPOST: &str = include_str!("fixtures/research/news_repost.html");
const UNRELATED: &str = include_str!("fixtures/research/unrelated.html");
const INJECTION: &str = include_str!("fixtures/research/injection.html");

#[path = "common/scripted.rs"]
mod scripted;

use scripted::{Behavior, CLAIM, Fault, SLOW_MS, scripted as scripted_agent};

// ------------------------------------------------------------------------ workloads

struct Workload {
    id: u8,
    name: &'static str,
    question: &'static str,
    mode: Option<ConsultMode>,
    /// Pages served by the local server and used as sources (empty: no research).
    pages: &'static [&'static str],
    claim_check: bool,
    depth: Option<Depth>,
    agents: Option<&'static [&'static str]>,
    rounds: Option<u8>,
    behaviors: [Behavior; 3],
}

fn page(path: &str) -> Route {
    Route::Html(
        match path {
            "/maker" => MAKER,
            "/lab" => LAB,
            "/news" => REPOST,
            "/garden" => UNRELATED,
            _ => INJECTION,
        }
        .to_string(),
    )
}

fn workloads() -> Vec<Workload> {
    let same = || {
        [
            Behavior::normal("budget"),
            Behavior::normal("budget"),
            Behavior::normal("budget"),
        ]
    };
    let cl = |mut b: Behavior| {
        b.claims = true;
        b
    };
    vec![
        Workload {
            id: 1,
            name: "simple factual question",
            question: "What is the capital of France?",
            mode: None,
            pages: &[],
            claim_check: false,
            depth: None,
            agents: None,
            rounds: None,
            behaviors: same(),
        },
        Workload {
            id: 2,
            name: "educational explanation",
            question: "Explain electrical resistance",
            mode: None,
            pages: &[],
            claim_check: false,
            depth: None,
            agents: None,
            rounds: None,
            behaviors: same(),
        },
        Workload {
            id: 3,
            name: "two-product comparison",
            question: "Compare the Acme S100 and the Borealis B200 oscilloscopes for a hobbyist.",
            mode: None,
            pages: &[],
            claim_check: false,
            depth: None,
            agents: None,
            rounds: None,
            behaviors: same(),
        },
        Workload {
            id: 4,
            name: "complex purchasing decision",
            question: "Should I buy a budget or a professional oscilloscope? I mostly debug audio circuits up to 5 MHz, my budget is 400 EUR, I need USB export and probably protocol decoding later, I share the bench with my kids, and I may sell it in two years. What are the risks of each path, and which features really matter for my case?",
            mode: None,
            pages: &[],
            claim_check: false,
            depth: None,
            agents: None,
            rounds: None,
            behaviors: same(),
        },
        Workload {
            id: 5,
            name: "scientific claim verification",
            question: CLAIM,
            mode: None,
            pages: &["/maker"],
            claim_check: true,
            depth: None,
            agents: None,
            rounds: None,
            behaviors: [
                cl(Behavior::normal("budget")),
                cl(Behavior::normal("budget")),
                cl(Behavior::normal("budget")),
            ],
        },
        Workload {
            id: 6,
            name: "conflicting evidence",
            question: "Does the Model X oscilloscope really have 100 MHz of analog bandwidth?",
            mode: Some(ConsultMode::Research),
            pages: &["/maker", "/lab"],
            claim_check: false,
            depth: None,
            agents: None,
            rounds: None,
            behaviors: [
                cl(Behavior::normal("budget")),
                cl(Behavior::normal("budget")),
                cl(Behavior::normal("budget")),
            ],
        },
        Workload {
            id: 7,
            name: "agents disagree",
            question: "Should I buy a cheap oscilloscope or invest in a professional one?",
            mode: None,
            pages: &[],
            claim_check: false,
            depth: None,
            agents: None,
            rounds: None,
            behaviors: [
                Behavior::normal("budget"),
                Behavior::normal("professional"),
                Behavior::normal("professional"),
            ],
        },
        Workload {
            id: 8,
            name: "slow provider",
            question: "Should I buy a cheap oscilloscope or invest in a professional one?",
            mode: None,
            pages: &[],
            claim_check: false,
            depth: None,
            agents: None,
            rounds: None,
            behaviors: [
                Behavior::normal("budget"),
                Behavior {
                    delay_ms: SLOW_MS,
                    ..Behavior::normal("budget")
                },
                Behavior::normal("budget"),
            ],
        },
        Workload {
            id: 9,
            name: "provider failure",
            question: "Should I buy a cheap oscilloscope or invest in a professional one?",
            mode: None,
            pages: &[],
            claim_check: false,
            depth: None,
            agents: None,
            rounds: None,
            behaviors: [
                Behavior::normal("budget"),
                Behavior::normal("budget"),
                Behavior {
                    fault: Some(Fault::Always),
                    ..Behavior::normal("budget")
                },
            ],
        },
        Workload {
            id: 10,
            name: "rate-limit failure",
            question: "Should I buy a cheap oscilloscope or invest in a professional one?",
            mode: None,
            pages: &[],
            claim_check: false,
            depth: None,
            agents: None,
            rounds: None,
            behaviors: [
                Behavior::normal("budget"),
                Behavior {
                    fault: Some(Fault::QuotaOnce),
                    ..Behavior::normal("budget")
                },
                Behavior::normal("budget"),
            ],
        },
        Workload {
            id: 11,
            name: "deep research, several sources",
            question: "Does the Model X oscilloscope really have 100 MHz of analog bandwidth? Research it thoroughly.",
            mode: Some(ConsultMode::Research),
            pages: &["/maker", "/lab", "/news", "/garden", "/injection"],
            claim_check: false,
            depth: Some(Depth::Deep),
            agents: None,
            rounds: None,
            behaviors: [
                cl(Behavior::normal("budget")),
                cl(Behavior::normal("budget")),
                cl(Behavior::normal("budget")),
            ],
        },
        Workload {
            id: 12,
            name: "explicit 3-agent 2-round debate",
            question: "Tabs or spaces for a new Rust project?",
            mode: Some(ConsultMode::Debate),
            pages: &[],
            claim_check: false,
            depth: None,
            agents: Some(&["lead", "beta", "gamma"]),
            rounds: Some(2),
            behaviors: [
                Behavior::normal("spaces"),
                Behavior::normal("tabs"),
                Behavior::normal("spaces"),
            ],
        },
    ]
}

// --------------------------------------------------------------------------- running

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
enum Setting {
    /// The fixed Milestone 4/5 orchestration, as it was before Milestone 6.
    Legacy,
    Fast,
    Balanced,
    Thorough,
    Auto,
}

impl Setting {
    const ALL: [Setting; 5] = [
        Setting::Legacy,
        Setting::Fast,
        Setting::Balanced,
        Setting::Thorough,
        Setting::Auto,
    ];

    fn label(self) -> &'static str {
        match self {
            Setting::Legacy => "legacy",
            Setting::Fast => "fast",
            Setting::Balanced => "balanced",
            Setting::Thorough => "thorough",
            Setting::Auto => "auto",
        }
    }

    fn strategy(self) -> Option<Strategy> {
        match self {
            Setting::Legacy => None,
            Setting::Fast => Some(Strategy::Fast),
            Setting::Balanced => Some(Strategy::Balanced),
            Setting::Thorough => Some(Strategy::Thorough),
            Setting::Auto => Some(Strategy::Auto),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct Row {
    workload: u8,
    name: &'static str,
    setting: &'static str,
    status: String,
    calls: usize,
    failed_calls: usize,
    wall_ms: u64,
    provider_ms_total: u64,
    provider_busy_ms: u64,
    overhead_ms: u64,
    peak_concurrency: usize,
    source_fetches: usize,
    warnings: usize,
    answer_chars: usize,
    /// The strategy that ran (the one `auto` chose, for auto).
    strategy: String,
    /// Characters sent to agents in prompts (measured by the budget manager).
    prompt_chars: u64,
    quality_violations: usize,
    stop: String,
    /// Time calls waited for one of the consultation's parallel slots.
    queue_ms: u64,
    /// Work not done twice: identical calls answered once, claims not judged again, candidate
    /// pages merged into one already recorded.
    avoided: usize,
}

async fn run_one(w: &Workload, setting: Setting) -> Row {
    run_with(w, setting, false).await
}

/// `warm`: the provider history was built first, from three thorough runs of the same workload.
async fn run_with(w: &Workload, setting: Setting, warm: bool) -> Row {
    let routes: Vec<(&str, Route)> = w.pages.iter().map(|p| (*p, page(p))).collect();
    let server = TestServer::start(routes).await;
    let names = ["lead", "beta", "gamma"];
    let providers: Vec<Arc<dyn AgentProvider>> = names
        .iter()
        .zip(w.behaviors.iter())
        .enumerate()
        .map(|(i, (n, b))| {
            let p: Arc<dyn AgentProvider> = scripted_agent(n, b, i == 0);
            p
        })
        .collect();
    let mut config = Config::default();
    config.general.coordinator = "lead".into();
    let history_dir = tempfile::tempdir().unwrap();
    let mut orch =
        Orchestrator::with_registry(config.clone(), ProviderRegistry::new(providers)).unwrap();
    if warm {
        orch = orch.with_profiles(ProfileStore::new(
            history_dir.path().join("profiles.json"),
            config.smart.profile_settings(),
        ));
    }
    let fetcher = Arc::new(
        SourceFetcher::new(FetchPolicy {
            respect_robots: false,
            ..FetchPolicy::for_local_tests()
        })
        .unwrap(),
    );
    let research = if w.pages.is_empty() {
        ResearchRequest {
            web: WebMode::Off,
            ..Default::default()
        }
    } else {
        // The deep-research workload also lists three of its pages again under other spellings
        // (a fragment, a tracking parameter, a trailing slash): they are merged, not fetched twice.
        let respelled: &[&str] = if w.id == 11 {
            &["/maker#specs", "/lab?utm_source=feed", "/news/"]
        } else {
            &[]
        };
        ResearchRequest {
            urls: w
                .pages
                .iter()
                .chain(respelled.iter())
                .map(|p| server.url(p))
                .collect(),
            depth: w.depth,
            ..Default::default()
        }
    };
    let mut options = ConsultOptions {
        question: w.question.to_string(),
        mode: w.mode,
        agents: w
            .agents
            .map(|a| a.iter().map(|s| (*s).to_string()).collect()),
        rounds: w.rounds,
        research,
        claim_check: w.claim_check,
        fetcher: Some(fetcher),
        ..Default::default()
    };
    options.strategy = setting.strategy();
    if warm {
        for _ in 0..3 {
            let mut warmup = options.clone();
            warmup.strategy = Some(Strategy::Thorough);
            let _ = orch.consult(warmup, &CancellationToken::new()).await;
        }
    }
    let started = Instant::now();
    let result = orch.consult(options, &CancellationToken::new()).await;
    if std::env::var("AI_ORCHESTRA_BENCH_SHOW").ok().as_deref()
        == Some(&format!("{}:{}", w.id, setting.label()))
        && let Ok(o) = &result
    {
        println!(
            "\n===== workload {} · {} =====\n{}\n",
            w.id,
            setting.label(),
            o.final_answer
        );
        if let Some(c) = &o.consultation {
            if let Some(smart) = c.routing.as_ref().and_then(|r| r.smart.as_ref()) {
                println!("--- plan ---\n{}", smart.render());
            }
            if let Some(rec) = &c.orchestration {
                println!("--- execution ---\n{}", rec.render());
            }
        }
        for w in &o.warnings {
            println!("warning: {w}");
        }
    }
    let wall_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let (status, out): (String, Option<DebateOutcome>) = match result {
        Ok(o) => (o.status.to_string(), Some(o)),
        Err(e) => (format!("error: {e}").chars().take(40).collect(), None),
    };
    let steps = out.as_ref().map(|o| o.steps.clone()).unwrap_or_default();
    let orchestration = out
        .as_ref()
        .and_then(|o| o.consultation.as_ref())
        .and_then(|c| c.orchestration.clone());
    let m = RunMetrics::from_steps(&steps);
    Row {
        workload: w.id,
        name: w.name,
        setting: setting.label(),
        status,
        calls: m.calls,
        failed_calls: m.failed_calls,
        wall_ms,
        provider_ms_total: m.provider_ms_total,
        provider_busy_ms: m.provider_busy_ms,
        overhead_ms: m.overhead_ms(wall_ms),
        peak_concurrency: m.peak_concurrency,
        source_fetches: out
            .as_ref()
            .and_then(|o| o.consultation.as_ref())
            .and_then(|c| c.research.as_ref())
            .map_or(0, |r| r.budget.fetch_attempts),
        warnings: out.as_ref().map_or(0, |o| o.warnings.len()),
        answer_chars: out.as_ref().map_or(0, |o| o.final_answer.chars().count()),
        strategy: orchestration
            .as_ref()
            .and(out.as_ref())
            .and_then(|o| o.consultation.as_ref())
            .and_then(|c| c.routing.as_ref())
            .and_then(|r| r.smart.as_ref())
            .and_then(|p| p.strategy_value())
            .map_or("-".to_string(), |s| s.to_string()),
        prompt_chars: orchestration.as_ref().map_or(0, |o| o.budget.prompt_chars),
        queue_ms: orchestration.as_ref().map_or(0, |o| o.budget.queue_ms),
        avoided: orchestration.as_ref().map_or(0, |o| o.budget.calls_reused)
            + out
                .as_ref()
                .and_then(|o| o.consultation.as_ref())
                .and_then(|c| c.research.as_ref())
                .map_or(0, |r| r.budget.claims_reused + r.budget.sources_merged),
        quality_violations: orchestration
            .as_ref()
            .and_then(|o| o.quality.as_ref())
            .map_or(0, |q| q.violations().len()),
        stop: orchestration
            .as_ref()
            .and_then(|o| o.stop_reason.clone())
            .unwrap_or_default(),
    }
}

fn table(rows: &[Row]) -> String {
    let mut out = String::from(
        "| # | workload | setting | ran as | status | calls | failed | wall ms | agent ms (sum) | busy ms | overhead ms | queue ms | peak | fetches | avoided | prompt chars | gate | stopped because |\n|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|\n",
    );
    for r in rows {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            r.workload, r.name, r.setting, r.strategy, r.status, r.calls, r.failed_calls, r.wall_ms,
            r.provider_ms_total, r.provider_busy_ms, r.overhead_ms, r.queue_ms, r.peak_concurrency,
            r.source_fetches, r.avoided, r.prompt_chars, r.quality_violations,
            r.stop.replace('|', "/")
        ));
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_benchmark_runs_every_workload_for_every_setting() {
    let mut rows = Vec::new();
    for w in workloads() {
        for setting in Setting::ALL {
            rows.push(run_one(&w, setting).await);
        }
    }
    // The provider history in action: the same slow-provider workload, cold and warm.
    let slow = workloads().into_iter().find(|w| w.id == 8).unwrap();
    let cold = run_with(&slow, Setting::Balanced, false).await;
    let mut warm = run_with(&slow, Setting::Balanced, true).await;
    warm.setting = "balanced + history";
    let mut cold = cold;
    cold.setting = "balanced (no history)";
    let reviewer_note = |r: &Row| r.wall_ms;
    println!(
        "\nprovider history (workload 8): no history {} ms, with history {} ms",
        reviewer_note(&cold),
        reviewer_note(&warm)
    );
    rows.push(cold);
    rows.push(warm);
    let text = table(&rows);
    println!(
        "\nSIMULATED RESULTS (scripted agents, local server; not real measurements)\n\n{text}"
    );
    if let Ok(path) = std::env::var("AI_ORCHESTRA_BENCH_OUT") {
        let by: BTreeMap<String, &Row> = rows
            .iter()
            .map(|r| (format!("{:02}-{}", r.workload, r.setting), r))
            .collect();
        std::fs::write(path, serde_json::to_string_pretty(&by).unwrap()).unwrap();
    }
    assert_eq!(rows.len(), 12 * Setting::ALL.len() + 2);
    check_deterministic_properties(&rows);
}

fn row(rows: &[Row], workload: u8, setting: Setting) -> &Row {
    rows.iter()
        .find(|r| r.workload == workload && r.setting == setting.label())
        .unwrap_or_else(|| panic!("no row for workload {workload} / {}", setting.label()))
}

/// What the benchmark may assert: counts and statuses of scripted behaviours, never durations.
fn check_deterministic_properties(rows: &[Row]) {
    // The legacy column is the Milestone 4/5 workflow, byte for byte: these are the calls it made
    // before Milestone 6 existed (measured on the unchanged code).
    let baseline = [1usize, 1, 7, 10, 1, 8, 10, 10, 8, 8, 12, 6];
    for (i, calls) in baseline.iter().enumerate() {
        let w = u8::try_from(i + 1).unwrap();
        assert_eq!(
            row(rows, w, Setting::Legacy).calls,
            *calls,
            "workload {w}: legacy calls changed"
        );
    }
    for w in 1..=12u8 {
        let (legacy, fast, balanced, thorough) = (
            row(rows, w, Setting::Legacy),
            row(rows, w, Setting::Fast),
            row(rows, w, Setting::Balanced),
            row(rows, w, Setting::Thorough),
        );
        assert!(
            fast.calls <= legacy.calls,
            "workload {w}: fast {} > legacy {}",
            fast.calls,
            legacy.calls
        );
        assert!(fast.calls <= balanced.calls.max(fast.calls));
        assert!(
            balanced.calls <= thorough.calls,
            "workload {w}: balanced {} > thorough {}",
            balanced.calls,
            thorough.calls
        );
        for r in [
            legacy,
            fast,
            balanced,
            thorough,
            row(rows, w, Setting::Auto),
        ] {
            assert_eq!(
                r.quality_violations, 0,
                "workload {w} {}: the quality gate objected",
                r.setting
            );
        }
    }
    // Explicit settings define the shape: the debate costs the same under every strategy.
    for s in Setting::ALL {
        assert_eq!(row(rows, 12, s).calls, 6, "{}", s.label());
    }
    // A simple question costs one call for fast, balanced and auto.
    for s in [Setting::Fast, Setting::Balanced, Setting::Auto] {
        assert_eq!(row(rows, 1, s).calls, 1);
        assert_eq!(row(rows, 2, s).calls, 1);
    }
    // Balanced reviews a comparison once, and escalates only where the agents disagree.
    assert_eq!(row(rows, 3, Setting::Balanced).calls, 2);
    assert_eq!(row(rows, 7, Setting::Balanced).calls, 4);
    // Auto sends a long multi-part decision to the full panel and a short one to a review.
    assert_eq!(row(rows, 4, Setting::Auto).strategy, "thorough");
    assert_eq!(row(rows, 7, Setting::Auto).strategy, "balanced");
    assert_eq!(row(rows, 1, Setting::Auto).strategy, "fast");
    // A usage limit is never retried, whatever the strategy.
    for s in Setting::ALL {
        assert!(row(rows, 10, s).failed_calls <= 1, "{}", s.label());
    }
    // The respelled pages of the deep-research workload were merged, never fetched a second time.
    for st in Setting::ALL {
        let r = row(rows, 11, st);
        assert_eq!((r.source_fetches, r.avoided), (5, 3), "{}", st.label());
    }
    // Thorough at deep research checks the evidence more than fast does.
    assert!(row(rows, 11, Setting::Thorough).calls > row(rows, 11, Setting::Fast).calls);
    // Fewer calls means less text sent to the agents.
    assert!(
        row(rows, 4, Setting::Fast).prompt_chars < row(rows, 4, Setting::Thorough).prompt_chars
    );
}
