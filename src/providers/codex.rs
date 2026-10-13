//! Adapter for OpenAI Codex CLI (`codex`).
//!
//! Verified against `codex exec --help` (0.160.x): `exec`, `--json`,
//! `--sandbox read-only|workspace-write`, `--ephemeral`,
//! `--skip-git-repo-check`, `--color`, `-m`, and `-` for a stdin prompt;
//! plus `codex login status`.
//!
//! `--json` prints JSONL events. The reply is the text of the last
//! `item.completed` event whose item type is `agent_message`.

use std::path::Path;

use async_trait::async_trait;
use serde_json::Value;

use super::{run_probe, snippet, urls_in_value};
use crate::config::ProviderConfig;
use crate::error::{OrchestraError, Result};
use crate::provider::{AgentProvider, AuthState, Invocation, InvokeContext};
use crate::research::capabilities::{ResearchCapabilities, Support};
use crate::types::{AccessMode, AgentRequest, AgentResponse, ProviderId, ReportedUrl, Usage};

pub struct CodexAdapter {
    id: ProviderId,
    config: ProviderConfig,
}

impl CodexAdapter {
    pub fn new(config: ProviderConfig) -> Self {
        Self {
            id: ProviderId::new("codex"),
            config,
        }
    }
}

#[async_trait]
impl AgentProvider for CodexAdapter {
    fn id(&self) -> &ProviderId {
        &self.id
    }

    fn display_name(&self) -> &str {
        "OpenAI Codex CLI"
    }

    fn config(&self) -> &ProviderConfig {
        &self.config
    }

    fn default_executable(&self) -> &str {
        "codex"
    }

    /// The valid levels depend on the model (the local model cache lists
    /// `low`..`max`, and `ultra` for some), so only the shape is checked here;
    /// Codex itself rejects a level its model does not support.
    fn check_effort(&self, effort: &str) -> Result<()> {
        if !effort.is_empty() && effort.chars().all(|c| c.is_ascii_lowercase()) {
            Ok(())
        } else {
            Err(OrchestraError::Config(format!(
                "invalid effort `{effort}` for codex (use a lowercase level such as low, medium, high, xhigh)"
            )))
        }
    }

    /// Names found in the Codex binary that identify a Codex session. They have
    /// not been observed set in a live Codex shell (this was developed from a
    /// different CLI's session), so treat this as best effort; a false
    /// positive can be overridden with `allow_nested = true`.
    fn host_env_markers(&self) -> &[&str] {
        &[
            "CODEX_THREAD_ID",
            "CODEX_SESSION_ID",
            "CODEX_SANDBOX",
            "CODEX_SHELL",
        ]
    }

    fn research_capabilities(&self) -> ResearchCapabilities {
        ResearchCapabilities {
            web_search: Support::Documented,
            web_fetch: Support::Unverified,
            exposes_source_urls: Support::Unverified,
            how: "codex --search exec ... (live web search; the --search flag is global, not an exec option)".to_string(),
            notes: "`codex --help` documents --search (\"the native Responses `web_search` tool is available to the \
                    model, no per-call approval\"). Checked locally with the installed CLI (0.160.1): `codex --search \
                    exec` is accepted by the parser and `codex exec --search` is rejected; the binary lists the \
                    modes disabled/cached/indexed/live for the `web_search` config key and a `web_search` item type \
                    in its events. Not yet run here: whether `exec --json` emits those items, and whether they \
                    carry the source URLs (URLs are harvested from any `web_search` item, whatever its fields)."
                .to_string(),
        }
    }

    fn build_invocation(&self, req: &AgentRequest) -> Result<Invocation> {
        let sandbox = match req.access {
            AccessMode::ReadOnly => "read-only",
            AccessMode::AllowWrite => "workspace-write",
        };
        let mut args: Vec<String> = [
            "exec",
            "--json",
            "--ephemeral",
            "--skip-git-repo-check",
            "--color",
            "never",
            "--sandbox",
            sandbox,
        ]
        .map(String::from)
        .to_vec();
        if req.isolate {
            // Documented in `codex exec --help`: "Do not load
            // $CODEX_HOME/config.toml; auth still uses CODEX_HOME". MCP servers
            // are configured there. Not yet verified live; the depth limit is
            // the backstop. Model and effort then come from ai-orchestra's own
            // config (or Codex's built-in defaults), not the user's config.toml.
            args.push("--ignore-user-config".to_string());
        }
        let (model, effort) = self.resolve_settings(req)?;
        if let Some(model) = model {
            args.extend(["--model".to_string(), model]);
        }
        if let Some(effort) = effort {
            // `-c key=value` parses the value as TOML, so a string needs quotes.
            args.extend([
                "-c".to_string(),
                format!("model_reasoning_effort=\"{effort}\""),
            ]);
        }
        args.extend(self.config.extra_args.iter().cloned());
        if req.web {
            // `--search` is a flag of the top-level command, not of `exec` (checked with the
            // installed CLI: `codex --search exec ...` is accepted, `codex exec --search` is not).
            args.insert(0, "--search".to_string());
        }
        // `-` tells codex to read the instructions from stdin.
        args.push("-".to_string());
        Ok(Invocation {
            args,
            stdin: Some(req.prompt.clone()),
        })
    }

