//! The local web interface (Milestone 9): the real HTTP server over mock agents and temporary stores.
//! No real model, no real home directory and no network beyond 127.0.0.1.

#![cfg(unix)]

#[path = "common/scripted.rs"]
mod scripted;

use std::sync::Arc;
use std::time::{Duration, Instant};

use ai_orchestra::Orchestrator;
use ai_orchestra::config::Config;
use ai_orchestra::history::DebateStore;
use ai_orchestra::memory::service::{MemoryService, MemorySettings};
use ai_orchestra::provider::{AgentProvider, ProviderRegistry};
use ai_orchestra::providers::mock::MockProvider;
use ai_orchestra::web::{self, AppState, ServeOptions};
use reqwest::{Client, Method};
use scripted::{Behavior, Fault, scripted};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

struct Server {
    base: String,
    port: u16,
    state: Arc<AppState>,
    agents: Vec<Arc<MockProvider>>,
    client: Client,
    shutdown: CancellationToken,
    serving: Option<tokio::task::JoinHandle<()>>,
    dir: tempfile::TempDir,
}

fn behaviors() -> Vec<(&'static str, Behavior)> {
    vec![
        ("lead", Behavior::normal("alpha")),
        ("beta", Behavior::normal("alpha")),
        ("gamma", Behavior::normal("alpha")),
    ]
}

async fn start() -> Server {
    start_with(behaviors(), |_| {}).await
}

