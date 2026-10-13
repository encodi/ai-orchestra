//! End-to-end tests of the real `ai-orchestra` binary against fake agent CLIs
//! (shell scripts). No real provider is ever contacted.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_ai-orchestra");

/// Fake `claude`: understands `--version`, `auth status` and `-p` with the
/// prompt on stdin. Behavior is selected by the prompt text.
const MOCK_CLAUDE: &str = r#"#!/bin/sh
case "$1" in
  --version) echo "9.9.9 (Mock Claude)"; exit 0;;
  auth) echo '{"loggedIn":true,"authMethod":"claude.ai","subscriptionType":"pro","email":"person@example.com"}'; exit 0;;
esac
prompt=$(cat)
echo "$@" > "$MOCK_DIR/claude.args"
echo "${AI_ORCHESTRA_ACTIVE:-unset}" > "$MOCK_DIR/claude.active"
case "$prompt" in
  FAIL_AUTH) echo '{"type":"result","subtype":"success","is_error":true,"result":"Invalid API key - Please run /login"}'; exit 1;;
  FAIL_QUOTA) echo "Claude AI usage limit reached" >&2; exit 1;;
  BADJSON) echo "this is not json"; exit 0;;
  SLEEP) sleep 30; exit 0;;
esac
printf '{"type":"result","subtype":"success","is_error":false,"result":"claude saw: %s | key=%s","session_id":"s1","usage":{"input_tokens":3,"output_tokens":4},"modelUsage":{"claude-haiku-4-5":{"inputTokens":1,"outputTokens":1},"claude-sonnet-5-5":{"inputTokens":5,"outputTokens":7}}}\n' "$prompt" "${ANTHROPIC_API_KEY:-absent}"
"#;

const MOCK_CODEX: &str = r#"#!/bin/sh
case "$1" in
  --version) echo "codex-cli 9.9.9"; exit 0;;
  login) echo "Logged in using ChatGPT"; exit 0;;
esac
prompt=$(cat)
echo "$@" > "$MOCK_DIR/codex.args"
printf '{"type":"thread.started","thread_id":"t1"}\n'
printf '{"type":"item.completed","item":{"id":"1","type":"agent_message","text":"codex saw: %s | key=%s"}}\n' "$prompt" "${OPENAI_API_KEY:-absent}"
printf '{"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":2}}\n'
"#;

/// Fake `agy`: the prompt is the argument after `-p`.
const MOCK_AGY: &str = r#"#!/bin/sh
case "$1" in
  --version) echo "1.9.9"; exit 0;;
esac
echo "$@" > "$MOCK_DIR/agy.args"
prompt=""
while [ $# -gt 0 ]; do
  if [ "$1" = "-p" ]; then prompt="$2"; fi
  shift
done
printf '{"conversation_id":"c1","status":"SUCCESS","response":"agy saw: %s"}\n' "$prompt"
"#;

struct Fixture {
    dir: TempDir,
    config: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        Self::with_executables(None, None, None)
    }

    /// Optionally override the executable path of each provider.
    fn with_executables(claude: Option<&str>, codex: Option<&str>, agy: Option<&str>) -> Self {
        Self::build([MOCK_CLAUDE, MOCK_CODEX, MOCK_AGY], [claude, codex, agy])
    }

    fn with_scripts(scripts: [&str; 3]) -> Self {
        Self::build(scripts, [None, None, None])
    }

    fn build(scripts: [&str; 3], overrides: [Option<&str>; 3]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let names = ["claude", "codex", "agy"];
        for (name, body) in names.iter().zip(scripts) {
            let path = bin.join(name);
            fs::write(&path, body).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        // macOS scans a freshly written executable on its first launch, which can
        // take seconds under load. Launch each once here so that cost never lands
        // inside a timed or timeout-bounded test.
        for name in names {
            let _ = Command::new(bin.join(name)).arg("--version").output();
        }
        let exe = |i: usize| match overrides[i] {
            Some(o) => o.to_string(),
            None => bin.join(names[i]).display().to_string(),
        };
        let config = dir.path().join("config.toml");
        fs::write(
            &config,
            format!(
                // `default_strategy = "off"` keeps the fixed Milestone 4/5 orchestration, which
                // these tests were written for; `--strategy` (and `Fixture::use_default_strategy`)
                // exercises the smart orchestration of Milestone 6.
                "[general]\ntimeout_secs = 20\n\
                 [smart]\ndefault_strategy = \"off\"\n\
                 [memory]\ndefault = \"off\"\n\
                 [providers.claude]\nexecutable = \"{}\"\n\
                 [providers.codex]\nexecutable = \"{}\"\n\
                 [providers.google]\nexecutable = \"{}\"\n",
                exe(0),
                exe(1),
                exe(2)
            ),
        )
        .unwrap();
        Self { dir, config }
    }

    /// Removes the `off` override, so requests that name no strategy get the built-in default.
    fn use_default_strategy(&self) {
        let text = fs::read_to_string(&self.config).unwrap();
        fs::write(
            &self.config,
            text.replace("[smart]\ndefault_strategy = \"off\"\n", ""),
        )
        .unwrap();
    }

    /// Removes the `off` override of memory, so requests get the built-in default (`auto`).
    fn use_memory(&self) {
        let text = fs::read_to_string(&self.config).unwrap();
        fs::write(
            &self.config,
            text.replace("[memory]\ndefault = \"off\"\n", ""),
        )
        .unwrap();
    }

    fn cmd(&self) -> Command {
        let mut c = Command::new(BIN);
        c.arg("--config")
            .arg(&self.config)
            // Tests may run inside a Claude Code session; do not inherit that.
            .env_remove("CLAUDECODE")
            .env_remove("AI_ORCHESTRA_ACTIVE")
            .env_remove("CODEX_THREAD_ID")
            .env_remove("CODEX_SESSION_ID")
            .env_remove("CODEX_SANDBOX")
            .env_remove("CODEX_SHELL")
            // Saved debates must never touch the real home directory.
            .env("HOME", self.dir.path().join("home"))
            .env("XDG_DATA_HOME", self.dir.path().join("data"))
            .env("MOCK_DIR", self.dir.path())
            // Credentials that must NOT reach the agents.
            .env("ANTHROPIC_API_KEY", "sk-test-should-be-scrubbed")
            .env("OPENAI_API_KEY", "sk-test-should-be-scrubbed");
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd().args(args).output().unwrap()
    }

    fn recorded_args(&self, file: &str) -> String {
        fs::read_to_string(self.dir.path().join(file)).unwrap()
    }
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn doctor_reports_all_three_agents() {
    let f = Fixture::new();
    let o = f.run(&["doctor"]);
    assert!(o.status.success(), "{}", err(&o));
    let text = out(&o);
    assert!(text.contains("Claude Code (claude)"), "{text}");
    assert!(text.contains("9.9.9 (Mock Claude)"));
    assert!(text.contains("logged in via claude.ai (pro plan)"));
    assert!(text.contains("logged in via ChatGPT"));
    assert!(text.contains("Google Antigravity CLI"));
    assert!(text.contains("unknown (this CLI has no safe auth-status command)"));
    assert!(
        !text.contains("person@example.com"),
        "account email must not be shown"
    );
    assert!(
        text.contains("ANTHROPIC_API_KEY") && text.contains("removed from agent child processes"),
        "doctor should warn about API-key variables: {text}"
    );
    assert!(
        !text.contains("sk-test-should-be-scrubbed"),
        "secret values must never be printed"
    );
}

#[test]
fn doctor_json_is_machine_readable() {
    let f = Fixture::new();
    let o = f.run(&["doctor", "--json"]);
    assert!(o.status.success(), "{}", err(&o));
    let v: serde_json::Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(v["providers"].as_array().unwrap().len(), 3);
    assert_eq!(v["providers"][0]["id"], "claude");
    assert_eq!(v["providers"][0]["installed"], true);
    assert_eq!(v["providers"][0]["auth"]["state"], "logged_in");
}

#[test]
fn doctor_flags_missing_binaries_and_fails_when_none_usable() {
    let missing = "/definitely/not/installed";
    let f = Fixture::with_executables(Some(missing), Some(missing), Some(missing));
    let o = f.run(&["doctor"]);
    assert!(!o.status.success());
    assert!(out(&o).contains("NOT INSTALLED"));

    // One missing agent is only a warning.
    let f = Fixture::with_executables(Some(missing), None, None);
    let o = f.run(&["doctor"]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&o).contains("NOT INSTALLED"));
}

#[test]
fn ask_claude_reads_only_scrubs_keys_and_keeps_prompt_off_argv() {
    let f = Fixture::new();
    let o = f.run(&["ask", "claude", "hello world"]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(
        out(&o).contains("claude saw: hello world | key=absent"),
        "{}",
        out(&o)
    );
    let args = f.recorded_args("claude.args");
    assert!(args.contains("--tools Read,Grep,Glob"), "{args}");
    assert!(!args.contains("acceptEdits"));
    assert!(
        !args.contains("hello"),
        "prompt must not appear in argv: {args}"
    );
}

#[test]
fn ask_codex_and_google_alias() {
    let f = Fixture::new();
    let o = f.run(&["ask", "codex", "review this"]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&o).contains("codex saw: review this | key=absent"));
    let args = f.recorded_args("codex.args");
    assert!(args.contains("--sandbox read-only"), "{args}");
    assert!(args.trim_end().ends_with(" -"), "{args}");

    for name in ["google", "agy", "Antigravity"] {
        let o = f.run(&["ask", name, "find problems"]);
        assert!(o.status.success(), "{name}: {}", err(&o));
        assert!(out(&o).contains("agy saw: find problems"));
    }
    assert!(f.recorded_args("agy.args").contains("--mode plan"));
}

#[test]
fn write_access_only_with_explicit_flag() {
    let f = Fixture::new();
    let o = f.run(&["ask", "claude", "edit it", "--allow-write"]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(err(&o).contains("write access enabled"));
    assert!(
        f.recorded_args("claude.args")
            .contains("--permission-mode acceptEdits")
    );
    f.run(&["ask", "codex", "edit it", "--allow-write"]);
    assert!(
        f.recorded_args("codex.args")
            .contains("--sandbox workspace-write")
    );
}

#[test]
fn ask_json_output_is_normalized() {
    let f = Fixture::new();
    let o = f.run(&["ask", "claude", "hi", "--json"]);
    assert!(o.status.success(), "{}", err(&o));
    let v: serde_json::Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(v["provider"], "claude");
    assert_eq!(v["session_id"], "s1");
    assert_eq!(v["usage"]["output_tokens"], 4);
    assert_eq!(v["truncated"], false);
}

#[test]
fn prompt_can_come_from_stdin() {
    use std::io::Write;
    use std::process::Stdio;
    let f = Fixture::new();
    let mut child = f
        .cmd()
        .args(["ask", "claude", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"from stdin")
        .unwrap();
    let o = child.wait_with_output().unwrap();
    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&o).contains("claude saw: from stdin"));
}

#[test]
fn auth_and_quota_failures_are_reported_clearly() {
    let f = Fixture::new();
    let o = f.run(&["ask", "claude", "FAIL_AUTH"]);
    assert_eq!(o.status.code(), Some(3), "{}", err(&o));
    assert!(err(&o).contains("authentication problem"));
    assert!(err(&o).contains("never handles credentials"));

    let o = f.run(&["ask", "claude", "FAIL_QUOTA"]);
    assert_eq!(o.status.code(), Some(4), "{}", err(&o));
    assert!(err(&o).contains("usage limit"));
}

#[test]
fn malformed_json_is_an_error_not_a_panic() {
    let f = Fixture::new();
    let o = f.run(&["ask", "claude", "BADJSON"]);
    assert_eq!(o.status.code(), Some(1));
    let e = err(&o);
    assert!(e.contains("could not understand the CLI output"), "{e}");
    assert!(!e.contains("panicked"));
}

#[test]
fn timeout_terminates_the_agent() {
    let f = Fixture::new();
    let started = Instant::now();
    let o = f.run(&["ask", "claude", "SLEEP", "--timeout", "1"]);
    assert_eq!(o.status.code(), Some(124), "{}", err(&o));
    assert!(err(&o).contains("timed out after 1s"));
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "took {:?}",
        started.elapsed()
    );
}

#[test]
fn missing_binary_is_reported() {
    let f = Fixture::with_executables(Some("/definitely/not/installed"), None, None);
    let o = f.run(&["ask", "claude", "hi"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("was not found"), "{}", err(&o));
}

#[test]
fn refuses_recursive_invocation_of_the_running_agent() {
    let f = Fixture::new();
    let o = f
        .cmd()
        .args(["ask", "claude", "hi"])
        .env("CLAUDECODE", "1")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    let e = err(&o);
    assert!(
        e.contains("recursive agent call") && e.contains("CLAUDECODE"),
        "{e}"
    );
    // Other agents are still reachable from inside a Claude session.
    let o = f
        .cmd()
        .args(["ask", "codex", "hi"])
        .env("CLAUDECODE", "1")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", err(&o));
}

#[test]
fn nested_guard_can_be_disabled_in_config() {
    let f = Fixture::new();
    let mut text = fs::read_to_string(&f.config).unwrap();
    text = text.replace(
        "timeout_secs = 20",
        "timeout_secs = 20\nallow_nested = true",
    );
    fs::write(&f.config, text).unwrap();
    let o = f
        .cmd()
        .args(["ask", "claude", "hi"])
        .env("CLAUDECODE", "1")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", err(&o));
}

#[test]
fn unknown_provider_and_bad_config_exit_with_usage_code() {
    let f = Fixture::new();
    let o = f.run(&["ask", "gpt", "hi"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("known providers: claude, codex, google"));

    let bad = f.dir.path().join("bad.toml");
    fs::write(&bad, "[general]\nbogus = true\n").unwrap();
    let o = Command::new(BIN)
        .arg("--config")
        .arg(&bad)
        .args(["ask", "claude", "hi"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("configuration error"));

    let o = Command::new(BIN)
        .arg("--config")
        .arg(Path::new("/no/such/config.toml"))
        .arg("doctor")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn oversized_prompt_is_rejected() {
    let f = Fixture::new();
    let mut text = fs::read_to_string(&f.config).unwrap();
    text = text.replace(
        "timeout_secs = 20",
        "timeout_secs = 20\nmax_prompt_bytes = 10",
    );
    fs::write(&f.config, text).unwrap();
    let o = f.run(&["ask", "claude", "this prompt is far too long"]);
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    assert!(err(&o).contains("exceeds the limit"));
}

// ---------------------------------------------------------------- debate ----

/// Role-aware fake agents with JSON-safe fixed replies. The role is decided
/// from marker phrases in the prompt. Every real call (anything but
/// `--version` / auth probes) appends its argument line to `<name>.calls`, its
/// role to `<name>.roles`, and the orchestration marker it saw to `<name>.active`.
/// Optional behavior files in $MOCK_DIR: `<name>.slow` (sleep 30),
/// `<name>.failsynth` (fail the synthesis), `<name>.quota` / `<name>.garbage`
/// (fail every call), `<name>.failall` (fail the proposal too).
const ROLE_CASE: &str = r#"
case "$prompt" in
  *"FINAL synthesis"*) role=synthesis; out="SYNTHESIS by NAME";;
  *"cross-review round"*) role=cross; out="CROSS-REVIEW from NAME";;
  *"independent review"*) role=review; out="REVIEW from NAME";;
  *) role=proposal; out="PROPOSAL from NAME";;
