//! MCP tests with a real `rmcp` client talking to the real server over an
//! in-memory pipe, with mock agents. No process is spawned and no quota is used.
//! Concurrency is checked with barriers and task state, not with sleeps.

use std::sync::Arc;
use std::time::Duration;

use ai_orchestra::Orchestrator;
use ai_orchestra::config::Config;
use ai_orchestra::history::DebateStore;
use ai_orchestra::mcp::{McpSettings, OrchestraMcp, TOOL_NAMES, serve_transport};
use ai_orchestra::provider::{AgentProvider, InvokeContext, ORCHESTRA_DEPTH_ENV, ProviderRegistry};
use ai_orchestra::providers::mock::{MockProvider, MockReply};
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::{RoleClient, RunningService};
use rmcp::{ServiceError, ServiceExt};
use serde_json::{Value, json};
use tokio::sync::Barrier;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

const SYNTHESIS: &str = "## Recommended solution\nUse X.\n\n## Significant disagreements\n- beta objected to Y; unresolved.\n\n## Unresolved risks\nNone.";

struct Harness {
    client: RunningService<RoleClient, ()>,
    server: JoinHandle<()>,
    shutdown: CancellationToken,
    store: DebateStore,
    mocks: Vec<Arc<MockProvider>>,
    _dir: tempfile::TempDir,
}

#[derive(Default)]
struct Setup {
    host: Option<&'static str>,
    allow_host: bool,
    max_concurrent: Option<usize>,
    max_queued: Option<usize>,
    max_task_secs: Option<u64>,
    max_result_bytes: Option<usize>,
    no_history: bool,
    depth: Option<u32>,
}

fn text(s: &str) -> MockReply {
    MockReply::Text(s.to_string())
}

async fn start(setup: Setup, mocks: Vec<Arc<MockProvider>>) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let store = DebateStore::new(dir.path().join("debates"));
    let mut config = Config::default();
    config.general.coordinator = mocks[0].id().to_string();
    // These tests exercise the fixed Milestone 3-5 orchestration; strategies have their own tests.
    config.smart.default_strategy = "off".into();
    if let Some(n) = setup.max_concurrent {
        config.mcp.max_concurrent_tasks = n;
    }
    if let Some(n) = setup.max_queued {
        config.mcp.max_queued_tasks = n;
    }
    if let Some(n) = setup.max_task_secs {
        config.mcp.max_task_secs = n;
    }
    if let Some(n) = setup.max_result_bytes {
        config.mcp.max_result_bytes = n;
    }
    config.mcp.allow_host_agent = setup.allow_host;
    let providers: Vec<Arc<dyn AgentProvider>> = mocks
        .iter()
        .map(|m| m.clone() as Arc<dyn AgentProvider>)
        .collect();
    let mut ctx = InvokeContext::from_config(&config);
    if let Some(d) = setup.depth {
        ctx.depth = d;
    }
    let mut orchestrator =
        Orchestrator::with_context(config.clone(), ProviderRegistry::new(providers), ctx).unwrap();
    if !setup.no_history {
        orchestrator = orchestrator.with_history(store.clone());
    }
    let settings = McpSettings::from_config(&config, &orchestrator, setup.host).unwrap();
    let server = OrchestraMcp::new(Arc::new(orchestrator), settings);

    let (server_io, client_io) = tokio::io::duplex(1 << 20);
    let shutdown = CancellationToken::new();
    let task = tokio::spawn({
        let shutdown = shutdown.clone();
        async move {
            let _ = serve_transport(server, server_io, shutdown).await;
        }
    });
    let client = ().serve(client_io).await.expect("client initializes");
    Harness {
        client,
        server: task,
        shutdown,
        store,
        mocks,
        _dir: dir,
    }
}

fn mock(name: &str) -> Arc<MockProvider> {
    Arc::new(MockProvider::new(name))
}

impl Harness {
    async fn try_call(&self, name: &str, args: Value) -> Result<CallToolResult, ServiceError> {
        let params = CallToolRequestParams::new(name.to_string())
            .with_arguments(args.as_object().cloned().unwrap_or_default());
        self.client.call_tool(params).await
    }