async fn start_with(
    agents: Vec<(&'static str, Behavior)>,
    tweak: impl FnOnce(&mut Config),
) -> Server {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.general.coordinator = "lead".into();
    config.memory.path = Some(dir.path().join("memory.db").display().to_string());
    config.memory.project = Some("web-tests".into());
    tweak(&mut config);
    let mocks: Vec<Arc<MockProvider>> = agents.iter().map(|(n, b)| scripted(n, b, false)).collect();
    let providers: Vec<Arc<dyn AgentProvider>> = mocks
        .iter()
        .map(|a| a.clone() as Arc<dyn AgentProvider>)
        .collect();
    let svc = MemoryService::new(
        dir.path().join("memory.db"),
        MemorySettings::from_config(&config),
    );
    let orch = Orchestrator::with_registry(config.clone(), ProviderRegistry::new(providers))
        .unwrap()
        .with_history(DebateStore::new(dir.path().join("debates")))
        .with_memory(svc);
    let listener = web::bind(&ServeOptions {
        port: 0,
        ..ServeOptions::default()
    })
    .await
    .unwrap();
    let port = listener.local_addr().unwrap().port();
    let state = AppState::new(Arc::new(orch), config, port);
    let shutdown = CancellationToken::new();
    let serving = {
        let (state, shutdown) = (state.clone(), shutdown.clone());
        tokio::spawn(async move {
            let _ = web::serve(listener, state, shutdown).await;
        })
    };
    let client = Client::builder().no_proxy().build().unwrap();
    Server {
        base: format!("http://127.0.0.1:{port}"),
        port,
        state,
        agents: mocks,
        client,
        shutdown,
        serving: Some(serving),
        dir,
    }
}

impl Server {
    async fn call(&self, method: Method, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut req = self
            .client
            .request(method.clone(), format!("{}{path}", self.base));
        if method != Method::GET {
            req = req.header("x-aio-token", &self.state.token);
        }
        if let Some(b) = body {
            req = req
                .header("content-type", "application/json")
                .body(b.to_string());
        }
        let res = req.send().await.unwrap();
        let status = res.status().as_u16();
        let text = res.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    async fn get(&self, path: &str) -> (u16, Value) {
        self.call(Method::GET, path, None).await
    }

    async fn post(&self, path: &str, body: Value) -> (u16, Value) {
        self.call(Method::POST, path, Some(body)).await
    }

    fn calls(&self) -> usize {
        self.agents.iter().map(|a| a.requests().len()).sum()
    }

    /// Reads a job's events (all of them when the job is finished; the stream then closes).
    async fn events(&self, job: &str, last_id: Option<u64>) -> Vec<Value> {
        let mut req = self
            .client
            .get(format!("{}/api/v1/consultations/{job}/events", self.base));
        if let Some(id) = last_id {
            req = req.header("last-event-id", id.to_string());
        }
        let mut res = req.send().await.unwrap();
        assert_eq!(res.status(), 200);
        assert!(
            res.headers()["content-type"]
                .to_str()
                .unwrap()
                .starts_with("text/event-stream")
        );
        let mut buf = String::new();
        let deadline = Instant::now() + Duration::from_secs(40);
        loop {
            let chunk = tokio::time::timeout(Duration::from_secs(40), res.chunk())
                .await
                .expect("the event stream stalled");
            match chunk.unwrap() {
                Some(c) => buf.push_str(&String::from_utf8_lossy(&c)),
                None => break,
            }
            assert!(Instant::now() < deadline, "the event stream never ended");
        }
        buf.replace("\r\n", "\n")
            .split("\n\n")
            .filter_map(|frame| {
                let data: Vec<&str> = frame
                    .lines()
                    .filter_map(|l| l.strip_prefix("data:"))
                    .map(str::trim)
                    .collect();
                (!data.is_empty()).then(|| serde_json::from_str::<Value>(&data.join("\n")).unwrap())
            })
            .collect()
    }

    async fn wait_finished(&self, job: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(40);
        loop {
            let (_, v) = self.get(&format!("/api/v1/consultations/{job}")).await;
            if v["job"]["status"] != "running" {
                return v;
            }
            assert!(
                Instant::now() < deadline,
                "the consultation never finished: {v}"
            );
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    }

    async fn stop(mut self) {
        self.shutdown.cancel();
        if let Some(h) = self.serving.take() {
            let _ = tokio::time::timeout(Duration::from_secs(10), h).await;
        }
    }
}

fn panel() -> Value {
    json!({"question": "Which oscilloscope should I buy for learning electronics?", "settings": {"strategy": "thorough"}})
}

async fn run_panel(s: &Server) -> (String, Value) {
    let (st, v) = s.post("/api/v1/consultations", panel()).await;
    assert_eq!(st, 202, "{v}");
    let job = v["job_id"].as_str().unwrap().to_string();
    let done = s.wait_finished(&job).await;
    (job, done)
}

// ----------------------------------------------------------------------------------- basics

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn health_and_capabilities_publish_the_settings_schema_from_the_backend() {
    let s = start().await;
    let (st, h) = s.get("/api/v1/health").await;
    assert_eq!(st, 200);
    assert_eq!(
        (h["ok"].clone(), h["running_consultations"].clone()),
        (json!(true), json!(0))
    );
    let (_, c) = s.get("/api/v1/capabilities").await;
    let keys: Vec<&str> = c["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["key"].as_str().unwrap())
        .collect();
    for k in [
        "mode",
        "coordinator",
        "agents",
        "rounds",
        "depth",
        "web",
        "strategy",
        "length",
        "timeout",
        "max-calls",
        "memory",
        "context-budget",
        "view",
        "max-sources",
        "freshness-days",
        "max-duration",
        "max-parallel",
    ] {
        assert!(keys.contains(&k), "the schema lacks {k}: {keys:?}");
    }
    assert_eq!(c["providers"].as_array().unwrap().len(), 3);
    assert_eq!(c["features"]["memory"], true);
    assert_eq!(c["limits"]["max_urls"], 20);
    assert!(c["safety"].as_array().unwrap().len() >= 4);
    assert_eq!(s.calls(), 0, "reading capabilities never calls an agent");
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_for_another_host_is_refused_whatever_it_asks() {
    let s = start().await;
    for host in [
        "evil.example",
        "evil.example:80",
        &format!("evil.example:{}", s.port),
        "127.0.0.1.evil.example",
    ] {
        let res = s
            .client
            .get(format!("{}/api/v1/health", s.base))
            .header("host", host)
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 421, "host {host} must be refused");
    }
    for host in [
        format!("localhost:{}", s.port),
        format!("127.0.0.1:{}", s.port),
    ] {
        let res = s
            .client
            .get(format!("{}/api/v1/health", s.base))
            .header("host", host)
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 200);
    }
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn requests_the_browser_labels_as_from_another_site_are_refused() {
    let s = start().await;
    let url = format!("{}/api/v1/health", s.base);
    for site in ["cross-site", "same-site"] {
        let res = s
            .client
            .get(&url)
            .header("sec-fetch-site", site)
            .header("sec-fetch-mode", "cors")
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 403, "a {site} read must be refused");
        let post = s
            .client
            .post(format!("{}/api/v1/consultations", s.base))
            .header("x-aio-token", &s.state.token)
            .header("sec-fetch-site", site)
            .header("content-type", "application/json")
            .body(json!({"question": "x", "dry_run": true}).to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(
            post.status(),
            403,
            "a {site} change is refused even with the token"
        );
    }
    // Our own page, the address bar, and a link followed from another site (it only shows the page).
    for (site, mode) in [
        ("same-origin", "cors"),
        ("none", "navigate"),
        ("cross-site", "navigate"),
    ] {
        let res = s
            .client
            .get(&url)
            .header("sec-fetch-site", site)
            .header("sec-fetch-mode", mode)
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 200, "{site} / {mode} must be allowed");
    }
    // A client that sends no fetch metadata (curl, the CLI tests) is unaffected: it still needs Host and the token.
    assert_eq!(s.client.get(&url).send().await.unwrap().status(), 200);
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_oversized_request_body_is_refused() {
    let s = start().await;
    let big = format!("{{\"question\": \"{}\"}}", "a".repeat(2 * 1024 * 1024));
    let res = s
        .client
        .post(format!("{}/api/v1/consultations", s.base))
        .header("x-aio-token", &s.state.token)
        .header("content-type", "application/json")
        .body(big)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 413);
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn changing_anything_needs_the_run_token_json_and_a_local_origin() {
    let s = start().await;
    let url = format!("{}/api/v1/consultations", s.base);
    let body = json!({"question": "x", "dry_run": true});
    let no_token = s
        .client
        .post(&url)
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(no_token.status(), 403);
    let wrong = s
        .client
        .post(&url)
        .header("x-aio-token", "0".repeat(64))
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), 403);
    let text = s
        .client
        .post(&url)
        .header("x-aio-token", &s.state.token)
        .header("content-type", "text/plain")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(
        text.status(),
        415,
        "a body that is not JSON is refused (a form post cannot reach the API)"
    );
    let foreign = s
        .client
        .post(&url)
        .header("x-aio-token", &s.state.token)
        .header("origin", "http://evil.example")
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(
        foreign.status(),
        403,
        "a foreign origin is refused even with the token"
    );
    let own = s
        .client
        .post(&url)
        .header("x-aio-token", &s.state.token)
        .header("origin", format!("http://127.0.0.1:{}", s.port))
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(own.status(), 200);
    let opt = s
        .client
        .request(Method::OPTIONS, &url)
        .send()
        .await
        .unwrap();
    assert_eq!(opt.status(), 405, "there is no CORS");
    assert!(opt.headers().get("access-control-allow-origin").is_none());
    for m in [Method::PUT, Method::DELETE] {
        let r = s
            .client
            .request(m, format!("{}/api/v1/settings", s.base))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 403, "every state-changing verb needs the token");
    }
    assert_eq!(s.calls(), 0);
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn responses_carry_the_security_headers() {
    let s = start().await;
    let res = s
        .client
        .get(format!("{}/api/v1/health", s.base))
        .send()
        .await
        .unwrap();
    let h = res.headers();
    let csp = h["content-security-policy"].to_str().unwrap();
    assert!(
        csp.contains("default-src 'none'")
            && csp.contains("script-src 'self'")
            && csp.contains("frame-ancestors 'none'"),
        "{csp}"
    );
    assert!(
        !csp.contains("unsafe-eval") && !csp.contains("http:") && !csp.contains("https:"),
        "no external origin is allowed: {csp}"
    );
    assert_eq!(h["x-content-type-options"], "nosniff");
    assert_eq!(h["referrer-policy"], "no-referrer");
    assert_eq!(h["x-frame-options"], "DENY");
    assert_eq!(h["cache-control"], "no-store");
    assert!(h.get("access-control-allow-origin").is_none());
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn static_files_come_only_from_the_embedded_table() {
    let s = start().await;
    for path in [
        "/../Cargo.toml",
        "/%2e%2e/Cargo.toml",
        "/..%2f..%2fCargo.toml",
        "/assets/../../Cargo.toml",
        "/etc/passwd.txt",
    ] {
        let res = s
            .client
            .get(format!("{}{path}", s.base))
            .send()
            .await
            .unwrap();
        let body = res.text().await.unwrap();
        assert!(
            !body.contains("[package]") && !body.contains("root:"),
            "{path} leaked a file"
        );
    }
    let (st, v) = s.get("/api/v1/nothing-here").await;
    assert_eq!((st, v["error"]["kind"].clone()), (404, json!("not_found")));
    let res = s
        .client
        .get(format!("{}/assets/missing.js", s.base))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 404);
    if web::assets::has_frontend() {
        let page = s
            .client
            .get(format!("{}/history", s.base))
            .send()
            .await
            .unwrap();
        assert_eq!(
            page.status(),
            200,
            "a page route falls back to the application"
        );
        let html = page.text().await.unwrap();
        assert!(
            html.contains(&s.state.token),
            "the page carries this run's token"
        );
        assert!(!html.contains("__AIO_TOKEN__"));
    }
    s.stop().await;
}

#[tokio::test]
async fn a_non_loopback_address_needs_an_explicit_flag() {
    let err = web::bind(&ServeOptions {
        host: "0.0.0.0".parse().unwrap(),
        port: 0,
        allow_non_local: false,
    })
    .await
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("loopback") && err.contains("--allow-non-local"),
        "{err}"
    );
}

// ------------------------------------------------------------------------------- validation

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_requests_are_rejected_before_any_agent_is_called() {
    let s = start().await;
    let cases: Vec<(Value, &str)> = vec![
        (json!({"question": "   "}), "question"),
        (
            json!({"question": "q", "settings": {"rounds": "3"}}),
            "settings.rounds",
        ),
        (
            json!({"question": "q", "session_settings": {"strategy": "reckless"}}),
            "session_settings.strategy",
        ),
        (
            json!({"question": "q", "settings": {"coordinator": "nobody"}}),
            "settings.agents",
        ),
        (
            json!({"question": "q", "settings": {"agents": ["lead", "ghost"]}}),
            "settings.agents",
        ),
        (
            json!({"question": "q", "urls": ["http://plain.example/"]}),
            "urls",
        ),
        (
            json!({"question": "q", "urls": ["https://a.example/ b"]}),
            "urls",
        ),
        (
            json!({"question": "q", "settings": {"web": "off"}, "urls": ["https://a.example/"]}),
            "urls",
        ),
        (json!({"question": "x".repeat(300_000)}), "question"),
        (
            json!({"question": "q", "settings": {"max_sources": 0}}),
            "settings.max-sources",
        ),
    ];
    for (body, field) in cases {
        let (st, v) = s.post("/api/v1/consultations", body.clone()).await;
        assert_eq!(st, 422, "{body}: {v}");
        assert_eq!(v["error"]["kind"], "validation");
        let got = v["error"]["field"].as_str().unwrap_or("");
        assert!(
            got == field || (field == "settings.max-sources" && got.starts_with("settings.")),
            "expected field {field}, got {got}: {v}"
        );
    }
    let (st, v) = s
        .post(
            "/api/v1/consultations",
            json!({"question": "q", "surprise": true}),
        )
        .await;
    assert_eq!(
        (st, v["error"]["kind"].clone()),
        (422, json!("validation")),
        "unknown fields are refused"
    );
    let (st, _) = s
        .post("/api/v1/consultations", json!("not an object"))
        .await;
    assert_eq!(st, 422);
    assert_eq!(s.calls(), 0, "nothing was started");
    assert_eq!(s.state.jobs.running(), 0);
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_plan_preview_runs_nothing_and_reports_where_each_setting_came_from() {
    let s = start().await;
    let (st, v) = s.post("/api/v1/settings", json!({})).await;
    assert_eq!(st, 405);
    let _ = v;
    let (st, _) = s
        .call(
            Method::PUT,
            "/api/v1/settings",
            Some(json!({"defaults": {"strategy": "fast", "length": "brief"}})),
        )
        .await;
    assert_eq!(st, 200);
    let body = json!({"question": "Which oscilloscope should I buy?", "dry_run": true, "session_settings": {"length": "full"}, "settings": {"strategy": "thorough"}});
    let (st, v) = s.post("/api/v1/consultations", body).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["dry_run"], true);
    assert!(
        v["plan_text"].as_str().unwrap().contains("Mode:")
            || v["plan_text"].as_str().unwrap().len() > 10
    );
    let f = &v["settings_in_force"];
    assert_eq!(
        (
            f["strategy"]["value"].clone(),
            f["strategy"]["origin"].clone()
        ),
        (json!("thorough"), json!("request")),
        "this question beats everything"
    );
    assert_eq!(
        (f["length"]["value"].clone(), f["length"]["origin"].clone()),
        (json!("full"), json!("session")),
        "the session beats the saved default"
    );
    assert_eq!(f["mode"]["origin"], "auto");
    assert_eq!(s.calls(), 0, "a preview calls no agent");
    assert_eq!(s.state.jobs.running(), 0);
    let (_, v) = s
        .post(
            "/api/v1/consultations",
            json!({"question": "Which oscilloscope should I buy?", "dry_run": true}),
        )
        .await;
    assert_eq!(
        v["settings_in_force"]["strategy"]["origin"], "preference",
        "the saved default applies when nothing overrides it"
    );
    s.stop().await;
}

// ----------------------------------------------------------------------- a consultation, live

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_consultation_streams_ordered_events_and_is_saved_with_its_real_steps() {
    let s = start().await;
    let (job, done) = run_panel(&s).await;
    assert_eq!(done["job"]["status"], "completed", "{done}");
    let consultation = &done["consultation"];
    assert_eq!(consultation["status"], "complete");
    assert!(
        consultation["final_answer"]
            .as_str()
            .unwrap()
            .contains("recommendation")
    );
    let calls_made = s.calls();
    assert_eq!(
        consultation["calls"], calls_made,
        "the reported calls are the calls the agents received"
    );

    let events = s.events(&job, None).await;
    let seqs: Vec<u64> = events.iter().map(|e| e["seq"].as_u64().unwrap()).collect();
    assert!(
        seqs.windows(2).all(|w| w[1] == w[0] + 1),
        "events are contiguous and ordered: {seqs:?}"
    );
    let kinds: Vec<&str> = events.iter().map(|e| e["kind"].as_str().unwrap()).collect();
    for k in [
        "planning_started",
        "strategy_selected",
        "consultation_created",
        "provider_started",
        "provider_completed",
        "agent_message_recorded",
        "consultation_completed",
    ] {
        assert!(kinds.contains(&k), "missing {k}: {kinds:?}");
    }
    assert_eq!(kinds.first(), Some(&"planning_started"));
    assert_eq!(kinds.last(), Some(&"consultation_completed"));
    assert_eq!(
        kinds
            .iter()
            .filter(|k| **k == "consultation_completed")
            .count(),
        1
    );
    let started = kinds.iter().filter(|k| **k == "provider_started").count();
    let finished = kinds.iter().filter(|k| **k == "provider_completed").count();
    assert_eq!(
        (started, finished),
        (calls_made, calls_made),
        "one start and one completion per real call"
    );
    assert!(
        !events.iter().any(|e| e.to_string().contains("question\"")),
        "events never carry the question text"
    );

    // The theater is exactly the recorded steps: one message per call, the text the agent returned.
    let id = consultation["id"].as_str().unwrap();
    let (st, tl) = s.get(&format!("/api/v1/consultations/{id}/theater")).await;
    assert_eq!(st, 200);
    let messages = tl["messages"].as_array().unwrap();
    assert_eq!(messages.len(), calls_made);
    assert!(
        messages
            .iter()
            .all(|m| m["ok"] == true && m["text"].as_str().is_some_and(|t| !t.is_empty()))
    );
    assert!(
        messages
            .iter()
            .any(|m| m["text"].as_str().is_some_and(|t| t.contains("alpha"))),
        "the agents' own words are carried through"
    );
    let recorded: Vec<&Value> = events
        .iter()
        .filter(|e| e["kind"] == "agent_message_recorded")
        .collect();
    assert_eq!(
        recorded.len(),
        messages.len(),
        "the live feed and the replay are the same record"
    );
    assert!(
        tl["note"]
            .as_str()
            .unwrap()
            .contains("Nothing is simulated")
    );

    // History lists it, searches it, filters it; analytics measure it.
    let (_, list) = s
        .get("/api/v1/consultations?q=oscilloscope&status=complete")
        .await;
    assert_eq!(list["total"], 1);
    assert_eq!(list["items"][0]["id"], id);
    let (_, none) = s.get("/api/v1/consultations?q=zebra").await;
    assert_eq!(none["total"], 0);
    let (_, none) = s.get("/api/v1/consultations?status=failed").await;
    assert_eq!(none["total"], 0);
    let (_, a) = s.get("/api/v1/analytics").await;
    assert_eq!(a["consultations"], 1);
    assert_eq!(a["by_status"]["complete"], 1);
    assert_eq!(
        a["kind"],
        "measurements of consultations saved on this machine"
    );

    // A consultation opens by its own id (served from the saved record).
    let (st, saved) = s.get(&format!("/api/v1/consultations/{id}")).await;
    assert_eq!(st, 200);
    assert_eq!(
        saved["consultation"]["final_answer"], consultation["final_answer"],
        "the same consultation, whether served from the running server or the saved file"
    );
    let (_, research) = s.get(&format!("/api/v1/consultations/{id}/research")).await;
    assert_eq!(
        research["web_occurred"], false,
        "no web research means no sources, said plainly"
    );
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconnecting_resumes_after_the_last_event_and_never_repeats_a_call() {
    let s = start().await;
    let (job, _) = run_panel(&s).await;
    let calls = s.calls();
    let all = s.events(&job, None).await;
    let from = all[3]["seq"].as_u64().unwrap();
    let resumed = s.events(&job, Some(from)).await;
    assert_eq!(
        resumed.len(),
        all.len() - 4,
        "only events after the last one seen"
    );
    assert_eq!(resumed.first().unwrap()["seq"].as_u64().unwrap(), from + 1);
    let (a, b) = tokio::join!(s.events(&job, None), s.events(&job, None));
    assert_eq!(a, b, "two tabs see the same events");
    assert_eq!(a, all);
    let res = s
        .client
        .get(format!(
            "{}/api/v1/consultations/{job}/events?after={from}",
            s.base
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        200,
        "?after= works where Last-Event-ID cannot be set"
    );
    assert_eq!(
        s.calls(),
        calls,
        "reading events (even repeatedly) calls no agent"
    );
    let res = s
        .client
        .get(format!(
            "{}/api/v1/consultations/job-unknown/events",
            s.base
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 404);
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn events_arrive_while_the_consultation_runs_not_only_at_the_end() {
    let slow = Behavior {
        delay_ms: 600,
        ..Behavior::normal("alpha")
    };
    let s = start_with(
        vec![
            ("lead", slow.clone()),
            ("beta", slow.clone()),
            ("gamma", slow),
        ],
        |_| {},
    )
    .await;
    let (_, v) = s.post("/api/v1/consultations", panel()).await;
    let job = v["job_id"].as_str().unwrap().to_string();
    let mut res = s
        .client
        .get(format!("{}/api/v1/consultations/{job}/events", s.base))
        .send()
        .await
        .unwrap();
    let begin = Instant::now();
    let mut first_at = None;
    let mut buf = String::new();
    while let Some(c) = tokio::time::timeout(Duration::from_secs(30), res.chunk())
        .await
        .unwrap()
        .unwrap()
    {
        buf.push_str(&String::from_utf8_lossy(&c));
        if first_at.is_none() && buf.contains("provider_started") {
            first_at = Some(begin.elapsed());
        }
    }
    let first = first_at.expect("a provider_started event");
    assert!(
        first < Duration::from_millis(500),
        "the first call is announced when it starts ({first:?}), not when the run ends"
    );
    assert!(
        begin.elapsed() > Duration::from_millis(1200),
        "the whole run took longer, so events were not batched at the end"
    );
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failing_agent_makes_the_result_partial_and_is_shown_as_a_failure() {
    let broken = Behavior {
        fault: Some(Fault::Always),
        ..Behavior::normal("alpha")
    };
    let s = start_with(
        vec![
            ("lead", Behavior::normal("alpha")),
            ("beta", broken),
            ("gamma", Behavior::normal("alpha")),
        ],
        |_| {},
    )
    .await;
    let (job, done) = run_panel(&s).await;
    assert_eq!(done["job"]["status"], "partial", "{done}");
    assert_eq!(done["consultation"]["status"], "partial", "never complete");
    let events = s.events(&job, None).await;
    assert!(events.iter().any(|e| e["kind"] == "provider_completed"
        && e["data"]["ok"] == false
        && e["data"]["agent"] == "beta"));
    assert_eq!(events.last().unwrap()["kind"], "consultation_completed");
    assert_eq!(events.last().unwrap()["data"]["status"], "partial");
    let id = done["consultation"]["id"].as_str().unwrap();
    let (_, tl) = s.get(&format!("/api/v1/consultations/{id}/theater")).await;
    let failed: Vec<&Value> = tl["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["ok"] == false)
        .collect();
    assert!(!failed.is_empty());
    assert!(
        failed
            .iter()
            .all(|m| m["text"].is_null() && m["error"].is_string()),
        "a failure carries its error and no invented reply"
    );
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_stops_the_work_and_is_never_reported_as_complete() {
    let slow = Behavior {
        delay_ms: 8000,
        ..Behavior::normal("alpha")
    };
    let s = start_with(
        vec![
            ("lead", slow.clone()),
            ("beta", slow.clone()),
            ("gamma", slow),
        ],
        |_| {},
    )
    .await;
    let (_, v) = s.post("/api/v1/consultations", panel()).await;
    let job = v["job_id"].as_str().unwrap().to_string();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let t0 = Instant::now();
    let (st, c) = s
        .post(&format!("/api/v1/consultations/{job}/cancel"), json!({}))
        .await;
    assert_eq!(st, 202, "{c}");
    let done = s.wait_finished(&job).await;
    assert!(
        t0.elapsed() < Duration::from_secs(5),
        "cancelling does not wait for the slow agents"
    );
    assert_eq!(done["job"]["status"], "cancelled");
    let events = s.events(&job, None).await;
    assert_eq!(events.last().unwrap()["kind"], "consultation_cancelled");
    assert!(!events.iter().any(|e| e["kind"] == "consultation_completed"));
    let (st, v) = s
        .post(&format!("/api/v1/consultations/{job}/cancel"), json!({}))
        .await;
    assert_eq!(
        (st, v["error"]["kind"].clone()),
        (409, json!("conflict")),
        "a finished consultation cannot be cancelled again"
    );
    let (st, _) = s
        .post("/api/v1/consultations/job-nope/cancel", json!({}))
        .await;
    assert_eq!(st, 404);
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_so_many_consultations_run_at_once() {
    let slow = Behavior {
        delay_ms: 1500,
        ..Behavior::normal("alpha")
    };
    let s = start_with(
        vec![
            ("lead", slow.clone()),
            ("beta", slow.clone()),
            ("gamma", slow),
        ],
        |c| c.web.max_concurrent_consultations = 1,
    )
    .await;
    let (st, first) = s.post("/api/v1/consultations", panel()).await;
    assert_eq!(st, 202);
    let (st, v) = s.post("/api/v1/consultations", panel()).await;
    assert_eq!((st, v["error"]["kind"].clone()), (429, json!("busy")));
    let (_, list) = s.get("/api/v1/consultations").await;
    assert_eq!(
        list["running"].as_array().unwrap().len(),
        1,
        "history lists what is running now"
    );
    let job = first["job_id"].as_str().unwrap();
    s.post(&format!("/api/v1/consultations/{job}/cancel"), json!({}))
        .await;
    s.wait_finished(job).await;
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stopping_the_server_mid_run_leaves_a_valid_cancelled_record() {
    let slow = Behavior {
        delay_ms: 6000,
        ..Behavior::normal("alpha")
    };
    let mut s = start_with(
        vec![
            ("lead", slow.clone()),
            ("beta", slow.clone()),
            ("gamma", slow),
        ],
        |_| {},
    )
    .await;
    let (_, v) = s.post("/api/v1/consultations", panel()).await;
    let job_id = v["job_id"].as_str().unwrap().to_string();
    tokio::time::sleep(Duration::from_millis(300)).await;
    s.shutdown.cancel();
    let handle = s.serving.take().unwrap();
    tokio::time::timeout(Duration::from_secs(15), handle)
        .await
        .expect("the server stops in time")
        .unwrap();
    assert_eq!(s.state.jobs.running(), 0, "no job is left running");
    let job = s.state.jobs.find(&job_id).unwrap();
    assert_eq!(format!("{:?}", job.status()), "Cancelled");
    let store = s.state.orch.history().unwrap();
    if let Some(id) = job.consultation_id() {
        let saved = store
            .load(&id)
            .expect("a record that exists parses completely");
        assert_ne!(saved.status.as_str(), "complete");
    }
    let leftovers: Vec<_> = std::fs::read_dir(s.dir.path().join("debates"))
        .map(|d| {
            d.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        leftovers.iter().all(|n| !n.ends_with(".tmp")),
        "no half-written file: {leftovers:?}"
    );
}

// ------------------------------------------------------------- history, feedback, old records

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn feedback_and_deleting_need_explicit_confirmation() {
    let s = start().await;
    let (_, done) = run_panel(&s).await;
    let id = done["consultation"]["id"].as_str().unwrap().to_string();
    let (st, v) = s
        .post(
            &format!("/api/v1/consultations/{id}/feedback"),
            json!({"rating": "great"}),
        )
        .await;
    assert_eq!((st, v["error"]["field"].clone()), (422, json!("rating")));
    let (st, _) = s
        .post(
            &format!("/api/v1/consultations/{id}/feedback"),
            json!({"rating": "helpful", "note": "clear"}),
        )
        .await;
    assert_eq!(st, 201);
    let (_, fb) = s.get("/api/v1/feedback").await;
    assert_eq!(fb["items"].as_array().unwrap().len(), 1);
    let (st, v) = s
        .call(Method::DELETE, &format!("/api/v1/consultations/{id}"), None)
        .await;
    assert_eq!((st, v["error"]["field"].clone()), (422, json!("confirm")));
    let (st, _) = s
        .call(
            Method::DELETE,
            &format!("/api/v1/consultations/{id}?confirm=true"),
            None,
        )
        .await;
    assert_eq!(st, 200);
    let (st, _) = s.get(&format!("/api/v1/consultations/{id}/theater")).await;
    assert_ne!(st, 200, "deleted: it is gone from the history");
    let (_, list) = s.get("/api/v1/consultations").await;
    assert_eq!(list["total"], 0);
    let (st, _) = s
        .call(
            Method::DELETE,
            "/api/v1/consultations/..%2f..%2fetc?confirm=true",
            None,
        )
        .await;
    assert_ne!(st, 200, "an id that is not an id deletes nothing");
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_older_plain_debate_record_still_opens_and_replays() {
    let s = start().await;
    let dir = s.dir.path().join("debates");
    std::fs::create_dir_all(&dir).unwrap();
    let record = json!({
        "schema": 1, "id": "20250101-000000-000001", "conversation": 1, "created_at_ms": 1_735_689_600_000u64,
        "question": "an old question", "coordinator": "lead", "participants": ["beta"], "skipped": [],
        "rounds_planned": 1, "status": "complete", "synthesized": true,
        "steps": [
            {"phase": "proposal", "round": 0, "agent": "lead", "offset_ms": 0, "duration_ms": 10, "ok": true, "text": "old proposal", "error": null, "error_kind": null, "settings": null},
            {"phase": "synthesis", "round": 1, "agent": "lead", "offset_ms": 20, "duration_ms": 10, "ok": true, "text": "old final", "error": null, "error_kind": null, "settings": null}
        ],
        "initial_proposal": "old proposal", "final_answer": "old final", "error": null, "calls": 2, "warnings": [], "duration_ms": 30
    });
    std::fs::write(dir.join("20250101-000000-000001.json"), record.to_string()).unwrap();
    let (st, v) = s.get("/api/v1/consultations/20250101-000000-000001").await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["consultation"]["final_answer"], "old final");
    assert!(v["consultation"]["mode"].is_null() && v["consultation"]["strategy"].is_null());
    assert!(
        v["consultation"]["age_days"].as_u64().unwrap() > 365,
        "its age is reported so the interface can warn"
    );
    let (_, tl) = s
        .get("/api/v1/consultations/20250101-000000-000001/theater")
        .await;
    assert_eq!(tl["messages"].as_array().unwrap().len(), 2);
    assert_eq!(
        s.calls(),
        0,
        "opening and replaying an old record calls no agent"
    );
    s.stop().await;
}

// ------------------------------------------------------------------------ settings, interface

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn saved_defaults_validate_persist_and_clear() {
    let s = start().await;
    let (st, v) = s
        .call(
            Method::PUT,
            "/api/v1/settings",
            Some(json!({"defaults": {"rounds": "9"}})),
        )
        .await;
    assert_eq!(
        (st, v["error"]["field"].clone()),
        (422, json!("defaults.rounds"))
    );
    let (st, _) = s.call(Method::PUT, "/api/v1/settings", Some(json!({"defaults": {"strategy": "thorough", "max_sources": 5, "agents": ["lead", "beta"]}}))).await;
    assert_eq!(st, 200);
    let (_, g) = s.get("/api/v1/settings").await;
    assert_eq!(g["defaults"]["strategy"], "thorough");
    assert_eq!(g["defaults"]["max_sources"], 5);
    assert_eq!(g["defaults"]["agents"], json!(["lead", "beta"]));
    let (st, _) = s
        .call(
            Method::PUT,
            "/api/v1/settings",
            Some(json!({"defaults": {}})),
        )
        .await;
    assert_eq!(st, 200);
    let (_, g) = s.get("/api/v1/settings").await;
    assert!(
        g["defaults"]
            .as_object()
            .unwrap()
            .values()
            .all(Value::is_null),
        "an empty save returns everything to Auto: {g}"
    );
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interface_preferences_validate_and_persist() {
    let s = start().await;
    let (_, g) = s.get("/api/v1/ui-preferences").await;
    assert_eq!(
        (
            g["preferences"]["theme"].clone(),
            g["preferences"]["language"].clone()
        ),
        (json!("dark"), json!("es")),
        "dark and Spanish by default"
    );
    let bad = [
        json!({"theme": "neon", "language": "es", "reduced_motion": "system", "theater_default": false}),
        json!({"theme": "dark", "language": "fr", "reduced_motion": "system", "theater_default": false}),
    ];
    for b in bad {
        let (st, v) = s.call(Method::PUT, "/api/v1/ui-preferences", Some(b)).await;
        assert_eq!((st, v["error"]["kind"].clone()), (422, json!("validation")));
    }
    let (st, _) = s.call(Method::PUT, "/api/v1/ui-preferences", Some(json!({"theme": "light", "language": "en", "reduced_motion": "on", "theater_default": true}))).await;
    assert_eq!(st, 200);
    let (_, g) = s.get("/api/v1/ui-preferences").await;
    assert_eq!(g["preferences"]["theme"], "light");
    assert_eq!(g["preferences"]["theater_default"], true);
    s.stop().await;
}

// ------------------------------------------------------------------------------------ memory

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn permanent_memory_needs_confirmation_and_sensitive_text_is_masked() {
    let s = start().await;
    let (st, v) = s
        .post(
            "/api/v1/memory",
            json!({"text": "My budget is 8000 MXN", "scope": "project"}),
        )
        .await;
    assert_eq!(
        (st, v["error"]["field"].clone()),
        (422, json!("confirm")),
        "nothing is remembered without explicit confirmation"
    );
    let (_, list) = s.get("/api/v1/memory").await;
    assert_eq!(list["items"].as_array().unwrap().len(), 0);
    let (st, v) = s.post("/api/v1/memory", json!({"text": "My budget is 8000 MXN", "scope": "project", "kind": "requirement", "confirm": true})).await;
    assert_eq!(st, 201, "{v}");
    let id = v["id"].as_str().unwrap().to_string();
    let (_, list) = s.get("/api/v1/memory?q=budget").await;
    assert_eq!(list["items"][0]["id"], id.as_str());
    assert_eq!(
        list["items"][0]["scope"], "project",
        "scope is shown on every record"
    );
    let (st, v) = s
        .post(
            "/api/v1/memory",
            json!({"text": "x", "scope": "galaxy", "confirm": true}),
        )
        .await;
    assert_eq!((st, v["error"]["field"].clone()), (422, json!("scope")));
    let (st, _) = s
        .call(
            Method::PUT,
            &format!("/api/v1/memory/{id}"),
            Some(json!({"text": "My budget is 9000 MXN"})),
        )
        .await;
    assert_eq!(st, 422, "correcting needs confirmation too");
    let (st, _) = s
        .call(
            Method::PUT,
            &format!("/api/v1/memory/{id}"),
            Some(json!({"text": "My budget is 9000 MXN", "confirm": true})),
        )
        .await;
    assert_eq!(st, 200);
    let (_, list) = s.get("/api/v1/memory").await;
    assert!(
        list["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["text"].as_str().unwrap().contains("9000")),
        "{list}"
    );
    let (st, _) = s
        .call(Method::DELETE, &format!("/api/v1/memory/{id}"), None)
        .await;
    assert_eq!(st, 422, "deleting needs confirm=true");
    let (st, exp) = s.get("/api/v1/memory/export").await;
    assert_eq!(st, 200);
    assert!(exp.to_string().contains("9000"));
    assert_eq!(s.calls(), 0, "memory management calls no agent");
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sessions_continue_through_memory_and_a_context_preview_sends_nothing() {
    let s = start().await;
    let (_, first) = s.post("/api/v1/consultations", json!({"question": "Which oscilloscope should I buy?", "settings": {"strategy": "fast"}, "new_session": true})).await;
    let job = first["job_id"].as_str().unwrap().to_string();
    let done = s.wait_finished(&job).await;
    let session = done["consultation"]["meta"]["context"]["session"]
        .as_str()
        .expect("a session was created")
        .to_string();
    let (_, sessions) = s.get("/api/v1/sessions").await;
    assert!(
        sessions["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == session.as_str())
    );
    let calls = s.calls();
    let (st, p) = s
        .post(
            "/api/v1/context/preview",
            json!({"question": "What if I increase my budget?", "session": session}),
        )
        .await;
    assert_eq!(st, 200, "{p}");
    assert!(p["note"].as_str().unwrap().contains("Nothing was sent"));
    assert_eq!(s.calls(), calls, "a context preview calls no agent");
    let (_, second) = s.post("/api/v1/consultations", json!({"question": "What if I increase my budget?", "settings": {"strategy": "fast"}, "session": session})).await;
    let done2 = s.wait_finished(second["job_id"].as_str().unwrap()).await;
    assert_eq!(
        done2["consultation"]["meta"]["context"]["session"],
        session.as_str(),
        "the follow-up continues the same session"
    );
    let (st, v) = s.post("/api/v1/consultations", json!({"question": "q", "settings": {"memory": "off"}, "session": session, "dry_run": true})).await;
    assert_eq!(
        (st, v["error"]["field"].clone()),
        (422, json!("memory")),
        "a session with memory off is refused up front"
    );
    let (st, _) = s
        .call(Method::DELETE, &format!("/api/v1/sessions/{session}"), None)
        .await;
    assert_eq!(st, 422);
    let (st, _) = s
        .call(
            Method::DELETE,
            &format!("/api/v1/sessions/{session}?confirm=true"),
            None,
        )
        .await;
    assert_eq!(st, 200);
    s.stop().await;
}

// -------------------------------------------------------------------- providers and reports

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_provider_page_sends_no_prompt_and_never_claims_to_know_who_is_signed_in() {
    let s = start().await;
    let (_, run) = run_panel(&s).await;
    assert_eq!(run["job"]["status"], "completed");
    let calls = s.calls();
    let (st, p) = s.get("/api/v1/providers").await;
    assert_eq!(st, 200);
    let rows = p["providers"].as_array().unwrap();
    assert_eq!(rows.len(), 3);
    for r in rows {
        assert_eq!(r["authentication"]["status"], "unknown");
    }
    let lead = rows.iter().find(|r| r["id"] == "lead").unwrap();
    assert!(
        lead["history"]["invocations"].as_u64().unwrap_or(0) == 0
            || lead["history"].is_object()
            || lead["history"].is_null()
    );
    assert_eq!(s.calls(), calls, "opening the provider page spends nothing");
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reports_list_evaluations_and_say_what_kind_of_result_they_are() {
    let s = start().await;
    let (st, r) = s.get("/api/v1/reports").await;
    assert_eq!(st, 200);
    assert!(r["items"].is_array());
    assert!(
        r["note"]
            .as_str()
            .unwrap()
            .contains("spends subscription quota")
    );
    let (st, v) = s.get("/api/v1/reports/eval-nope").await;
    assert_eq!((st, v["error"]["kind"].clone()), (404, json!("not_found")));
    let (_, a) = s.get("/api/v1/analytics").await;
    assert_eq!(
        a["consultations"], 0,
        "no data means no numbers, not zeros presented as results"
    );
    s.stop().await;
}

// ------------------------------------------------------------------------------ research record

/// A saved consultation with web research, built from the real types (so it cannot drift from the
/// engine's record format). `web/e2e/fixtures/research-record.json` is generated from this; the end-to-end
/// browser tests load it into a private history so the Research screen has verified sources to show.
fn research_record() -> Value {
    use ai_orchestra::consult::{ConsultMode, ConsultationMeta};
    use ai_orchestra::research::claims::{ClaimRecord, ClaimStatus, EvidenceLink, Relation};
    use ai_orchestra::research::engine::{
        CitationSummary, Contradiction, ResearchBudget, ResearchReport,
    };
    use ai_orchestra::research::plan::{Depth, WebMode};
    use ai_orchestra::research::quality::{ClaimCategory, Freshness, SourceKind, Suitability};
    use ai_orchestra::research::source::{
        Discovery, DiscoveryMethod, Excerpt, SourceRecord, SourceStatus,
    };

    let retrieved_at = 1_785_542_400_000u64; // 2026-08-01
    let source =
        |id: u32, url: &str, domain: &str, title: &str, status: SourceStatus, text: &str| {
            SourceRecord {
                id,
                original_url: url.into(),
                final_url: Some(url.into()),
                canonical_url: None,
                title: Some(title.into()),
                domain: domain.into(),
                author: None,
                published: Some("2026-06-10".into()),
                updated: None,
                retrieved_at_ms: (status == SourceStatus::Retrieved).then_some(retrieved_at),
                discovery: vec![Discovery {
                    method: DiscoveryMethod::ProviderToolReported,
                    provider: Some("lead".into()),
                }],
                http_status: (status == SourceStatus::Retrieved).then_some(200),
                content_type: Some("text/html".into()),
                content_hash: None,
                status,
                error_kind: (status != SourceStatus::Retrieved).then(|| "http_error".into()),
                error: (status != SourceStatus::Retrieved).then(|| "the page answered 404".into()),
                text_chars: text.len(),
                text_preview: text.into(),
                kind: SourceKind::VendorOrProjectDocumentation,
                kind_note: "documentation by URL shape".into(),
                upstream_group: None,
                excerpts: if status == SourceStatus::Retrieved {
                    vec![Excerpt {
                        text: text.into(),
                        offset: 0,
                    }]
                } else {
                    vec![]
                },
                limitations: vec![],
            }
        };
    let link = |source_id: u32, relation: Relation, passage: &str| EvidenceLink {
        source_id,
        relation,
        passage: passage.into(),
        offset: 0,
        assessed_by: vec!["beta".into()],
        source_kind: SourceKind::VendorOrProjectDocumentation,
        suitability: Suitability::Strong,
        suitability_note: "the manufacturer's own specification".into(),
        freshness: Freshness::Current,
        reason: "states the figure directly".into(),
    };
    let claim = |id: u32,
                 text: &str,
                 status: ClaimStatus,
                 evidence: Vec<EvidenceLink>,
                 sup: usize,
                 con: usize| ClaimRecord {
        id,
        text: text.into(),
        category: ClaimCategory::ProductSpecification,
        originating_agents: vec!["lead".into()],
        needs_external_evidence: true,
        cited_by_agents: vec![1],
        status,
        evidence,
        supporting_independent_sources: sup,
        contradicting_independent_sources: con,
        notes: vec![],
    };
    let report = ResearchReport {
        schema: 1,
        web_requested: WebMode::On,
        depth: Depth::Standard,
        max_sources: 6,
        freshness_days: Some(30),
        discovery_agent: Some("lead".into()),
        subquestions: vec!["What is the analog bandwidth of the Model X?".into()],
        search_queries: vec!["Model X oscilloscope analog bandwidth datasheet".into()],
        sources: vec![
            source(
                1,
                "https://vendor.example/model-x/datasheet",
                "vendor.example",
                "Model X datasheet",
                SourceStatus::Retrieved,
                "The Model X has 100 MHz of analog bandwidth and a 1 GSa/s sample rate.",
            ),
            source(
                2,
                "https://review.example/model-x",
                "review.example",
                "Model X review",
                SourceStatus::Retrieved,
                "In our bench test the Model X measured 70 MHz at the -3 dB point.",
            ),
            source(
                3,
                "https://forum.example/thread/42",
                "forum.example",
                "Forum thread",
                SourceStatus::Inaccessible,
                "",
            ),
        ],
        claims: vec![
            claim(
                1,
                "The Model X has 100 MHz of analog bandwidth",
                ClaimStatus::Contradicted,
                vec![
                    link(
                        1,
                        Relation::Supports,
                        "The Model X has 100 MHz of analog bandwidth",
                    ),
                    link(
                        2,
                        Relation::Contradicts,
                        "the Model X measured 70 MHz at the -3 dB point",
                    ),
                ],
                1,
                1,
            ),
            claim(
                2,
                "The Model X samples at 1 GSa/s",
                ClaimStatus::Supported,
                vec![link(1, Relation::Supports, "a 1 GSa/s sample rate")],
                1,
                0,
            ),
        ],
        contradictions: vec![Contradiction {
            claim_id: 1,
            supporting_sources: vec![1],
            contradicting_sources: vec![2],
        }],
        citations: CitationSummary {
            cited: vec![1, 2],
            unknown_removed: vec![],
            unretrieved_removed: vec![3],
        },
        assessors: vec!["beta".into()],
        budget: ResearchBudget {
            fetch_attempts: 3,
            fetched: 2,
            research_calls: 2,
            claims_reused: 0,
            sources_merged: 0,
        },
        limitations: vec!["One candidate page could not be retrieved.".into()],
    };
    let meta = ConsultationMeta {
        schema: 1,
        mode: Some(ConsultMode::Research),
        strategy: "panel".into(),
        rounds_run: 1,
        research: Some(report),
        unresolved_disagreement: true,
        disagreements: Some("The agents disagree on whether the vendor figure or the bench measurement should be trusted.".into()),
        ..ConsultationMeta::default()
    };
    json!({
        "schema": 1, "id": "20260801-120000-000001", "conversation": 1, "created_at_ms": retrieved_at,
        "question": "Does the Model X really have 100 MHz of bandwidth?", "coordinator": "lead", "participants": ["beta", "gamma"], "skipped": [],
        "rounds_planned": 1, "status": "complete", "synthesized": true,
        "steps": [
            {"phase": "perspective", "round": 0, "agent": "lead", "offset_ms": 0, "duration_ms": 900, "ok": true, "text": "The datasheet says 100 MHz [1].", "error": null, "error_kind": null, "settings": null},
            {"phase": "perspective", "round": 0, "agent": "beta", "offset_ms": 0, "duration_ms": 1200, "ok": true, "text": "A bench test measured 70 MHz [2].", "error": null, "error_kind": null, "settings": null},
            {"phase": "perspective", "round": 0, "agent": "gamma", "offset_ms": 0, "duration_ms": 1500, "ok": false, "text": null, "error": "timed out after 1s", "error_kind": "timeout", "settings": null},
            {"phase": "synthesis", "round": 1, "agent": "lead", "offset_ms": 2000, "duration_ms": 800, "ok": true, "text": "## Answer\n\nThe vendor states 100 MHz [1], but an independent bench test measured 70 MHz [2]; the claim is contradicted.\n\n## Significant disagreements\n\nVendor figure versus bench measurement.", "error": null, "error_kind": null, "settings": null}
        ],
        "initial_proposal": "", "final_answer": "## Answer\n\nThe vendor states 100 MHz [1], but an independent bench test measured 70 MHz [2]; the claim is contradicted.\n\n## Significant disagreements\n\nVendor figure versus bench measurement.",
        "error": null, "calls": 4, "warnings": [], "duration_ms": 2800,
        "consultation": serde_json::to_value(meta).unwrap()
    })
}

const RESEARCH_FIXTURE: &str = include_str!("../web/e2e/fixtures/research-record.json");

#[test]
#[ignore = "regenerates web/e2e/fixtures/research-record.json: cargo test --test web_api regenerate -- --ignored"]
fn regenerate_the_research_fixture() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("web/e2e/fixtures/research-record.json");
    std::fs::write(
        path,
        serde_json::to_string_pretty(&research_record()).unwrap() + "\n",
    )
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_research_endpoint_keeps_retrieved_sources_apart_from_candidates() {
    let s = start().await;
    let fixture: Value = serde_json::from_str(RESEARCH_FIXTURE).unwrap();
    assert_eq!(
        fixture,
        research_record(),
        "the committed fixture matches the real record types; regenerate it with the ignored test"
    );
    let dir = s.dir.path().join("debates");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("20260801-120000-000001.json"), RESEARCH_FIXTURE).unwrap();
    let (st, r) = s
        .get("/api/v1/consultations/20260801-120000-000001/research")
        .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["web_occurred"], true);
    assert_eq!(r["retrieved_sources"], 2);
    let sources = r["sources"].as_array().unwrap();
    assert_eq!(sources[0]["retrieved"], true);
    assert!(
        sources[0]["provenance"]
            .as_str()
            .unwrap()
            .contains("retrieved by AI Orchestra itself")
    );
    assert_eq!(sources[2]["retrieved"], false);
    assert!(
        sources[2]["provenance"]
            .as_str()
            .unwrap()
            .contains("NOT retrieved: it is not evidence")
    );
    assert!(sources[0]["age_days"].as_u64().is_some());
    let c1 = &r["claims"][0];
    assert_eq!(
        c1["status"], "CONTRADICTED",
        "computed from the evidence links"
    );
    assert_eq!(c1["evidence"].as_array().unwrap().len(), 2);
    assert!(
        r["judgement_note"]
            .as_str()
            .unwrap()
            .contains("model-dependent")
    );
    assert!(
        r["freshness_note"]
            .as_str()
            .unwrap()
            .contains("Nothing is re-checked")
    );
    let (_, one) = s.get("/api/v1/consultations/20260801-120000-000001").await;
    assert_eq!(one["consultation"]["research"]["retrieved"], 2);
    assert!(
        one["consultation"]["meta"].get("research").is_none(),
        "the research record has its own endpoint"
    );
    let (_, tl) = s
        .get("/api/v1/consultations/20260801-120000-000001/theater")
        .await;
    let failed = tl["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["ok"] == false)
        .count();
    assert_eq!(failed, 1, "the timed-out call is shown as a failure");
    assert_eq!(s.calls(), 0);
    s.stop().await;
}

// ------------------------------------------------------------- characters, language, summary, usage

fn prompts_of(s: &Server) -> Vec<String> {
    s.agents
        .iter()
        .flat_map(|a| a.requests().into_iter().map(|r| r.prompt))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn characters_validate_persist_and_start_from_sensible_defaults() {
    let s = start().await;
    let (st, g) = s.get("/api/v1/characters").await;
    assert_eq!(st, 200);
    assert_eq!(
        (
            g["settings"]["reply_language"].clone(),
            g["settings"]["conversational"].clone()
        ),
        (json!("auto"), json!(true))
    );
    assert!(
        g["note"]
            .as_str()
            .unwrap()
            .contains("cannot change what an agent is allowed to do")
    );
    let put = |body: Value| s.call(Method::PUT, "/api/v1/characters", Some(body));
    let cases = [
        (
            json!({"characters": {"ghost": {"name": "x"}}}),
            "characters.ghost",
        ),
        (
            json!({"characters": {"lead": {"name": "n".repeat(25)}}}),
            "characters.lead",
        ),
        (
            json!({"characters": {"lead": {"personality": "p".repeat(601)}}}),
            "characters.lead",
        ),
        (
            json!({"characters": {"lead": {"appearance": {"palette": 3}}}}),
            "characters.lead.appearance.palette",
        ),
        (
            json!({"characters": {"lead": {"appearance": {"accessory": "jetpack"}}}}),
            "characters.lead.appearance.accessory",
        ),
        (json!({"reply_language": "klingon"}), "reply_language"),
    ];
    for (body, field) in cases {
        let (st, v) = put(body.clone()).await;
        assert_eq!(
            (st, v["error"]["field"].clone()),
            (422, json!(field)),
            "{body}: {v}"
        );
    }
    let (st, _) = put(json!({"surprise": 1})).await;
    assert_eq!(st, 422, "unknown fields are refused");
    let (st, v) = put(json!({"reply_language": "es", "conversational": false, "characters": {"lead": {"name": "  Ámbar \n", "personality": "Cautelosa", "appearance": {"palette": 2, "accessory": "glasses"}}}})).await;
    assert_eq!(st, 200, "{v}");
    let (_, g) = s.get("/api/v1/characters").await;
    assert_eq!(
        g["settings"]["characters"]["lead"]["name"], "Ámbar",
        "cleaned"
    );
    assert_eq!(
        g["settings"]["characters"]["lead"]["appearance"],
        json!({"palette": 2, "accessory": "glasses"})
    );
    assert_eq!(g["settings"]["reply_language"], "es");
    let (st, _) = s.post("/api/v1/characters", json!({})).await;
    assert_eq!(st, 405);
    assert_eq!(s.calls(), 0);
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_reply_language_rule_is_always_in_the_prompts_that_write_prose() {
    let s = start().await;
    let (_, done) = run_panel(&s).await;
    assert_eq!(done["job"]["status"], "completed");
    let prompts = prompts_of(&s);
    assert!(prompts.len() >= 6);
    assert!(
        prompts
            .iter()
            .all(|p| p.starts_with("<style kind=\"user_preferences\">")
                && p.contains("same language as the text inside <question>")),
        "every call that writes prose carries the language rule (what stops answers mixing two languages)"
    );
    assert!(
        done["consultation"]["meta"]["style"].is_object()
            || done["consultation"]["meta"]["style"].is_null()
    );
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_character_reaches_its_own_prompts_and_the_reviewers_know_the_names_but_first_answers_do_not()
 {
    let s = start().await;
    let (st, _) = s
        .call(
            Method::PUT,
            "/api/v1/characters",
            Some(json!({"reply_language": "es", "conversational": true, "characters": {"lead": {"name": "Ámbar", "personality": "Cautelosa y metódica"}, "beta": {"name": "Circuito"}}})),
        )
        .await;
    assert_eq!(st, 200);
    let (_, done) = run_panel(&s).await;
    assert_eq!(done["job"]["status"], "completed", "{done}");
    let lead: Vec<String> = s.agents[0]
        .requests()
        .into_iter()
        .map(|r| r.prompt)
        .collect();
    assert!(
        lead.iter().all(|p| p.contains("\"Ámbar\"")
            && p.contains("Cautelosa y metódica")
            && p.contains("Write in Spanish")),
        "{lead:?}"
    );
    let beta: Vec<String> = s.agents[1]
        .requests()
        .into_iter()
        .map(|r| r.prompt)
        .collect();
    assert!(
        beta.iter()
            .all(|p| p.contains("\"Circuito\"") && !p.contains("Cautelosa")),
        "a profile goes only to its own agent"
    );
    // The independent first answer is the first call of each agent: it never mentions the other participants.
    for a in &s.agents {
        let first = &a.requests()[0].prompt;
        assert!(
            !first.contains("other participants"),
            "an independent answer is never told who else takes part: {first}"
        );
        assert!(
            !first.contains("colleagues in a meeting"),
            "independent answers are not told to talk to anyone"
        );
    }
    let reviews: Vec<String> = prompts_of(&s)
        .into_iter()
        .filter(|p| p.contains("colleagues in a meeting"))
        .collect();
    assert!(
        !reviews.is_empty(),
        "reviewers are asked to speak to the others"
    );
    assert!(
        reviews
            .iter()
            .any(|p| p.contains("lead = \"Ámbar\"") || p.contains("beta = \"Circuito\"")),
        "{reviews:?}"
    );
    let style = &done["consultation"]["meta"]["style"];
    assert_eq!(style["language"], "es");
    assert_eq!(
        style["characters"],
        json!(["beta", "lead"]),
        "the record says who had a character, not the profile text"
    );
    assert!(
        !done["consultation"]
            .to_string()
            .contains("Cautelosa y metódica"),
        "the profile text is not copied into the record"
    );
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_summary_is_one_explicit_call_to_the_coordinator_and_is_saved_with_its_author() {
    let s = start().await;
    let (_, done) = run_panel(&s).await;
    let id = done["consultation"]["id"].as_str().unwrap().to_string();
    let before = s.calls();
    let coordinator_before = s.agents[0].requests().len();
    let (st, v) = s
        .post(&format!("/api/v1/consultations/{id}/summary"), json!({}))
        .await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(s.calls(), before + 1, "exactly one more call");
    assert_eq!(
        s.agents[0].requests().len(),
        coordinator_before + 1,
        "made to the coordinator"
    );
    assert_eq!(v["summary"]["agent"], "lead");
    assert_eq!(v["summary"]["origin"], "ai");
    assert!(v["summary"]["text"].as_str().unwrap().len() > 3);
    assert!(v["note"].as_str().unwrap().contains("not a check"));
    let all = s.agents[0].requests();
    let prompt = &all.last().unwrap().prompt;
    assert!(
        prompt.contains("<report status=\"complete\">")
            && prompt.contains("at most 180 words")
            && prompt.contains("Do not run commands"),
        "{prompt}"
    );
    // It is stored with the consultation and shown when it is opened again.
    let (_, saved) = s.get(&format!("/api/v1/consultations/{id}")).await;
    assert_eq!(
        saved["consultation"]["summaries"].as_array().unwrap().len(),
        1
    );
    let on_disk =
        std::fs::read_to_string(s.dir.path().join("debates").join(format!("{id}.json"))).unwrap();
    assert!(on_disk.contains("\"summaries\"") && on_disk.contains("\"origin\": \"ai\""));
    // A named participant may write it; someone who did not take part may not.
    let (st, v) = s
        .post(
            &format!("/api/v1/consultations/{id}/summary"),
            json!({"agent": "beta"}),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["summary"]["agent"], "beta");
    let (st, v) = s
        .post(
            &format!("/api/v1/consultations/{id}/summary"),
            json!({"agent": "ghost"}),
        )
        .await;
    assert_eq!((st, v["error"]["field"].clone()), (422, json!("agent")));
    let (st, _) = s
        .post(
            "/api/v1/consultations/20250101-000000-nope/summary",
            json!({}),
        )
        .await;
    assert_eq!(st, 404);
    let (_, saved) = s.get(&format!("/api/v1/consultations/{id}")).await;
    assert_eq!(
        saved["consultation"]["summaries"].as_array().unwrap().len(),
        2
    );
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_summary_is_never_started_twice_and_never_for_a_running_consultation() {
    let slow = Behavior {
        delay_ms: 700,
        ..Behavior::normal("alpha")
    };
    let s = start_with(
        vec![
            ("lead", slow.clone()),
            ("beta", slow.clone()),
            ("gamma", slow),
        ],
        |_| {},
    )
    .await;
    let (_, running) = s.post("/api/v1/consultations", panel()).await;
    let job = running["job_id"].as_str().unwrap().to_string();
    let (st, v) = s
        .post(&format!("/api/v1/consultations/{job}/summary"), json!({}))
        .await;
    assert_eq!(
        (st, v["error"]["kind"].clone()),
        (409, json!("conflict")),
        "wait for the consultation to finish: {v}"
    );
    let done = s.wait_finished(&job).await;
    let id = done["consultation"]["id"].as_str().unwrap().to_string();
    let url = format!("/api/v1/consultations/{id}/summary");
    let before = s.calls();
    let (a, b) = tokio::join!(s.post(&url, json!({})), s.post(&url, json!({})));
    let mut codes = [a.0, b.0];
    codes.sort_unstable();
    assert_eq!(codes, [200, 409], "{a:?} {b:?}");
    assert_eq!(s.calls(), before + 1, "a double click makes one call");
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn leaving_during_a_summary_releases_it_and_a_new_request_works() {
    let s = start().await;
    let (_, done) = run_panel(&s).await;
    let id = done["consultation"]["id"].as_str().unwrap().to_string();
    // A client that gives up at once: the request must still be released, not left marked as busy.
    let impatient = Client::builder()
        .no_proxy()
        .timeout(Duration::from_millis(1))
        .build()
        .unwrap();
    let _ = impatient
        .post(format!("{}/api/v1/consultations/{id}/summary", s.base))
        .header("x-aio-token", &s.state.token)
        .header("content-type", "application/json")
        .body("{}")
        .send()
        .await;
    let deadline = Instant::now() + Duration::from_secs(5);
    while !s.state.summarizing.lock().unwrap().is_empty() {
        assert!(Instant::now() < deadline, "the request was released");
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    let (st, v) = s
        .post(&format!("/api/v1/consultations/{id}/summary"), json!({}))
        .await;
    assert_eq!(st, 200, "{v}");
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn usage_by_agent_comes_only_from_what_the_clis_reported_and_quota_is_not_invented() {
    let s = start().await;
    let now = ai_orchestra::history::now_ms();
    let dir = s.dir.path().join("debates");
    std::fs::create_dir_all(&dir).unwrap();
    let step = |agent: &str, offset: u64, usage: Value, err: Option<&str>| {
        json!({"phase": "perspective", "round": 0, "agent": agent, "offset_ms": offset, "duration_ms": 100, "ok": err.is_none(), "text": if err.is_none() { json!("t") } else { Value::Null },
               "error": err, "error_kind": err.map(|_| "quota"), "settings": null, "usage": usage})
    };
    let record = |id: &str, created: u64, steps: Vec<Value>| {
        json!({"schema": 1, "id": id, "conversation": 1, "created_at_ms": created, "question": "q", "coordinator": "claude", "participants": ["codex"], "skipped": [],
               "rounds_planned": 1, "status": "complete", "synthesized": true, "steps": steps, "initial_proposal": "", "final_answer": "a", "error": null, "calls": 3, "warnings": [], "duration_ms": 10})
    };
    let recent = record(
        "20261012-000000-000001",
        now - 3_600_000,
        vec![
            step(
                "claude",
                0,
                json!({"input_tokens": 100, "output_tokens": 40}),
                None,
            ),
            step(
                "codex",
                0,
                json!({"input_tokens": 10, "output_tokens": 5, "cached_input_tokens": 4}),
                None,
            ),
            step("google", 0, Value::Null, None),
        ],
    );
    let old = record(
        "20261001-000000-000001",
        now - 3 * 86_400_000,
        vec![
            step(
                "claude",
                0,
                json!({"input_tokens": 1000, "output_tokens": 500}),
                None,
            ),
            step("codex", 50, Value::Null, Some("usage limit reached")),
        ],
    );
    std::fs::write(dir.join("20261012-000000-000001.json"), recent.to_string()).unwrap();
    std::fs::write(dir.join("20261001-000000-000001.json"), old.to_string()).unwrap();
    let (st, a) = s.get("/api/v1/analytics").await;
    assert_eq!(st, 200, "{a}");
    let u = &a["usage_by_agent"];
    assert_eq!(
        u["claude"]["total"],
        json!({"calls": 2, "calls_reporting_tokens": 2, "input": 1100, "output": 540})
    );
    assert_eq!(
        u["claude"]["last_5_hours"]["input"], 100,
        "only the recent call is in the last five hours"
    );
    assert_eq!(u["claude"]["last_7_days"]["input"], 1100);
    assert_eq!(u["codex"]["cached_input_tokens"], 4);
    assert_eq!(u["codex"]["quota_errors"], 1);
    assert!(u["codex"]["last_quota_error_ms"].as_u64().is_some());
    assert_eq!(
        u["google"]["total"],
        json!({"calls": 1, "calls_reporting_tokens": 0, "input": 0, "output": 0}),
        "a CLI that reports nothing shows no tokens, not zeros presented as usage"
    );
    assert_eq!(a["quota"]["remaining_known"], false);
    assert!(a["quota"]["note"].as_str().unwrap().contains("not exposed"));
    assert!(!u["claude"]["by_day"].as_object().unwrap().is_empty());
    let (_, tl) = s
        .get("/api/v1/consultations/20261012-000000-000001/theater")
        .await;
    assert_eq!(
        tl["messages"][0]["usage"]["input_tokens"], 100,
        "each message carries the tokens its CLI reported"
    );
    assert!(tl["messages"][2]["usage"].is_null());
    s.stop().await;
}
