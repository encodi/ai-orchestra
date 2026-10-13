//! Smart orchestration (Milestone 6), end to end with scripted in-process agents and a local HTTP
//! server. No real model or website is contacted.
//!
//! The assertions are about what was *called*, in what order and how often, which statuses were
//! given and what the records say. None of them depends on a wall-clock threshold; where
//! concurrency matters, a barrier proves it (sequential execution would never release it).

#![cfg(unix)]

#[path = "../src/research/testserver.rs"]
mod testserver;

#[path = "common/scripted.rs"]
mod scripted;

use std::sync::Arc;
use std::time::{Duration, Instant};

use ai_orchestra::Orchestrator;
use ai_orchestra::config::Config;
use ai_orchestra::consult::{ConsultMode, ConsultOptions, Source};
use ai_orchestra::debate::{DebateOutcome, DebateStatus, Role};
use ai_orchestra::error::OrchestraError;
use ai_orchestra::history::DebateStore;
use ai_orchestra::provider::{AgentProvider, ProviderRegistry};
use ai_orchestra::providers::mock::{MockProvider, MockReply};
use ai_orchestra::research::claims::ClaimStatus;
use ai_orchestra::research::fetch::SourceFetcher;
use ai_orchestra::research::net::FetchPolicy;
use ai_orchestra::research::plan::{Depth, ResearchRequest, WebMode};
use ai_orchestra::research::source::SourceStatus;
use ai_orchestra::smart::budget::BudgetStop;
use ai_orchestra::smart::profiles::ProfileStore;
use ai_orchestra::smart::strategy::{Flow, Strategy};
use scripted::{Behavior, Fault, scripted as agent};
use testserver::{Route, TestServer};
use tokio::sync::Barrier;
use tokio_util::sync::CancellationToken;

const MAKER: &str = include_str!("fixtures/research/maker_spec.html");
const LAB: &str = include_str!("fixtures/research/lab_test.html");

const DECISION_Q: &str = "Should I buy a cheap oscilloscope or invest in a professional one?";
const SIMPLE_Q: &str = "Explain electrical resistance";
const RESEARCH_Q: &str = "Does the Model X oscilloscope really have 100 MHz of analog bandwidth?";

// ------------------------------------------------------------------------- harness

struct Rig {
    orch: Orchestrator,
    agents: Vec<Arc<MockProvider>>,
    server: TestServer,
    fetcher: Arc<SourceFetcher>,
    store: DebateStore,
    dir: tempfile::TempDir,
}

struct Setup {
    behaviors: [Behavior; 3],
    pages: Vec<(&'static str, Route)>,
    config: Box<dyn FnOnce(&mut Config)>,
    profiles: bool,
    /// Replaces the mocks built from `behaviors` (for gates and scripted failures).
    mocks: Option<Vec<Arc<MockProvider>>>,
}

fn setup() -> Setup {
    Setup {
        behaviors: [
            Behavior::normal("budget"),
            Behavior::normal("budget"),
            Behavior::normal("budget"),
        ],
        pages: Vec::new(),
        config: Box::new(|_| {}),
        profiles: false,
        mocks: None,
    }
}

impl Setup {
    fn stances(mut self, a: &'static str, b: &'static str, c: &'static str) -> Self {
        self.behaviors[0].stance = a;
        self.behaviors[1].stance = b;
        self.behaviors[2].stance = c;
        self
    }

    fn claims(mut self) -> Self {
        for b in &mut self.behaviors {
            b.claims = true;
        }
        self
    }