    fn parse_output(&self, stdout: &str) -> Result<AgentResponse> {
        let mut message: Option<String> = None;
        let mut thread_id: Option<String> = None;
        let mut usage: Option<Usage> = None;
        let mut failure: Option<String> = None;
        let mut last_error_event: Option<String> = None;
        let mut events = 0usize;
        let mut reported_urls: Vec<ReportedUrl> = Vec::new();

        for line in stdout.lines().map(str::trim).filter(|l| !l.is_empty()) {
            // Tolerate stray non-JSON lines (warnings); require at least one event.
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            events += 1;
            // Web search events: report every URL they contain, whatever their fields are called.
            if matches!(
                v["type"].as_str(),
                Some("item.started" | "item.updated" | "item.completed")
            ) && v["item"]["type"] == "web_search"
            {
                let mut urls = Vec::new();
                urls_in_value(&v["item"], &mut urls);
                for url in urls {
                    if !reported_urls.iter().any(|r| r.url == url) {
                        reported_urls.push(ReportedUrl {
                            url,
                            tool: "web_search".to_string(),
                        });
                    }
                }
            }
            match v["type"].as_str().unwrap_or("") {
                "thread.started" => {
                    thread_id = v["thread_id"].as_str().map(String::from);
                }
                "item.completed" if v["item"]["type"] == "agent_message" => {
                    if let Some(t) = v["item"]["text"].as_str() {
                        message = Some(t.to_string());
                    }
                }
                "turn.completed" => {
                    let u = Usage {
                        input_tokens: v["usage"]["input_tokens"].as_u64(),
                        output_tokens: v["usage"]["output_tokens"].as_u64(),
                        cached_input_tokens: v["usage"]["cached_input_tokens"].as_u64(),
                    };
                    usage = (!u.is_empty()).then_some(u);
                }
                "turn.failed" => {
                    failure = v["error"]["message"]
                        .as_str()
                        .or_else(|| v["message"].as_str())
                        .map(String::from)
                        .or_else(|| Some("turn failed".to_string()));
                }
                // Non-terminal `error` events also report retries, so they only
                // matter when no answer arrives.
                "error" => {
                    last_error_event = v["message"].as_str().map(String::from);
                }
                _ => {}
            }
        }

        if events == 0 {
            return Err(OrchestraError::MalformedOutput {
                provider: self.id.clone(),
                detail: format!("no JSON events in output: {}", snippet(stdout, 200)),
            });
        }
        if let Some(message) = failure {
            return Err(OrchestraError::AgentError {
                provider: self.id.clone(),
                message,
            });
        }
        let Some(text) = message else {
            return Err(match last_error_event {
                Some(message) => OrchestraError::AgentError {
                    provider: self.id.clone(),
                    message,
                },
                None => OrchestraError::EmptyResponse {
                    provider: self.id.clone(),
                    detail: None,
                },
            });
        };
        if text.trim().is_empty() {
            return Err(OrchestraError::EmptyResponse {
                provider: self.id.clone(),
                detail: None,
            });
        }
        let mut response = AgentResponse::new(self.id.clone(), text.trim_end());
        response.session_id = thread_id;
        response.usage = usage;
        response.reported_urls = reported_urls;
        Ok(response)
    }

    async fn auth_status(&self, ctx: &InvokeContext, executable: &Path) -> AuthState {
        match run_probe(ctx, executable, &["login", "status"]).await {
            Some(out) => {
                let combined = format!("{}\n{}", out.stdout, out.stderr);
                parse_login_status(&combined, out.success)
            }
            None => AuthState::Unknown {
                reason: "could not run `codex login status`".to_string(),
            },
        }
    }
}