    async fn call(&self, name: &str, args: Value) -> CallToolResult {
        self.try_call(name, args)
            .await
            .expect("tool call succeeds at protocol level")
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

fn error_kind(v: &Value) -> &str {
    v["error"]["kind"].as_str().unwrap_or("")
}

// ---- initialization and discovery --------------------------------------------

#[tokio::test]
async fn initialization_negotiates_and_advertises_tools_only() {
    let h = start(Setup::default(), vec![mock("lead"), mock("beta")]).await;
    let info = h.client.peer_info().expect("server info after initialize");
    let server = info.server_info.as_ref().expect("server_info");
    assert_eq!(server.name, "ai-orchestra");
    assert_eq!(server.version, "0.9.1");
    assert!(info.capabilities.tools.is_some(), "tools capability");
    assert!(info.capabilities.resources.is_none() && info.capabilities.prompts.is_none());
    let instructions = info.instructions.clone().unwrap_or_default();
    assert!(
        instructions.contains("list_agents") && instructions.contains("never called through it"),
        "{instructions}"
    );
    h.stop().await;
}

#[tokio::test]
async fn tool_discovery_lists_the_tools_with_valid_schemas() {
    let h = start(Setup::default(), vec![mock("lead"), mock("beta")]).await;
    let tools = h.client.list_all_tools().await.unwrap();
    let mut names: Vec<String> = tools.iter().map(|t| t.name.to_string()).collect();
    names.sort();
    let mut expected: Vec<String> = TOOL_NAMES.iter().map(|s| s.to_string()).collect();
    expected.sort();
    assert_eq!(names, expected);

    for t in &tools {
        let schema = Value::Object((*t.input_schema).clone());
        assert_eq!(schema["type"], "object", "{}: {schema}", t.name);
        assert!(
            t.description.as_deref().is_some_and(|d| d.len() > 20),
            "{} needs a description",
            t.name
        );
        assert!(t.annotations.is_some(), "{} needs annotations", t.name);
    }
    let schema_of = |name: &str| -> Value {
        let t = tools.iter().find(|t| t.name == name).unwrap();
        Value::Object((*t.input_schema).clone())
    };
    let required = |v: &Value| -> Vec<String> {
        v["required"]
            .as_array()
            .map(|a| a.iter().map(|x| x.as_str().unwrap().to_string()).collect())
            .unwrap_or_default()
    };
    assert_eq!(required(&schema_of("ask_agent")), ["provider", "prompt"]);
    assert_eq!(required(&schema_of("debate")), ["question"]);
    assert_eq!(required(&schema_of("consult")), ["question"]);
    for optional in [
        "mode",
        "coordinator",
        "length",
        "rounds",
        "agents",
        "timeout_seconds",
        "wait_seconds",
    ] {
        assert!(
            schema_of("consult")["properties"][optional].is_object(),
            "consult.{optional}"
        );
    }
    let consult_rounds = &schema_of("consult")["properties"]["rounds"];
    assert!(
        consult_rounds.to_string().contains("\"maximum\":2")
            && consult_rounds.to_string().contains("\"minimum\":1"),
        "{consult_rounds}"
    );
    assert_eq!(required(&schema_of("get_task_status")), ["task_id"]);
    assert_eq!(required(&schema_of("cancel_task")), ["task_id"]);
    assert_eq!(required(&schema_of("get_debate")), ["debate_id"]);
    assert!(required(&schema_of("list_agents")).is_empty());
    for optional in ["timeout_seconds", "wait_seconds"] {
        assert!(
            schema_of("ask_agent")["properties"][optional].is_object(),
            "{optional}"
        );
    }
    // The round limit is part of the published schema.
    let rounds = &schema_of("debate")["properties"]["rounds"];
    assert!(
        rounds.to_string().contains("\"maximum\":2")
            && rounds.to_string().contains("\"minimum\":1"),
        "{rounds}"
    );
    // Read-only tools are marked so hosts can auto-approve them.
    let ann = |name: &str| {
        tools
            .iter()
            .find(|t| t.name == name)
            .unwrap()
            .annotations
            .clone()
            .unwrap()
    };
    assert_eq!(ann("list_agents").read_only_hint, Some(true));
    assert_eq!(ann("get_debate").read_only_hint, Some(true));
    assert_eq!(ann("ask_agent").open_world_hint, Some(true));
    h.stop().await;
}

#[tokio::test]
async fn list_agents_reports_availability_without_asking_any_agent() {
    let off = Arc::new(MockProvider::new("off").disabled());
    let h = start(
        Setup {
            host: Some("lead"),
            ..Default::default()
        },
        vec![mock("lead"), mock("beta"), off.clone()],
    )
    .await;
    let r = h.call("list_agents", json!({})).await;
    assert!(!is_error(&r));
    let v = data(&r);
    assert_eq!(v["host"], "lead");
    assert_eq!(v["max_depth"], 1);
    let agent = |id: &str| {
        v["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["id"] == id)
            .unwrap()
            .clone()
    };
    assert_eq!(agent("lead")["callable"], false);
    assert_eq!(agent("lead")["is_host"], true);
    assert!(
        agent("lead")["unavailable_reason"]
            .as_str()
            .unwrap()
            .contains("hosts this MCP server")
    );
    assert_eq!(agent("beta")["callable"], true);
    assert_eq!(agent("beta")["capabilities"]["read_only"], true);
    assert_eq!(agent("beta")["capabilities"]["writes"], false);
    assert_eq!(agent("off")["callable"], false);
    assert_eq!(agent("off")["unavailable_reason"], "disabled in config");
    for m in &h.mocks {
        assert!(
            m.requests().is_empty(),
            "listing agents must never send a prompt"
        );
    }
    h.stop().await;
}

// ---- ask_agent -------------------------------------------------------------------

#[tokio::test]
async fn ask_agent_returns_a_structured_answer() {
    let beta = Arc::new(MockProvider::new("beta").with_reply(text("pong")));
    let h = start(Setup::default(), vec![mock("lead"), beta.clone()]).await;
    let r = h
        .call("ask_agent", json!({"provider": "beta", "prompt": "ping"}))
        .await;
    assert!(!is_error(&r), "{r:?}");
    let v = data(&r);
    assert_eq!(v["status"], "completed");
    assert_eq!(v["ok"], true);
    assert_eq!(v["provider"], "beta");
    assert_eq!(v["text"], "pong");
    assert_eq!(v["truncated"], false);
    assert!(v["duration_ms"].is_u64() && v["task_id"].as_str().unwrap().starts_with("task-"));
    assert_eq!(v["kind"], "ask_agent");
    assert!(v["error"].is_null());
    // The same structure is also available as text for clients without structured content.
    let as_text = r.content[0].as_text().map(|t| t.text.clone()).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&as_text).unwrap()["text"],
        "pong"
    );
    // Read-only, as always.
    let seen = beta.requests();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].access, ai_orchestra::AccessMode::ReadOnly);
    assert!(seen[0].isolate, "children are started in isolated mode");
    assert_eq!(seen[0].prompt, "ping");
    h.stop().await;
}

#[tokio::test]
async fn provider_failures_arrive_as_structured_errors_with_their_kind() {
    let beta = Arc::new(
        MockProvider::new("beta")
            .with_reply(MockReply::Quota("You've hit your usage limit".into()))
            .with_reply(MockReply::Auth("not logged in".into()))
            .with_reply(MockReply::Malformed("garbage".into()))
            .with_reply(MockReply::Failure("boom".into())),
    );
    let h = start(Setup::default(), vec![mock("lead"), beta]).await;
    for (kind, needle) in [
        ("quota", "usage limit"),
        ("auth", "not logged in"),
        ("malformed", "garbage"),
        ("error", "boom"),
    ] {
        let r = h
            .call("ask_agent", json!({"provider": "beta", "prompt": "x"}))
            .await;
        assert!(is_error(&r), "{kind}");
        let v = data(&r);
        assert_eq!(v["status"], "failed", "{kind}");
        assert_eq!(v["ok"], false);
        assert_eq!(error_kind(&v), kind);
        assert!(
            v["error"]["message"].as_str().unwrap().contains(needle),
            "{kind}: {v}"
        );
        assert!(v["text"].is_null(), "no text on failure");
    }
    h.stop().await;
}