esac
echo "$role" >> "$MOCK_DIR/NAME.roles"
echo "${AI_ORCHESTRA_ACTIVE:-unset}" >> "$MOCK_DIR/NAME.active"
[ -f "$MOCK_DIR/NAME.slow" ] && sleep 30
[ -f "$MOCK_DIR/NAME.failall" ] && { echo "Not logged in. Run login" >&2; exit 1; }
if [ -f "$MOCK_DIR/NAME.failsynth" ] && [ "$role" = synthesis ]; then echo "Claude AI usage limit reached" >&2; exit 1; fi
if [ -f "$MOCK_DIR/NAME.quota" ]; then echo "You've hit your usage limit. Try again later." >&2; exit 1; fi
"#;

fn role_case(name: &str) -> String {
    ROLE_CASE.replace("NAME", name)
}

fn debate_claude() -> String {
    format!(
        r#"#!/bin/sh
case "$1" in
  --version) echo "9.9.9"; exit 0;;
  auth) echo '{{"loggedIn":true,"authMethod":"claude.ai"}}'; exit 0;;
esac
prompt=$(cat)
echo "$@" >> "$MOCK_DIR/claude.calls"
{}
printf '{{"type":"result","is_error":false,"result":"%s","modelUsage":{{"claude-sonnet-5-5":{{"inputTokens":5,"outputTokens":7}}}}}}\n' "$out"
"#,
        role_case("claude")
    )
}

fn debate_codex() -> String {
    format!(
        r#"#!/bin/sh
case "$1" in
  --version) echo "codex-cli 9.9.9"; exit 0;;
  login) echo "Logged in using ChatGPT"; exit 0;;
esac
prompt=$(cat)
echo "$@" >> "$MOCK_DIR/codex.calls"
{}
if [ -f "$MOCK_DIR/codex.garbage" ]; then echo "this is not json events"; exit 0; fi
printf '{{"type":"item.completed","item":{{"type":"agent_message","text":"%s"}}}}\n' "$out"
"#,
        role_case("codex")
    )
}

fn debate_agy() -> String {
    format!(
        r#"#!/bin/sh
case "$1" in
  --version) echo "1.9.9"; exit 0;;
esac
echo "$@" >> "$MOCK_DIR/agy.calls"
prompt=""
while [ $# -gt 0 ]; do
  if [ "$1" = "-p" ]; then prompt="$2"; fi
  shift
done
{}
printf '{{"status":"SUCCESS","response":"%s"}}\n' "$out"
"#,
        role_case("agy")
    )
}

fn debate_fixture() -> Fixture {
    Fixture::with_scripts([&debate_claude(), &debate_codex(), &debate_agy()])
}

/// Makes the named fake agent behave in a special way (see `ROLE_CASE`).
fn behave(f: &Fixture, agent: &str, behavior: &str) {
    fs::write(f.dir.path().join(format!("{agent}.{behavior}")), "").unwrap();
}

/// Argument lines of the real calls an agent received. `agy` takes the
/// prompt as an argument, so its log has continuation lines that are skipped
/// by keying on how each CLI's argument list starts.
fn call_lines(f: &Fixture, name: &str) -> Vec<String> {
    let start = match name {
        "claude" => "-p ",
        "codex" => "exec ",
        _ => "--output-format ",
    };
    fs::read_to_string(f.dir.path().join(format!("{name}.calls")))
        .unwrap_or_default()
        .lines()
        // A research request puts the global `--search` flag before `exec`.
        .filter(|l| l.starts_with(start) || (name == "codex" && l.starts_with("--search exec ")))
        .map(String::from)
        .collect()
}

fn call_count(f: &Fixture, name: &str) -> usize {
    call_lines(f, name).len()
}

fn roles(f: &Fixture, name: &str) -> Vec<String> {
    fs::read_to_string(f.dir.path().join(format!("{name}.roles")))
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect()
}

fn json(o: &Output) -> serde_json::Value {
    serde_json::from_str(&out(o)).unwrap_or_else(|e| panic!("not JSON ({e}): {}", out(o)))
}

#[test]
fn three_agents_debate_with_one_review_round_by_default() {
    let f = debate_fixture();
    let o = f.run(&["debate", "Which architecture is best?"]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(
        out(&o).trim(),
        "SYNTHESIS by claude",
        "stdout is only the decision"
    );

    let e = err(&o);
    for line in [
        "[Coordinator] Preparing initial proposal...",
        "[Coordinator] Initial proposal ready",
        "[Codex] Reviewing...",
        "[Google] Reviewing...",
        "[Codex] Review ready",
        "[Google] Review ready",
        "[Coordinator] Producing final synthesis...",
        "[Coordinator] Final synthesis ready",
        "[Debate] COMPLETE · 4 calls",
    ] {
        assert!(e.contains(line), "missing {line:?} in:\n{e}");
    }
    let at = |needle: &str| e.find(needle).unwrap();
    assert!(
        at("Preparing initial proposal") < at("[Codex] Reviewing")
            && at("[Codex] Reviewing") < at("Producing final synthesis")
    );
    assert!(
        e.contains("saved as ") && e.contains("ai-orchestra show"),
        "{e}"
    );
    assert!(
        !e.contains("Reconsidering"),
        "one round has no cross-review"
    );

    // 1 proposal + 1 review each + 1 synthesis.
    assert_eq!(roles(&f, "claude"), ["proposal", "synthesis"]);
    assert_eq!(roles(&f, "codex"), ["review"]);
    assert_eq!(roles(&f, "agy"), ["review"]);
    // Read-only everywhere.
    assert!(
        !call_lines(&f, "claude")
            .iter()
            .any(|l| l.contains("acceptEdits"))
    );
    assert!(
        call_lines(&f, "codex")
            .iter()
            .all(|l| l.contains("--sandbox read-only"))
    );
    assert!(
        call_lines(&f, "agy")
            .iter()
            .all(|l| l.contains("--mode plan"))
    );
}

#[test]
fn two_rounds_add_a_cross_review_where_reviewers_reconsider() {
    let f = debate_fixture();
    let o = f.run(&["debate", "q", "--rounds", "2"]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(out(&o).trim(), "SYNTHESIS by claude");
    assert_eq!(
        roles(&f, "claude"),
        ["proposal", "synthesis"],
        "no coordinator revision between rounds"
    );
    assert_eq!(roles(&f, "codex"), ["review", "cross"]);
    assert_eq!(roles(&f, "agy"), ["review", "cross"]);
    let e = err(&o);
    assert!(
        e.contains("[Codex] Reconsidering after cross-review (round 2)..."),
        "{e}"
    );
    assert!(
        e.contains("[Debate] COMPLETE · 6 calls"),
        "1 + 2 x 2 + 1: {e}"
    );
    // The CLI refuses anything beyond the maximum of two.
    for bad in ["3", "0", "9"] {
        let o = f.run(&["debate", "q", "--rounds", bad]);
        assert_eq!(o.status.code(), Some(2), "--rounds {bad}: {}", err(&o));
    }
    assert_eq!(call_count(&f, "claude"), 2, "rejected runs called nothing");
}

#[test]
fn every_provider_can_be_the_coordinator() {
    for (name, label) in [
        ("claude", "claude"),
        ("codex", "codex"),
        ("google", "agy"),
        ("agy", "agy"),
    ] {
        let f = debate_fixture();
        let o = f.run(&["debate", "q", "--coordinator", name]);
        assert!(o.status.success(), "{name}: {}", err(&o));
        assert_eq!(out(&o).trim(), format!("SYNTHESIS by {label}"), "{name}");
        assert_eq!(roles(&f, label), ["proposal", "synthesis"], "{name} led");
    }
}

#[test]
fn output_json_is_the_typed_record_and_json_stays_as_an_alias() {
    let f = debate_fixture();
    for flag in [["--output", "json"], ["--json", "--output=text"]] {
        let o = f.run(&["debate", "q", "--rounds", "2", flag[0], flag[1], "--quiet"]);
        assert!(o.status.success(), "{}", err(&o));
        let v = json(&o);
        assert_eq!(v["status"], "complete");
        assert_eq!(v["synthesized"], true);
        assert_eq!(v["coordinator"], "claude");
        assert_eq!(v["participants"], serde_json::json!(["codex", "google"]));
        assert_eq!(v["rounds_planned"], 2);
        assert_eq!(v["calls"], 6);
        assert_eq!(v["final_answer"], "SYNTHESIS by claude");
        assert_eq!(v["steps"].as_array().unwrap().len(), 6);
        assert_eq!(v["steps"][0]["phase"], "proposal");
        assert_eq!(v["steps"][5]["phase"], "synthesis");
        assert!(
            v["steps"]
                .as_array()
                .unwrap()
                .iter()
                .all(|s| s["duration_ms"].is_u64() && s["ok"] == true)
        );
        assert!(v["id"].as_str().unwrap().len() > 15);
        assert!(!err(&o).contains("[Codex]"), "--quiet silences progress");
    }
}

// ---- partial failures never look like success -----------------------------

#[test]
fn a_reviewer_out_of_quota_makes_the_debate_partial_exit_5_and_is_not_retried() {
    let f = debate_fixture();
    behave(&f, "codex", "quota");
    let o = f.run(&["debate", "q", "--rounds", "2"]);
    assert_eq!(o.status.code(), Some(5), "{}", err(&o));
    assert!(
        out(&o).contains("SYNTHESIS by claude"),
        "the result is still printed"
    );
    let e = err(&o);
    assert!(
        e.contains("PARTIAL RESULT") && e.contains("[Codex] Review FAILED"),
        "{e}"
    );
    assert!(
        e.contains("usage limit") && e.contains("[Debate] PARTIAL"),
        "{e}"
    );
    assert!(
        !e.contains(" WARN "),
        "no raw log lines in normal output: {e}"
    );
    assert!(
        e.matches("You've hit your usage limit").count() <= 2,
        "at most the progress line and the final warning mention it: {e}"
    );
    assert!(
        !e.contains("This is a subscription limit"),
        "the multi-line detail stays in the saved record, not the console: {e}"
    );
    assert_eq!(
        roles(&f, "codex"),
        ["review"],
        "a quota failure is never retried, not even in round 2"
    );
    assert_eq!(roles(&f, "agy"), ["review", "cross"]);

    let v = json(&f.run(&["show", &saved_id(&f), "--output", "json"]));
    assert_eq!(v["status"], "partial");
    let failed: Vec<_> = v["steps"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["ok"] == false)
        .collect();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0]["agent"], "codex");
    assert_eq!(failed[0]["error_kind"], "quota");
}

#[test]
fn a_reviewer_timeout_is_terminated_recorded_and_makes_the_debate_partial() {
    let f = debate_fixture();
    behave(&f, "agy", "slow");
    let started = Instant::now();
    let o = f.run(&["debate", "q", "--timeout", "1"]);
    assert_eq!(o.status.code(), Some(5), "{}", err(&o));
    assert!(
        started.elapsed() < Duration::from_secs(25),
        "took {:?}",
        started.elapsed()
    );
    assert!(err(&o).contains("timed out after 1s"), "{}", err(&o));
    assert!(out(&o).contains("SYNTHESIS by claude"));
    let v = json(&f.run(&["show", &saved_id(&f), "--output", "json"]));
    let timed_out = v["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["agent"] == "google")
        .unwrap();
    assert_eq!(timed_out["error_kind"], "timeout");
    assert_eq!(v["status"], "partial");
}

#[test]
fn malformed_reviewer_output_is_partial_not_a_crash() {
    let f = debate_fixture();
    behave(&f, "codex", "garbage");
    let o = f.run(&["debate", "q"]);
    assert_eq!(o.status.code(), Some(5), "{}", err(&o));
    assert!(!err(&o).contains("panicked"));
    let v = json(&f.run(&["show", &saved_id(&f), "--output", "json"]));
    let bad = v["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["agent"] == "codex")
        .unwrap();
    assert_eq!(bad["error_kind"], "malformed");
    assert!(
        bad["error"]
            .as_str()
            .unwrap()
            .contains("could not understand")
    );
}

#[test]
fn a_coordinator_that_fails_the_synthesis_leaves_a_labelled_partial_result() {
    let f = debate_fixture();
    behave(&f, "claude", "failsynth");
    let o = f.run(&["debate", "q"]);
    assert_eq!(o.status.code(), Some(5), "{}", err(&o));
    assert_eq!(
        out(&o).trim(),
        "PROPOSAL from claude",
        "falls back to the unreviewed proposal"
    );
    assert!(
        err(&o).contains("failed to write the synthesis") && err(&o).contains("PARTIAL RESULT")
    );
    let shown = out(&f.run(&["show", &saved_id(&f)]));
    assert!(
        shown.contains("(partial)") && shown.contains("NOT a synthesis"),
        "{shown}"
    );
}

#[test]
fn a_failed_initial_proposal_is_a_failed_debate_with_the_cause_exit_code() {
    let f = debate_fixture();
    behave(&f, "claude", "failall");
    let o = f.run(&["debate", "q"]);
    assert_eq!(
        o.status.code(),
        Some(3),
        "auth failure maps to exit 3: {}",
        err(&o)
    );
    assert!(out(&o).is_empty(), "no result on stdout");
    assert_eq!(
        call_count(&f, "codex") + call_count(&f, "agy"),
        0,
        "reviewers are not consulted"
    );
    let v = json(&f.run(&["show", &saved_id(&f), "--output", "json"]));
    assert_eq!(v["status"], "failed");
    assert!(v["error"].as_str().unwrap().contains("authentication"));
    assert_eq!(v["final_answer"], "");
    let listing = out(&f.run(&["history"]));
    assert!(listing.contains("failed"), "{listing}");
}

#[test]
fn a_missing_reviewer_is_skipped_but_no_reviewer_at_all_is_an_error() {
    let f = debate_fixture();
    let text = fs::read_to_string(&f.config).unwrap().replace(
        &f.dir.path().join("bin/codex").display().to_string(),
        "/definitely/not/installed",
    );
    fs::write(&f.config, text).unwrap();
    let o = f.run(&["debate", "q"]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(err(&o).contains("not consulted: Codex"), "{}", err(&o));
    assert_eq!(call_count(&f, "codex"), 0);
    assert_eq!(
        json(&f.run(&["show", &saved_id(&f), "--output", "json"]))["participants"],
        serde_json::json!(["google"])
    );

    let o = f.run(&["debate", "q", "--peers", "codex"]);
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    assert!(
        err(&o).contains("needs at least one available agent"),
        "{}",
        err(&o)
    );
}

#[test]
fn ctrl_c_cancels_the_debate_terminates_agents_and_is_recorded() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    let f = debate_fixture();
    behave(&f, "claude", "slow");
    let mut child = f
        .cmd()
        .args(["debate", "q"])
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stderr.take().unwrap());
    let started = Instant::now();
    let mut line = String::new();
    // Wait until the coordinator is demonstrably running, then interrupt.
    loop {
        line.clear();
        assert!(
            lines.read_line(&mut line).unwrap() > 0,
            "stderr closed early"
        );
        if line.contains("Preparing initial proposal") {
            break;
        }
    }
    let status = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    let exit = child.wait().unwrap();
    assert_eq!(exit.code(), Some(130), "cancelled");
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "agents were terminated, not waited for"
    );
    let v = json(&f.run(&["show", &saved_id(&f), "--output", "json"]));
    assert_eq!(v["status"], "cancelled");
    assert!(v["error"].as_str().unwrap().contains("cancelled"));
}