/// Parses the text output of `codex login status`, e.g. `Logged in using
/// ChatGPT` or `Logged in using an API key`.
pub(crate) fn parse_login_status(text: &str, exit_ok: bool) -> AuthState {
    let lower = text.to_lowercase();
    if lower.contains("not logged in") || lower.contains("logged out") {
        return AuthState::NotLoggedIn {
            hint: "run `codex login`".to_string(),
        };
    }
    if lower.contains("logged in") {
        let uses_api_key = lower.contains("api key") || lower.contains("api-key");
        let method = if uses_api_key {
            "API key"
        } else if lower.contains("chatgpt") {
            "ChatGPT"
        } else {
            "unknown method"
        };
        return AuthState::LoggedIn {
            method: method.to_string(),
            uses_api_key,
        };
    }
    if exit_ok {
        AuthState::Unknown {
            reason: "unrecognized `codex login status` output".to_string(),
        }
    } else {
        AuthState::NotLoggedIn {
            hint: "run `codex login`".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter() -> CodexAdapter {
        CodexAdapter::new(ProviderConfig::default())
    }

    #[test]
    fn web_research_puts_the_global_search_flag_before_exec_and_stays_read_only() {
        let inv = adapter()
            .build_invocation(&AgentRequest::new("q").with_web(true))
            .unwrap();
        assert_eq!(
            &inv.args[..2],
            ["--search", "exec"],
            "--search is a top-level flag"
        );
        let s = inv.args.iter().position(|a| a == "--sandbox").unwrap();
        assert_eq!(inv.args[s + 1], "read-only");
        assert!(!inv.args.iter().any(|a| a.contains("danger")));
        assert_eq!(inv.args.last().map(String::as_str), Some("-"));
        let plain = adapter().build_invocation(&AgentRequest::new("q")).unwrap();
        assert_eq!(plain.args[0], "exec");
        assert!(!plain.args.contains(&"--search".to_string()));
    }

    #[test]
    fn urls_in_web_search_items_are_reported_and_other_items_are_not() {
        let out = [
            r#"{"type":"thread.started","thread_id":"t1"}"#,
            r#"{"type":"item.started","item":{"id":"i1","type":"web_search","query":"model x"}}"#,
            r#"{"type":"item.completed","item":{"id":"i1","type":"web_search","query":"model x","action":{"type":"search","sources":[{"url":"https://maker.example/spec"},{"url":"https://lab.example/test"}]}}}"#,
            r#"{"type":"item.completed","item":{"id":"i2","type":"command_execution","aggregated_output":"see https://not-from-search.example/x"}}"#,
            r#"{"type":"item.completed","item":{"id":"i3","type":"agent_message","text":"Answer mentioning https://written-by-model.example/y"}}"#,
            r#"{"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":2}}"#,
        ]
        .join("\n");
        let r = adapter().parse_output(&out).unwrap();
        assert!(r.text.starts_with("Answer mentioning"));
        let urls: Vec<&str> = r.reported_urls.iter().map(|u| u.url.as_str()).collect();
        assert_eq!(
            urls,
            ["https://maker.example/spec", "https://lab.example/test"]
        );
        assert!(r.reported_urls.iter().all(|u| u.tool == "web_search"));
    }

    #[test]
    fn read_only_by_default_and_prompt_via_stdin() {
        let inv = adapter()
            .build_invocation(&AgentRequest::new("review; rm -rf /"))
            .unwrap();
        let s = inv.args.iter().position(|a| a == "--sandbox").unwrap();
        assert_eq!(inv.args[s + 1], "read-only");
        assert_eq!(inv.args.last().unwrap(), "-");
        assert_eq!(inv.stdin.as_deref(), Some("review; rm -rf /"));
        assert!(!inv.args.iter().any(|a| a.contains("dangerously")));
        assert_eq!(inv.args[0], "exec");
    }

    #[test]
    fn write_access_uses_workspace_write_never_full_access() {
        let inv = adapter()
            .build_invocation(&AgentRequest::new("x").with_access(AccessMode::AllowWrite))
            .unwrap();
        let s = inv.args.iter().position(|a| a == "--sandbox").unwrap();
        assert_eq!(inv.args[s + 1], "workspace-write");
        assert!(!inv.args.contains(&"danger-full-access".to_string()));
    }

    #[test]
    fn extra_args_come_before_the_stdin_marker() {
        let cfg = ProviderConfig {
            extra_args: vec!["-c".into(), "model_reasoning_effort=low".into()],
            ..Default::default()
        };
        let inv = CodexAdapter::new(cfg)
            .build_invocation(&AgentRequest::new("x"))
            .unwrap();
        let n = inv.args.len();
        assert_eq!(inv.args[n - 1], "-");
        assert_eq!(inv.args[n - 2], "model_reasoning_effort=low");
    }

    const OK: &str = r#"{"type":"thread.started","thread_id":"t-1"}
{"type":"turn.started"}
{"type":"item.completed","item":{"id":"i0","type":"reasoning","text":"thinking"}}
{"type":"item.completed","item":{"id":"i1","type":"agent_message","text":"first"}}
{"type":"item.completed","item":{"id":"i2","type":"agent_message","text":"The answer.\n"}}
{"type":"turn.completed","usage":{"input_tokens":100,"cached_input_tokens":40,"output_tokens":7}}
"#;

    #[test]
    fn parses_events_and_takes_last_agent_message() {
        let r = adapter().parse_output(OK).unwrap();
        assert_eq!(r.text, "The answer.");
        assert_eq!(r.session_id.as_deref(), Some("t-1"));
        let u = r.usage.unwrap();
        assert_eq!(
            (u.input_tokens, u.cached_input_tokens, u.output_tokens),
            (Some(100), Some(40), Some(7))
        );
    }

    #[test]
    fn tolerates_noise_lines() {
        let noisy = format!("WARNING: something\n{OK}\n\n");
        assert_eq!(adapter().parse_output(&noisy).unwrap().text, "The answer.");
    }

    #[test]
    fn failures_and_malformed_output() {
        let a = adapter();
        let failed = r#"{"type":"thread.started","thread_id":"t"}
{"type":"error","message":"Reconnecting... 1/5"}
{"type":"turn.failed","error":{"message":"You've hit your usage limit"}}"#;
        assert!(matches!(
            a.parse_output(failed).unwrap_err(),
            OrchestraError::AgentError { ref message, .. } if message.contains("usage limit")
        ));
        // A retry notice followed by a real answer is not a failure.
        let retried = r#"{"type":"error","message":"Reconnecting... 1/5"}
{"type":"item.completed","item":{"type":"agent_message","text":"ok"}}"#;
        assert_eq!(a.parse_output(retried).unwrap().text, "ok");
        // Error with no answer surfaces the error.
        let only_error = r#"{"type":"error","message":"boom"}"#;
        assert!(matches!(
            a.parse_output(only_error).unwrap_err(),
            OrchestraError::AgentError { ref message, .. } if message == "boom"
        ));
        assert!(matches!(
            a.parse_output("plain text").unwrap_err(),
            OrchestraError::MalformedOutput { .. }
        ));
        assert!(matches!(
            a.parse_output("").unwrap_err(),
            OrchestraError::MalformedOutput { .. }
        ));
        assert!(matches!(
            a.parse_output(r#"{"type":"turn.completed"}"#).unwrap_err(),
            OrchestraError::EmptyResponse { .. }
        ));
    }

    #[test]
    fn login_status_parsing() {
        assert!(matches!(
            parse_login_status("Logged in using ChatGPT", true),
            AuthState::LoggedIn { ref method, uses_api_key: false } if method == "ChatGPT"
        ));
        assert!(matches!(
            parse_login_status("Logged in using an API key - sk-***", true),
            AuthState::LoggedIn {
                uses_api_key: true,
                ..
            }
        ));
        assert!(matches!(
            parse_login_status("Not logged in", false),
            AuthState::NotLoggedIn { .. }
        ));
        assert!(matches!(
            parse_login_status("???", true),
            AuthState::Unknown { .. }
        ));
        assert!(matches!(
            parse_login_status("???", false),
            AuthState::NotLoggedIn { .. }
        ));
    }

    #[test]
    fn effort_becomes_a_quoted_toml_override_before_the_stdin_marker() {
        let inv = adapter()
            .build_invocation(
                &AgentRequest::new("x")
                    .with_effort("low")
                    .with_model("gpt-6-luna"),
            )
            .unwrap();
        let c = inv.args.iter().position(|a| a == "-c").unwrap();
        assert_eq!(inv.args[c + 1], "model_reasoning_effort=\"low\"");
        let m = inv.args.iter().position(|a| a == "--model").unwrap();
        assert_eq!(inv.args[m + 1], "gpt-6-luna");
        assert_eq!(inv.args.last().unwrap(), "-");
        assert!(c < inv.args.len() - 1);
        // No settings: Codex uses its own config.toml.
        let bare = adapter().build_invocation(&AgentRequest::new("x")).unwrap();
        assert!(!bare.args.contains(&"-c".to_string()));
        assert!(!bare.args.contains(&"--model".to_string()));
        // The valid levels depend on the model, so only the shape is enforced.
        adapter()
            .build_invocation(&AgentRequest::new("x").with_effort("ultra"))
            .unwrap();
        for bad in ["High", "low\"; rm", "", "1", "a b"] {
            assert!(
                matches!(
                    adapter()
                        .build_invocation(&AgentRequest::new("x").with_effort(bad))
                        .unwrap_err(),
                    OrchestraError::Config(_)
                ),
                "{bad}"
            );
        }
    }
}
