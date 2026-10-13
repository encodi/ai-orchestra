//! The research tools of the MCP server (`research`, `verify_claim`, `inspect_sources`,
//! `get_research_report`, and the research options of `consult`) with a real `rmcp` client, mock
//! agents and a local HTTP server serving fixture pages. No real model or website is contacted.

#![cfg(unix)]

#[path = "../src/research/testserver.rs"]
mod testserver;

use std::sync::Arc;
use std::time::Duration;

use ai_orchestra::Orchestrator;
use ai_orchestra::config::Config;
use ai_orchestra::history::DebateStore;
use ai_orchestra::mcp::{McpSettings, OrchestraMcp, serve_transport};
use ai_orchestra::provider::{AgentProvider, ProviderRegistry};
use ai_orchestra::providers::mock::{MockProvider, MockReply};
use ai_orchestra::research::fetch::SourceFetcher;
use ai_orchestra::research::net::FetchPolicy;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::{RoleClient, RunningService};
use serde_json::{Value, json};
use testserver::{Route, TestServer};
use tokio_util::sync::CancellationToken;

const MAKER: &str = include_str!("fixtures/research/maker_spec.html");
const LAB: &str = include_str!("fixtures/research/lab_test.html");

/// A fake model that answers every kind of request by rule (see tests/research.rs).
fn model(claims: &str, synthesis: &str) -> MockReply {
    let (claims, synthesis) = (claims.to_string(), synthesis.to_string());
    MockReply::Responder(Arc::new(move |req| {
        let p = &req.prompt;
        if p.contains("You are the research planner") {
            "{}".to_string()
        } else if p.contains("You are checking factual claims against passages") {
            verdicts(p)
        } else if p.contains("Write the FINAL answer") || p.contains("complete final answer") {
            synthesis.clone()
        } else if p.contains("cross-review") {
            "Review: citations look right.".to_string()
        } else {
            format!("Answer [1].\n\n{claims}")
        }
    }))
}

fn verdicts(prompt: &str) -> String {
    let mut out: Vec<Value> = Vec::new();
    let mut current: Option<(u64, String)> = None;
    let mut items: Vec<Value> = Vec::new();
    let mut rest = prompt;
    while let Some(i) = rest.find('<') {
        rest = &rest[i..];
        if let Some(r) = rest.strip_prefix("<claim id=\"") {
            if let Some((id, _)) = current.take().filter(|_| !items.is_empty()) {
                out.push(json!({"claim": id, "passages": std::mem::take(&mut items)}));
            }
            let id: u64 = r[..r.find('"').unwrap()].parse().unwrap();
            let end = r.find("</claim>").unwrap();
            current = Some((id, r[r.find('>').unwrap() + 1..end].to_string()));
            rest = &r[end..];
        } else if let Some(r) = rest.strip_prefix("<passage id=\"") {
            let pid = r[..r.find('"').unwrap()].to_string();
            let end = r.find("</passage>").unwrap();
            let body = &r[r.find('>').unwrap() + 1..end];
            let wants = current.as_ref().is_some_and(|(_, c)| c.contains("100 MHz"));
            let rel = if wants && body.contains("100 MHz") {
                "supports"
            } else if wants && body.contains("70 MHz") {
                "contradicts"
            } else {
                "unrelated"
            };
            items.push(json!({"id": pid, "relation": rel, "reason": "by rule"}));
            rest = &r[end..];
        } else {
            rest = &rest[1..];
        }
    }
    if let Some((id, _)) = current.filter(|_| !items.is_empty()) {
        out.push(json!({"claim": id, "passages": items}));
    }
    serde_json::to_string(&out).unwrap()
}

const CLAIM_BLOCK: &str = "```claims\n[{\"claim\": \"The Model X has 100 MHz of analog bandwidth\", \"category\": \"product_specification\", \"sources\": [1], \"needs_external_evidence\": true}]\n```";
const SYNTHESIS: &str = "## Answer\n\nYes, the maker states 100 MHz [1].";

