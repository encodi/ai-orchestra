//! End-to-end MCP tests: the real `ai-orchestra mcp serve` process, driven by a
//! real `rmcp` client and by raw JSON-RPC over pipes, with fake agent CLIs
//! (shell scripts). No real agent is contacted and no quota is used.

#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::transport::TokioChildProcess;
use serde_json::{Value, json};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_ai-orchestra");

/// Role from marker phrases in the prompt, as in the CLI debate tests.
const ROLE_CASE: &str = r#"
case "$prompt" in
  *"FINAL synthesis"*) out="SYNTHESIS by NAME";;
  *"cross-review round"*) out="CROSS-REVIEW from NAME";;
  *"independent review"*) out="REVIEW from NAME";;
  *) out="PROPOSAL from NAME";;
esac
echo "$@" >> "$MOCK_DIR/NAME.calls"
echo "depth=${AI_ORCHESTRA_DEPTH:-unset} active=${AI_ORCHESTRA_ACTIVE:-unset}" >> "$MOCK_DIR/NAME.env"
[ -f "$MOCK_DIR/NAME.slow" ] && sleep 30
"#;

fn script(name: &str, kind: &str) -> String {
    let role = ROLE_CASE.replace("NAME", name);
    match kind {
        "claude" => format!(
            r#"#!/bin/sh
case "$1" in --version) echo "claude-fake 1.2.3"; exit 0;; auth) echo '{{"loggedIn":true}}'; exit 0;; esac
prompt=$(cat)
{role}
printf '{{"type":"result","is_error":false,"result":"%s"}}\n' "$out"
"#
        ),
        "codex" => format!(
            r#"#!/bin/sh
case "$1" in --version) echo "codex-fake 4.5.6"; exit 0;; login) echo "Logged in using ChatGPT"; exit 0;; esac
prompt=$(cat)
{role}
printf '{{"type":"item.completed","item":{{"type":"agent_message","text":"%s"}}}}\n' "$out"
"#
        ),
        _ => format!(
            r#"#!/bin/sh
case "$1" in --version) echo "agy-fake 7.8.9"; exit 0;; esac
prompt=""
while [ $# -gt 0 ]; do [ "$1" = "-p" ] && prompt="$2"; shift; done
{role}
printf '{{"status":"SUCCESS","response":"%s"}}\n' "$out"
"#
        ),
    }
}

struct Fx {
    dir: TempDir,
    config: PathBuf,
}

impl Fx {
    fn new() -> Self {
        Self::with_config("")
    }

    fn with_config(extra: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        fs::create_dir(&bin).unwrap();
        for (name, kind) in [("claude", "claude"), ("codex", "codex"), ("agy", "agy")] {
            let path = bin.join(name);
            fs::write(&path, script(name, kind)).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            // macOS scans a fresh executable on first launch; do it outside any timed step.
            let _ = Command::new(&path).arg("--version").output();
        }
        let config = dir.path().join("config.toml");
        fs::write(
            &config,
            format!(
                "{extra}\n[smart]\ndefault_strategy = \"off\"\n[memory]\ndefault = \"off\"\n[providers.claude]\nexecutable = \"{}\"\n[providers.codex]\nexecutable = \"{}\"\n[providers.google]\nexecutable = \"{}\"\n",
                bin.join("claude").display(),
                bin.join("codex").display(),
                bin.join("agy").display()
            ),
        )
        .unwrap();
        fs::create_dir(dir.path().join("home")).unwrap();
        Self { dir, config }
    }

    fn apply_env<'a>(&self, cmd: &'a mut Command) -> &'a mut Command {
        cmd.env_remove("CLAUDECODE")
            .env_remove("AI_ORCHESTRA_ACTIVE")
            .env_remove("AI_ORCHESTRA_DEPTH")
            .env_remove("CODEX_THREAD_ID")
            .env_remove("CODEX_SESSION_ID")
            .env_remove("CODEX_SANDBOX")
            .env_remove("CODEX_SHELL")
            .env("HOME", self.dir.path().join("home"))
            .env("XDG_DATA_HOME", self.dir.path().join("data"))
            .env("MOCK_DIR", self.dir.path())
    }

    fn serve_command(&self, host: &str) -> Command {
        let mut cmd = Command::new(BIN);
        cmd.arg("--config")
            .arg(&self.config)
            .args(["mcp", "serve", "--host", host]);
        self.apply_env(&mut cmd);
        cmd
    }

    fn calls(&self, name: &str) -> String {
        fs::read_to_string(self.dir.path().join(format!("{name}.calls"))).unwrap_or_default()
    }

    fn agent_env(&self, name: &str) -> String {
        fs::read_to_string(self.dir.path().join(format!("{name}.env"))).unwrap_or_default()
    }
}