// ---- safety ----------------------------------------------------------------

#[test]
fn agents_are_marked_so_a_nested_orchestra_refuses_to_run() {
    let f = debate_fixture();
    assert!(f.run(&["debate", "q"]).status.success());
    for name in ["claude", "codex", "agy"] {
        let seen = fs::read_to_string(f.dir.path().join(format!("{name}.active"))).unwrap();
        assert!(
            seen.lines().all(|l| l == "1"),
            "{name} must run with AI_ORCHESTRA_ACTIVE=1, saw {seen:?}"
        );
    }
    // Pretend we are an agent started by ai-orchestra.
    let before = call_count(&f, "claude");
    for args in [vec!["debate", "q"], vec!["ask", "claude", "hi"]] {
        let o = f
            .cmd()
            .args(&args)
            .env("AI_ORCHESTRA_ACTIVE", "1")
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(1), "{args:?}: {}", err(&o));
        assert!(
            err(&o).contains("recursive agent call") && err(&o).contains("AI_ORCHESTRA_ACTIVE"),
            "{}",
            err(&o)
        );
    }
    assert_eq!(call_count(&f, "claude"), before, "nothing was called");
    // Reading history is still allowed.
    assert!(
        f.cmd()
            .args(["history"])
            .env("AI_ORCHESTRA_ACTIVE", "1")
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn a_codex_session_marker_stops_codex_from_being_called_recursively() {
    let f = debate_fixture();
    let o = f
        .cmd()
        .args(["debate", "q"])
        .env("CODEX_THREAD_ID", "t-1")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", err(&o));
    assert!(err(&o).contains("not consulted: Codex"), "{}", err(&o));
    assert_eq!(call_count(&f, "codex"), 0);
    let o = f
        .cmd()
        .args(["ask", "codex", "hi"])
        .env("CODEX_THREAD_ID", "t-1")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("recursive agent call"), "{}", err(&o));
    let o = f
        .cmd()
        .args(["debate", "q", "--coordinator", "codex"])
        .env("CODEX_THREAD_ID", "t-1")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
}

#[test]
fn debate_inside_claude_code_skips_claude_as_a_reviewer_and_refuses_it_as_coordinator() {
    let f = debate_fixture();
    let o = f
        .cmd()
        .args(["debate", "q", "--coordinator", "codex"])
        .env("CLAUDECODE", "1")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", err(&o));
    assert!(err(&o).contains("not consulted: Claude"), "{}", err(&o));
    assert_eq!(
        call_count(&f, "claude"),
        0,
        "the running agent must never be called"
    );
    let o = f
        .cmd()
        .args(["debate", "q"])
        .env("CLAUDECODE", "1")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("recursive agent call"), "{}", err(&o));
}

#[test]
fn debate_input_errors_use_the_usage_exit_code() {
    let f = debate_fixture();
    for args in [
        vec!["debate", "q", "--peers", "claude"], // the coordinator cannot be a reviewer
        vec!["debate", "q", "--peers", "nobody"],
        vec!["debate", "q", "--coordinator", "gpt"],
        vec!["debate", "   "],
        vec!["debate", "q", "--timeout", "0"],
    ] {
        let o = f.run(&args);
        assert_eq!(o.status.code(), Some(2), "{args:?}: {}", err(&o));
    }
    assert_eq!(
        call_count(&f, "claude"),
        0,
        "validation fails before any agent runs"
    );
}

#[test]
fn debate_total_budget_stops_runaway_agents() {
    let f = debate_fixture();
    let mut text = fs::read_to_string(&f.config).unwrap();
    text.push_str("\n[debate]\ntotal_timeout_secs = 1\n");
    fs::write(&f.config, text).unwrap();
    behave(&f, "claude", "slow");
    let started = Instant::now();
    let o = f.run(&["debate", "q"]);
    assert_eq!(o.status.code(), Some(124), "{}", err(&o));
    assert!(err(&o).contains("total time budget"), "{}", err(&o));
    assert!(started.elapsed() < Duration::from_secs(20));
    let v = json(&f.run(&["show", &saved_id(&f), "--output", "json"]));
    assert_eq!(v["status"], "failed");
}

// ---- history and show --------------------------------------------------------

/// Id of the most recently saved debate.
fn saved_id(f: &Fixture) -> String {
    let v = json(&f.run(&["history", "--output", "json", "--limit", "1"]));
    v[0]["id"].as_str().expect("a saved debate").to_string()
}

#[test]
fn history_lists_saved_debates_newest_first_and_show_reads_them_back() {
    let f = debate_fixture();
    assert!(out(&f.run(&["history"])).contains("No saved debates yet"));
    assert!(
        f.run(&["debate", "first question", "--quiet"])
            .status
            .success()
    );
    std::thread::sleep(Duration::from_millis(1100)); // ids are second-resolution
    behave(&f, "codex", "quota");
    assert_eq!(
        f.run(&["debate", "second question", "--quiet"])
            .status
            .code(),
        Some(5)
    );

    let text = out(&f.run(&["history"]));
    assert!(text.lines().next().unwrap().contains("STATUS"), "{text}");
    let first_row = text.lines().nth(1).unwrap();
    assert!(
        first_row.contains("partial") && first_row.contains("second question"),
        "newest first: {text}"
    );
    assert!(text.lines().nth(2).unwrap().contains("complete"), "{text}");

    let listing = json(&f.run(&["history", "--output", "json"]));
    assert_eq!(listing.as_array().unwrap().len(), 2);
    assert_eq!(listing[0]["status"], "partial");
    assert_eq!(
        json(&f.run(&["history", "--output", "json", "--limit", "1"]))
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // `show` by full id and by unique prefix; transcript and JSON views.
    let id = listing[1]["id"].as_str().unwrap().to_string();
    let shown = out(&f.run(&["show", &id]));
    for expected in [
        "(complete)",
        "first question",
        "Coordinator: claude",
        "Timings:",
        "--- Final decision ---",
        "SYNTHESIS by claude",
    ] {
        assert!(shown.contains(expected), "missing {expected:?}:\n{shown}");
    }
    let by_prefix = out(&f.run(&["show", &id[..id.len() - 2]]));
    assert!(by_prefix.contains(&id), "{by_prefix}");
    let transcript = out(&f.run(&["show", &id, "--transcript"]));
    assert!(
        transcript.contains("REVIEW from codex")
            && transcript.contains("REVIEW from agy")
            && transcript.contains("PROPOSAL from claude"),
        "{transcript}"
    );
    let record = json(&f.run(&["show", &id, "--output", "json"]));
    assert_eq!(record["question"], "first question");
    assert_eq!(record["schema"], 1);
    assert!(record["created_at_ms"].as_u64().unwrap() > 0);
}

#[test]
fn show_rejects_unknown_ambiguous_and_unsafe_ids() {
    let f = debate_fixture();
    assert!(f.run(&["debate", "q", "--quiet"]).status.success());
    for bad in ["nope-1234", "../../etc/passwd", "a/b", ".."] {
        let o = f.run(&["show", bad]);
        assert_eq!(o.status.code(), Some(2), "{bad}: {}", err(&o));
    }
    assert!(err(&f.run(&["show", "2999"])).contains("no saved debate matches"));
}

#[test]
fn debates_can_be_left_unsaved_or_saved_elsewhere() {
    let f = debate_fixture();
    assert!(
        f.run(&["debate", "q", "--no-save", "--quiet"])
            .status
            .success()
    );
    assert!(out(&f.run(&["history"])).contains("No saved debates yet"));

    let mut text = fs::read_to_string(&f.config).unwrap();
    text.push_str("\n[debate]\nsave = false\n");
    fs::write(&f.config, &text).unwrap();
    assert!(f.run(&["debate", "q", "--quiet"]).status.success());
    assert!(
        out(&f.run(&["history"])).contains("No saved debates yet"),
        "debate.save = false"
    );

    let dir = f.dir.path().join("elsewhere");
    let text = text.replace(
        "save = false",
        &format!("history_dir = \"{}\"", dir.display()),
    );
    fs::write(&f.config, text).unwrap();
    assert!(f.run(&["debate", "q", "--quiet"]).status.success());
    let files: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&files[0]).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    assert!(
        out(&f.run(&["history"])).contains("complete"),
        "history reads the configured dir"
    );
    assert!(
        !f.dir.path().join("data").exists(),
        "the default data dir was not used"
    );
}

// ---- live view and log file --------------------------------------------------

#[test]
fn live_flag_shows_each_step_on_stderr_and_keeps_stdout_clean() {
    let f = debate_fixture();
    let o = f.run(&["debate", "q", "--rounds", "2", "--live"]);
    assert!(o.status.success(), "{}", err(&o));
    let e = err(&o);
    for expected in [
        "claude leads · reviewers: codex, google · 2 round(s)",
        "claude is drafting the initial proposal",
        "codex is reviewing (round 1)",
        "codex is reconsidering (round 2)",
        "initial proposal · claude",
        "review · codex · round 1",
        "cross-review · google · round 2",
        "PROPOSAL from claude",
        "REVIEW from codex",
        "CROSS-REVIEW from agy", // the fake agy signs as itself
        "claude is writing the final synthesis",
        "final synthesis · claude",
        "== finished (complete): 6 calls",
    ] {
        assert!(e.contains(expected), "missing {expected:?} in:\n{e}");
    }
    assert!(
        !e.contains("[Codex] Reviewing..."),
        "--live replaces the short progress lines"
    );
    let at = |needle: &str| e.find(needle).unwrap();
    assert!(
        at("initial proposal · claude") < at("review · codex")
            && at("review · codex") < at("final synthesis · claude")
    );
    assert_eq!(out(&o).trim(), "SYNTHESIS by claude");
}