#[tokio::test]
async fn timeouts_are_reported_as_timed_out_not_failed() {
    // The provider itself times out.
    let slow = Arc::new(MockProvider::new("beta").with_reply(MockReply::Timeout));
    let h = start(Setup::default(), vec![mock("lead"), slow]).await;
    let v = data(
        &h.call("ask_agent", json!({"provider": "beta", "prompt": "x"}))
            .await,
    );
    assert_eq!(v["status"], "timed_out");
    assert_eq!(error_kind(&v), "timeout");
    h.stop().await;

    // The task exceeds the server's hard task limit and is stopped.
    let gate = Arc::new(Barrier::new(9)); // never opens
    let stuck = Arc::new(MockProvider::new("beta").with_gate(gate));
    let h = start(
        Setup {
            max_task_secs: Some(1),
            ..Default::default()
        },
        vec![mock("lead"), stuck],
    )
    .await;
    let r = tokio::time::timeout(
        Duration::from_secs(30),
        h.call(
            "ask_agent",
            json!({"provider": "beta", "prompt": "x", "wait_seconds": 30}),
        ),
    )
    .await
    .expect("the task limit must end the call");
    let v = data(&r);
    assert_eq!(v["status"], "timed_out", "{v}");
    assert_eq!(error_kind(&v), "timeout");
    assert!(is_error(&r));
    h.stop().await;
}

// ---- input validation ---------------------------------------------------------------

#[tokio::test]
async fn malformed_input_is_rejected_by_the_protocol_or_as_a_structured_error() {
    let h = start(Setup::default(), vec![mock("lead"), mock("beta")]).await;
    // Wrong shapes never reach the handler. rmcp reports them either as a protocol error or as
    // a tool result with isError and a readable message, which lets a model correct itself.
    let rejected_before_the_handler = |result: Result<CallToolResult, ServiceError>| match result {
        Err(ServiceError::McpError(_)) => true,
        Ok(r) => {
            is_error(&r)
                && r.content.iter().filter_map(|c| c.as_text()).any(|t| {
                    t.text.contains("deserialize")
                        || t.text.contains("missing field")
                        || t.text.contains("unknown field")
                })
        }
        Err(_) => false,
    };
    for args in [
        json!({"provider": 5, "prompt": "x"}),
        json!({"provider": "beta"}),
        json!({"prompt": "x"}),
        json!({"provider": "beta", "prompt": "x", "timeout_seconds": "ten"}),
        json!({"provider": "beta", "prompt": "x", "timeout_seconds": -1}),
        json!({"provider": "beta", "prompt": "x", "surprise": true}),
        json!({}),
    ] {
        assert!(
            rejected_before_the_handler(h.try_call("ask_agent", args.clone()).await),
            "{args} must be rejected"
        );
    }
    assert!(rejected_before_the_handler(
        h.try_call("debate", json!({"question": "q", "rounds": "two"}))
            .await
    ));
    assert!(rejected_before_the_handler(
        h.try_call("get_task_status", json!({})).await
    ));
    let unknown = h.try_call("no_such_tool", json!({})).await;
    assert!(
        unknown.is_err() || unknown.is_ok_and(|r| is_error(&r)),
        "an unknown tool is an error"
    );

    // Well-formed but unacceptable: a structured error the model can read.
    for (args, what) in [
        (json!({"provider": "beta", "prompt": ""}), "empty prompt"),
        (json!({"provider": "beta", "prompt": "   "}), "blank prompt"),
        (
            json!({"provider": "nobody", "prompt": "x"}),
            "unknown provider",
        ),
        (
            json!({"provider": "beta", "prompt": "x", "timeout_seconds": 0}),
            "zero timeout",
        ),
        (
            json!({"provider": "beta", "prompt": "x", "timeout_seconds": 100000}),
            "huge timeout",
        ),
        (
            json!({"provider": "beta", "prompt": "x", "wait_seconds": 600}),
            "huge wait",
        ),
    ] {
        let r = h.call("ask_agent", args).await;
        assert!(is_error(&r), "{what}");
        let v = data(&r);
        assert_eq!(v["status"], "rejected", "{what}");
        assert_eq!(error_kind(&v), "invalid_request", "{what}: {v}");
    }
    for (args, what) in [
        (json!({"question": " "}), "blank question"),
        (json!({"question": "q", "rounds": 0}), "zero rounds"),
        (json!({"question": "q", "rounds": 3}), "three rounds"),
        (
            json!({"question": "q", "coordinator": "nobody"}),
            "unknown coordinator",
        ),
    ] {
        let r = h.call("debate", args).await;
        assert!(
            is_error(&r) && error_kind(&data(&r)) == "invalid_request",
            "{what}"
        );
    }
    for m in &h.mocks {
        assert!(
            m.requests().is_empty(),
            "rejected requests never reach an agent"
        );
    }
    h.stop().await;
}

#[tokio::test]
async fn oversized_prompts_and_results_are_bounded() {
    let big = "x".repeat(300_000);
    let beta = Arc::new(MockProvider::new("beta").with_reply(text(&big)));
    let h = start(
        Setup {
            max_result_bytes: Some(4096),
            ..Default::default()
        },
        vec![mock("lead"), beta.clone()],
    )
    .await;
    let too_long = "p".repeat(2 * 1024 * 1024);
    let r = h
        .call("ask_agent", json!({"provider": "beta", "prompt": too_long}))
        .await;
    assert!(is_error(&r) && error_kind(&data(&r)) == "invalid_request");
    assert!(beta.requests().is_empty());

    let v = data(
        &h.call("ask_agent", json!({"provider": "beta", "prompt": "go"}))
            .await,
    );
    assert_eq!(v["truncated"], true);
    let returned = v["text"].as_str().unwrap();
    assert!(returned.len() <= 4096 + 40, "{} bytes", returned.len());
    assert!(returned.ends_with("[... truncated ...]"));
    h.stop().await;
}

// ---- tasks: status, waiting, cancellation, concurrency --------------------------------

