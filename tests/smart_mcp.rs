//! Strategies and limits through the MCP `consult` tool, with a real `rmcp` client and scripted
//! in-process agents. No real model or website is contacted.

#![cfg(unix)]

#[path = "../src/research/testserver.rs"]
mod testserver;

#[path = "common/scripted.rs"]
mod scripted;

use std::sync::Arc;
use std::time::Duration;

use ai_orchestra::Orchestrator;
use ai_orchestra::config::Config;
use ai_orchestra::history::DebateStore;
use ai_orchestra::mcp::{McpSettings, OrchestraMcp, serve_transport};
use ai_orchestra::provider::{AgentProvider, ProviderRegistry};
use ai_orchestra::providers::mock::MockProvider;
use ai_orchestra::research::fetch::SourceFetcher;
use ai_orchestra::research::net::FetchPolicy;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::{RoleClient, RunningService};
use scripted::{Behavior, scripted as agent};
use serde_json::{Value, json};
use testserver::{Route, TestServer};
use tokio_util::sync::CancellationToken;

const MAKER: &str = include_str!("fixtures/research/maker_spec.html");

const DECISION_Q: &str = "Should I buy a cheap oscilloscope or invest in a professional one?";
const SIMPLE_Q: &str = "Explain electrical resistance";

struct Rig {
    client: RunningService<RoleClient, ()>,
    server: tokio::task::JoinHandle<()>,
    shutdown: CancellationToken,
    agents: Vec<Arc<MockProvider>>,
}

#[derive(Default)]
struct Setup {
    host: Option<&'static str>,
    default_strategy: Option<&'static str>,
}

async fn start(setup: Setup) -> Rig {
    let behavior = Behavior {
        delay_ms: 1,
        ..Behavior::normal("budget")
    };
    let agents: Vec<Arc<MockProvider>> = ["claude", "codex", "google"]
        .iter()
        .map(|n| agent(n, &behavior, false))
        .collect();
    let providers: Vec<Arc<dyn AgentProvider>> = agents
        .iter()
        .map(|a| a.clone() as Arc<dyn AgentProvider>)
        .collect();
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.general.coordinator = "codex".into();
    if let Some(s) = setup.default_strategy {
        config.smart.default_strategy = s.into();
    }
    let orch = Orchestrator::with_registry(config.clone(), ProviderRegistry::new(providers))
        .unwrap()
        .with_history(DebateStore::new(dir.path().join("debates")));
    let settings = McpSettings::from_config(&config, &orch, setup.host).unwrap();
    let server = OrchestraMcp::new(Arc::new(orch), settings);
    let (server_io, client_io) = tokio::io::duplex(1 << 20);
    let shutdown = CancellationToken::new();
    let task = tokio::spawn({
        let shutdown = shutdown.clone();
        async move {
            let _ = serve_transport(server, server_io, shutdown).await;
            drop(dir);
        }
    });
    let client = ().serve(client_io).await.expect("client initializes");
    Rig {
        client,
        server: task,
        shutdown,
        agents,
    }
}

impl Rig {
    async fn call(&self, name: &str, args: Value) -> CallToolResult {
        let params = CallToolRequestParams::new(name.to_string())
            .with_arguments(args.as_object().cloned().unwrap_or_default());
        self.client
            .call_tool(params)
            .await
            .expect("protocol-level success")
    }

    fn calls(&self) -> Vec<usize> {
        self.agents.iter().map(|a| a.requests().len()).collect()
    }

    async fn stop(self) {
        drop(self.client);
        self.shutdown.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(20), self.server).await;
    }
}

fn data(r: &CallToolResult) -> Value {
    r.structured_content.clone().expect("structured content")
}

fn is_error(r: &CallToolResult) -> bool {
    r.is_error == Some(true)
}

#[tokio::test]
async fn consult_takes_a_strategy_and_reports_it_with_what_happened() {
    let r = start(Setup::default()).await;
    let v = data(
        &r.call(
            "consult",
            json!({"question": SIMPLE_Q, "strategy": "fast", "web": "off", "wait_seconds": 30}),
        )
        .await,
    );
    assert_eq!(v["status"], "completed", "{v}");
    assert_eq!(v["calls"], 1, "{v}");
    assert_eq!(v["strategy"]["value"], "fast");
    assert_eq!(v["strategy"]["source"], "manual");
    assert_eq!(v["orchestration"]["calls"], 1);
    assert!(v["orchestration"]["events"].as_array().unwrap().len() >= 2);
    assert_eq!(r.calls().iter().sum::<usize>(), 1);
    r.stop().await;
}

#[tokio::test]
async fn balanced_with_a_call_limit_stays_inside_it_and_says_what_it_skipped() {
    let r = start(Setup::default()).await;
    let v = data(&r.call("consult", json!({
        "question": DECISION_Q, "strategy": "balanced", "web": "off", "max_agent_calls": 2, "wait_seconds": 30,
    })).await);
    assert_eq!(v["status"], "completed", "{v}");
    assert!(v["calls"].as_u64().unwrap() <= 2, "{v}");
    assert_eq!(
        v["orchestration"]["budget_stopped_by"],
        Value::Null,
        "the plan fit the limit, nothing had to be refused"
    );
    r.stop().await;
}