fn data(r: &CallToolResult) -> Value {
    r.structured_content.clone().expect("structured content")
}

async fn call(
    client: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
    name: &str,
    args: Value,
) -> CallToolResult {
    client
        .call_tool(
            CallToolRequestParams::new(name.to_string())
                .with_arguments(args.as_object().cloned().unwrap_or_default()),
        )
        .await
        .expect("tool call")
}

async fn connect(fx: &Fx, host: &str) -> rmcp::service::RunningService<rmcp::RoleClient, ()> {
    let mut std_cmd = fx.serve_command(host);
    let cmd: tokio::process::Command = std::mem::replace(&mut std_cmd, Command::new("true")).into();
    let (transport, _stderr) = TokioChildProcess::builder(cmd)
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the server");
    ().serve(transport)
        .await
        .expect("initialize over the real binary")
}

// -------------------------------------------------------------------------------------------
// Real client, real binary

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_real_client_uses_the_real_server_end_to_end() {
    let fx = Fx::new();
    let client = connect(&fx, "claude").await;

    let tools = client.list_all_tools().await.unwrap();
    let mut names: Vec<String> = tools.iter().map(|t| t.name.to_string()).collect();
    names.sort();
    assert_eq!(
        names,
        [
            "ask_agent",
            "cancel_task",
            "consult",
            "debate",
            "explain_context",
            "forget_memory",
            "get_debate",
            "get_eval_report",
            "get_memory",
            "get_research_report",
            "get_session_context",
            "get_task_status",
            "inspect_sources",
            "list_agents",
            "research",
            "save_memory",
            "search_memory",
            "submit_feedback",
            "verify_claim"
        ]
    );

    // list_agents: versions come from local --version calls; the host is marked and not callable.
    let agents = data(&call(&client, "list_agents", json!({})).await);
    assert_eq!(agents["host"], "claude");
    let agent = |id: &str| {
        agents["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["id"] == id)
            .unwrap()
            .clone()
    };
    assert_eq!(agent("codex")["version"], "codex-fake 4.5.6");
    assert_eq!(agent("google")["version"], "agy-fake 7.8.9");
    assert_eq!(agent("claude")["callable"], false);
    assert_eq!(agent("codex")["callable"], true);
    assert_eq!(fx.calls("codex"), "", "listing never sends a prompt");

    // ask_agent reaches the real adapter and the fake CLI, read-only, isolated, one level deeper.
    let ask = data(
        &call(
            &client,
            "ask_agent",
            json!({"provider": "codex", "prompt": "hello"}),
        )
        .await,
    );
    assert_eq!(
        (ask["status"].as_str(), ask["ok"].as_bool()),
        (Some("completed"), Some(true)),
        "{ask}"
    );
    assert_eq!(ask["text"], "PROPOSAL from codex");
    let args = fx.calls("codex");
    assert!(
        args.contains("--sandbox read-only") && args.contains("--ignore-user-config"),
        "{args}"
    );
    assert_eq!(
        fx.agent_env("codex").trim(),
        "depth=1 active=1",
        "children run one level deeper"
    );

    // The host is refused.
    let blocked = call(
        &client,
        "ask_agent",
        json!({"provider": "claude", "prompt": "hi"}),
    )
    .await;
    assert_eq!(blocked.is_error, Some(true));
    assert_eq!(data(&blocked)["error"]["kind"], "host_agent_blocked");
    assert_eq!(fx.calls("claude"), "", "the host was never started");

    // A debate: the host is excluded, so another agent coordinates and the third reviews.
    let debate = data(
        &call(
            &client,
            "debate",
            json!({"question": "Which design?", "wait_seconds": 50}),
        )
        .await,
    );
    assert_eq!(debate["debate_status"], "complete", "{debate}");
    assert_eq!(debate["coordinator"], "codex");
    assert_eq!(debate["participants"], json!(["google"]));
    assert_eq!(debate["final_synthesis"], "SYNTHESIS by codex");
    assert_eq!(debate["calls"], 3);
    assert_eq!(fx.calls("claude"), "");
    let id = debate["debate_id"].as_str().unwrap().to_string();

    // The same record is retrievable through MCP and through the CLI.
    let got = data(
        &call(
            &client,
            "get_debate",
            json!({"debate_id": id, "include_transcript": true}),
        )
        .await,
    );
    assert_eq!(got["final_synthesis"], "SYNTHESIS by codex");
    assert_eq!(got["transcript"].as_array().unwrap().len(), 3);
    let mut show = Command::new(BIN);
    show.arg("--config").arg(&fx.config).args(["show", &id]);
    fx.apply_env(&mut show);
    let shown = show.output().unwrap();
    assert!(
        shown.status.success()
            && String::from_utf8_lossy(&shown.stdout).contains("SYNTHESIS by codex")
    );

    client.cancel().await.ok();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn children_are_started_in_isolated_mode_where_their_cli_supports_it() {
    let fx = Fx::new();
    let client = connect(&fx, "google").await; // Google hosts; claude and codex are callable
    for provider in ["claude", "codex"] {
        let v = data(
            &call(
                &client,
                "ask_agent",
                json!({"provider": provider, "prompt": "x"}),
            )
            .await,
        );
        assert_eq!(v["status"], "completed", "{provider}: {v}");
    }
    // Claude: --strict-mcp-config is always passed, so it loads no MCP servers.
    assert!(
        fx.calls("claude").contains("--strict-mcp-config"),
        "{}",
        fx.calls("claude")
    );
    // Codex: --ignore-user-config (its documented way not to load config.toml).
    assert!(fx.calls("codex").contains("--ignore-user-config"));
    // Google (agy) has no such option; the depth limit is the safeguard (see the next tests).
    assert_eq!(fx.calls("agy"), "");
    client.cancel().await.ok();

    // The operator can turn isolation off.
    let fx = Fx::with_config("[mcp]\nisolate_children = false\n");
    let client = connect(&fx, "google").await;
    call(
        &client,
        "ask_agent",
        json!({"provider": "codex", "prompt": "x"}),
    )
    .await;
    assert!(!fx.calls("codex").contains("--ignore-user-config"));
    client.cancel().await.ok();
}

// -------------------------------------------------------------------------------------------
// Raw JSON-RPC: protocol purity, shutdown and depth limits

struct Raw {
    child: Child,
    lines: mpsc::Receiver<String>,
}

impl Raw {
    fn start(mut cmd: Command) -> Self {
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(|l| l.ok()) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Self { child, lines: rx }
    }

    fn send(&mut self, v: Value) {
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{v}").unwrap();
        stdin.flush().unwrap();
    }

    fn recv(&self) -> String {
        self.lines
            .recv_timeout(Duration::from_secs(30))
            .expect("a line on stdout")
    }

    fn handshake(&mut self) -> Vec<String> {
        self.send(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"raw","version":"0"}}}));
        let first = self.recv();
        self.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        vec![first]
    }

    /// Closes stdin, waits for exit and returns every stdout line that was not read yet.
    fn finish(mut self) -> (Vec<String>, std::process::ExitStatus, String) {
        drop(self.child.stdin.take());
        let status = self.child.wait().unwrap();
        let rest: Vec<String> = self.lines.try_iter().collect();
        let mut stderr = String::new();
        if let Some(mut e) = self.child.stderr.take() {
            std::io::Read::read_to_string(&mut e, &mut stderr).unwrap();
        }
        (rest, status, stderr)
    }
}