#[tokio::test]
async fn long_calls_become_tasks_that_can_be_followed_to_completion() {
    let gate = Arc::new(Barrier::new(2));
    let beta = Arc::new(
        MockProvider::new("beta")
            .with_reply(text("late answer"))
            .with_gate(gate.clone()),
    );
    let h = start(Setup::default(), vec![mock("lead"), beta]).await;

    // wait_seconds = 0: answer immediately with a task id while the agent is still working.
    let r = h
        .call(
            "ask_agent",
            json!({"provider": "beta", "prompt": "x", "wait_seconds": 0}),
        )
        .await;
    assert!(!is_error(&r), "a running task is not an error");
    let v = data(&r);
    let id = v["task_id"].as_str().unwrap().to_string();
    assert!(
        matches!(v["status"].as_str().unwrap(), "pending" | "running"),
        "{v}"
    );
    assert!(v["ok"].is_null());
    assert!(v["next_step"].as_str().unwrap().contains("get_task_status"));

    let st = data(&h.call("get_task_status", json!({"task_id": id})).await);
    assert!(
        matches!(st["status"].as_str().unwrap(), "pending" | "running"),
        "{st}"
    );
    assert!(st["finished_at_ms"].is_null());

    gate.wait().await; // the agent finishes
    // One long wait (no polling loop) returns as soon as the task is done.
    let done = data(
        &h.call(
            "get_task_status",
            json!({"task_id": id, "wait_seconds": 30}),
        )
        .await,
    );
    assert_eq!(done["status"], "completed", "{done}");
    assert_eq!(done["ok"], true);
    assert_eq!(done["text"], "late answer");
    assert!(done["finished_at_ms"].is_u64() && done["started_at_ms"].is_u64());
    // Asking about a task that does not exist is an error, not a hang.
    let missing = h
        .call("get_task_status", json!({"task_id": "task-nope"}))
        .await;
    assert!(is_error(&missing) && error_kind(&data(&missing)) == "not_found");
    h.stop().await;
}

#[tokio::test]
async fn cancel_task_stops_a_running_agent_and_leaves_finished_tasks_alone() {
    let gate = Arc::new(Barrier::new(9)); // never opens
    let beta = Arc::new(MockProvider::new("beta").with_gate(gate));
    let h = start(Setup::default(), vec![mock("lead"), beta]).await;
    let v = data(
        &h.call(
            "ask_agent",
            json!({"provider": "beta", "prompt": "x", "wait_seconds": 0}),
        )
        .await,
    );
    let id = v["task_id"].as_str().unwrap().to_string();

    let cancelled = data(&h.call("cancel_task", json!({"task_id": id})).await);
    assert_eq!(cancelled["status"], "cancelled", "{cancelled}");
    assert_eq!(cancelled["ok"], false);
    assert_eq!(error_kind(&cancelled), "cancelled");
    let again = data(&h.call("get_task_status", json!({"task_id": id})).await);
    assert_eq!(again["status"], "cancelled");
    // Cancelling again, or a task that already finished, changes nothing.
    assert_eq!(
        data(&h.call("cancel_task", json!({"task_id": id})).await)["status"],
        "cancelled"
    );
    let nope = h.call("cancel_task", json!({"task_id": "task-nope"})).await;
    assert!(is_error(&nope) && error_kind(&data(&nope)) == "not_found");

    let ok_agent = Arc::new(MockProvider::new("gamma").with_reply(text("fine")));
    drop(h.stop());
    let h2 = start(Setup::default(), vec![mock("lead"), ok_agent]).await;
    let done = data(
        &h2.call("ask_agent", json!({"provider": "gamma", "prompt": "x"}))
            .await,
    );
    let id2 = done["task_id"].as_str().unwrap().to_string();
    assert_eq!(
        data(&h2.call("cancel_task", json!({"task_id": id2})).await)["status"],
        "completed"
    );
    h2.stop().await;
}