#[test]
fn log_file_records_jsonl_privately_and_appends() {
    use std::os::unix::fs::PermissionsExt;
    let f = debate_fixture();
    let log = f.dir.path().join("run.jsonl");
    let log_arg = log.to_str().unwrap();
    for _ in 0..2 {
        let o = f.run(&["debate", "q", "--log-file", log_arg]);
        assert!(o.status.success(), "{}", err(&o));
        assert!(
            !err(&o).contains("is reviewing"),
            "--log-file alone must not enable --live"
        );
    }
    let text = fs::read_to_string(&log).unwrap();
    let lines: Vec<serde_json::Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).expect("valid JSONL"))
        .collect();
    // Two runs of: started + 4 x (call_started + call_finished) + finished.
    assert_eq!(lines.len(), 2 * 10, "{text}");
    assert_eq!(lines[0]["event"], "started");
    assert_eq!(lines[0]["question"], "q");
    assert_eq!(lines[9]["event"], "finished");
    assert_eq!(lines[9]["status"], "complete");
    assert_eq!(lines[10]["event"], "started", "second run appends");
    assert_ne!(lines[0]["debate_id"], lines[10]["debate_id"]);
    let synthesis: Vec<_> = lines
        .iter()
        .filter(|l| l["event"] == "call_finished" && l["role"] == "synthesis")
        .collect();
    assert_eq!(synthesis.len(), 2);
    assert_eq!(synthesis[0]["text"], "SYNTHESIS by claude");
    assert!(lines.iter().all(|l| l["ts_ms"].as_u64().unwrap() > 0));
    assert_eq!(
        fs::metadata(&log).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn log_file_captures_failed_reviewers_and_fatal_errors() {
    let f = debate_fixture();
    behave(&f, "codex", "quota");
    let log = f.dir.path().join("fail.jsonl");
    let o = f.run(&["debate", "q", "--log-file", log.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(5), "{}", err(&o));
    let text = fs::read_to_string(&log).unwrap();
    assert!(
        text.lines()
            .any(|l| l.contains("\"ok\":false") && l.contains("\"provider\":\"codex\"")),
        "{text}"
    );
    assert!(
        text.lines()
            .last()
            .unwrap()
            .contains("\"status\":\"partial\"")
    );

    let log = f.dir.path().join("fatal.jsonl");
    let o = f
        .cmd()
        .args(["debate", "q", "--log-file", log.to_str().unwrap()])
        .env("CLAUDECODE", "1")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    // The refusal happens at the first agent call, so the failure is logged.
    let last = fs::read_to_string(&log)
        .unwrap()
        .lines()
        .last()
        .unwrap()
        .to_string();
    assert!(
        last.contains("\"event\":\"failed\"") && last.contains("recursive"),
        "{last}"
    );

    behave(&f, "claude", "failall");
    let log = f.dir.path().join("fatal2.jsonl");
    let o = f.run(&["debate", "q", "--log-file", log.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(3));
    let last = fs::read_to_string(&log)
        .unwrap()
        .lines()
        .last()
        .unwrap()
        .to_string();
    assert!(
        last.contains("\"event\":\"failed\"") && last.contains("authentication"),
        "{last}"
    );
}

#[test]
fn unwritable_log_path_fails_before_any_agent_runs() {
    let f = debate_fixture();
    let o = f.run(&["debate", "q", "--log-file", "/nonexistent-dir/x.jsonl"]);
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    assert!(err(&o).contains("cannot open log file"));
    assert_eq!(call_count(&f, "claude"), 0);
}

// ---- performance (measured, loosely bounded) -----------------------------------

#[test]
fn orchestration_overhead_with_instant_fake_agents_is_small() {
    let f = debate_fixture();
    // The first launch of a fresh binary is slow on macOS (it is scanned), so
    // warm up once and then take several samples.
    let args = [
        "debate",
        "q",
        "--rounds",
        "2",
        "--quiet",
        "--no-save",
        "--output",
        "json",
    ];
    assert!(f.run(&args).status.success());
    let mut walls = Vec::new();
    let mut inner = Vec::new();
    for _ in 0..7 {
        let t = Instant::now();
        let o = f.run(&args);
        walls.push(t.elapsed());
        assert!(o.status.success(), "{}", err(&o));
        inner.push(json(&o)["duration_ms"].as_u64().unwrap());
    }
    walls.sort();
    inner.sort();
    eprintln!(
        "PERF e2e: 6 calls to 3 fake shell CLIs (3 agents, 2 rounds), 7 samples: \
         process wall min {:?} / median {:?} / max {:?}; engine time min {} / median {} / max {} ms",
        walls[0], walls[3], walls[6], inner[0], inner[3], inner[6]
    );
    assert!(walls[3] < Duration::from_secs(5), "{:?}", walls[3]);
}

#[test]
fn ask_passes_model_and_effort_to_each_cli() {
    let f = Fixture::new();
    let o = f.run(&["ask", "claude", "hi", "--model", "opus", "--effort", "low"]);
    assert!(o.status.success(), "{}", err(&o));
    let args = f.recorded_args("claude.args");
    assert!(
        args.contains("--model opus") && args.contains("--effort low"),
        "{args}"
    );

    let o = f.run(&[
        "ask",
        "codex",
        "hi",
        "--model",
        "gpt-6-luna",
        "--effort",
        "low",
    ]);
    assert!(o.status.success(), "{}", err(&o));
    let args = f.recorded_args("codex.args");
    assert!(args.contains("--model gpt-6-luna"), "{args}");
    assert!(args.contains("-c model_reasoning_effort=\"low\""), "{args}");

    let o = f.run(&[
        "ask",
        "google",
        "hi",
        "--model",
        "gemini-3.8-flash-low",
        "--effort",
        "medium",
    ]);
    assert!(o.status.success(), "{}", err(&o));
    let args = f.recorded_args("agy.args");
    assert!(
        args.contains("--model gemini-3.8-flash-low") && args.contains("--effort medium"),
        "{args}"
    );
}

#[test]
fn without_settings_the_cli_keeps_its_own_defaults() {
    let f = Fixture::new();
    for name in ["claude", "codex", "google"] {
        assert!(f.run(&["ask", name, "hi"]).status.success());
    }
    for file in ["claude.args", "codex.args", "agy.args"] {
        let args = f.recorded_args(file);
        assert!(
            !args.contains("--model") && !args.contains("--effort"),
            "{file}: {args}"
        );
        assert!(!args.contains("model_reasoning_effort"), "{file}: {args}");
    }
}

#[test]
fn config_settings_apply_and_flags_override_them() {
    let f = Fixture::new();
    let text = fs::read_to_string(&f.config).unwrap().replace(
        "[providers.claude]\n",
        "[providers.claude]\nmodel = \"sonnet\"\neffort = \"high\"\n",
    );
    fs::write(&f.config, text).unwrap();

    assert!(f.run(&["ask", "claude", "hi"]).status.success());
    let args = f.recorded_args("claude.args");
    assert!(
        args.contains("--model sonnet") && args.contains("--effort high"),
        "{args}"
    );

    assert!(
        f.run(&["ask", "claude", "hi", "--effort", "low"])
            .status
            .success()
    );
    let args = f.recorded_args("claude.args");
    assert!(
        args.contains("--effort low") && !args.contains("--effort high"),
        "{args}"
    );
    assert!(
        args.contains("--model sonnet"),
        "unrelated settings are kept: {args}"
    );
}

#[test]
fn invalid_settings_exit_with_the_usage_code_before_any_agent_runs() {
    let f = Fixture::new();
    let o = f.run(&["ask", "claude", "hi", "--effort", "turbo"]);
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    assert!(err(&o).contains("invalid effort `turbo`") && err(&o).contains("low, medium, high"));
    let o = f.run(&["ask", "claude", "hi", "--model", "--bare"]);
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    assert!(
        !f.dir.path().join("claude.args").exists(),
        "no agent may run"
    );

    // A bad value in the config file fails at startup, for every command.
    let text = fs::read_to_string(&f.config).unwrap().replace(
        "[providers.google]\n",
        "[providers.google]\neffort = \"turbo\"\n",
    );
    fs::write(&f.config, text).unwrap();
    let o = f.run(&["doctor"]);
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    assert!(err(&o).contains("providers.google"), "{}", err(&o));
}

#[test]
fn debate_overrides_apply_per_agent() {
    let f = debate_fixture();
    let o = f.run(&[
        "debate",
        "q",
        "--quiet",
        "--model",
        "claude=opus",
        "--effort",
        "claude=medium",
        "--effort",
        "codex=low",
    ]);
    assert!(o.status.success(), "{}", err(&o));
    let claude = call_lines(&f, "claude");
    assert_eq!(claude.len(), 2, "proposal and synthesis");
    assert!(
        claude
            .iter()
            .all(|l| l.contains("--model opus") && l.contains("--effort medium")),
        "{claude:?}"
    );
    let codex = call_lines(&f, "codex");
    assert!(
        codex
            .iter()
            .all(|l| l.contains("model_reasoning_effort=\"low\"") && !l.contains("--model")),
        "{codex:?}"
    );
    let agy = call_lines(&f, "agy");
    assert_eq!(agy.len(), 1);
    assert!(
        agy.iter()
            .all(|l| !l.contains("--effort") && !l.contains("--model")),
        "an unconfigured agent keeps its own defaults: {agy:?}"
    );
}

#[test]
fn debate_override_mistakes_are_usage_errors() {
    let f = debate_fixture();
    for args in [
        vec!["debate", "q", "--model", "claude"], // no AGENT=
        vec!["debate", "q", "--model", "=opus"],
        vec!["debate", "q", "--effort", "claude="],
        vec!["debate", "q", "--effort", "ghost=low"], // unknown agent
        vec!["debate", "q", "--effort", "claude=turbo"], // invalid level
        vec!["debate", "q", "--model", "codex=--bad"],
    ] {
        let o = f.run(&args);
        assert_eq!(o.status.code(), Some(2), "{args:?}: {}", err(&o));
    }
    assert_eq!(
        call_count(&f, "claude"),
        0,
        "validation fails before any agent runs"
    );
}

#[test]
fn doctor_shows_configured_settings_or_says_the_cli_default_applies() {
    let f = Fixture::new();
    let text = fs::read_to_string(&f.config).unwrap().replace(
        "[providers.codex]\n",
        "[providers.codex]\nmodel = \"gpt-6-luna\"\neffort = \"low\"\n",
    );
    fs::write(&f.config, text).unwrap();
    let o = f.run(&["doctor"]);
    let text = out(&o);
    assert!(
        text.contains("gpt-6-luna · effort low (from ai-orchestra config)"),
        "{text}"
    );
    assert!(
        text.contains("CLI default (model and effort come from the CLI's own config)"),
        "{text}"
    );
}

#[test]
fn responses_say_which_model_and_effort_were_requested_or_reported() {
    let f = Fixture::new();
    let o = f.run(&[
        "ask", "claude", "hi", "--model", "sonnet", "--effort", "medium", "--json",
    ]);
    assert!(o.status.success(), "{}", err(&o));
    let v: serde_json::Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(v["model_requested"], "sonnet");
    assert_eq!(v["effort_requested"], "medium");
    assert_eq!(
        v["model_reported"], "claude-sonnet-5-5",
        "the main model, not the helper"
    );

    // Human footer (stderr).
    let o = f.run(&[
        "ask", "claude", "hi", "--model", "sonnet", "--effort", "medium",
    ]);
    assert!(
        err(&o).contains("claude-sonnet-5-5 (requested sonnet) · effort medium"),
        "{}",
        err(&o)
    );

    // CLIs that do not report a model: only what we asked for is shown.
    let o = f.run(&[
        "ask",
        "codex",
        "hi",
        "--model",
        "gpt-6-luna",
        "--effort",
        "low",
    ]);
    assert!(err(&o).contains("gpt-6-luna · effort low"), "{}", err(&o));

    // Nothing requested and nothing reported: say so rather than guess.
    let o = f.run(&["ask", "google", "hi", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out(&o)).unwrap();
    assert!(v["model_requested"].is_null() && v["model_reported"].is_null());
    let o = f.run(&["ask", "google", "hi"]);
    assert!(
        err(&o).contains("CLI-default model · CLI-default effort"),
        "{}",
        err(&o)
    );
}

#[test]
fn live_debate_shows_model_and_effort_behind_each_reply() {
    let f = debate_fixture();
    let o = f.run(&[
        "debate",
        "q",
        "--live",
        "--model",
        "codex=gpt-6-luna",
        "--effort",
        "codex=medium",
        "--effort",
        "claude=medium",
    ]);
    assert!(o.status.success(), "{}", err(&o));
    let e = err(&o);
    assert!(
        e.contains("review · codex · round 1") && e.contains("gpt-6-luna · effort medium"),
        "{e}"
    );
    assert!(
        e.contains("claude-sonnet-5-5 · effort medium"),
        "claude reports its model: {e}"
    );
    assert!(
        e.contains("CLI-default model · CLI-default effort"),
        "google has neither: {e}"
    );
}

// ---- consult ------------------------------------------------------------------

/// Roles of a consultation are recognized from marker phrases in the prompts. The behavior
/// files of `ROLE_CASE` (`.slow`, `.quota`, `.failall`, `.failsynth`) work the same way.
const CONSULT_HEAD: &str = r###"
case "$prompt" in
  *"FINAL answer in Markdown"*) role=synthesis; out="## Answer\\nSYNTHESIS by NAME\\n\\n## Recommendation\\nGo with NAME.\\n\\n## Areas of agreement\\nAll agree.\\n\\n## Significant disagreements\\n- gamma disagrees; unresolved.\\n\\n## Assumptions\\nBudget fixed.\\n\\n## Evidence limitations\\nNothing was verified.";;
  *"only AI assistant"*) role=single; out="SINGLE by NAME";;
  *"second cross-review round"*) role=cross; out="CROSS-REVIEW from NAME";;
  *"cross-review them"*) role=review; out="REVIEW from NAME";;
  *"FINAL synthesis"*) role=synthesis; out="SYNTHESIS by NAME";;
  *"cross-review round"*) role=cross; out="CROSS-REVIEW from NAME";;
  *"independent review"*) role=review; out="REVIEW from NAME";;
  *"cannot see their work"*) role=perspective; out="PERSPECTIVE from NAME";;
  *) role=proposal; out="PROPOSAL from NAME";;
esac
"###;

fn consult_case(name: &str) -> String {
    let tail = ROLE_CASE.split_once("esac\n").expect("role case").1;
    format!("{CONSULT_HEAD}{tail}").replace("NAME", name)
}

fn consult_fixture() -> Fixture {
    let swap = |script: String, name: &str| script.replace(&role_case(name), &consult_case(name));
    Fixture::with_scripts([
        &swap(debate_claude(), "claude"),
        &swap(debate_codex(), "codex"),
        &swap(debate_agy(), "agy"),
    ])
}

/// A plain-looking product question: routes to decision with one review round.
const PRODUCT_Q: &str = "Which electronics microscope should I buy?";

#[test]
fn consult_chooses_everything_automatically_and_explains_it() {
    let f = consult_fixture();
    let o = f.run(&["consult", PRODUCT_Q]);
    assert!(o.status.success(), "{}", err(&o));
    let text = out(&o);
    assert!(text.contains("SYNTHESIS by claude"), "{text}");
    let e = err(&o);
    for needle in [
        "[routing] mode:         decision (auto:",
        "[routing] coordinator:  claude (auto:",
        "[routing] participants: claude, codex, google (auto:",
        "[routing] rounds:       1 (auto:",
        "[routing] agent calls:  at most 7",
        "[Consult:decision]",
        "Forming its own answer",
    ] {
        assert!(e.contains(needle), "missing `{needle}` in:\n{e}");
    }
    assert_eq!(roles(&f, "claude"), ["perspective", "review", "synthesis"]);
    assert_eq!(roles(&f, "codex"), ["perspective", "review"]);
    assert_eq!(roles(&f, "agy"), ["perspective", "review"]);

    // Saved like a debate, with the routing facts and the sections read back.
    let id = saved_id(&f);
    let v = json(&f.run(&["show", &id, "--output", "json"]));
    assert_eq!(v["status"], "complete");
    assert_eq!(v["consultation"]["mode"], "decision");
    assert_eq!(v["consultation"]["strategy"], "panel");
    assert_eq!(v["consultation"]["recommendation"], "Go with claude.");
    assert_eq!(v["consultation"]["unresolved_disagreement"], true);
    assert_eq!(v["consultation"]["rounds_run"], 1);
    assert_eq!(v["consultation"]["invocations"]["claude"], 3);
    // The fake CLIs report no token counts, so none are invented.
    assert!(v["consultation"]["token_usage"].is_null());
    assert_eq!(v["calls"], 7);
    let shown = out(&f.run(&["show", &id]));
    assert!(
        shown.starts_with(&format!("Consultation {id} (complete)")),
        "{shown}"
    );
    assert!(
        shown.contains("Routing:") && shown.contains("--- Answer ---"),
        "{shown}"
    );
    let transcript = out(&f.run(&["show", &id, "--transcript"]));
    assert!(
        transcript.contains("## Independent perspectives"),
        "{transcript}"
    );
}

#[test]
fn consult_plan_shows_the_routing_without_calling_any_agent() {
    let f = consult_fixture();
    let o = f.run(&["consult", PRODUCT_Q, "--plan"]);
    assert!(o.status.success(), "{}", err(&o));
    let text = out(&o);
    assert!(
        text.contains("mode:         decision (auto:") && text.contains("agent calls:  at most 7"),
        "{text}"
    );
    let v = json(&f.run(&[
        "consult",
        "Explain quantum entanglement in depth",
        "--plan",
        "--output",
        "json",
    ]));
    assert_eq!(v["mode"]["value"], "learn");
    assert_eq!(v["mode"]["source"], "auto");
    for agent in ["claude", "codex", "agy"] {
        assert_eq!(
            call_count(&f, agent),
            0,
            "{agent} must not be called by --plan"
        );
    }
    assert!(
        out(&f.run(&["history"])).contains("No saved debates yet"),
        "--plan saves nothing"
    );
}

// ---- Milestone 6: strategies, limits and the plan ----------------------------------

#[test]
fn dry_run_prints_the_smart_plan_and_calls_nothing() {
    let f = consult_fixture();
    let o = f.run(&["consult", PRODUCT_Q, "--strategy", "balanced", "--dry-run"]);
    assert!(o.status.success(), "{}", err(&o));
    let text = out(&o);
    for needle in [
        "strategy:     balanced (manual: requested with --strategy balanced)",
        "flow:         adaptive",
        "calls:        3 expected, at most 4",
        "history:      no provider history yet",
    ] {
        assert!(text.contains(needle), "missing `{needle}`:\n{text}");
    }
    let v = json(&f.run(&[
        "consult",
        PRODUCT_Q,
        "--strategy",
        "fast",
        "--dry-run",
        "--output",
        "json",
    ]));
    assert_eq!(v["smart"]["flow"], "single");
    assert_eq!(v["smart"]["strategy"]["value"], "fast");
    assert_eq!(v["smart"]["max_calls"], 1);
    assert_eq!(v["participants"]["value"], serde_json::json!(["claude"]));
    for agent in ["claude", "codex", "agy"] {
        assert_eq!(
            call_count(&f, agent),
            0,
            "{agent} must not be called by --dry-run"
        );
    }
    assert!(out(&f.run(&["history"])).contains("No saved debates yet"));
}