fn assert_protocol_line(line: &str) -> Value {
    let v: Value = serde_json::from_str(line)
        .unwrap_or_else(|e| panic!("stdout line is not JSON ({e}): {line:?}"));
    assert_eq!(v["jsonrpc"], "2.0", "not a JSON-RPC message: {line}");
    assert!(
        v.get("id").is_some() || v.get("method").is_some(),
        "neither a response nor a notification: {line}"
    );
    v
}

#[test]
fn stdout_carries_only_protocol_messages_even_with_verbose_logging() {
    let fx = Fx::new();
    let mut cmd = fx.serve_command("claude");
    cmd.args(["-vv"]).env("RUST_LOG", "debug");
    let mut raw = Raw::start(cmd);
    let mut seen = raw.handshake();
    raw.send(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}));
    seen.push(raw.recv());
    raw.send(json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"list_agents","arguments":{}}}));
    seen.push(raw.recv());
    raw.send(json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"ask_agent","arguments":{"provider":"codex","prompt":"hello"}}}));
    seen.push(raw.recv());
    // A malformed request and an unknown method must also answer in protocol form.
    raw.send(json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"ask_agent","arguments":{"provider":42}}}));
    seen.push(raw.recv());
    raw.send(json!({"jsonrpc":"2.0","id":6,"method":"no/such/method"}));
    seen.push(raw.recv());
    let (rest, status, stderr) = raw.finish();
    seen.extend(rest);

    let messages: Vec<Value> = seen.iter().map(|l| assert_protocol_line(l)).collect();
    assert_eq!(messages[0]["result"]["serverInfo"]["name"], "ai-orchestra");
    assert_eq!(messages[1]["result"]["tools"].as_array().unwrap().len(), 19);
    assert_eq!(
        messages[3]["result"]["structuredContent"]["text"],
        "PROPOSAL from codex"
    );
    assert!(
        messages[5]["error"].is_object(),
        "unknown methods get a JSON-RPC error: {}",
        messages[5]
    );
    assert!(
        status.success(),
        "a closed stdin is a clean shutdown: {status:?} {stderr}"
    );
    // Logging happened, and all of it went to stderr.
    assert!(
        !stderr.is_empty(),
        "-vv should produce diagnostics on stderr"
    );
    assert!(
        !seen
            .iter()
            .any(|l| l.contains("INFO") || l.contains("DEBUG")),
        "log lines leaked to stdout"
    );
}