#[tokio::test]
async fn concurrent_requests_respect_the_task_limits() {
    let gate = Arc::new(Barrier::new(2));
    let beta = Arc::new(MockProvider::new("beta").with_gate(gate.clone()));
    let h = start(
        Setup {
            max_concurrent: Some(2),
            max_queued: Some(0),
            ..Default::default()
        },
        vec![mock("lead"), beta.clone()],
    )
    .await;
    let peer = h.client.peer().clone();
    let call = |peer: rmcp::Peer<RoleClient>| async move {
        let args = json!({"provider": "beta", "prompt": "x", "wait_seconds": 0});
        peer.call_tool(
            CallToolRequestParams::new("ask_agent")
                .with_arguments(args.as_object().cloned().unwrap()),
        )
        .await
        .unwrap()
    };
    // Three requests at once: two get slots, the third finds no room in the queue.
    let (a, b, c) = tokio::join!(call(peer.clone()), call(peer.clone()), call(peer.clone()));
    let results = [a, b, c];
    let busy = results
        .iter()
        .filter(|r| is_error(r) && error_kind(&data(r)) == "busy")
        .count();
    let accepted: Vec<String> = results
        .iter()
        .filter(|r| !is_error(r))
        .map(|r| data(r)["task_id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!((busy, accepted.len()), (1, 2), "{results:?}");

    // The two accepted tasks are inside the agent at the same time: its barrier only opens
    // when both are there, so sequential execution would never finish (hence the time guard).
    for id in &accepted {
        let call = h.call(
            "get_task_status",
            json!({"task_id": id, "wait_seconds": 30}),
        );
        let done = data(
            &tokio::time::timeout(Duration::from_secs(45), call)
                .await
                .expect("the tasks did not run concurrently"),
        );
        assert_eq!(done["status"], "completed", "{done}");
    }
    assert_eq!(beta.requests().len(), 2);
    h.stop().await;
}

#[tokio::test]
async fn many_parallel_requests_all_complete_independently() {
    let beta = Arc::new(MockProvider::new("beta"));
    let h = start(Setup::default(), vec![mock("lead"), beta.clone()]).await;
    let peer = h.client.peer().clone();
    let mut calls = tokio::task::JoinSet::new();
    for i in 0..4 {
        let peer = peer.clone();
        calls.spawn(async move {
            let args = json!({"provider": "beta", "prompt": format!("request {i}")});
            let r = peer
                .call_tool(
                    CallToolRequestParams::new("ask_agent")
                        .with_arguments(args.as_object().cloned().unwrap()),
                )
                .await
                .unwrap();
            (i, data(&r))
        });
    }
    let mut seen = Vec::new();
    while let Some(joined) = calls.join_next().await {
        let (i, v) = joined.unwrap();
        assert_eq!(v["status"], "completed", "{v}");
        assert_eq!(
            v["text"],
            format!("mock reply to: request {i}"),
            "each caller gets its own answer"
        );
        seen.push(v["task_id"].as_str().unwrap().to_string());
    }
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 4, "distinct task ids");
    h.stop().await;
}

// ---- debate and retrieval ---------------------------------------------------------------

fn debate_mocks() -> (Arc<MockProvider>, Arc<MockProvider>, Arc<MockProvider>) {
    (
        Arc::new(
            MockProvider::new("lead")
                .with_reply(text("PROPOSAL"))
                .with_reply(text(SYNTHESIS)),
        ),
        Arc::new(MockProvider::new("beta").with_reply(text("beta review"))),
        Arc::new(MockProvider::new("gamma").with_reply(text("gamma review"))),
    )
}

#[tokio::test]
async fn debate_runs_the_existing_engine_and_returns_the_synthesis() {
    let (lead, beta, gamma) = debate_mocks();
    let h = start(
        Setup::default(),
        vec![lead.clone(), beta.clone(), gamma.clone()],
    )
    .await;
    let r = h
        .call("debate", json!({"question": "Which design?", "rounds": 1}))
        .await;
    assert!(!is_error(&r), "{r:?}");
    let v = data(&r);
    assert_eq!(v["status"], "completed");
    assert_eq!(v["debate_status"], "complete");
    assert_eq!(v["ok"], true);
    assert_eq!(v["coordinator"], "lead");
    assert_eq!(v["participants"], json!(["beta", "gamma"]));
    assert_eq!(v["rounds"], 1);
    assert_eq!(v["calls"], 4);
    assert_eq!(v["final_synthesis"], SYNTHESIS);
    assert!(
        v["disagreements"]
            .as_str()
            .unwrap()
            .contains("beta objected to Y"),
        "{v}"
    );
    assert!(
        !v["disagreements"]
            .as_str()
            .unwrap()
            .contains("Unresolved risks")
    );
    assert!(v["debate_id"].as_str().unwrap().len() > 15);
    assert!(v["note"].is_null());
    // The engine ran as in the CLI: the coordinator was asked twice, reviewers once.
    assert_eq!(
        (
            lead.requests().len(),
            beta.requests().len(),
            gamma.requests().len()
        ),
        (2, 1, 1)
    );
    assert!(
        lead.requests()
            .iter()
            .chain(&beta.requests())
            .all(|r| r.access == ai_orchestra::AccessMode::ReadOnly && r.isolate)
    );
    h.stop().await;
}

#[tokio::test]
async fn a_partial_debate_is_flagged_as_incomplete() {
    let lead = Arc::new(
        MockProvider::new("lead")
            .with_reply(text("P"))
            .with_reply(text(SYNTHESIS)),
    );
    let bad = Arc::new(MockProvider::new("beta").with_reply(MockReply::Quota("limit".into())));
    let h = start(Setup::default(), vec![lead, bad, mock("gamma")]).await;
    let r = h
        .call("debate", json!({"question": "q", "rounds": 2}))
        .await;
    assert!(
        is_error(&r),
        "a partial result must not look like a success"
    );
    let v = data(&r);
    assert_eq!(v["debate_status"], "partial");
    assert_eq!(v["ok"], false);
    assert_eq!(v["status"], "completed", "the task itself finished");
    assert!(v["note"].as_str().unwrap().contains("PARTIAL"));
    assert!(
        v["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().contains("beta failed"))
    );
    assert_eq!(
        v["final_synthesis"], SYNTHESIS,
        "the useful result is still returned"
    );
    h.stop().await;
}

#[tokio::test]
async fn debates_can_be_retrieved_by_id_or_prefix_afterwards() {
    let (lead, beta, gamma) = debate_mocks();
    let h = start(Setup::default(), vec![lead, beta, gamma]).await;
    let v = data(
        &h.call("debate", json!({"question": "Retrieve me", "rounds": 1}))
            .await,
    );
    let id = v["debate_id"].as_str().unwrap().to_string();

    let got = data(&h.call("get_debate", json!({"debate_id": id})).await);
    assert_eq!(got["debate_id"], id);
    assert_eq!(got["question"], "Retrieve me");
    assert_eq!(got["final_synthesis"], SYNTHESIS);
    assert_eq!(got["steps"].as_array().unwrap().len(), 4);
    assert!(got["transcript"].is_null(), "no transcript unless asked");
    assert!(got["created_at_ms"].as_u64().unwrap() > 0);

    let by_prefix = data(
        &h.call(
            "get_debate",
            json!({"debate_id": &id[..id.len() - 2], "include_transcript": true}),
        )
        .await,
    );
    assert_eq!(by_prefix["debate_id"], id);
    let transcript = by_prefix["transcript"].as_array().unwrap();
    assert_eq!(transcript.len(), 4);
    assert!(
        transcript
            .iter()
            .any(|t| t["agent"] == "beta" && t["text"] == "beta review")
    );
    // The same record is what `ai-orchestra show` reads.
    assert_eq!(h.store.load(&id).unwrap().final_answer, SYNTHESIS);

    for bad in ["20990101-000000-ffffff", "../etc/passwd", ""] {
        let r = h.call("get_debate", json!({"debate_id": bad})).await;
        assert!(
            is_error(&r) && error_kind(&data(&r)) == "not_found",
            "{bad}"
        );
    }
    h.stop().await;

    // Without saved history there is nothing to retrieve, and the tool says so.
    let (lead, beta, _) = debate_mocks();
    let h = start(
        Setup {
            no_history: true,
            ..Default::default()
        },
        vec![lead, beta],
    )
    .await;
    let r = h.call("get_debate", json!({"debate_id": "20260101"})).await;
    assert_eq!(error_kind(&data(&r)), "history_disabled");
    h.stop().await;
}

#[tokio::test]
async fn debates_run_as_cancellable_tasks() {
    let gate = Arc::new(Barrier::new(9));
    let stuck = Arc::new(MockProvider::new("beta").with_gate(gate));
    let h = start(Setup::default(), vec![mock("lead"), stuck]).await;
    let v = data(
        &h.call("debate", json!({"question": "q", "wait_seconds": 0}))
            .await,
    );
    assert_eq!(v["kind"], "debate");
    let id = v["task_id"].as_str().unwrap().to_string();
    let cancelled = data(&h.call("cancel_task", json!({"task_id": id})).await);
    assert_eq!(cancelled["status"], "cancelled", "{cancelled}");
    h.stop().await;
}

// ---- recursion prevention -------------------------------------------------------------------

#[tokio::test]
async fn the_host_agent_is_never_called_through_the_server() {
    let (lead, beta, gamma) = debate_mocks();
    let h = start(
        Setup {
            host: Some("lead"),
            ..Default::default()
        },
        vec![lead.clone(), beta.clone(), gamma.clone()],
    )
    .await;

    // ask_agent refuses the host and explains what to do instead.
    let r = h
        .call("ask_agent", json!({"provider": "lead", "prompt": "hello"}))
        .await;
    assert!(is_error(&r));
    let v = data(&r);
    assert_eq!(error_kind(&v), "host_agent_blocked");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("second copy"),
        "{v}"
    );
    assert!(lead.requests().is_empty());

    // A debate cannot be coordinated by the host...
    let r = h
        .call("debate", json!({"question": "q", "coordinator": "lead"}))
        .await;
    assert_eq!(error_kind(&data(&r)), "host_agent_blocked");
    assert!(
        data(&r)["error"]["message"]
            .as_str()
            .unwrap()
            .contains("ask_agent"),
        "tells the host how to coordinate itself"
    );

    // ...so the default coordinator is another agent, and the host is not a reviewer either.
    let beta = Arc::new(
        MockProvider::new("beta")
            .with_reply(text("P"))
            .with_reply(text(SYNTHESIS)),
    );
    let gamma2 = Arc::new(MockProvider::new("gamma").with_reply(text("gamma review")));
    h.stop().await;
    let h = start(
        Setup {
            host: Some("lead"),
            ..Default::default()
        },
        vec![lead.clone(), beta.clone(), gamma2.clone()],
    )
    .await;
    let v = data(
        &h.call("debate", json!({"question": "q", "rounds": 1}))
            .await,
    );
    assert_eq!(
        v["coordinator"], "beta",
        "the configured coordinator is the host, so another agent leads: {v}"
    );
    assert_eq!(v["participants"], json!(["gamma"]));
    assert!(
        v["skipped"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["provider"] == "lead"),
        "{v}"
    );
    assert!(lead.requests().is_empty(), "the host was never called");
    h.stop().await;
}

#[tokio::test]
async fn the_host_can_be_allowed_explicitly() {
    let lead = Arc::new(MockProvider::new("lead").with_reply(text("hi from host")));
    let h = start(
        Setup {
            host: Some("lead"),
            allow_host: true,
            ..Default::default()
        },
        vec![lead.clone(), mock("beta")],
    )
    .await;
    let v = data(
        &h.call("ask_agent", json!({"provider": "lead", "prompt": "x"}))
            .await,
    );
    assert_eq!(v["status"], "completed");
    assert_eq!(lead.requests().len(), 1);
    h.stop().await;
}

#[tokio::test]
async fn excessive_delegation_depth_is_refused() {
    let beta = mock("beta");
    // This server runs inside an agent that ai-orchestra itself started (depth 1 = max_depth).
    let h = start(
        Setup {
            depth: Some(1),
            ..Default::default()
        },
        vec![mock("lead"), beta.clone()],
    )
    .await;
    let r = h
        .call("ask_agent", json!({"provider": "beta", "prompt": "x"}))
        .await;
    assert!(is_error(&r));
    let v = data(&r);
    assert_eq!(error_kind(&v), "nested", "{v}");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains(ORCHESTRA_DEPTH_ENV)
    );
    assert_eq!(
        error_kind(&data(&h.call("debate", json!({"question": "q"})).await)),
        "nested"
    );
    assert!(beta.requests().is_empty());
    h.stop().await;
}