#[test]
fn explain_plan_shows_the_plan_even_when_quiet_and_then_runs() {
    let f = consult_fixture();
    let o = f.run(&[
        "consult",
        "Explain electrical resistance",
        "--strategy",
        "fast",
        "--quiet",
        "--explain-plan",
    ]);
    assert!(o.status.success(), "{}", err(&o));
    let e = err(&o);
    assert!(
        e.contains("[plan] strategy:     fast") && e.contains("[plan] flow:         single"),
        "{e}"
    );
    assert!(e.contains("[plan] participants: claude"), "{e}");
    assert_eq!(call_count(&f, "claude"), 1);
    assert_eq!(
        (call_count(&f, "codex"), call_count(&f, "agy")),
        (0, 0),
        "fast calls one agent"
    );
    assert!(!out(&o).contains("[plan]"), "stdout stays only the answer");
}

#[test]
fn limits_that_cannot_be_met_are_rejected_before_any_agent_is_called() {
    let f = consult_fixture();
    let o = f.run(&[
        "consult",
        PRODUCT_Q,
        "--strategy",
        "auto",
        "--agents",
        "claude,codex,google",
        "--rounds",
        "2",
        "--max-agent-calls",
        "5",
    ]);
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    let e = err(&o);
    assert!(
        e.contains("at least 10 agent call(s)") && e.contains("--max-agent-calls is 5"),
        "{e}"
    );
    // The fixed workflow (strategy `off` in the config) checks its limits the same way.
    let o = f.run(&["consult", PRODUCT_Q, "--max-agent-calls", "5"]);
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    assert!(
        err(&o).contains("needs 7 agent call(s) and --max-agent-calls is 5"),
        "{}",
        err(&o)
    );
    for args in [
        &["--max-agent-calls", "0"][..],
        &["--max-duration", "0"][..],
        &["--max-parallel", "0"][..],
        &["--max-duration", "999999"][..],
        &["--strategy", "turbo"][..],
    ] {
        let mut all = vec!["consult", PRODUCT_Q];
        all.extend_from_slice(args);
        let o = f.run(&all);
        assert_eq!(o.status.code(), Some(2), "{args:?}: {}", err(&o));
    }
    for agent in ["claude", "codex", "agy"] {
        assert_eq!(call_count(&f, agent), 0, "{agent}");
    }
}

#[test]
fn without_a_flag_the_default_strategy_is_auto_and_the_config_can_switch_it_off() {
    let f = consult_fixture();
    // The fixture's config says `off`: the fixed workflow, no smart plan.
    let v = json(&f.run(&["consult", PRODUCT_Q, "--dry-run", "--output", "json"]));
    assert!(
        v.get("smart").is_none(),
        "off keeps the fixed Milestone 4/5 workflow"
    );
    assert_eq!(v["participants"]["value"].as_array().unwrap().len(), 3);
    // Without the override the built-in default (auto) applies.
    f.use_default_strategy();
    let v = json(&f.run(&["consult", PRODUCT_Q, "--dry-run", "--output", "json"]));
    assert_eq!(v["smart"]["strategy"]["source"], "auto");
    assert_eq!(v["smart"]["strategy"]["value"], "balanced");
    // An explicit flag beats whatever the configuration says.
    let v = json(&f.run(&[
        "consult",
        PRODUCT_Q,
        "--strategy",
        "thorough",
        "--dry-run",
        "--output",
        "json",
    ]));
    assert_eq!(v["smart"]["strategy"]["value"], "thorough");
    assert_eq!(v["rounds"]["value"], 2);
}

#[test]
fn a_smart_consultation_saves_its_trace_and_show_prints_it() {
    let f = consult_fixture();
    let o = f.run(&[
        "consult",
        PRODUCT_Q,
        "--strategy",
        "balanced",
        "--output",
        "json",
    ]);
    assert!(o.status.success(), "{}", err(&o));
    let v = json(&o);
    let orch = &v["consultation"]["orchestration"];
    assert_eq!(v["consultation"]["strategy"], "adaptive");
    assert!(orch["events"].as_array().unwrap().len() >= 3, "{orch}");
    assert!(orch["stop_reason"].is_string());
    assert_eq!(orch["metrics"]["calls"], v["calls"]);
    assert!(orch["quality"]["checks"].is_array());
    assert!(
        !serde_json::to_string(orch).unwrap().contains("microscope"),
        "the trace holds no question text"
    );
    let id = v["id"].as_str().unwrap();
    let shown = out(&f.run(&["show", id]));
    assert!(
        shown.contains("Orchestration (what happened while it ran):"),
        "{shown}"
    );
    assert!(shown.contains("strategy:     balanced"), "{shown}");
    // The provider history was written next to the saved debates, with durations only.
    let profiles = f
        .dir
        .path()
        .join("data")
        .join("ai-orchestra")
        .join("profiles.json");
    let text = fs::read_to_string(&profiles).expect("a provider history file");
    assert!(
        text.contains("\"invocations\"") && !text.contains("microscope"),
        "{text}"
    );
}

#[test]
fn the_time_limit_stops_a_slow_agent_process_and_keeps_the_finished_answer() {
    let f = consult_fixture();
    // codex would review for 30 s; the whole consultation may take 3.
    fs::write(f.dir.path().join("codex.slow"), "").unwrap();
    let started = std::time::Instant::now();
    let o = f.run(&[
        "consult",
        PRODUCT_Q,
        "--strategy",
        "balanced",
        "--max-duration",
        "3",
        "--quiet",
    ]);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(25),
        "took {:?}",
        started.elapsed()
    );
    assert_eq!(o.status.code(), Some(5), "a partial result: {}", err(&o));
    let e = err(&o);
    // The process was stopped either by its own timeout (clipped to what was left of the 3 s) or
    // by the deadline itself, whichever came first.
    assert!(
        e.contains("PARTIAL RESULT") && (e.contains("timed out after") || e.contains("time limit")),
        "{e}"
    );
    assert_eq!(call_count(&f, "claude"), 1, "the lead answered");
    assert_eq!(
        call_count(&f, "agy"),
        0,
        "nothing else was started after the deadline"
    );
    assert!(
        out(&o).contains("PROPOSAL from claude"),
        "the lead's answer is kept: {}",
        out(&o)
    );
}

#[test]
fn no_save_writes_neither_the_debate_nor_the_provider_history() {
    let f = consult_fixture();
    let o = f.run(&[
        "consult",
        PRODUCT_Q,
        "--strategy",
        "fast",
        "--no-save",
        "--quiet",
    ]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(
        !f.dir
            .path()
            .join("data")
            .join("ai-orchestra")
            .join("profiles.json")
            .exists()
    );
    assert!(out(&f.run(&["history"])).contains("No saved debates yet"));
}

#[test]
fn consult_manual_settings_override_every_automatic_choice() {
    let f = consult_fixture();
    let o = f.run(&[
        "consult",
        PRODUCT_Q,
        "--mode",
        "learn",
        "--coordinator",
        "codex",
        "--agents",
        "codex,google",
        "--rounds",
        "2",
    ]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&o).contains("SYNTHESIS by codex"));
    let e = err(&o);
    for needle in [
        "mode:         learn (manual:",
        "coordinator:  codex (manual:",
        "participants: codex, google (manual:",
        "rounds:       2 (manual:",
    ] {
        assert!(e.contains(needle), "missing `{needle}` in:\n{e}");
    }
    assert_eq!(
        roles(&f, "codex"),
        ["perspective", "review", "cross", "synthesis"]
    );
    assert_eq!(roles(&f, "agy"), ["perspective", "review", "cross"]);
    assert_eq!(
        call_count(&f, "claude"),
        0,
        "an agent that was not selected is not called"
    );
    let v = json(&f.run(&["show", &saved_id(&f), "--output", "json"]));
    assert_eq!(
        v["consultation"]["manual_overrides"],
        serde_json::json!(["mode", "coordinator", "agents", "rounds"])
    );
}

#[test]
fn consult_with_one_agent_makes_one_call_and_says_there_was_no_review() {
    let f = consult_fixture();
    let o = f.run(&["consult", PRODUCT_Q, "--agents", "google"]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&o).contains("SINGLE by agy"));
    assert!(err(&o).contains("no independent review"), "{}", err(&o));
    assert_eq!(roles(&f, "agy"), ["single"]);
    assert_eq!(call_count(&f, "claude") + call_count(&f, "codex"), 0);
}

#[test]
fn a_short_lesson_takes_the_lightweight_path_and_the_shorthand_works() {
    let f = consult_fixture();
    // The shorthand: a quoted multi-word question without the subcommand.
    let o = f.run(&["Explain what a resistor does"]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&o).contains("SINGLE by claude"), "{}", out(&o));
    assert_eq!(roles(&f, "claude"), ["single"]);
    assert_eq!(
        call_count(&f, "codex") + call_count(&f, "agy"),
        0,
        "one agent is enough here"
    );
    assert!(
        err(&o).contains("a short, uncontested teaching question"),
        "{}",
        err(&o)
    );

    // Options after the question, and global options before it, behave as with `consult`.
    let o = f.run(&["Compare Rust and C++", "--mode", "learn", "--plan"]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(
        out(&o).contains("mode:         learn (manual:"),
        "{}",
        out(&o)
    );
    let o = f
        .cmd()
        .args(["Compare Rust and C++", "--plan", "-q"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", err(&o));

    // A mistyped subcommand is an error and never spends quota.
    let before = call_count(&f, "claude");
    let o = f.run(&["docter"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(
        err(&o).contains("unrecognized subcommand 'docter'"),
        "{}",
        err(&o)
    );
    assert_eq!(call_count(&f, "claude"), before);
}

#[test]
fn consult_input_errors_use_the_usage_exit_code_and_call_no_agent() {
    let f = consult_fixture();
    for args in [
        vec!["consult", "q", "--mode", "poetry"],
        vec!["consult", "q", "--rounds", "3"],
        vec!["consult", "q", "--rounds", "0"],
        vec!["consult", "q", "--rounds", "many"],
        vec!["consult", "q", "--agents", "claude,nobody"],
        vec!["consult", "q", "--agents", "auto,claude"],
        vec!["consult", "q", "--coordinator", "nobody"],
        vec![
            "consult",
            "q",
            "--coordinator",
            "codex",
            "--agents",
            "claude,google",
        ],
        vec!["consult", "q", "--timeout", "0"],
        vec!["consult", "q", "--model", "claude"],
        vec!["consult", "   "],
        vec!["consult", "q", "--output", "xml"],
    ] {
        let o = f.run(&args);
        assert_eq!(o.status.code(), Some(2), "{args:?}: {}", err(&o));
    }
    // A debate cannot be run with one agent.
    let o = f.run(&["consult", "q", "--mode", "debate", "--agents", "claude"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("at least two"), "{}", err(&o));
    for agent in ["claude", "codex", "agy"] {
        assert_eq!(call_count(&f, agent), 0, "{agent}");
    }
}

#[test]
fn consult_debate_mode_runs_the_debate_engine_and_is_listed_next_to_plain_debates() {
    let f = consult_fixture();
    let o = f.run(&["consult", "Tabs or spaces?", "--mode", "debate", "--quiet"]);
    assert!(o.status.success(), "{}", err(&o));
    // The Milestone 2 workflow: proposal, reviews, synthesis; no independent perspectives.
    assert_eq!(roles(&f, "claude"), ["proposal", "synthesis"]);
    assert_eq!(roles(&f, "codex"), ["review"]);
    let o = f.run(&["debate", "Plain debate?", "--quiet"]);
    assert!(o.status.success(), "{}", err(&o));
    let table = out(&f.run(&["history"]));
    assert!(
        table.contains("KIND") && table.contains("debate"),
        "{table}"
    );
    let listing = json(&f.run(&["history", "--output", "json"]));
    let modes: Vec<_> = listing
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["mode"].clone())
        .collect();
    assert!(
        modes.contains(&serde_json::json!("debate")) && modes.contains(&serde_json::Value::Null),
        "{modes:?}"
    );
}

#[test]
fn consult_json_output_is_the_typed_record() {
    let f = consult_fixture();
    let o = f.run(&["consult", PRODUCT_Q, "--output", "json", "--quiet"]);
    assert!(o.status.success(), "{}", err(&o));
    let v = json(&o);
    assert_eq!(v["status"], "complete");
    assert_eq!(v["consultation"]["routing"]["mode"]["value"], "decision");
    assert_eq!(v["steps"].as_array().unwrap().len(), 7);
    assert!(
        v["final_answer"]
            .as_str()
            .unwrap()
            .contains("SYNTHESIS by claude")
    );
    assert!(v["steps"][0]["usage"].is_null() || v["steps"][0]["usage"].is_object());
}

// ---- consult: failures never look like success ----------------------------------------

#[test]
fn a_participant_out_of_quota_makes_the_consultation_partial_and_is_not_retried() {
    let f = consult_fixture();
    behave(&f, "agy", "quota");
    let o = f.run(&["consult", PRODUCT_Q]);
    assert_eq!(o.status.code(), Some(5), "{}", err(&o));
    assert!(
        out(&o).contains("SYNTHESIS by claude"),
        "the synthesis is still written"
    );
    assert!(err(&o).contains("PARTIAL RESULT"), "{}", err(&o));
    assert_eq!(
        roles(&f, "agy"),
        ["perspective"],
        "no retry, and not asked to review"
    );
    assert_eq!(roles(&f, "claude"), ["perspective", "review", "synthesis"]);
    assert_eq!(
        json(&f.run(&["show", &saved_id(&f), "--output", "json"]))["status"],
        "partial"
    );
}

#[test]
fn a_failed_coordinator_leaves_a_labelled_partial_answer_not_a_synthesis() {
    let f = consult_fixture();
    behave(&f, "claude", "failall");
    let o = f.run(&["consult", PRODUCT_Q]);
    assert_eq!(o.status.code(), Some(5), "{}", err(&o));
    assert!(out(&o).contains("PERSPECTIVE from codex"), "{}", out(&o));
    assert!(!out(&o).contains("SYNTHESIS"), "{}", out(&o));
    assert!(err(&o).contains("NOT a synthesis"), "{}", err(&o));
    assert_eq!(
        roles(&f, "claude"),
        ["perspective"],
        "no retry and no synthesis attempt"
    );
    assert!(out(&f.run(&["show", &saved_id(&f)])).contains("NOT a synthesis"));
}

#[test]
fn if_every_agent_fails_the_consultation_fails_with_the_cause_exit_code() {
    let f = consult_fixture();
    for agent in ["claude", "codex", "agy"] {
        behave(&f, agent, "failall");
    }
    let o = f.run(&["consult", PRODUCT_Q, "--quiet"]);
    assert_eq!(
        o.status.code(),
        Some(3),
        "auth failure exit code: {}",
        err(&o)
    );
    assert_eq!(
        json(&f.run(&["show", &saved_id(&f), "--output", "json"]))["status"],
        "failed"
    );
}

#[test]
fn an_unavailable_agent_is_reported_and_the_rest_still_consult() {
    let f = Fixture::build(
        [
            &consult_claude_script(),
            &consult_codex_script(),
            &consult_agy_script(),
        ],
        [None, None, Some("/definitely/not/installed")],
    );
    let o = f.run(&["consult", PRODUCT_Q, "--agents", "claude,codex,google"]);
    assert!(o.status.success(), "{}", err(&o));
    let e = err(&o);
    assert!(
        e.contains("not used:     google (requested but unavailable"),
        "{e}"
    );
    assert!(e.contains("not consulted: Google"), "{e}");
    assert_eq!(call_count(&f, "claude"), 3);
    assert_eq!(call_count(&f, "codex"), 2);
}

fn consult_claude_script() -> String {
    debate_claude().replace(&role_case("claude"), &consult_case("claude"))
}
fn consult_codex_script() -> String {
    debate_codex().replace(&role_case("codex"), &consult_case("codex"))
}
fn consult_agy_script() -> String {
    debate_agy().replace(&role_case("agy"), &consult_case("agy"))
}

#[test]
fn a_consultation_timeout_terminates_the_agent_and_is_partial() {
    let f = consult_fixture();
    behave(&f, "codex", "slow");
    let t = Instant::now();
    let o = f.run(&["consult", PRODUCT_Q, "--timeout", "2", "--quiet"]);
    assert_eq!(o.status.code(), Some(5), "{}", err(&o));
    assert!(
        t.elapsed() < Duration::from_secs(20),
        "the slow agent was terminated: {:?}",
        t.elapsed()
    );
    assert!(err(&o).contains("codex failed"), "{}", err(&o));
    let v = json(&f.run(&["show", &saved_id(&f), "--output", "json"]));
    let failed: Vec<_> = v["steps"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["ok"] == false)
        .collect();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0]["error_kind"], "timeout");
}

#[test]
fn ctrl_c_cancels_a_consultation_terminates_agents_and_records_it_as_cancelled() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    let f = consult_fixture();
    behave(&f, "claude", "slow");
    let mut child = f
        .cmd()
        .args(["consult", PRODUCT_Q])
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stderr.take().unwrap());
    let started = Instant::now();
    let mut line = String::new();
    loop {
        line.clear();
        assert!(
            lines.read_line(&mut line).unwrap() > 0,
            "stderr closed early"
        );
        if line.contains("Forming its own answer") {
            break;
        }
    }
    let status = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    let exit = child.wait().unwrap();
    assert_eq!(exit.code(), Some(130), "cancelled");
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "agents were terminated, not waited for"
    );
    let v = json(&f.run(&["show", &saved_id(&f), "--output", "json"]));
    assert_eq!(v["status"], "cancelled");
    assert!(v["error"].as_str().unwrap().contains("cancelled"));
    assert_eq!(v["consultation"]["mode"], "decision");
}