#[test]
fn the_server_refuses_to_start_inside_an_agent_that_ai_orchestra_started() {
    let fx = Fx::new();
    for (var, value) in [("AI_ORCHESTRA_DEPTH", "1"), ("AI_ORCHESTRA_ACTIVE", "1")] {
        let mut cmd = fx.serve_command("claude");
        cmd.env(var, value);
        let out = cmd.stdin(Stdio::null()).output().unwrap();
        assert_eq!(out.status.code(), Some(1), "{var}");
        assert!(
            out.stdout.is_empty(),
            "nothing on stdout when refusing: {:?}",
            String::from_utf8_lossy(&out.stdout)
        );
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("recursive agent call") && err.contains(var),
            "{err}"
        );
    }
    // A deeper limit is a deliberate operator choice and is honoured.
    let fx = Fx::with_config("[general]\nmax_depth = 2\n");
    let mut cmd = fx.serve_command("claude");
    cmd.env("AI_ORCHESTRA_DEPTH", "1");
    let mut raw = Raw::start(cmd);
    let first = raw.handshake();
    assert_protocol_line(&first[0]);
    let (_, status, _) = raw.finish();
    assert!(status.success());
    // ...but not beyond it.
    let mut cmd = fx.serve_command("claude");
    cmd.env("AI_ORCHESTRA_DEPTH", "2");
    assert_eq!(
        cmd.stdin(Stdio::null()).output().unwrap().status.code(),
        Some(1)
    );
}

#[test]
fn sigterm_shuts_the_server_down_gracefully_and_kills_running_agents() {
    let fx = Fx::new();
    fs::write(fx.dir.path().join("codex.slow"), "").unwrap();
    let mut raw = Raw::start(fx.serve_command("claude"));
    raw.handshake();
    raw.send(json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"ask_agent","arguments":{"provider":"codex","prompt":"slow","wait_seconds":0}}}));
    let started = assert_protocol_line(&raw.recv());
    assert!(
        matches!(
            started["result"]["structuredContent"]["status"].as_str(),
            Some("pending" | "running")
        ),
        "{started}"
    );

    // Wait until the fake agent is really running before interrupting.
    let marker = fx.dir.path().join("codex.env");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !marker.exists() {
        assert!(Instant::now() < deadline, "the agent never started");
        std::thread::sleep(Duration::from_millis(50));
    }
    let pid = raw.child.id().to_string();
    assert!(
        Command::new("kill")
            .args(["-TERM", &pid])
            .status()
            .unwrap()
            .success()
    );
    let began = Instant::now();
    let status = raw.child.wait().unwrap();
    assert!(
        status.success(),
        "SIGTERM is a graceful shutdown (exit 0), got {status:?}"
    );
    assert!(
        began.elapsed() < Duration::from_secs(25),
        "shutdown took {:?}",
        began.elapsed()
    );

    // The agent process (and its sleep) were terminated, not orphaned.
    std::thread::sleep(Duration::from_millis(300));
    let leftover = Command::new("pgrep")
        .args(["-f", &format!("{}/bin/codex", fx.dir.path().display())])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&leftover.stdout).trim().is_empty(),
        "leftover agent: {:?}",
        leftover.stdout
    );
}

// -------------------------------------------------------------------------------------------
// Setup instructions