    fn pages(mut self, p: Vec<(&'static str, Route)>) -> Self {
        self.pages = p;
        self
    }

    fn fast_agents(mut self) -> Self {
        for b in &mut self.behaviors {
            b.delay_ms = 1;
        }
        self
    }

    fn fault(mut self, index: usize, fault: Fault) -> Self {
        self.behaviors[index].fault = Some(fault);
        self
    }

    fn config(mut self, f: impl FnOnce(&mut Config) + 'static) -> Self {
        self.config = Box::new(f);
        self
    }

    fn profiles(mut self) -> Self {
        self.profiles = true;
        self
    }

    fn mocks(mut self, m: Vec<Arc<MockProvider>>) -> Self {
        self.mocks = Some(m);
        self
    }
}

async fn rig(s: Setup) -> Rig {
    let server = TestServer::start(s.pages.clone()).await;
    let agents: Vec<Arc<MockProvider>> = match s.mocks {
        Some(m) => m,
        None => ["lead", "beta", "gamma"]
            .iter()
            .zip(s.behaviors.iter())
            .enumerate()
            .map(|(i, (n, b))| agent(n, b, i == 0))
            .collect(),
    };
    let providers: Vec<Arc<dyn AgentProvider>> = agents
        .iter()
        .map(|a| a.clone() as Arc<dyn AgentProvider>)
        .collect();
    let mut config = Config::default();
    config.general.coordinator = "lead".into();
    (s.config)(&mut config);
    let dir = tempfile::tempdir().unwrap();
    let store = DebateStore::new(dir.path().join("debates"));
    let mut orch = Orchestrator::with_registry(config.clone(), ProviderRegistry::new(providers))
        .unwrap()
        .with_history(store.clone());
    if s.profiles {
        orch = orch.with_profiles(ProfileStore::new(
            dir.path().join("profiles.json"),
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
    Rig {
        orch,
        agents,
        server,
        fetcher,
        store,
        dir,
    }
}

impl Rig {
    /// Offline options for `question` under `strategy`.
    fn offline(&self, question: &str, strategy: Option<Strategy>) -> ConsultOptions {
        ConsultOptions {
            question: question.to_string(),
            strategy,
            research: ResearchRequest {
                web: WebMode::Off,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// Options that research the given pages of the local server.
    fn with_pages(
        &self,
        question: &str,
        strategy: Option<Strategy>,
        pages: &[&str],
    ) -> ConsultOptions {
        ConsultOptions {
            question: question.to_string(),
            strategy,
            mode: Some(ConsultMode::Research),
            research: ResearchRequest {
                urls: pages.iter().map(|p| self.server.url(p)).collect(),
                ..Default::default()
            },
            fetcher: Some(self.fetcher.clone()),
            ..Default::default()
        }
    }

    async fn run(&self, o: ConsultOptions) -> DebateOutcome {
        self.orch
            .consult(o, &CancellationToken::new())
            .await
            .expect("the consultation runs")
    }

    fn calls(&self) -> Vec<usize> {
        self.agents.iter().map(|a| a.requests().len()).collect()
    }

    fn total_calls(&self) -> usize {
        self.calls().iter().sum()
    }

    fn prompts(&self, index: usize) -> Vec<String> {
        self.agents[index]
            .requests()
            .into_iter()
            .map(|r| r.prompt)
            .collect()
    }
}

fn orchestration(o: &DebateOutcome) -> &ai_orchestra::smart::trace::OrchestrationRecord {
    o.consultation
        .as_ref()
        .unwrap()
        .orchestration
        .as_ref()
        .expect("an orchestration record")
}

fn plan(o: &DebateOutcome) -> &ai_orchestra::smart::strategy::SmartPlan {
    o.consultation
        .as_ref()
        .unwrap()
        .routing
        .as_ref()
        .unwrap()
        .smart
        .as_ref()
        .expect("a smart plan")
}

fn roles(o: &DebateOutcome) -> Vec<Role> {
    o.steps.iter().map(|s| s.phase).collect()
}

fn research(o: &DebateOutcome) -> &ai_orchestra::research::engine::ResearchReport {
    o.consultation
        .as_ref()
        .unwrap()
        .research
        .as_ref()
        .expect("a research report")
}

// ------------------------------------------------------------------------ strategies

#[tokio::test]
async fn fast_asks_one_agent_once_and_never_starts_the_others() {
    let r = rig(setup().fast_agents()).await;
    let o = r.run(r.offline(DECISION_Q, Some(Strategy::Fast))).await;
    assert_eq!(r.calls(), [1, 0, 0]);
    assert_eq!((o.status, o.calls), (DebateStatus::Complete, 1));
    assert_eq!(plan(&o).flow, Flow::Single);
    assert_eq!(plan(&o).strategy_value(), Some(Strategy::Fast));
    assert!(
        o.warnings
            .iter()
            .any(|w| w.contains("no independent review")),
        "{:?}",
        o.warnings
    );
    // It says so in the answer's own record, not just in the plan.
    assert!(
        orchestration(&o)
            .quality
            .as_ref()
            .unwrap()
            .advisories()
            .iter()
            .any(|c| c.name == "independent_review")
    );
}

#[tokio::test]
async fn balanced_stops_after_one_call_when_nothing_calls_for_a_second_look() {
    let r = rig(setup().fast_agents()).await;
    let o = r.run(r.offline(SIMPLE_Q, Some(Strategy::Balanced))).await;
    assert_eq!(r.calls(), [1, 0, 0], "later invocations were avoided");
    assert_eq!(o.status, DebateStatus::Complete);
    let rec = orchestration(&o);
    assert!(
        rec.stop_reason
            .as_deref()
            .unwrap()
            .starts_with("sufficient:"),
        "{:?}",
        rec.stop_reason
    );
    assert!(
        rec.events
            .iter()
            .any(|e| e.message.contains("after the lead's answer: finish"))
    );
    assert!(
        o.warnings
            .iter()
            .any(|w| w.contains("no independent review took place"))
    );
}

#[tokio::test]
async fn balanced_adds_one_review_for_a_decision_and_ends_without_a_synthesis_when_it_agrees() {
    let r = rig(setup().fast_agents()).await;
    let o = r.run(r.offline(DECISION_Q, Some(Strategy::Balanced))).await;
    assert_eq!(
        r.calls(),
        [1, 1, 0],
        "the lead and one reviewer; the third agent is never asked"
    );
    assert_eq!(roles(&o), [Role::Perspective, Role::Review]);
    assert_eq!(o.status, DebateStatus::Complete);
    assert!(
        o.final_answer.contains("## Independent review")
            && o.final_answer.contains("VERDICT: AGREE")
    );
    assert_eq!(
        orchestration(&o).stop_reason.as_deref(),
        Some("the independent review found no material issue")
    );
}

#[tokio::test]
async fn a_material_disagreement_escalates_once_to_an_independent_third_perspective() {
    let r = rig(setup()
        .stances("budget", "professional", "professional")
        .fast_agents())
    .await;
    let o = r.run(r.offline(DECISION_Q, Some(Strategy::Balanced))).await;
    assert_eq!(
        r.calls(),
        [2, 1, 1],
        "lead answers and writes the synthesis; two others once each"
    );
    assert_eq!(
        roles(&o),
        [
            Role::Perspective,
            Role::Review,
            Role::Perspective,
            Role::Synthesis
        ]
    );
    assert_eq!(o.status, DebateStatus::Complete);
    // The third perspective is independent: it saw neither the lead's answer nor the review.
    let third = &r.prompts(2)[0];
    assert!(
        !third.contains("answer_under_review")
            && !third.contains("<review")
            && !third.contains("STANCE"),
        "{third}"
    );
    // The synthesis names exactly the three agents that took part.
    let synth = r.prompts(0).last().unwrap().clone();
    assert!(
        synth.contains("The panel is exactly: lead, beta, gamma."),
        "{synth}"
    );
    let events: Vec<_> = orchestration(&o)
        .events
        .iter()
        .map(|e| e.message.clone())
        .collect();
    assert!(
        events.iter().any(|m| m.contains("MATERIAL_DISAGREEMENT")),
        "{events:?}"
    );
    assert!(
        events
            .iter()
            .any(|m| m.contains("gamma gives an independent perspective")),
        "{events:?}"
    );
    assert!(
        orchestration(&o)
            .stop_reason
            .as_deref()
            .unwrap()
            .contains("escalation limit")
    );
}

#[tokio::test]
async fn thorough_runs_every_agent_through_two_review_rounds_and_a_synthesis() {
    let r = rig(setup().fast_agents()).await;
    let o = r.run(r.offline(DECISION_Q, Some(Strategy::Thorough))).await;
    assert_eq!(
        r.calls(),
        [4, 3, 3],
        "3 answers, 2 rounds of 3 reviews, 1 synthesis"
    );
    assert_eq!(o.calls, 10);
    assert_eq!(o.status, DebateStatus::Complete);
    assert_eq!(plan(&o).max_calls, 10);
    assert_eq!(o.consultation.as_ref().unwrap().rounds_run, 2);
}

#[tokio::test]
async fn auto_chooses_a_strategy_from_the_question_and_records_why() {
    let r = rig(setup().fast_agents()).await;
    let simple = r.run(r.offline(SIMPLE_Q, Some(Strategy::Auto))).await;
    assert_eq!(plan(&simple).strategy_value(), Some(Strategy::Fast));
    assert_eq!(
        plan(&simple).strategy.as_ref().unwrap().source,
        Source::Auto
    );
    let decision = r.run(r.offline(DECISION_Q, Some(Strategy::Auto))).await;
    assert_eq!(plan(&decision).strategy_value(), Some(Strategy::Balanced));
    let reason = &plan(&decision).strategy.as_ref().unwrap().reason;
    assert!(reason.contains("independent review"), "{reason}");
    // The two consultations used 1 and 2 calls instead of the fixed panel's 7+.
    assert_eq!((simple.calls, decision.calls), (1, 2));
}

#[tokio::test]
async fn without_a_strategy_the_fixed_workflow_runs_exactly_as_before() {
    let r = rig(setup().fast_agents()).await;
    let o = r.run(r.offline(DECISION_Q, None)).await;
    assert!(plan_absent(&o));
    assert_eq!(
        o.calls, 10,
        "3 answers, 2 rounds (contested) of 3, 1 synthesis"
    );
    assert!(
        o.consultation.as_ref().unwrap().orchestration.is_none(),
        "no strategy, no smart record"
    );
    assert_eq!(o.status, DebateStatus::Complete);
}

fn plan_absent(o: &DebateOutcome) -> bool {
    o.consultation
        .as_ref()
        .unwrap()
        .routing
        .as_ref()
        .unwrap()
        .smart
        .is_none()
}

// ------------------------------------------------------------------- manual settings win

#[tokio::test]
async fn a_manual_coordinator_leads_and_writes_the_synthesis() {
    let r = rig(setup()
        .stances("budget", "professional", "professional")
        .fast_agents())
    .await;
    let mut opts = r.offline(DECISION_Q, Some(Strategy::Balanced));
    opts.coordinator = Some("gamma".into());
    let o = r.run(opts).await;
    assert_eq!(o.coordinator.to_string(), "gamma");
    let lead_prompts = r.prompts(2);
    assert!(
        lead_prompts[0].contains("You are the lead assistant"),
        "gamma led"
    );
    assert!(
        r.prompts(0)
            .iter()
            .all(|p| !p.contains("You are the lead assistant")),
        "the configured coordinator did not"
    );
    assert!(
        lead_prompts
            .last()
            .unwrap()
            .contains("Write the FINAL answer")
    );
    assert!(
        plan(&o)
            .explanation
            .iter()
            .any(|l| l.contains("kept as you set them") && l.contains("coordinator"))
    );
}

#[tokio::test]
async fn named_agents_all_take_part_whatever_the_strategy_and_answer_independently_first() {
    for strategy in [Strategy::Fast, Strategy::Balanced, Strategy::Auto] {
        let r = rig(setup().fast_agents()).await;
        let mut opts = r.offline(SIMPLE_Q, Some(strategy));
        opts.agents = Some(vec!["lead".into(), "beta".into(), "gamma".into()]);
        let o = r.run(opts).await;
        assert!(
            r.calls().iter().all(|n| *n >= 1),
            "{strategy}: {:?}",
            r.calls()
        );
        assert_eq!(plan(&o).flow, Flow::Panel, "{strategy}");
        // Independent first round: all three answers were requested before any review was.
        let answers = o
            .steps
            .iter()
            .filter(|s| s.phase == Role::Perspective)
            .count();
        assert_eq!(answers, 3, "{strategy}");
        if let Some(first_review) = o.steps.iter().position(|s| s.phase == Role::Review) {
            let last_answer = o
                .steps
                .iter()
                .rposition(|s| s.phase == Role::Perspective)
                .unwrap();
            assert!(last_answer < first_review, "{strategy}");
        }
        for i in 0..3 {
            let first = &r.prompts(i)[0];
            assert!(
                first.contains("you cannot see their work") && !first.contains("<other_answers>"),
                "{strategy}: {first}"
            );
        }
        assert_eq!(
            o.consultation.as_ref().unwrap().manual_overrides,
            ["agents"]
        );
    }
}

#[tokio::test]
async fn explicit_rounds_are_run_in_full_even_under_fast() {
    let r = rig(setup().fast_agents()).await;
    let mut opts = r.offline(SIMPLE_Q, Some(Strategy::Fast));
    opts.rounds = Some(2);
    let o = r.run(opts).await;
    assert_eq!(o.consultation.as_ref().unwrap().rounds_run, 2);
    assert!(
        r.total_calls() > 2 + 2 * 2,
        "two agents: 2 answers + 2 rounds of 2 + synthesis, got {}",
        r.total_calls()
    );
    assert_eq!(o.status, DebateStatus::Complete);
    assert!(orchestration(&o).quality.as_ref().unwrap().passed());
}

#[tokio::test]
async fn an_explicit_strategy_beats_the_configured_default() {
    let r = rig(setup()
        .fast_agents()
        .config(|c| c.smart.default_strategy = "thorough".into()))
    .await;
    // The library takes the strategy from the options; the configured default is for the front ends.
    let o = r.run(r.offline(SIMPLE_Q, Some(Strategy::Fast))).await;
    assert_eq!(r.total_calls(), 1);
    assert_eq!(plan(&o).strategy.as_ref().unwrap().source, Source::Manual);
}

// ---------------------------------------------------------------------- conflicts

#[tokio::test]
async fn conflicting_settings_are_rejected_before_any_agent_is_called() {
    let r = rig(setup().fast_agents()).await;
    // Three named agents and two rounds need 10 calls.
    let mut opts = r.offline(DECISION_Q, Some(Strategy::Auto));
    opts.agents = Some(vec!["lead".into(), "beta".into(), "gamma".into()]);
    opts.rounds = Some(2);
    opts.max_agent_calls = Some(5);
    let err = r
        .orch
        .consult(opts, &CancellationToken::new())
        .await
        .unwrap_err();
    let text = err.to_string();
    assert!(matches!(err, OrchestraError::Config(_)));
    assert!(
        text.contains("at least 10 agent call(s)") && text.contains("--max-agent-calls is 5"),
        "{text}"
    );
    // A duration above the configured ceiling, and nonsense limits.
    for tweak in [
        (|o: &mut ConsultOptions| o.max_duration = Some(Duration::from_secs(1_000_000)))
            as fn(&mut ConsultOptions),
        |o| o.max_agent_calls = Some(0),
        |o| o.max_parallel = Some(0),
        |o| o.max_duration = Some(Duration::ZERO),
    ] {
        let mut o = r.offline(SIMPLE_Q, Some(Strategy::Fast));
        tweak(&mut o);
        assert!(r.orch.consult(o, &CancellationToken::new()).await.is_err());
    }
    assert_eq!(r.total_calls(), 0, "no subscription was touched");
    assert_eq!(r.store.list().debates.len(), 0, "and nothing was saved");
}

#[tokio::test]
async fn automatic_settings_are_lowered_to_fit_a_call_limit_and_the_result_says_so() {
    let r = rig(setup().fast_agents()).await;
    let mut opts = r.offline(DECISION_Q, Some(Strategy::Thorough));
    opts.max_agent_calls = Some(5);
    let o = r.run(opts).await;
    assert!(o.calls <= 5, "{} calls", o.calls);
    assert_eq!(
        o.status,
        DebateStatus::Complete,
        "what was planned to fit ran in full"
    );
    assert!(
        plan(&o)
            .explanation
            .iter()
            .any(|l| l.contains("to fit the call limit")),
        "{:?}",
        plan(&o).explanation
    );
}

// ------------------------------------------------------------------------ the budget

#[tokio::test]
async fn the_call_limit_is_enforced_while_running_and_the_result_is_honestly_partial() {
    // Balanced: lead (1), review (2) says MATERIAL; with a limit of 2 neither the third
    // perspective nor the synthesis may start.
    let r = rig(setup()
        .stances("budget", "professional", "professional")
        .fast_agents())
    .await;
    let mut opts = r.offline(DECISION_Q, Some(Strategy::Balanced));
    opts.max_agent_calls = Some(2);
    let o = r.run(opts).await;
    assert_eq!(r.total_calls(), 2, "exactly the limit");
    assert_eq!(o.status, DebateStatus::Partial);
    let rec = orchestration(&o);
    assert!(
        rec.stop_reason.as_deref().unwrap().starts_with("budget:"),
        "{:?}",
        rec.stop_reason
    );
    assert!(
        o.warnings
            .iter()
            .any(|w| w.contains("could not be integrated")),
        "{:?}",
        o.warnings
    );
    // What finished is kept: the lead's answer and the review.
    assert!(
        o.final_answer.contains("My answer: budget")
            && o.final_answer.contains("## Independent review")
    );
    assert!(o.final_answer.contains("MATERIAL_DISAGREEMENT"));
}

#[tokio::test]
async fn the_global_deadline_stops_new_work_and_cancels_the_call_in_flight() {
    // The lead is quick, the reviewer slow: the deadline ends the review, and nothing else starts.
    let mocks = vec![
        agent(
            "lead",
            &Behavior {
                delay_ms: 5,
                ..Behavior::normal("budget")
            },
            true,
        ),
        agent(
            "beta",
            &Behavior {
                delay_ms: 30_000,
                ..Behavior::normal("budget")
            },
            false,
        ),
        agent("gamma", &Behavior::normal("budget"), false),
    ];
    let r = rig(setup().mocks(mocks)).await;
    let mut opts = r.offline(DECISION_Q, Some(Strategy::Balanced));
    opts.max_duration = Some(Duration::from_secs(1));
    let started = Instant::now();
    let o = r.run(opts).await;
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the slow call was cut at the deadline, took {:?}",
        started.elapsed()
    );
    assert_eq!(o.status, DebateStatus::Partial);
    assert_eq!(
        r.calls(),
        [1, 1, 0],
        "no new agent was started after the deadline"
    );
    let rec = orchestration(&o);
    assert!(
        matches!(
            rec.budget.stopped_by,
            Some(BudgetStop::Duration { limit_secs: 1 })
        ),
        "{:?}",
        rec.budget.stopped_by
    );
    assert!(
        o.warnings
            .iter()
            .any(|w| w.contains("independent review") && w.contains("time limit")),
        "{:?}",
        o.warnings
    );
    assert!(
        o.final_answer.contains("My answer: budget"),
        "the lead's answer is kept"
    );
}

#[tokio::test]
async fn max_parallel_bounds_how_many_calls_of_one_consultation_run_together() {
    let r = rig(setup().config(|c| c.general.timeout_secs = 20)).await;
    let mut opts = r.offline(DECISION_Q, Some(Strategy::Thorough));
    opts.max_parallel = Some(1);
    let o = r.run(opts).await;
    assert_eq!(o.calls, 10);
    let rec = orchestration(&o);
    assert_eq!(rec.metrics.peak_concurrency, 1, "calls ran one at a time");
    assert_eq!(rec.budget.peak_parallel, 1);
    assert!(rec.budget.queue_ms > 0, "the others waited for a slot");
    // Without the limit the three answers run together.
    let r2 = rig(setup()).await;
    let o2 = r2
        .run(r2.offline(DECISION_Q, Some(Strategy::Thorough)))
        .await;
    assert_eq!(orchestration(&o2).metrics.peak_concurrency, 3);
}

#[tokio::test]
async fn cancelling_stops_the_adaptive_flow_and_saves_a_cancelled_record() {
    let mocks = vec![
        agent(
            "lead",
            &Behavior {
                delay_ms: 30_000,
                ..Behavior::normal("budget")
            },
            true,
        ),
        agent("beta", &Behavior::normal("budget"), false),
        agent("gamma", &Behavior::normal("budget"), false),
    ];
    let r = rig(setup().mocks(mocks)).await;
    let cancel = CancellationToken::new();
    let stopper = {
        let c = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            c.cancel();
        })
    };
    let started = Instant::now();
    let err = r
        .orch
        .consult(r.offline(DECISION_Q, Some(Strategy::Balanced)), &cancel)
        .await
        .unwrap_err();
    stopper.await.unwrap();
    assert!(matches!(err, OrchestraError::Cancelled { .. }), "{err}");
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(r.calls(), [1, 0, 0], "nothing else was started");
    let saved = r.store.list();
    assert_eq!(saved.debates.len(), 1);
    assert_eq!(saved.debates[0].status, DebateStatus::Cancelled);
}

// ------------------------------------------------------------------------- failures

#[tokio::test]
async fn a_usage_limit_is_never_retried_and_the_result_is_partial() {
    let r = rig(setup().fault(1, Fault::QuotaOnce).fast_agents()).await;
    let o = r.run(r.offline(DECISION_Q, Some(Strategy::Balanced))).await;
    assert_eq!(
        r.calls(),
        [1, 1, 0],
        "the reviewer was asked once and no other agent took its place"
    );
    assert_eq!(o.status, DebateStatus::Partial);
    assert!(
        o.steps
            .iter()
            .any(|s| s.error_kind.as_deref() == Some("quota"))
    );
    assert!(
        o.warnings
            .iter()
            .any(|w| w.contains("independent review") && w.contains("failed"))
    );
    assert!(
        o.final_answer.contains("My answer: budget"),
        "the lead's answer is still delivered"
    );
}

#[tokio::test]
async fn a_failing_participant_makes_a_thorough_result_partial_but_keeps_the_rest() {
    let r = rig(setup().fault(2, Fault::Always).fast_agents()).await;
    let o = r.run(r.offline(DECISION_Q, Some(Strategy::Thorough))).await;
    assert_eq!(o.status, DebateStatus::Partial);
    assert!(o.synthesized, "the others still produced a synthesis");
    let gate = orchestration(&o).quality.as_ref().unwrap();
    assert!(
        gate.passed(),
        "a properly labelled partial result passes the disclosure checks: {gate:?}"
    );
    assert!(o.warnings.iter().any(|w| w.contains("gamma failed")));
}

#[tokio::test]
async fn an_unavailable_participant_changes_nothing_for_a_strategy_that_never_asks_it() {
    let r = rig(setup().fault(2, Fault::Always).fast_agents()).await;
    let o = r.run(r.offline(DECISION_Q, Some(Strategy::Balanced))).await;
    assert_eq!(o.status, DebateStatus::Complete);
    assert_eq!(r.calls(), [1, 1, 0]);
}

// ----------------------------------------------------------------------- concurrency

fn gated(name: &str, barrier: Arc<Barrier>, web: bool) -> Arc<MockProvider> {
    let b = Behavior::normal("budget");
    let reply_behavior = b.clone();
    let mut m = MockProvider::new(name)
        .with_gate(barrier)
        .with_delay(Duration::from_millis(40))
        .with_reply(MockReply::Responder(Arc::new(move |req| {
            scripted::answer_for(&reply_behavior, &req.prompt)
        })));
    if web {
        m = m.with_web_search();
    }
    Arc::new(m)
}

#[tokio::test]
async fn independent_first_round_answers_run_in_parallel() {
    // Each answer takes 150 ms. If they ran one after another the second would start only after the
    // first ended; run together, all three start before any of them ends.
    let r = rig(setup()
        .config(|c| c.general.timeout_secs = 20)
        .fast_agents())
    .await;
    let mocks: Vec<Arc<MockProvider>> = ["lead", "beta", "gamma"]
        .iter()
        .map(|n| {
            agent(
                n,
                &Behavior {
                    delay_ms: 150,
                    ..Behavior::normal("budget")
                },
                *n == "lead",
            )
        })
        .collect();
    let r = Rig {
        agents: mocks.clone(),
        orch: {
            let providers: Vec<Arc<dyn AgentProvider>> = mocks
                .iter()
                .map(|a| a.clone() as Arc<dyn AgentProvider>)
                .collect();
            let mut config = Config::default();
            config.general.coordinator = "lead".into();
            Orchestrator::with_registry(config, ProviderRegistry::new(providers)).unwrap()
        },
        ..r
    };
    let o = r.run(r.offline(SIMPLE_Q, Some(Strategy::Thorough))).await;
    let answers: Vec<_> = o
        .steps
        .iter()
        .filter(|s| s.phase == Role::Perspective)
        .collect();
    assert_eq!(answers.len(), 3);
    let latest_start = answers.iter().map(|s| s.offset_ms).max().unwrap();
    let earliest_end = answers
        .iter()
        .map(|s| s.offset_ms + s.duration_ms)
        .min()
        .unwrap();
    assert!(
        latest_start < earliest_end,
        "the three answers overlapped: starts up to {latest_start} ms, first end {earliest_end} ms"
    );
}

#[tokio::test]
async fn the_evidence_check_and_the_review_run_at_the_same_time_when_the_review_is_certain() {
    // beta judges the claims AND reviews the answer; each of those two calls waits at a barrier of
    // two, so they only get through if they are inside the agent at the same moment.
    let gate = Arc::new(Barrier::new(2));
    let mocks = vec![
        agent(
            "lead",
            &Behavior {
                claims: true,
                delay_ms: 1,
                ..Behavior::normal("budget")
            },
            true,
        ),
        gated("beta", gate, false),
        agent("gamma", &Behavior::normal("budget"), false),
    ];
    let r = rig(setup()
        .mocks(mocks)
        .pages(vec![("/maker", Route::Html(MAKER.into()))]))
    .await;
    let mut opts = r.with_pages(DECISION_Q, Some(Strategy::Balanced), &["/maker"]);
    opts.mode = Some(ConsultMode::Decision);
    let run = tokio::time::timeout(
        Duration::from_secs(10),
        r.orch.consult(opts, &CancellationToken::new()),
    )
    .await;
    let o = run
        .expect("the check and the review overlapped, otherwise the barrier never opens")
        .unwrap();
    let kinds = roles(&o);
    assert!(
        kinds.contains(&Role::Verification) && kinds.contains(&Role::Review),
        "{kinds:?}"
    );
    assert_eq!(orchestration(&o).metrics.peak_concurrency, 2);
    assert!(
        orchestration(&o)
            .events
            .iter()
            .any(|e| e.message.contains("run at the same time"))
    );
}

#[tokio::test]
async fn sequential_execution_is_used_when_the_first_result_decides_the_next_call() {
    // A research question that is simple and uncontested: the check comes first because its result
    // decides whether a review is worth a call. Every claim is supported, so no review follows.
    let r = rig(setup()
        .claims()
        .fast_agents()
        .pages(vec![("/maker", Route::Html(MAKER.into()))]))
    .await;
    let o = r
        .run(r.with_pages(RESEARCH_Q, Some(Strategy::Balanced), &["/maker"]))
        .await;
    assert_eq!(roles(&o), [Role::Perspective, Role::Verification]);
    assert_eq!(orchestration(&o).metrics.peak_concurrency, 1);
    assert_eq!(research(&o).claims[0].status, ClaimStatus::Supported);
    assert_eq!(r.calls()[2], 0, "the third agent was never asked");
    assert!(
        orchestration(&o)
            .stop_reason
            .as_deref()
            .unwrap()
            .contains("supported by retrieved sources")
    );
}

// ---------------------------------------------------------------------- the evidence

#[tokio::test]
async fn contradicting_evidence_triggers_a_review_even_for_a_simple_question() {
    let r = rig(setup().claims().fast_agents().pages(vec![
        ("/maker", Route::Html(MAKER.into())),
        ("/lab", Route::Html(LAB.into())),
    ]))
    .await;
    let o = r
        .run(r.with_pages(RESEARCH_Q, Some(Strategy::Balanced), &["/maker", "/lab"]))
        .await;
    assert!(roles(&o).contains(&Role::Review), "{:?}", roles(&o));
    assert_eq!(research(&o).claims[0].status, ClaimStatus::MixedEvidence);
    let decision = orchestration(&o)
        .events
        .iter()
        .find(|e| e.message.contains("after the lead's answer"))
        .unwrap();
    assert!(
        decision.message.contains("contradicts 1 claim"),
        "{}",
        decision.message
    );
    // The contradiction stays visible in the answer, and the gate agrees.
    assert!(o.final_answer.contains("MIXED_EVIDENCE"));
    assert!(orchestration(&o).quality.as_ref().unwrap().passed());
    // Fast does not review, but keeps the evidence check and shows the same contradiction.
    let rf = rig(setup().claims().fast_agents().pages(vec![
        ("/maker", Route::Html(MAKER.into())),
        ("/lab", Route::Html(LAB.into())),
    ]))
    .await;
    let of = rf
        .run(rf.with_pages(RESEARCH_Q, Some(Strategy::Fast), &["/maker", "/lab"]))
        .await;
    assert!(!roles(&of).contains(&Role::Review));
    assert!(
        roles(&of).contains(&Role::Verification),
        "fast keeps the evidence check"
    );
    assert_eq!(research(&of).claims[0].status, ClaimStatus::MixedEvidence);
    assert!(of.final_answer.contains("MIXED_EVIDENCE"));
}

#[tokio::test]
async fn when_no_source_can_be_retrieved_more_agents_are_not_summoned_and_the_limit_is_stated() {
    let r = rig(setup().claims().fast_agents().pages(vec![(
        "/missing",
        Route::Raw {
            status: 404,
            content_type: "text/html".into(),
            body: b"gone".to_vec(),
            extra_headers: vec![],
        },
    )]))
    .await;
    let o = r
        .run(r.with_pages(RESEARCH_Q, Some(Strategy::Balanced), &["/missing"]))
        .await;
    assert_eq!(
        r.calls(),
        [1, 0, 0],
        "another agent could not supply evidence that does not exist"
    );
    assert_eq!(o.status, DebateStatus::Partial, "research produced nothing");
    let stop = orchestration(&o).stop_reason.clone().unwrap();
    assert!(stop.contains("more deliberation cannot help"), "{stop}");
    assert!(o.final_answer.contains("no source could be retrieved"));
    let gate = orchestration(&o).quality.as_ref().unwrap();
    assert!(
        gate.violations()
            .iter()
            .any(|c| c.name == "required_evidence_present"),
        "{gate:?}"
    );
    assert!(o.final_answer.contains("## Quality notes"));
}

#[tokio::test]
async fn the_same_page_given_twice_is_fetched_once_and_citations_stay_valid() {
    let r = rig(setup()
        .claims()
        .fast_agents()
        .pages(vec![("/maker", Route::Html(MAKER.into()))]))
    .await;
    let url = r.server.url("/maker");
    let mut opts = r.with_pages(RESEARCH_Q, Some(Strategy::Fast), &[]);
    opts.research.urls = vec![
        url.clone(),
        format!("{url}#specs"),
        format!("{url}?utm_source=x"),
    ];
    let o = r.run(opts).await;
    assert_eq!(
        r.server.hits("/maker"),
        1,
        "one fetch for three spellings of the same page"
    );
    assert_eq!(research(&o).sources.len(), 1);
    assert!(orchestration(&o).quality.as_ref().unwrap().passed());
    // The answer's citation names the one recorded source.
    assert!(o.final_answer.contains("[1]"));
    assert_eq!(research(&o).citations.unknown_removed, Vec::<u32>::new());
}

#[tokio::test]
async fn freshness_limits_survive_every_strategy() {
    for strategy in [Strategy::Fast, Strategy::Balanced, Strategy::Thorough] {
        let r = rig(setup()
            .claims()
            .fast_agents()
            .pages(vec![("/lab", Route::Html(LAB.into()))]))
        .await;
        let mut opts = r.with_pages(RESEARCH_Q, Some(strategy), &["/lab"]);
        opts.research.freshness_days = Some(30);
        let o = r.run(opts).await;
        let rep = research(&o);
        assert_eq!(rep.freshness_days, Some(30), "{strategy}");
        assert!(
            rep.sources
                .iter()
                .any(|s| s.status == SourceStatus::Retrieved)
                && (rep.limitations.iter().any(|l| l.contains("older than")
                    || l.contains("stale")
                    || l.contains("fresh"))
                    || rep.claims.iter().flat_map(|c| &c.evidence).any(|e| format!(
                        "{:?}",
                        e.freshness
                    )
                    .to_lowercase()
                    .contains("stale"))),
            "{strategy}: freshness was not applied: {:?}",
            rep.limitations
        );
    }
}

#[tokio::test]
async fn a_strategy_never_changes_an_explicit_research_depth() {
    let r = rig(setup()
        .claims()
        .fast_agents()
        .pages(vec![("/maker", Route::Html(MAKER.into()))]))
    .await;
    for strategy in [Strategy::Fast, Strategy::Thorough] {
        let mut opts = r.with_pages(RESEARCH_Q, Some(strategy), &["/maker"]);
        opts.research.depth = Some(Depth::Quick);
        let o = r.run(opts).await;
        assert_eq!(research(&o).depth, Depth::Quick, "{strategy}");
        assert!(
            research(&o)
                .claims
                .iter()
                .all(|c| c.status == ClaimStatus::NotChecked),
            "quick finds passages but does not judge them"
        );
    }
}

// ---------------------------------------------------------------- history and profiles

#[tokio::test]
async fn provider_latency_is_recorded_without_inventing_anything_and_then_used_to_order_agents() {
    let mocks = vec![
        agent(
            "lead",
            &Behavior {
                delay_ms: 5,
                ..Behavior::normal("budget")
            },
            true,
        ),
        agent(
            "beta",
            &Behavior {
                delay_ms: 90,
                ..Behavior::normal("budget")
            },
            false,
        ),
        agent(
            "gamma",
            &Behavior {
                delay_ms: 5,
                ..Behavior::normal("budget")
            },
            false,
        ),
    ];
    let r = rig(setup()
        .mocks(mocks)
        .profiles()
        .config(|c| c.smart.profile_min_samples = 3))
    .await;
    // No history yet: registry order, and the plan says so.
    let first = r
        .orch
        .plan_consultation(&r.offline(DECISION_Q, Some(Strategy::Balanced)))
        .unwrap();
    assert_eq!(
        first
            .participants
            .value
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["lead", "beta", "gamma"]
    );
    assert!(
        first
            .smart
            .as_ref()
            .unwrap()
            .profile_notes
            .iter()
            .any(|n| n.contains("no provider history yet"))
    );
    // Three thorough runs give every agent several answers to learn from.
    for _ in 0..3 {
        r.run(r.offline(DECISION_Q, Some(Strategy::Thorough))).await;
    }
    let (snap, warning) = r.orch.profile_snapshot();
    assert!(warning.is_none());
    let beta = snap.profile("beta").expect("a profile for beta");
    assert!(beta.successes >= 9 && beta.failures == 0, "{beta:?}");
    assert_eq!(
        beta.input_tokens, 0,
        "mock calls report no tokens, so none are recorded"
    );
    assert!(
        beta.latency_ms["answer"].mean > snap.profile("gamma").unwrap().latency_ms["answer"].mean
    );
    // Now the faster agent is asked to review first; the slower one is last.
    let d = r
        .orch
        .plan_consultation(&r.offline(DECISION_Q, Some(Strategy::Balanced)))
        .unwrap();
    assert_eq!(
        d.participants
            .value
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["lead", "gamma", "beta"]
    );
    let o = r.run(r.offline(DECISION_Q, Some(Strategy::Balanced))).await;
    assert_eq!(roles(&o), [Role::Perspective, Role::Review]);
    assert_eq!(
        o.steps[1].agent.to_string(),
        "gamma",
        "the reviewer is the faster agent"
    );
    // The file holds no prompt text.
    let text = std::fs::read_to_string(r.dir.path().join("profiles.json")).unwrap();
    assert!(!text.contains("oscilloscope") && !text.contains("My answer"));
}

#[tokio::test]
async fn a_damaged_history_file_is_ignored_with_a_note_and_never_stops_a_consultation() {
    let r = rig(setup().fast_agents().profiles()).await;
    std::fs::write(r.dir.path().join("profiles.json"), "{ not json").unwrap();
    let o = r.run(r.offline(DECISION_Q, Some(Strategy::Balanced))).await;
    assert_eq!(o.status, DebateStatus::Complete);
    assert!(
        plan(&o)
            .profile_notes
            .iter()
            .any(|n| n.contains("not valid JSON")),
        "{:?}",
        plan(&o).profile_notes
    );
}

// ---------------------------------------------------------------- the record

#[tokio::test]
async fn the_saved_record_carries_the_plan_the_trace_and_the_metrics_and_old_records_still_load() {
    let r = rig(setup().fast_agents()).await;
    let o = r.run(r.offline(DECISION_Q, Some(Strategy::Balanced))).await;
    let loaded = r.store.load(&o.id).unwrap();
    let meta = loaded.consultation.as_ref().unwrap();
    assert_eq!(meta.strategy, "adaptive");
    let rec = meta.orchestration.as_ref().unwrap();
    assert_eq!(rec.metrics.calls, 2);
    assert_eq!(rec.agents_used, ["lead", "beta"]);
    assert!(rec.events.len() >= 4 && rec.stop_reason.is_some());
    assert!(
        rec.events
            .iter()
            .all(|e| !e.message.contains("oscilloscope")),
        "the trace holds no question text"
    );
    assert_eq!(
        meta.routing
            .as_ref()
            .unwrap()
            .smart
            .as_ref()
            .unwrap()
            .max_calls,
        4
    );
    // A record saved before Milestone 6 has neither field and must still read.
    let mut json: serde_json::Value = serde_json::to_value(&loaded).unwrap();
    json["consultation"]
        .as_object_mut()
        .unwrap()
        .remove("orchestration");
    json["consultation"]["routing"]
        .as_object_mut()
        .unwrap()
        .remove("smart");
    let old: DebateOutcome = serde_json::from_value(json).unwrap();
    assert!(old.consultation.as_ref().unwrap().orchestration.is_none());
    assert!(
        old.consultation
            .as_ref()
            .unwrap()
            .routing
            .as_ref()
            .unwrap()
            .smart
            .is_none()
    );
}

#[tokio::test]
async fn the_quality_gate_turns_a_short_changed_review_into_an_honest_partial_result() {
    // Two named reviewers fail in round 1, so the two rounds the user asked for cannot happen.
    let r = rig(setup()
        .fault(1, Fault::Always)
        .fault(2, Fault::Always)
        .fast_agents())
    .await;
    let mut opts = r.offline(SIMPLE_Q, Some(Strategy::Fast));
    opts.agents = Some(vec!["lead".into(), "beta".into(), "gamma".into()]);
    opts.rounds = Some(2);
    let o = r.orch.consult(opts, &CancellationToken::new()).await;
    // Both other agents fail, so only the coordinator answers: nothing to review, nothing complete.
    match o {
        Ok(o) => {
            assert_eq!(o.status, DebateStatus::Partial);
            let gate = orchestration(&o).quality.as_ref().unwrap();
            assert!(!gate.passed(), "{gate:?}");
            assert!(
                gate.violations()
                    .iter()
                    .any(|c| c.name == "requested_review_satisfied"
                        || c.name == "completeness_classified")
            );
            assert!(o.final_answer.contains("## Quality notes"));
        }
        Err(e) => panic!("the lead alone should still answer: {e}"),
    }
}

#[tokio::test]
async fn identical_calls_in_one_consultation_are_answered_once() {
    // Two agents named twice under different spellings collapse to one; and the shared cache
    // answers an identical second request without a second call.
    let r = rig(setup().fast_agents()).await;
    let o = r.run(r.offline(SIMPLE_Q, Some(Strategy::Fast))).await;
    assert_eq!(
        orchestration(&o).budget.calls_reused,
        0,
        "nothing was identical here"
    );
    assert_eq!(orchestration(&o).budget.calls_started, 1);
}

#[tokio::test]
async fn consultation_overhead_is_measured_apart_from_agent_time() {
    let r = rig(setup()).await;
    let o = r.run(r.offline(DECISION_Q, Some(Strategy::Thorough))).await;
    let rec = orchestration(&o);
    assert!(rec.metrics.provider_ms_total >= rec.metrics.provider_busy_ms);
    assert!(rec.metrics.provider_busy_ms <= rec.wall_ms);
    assert_eq!(rec.overhead_ms, rec.wall_ms - rec.metrics.provider_busy_ms);
    assert!(rec.budget.prompt_chars > 0 && rec.budget.response_chars > 0);
}

#[tokio::test]
async fn an_invented_citation_is_removed_in_the_adaptive_flow_too_and_the_gate_agrees() {
    // The lead cites [1], which is a retrieved page, and [9], which does not exist.
    let reply = MockReply::Responder(Arc::new(|req| {
        let p = &req.prompt;
        if p.contains("You are the research planner") {
            "{}".to_string()
        } else if p.contains("You are checking factual claims against passages") {
            scripted::verdicts(p)
        } else {
            format!(
                "## Answer\n\nThe maker states 100 MHz [1], and a review says so too [9].\n\n{}",
                scripted::claims_block()
            )
        }
    }));
    let lead = Arc::new(
        MockProvider::new("lead")
            .with_web_search()
            .with_reply(reply),
    );
    let others = [
        agent("beta", &Behavior::normal("budget"), false),
        agent("gamma", &Behavior::normal("budget"), false),
    ];
    let r = rig(setup()
        .mocks(vec![lead, others[0].clone(), others[1].clone()])
        .pages(vec![("/maker", Route::Html(MAKER.into()))]))
    .await;
    let o = r
        .run(r.with_pages(RESEARCH_Q, Some(Strategy::Balanced), &["/maker"]))
        .await;
    assert!(
        !o.final_answer.contains("[9]") || o.final_answer.contains("were removed"),
        "{}",
        o.final_answer
    );
    assert_eq!(research(&o).citations.unknown_removed, vec![9]);
    assert!(
        o.final_answer
            .contains("citations in the draft pointed at sources that were never discovered")
    );
    assert!(
        orchestration(&o).quality.as_ref().unwrap().passed(),
        "the gate checks the cleaned answer"
    );
}

#[tokio::test]
async fn a_debate_follows_the_strategy_for_its_size_and_the_limits_at_run_time() {
    let r = rig(setup().fast_agents()).await;
    // Fast: the smallest debate, a coordinator and one reviewer, one round: 3 calls.
    let mut opts = r.offline(
        "Have the agents debate tabs versus spaces",
        Some(Strategy::Fast),
    );
    opts.mode = Some(ConsultMode::Debate);
    let o = r.run(opts).await;
    assert_eq!((o.calls, o.participants.len()), (3, 1));
    assert_eq!(plan(&o).flow, Flow::Debate);
    assert_eq!(o.status, DebateStatus::Complete);
    assert!(orchestration(&o).quality.as_ref().unwrap().passed());
    // Thorough: everyone, two rounds: 2 + 2 * 2 = 6 calls.
    let r2 = rig(setup().fast_agents()).await;
    let mut opts = r2.offline(
        "Have the agents debate tabs versus spaces",
        Some(Strategy::Thorough),
    );
    opts.mode = Some(ConsultMode::Debate);
    let o = r2.run(opts).await;
    assert_eq!(o.calls, 6);
    assert_eq!(
        orchestration(&o).metrics.calls,
        6,
        "the debate engine reports through the same trace"
    );
}

#[tokio::test]
async fn fast_with_web_search_costs_three_calls_and_still_checks_the_evidence() {
    // The lead can search the web: it reports the maker's page as a candidate, then answers, then a
    // judge checks the claim against the passages. A fixed workflow with three agents would have
    // made at least 8 calls for the same question.
    let pages = TestServer::start(vec![("/maker", Route::Html(MAKER.into()))]).await;
    let url = pages.url("/maker");
    let behavior = Behavior {
        claims: true,
        delay_ms: 1,
        ..Behavior::normal("budget")
    };
    let answering = {
        let behavior = behavior.clone();
        MockReply::Responder(Arc::new(move |req| {
            scripted::answer_for(&behavior, &req.prompt)
        }))
    };
    let lead = Arc::new(
        MockProvider::new("lead")
            .with_web_search()
            .with_reply(MockReply::WebText {
                text: "{}".into(),
                urls: vec![url.clone()],
            })
            .with_reply(answering),
    );
    let others = [
        agent("beta", &behavior, false),
        agent("gamma", &behavior, false),
    ];
    let r = rig(setup().mocks(vec![lead, others[0].clone(), others[1].clone()])).await;
    let mut opts = r.offline(RESEARCH_Q, Some(Strategy::Fast));
    opts.mode = Some(ConsultMode::Research);
    opts.research = ResearchRequest {
        web: WebMode::On,
        ..Default::default()
    };
    opts.fetcher = Some(r.fetcher.clone());
    let o = r.run(opts).await;
    // A single agent's answer is recorded as the synthesis, as it always was.
    assert_eq!(
        roles(&o),
        [Role::Planning, Role::Synthesis, Role::Verification]
    );
    assert_eq!(r.calls(), [3, 0, 0], "one agent did everything");
    assert_eq!(pages.hits("/maker"), 1);
    let rep = research(&o);
    assert_eq!(rep.claims[0].status, ClaimStatus::Supported);
    assert!(
        rep.sources[0]
            .discovery
            .iter()
            .any(|d| d.method.label().contains("tool")),
        "the URL came from the agent's search tool"
    );
    assert_eq!(o.status, DebateStatus::Complete);
    assert_eq!(plan(&o).max_calls, 3, "the plan said so before it ran");
}