// ---- consult: safety ---------------------------------------------------------------------

#[test]
fn consult_inside_claude_code_never_calls_claude_and_refuses_it_as_coordinator() {
    let f = consult_fixture();
    let o = f
        .cmd()
        .args(["consult", PRODUCT_Q, "--quiet"])
        .env("CLAUDECODE", "1")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(
        call_count(&f, "claude"),
        0,
        "the running agent must never be called"
    );
    assert!(
        out(&o).contains("SYNTHESIS by codex"),
        "another agent coordinates: {}",
        out(&o)
    );
    let o = f
        .cmd()
        .args(["consult", PRODUCT_Q, "--coordinator", "claude"])
        .env("CLAUDECODE", "1")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("cannot be used"), "{}", err(&o));
    assert_eq!(call_count(&f, "claude"), 0);
}

#[test]
fn consult_agents_are_read_only_isolated_markers_and_never_see_api_keys() {
    let f = consult_fixture();
    let o = f.run(&["consult", PRODUCT_Q, "--quiet"]);
    assert!(o.status.success(), "{}", err(&o));
    for agent in ["claude", "codex", "agy"] {
        for marker in fs::read_to_string(f.dir.path().join(format!("{agent}.active")))
            .unwrap()
            .lines()
        {
            assert_eq!(marker, "1", "{agent} runs marked as orchestrated");
        }
    }
    let codex = call_lines(&f, "codex").join("\n");
    assert!(codex.contains("--sandbox read-only"), "{codex}");
    assert!(!codex.contains("danger"), "{codex}");
    let claude = call_lines(&f, "claude").join("\n");
    assert!(
        !claude.contains("bypassPermissions") && !claude.contains("dangerously"),
        "{claude}"
    );
}

#[test]
fn consult_overhead_with_instant_fake_agents_is_small() {
    let f = consult_fixture();
    let args = [
        "consult",
        PRODUCT_Q,
        "--rounds",
        "2",
        "--quiet",
        "--no-save",
        "--output",
        "json",
    ];
    assert!(f.run(&args).status.success());
    let mut walls = Vec::new();
    let mut inner = Vec::new();
    for _ in 0..7 {
        let t = Instant::now();
        let o = f.run(&args);
        walls.push(t.elapsed());
        assert!(o.status.success(), "{}", err(&o));
        inner.push(json(&o)["duration_ms"].as_u64().unwrap());
    }
    walls.sort();
    inner.sort();
    eprintln!(
        "PERF consult e2e: 10 calls to 3 fake shell CLIs (3 agents, 2 rounds), 7 samples: \
         process wall min {:?} / median {:?} / max {:?}; engine time min {} / median {} / max {} ms",
        walls[0], walls[3], walls[6], inner[0], inner[3], inner[6]
    );
    assert!(walls[3] < Duration::from_secs(5), "{:?}", walls[3]);
}

#[test]
fn output_is_kept_when_a_cli_leaves_a_background_process_holding_its_output() {
    // Seen with the real Codex: the CLI exited but something it started kept stdout open, the
    // reader was abandoned after the grace period and the whole reply was thrown away.
    let codex = r#"#!/bin/sh
case "$1" in
  --version) echo "codex-cli 9.9.9"; exit 0;;
  login) echo "Logged in using ChatGPT"; exit 0;;
esac
prompt=$(cat)
printf '{"type":"thread.started","thread_id":"t1"}\n'
printf '{"type":"item.completed","item":{"id":"1","type":"agent_message","text":"codex saw: %s"}}\n' "$prompt"
printf '{"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":2}}\n'
sleep 6 &
"#;
    let f = Fixture::with_scripts([MOCK_CLAUDE, codex, MOCK_AGY]);
    let t = Instant::now();
    let o = f.run(&["ask", "codex", "hi"]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&o).contains("codex saw: hi"), "{}", out(&o));
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
}

#[test]
fn consult_length_is_automatic_for_short_answer_questions_and_can_be_forced() {
    let f = consult_fixture();
    let plan = |args: &[&str]| out(&f.run(args));
    let t = plan(&["consult", "In two sentences: tabs or spaces?", "--plan"]);
    assert!(
        t.contains("length:       brief (auto: the question asks for a short answer"),
        "{t}"
    );
    let t = plan(&[
        "consult",
        "In two sentences: tabs or spaces?",
        "--plan",
        "--length",
        "full",
    ]);
    assert!(
        t.contains("length:       full (manual: requested with --length"),
        "{t}"
    );
    let t = plan(&[
        "consult",
        "Which electronics microscope should I buy?",
        "--plan",
        "--length",
        "brief",
    ]);
    assert!(t.contains("length:       brief (manual:"), "{t}");
    let t = plan(&["consult", PRODUCT_Q, "--plan"]);
    assert!(t.contains("length:       full (auto:"), "{t}");
    let o = f.run(&["consult", "q", "--length", "huge"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("unknown length"), "{}", err(&o));
    assert_eq!(
        call_count(&f, "claude") + call_count(&f, "codex") + call_count(&f, "agy"),
        0
    );
}

// ---- research (Milestone 5) -----------------------------------------------------------------

#[test]
fn capabilities_lists_what_each_cli_can_do_and_how_well_that_is_known() {
    let f = consult_fixture();
    let o = f.run(&["capabilities"]);
    assert!(o.status.success(), "{}", err(&o));
    let text = out(&o);
    for needle in [
        "claude",
        "codex",
        "google",
        "documented",
        "unverified",
        "unsupported",
        "retrieves the pages itself",
        "codex --search exec",
        "--allowedTools WebSearch,WebFetch",
        "--dangerously-skip-permissions` is never used",
    ] {
        assert!(text.contains(needle), "missing `{needle}`:\n{text}");
    }
    let v = json(&f.run(&["capabilities", "--output", "json"]));
    let rows = v.as_array().unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0][0], "claude");
    assert_eq!(rows[2][1]["web_search"], "unsupported");
    for agent in ["claude", "codex", "agy"] {
        assert_eq!(call_count(&f, agent), 0, "capabilities sends nothing");
    }
}

#[test]
fn inspect_refuses_private_local_and_non_https_destinations_without_any_agent() {
    let f = consult_fixture();
    let o = f.run(&[
        "inspect",
        "https://127.0.0.1/admin",
        "https://169.254.169.254/latest/meta-data/",
        "file:///etc/passwd",
        "http://example.com/page",
        "https://localhost/x",
        "https://user:pw@example.com/",
    ]);
    assert_eq!(
        o.status.code(),
        Some(1),
        "nothing could be retrieved: {}",
        err(&o)
    );
    let text = out(&o);
    assert_eq!(text.matches("status:     Blocked").count(), 6, "{text}");
    for needle in [
        "a loopback address",
        "cloud metadata services",
        "scheme `file` is not allowed",
        "scheme `http` is not allowed",
        "an internal host name",
        "user name or password",
    ] {
        assert!(text.contains(needle), "missing `{needle}`:\n{text}");
    }
    let json = json(&f.run(&["inspect", "https://10.0.0.1/", "--output", "json"]));
    assert_eq!(json[0]["status"], "blocked");
    assert_eq!(json[0]["error_kind"], "blocked_address");
    assert!(
        f.run(&["inspect"]).status.code() == Some(2),
        "at least one URL is required"
    );
    assert_eq!(
        call_count(&f, "claude") + call_count(&f, "codex") + call_count(&f, "agy"),
        0
    );
}

#[test]
fn plan_shows_the_research_decisions_and_manual_settings_win() {
    let f = consult_fixture();
    let plan = |args: &[&str]| {
        let mut a = vec!["consult"];
        a.extend_from_slice(args);
        a.push("--plan");
        let o = f.run(&a);
        assert!(o.status.success(), "{args:?}: {}", err(&o));
        out(&o)
    };
    let t = plan(&["What is the latest firmware version of the Model X oscilloscope?"]);
    assert!(t.contains("research:     on (auto:") && t.contains("depth standard (auto:") && t.contains("claude searches the web; every page it reports is fetched and checked independently"), "{t}");
    assert!(t.contains("agent calls:  at most "), "{t}");
    let t = plan(&["Explain how a transistor works"]);
    assert!(
        t.contains("research:     off (auto: a basic question that does not need outside facts)"),
        "{t}"
    );
    let t = plan(&["Explain how a transistor works", "--web", "off"]);
    assert!(
        t.contains("research:     off (manual:") && t.contains("limitation:   web research is off"),
        "{t}"
    );
    let t = plan(&[
        "Explain how a transistor works",
        "--web",
        "on",
        "--depth",
        "deep",
        "--max-sources",
        "4",
    ]);
    assert!(
        t.contains("research:     on (manual: requested with --web on)")
            && t.contains("depth deep (manual:")
            && t.contains("up to 4 source(s)"),
        "{t}"
    );
    assert!(
        t.contains("rounds:       2 (auto: deep research"),
        "deep research uses two rounds by default: {t}"
    );
    let t = plan(&["Latest news?", "--depth", "quick"]);
    assert!(
        t.contains("rounds:       0 (auto: quick research")
            && t.contains("participants: claude, codex"),
        "{t}"
    );
    let t = plan(&[
        "Summarize this study",
        "--url",
        "https://example.org/study",
        "--url",
        "https://example.org/other",
    ]);
    assert!(
        t.contains("sources:      2 URL(s) provided by you")
            && t.contains("research:     on (auto: URLs were provided"),
        "{t}"
    );
    assert!(
        t.contains("search:       none") || !t.contains("searches the web; every page it reports"),
        "with web auto and URLs given, only those URLs are used: {t}"
    );
    // Not every provider can search: google never does.
    let t = plan(&[
        "Latest news?",
        "--agents",
        "google",
        "--url",
        "https://example.org/a",
    ]);
    assert!(
        t.contains("no agent searches the web; only the provided URLs are used"),
        "{t}"
    );
    for agent in ["claude", "codex", "agy"] {
        assert_eq!(call_count(&f, agent), 0, "--plan calls nothing");
    }
}

#[test]
fn research_option_errors_use_the_usage_exit_code_and_call_no_agent() {
    let f = consult_fixture();
    for args in [
        vec!["consult", "q", "--web", "maybe"],
        vec!["consult", "q", "--depth", "infinite"],
        vec!["consult", "q", "--max-sources", "0"],
        vec!["consult", "q", "--max-sources", "21"],
        vec!["consult", "q", "--freshness-days", "0"],
        vec!["consult", "q", "--url", "http://example.org/plain-http"],
        vec!["consult", "q", "--url", "https://127.0.0.1/x"],
        vec!["consult", "q", "--url", "file:///etc/passwd"],
        vec![
            "consult",
            "q",
            "--web",
            "off",
            "--url",
            "https://example.org/a",
        ],
        vec!["consult", "q", "--web", "on", "--agents", "google"],
        vec!["consult", "q", "--verify", "--web", "off"],
    ] {
        let o = f.run(&args);
        assert_eq!(o.status.code(), Some(2), "{args:?}: {}", err(&o));
    }
    let o = f.run(&["consult", "q", "--web", "on", "--agents", "google"]);
    assert!(
        err(&o).contains("--web on needs a way to find or fetch sources"),
        "{}",
        err(&o)
    );
    for agent in ["claude", "codex", "agy"] {
        assert_eq!(call_count(&f, agent), 0, "{agent}");
    }
}