#[test]
fn setup_instructions_print_the_verified_commands_and_change_nothing() {
    let fx = Fx::new();
    let before: Vec<_> = fs::read_dir(fx.dir.path().join("home")).unwrap().collect();
    let run = |host: &str| {
        let mut cmd = Command::new(BIN);
        cmd.arg("--config")
            .arg(&fx.config)
            .args(["mcp", "config", host]);
        fx.apply_env(&mut cmd);
        cmd.output().unwrap()
    };
    let bin: &Path = Path::new(BIN);
    let cfg = fs::canonicalize(&fx.config).unwrap();
    let expect = |host: &str, add: String, list: &str, remove: &str| {
        let out = run(host);
        assert!(out.status.success(), "{host}");
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        for needle in [add.as_str(), list, remove, "Nothing has been registered"] {
            assert!(
                text.contains(needle),
                "{host}: missing {needle:?} in:\n{text}"
            );
        }
    };
    expect(
        "claude",
        format!(
            "claude mcp add --scope user ai-orchestra -- {} mcp serve --host claude --config {}",
            bin.display(),
            cfg.display()
        ),
        "claude mcp list",
        "claude mcp remove ai-orchestra",
    );
    expect(
        "codex",
        format!(
            "codex mcp add ai-orchestra -- {} mcp serve --host codex --config {}",
            bin.display(),
            cfg.display()
        ),
        "codex mcp list",
        "codex mcp remove ai-orchestra",
    );
    expect(
        "google",
        format!(
            "agy mcp add ai-orchestra -- {} mcp serve --host google --config {}",
            bin.display(),
            cfg.display()
        ),
        "agy mcp list",
        "agy mcp remove ai-orchestra",
    );
    assert!(
        String::from_utf8_lossy(&run("agy").stdout).contains("--host google"),
        "aliases work"
    );
    assert_eq!(run("gemini").status.code(), Some(2));
    let after: Vec<_> = fs::read_dir(fx.dir.path().join("home")).unwrap().collect();
    assert_eq!(
        before.len(),
        after.len(),
        "printing instructions must not create or change any file"
    );
}

#[test]
fn the_server_starts_without_a_home_directory_and_says_debates_are_not_saved() {
    // Some hosts start MCP servers with a minimal environment.
    let fx = Fx::new();
    let mut cmd = fx.serve_command("claude");
    cmd.env_remove("HOME").env_remove("XDG_DATA_HOME");
    let mut raw = Raw::start(cmd);
    let first = raw.handshake();
    assert_protocol_line(&first[0]);
    raw.send(json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"get_debate","arguments":{"debate_id":"20260101"}}}));
    let reply = assert_protocol_line(&raw.recv());
    assert_eq!(
        reply["result"]["structuredContent"]["error"]["kind"], "history_disabled",
        "{reply}"
    );
    let (_, status, stderr) = raw.finish();
    assert!(status.success(), "{stderr}");
    assert!(
        stderr.contains("will not be saved"),
        "the reason is logged to stderr: {stderr}"
    );
}

#[tokio::test]
async fn consult_through_the_real_binary_skips_the_host_and_runs_agents_isolated() {
    let fx = Fx::new();
    let client = connect(&fx, "claude").await;
    let v = data(
        &call(
            &client,
            "consult",
            json!({"question": "Which design should we use?", "wait_seconds": 50}),
        )
        .await,
    );
    assert_eq!(v["status"], "completed", "{v}");
    assert_eq!(v["consultation_status"], "complete", "{v}");
    // The host (claude) is the configured coordinator but is never started: codex coordinates.
    assert_eq!(v["coordinator"], "codex");
    assert_eq!(v["participants"], json!(["codex", "google"]));
    assert_eq!(v["calls"], 2 + 2 + 1);
    assert_eq!(fx.calls("claude"), "", "the host was never started");
    assert_eq!(fx.calls("codex").lines().count(), 3);
    // The fake agy logs after consuming its arguments, so there is one (empty) line per call.
    assert_eq!(fx.calls("agy").lines().count(), 2);
    let codex_args = fx.calls("codex");
    assert!(
        codex_args.contains("--sandbox read-only") && codex_args.contains("--ignore-user-config"),
        "{codex_args}"
    );
    for line in fx.agent_env("codex").lines() {
        assert_eq!(line, "depth=1 active=1", "children run one level deeper");
    }
    // Asking for the host as coordinator is refused.
    let r = call(
        &client,
        "consult",
        json!({"question": "q", "coordinator": "claude"}),
    )
    .await;
    assert_eq!(r.is_error, Some(true));
    assert_eq!(data(&r)["error"]["kind"], "host_agent_blocked");
    assert_eq!(fx.calls("claude"), "");
}