// ---- lifecycle ------------------------------------------------------------------------------------

#[tokio::test]
async fn shutdown_stops_running_tasks_and_the_server_returns() {
    let gate = Arc::new(Barrier::new(9));
    let stuck = Arc::new(MockProvider::new("beta").with_gate(gate));
    let h = start(Setup::default(), vec![mock("lead"), stuck]).await;
    let v = data(
        &h.call(
            "ask_agent",
            json!({"provider": "beta", "prompt": "x", "wait_seconds": 0}),
        )
        .await,
    );
    assert!(v["task_id"].is_string());
    // The client goes away mid-task: the server must still shut down by itself.
    drop(h.client);
    let ended = tokio::time::timeout(Duration::from_secs(30), h.server).await;
    assert!(
        ended.is_ok(),
        "the server did not stop after the client disconnected"
    );
}

// ---- consult ---------------------------------------------------------------------------------

const ANSWER: &str = "## Answer\nUse X.\n\n## Recommendation\nBuy X.\n\n## Areas of agreement\nCost matters.\n\n## Significant disagreements\n- beta wants Y; unresolved.\n\n## Assumptions\nBudget is fixed.\n\n## Evidence limitations\nPrices were not verified.";

fn panel(names: &[&str], lead_script: &[&str], others: &[&str]) -> Vec<Arc<MockProvider>> {
    names
        .iter()
        .map(|n| {
            let mut m = MockProvider::new(n);
            let script = if *n == names[0] { lead_script } else { others };
            for r in script {
                m = m.with_reply(text(r));
            }
            Arc::new(m)
        })
        .collect()
}