#[test]
fn a_search_agent_that_returns_no_sources_leaves_a_partial_result_that_says_nothing_was_checked() {
    // The fake CLIs answer the planning request with plain text, not a source list, and report no
    // search results: nothing can be retrieved, and the result must not look verified.
    let f = consult_fixture();
    let o = f.run(&[
        "consult",
        "What is the latest firmware version of the Model X oscilloscope?",
        "--web",
        "on",
        "--agents",
        "claude,codex",
        "--quiet",
    ]);
    assert_eq!(o.status.code(), Some(5), "{}", err(&o));
    let text = out(&o);
    assert!(
        text.contains("No source could be retrieved")
            && text.contains("nothing in this answer is backed by a retrieved source")
            && text.contains("## Research limitations"),
        "{text}"
    );
    assert!(!text.contains("SUPPORTED"));
    assert!(
        err(&o).contains("research: the search agent (claude) returned no usable source URLs"),
        "{}",
        err(&o)
    );
    // The planning call (and only it) enabled the web tools, with the documented flags.
    let claude: Vec<String> = call_lines(&f, "claude");
    let planning: Vec<&String> = claude
        .iter()
        .filter(|l| l.contains("--allowedTools"))
        .collect();
    assert_eq!(planning.len(), 1, "{claude:?}");
    assert!(
        planning[0].contains("--tools Read,Grep,Glob,WebSearch,WebFetch")
            && planning[0].contains("stream-json")
            && planning[0].contains("--verbose"),
        "{}",
        planning[0]
    );
    assert!(
        !claude.iter().any(|l| l.contains("bypassPermissions")
            || l.contains("dangerously")
            || l.contains("Bash")),
        "{claude:?}"
    );
    let codex = call_lines(&f, "codex");
    assert!(
        codex.iter().all(|l| !l.starts_with("--search")),
        "codex was not asked to search: {codex:?}"
    );
    // Saved with its research report.
    let v = json(&f.run(&["show", &saved_id(&f), "--output", "json"]));
    assert_eq!(v["status"], "partial");
    assert_eq!(
        v["consultation"]["research"]["sources"],
        serde_json::json!([])
    );
    assert!(
        v["consultation"]["research"]["limitations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l.as_str().unwrap().contains("no source could be retrieved"))
    );
}

#[test]
fn codex_searches_with_the_global_flag_before_exec_when_it_is_the_search_agent() {
    let f = consult_fixture();
    // Inside Claude Code the claude agent is unavailable, so codex is the search agent.
    let o = f
        .cmd()
        .args([
            "consult",
            "What is the latest firmware version of the Model X oscilloscope?",
            "--web",
            "on",
            "--quiet",
        ])
        .env("CLAUDECODE", "1")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(5), "{}", err(&o));
    let codex = call_lines(&f, "codex");
    let search: Vec<&String> = codex
        .iter()
        .filter(|l| l.starts_with("--search exec"))
        .collect();
    assert_eq!(search.len(), 1, "{codex:?}");
    assert!(search[0].contains("--sandbox read-only"), "{}", search[0]);
    assert_eq!(call_count(&f, "claude"), 0);
}

#[test]
fn consult_without_research_keeps_working_exactly_as_before() {
    let f = consult_fixture();
    let o = f.run(&["consult", PRODUCT_Q, "--web", "off", "--quiet"]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&o).contains("SYNTHESIS by claude"));
    for line in call_lines(&f, "claude")
        .iter()
        .chain(&call_lines(&f, "codex"))
    {
        assert!(
            !line.contains("Web") && !line.starts_with("--search"),
            "{line}"
        );
    }
}

// ------------------------------------------------------------------------------------------------
// Milestone 7: memory and context (CLI). Memory is pinned off in `Fixture`; these tests switch it on.

const SHOP_Q1: &str =
    "Research three portable oscilloscopes for learning electronics and compare them.";
const SHOP_Q2: &str = "Of the three above, which one is the best value for money?";
const SHOP_PRODUCTS: &str = "Rigol DHO804, Siglent SDS1104X-E and Hantek DSO2D15";

/// A fixture whose claude records every prompt it receives and answers a single-agent question with
/// a known product list.
fn memory_fixture() -> Fixture {
    let record = |script: String| {
        script.replace(
            "prompt=$(cat)\n",
            "prompt=$(cat)\nprintf '%s\\n=====\\n' \"$prompt\" >> \"$MOCK_DIR/claude.prompts\"\n",
        )
    };
    let claude = record(consult_claude_script()).replace(
        "out=\"SINGLE by claude\"",
        &format!("out=\"The oscilloscopes are the {SHOP_PRODUCTS}. SINGLE by claude\""),
    );
    let f = Fixture::with_scripts([&claude, &consult_codex_script(), &consult_agy_script()]);
    f.use_memory();
    f
}

fn prompts_sent(f: &Fixture) -> Vec<String> {
    fs::read_to_string(f.dir.path().join("claude.prompts"))
        .unwrap_or_default()
        .split("\n=====\n")
        .filter(|p| !p.trim().is_empty())
        .map(String::from)
        .collect()
}

fn single_args<'a>(question: &'a str, extra: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec![
        "consult",
        question,
        "--agents",
        "claude",
        "--coordinator",
        "claude",
        "--strategy",
        "fast",
        "--web",
        "off",
        "--quiet",
    ];
    args.extend_from_slice(extra);
    args
}

fn memory_db(f: &Fixture) -> PathBuf {
    f.dir
        .path()
        .join("data")
        .join("ai-orchestra")
        .join("memory.db")
}

#[test]
fn a_follow_up_consultation_receives_the_earlier_result_and_can_explain_why() {
    let f = memory_fixture();
    let o = f.run(&single_args(SHOP_Q1, &[]));
    assert!(o.status.success(), "{}", err(&o));
    assert!(
        memory_db(&f).exists(),
        "the first consultation was remembered"
    );
    assert!(!prompts_sent(&f)[0].contains("<memory_context"));

    let o = f.run(&single_args(SHOP_Q2, &["--explain-context"]));
    assert!(o.status.success(), "{}", err(&o));
    let prompts = prompts_sent(&f);
    assert_eq!(prompts.len(), 2);
    assert!(
        prompts[1].contains("<memory_context") && prompts[1].contains("Hantek DSO2D15"),
        "{}",
        prompts[1]
    );
    assert!(prompts[1].contains(SHOP_Q2));
    let e = err(&o);
    assert!(e.contains("context:") && e.contains("estimated"), "{e}");
    assert!(e.contains("item(s) of"), "{e}");
    assert!(!e.contains("api"), "no secrets or keys in the report");
}

#[test]
fn memory_off_sends_no_context_and_writes_nothing() {
    let f = memory_fixture();
    assert!(
        f.run(&single_args(SHOP_Q1, &["--memory", "off"]))
            .status
            .success()
    );
    assert!(
        f.run(&single_args(SHOP_Q2, &["--memory", "off"]))
            .status
            .success()
    );
    assert!(
        prompts_sent(&f)
            .iter()
            .all(|p| !p.contains("<memory_context"))
    );
    assert!(!memory_db(&f).exists(), "nothing was created");
}

#[test]
fn no_save_reads_memory_but_stores_nothing_new() {
    let f = memory_fixture();
    assert!(f.run(&single_args(SHOP_Q1, &[])).status.success());
    let before = json(&f.run(&["memory", "stats", "--output", "json"]));
    assert!(
        f.run(&single_args(SHOP_Q2, &["--no-save"]))
            .status
            .success()
    );
    assert!(
        prompts_sent(&f)[1].contains("Hantek DSO2D15"),
        "still reads"
    );
    let after = json(&f.run(&["memory", "stats", "--output", "json"]));
    assert_eq!(before["consultations"], after["consultations"]);
    assert_eq!(before["memories"], after["memories"]);
    // And the debate history was not written either.
    let history = json(&f.run(&["history", "--output", "json"]));
    assert_eq!(history.as_array().unwrap().len(), 1);
}

#[test]
fn the_plan_can_show_the_recalled_context_without_calling_an_agent() {
    let f = memory_fixture();
    assert!(f.run(&single_args(SHOP_Q1, &[])).status.success());
    let calls = prompts_sent(&f).len();
    let o = f.run(&[
        "consult",
        SHOP_Q2,
        "--plan",
        "--explain-context",
        "--strategy",
        "fast",
        "--web",
        "off",
    ]);
    assert!(o.status.success(), "{}", err(&o));
    let text = out(&o);
    assert!(text.contains("mode:") && text.contains("context:") && !text.contains("Hantek"));
    assert!(text.contains("item(s) of"), "{text}");
    assert_eq!(prompts_sent(&f).len(), calls, "no agent was called");
}

#[test]
fn memory_flags_that_cannot_be_honoured_are_usage_errors_before_any_agent_runs() {
    let f = memory_fixture();
    for extra in [
        vec!["--context-budget", "0"],
        vec!["--memory", "off", "--session", "ses-x"],
        vec!["--memory", "off", "--context-budget", "500"],
        vec!["--memory", "sometimes"],
        vec!["--session", "ses-doesnotexist"],
    ] {
        let o = f.run(&single_args(SHOP_Q1, &extra));
        assert_eq!(o.status.code(), Some(2), "{extra:?}: {}", err(&o));
    }
    assert!(prompts_sent(&f).is_empty(), "no agent was called");
}

#[test]
fn the_memory_commands_add_search_correct_forget_and_export() {
    let f = memory_fixture();
    let add = |text: &str, extra: &[&str]| {
        let mut args = vec!["memory", "add", text];
        args.extend_from_slice(extra);
        f.run(&args)
    };
    let o = add(
        "My oscilloscope budget is 5000 MXN",
        &["--kind", "preference", "--topic", "budget"],
    );
    assert!(o.status.success(), "{}", err(&o));
    let o = add(
        "My oscilloscope budget is 8000 MXN",
        &["--kind", "preference", "--topic", "budget"],
    );
    assert!(out(&o).contains("It replaces"), "{}", out(&o));

    let current = out(&f.run(&["memory", "list"]));
    assert!(
        current.contains("8000 MXN") && !current.contains("5000 MXN"),
        "{current}"
    );
    let all = out(&f.run(&["memory", "list", "--all-versions"]));
    assert!(
        all.contains("5000 MXN") && all.contains("[replaced]"),
        "{all}"
    );

    let found = json(&f.run(&["memory", "search", "budget", "--output", "json"]));
    assert_eq!(found.as_array().unwrap().len(), 1);
    let id = found[0]["record"]["id"].as_str().unwrap().to_string();

    let shown = out(&f.run(&["memory", "show", &id]));
    assert!(
        shown.contains("said by:       user") && shown.contains("trust:"),
        "{shown}"
    );

    let o = f.run(&[
        "memory",
        "correct",
        &id,
        "My oscilloscope budget is 9000 MXN",
    ]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&f.run(&["memory", "list"])).contains("9000 MXN"));

    let export = json(&f.run(&["memory", "export"]));
    assert_eq!(export["format"], "ai-orchestra-memory-export");
    assert!(!export["memories"].as_array().unwrap().is_empty());
    let file = f.dir.path().join("export.json");
    let o = f.run(&["memory", "export", "--file", file.to_str().unwrap()]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(
        fs::metadata(&file).unwrap().permissions().mode() & 0o077,
        0,
        "the export is owner-only"
    );

    let id9 = json(&f.run(&["memory", "search", "9000", "--output", "json"]))[0]["record"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(f.run(&["memory", "forget", &id9]).status.success());
    assert!(!out(&f.run(&["memory", "list"])).contains("9000 MXN"));
    let bytes = fs::read(memory_db(&f)).unwrap();
    assert!(
        !bytes.windows(8).any(|w| w == b"9000 MXN"),
        "the deleted text is gone from the file"
    );
}

#[test]
fn credentials_and_personal_data_are_refused_and_the_database_is_private() {
    let f = memory_fixture();
    let o = f.run(&[
        "memory",
        "add",
        "token sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789",
    ]);
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("credential"), "{}", err(&o));
    let o = f.run(&["memory", "add", "write to me at someone@example.com"]);
    assert_eq!(o.status.code(), Some(2));
    let o = f.run(&[
        "memory",
        "add",
        "write to me at someone@example.com",
        "--allow-sensitive",
    ]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(
        fs::metadata(memory_db(&f)).unwrap().permissions().mode() & 0o077,
        0,
        "owner-only file"
    );

    // A question holding a credential is answered, but nothing of it is stored.
    let o = f.run(&single_args(
        "Explain this setup, token sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789",
        &[],
    ));
    assert!(o.status.success(), "{}", err(&o));
    let export = out(&f.run(&["memory", "export"]));
    assert!(!export.contains("abcdefghijklmnopqrstuvwxyz"));
}

#[test]
fn clearing_a_persistent_scope_needs_confirmation() {
    let f = memory_fixture();
    assert!(
        f.run(&["memory", "add", "I prefer four channels"])
            .status
            .success()
    );
    let o = f.run(&["memory", "clear", "--scope", "project"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("--yes"));
    assert!(
        out(&f.run(&["memory", "list"])).contains("four channels"),
        "nothing was deleted"
    );
    assert!(
        f.run(&["memory", "clear", "--scope", "project", "--yes"])
            .status
            .success()
    );
    assert!(out(&f.run(&["memory", "list"])).contains("Nothing is remembered"));
}

#[test]
fn sessions_can_be_listed_shown_resumed_and_cleared_and_never_claim_provider_continuation() {
    let f = memory_fixture();
    assert!(out(&f.run(&["session", "list"])).contains("No sessions"));
    assert!(f.run(&single_args(SHOP_Q1, &[])).status.success());
    let sessions = json(&f.run(&["session", "list", "--output", "json"]));
    let id = sessions[0]["id"].as_str().unwrap().to_string();
    assert_eq!(sessions[0]["turns"], 1);

    let shown = out(&f.run(&["session", "show"]));
    assert!(shown.contains(&id) && shown.contains("1 turn"), "{shown}");

    let resumed = out(&f.run(&["session", "resume", &id]));
    assert!(
        resumed.contains("AI Orchestra's own session")
            && resumed.contains("no provider CLI session is resumed"),
        "{resumed}"
    );

    // An explicit session id continues that session.
    assert!(
        f.run(&single_args(SHOP_Q2, &["--session", &id]))
            .status
            .success()
    );
    assert!(prompts_sent(&f)[1].contains("Hantek DSO2D15"));
    assert_eq!(
        json(&f.run(&["session", "list", "--output", "json"]))[0]["turns"],
        2
    );

    let cleared = out(&f.run(&["session", "clear", &id]));
    assert!(cleared.contains("Cleared session"), "{cleared}");
    assert!(out(&f.run(&["memory", "list"])).contains("Nothing is remembered"));
}

#[test]
fn memory_status_states_the_privacy_position_and_the_provider_session_position() {
    let f = memory_fixture();
    let text = out(&f.run(&["memory", "status"]));
    assert!(
        text.contains("NOT encrypted at rest") && text.contains("no telemetry"),
        "{text}"
    );
    assert!(
        text.contains("Provider-native sessions are not used"),
        "{text}"
    );
    for flag in ["claude --resume", "codex exec resume", "agy --continue"] {
        assert!(
            text.contains(flag),
            "documented flag `{flag}` listed: {text}"
        );
    }
    assert!(text.contains("verified: false, used: false"));
    let v = json(&f.run(&["memory", "status", "--output", "json"]));
    assert_eq!(v["encrypted_at_rest"], false);
}

#[test]
fn a_damaged_memory_file_never_stops_a_consultation_and_can_be_set_aside() {
    let f = memory_fixture();
    assert!(f.run(&single_args(SHOP_Q1, &[])).status.success());
    fs::write(
        memory_db(&f),
        b"not a database, just some text that is long enough to look odd",
    )
    .unwrap();

    let o = f.run(&single_args(SHOP_Q2, &[]));
    assert!(o.status.success(), "{}", err(&o));
    assert!(err(&o).contains("warning: memory:"), "{}", err(&o));

    let o = f.run(&["memory", "check"]);
    assert_eq!(
        o.status.code(),
        Some(1),
        "an unhealthy file is reported: {}",
        out(&o)
    );
    assert!(out(&o).contains("PROBLEMS FOUND"));
    let o = f.run(&["memory", "check", "--quarantine"]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&o).contains("not deleted"), "{}", out(&o));
    let quarantined = fs::read_dir(memory_db(&f).parent().unwrap())
        .unwrap()
        .filter_map(Result::ok)
        .any(|e| e.file_name().to_string_lossy().contains("corrupt"));
    assert!(quarantined, "the damaged file was kept");
    assert!(out(&f.run(&["memory", "check"])).contains("healthy"));
}

#[test]
fn earlier_saved_consultations_can_be_imported_into_memory() {
    let f = memory_fixture();
    // Saved with memory off: only the debate history has it.
    assert!(
        f.run(&single_args(SHOP_Q1, &["--memory", "off"]))
            .status
            .success()
    );
    assert!(!memory_db(&f).exists());
    let o = f.run(&["memory", "import-history"]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&o).contains("Imported 1"), "{}", out(&o));
    assert!(
        out(&f.run(&["memory", "import-history"])).contains("Imported 0"),
        "importing twice adds nothing"
    );
    assert!(f.run(&single_args(SHOP_Q2, &[])).status.success());
    assert!(prompts_sent(&f).last().unwrap().contains("Hantek DSO2D15"));
}