struct Rig {
    client: RunningService<RoleClient, ()>,
    server: JoinHandleBox,
    shutdown: CancellationToken,
    pages: TestServer,
    lead: Arc<MockProvider>,
    beta: Arc<MockProvider>,
    gamma: Arc<MockProvider>,
    _dir: tempfile::TempDir,
}

type JoinHandleBox = tokio::task::JoinHandle<()>;

#[derive(Default)]
struct Setup {
    host: Option<&'static str>,
    default_fetcher: bool,
}

async fn start(setup: Setup, routes: Vec<(&str, Route)>) -> Rig {
    let pages = TestServer::start(routes).await;
    let mk = |name: &str, web: bool| {
        let m = MockProvider::new(name).with_reply(model(CLAIM_BLOCK, SYNTHESIS));
        Arc::new(if web { m.with_web_search() } else { m })
    };
    let (lead, beta, gamma) = (mk("lead", true), mk("beta", false), mk("gamma", false));
    let providers: Vec<Arc<dyn AgentProvider>> = vec![lead.clone(), beta.clone(), gamma.clone()];
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.general.coordinator = "lead".into();
    // These tests exercise the fixed Milestone 5 orchestration; strategies have their own tests.
    config.smart.default_strategy = "off".into();
    let orch = Orchestrator::with_registry(config.clone(), ProviderRegistry::new(providers))
        .unwrap()
        .with_history(DebateStore::new(dir.path().join("debates")));
    let mut settings = McpSettings::from_config(&config, &orch, setup.host).unwrap();
    if !setup.default_fetcher {
        settings.fetcher = Some(Arc::new(
            SourceFetcher::new(FetchPolicy {
                respect_robots: false,
                ..FetchPolicy::for_local_tests()
            })
            .unwrap(),
        ));
    }
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
    Rig {
        client,
        server: task,
        shutdown,
        pages,
        lead,
        beta,
        gamma,
        _dir: dir,
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

    fn url(&self, path: &str) -> String {
        self.pages.url(path)
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

fn pages() -> Vec<(&'static str, Route)> {
    vec![
        ("/maker", Route::Html(MAKER.into())),
        ("/lab", Route::Html(LAB.into())),
    ]
}

#[tokio::test]
async fn the_research_tool_returns_claims_sources_and_citations_and_the_report_is_retrievable() {
    let r = start(Setup::default(), pages()).await;
    let v = data(&r.call("research", json!({"question": "Does the Model X have 100 MHz of bandwidth?", "urls": [r.url("/maker")], "agents": ["lead", "beta"], "rounds": 1})).await);
    assert_eq!(v["status"], "completed", "{v}");
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(v["mode"], "research");
    let rep = &v["research"];
    assert_eq!(rep["retrieved_sources"], 1);
    assert_eq!(rep["sources"][0]["url"], r.url("/maker"));
    assert_eq!(rep["sources"][0]["published"], "2025-03-04");
    assert_eq!(
        rep["sources"][0]["updated"],
        Value::Null,
        "not stated, not invented"
    );
    assert_eq!(rep["claims"][0]["status"], "SUPPORTED");
    assert!(
        rep["claims"][0]["evidence"][0]["passage"]
            .as_str()
            .unwrap()
            .contains("100 MHz")
    );
    assert!(
        v["answer"].as_str().unwrap().contains("## Sources"),
        "{}",
        v["answer"]
    );
    assert!(v["caveat"].as_str().unwrap().contains("model-dependent"));

    // The saved report comes back with the same sources and claims.
    let id = v["consultation_id"].as_str().unwrap().to_string();
    let rr = data(
        &r.call("get_research_report", json!({"consultation_id": id}))
            .await,
    );
    assert_eq!(rr["ok"], true);
    assert_eq!(rr["report"]["sources"][0]["original_url"], r.url("/maker"));
    assert_eq!(rr["report"]["claims"][0]["status"], "SUPPORTED");
    assert_eq!(rr["report"]["citations"]["cited"], json!([1]));
    // An unknown id and a record without research are clear errors.
    let none = r
        .call("get_research_report", json!({"consultation_id": "nope"}))
        .await;
    assert!(is_error(&none));
    r.stop().await;
}

#[tokio::test]
async fn consult_accepts_the_research_options_and_web_off_uses_no_network() {
    let r = start(Setup::default(), pages()).await;
    let v = data(&r.call("consult", json!({"question": "Does the Model X have 100 MHz of bandwidth?", "mode": "research", "web": "off", "agents": ["lead", "beta"], "rounds": 1})).await);
    assert_eq!(v["status"], "completed", "{v}");
    assert_eq!(v["research"], Value::Null);
    assert!(
        v["answer"]
            .as_str()
            .unwrap()
            .contains("nothing in this answer was checked against external sources"),
        "{}",
        v["answer"]
    );
    assert_eq!(r.pages.total_hits(), 0);
    for q in r.lead.requests().iter().chain(&r.beta.requests()) {
        assert!(!q.web && !q.prompt.contains("<evidence>"));
    }
    // Same consult with sources: depth and max_sources are honoured.
    let v = data(&r.call("consult", json!({"question": "Does the Model X have 100 MHz of bandwidth?", "mode": "research", "urls": [r.url("/maker"), r.url("/lab")], "depth": "quick", "max_sources": 1, "agents": ["lead", "beta"]})).await);
    assert_eq!(v["research"]["retrieved_sources"], 1);
    assert_eq!(v["research"]["depth"], "quick");
    r.stop().await;
}

#[tokio::test]
async fn verify_claim_checks_one_claim_against_the_given_sources_without_an_answer_panel() {
    let r = start(Setup::default(), pages()).await;
    let v = data(&r.call("verify_claim", json!({"claim": "The Model X has 100 MHz of analog bandwidth", "urls": [r.url("/maker"), r.url("/lab")], "agents": ["lead", "beta"]})).await);
    assert_eq!(v["status"], "completed", "{v}");
    let claim = &v["research"]["claims"][0];
    assert_eq!(claim["status"], "MIXED_EVIDENCE", "{claim}");
    assert_eq!(
        (
            claim["supporting_independent_sources"].as_u64(),
            claim["contradicting_independent_sources"].as_u64()
        ),
        (Some(1), Some(1))
    );
    assert_eq!(v["research"]["contradictions"][0]["claim_id"], 1);
    // Only verification was done: no independent answers, no reviews, no synthesis.
    let roles = r.lead.requests().len() + r.beta.requests().len();
    assert_eq!(roles, 1, "one verifier call and nothing else");
    assert!(
        v["answer"]
            .as_str()
            .unwrap()
            .contains("Claim checked: The Model X has 100 MHz"),
        "{}",
        v["answer"]
    );
    // The sources the verdict rests on are named in the answer and count as cited.
    assert!(
        v["answer"]
            .as_str()
            .unwrap()
            .contains("Passages used: [1] [2]"),
        "{}",
        v["answer"]
    );
    // A claim with no way to find sources is rejected up front.
    let none = r.call("verify_claim", json!({"claim": "The Model X has 100 MHz of analog bandwidth", "agents": ["beta", "gamma"]})).await;
    assert!(is_error(&none));
    assert!(
        data(&none)["error"]["message"]
            .as_str()
            .unwrap()
            .contains("needs"),
        "{}",
        data(&none)
    );
    r.stop().await;
}

#[tokio::test]
async fn inspect_sources_describes_pages_without_calling_any_agent_and_reports_refusals() {
    let r = start(Setup::default(), pages()).await;
    let v = data(
        &r.call(
            "inspect_sources",
            json!({"urls": [r.url("/maker"), r.url("/lab"), r.url("/missing")]}),
        )
        .await,
    );
    assert_eq!(v["status"], "completed", "{v}");
    assert_eq!(v["retrieved"], 2);
    assert_eq!(
        v["sources"][0]["title"],
        "Model X Oscilloscope - Specifications"
    );
    assert_eq!(
        (
            v["sources"][1]["published"].as_str(),
            v["sources"][1]["updated"].as_str()
        ),
        (Some("2025-06-10"), Some("2025-07-01"))
    );
    assert_eq!(v["sources"][2]["status"], "inaccessible");
    assert_eq!(v["sources"][2]["http_status"], 404);
    assert_eq!(
        r.lead.requests().len() + r.beta.requests().len() + r.gamma.requests().len(),
        0,
        "no agent was called"
    );
    // Bad input.
    assert!(is_error(
        &r.call("inspect_sources", json!({"urls": []})).await
    ));
    assert!(is_error(
        &r.call(
            "inspect_sources",
            json!({"urls": (0..11).map(|i| format!("https://example.org/{i}")).collect::<Vec<_>>()})
        )
        .await
    ));
    r.stop().await;

    // With the product's own fetcher (default policy) local and non-HTTPS destinations are refused.
    let r = start(
        Setup {
            default_fetcher: true,
            ..Default::default()
        },
        pages(),
    )
    .await;
    let v = data(&r.call("inspect_sources", json!({"urls": [r.url("/maker"), "https://127.0.0.1/x", "https://169.254.169.254/latest/meta-data/", "file:///etc/passwd"]})).await);
    assert_eq!(v["retrieved"], 0);
    assert_eq!(v["ok"], false);
    for s in v["sources"].as_array().unwrap() {
        assert_eq!(s["status"], "blocked", "{s}");
    }
    assert_eq!(r.pages.total_hits(), 0, "nothing reached the local server");
    r.stop().await;
}

#[tokio::test]
async fn research_options_are_validated_before_anything_runs() {
    let r = start(Setup::default(), pages()).await;
    for bad in [
        json!({"question": "q", "web": "maybe"}),
        json!({"question": "q", "depth": "infinite"}),
        json!({"question": "q", "max_sources": 0}),
        json!({"question": "q", "max_sources": 21}),
        json!({"question": "q", "freshness_days": 0}),
        json!({"question": "q", "urls": ["file:///etc/passwd"], "web": "on"}),
        json!({"question": "q", "web": "off", "urls": ["https://example.org/a"]}),
        json!({"question": "q", "unknown_field": 1}),
    ] {
        if let Ok(res) = r
            .client
            .call_tool(
                CallToolRequestParams::new("research".to_string())
                    .with_arguments(bad.as_object().cloned().unwrap()),
            )
            .await
        {
            assert!(is_error(&res), "{bad} should be rejected: {res:?}");
        }
    }
    assert_eq!(
        r.lead.requests().len() + r.beta.requests().len() + r.gamma.requests().len(),
        0
    );
    r.stop().await;
}

#[tokio::test]
async fn the_host_agent_is_never_used_for_research_either() {
    // lead hosts the server and is also the only agent that can search the web.
    let r = start(
        Setup {
            host: Some("lead"),
            ..Default::default()
        },
        pages(),
    )
    .await;
    // web on with no URL: the only searcher is the host, so there is no way to find sources.
    let no = r
        .call(
            "research",
            json!({"question": "What is the latest news on the Model X?"}),
        )
        .await;
    assert!(is_error(&no));
    assert!(
        data(&no)["error"]["message"]
            .as_str()
            .unwrap()
            .contains("--web on needs a way to find or fetch sources"),
        "{}",
        data(&no)
    );
    // With URLs it works, with the host left out entirely.
    let v = data(&r.call("research", json!({"question": "Does the Model X have 100 MHz of bandwidth?", "urls": [r.url("/maker")]})).await);
    assert_eq!(v["status"], "completed", "{v}");
    assert_eq!(v["coordinator"], "beta");
    assert!(
        v["host_note"]
            .as_str()
            .unwrap()
            .contains("lead hosts this server")
    );
    // Naming the host as coordinator is refused, for every consult-like tool.
    for tool in ["research", "consult"] {
        let blocked = r
            .call(
                tool,
                json!({"question": "q", "coordinator": "lead", "urls": [r.url("/maker")]}),
            )
            .await;
        assert!(is_error(&blocked), "{tool}");
        assert_eq!(data(&blocked)["error"]["kind"], "host_agent_blocked");
    }
    // verify_claim judges with the other agents.
    let v = data(&r.call("verify_claim", json!({"claim": "The Model X has 100 MHz of analog bandwidth", "urls": [r.url("/maker")]})).await);
    assert_eq!(v["status"], "completed", "{v}");
    assert_eq!(r.lead.requests().len(), 0, "the host was never called");
    assert!(
        r.beta
            .requests()
            .iter()
            .chain(&r.gamma.requests())
            .all(|q| !q.web),
        "analysts never browse"
    );
    r.stop().await;
}

#[tokio::test]
async fn long_research_becomes_a_task_that_can_be_followed_and_cancelled() {
    let slow = vec![
        (
            "/slow",
            Route::Slow(
                Duration::from_millis(400),
                Box::new(Route::Html(MAKER.into())),
            ),
        ),
        (
            "/stuck",
            Route::Slow(Duration::from_secs(30), Box::new(Route::Html(MAKER.into()))),
        ),
    ];
    let r = start(Setup::default(), slow).await;
    let first = data(&r.call("research", json!({"question": "Does the Model X have 100 MHz of bandwidth?", "urls": [r.url("/slow")], "agents": ["lead", "beta"], "wait_seconds": 0})).await);
    assert!(
        matches!(first["status"].as_str(), Some("pending" | "running")),
        "{first}"
    );
    assert_eq!(first["kind"], "research");
    let id = first["task_id"].as_str().unwrap().to_string();
    let done = data(
        &r.call(
            "get_task_status",
            json!({"task_id": id, "wait_seconds": 30}),
        )
        .await,
    );
    assert_eq!(done["status"], "completed", "{done}");
    assert_eq!(done["research"]["claims"][0]["status"], "SUPPORTED");

    // Cancellation during retrieval terminates the work promptly.
    let first = data(&r.call("verify_claim", json!({"claim": "The Model X has 100 MHz of analog bandwidth", "urls": [r.url("/stuck")], "agents": ["lead", "beta"], "wait_seconds": 0})).await);
    let id = first["task_id"].as_str().unwrap().to_string();
    while r.pages.hits("/stuck") == 0 {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let _ = r.call("cancel_task", json!({"task_id": id})).await;
    let t = std::time::Instant::now();
    let done = data(
        &r.call(
            "get_task_status",
            json!({"task_id": id, "wait_seconds": 30}),
        )
        .await,
    );
    assert_eq!(done["status"], "cancelled", "{done}");
    assert!(t.elapsed() < Duration::from_secs(10));
    r.stop().await;
}

#[tokio::test]
async fn a_partial_source_failure_is_visible_in_the_tool_result() {
    let r = start(
        Setup::default(),
        vec![("/maker", Route::Html(MAKER.into()))],
    )
    .await;
    let v = data(&r.call("research", json!({"question": "Does the Model X have 100 MHz of bandwidth?", "urls": [r.url("/maker"), r.url("/gone")], "agents": ["lead", "beta"], "rounds": 1})).await);
    assert_eq!(v["ok"], true, "one source was enough: {v}");
    let sources = v["research"]["sources"].as_array().unwrap();
    assert_eq!(
        sources
            .iter()
            .filter(|s| s["status"] == "inaccessible")
            .count(),
        1
    );
    assert!(
        v["research"]["limitations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l.as_str().unwrap().contains("could not be retrieved"))
    );
    r.stop().await;
}