#[tokio::test]
async fn consult_chooses_everything_and_reuses_the_consultation_engine() {
    let mocks = panel(
        &["lead", "beta", "gamma"],
        &["lead answer", "lead review", ANSWER],
        &["answer", "review"],
    );
    let h = start(Setup::default(), mocks.clone()).await;
    let r = h
        .call(
            "consult",
            json!({"question": "Which design should I pick?"}),
        )
        .await;
    assert!(!is_error(&r), "{r:?}");
    let v = data(&r);
    assert_eq!(v["status"], "completed", "{v}");
    assert_eq!(v["ok"], true);
    assert_eq!(v["consultation_status"], "complete");
    assert_eq!(v["mode"], "decision");
    assert_eq!(v["coordinator"], "lead");
    assert_eq!(v["participants"], json!(["lead", "beta", "gamma"]));
    assert_eq!(v["rounds_run"], 1);
    assert_eq!(v["calls"], 7);
    assert_eq!(v["answer"], ANSWER);
    assert_eq!(v["recommendation"], "Buy X.");
    assert_eq!(v["agreements"], "Cost matters.");
    assert!(
        v["disagreements"]
            .as_str()
            .unwrap()
            .contains("beta wants Y")
    );
    assert_eq!(v["unresolved_disagreement"], true);
    assert_eq!(v["assumptions"], "Budget is fixed.");
    assert_eq!(v["evidence_limitations"], "Prices were not verified.");
    assert_eq!(v["invocations"]["lead"], 3);
    assert_eq!(v["routing"]["mode"]["source"], "auto");
    assert_eq!(v["manual_overrides"], json!([]));
    assert!(v["host_note"].is_null() && v["note"].is_null());
    assert!(v["caveat"].as_str().unwrap().contains("not proof"));
    assert!(v["consultation_id"].as_str().unwrap().len() > 15);
    for m in &mocks {
        for req in m.requests() {
            assert_eq!(req.access, ai_orchestra::AccessMode::ReadOnly);
            assert!(req.isolate, "children are isolated through MCP");
        }
    }
    h.stop().await;
}

#[tokio::test]
async fn consult_manual_parameters_override_the_automatic_choices() {
    let mocks = panel(&["lead", "beta", "gamma"], &[], &[]);
    // beta coordinates; lead is left out entirely.
    let h = start(Setup::default(), mocks.clone()).await;
    let v = data(
        &h.call(
            "consult",
            json!({"question": "Explain what a resistor does", "mode": "learn", "coordinator": "beta",
                   "agents": ["beta", "gamma"], "rounds": 2}),
        )
        .await,
    );
    assert_eq!(v["status"], "completed", "{v}");
    assert_eq!(v["mode"], "learn");
    assert_eq!(v["coordinator"], "beta");
    assert_eq!(v["participants"], json!(["beta", "gamma"]));
    assert_eq!(v["rounds_run"], 2);
    assert_eq!(v["calls"], 2 + 4 + 1);
    assert_eq!(
        v["manual_overrides"],
        json!(["mode", "coordinator", "agents", "rounds"])
    );
    assert!(mocks[0].requests().is_empty(), "lead was not selected");
    h.stop().await;
}

#[tokio::test]
async fn consult_accepts_auto_values_and_rejects_bad_ones_without_calling_agents() {
    let mocks = panel(&["lead", "beta"], &[], &[]);
    let h = start(Setup::default(), mocks.clone()).await;
    let ok = data(
        &h.call(
            "consult",
            json!({"question": "Compare A and B", "mode": "auto", "coordinator": "auto", "agents": ["auto"]}),
        )
        .await,
    );
    assert_eq!(ok["status"], "completed", "{ok}");
    for bad in [
        json!({"question": ""}),
        json!({"question": "q", "mode": "poetry"}),
        json!({"question": "q", "rounds": 3}),
        json!({"question": "q", "rounds": 0}),
        json!({"question": "q", "agents": ["nobody"]}),
        json!({"question": "q", "coordinator": "nobody"}),
        json!({"question": "q", "timeout_seconds": 0}),
        json!({"question": "q", "wait_seconds": 56}),
        json!({"question": "q", "mode": "debate", "agents": ["lead"]}),
        json!({"question": "q", "coordinator": "beta", "agents": ["lead"]}),
        json!({"question": "q", "unknown_field": true}),
    ] {
        // A protocol-level rejection (Err) is fine too; only a successful call must be an error.
        if let Ok(r) = h.try_call("consult", bad.clone()).await {
            assert!(is_error(&r), "{bad} should be rejected: {r:?}");
        }
    }
    let calls_before: usize = mocks.iter().map(|m| m.requests().len()).sum();
    assert_eq!(
        calls_before,
        2 + 2 + 1,
        "only the one valid consultation ran"
    );
    h.stop().await;
}

#[tokio::test]
async fn consult_never_calls_the_host_and_never_lets_it_coordinate() {
    let mocks = panel(&["lead", "beta", "gamma"], &[], &[]);
    let h = start(
        Setup {
            host: Some("lead"),
            ..Default::default()
        },
        mocks.clone(),
    )
    .await;
    // Automatic: the configured coordinator is the host, so another agent coordinates.
    let v = data(
        &h.call("consult", json!({"question": "Compare A and B"}))
            .await,
    );
    assert_eq!(v["status"], "completed", "{v}");
    assert_eq!(v["coordinator"], "beta");
    assert_eq!(v["participants"], json!(["beta", "gamma"]));
    assert!(
        v["skipped"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["provider"] == "lead"
                && s["reason"]
                    .as_str()
                    .unwrap()
                    .contains("hosts this MCP server")),
        "{v}"
    );
    assert!(
        v["host_note"]
            .as_str()
            .unwrap()
            .contains("lead hosts this server"),
        "the host is told how to add its own view: {v}"
    );
    assert!(mocks[0].requests().is_empty(), "the host was never called");

    // Explicitly naming the host as coordinator is refused with an explanation.
    let r = h
        .call("consult", json!({"question": "q", "coordinator": "lead"}))
        .await;
    assert!(is_error(&r));
    let v = data(&r);
    assert_eq!(error_kind(&v), "host_agent_blocked");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("second copy")
    );

    // Naming the host among the agents reports it as not used instead of calling it.
    let v = data(
        &h.call(
            "consult",
            json!({"question": "Compare A and B", "agents": ["lead", "beta", "gamma"]}),
        )
        .await,
    );
    assert_eq!(v["participants"], json!(["beta", "gamma"]));
    assert!(mocks[0].requests().is_empty());
    h.stop().await;
}

#[tokio::test]
async fn consult_with_only_the_host_available_is_a_clear_rejection() {
    let mocks = panel(&["lead", "beta"], &[], &[]);
    let h = start(
        Setup {
            host: Some("lead"),
            ..Default::default()
        },
        vec![
            mocks[0].clone(),
            Arc::new(MockProvider::new("beta").disabled()),
        ],
    )
    .await;
    let r = h
        .call("consult", json!({"question": "Compare A and B"}))
        .await;
    assert!(is_error(&r));
    let v = data(&r);
    assert_eq!(error_kind(&v), "invalid_request");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no agent is available"),
        "{v}"
    );
    assert!(mocks[0].requests().is_empty());
    h.stop().await;
}