#[test]
fn projects_are_separate_through_the_project_flag() {
    let f = memory_fixture();
    assert!(
        f.run(&[
            "--project",
            "alpha",
            "memory",
            "add",
            "alpha only: prefers Rigol"
        ])
        .status
        .success()
    );
    assert!(out(&f.run(&["--project", "alpha", "memory", "list"])).contains("alpha only"));
    assert!(
        out(&f.run(&["--project", "beta", "memory", "list"])).contains("Nothing is remembered")
    );
}

// ------------------------------------------------------------------------------------------------
// Milestone 8: the chat, history search, evaluation and feedback commands (CLI).

fn eval_run_id(f: &Fixture) -> String {
    let v = json(&f.run(&["eval", "list", "--output", "json"]));
    v["runs"][0]["id"].as_str().expect("a run").to_string()
}

fn total_agent_calls(f: &Fixture) -> usize {
    ["claude", "codex", "agy"]
        .iter()
        .map(|a| call_count(f, a))
        .sum()
}

#[test]
fn plain_ai_orchestra_without_a_terminal_still_prints_usage_and_exits_2() {
    let f = Fixture::new();
    let o = f.cmd().stdin(std::process::Stdio::null()).output().unwrap();
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    assert!(err(&o).contains("Usage"), "{}", err(&o));
    assert_eq!(total_agent_calls(&f), 0);
}

#[test]
fn chat_reads_a_scripted_conversation_from_a_pipe() {
    use std::io::Write;
    use std::process::Stdio;
    let f = consult_fixture();
    let mut child = f
        .cmd()
        .args([
            "chat",
            "--set",
            "strategy=fast",
            "--set",
            "agents=claude",
            "--set",
            "coordinator=claude",
            "--set",
            "web=off",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            b"/settings\nWhich electronics microscope should I buy?\n/why\n/mode nonsense\n/exit\n",
        )
        .unwrap();
    let o = child.wait_with_output().unwrap();
    assert!(o.status.success(), "{}", err(&o));
    let text = out(&o);
    assert!(
        text.contains("AI Orchestra") && text.contains("/help"),
        "{text}"
    );
    assert!(
        text.contains("strategy") && text.contains("fast") && text.contains("(this session)"),
        "{text}"
    );
    assert!(
        text.contains("SINGLE by claude") && text.contains("[complete"),
        "{text}"
    );
    assert!(
        text.contains("accepts: auto, decision"),
        "a bad value is refused with what it accepts: {text}"
    );
    assert_eq!(call_count(&f, "claude"), 1);
    assert_eq!(
        call_count(&f, "codex"),
        0,
        "the agent the user chose is the only one called"
    );
    assert!(
        err(&f_out_stub(&o)).contains("Mode:"),
        "the routing is shown on stderr"
    );
}

/// The stderr of `o` as an `Output` that `err` accepts.
fn f_out_stub(o: &Output) -> Output {
    Output {
        status: o.status,
        stdout: o.stderr.clone(),
        stderr: o.stderr.clone(),
    }
}

#[test]
fn history_search_finds_saved_consultations_by_their_words_and_calls_no_agent() {
    let f = consult_fixture();
    assert!(
        f.run(&[
            "consult",
            PRODUCT_Q,
            "--quiet",
            "--agents",
            "claude",
            "--coordinator",
            "claude",
            "--strategy",
            "fast",
            "--web",
            "off"
        ])
        .status
        .success()
    );
    let calls = total_agent_calls(&f);
    let o = f.run(&["history", "search", "microscope"]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&o).contains("microscope"), "{}", out(&o));
    let v = json(&f.run(&["history", "search", "microscope", "--output", "json"]));
    assert_eq!(v.as_array().unwrap().len(), 1);
    assert!(out(&f.run(&["history", "search", "zebra"])).contains("Nothing matches"));
    assert_eq!(total_agent_calls(&f), calls);
    // The plain listing is unchanged.
    assert!(out(&f.run(&["history"])).contains("ID"));
}

#[test]
fn offline_evaluation_runs_reports_and_exports_without_calling_any_provider() {
    let f = consult_fixture();
    let o = f.run(&["eval", "validate", "starter"]);
    assert!(
        o.status.success() && out(&o).contains("valid (12 case(s), 12 categories"),
        "{}",
        out(&o)
    );
    assert!(out(&f.run(&["eval", "list"])).contains("No evaluation runs yet"));
    let o = f.run(&["eval", "run", "starter", "--offline"]);
    assert!(o.status.success(), "{}", err(&o));
    let text = out(&o);
    assert!(
        text.contains("offline: no provider was called")
            && text.contains("READ THIS FIRST")
            && text.contains("fixture"),
        "{text}"
    );
    let id = eval_run_id(&f);
    assert!(out(&f.run(&["eval", "report", &id])).contains("== fixture results =="));
    let v = json(&f.run(&["eval", "report", &id, "--output", "json"]));
    assert!(v["summaries"].as_array().unwrap().len() >= 4);
    let exported = json(&f.run(&["eval", "export", &id]));
    assert_eq!(exported["format"], "ai-orchestra-eval-run");
    let file = f.dir.path().join("run.json");
    assert!(
        f.run(&["eval", "export", &id, "--file", file.to_str().unwrap()])
            .status
            .success()
    );
    assert_eq!(fs::metadata(&file).unwrap().permissions().mode() & 0o077, 0);
    let inspected = out(&f.run(&["eval", "inspect", "fact-ohm", "--run", &id]));
    assert!(
        inspected.contains("Ohm")
            && inspected.contains("factual_correctness")
            && inspected.contains("single:claude"),
        "{inspected}"
    );
    assert_eq!(total_agent_calls(&f), 0, "nothing offline calls an agent");

    // Choosing neither mode is a usage error, and so is an unknown arm or dataset.
    assert_eq!(f.run(&["eval", "run", "starter"]).status.code(), Some(2));
    assert_eq!(
        f.run(&["eval", "run", "starter", "--offline", "--arms", "turbo"])
            .status
            .code(),
        Some(2)
    );
    assert_eq!(
        f.run(&["eval", "run", "nonexistent.json", "--offline"])
            .status
            .code(),
        Some(2)
    );
    let ids = json(&f.run(&["eval", "list", "--output", "json"]));
    assert_eq!(
        ids["runs"].as_array().unwrap().len(),
        1,
        "failed commands saved nothing"
    );
    let runs_dir = f.dir.path().join("data").join("ai-orchestra").join("evals");
    let mode = fs::metadata(runs_dir.join(format!("{id}.json")))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o077, 0, "runs are owner-only");
}

#[test]
fn a_blind_comparison_hides_identities_until_the_report_and_keeps_the_judgments() {
    let f = consult_fixture();
    assert!(
        f.run(&["eval", "run", "starter", "--offline"])
            .status
            .success()
    );
    let id = eval_run_id(&f);
    let o = f.run(&[
        "eval",
        "blind",
        &id,
        "--pairs",
        "single-codex:thorough",
        "--seed",
        "5",
    ]);
    assert!(o.status.success(), "{}", err(&o));
    let packet = out(&o);
    assert!(
        packet.contains("Answer 1") && packet.contains("Answer 2") && packet.contains("item-001"),
        "{packet}"
    );
    for hidden in ["single:codex", "thorough", "fixture"] {
        assert!(!packet.contains(hidden), "the packet leaks `{hidden}`");
    }
    // The same seed prints the same packet again.
    assert_eq!(
        out(&f.run(&[
            "eval",
            "blind",
            &id,
            "--pairs",
            "single-codex:thorough",
            "--seed",
            "5"
        ])),
        packet
    );
    let o = f.run(&[
        "eval",
        "judge",
        &id,
        "item-001",
        "--choice",
        "first",
        "--evaluator",
        "ido",
        "--why",
        "clearer",
    ]);
    assert!(
        o.status.success() && out(&o).contains("stays hidden"),
        "{}{}",
        out(&o),
        err(&o)
    );
    // A model that produced one of the answers cannot judge it; a bad item or choice is refused.
    let o = f.run(&[
        "eval",
        "judge",
        &id,
        "item-002",
        "--choice",
        "tie",
        "--evaluator",
        "codex",
        "--kind",
        "ai",
    ]);
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("cannot be the judge"), "{}", err(&o));
    assert_eq!(
        f.run(&[
            "eval",
            "judge",
            &id,
            "item-999",
            "--choice",
            "tie",
            "--evaluator",
            "ido"
        ])
        .status
        .code(),
        Some(2)
    );
    assert_eq!(
        f.run(&[
            "eval",
            "judge",
            &id,
            "item-002",
            "--choice",
            "best",
            "--evaluator",
            "ido"
        ])
        .status
        .code(),
        Some(2)
    );
    let report = out(&f.run(&["eval", "report", &id]));
    assert!(
        report.contains("Blind pairwise comparison") && report.contains("single:codex vs thorough"),
        "{report}"
    );
    assert!(
        report.contains("bias checks") && report.contains("too few"),
        "{report}"
    );
    let v = json(&f.run(&["eval", "report", &id, "--output", "json"]));
    assert_eq!(v["pairs"][0]["human"]["judged"], 1);
    assert_eq!(total_agent_calls(&f), 0);
}

#[test]
fn live_evaluation_shows_its_plan_and_calls_nothing_without_the_explicit_flag() {
    let f = consult_fixture();
    let o = f.run(&[
        "eval",
        "run",
        "starter",
        "--live",
        "--arms",
        "single:codex",
        "--cases",
        "cheap-dc",
    ]);
    assert!(o.status.success(), "{}", err(&o));
    let text = out(&o);
    assert!(
        text.contains("LIVE EVALUATION PLAN")
            && text.contains("subscription quota")
            && text.contains("cheap-dc"),
        "{text}"
    );
    assert!(
        text.contains("Nothing was run") && text.contains("--yes-run-live"),
        "{text}"
    );
    assert_eq!(total_agent_calls(&f), 0);
    assert!(
        out(&f.run(&["eval", "list"])).contains("No evaluation runs yet"),
        "no run was saved"
    );
    // --live without arms is a usage error, and so is combining the two modes.
    assert_eq!(
        f.run(&["eval", "run", "starter", "--live"]).status.code(),
        Some(2)
    );
    assert_eq!(
        f.run(&["eval", "run", "starter", "--live", "--offline"])
            .status
            .code(),
        Some(2)
    );
    // A ceiling below the worst case refuses, even with the flag.
    let o = f.run(&[
        "eval",
        "run",
        "starter",
        "--live",
        "--arms",
        "thorough",
        "--cases",
        "cheap-dc",
        "--max-total-calls",
        "2",
        "--yes-run-live",
    ]);
    assert_eq!(o.status.code(), Some(2), "{}", out(&o));
    assert_eq!(total_agent_calls(&f), 0);
}

#[test]
fn live_evaluation_with_the_flag_runs_exactly_the_plan_and_records_it_as_live() {
    let f = consult_fixture();
    let o = f.run(&[
        "eval",
        "run",
        "starter",
        "--live",
        "--arms",
        "single:codex",
        "--cases",
        "cheap-dc",
        "--yes-run-live",
    ]);
    assert!(o.status.success(), "{}{}", out(&o), err(&o));
    assert_eq!(
        call_count(&f, "codex"),
        1,
        "one case, one single-agent arm, one call"
    );
    assert_eq!(call_count(&f, "claude") + call_count(&f, "agy"), 0);
    let text = out(&o);
    assert!(
        text.contains("live: providers were called") && text.contains("== live results =="),
        "{text}"
    );
    assert!(!text.contains("READ THIS FIRST"), "{text}");
    let v = json(&f.run(&["eval", "list", "--output", "json"]));
    assert_eq!(v["runs"][0]["kind"], "live");
    assert_eq!(v["runs"][0]["provenance"][0], "live");
    // The ordinary history was not filled with evaluation consultations.
    assert!(out(&f.run(&["history"])).contains("No saved debates yet"));
}

#[test]
fn feedback_is_stored_listed_and_deleted_from_the_command_line() {
    let f = consult_fixture();
    let o = f.run(&[
        "eval",
        "feedback",
        "20261012-000000-000001",
        "--rating",
        "inaccurate",
        "--note",
        "the price was wrong",
    ]);
    assert!(
        o.status.success() && out(&o).contains("changes nothing by itself"),
        "{}{}",
        out(&o),
        err(&o)
    );
    let list = json(&f.run(&["eval", "feedback", "--list", "--output", "json"]));
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["rating"], "inaccurate");
    let id = list[0]["id"].as_str().unwrap().to_string();
    assert!(out(&f.run(&["eval", "feedback"])).contains("the price was wrong"));
    assert_eq!(
        f.run(&[
            "eval",
            "feedback",
            "20261012-000000-000001",
            "--rating",
            "great"
        ])
        .status
        .code(),
        Some(2)
    );
    assert!(out(&f.run(&["eval", "feedback", "--delete", &id])).contains("Deleted"));
    assert!(out(&f.run(&["eval", "feedback"])).contains("No feedback stored"));
    // It did not become a preference.
    assert!(
        !f.dir
            .path()
            .join("data")
            .join("ai-orchestra")
            .join("preferences.json")
            .exists()
    );
    assert_eq!(total_agent_calls(&f), 0);
}