#[tokio::test]
async fn dry_run_returns_the_plan_and_calls_no_agent() {
    let r = start(Setup::default()).await;
    let res = r
        .call(
            "consult",
            json!({"question": DECISION_Q, "strategy": "thorough", "dry_run": true, "web": "off"}),
        )
        .await;
    assert!(!is_error(&res));
    let v = data(&res);
    assert_eq!(v["dry_run"], true);
    assert_eq!(v["plan"]["smart"]["strategy"]["value"], "thorough");
    assert_eq!(v["plan"]["smart"]["max_calls"], 10);
    assert!(
        v["explanation"]
            .as_str()
            .unwrap()
            .contains("flow:         panel")
    );
    assert!(v["note"].as_str().unwrap().contains("Nothing was run"));
    assert_eq!(r.calls(), [0, 0, 0]);
    // The same through the research tool.
    let v = data(
        &r.call(
            "research",
            json!({"question": DECISION_Q, "strategy": "fast", "dry_run": true, "web": "off"}),
        )
        .await,
    );
    assert_eq!(v["plan"]["smart"]["flow"], "single");
    assert_eq!(r.calls(), [0, 0, 0]);
    r.stop().await;
}

#[tokio::test]
async fn bad_strategies_and_impossible_limits_are_rejected_before_any_agent_runs() {
    let r = start(Setup::default()).await;
    for args in [
        json!({"question": SIMPLE_Q, "strategy": "turbo"}),
        json!({"question": SIMPLE_Q, "max_agent_calls": 0}),
        json!({"question": SIMPLE_Q, "max_duration_seconds": 0}),
        json!({"question": SIMPLE_Q, "max_parallel": 0}),
        json!({"question": SIMPLE_Q, "max_duration_seconds": 99999999}),
        json!({"question": DECISION_Q, "strategy": "auto", "agents": ["claude", "codex", "google"], "rounds": 2, "max_agent_calls": 5, "web": "off"}),
    ] {
        let res = r.call("consult", args.clone()).await;
        let is_rejection = is_error(&res) || data(&res)["status"] == "rejected";
        assert!(is_rejection, "{args}: {}", data(&res));
    }
    let res = r.call("consult", json!({"question": DECISION_Q, "strategy": "auto", "agents": ["claude", "codex", "google"], "rounds": 2, "max_agent_calls": 5, "web": "off"})).await;
    assert!(
        data(&res)["error"]["message"]
            .as_str()
            .unwrap()
            .contains("at least 10 agent call(s)"),
        "{}",
        data(&res)
    );
    assert_eq!(r.calls(), [0, 0, 0], "no subscription was touched");
    r.stop().await;
}

#[tokio::test]
async fn the_configured_default_strategy_applies_when_the_call_names_none() {
    let r = start(Setup {
        default_strategy: Some("fast"),
        ..Default::default()
    })
    .await;
    let v = data(
        &r.call(
            "consult",
            json!({"question": DECISION_Q, "web": "off", "wait_seconds": 30}),
        )
        .await,
    );
    assert_eq!(v["calls"], 1, "{v}");
    assert_eq!(v["strategy"]["value"], "fast");
    r.stop().await;
    // `off` keeps the fixed workflow, and the result has no smart record.
    let r = start(Setup {
        default_strategy: Some("off"),
        ..Default::default()
    })
    .await;
    let v = data(
        &r.call(
            "consult",
            json!({"question": DECISION_Q, "web": "off", "wait_seconds": 30}),
        )
        .await,
    );
    assert!(
        v["strategy"].is_null() && v["orchestration"].is_null(),
        "{v}"
    );
    assert!(v["calls"].as_u64().unwrap() > 4, "{v}");
    r.stop().await;
}

#[tokio::test]
async fn no_strategy_ever_calls_the_host_agent_and_an_explicit_host_coordinator_is_refused() {
    for strategy in ["fast", "balanced", "thorough", "auto"] {
        let r = start(Setup {
            host: Some("claude"),
            ..Default::default()
        })
        .await;
        let v = data(&r.call("consult", json!({"question": DECISION_Q, "strategy": strategy, "web": "off", "wait_seconds": 30})).await);
        assert_eq!(v["status"], "completed", "{strategy}: {v}");
        assert_eq!(
            r.calls()[0],
            0,
            "{strategy}: the host (claude) must never be called"
        );
        assert!(
            v["skipped"].to_string().contains("hosts this MCP server")
                || v["skipped"].to_string().contains("claude"),
            "{strategy}: {}",
            v["skipped"]
        );
        r.stop().await;
    }
    let r = start(Setup {
        host: Some("claude"),
        ..Default::default()
    })
    .await;
    let res = r.call("consult", json!({"question": DECISION_Q, "strategy": "fast", "coordinator": "claude", "web": "off"})).await;
    assert!(is_error(&res), "{}", data(&res));
    assert_eq!(data(&res)["error"]["kind"], "host_agent_blocked");
    // Naming the host in `agents` is reported, never silently honoured.
    let v = data(&r.call("consult", json!({"question": DECISION_Q, "strategy": "balanced", "agents": ["claude", "codex"], "web": "off", "wait_seconds": 30})).await);
    assert_eq!(r.calls()[0], 0);
    assert!(
        v["skipped"].to_string().contains("claude"),
        "{}",
        v["skipped"]
    );
    r.stop().await;
}