#[tokio::test]
async fn consult_is_refused_when_delegation_is_too_deep() {
    let mocks = panel(&["lead", "beta"], &[], &[]);
    let h = start(
        Setup {
            depth: Some(1),
            ..Default::default()
        },
        mocks.clone(),
    )
    .await;
    let r = h
        .call("consult", json!({"question": "Compare A and B"}))
        .await;
    assert!(is_error(&r));
    assert_eq!(error_kind(&data(&r)), "nested");
    assert!(mocks.iter().all(|m| m.requests().is_empty()));
    h.stop().await;
}

#[tokio::test]
async fn a_partial_consultation_is_flagged_as_incomplete() {
    let lead = Arc::new(
        MockProvider::new("lead")
            .with_reply(text("a"))
            .with_reply(text("r"))
            .with_reply(text(ANSWER)),
    );
    let bad = Arc::new(MockProvider::new("beta").with_reply(MockReply::Quota("limit".into())));
    let h = start(Setup::default(), vec![lead, bad]).await;
    let r = h
        .call("consult", json!({"question": "Compare A and B"}))
        .await;
    assert!(
        is_error(&r),
        "a partial result must not look like a success"
    );
    let v = data(&r);
    assert_eq!(v["consultation_status"], "partial");
    assert_eq!(v["ok"], false);
    assert_eq!(v["status"], "completed", "the task itself finished");
    assert_eq!(v["provider_failures"], 1);
    assert!(v["note"].as_str().unwrap().contains("PARTIAL"));
    assert!(
        v["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().contains("beta failed")),
        "{v}"
    );
    h.stop().await;
}

#[tokio::test]
async fn long_consultations_become_tasks_that_can_be_followed_and_retrieved() {
    // Each call takes a moment, and wait_seconds 0 returns at once: the result is a task id.
    let mocks: Vec<_> = ["lead", "beta"]
        .iter()
        .map(|n| Arc::new(MockProvider::new(n).with_delay(Duration::from_millis(150))))
        .collect();
    let h = start(Setup::default(), mocks).await;
    let first = data(
        &h.call(
            "consult",
            json!({"question": "Compare A and B", "wait_seconds": 0}),
        )
        .await,
    );
    assert!(
        matches!(first["status"].as_str(), Some("pending" | "running")),
        "{first}"
    );
    assert_eq!(first["ok"], Value::Null);
    assert_eq!(first["kind"], "consult");
    let task_id = first["task_id"].as_str().unwrap().to_string();

    let done = data(
        &h.call(
            "get_task_status",
            json!({"task_id": task_id, "wait_seconds": 30}),
        )
        .await,
    );
    assert_eq!(done["status"], "completed", "{done}");
    assert_eq!(done["consultation_status"], "complete");
    let id = done["consultation_id"].as_str().unwrap().to_string();

    // The saved record is retrievable with the existing get_debate tool.
    let got = data(&h.call("get_debate", json!({"debate_id": id})).await);
    assert_eq!(got["kind"], "consultation");
    assert_eq!(got["mode"], "decision");
    assert!(h.store.load(&id).unwrap().consultation.is_some());
    h.stop().await;
}

#[tokio::test]
async fn a_consultation_task_can_be_cancelled_and_stops_its_agents() {
    let gate = Arc::new(Barrier::new(9));
    let stuck = Arc::new(MockProvider::new("beta").with_gate(gate));
    let h = start(Setup::default(), vec![mock("lead"), stuck]).await;
    let first = data(
        &h.call(
            "consult",
            json!({"question": "Compare A and B", "wait_seconds": 0}),
        )
        .await,
    );
    let task_id = first["task_id"].as_str().unwrap().to_string();
    let cancelled = data(&h.call("cancel_task", json!({"task_id": task_id})).await);
    let done = data(
        &h.call(
            "get_task_status",
            json!({"task_id": task_id, "wait_seconds": 30}),
        )
        .await,
    );
    assert_eq!(done["status"], "cancelled", "{cancelled} / {done}");
    h.stop().await;
}

#[tokio::test]
async fn consult_results_are_clipped_to_the_configured_size() {
    let long = format!(
        "## Answer\n{}\n\n## Recommendation\n{}",
        "x".repeat(5000),
        "y".repeat(5000)
    );
    let lead = Arc::new(
        MockProvider::new("lead")
            .with_reply(text("a"))
            .with_reply(text("r"))
            .with_reply(text(&long)),
    );
    let h = start(
        Setup {
            max_result_bytes: Some(2048),
            ..Default::default()
        },
        vec![lead, mock("beta")],
    )
    .await;
    let v = data(
        &h.call("consult", json!({"question": "Compare A and B"}))
            .await,
    );
    assert_eq!(v["truncated"], true);
    assert!(
        v["answer"].as_str().unwrap().len() <= 2048 * 3 / 4 + 40,
        "{}",
        v["answer"].as_str().unwrap().len()
    );
    assert!(v["recommendation"].as_str().unwrap().len() <= 2048 / 16 + 40);
    h.stop().await;
}

#[tokio::test]
async fn consult_length_brief_is_applied_automatically_or_on_request() {
    let mocks = panel(&["lead", "beta"], &[], &[]);
    let h = start(Setup::default(), mocks.clone()).await;
    let v = data(
        &h.call(
            "consult",
            json!({"question": "In two sentences: tabs or spaces?"}),
        )
        .await,
    );
    assert_eq!(v["brief"], true, "{v}");
    assert!(mocks[0].requests()[0].prompt.contains("SHORT answer"));
    let v = data(
        &h.call(
            "consult",
            json!({"question": "In two sentences: tabs or spaces?", "length": "full"}),
        )
        .await,
    );
    assert_eq!(v["brief"], false);
    assert_eq!(v["manual_overrides"], json!(["length"]));
    let r = h
        .call("consult", json!({"question": "q", "length": "huge"}))
        .await;
    assert!(is_error(&r));
    assert_eq!(error_kind(&data(&r)), "invalid_request");
    h.stop().await;
}