#[tokio::test]
async fn a_long_consult_is_a_task_that_can_be_followed_and_its_record_has_the_trace() {
    let r = start(Setup::default()).await;
    let v = data(&r.call("consult", json!({"question": DECISION_Q, "strategy": "thorough", "web": "off", "wait_seconds": 0})).await);
    let id = v["task_id"].as_str().expect("a task id").to_string();
    let done = data(
        &r.call(
            "get_task_status",
            json!({"task_id": id, "wait_seconds": 30}),
        )
        .await,
    );
    assert_eq!(done["status"], "completed", "{done}");
    assert_eq!(done["calls"], 10);
    let consultation_id = done["consultation_id"].as_str().unwrap();
    let saved = data(
        &r.call("get_debate", json!({"debate_id": consultation_id}))
            .await,
    );
    assert_eq!(saved["strategy"]["value"], "thorough");
    assert!(
        saved["orchestration"]["events"].as_array().unwrap().len() >= 3,
        "the saved record carries the trace: {saved}"
    );
    r.stop().await;
}

#[tokio::test]
async fn research_and_verify_claim_run_under_the_default_strategy_with_the_evidence_check_kept() {
    // Agents that end their answers with the claim, so there is something to check.
    let pages = TestServer::start(vec![("/maker", Route::Html(MAKER.into()))]).await;
    let behavior = Behavior {
        delay_ms: 1,
        claims: true,
        ..Behavior::normal("budget")
    };
    let agents: Vec<Arc<MockProvider>> = ["claude", "codex", "google"]
        .iter()
        .map(|n| agent(n, &behavior, false))
        .collect();
    let providers: Vec<Arc<dyn AgentProvider>> = agents
        .iter()
        .map(|a| a.clone() as Arc<dyn AgentProvider>)
        .collect();
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.general.coordinator = "codex".into();
    let orch = Orchestrator::with_registry(config.clone(), ProviderRegistry::new(providers))
        .unwrap()
        .with_history(DebateStore::new(dir.path().join("debates")));
    let mut settings = McpSettings::from_config(&config, &orch, Some("claude")).unwrap();
    settings.fetcher = Some(Arc::new(
        SourceFetcher::new(FetchPolicy {
            respect_robots: false,
            ..FetchPolicy::for_local_tests()
        })
        .unwrap(),
    ));
    let server = OrchestraMcp::new(Arc::new(orch), settings);
    let (server_io, client_io) = tokio::io::duplex(1 << 20);
    let shutdown = CancellationToken::new();
    let task = tokio::spawn({
        let shutdown = shutdown.clone();
        async move {
            let _ = serve_transport(server, server_io, shutdown).await;
        }
    });
    let client = ().serve(client_io).await.expect("client initializes");
    let call = |name: &'static str, args: Value| {
        let client = &client;
        async move {
            let params = CallToolRequestParams::new(name.to_string())
                .with_arguments(args.as_object().cloned().unwrap_or_default());
            data(
                &client
                    .call_tool(params)
                    .await
                    .expect("protocol-level success"),
            )
        }
    };
    let url = pages.url("/maker");
    // verify_claim: one judge, no answer panel, the host never involved.
    let v = call(
        "verify_claim",
        json!({"claim": scripted::CLAIM, "urls": [url], "wait_seconds": 30}),
    )
    .await;
    assert_eq!(v["status"], "completed", "{v}");
    assert_eq!(v["research"]["claims"][0]["status"], "SUPPORTED", "{v}");
    assert_eq!(v["strategy"]["source"], "auto");
    assert!(v["orchestration"]["calls"].as_u64().unwrap() <= 2, "{v}");
    assert_eq!(
        agents[0].requests().len(),
        0,
        "the host (claude) was not called"
    );
    // research with a call limit: the evidence check stays, the limit holds.
    let v = call("research", json!({
        "question": "Does the Model X have 100 MHz of analog bandwidth?", "urls": [pages.url("/maker")],
        "strategy": "fast", "max_agent_calls": 2, "wait_seconds": 30,
    })).await;
    assert_eq!(v["status"], "completed", "{v}");
    assert!(v["calls"].as_u64().unwrap() <= 2, "{v}");
    assert_eq!(v["research"]["claims"][0]["status"], "SUPPORTED", "{v}");
    assert_eq!(v["strategy"]["value"], "fast");
    // Naming `quick` depth explicitly is respected: passages found, not judged.
    let v = call("research", json!({
        "question": "Does the Model X have 100 MHz of analog bandwidth?", "urls": [pages.url("/maker")],
        "strategy": "thorough", "depth": "quick", "wait_seconds": 30,
    })).await;
    assert_eq!(v["research"]["claims"][0]["status"], "NOT_CHECKED", "{v}");
    drop(client);
    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(20), task).await;
}
