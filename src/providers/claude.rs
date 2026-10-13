//! Adapter for Anthropic Claude Code (`claude`).
//!
//! Verified against `claude --help` (2.1.x): `-p/--print`,
//! `--output-format json`, `--tools`, `--permission-mode`,
//! `--permission-prompts none`, `--strict-mcp-config`,
//! `--no-session-persistence`, `--model`, and `claude auth status`.
//!
//! The prompt goes over stdin. `--bare` is deliberately never used: it
//! disables the subscription (OAuth) login and requires an API key.

use std::path::Path;

use async_trait::async_trait;
use serde_json::Value;

use super::{run_probe, snippet, urls_in_value};
use crate::config::ProviderConfig;
use crate::error::{OrchestraError, Result};
use crate::provider::{AgentProvider, AuthState, Invocation, InvokeContext, check_effort_in};
use crate::research::capabilities::{ResearchCapabilities, Support};
use crate::types::{AccessMode, AgentRequest, AgentResponse, ProviderId, ReportedUrl, Usage};

/// From `claude --help` (2.1.x): `--effort <level>`.
const CLAUDE_EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

/// Built-in tools that cannot modify anything.
const READ_ONLY_TOOLS: &str = "Read,Grep,Glob";
/// Web tools, added only for research requests. Both are read-only: they fetch public pages.
const WEB_TOOLS: &str = "WebSearch,WebFetch";

pub struct ClaudeAdapter {
    id: ProviderId,
    config: ProviderConfig,
}

impl ClaudeAdapter {
    pub fn new(config: ProviderConfig) -> Self {
        Self {
            id: ProviderId::new("claude"),
            config,
        }
    }

    fn malformed(&self, detail: impl Into<String>) -> OrchestraError {
        OrchestraError::MalformedOutput {
            provider: self.id.clone(),
            detail: detail.into(),
        }
    }
}

#[async_trait]
impl AgentProvider for ClaudeAdapter {
    fn id(&self) -> &ProviderId {
        &self.id
    }

    fn display_name(&self) -> &str {
        "Claude Code"
    }

    fn config(&self) -> &ProviderConfig {
        &self.config
    }

    fn default_executable(&self) -> &str {
        "claude"
    }

    fn host_env_markers(&self) -> &[&str] {
        &["CLAUDECODE"]
    }

    fn check_effort(&self, effort: &str) -> Result<()> {
        check_effort_in(&self.id, effort, CLAUDE_EFFORTS)
    }

    fn research_capabilities(&self) -> ResearchCapabilities {
        ResearchCapabilities {
            web_search: Support::Documented,
            web_fetch: Support::Documented,
            exposes_source_urls: Support::Unverified,
            how: "--tools Read,Grep,Glob,WebSearch,WebFetch --allowedTools WebSearch,WebFetch (print mode, stream-json)".to_string(),
            notes: "WebSearch and WebFetch are built-in tools named in the installed CLI (2.1.296) and in its \
                    documentation; print mode needs them in --allowedTools because it cannot ask. Not yet run \
                    here: whether the subscription allows them, and where the source URLs appear in the \
                    stream-json events (URLs are harvested from any web-tool result, whatever its shape). \
                    WebFetch returns a model-written answer about a page, not the page, so it is never used as \
                    evidence; AI Orchestra fetches pages itself."
                .to_string(),
        }
    }

    fn build_invocation(&self, req: &AgentRequest) -> Result<Invocation> {
        let mut args: Vec<String> = [
            "-p",
            "--output-format",
            if req.web { "stream-json" } else { "json" },
            "--no-session-persistence",
            "--strict-mcp-config",
            "--permission-prompts",
            "none",
        ]
        .map(String::from)
        .to_vec();
        if req.web {
            // stream-json needs --verbose; the events are where web-tool results (and their
            // URLs) are visible.
            args.push("--verbose".to_string());
        }

        match req.access {
            AccessMode::ReadOnly => {
                // Only read tools (and, for research, the web tools) exist, so nothing can be
                // modified or executed.
                let tools = if req.web {
                    format!("{READ_ONLY_TOOLS},{WEB_TOOLS}")
                } else {
                    READ_ONLY_TOOLS.to_string()
                };
                args.extend(["--tools".to_string(), tools]);
            }
            AccessMode::AllowWrite => {
                args.extend(["--permission-mode".to_string(), "acceptEdits".to_string()]);
            }
        }
        if req.web {
            // Print mode cannot ask for permission, so the web tools must be allowed up front.
            args.extend(["--allowedTools".to_string(), WEB_TOOLS.to_string()]);
        }
        let (model, effort) = self.resolve_settings(req)?;
        if let Some(model) = model {
            args.extend(["--model".to_string(), model]);
        }
        if let Some(effort) = effort {
            args.extend(["--effort".to_string(), effort]);
        }
        args.extend(self.config.extra_args.iter().cloned());

        Ok(Invocation {
            args,
            stdin: Some(req.prompt.clone()),
        })
    }

    fn parse_output(&self, stdout: &str) -> Result<AgentResponse> {
        let value: Value = match serde_json::from_str(stdout.trim()) {
            Ok(v) => v,
            Err(e) => {
                // `--output-format stream-json` prints one JSON event per line.
                let events: Vec<Value> = stdout
                    .lines()
                    .filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
                    .collect();
                if events.is_empty() {
                    return Err(
                        self.malformed(format!("invalid JSON ({e}): {}", snippet(stdout, 200)))
                    );
                }
                Value::Array(events)
            }
        };
        let reported_urls = match &value {
            Value::Array(events) => web_tool_urls(events),
            _ => Vec::new(),
        };

        // `--output-format json` yields one result object; some versions emit
        // an array of messages whose last `result` element is what we want.
        let result = match &value {
            Value::Object(_) => &value,
            Value::Array(items) => items
                .iter()
                .rev()
                .find(|v| v["type"] == "result")
                .ok_or_else(|| self.malformed("no `result` message in JSON array"))?,
            _ => return Err(self.malformed("expected a JSON object")),
        };

        let subtype = result["subtype"].as_str().unwrap_or("");
        let text = result["result"].as_str();
        let is_error =
            result["is_error"].as_bool().unwrap_or(false) || subtype.starts_with("error");
        if is_error {
            let message = match text {
                Some(t) if !t.trim().is_empty() => t.trim().to_string(),
                _ if !subtype.is_empty() => format!("Claude Code ended with `{subtype}`"),
                _ => "Claude Code reported an error".to_string(),
            };
            return Err(OrchestraError::AgentError {
                provider: self.id.clone(),
                message,
            });
        }
        let text = text.ok_or_else(|| self.malformed("missing `result` field"))?;
        if text.trim().is_empty() {
            return Err(OrchestraError::EmptyResponse {
                provider: self.id.clone(),
                detail: None,
            });
        }

        let mut response = AgentResponse::new(self.id.clone(), text.trim_end());
        response.reported_urls = reported_urls;
        response.session_id = result["session_id"].as_str().map(String::from);
        response.reported_cost_usd = result["total_cost_usd"].as_f64();
        let usage = Usage {
            input_tokens: result["usage"]["input_tokens"].as_u64(),
            output_tokens: result["usage"]["output_tokens"].as_u64(),
            cached_input_tokens: result["usage"]["cache_read_input_tokens"].as_u64(),
        };
        response.usage = (!usage.is_empty()).then_some(usage);
        // `modelUsage` maps model id -> token counts. Several models can appear
        // (helpers); the one that handled the most tokens is the main model.
        response.model_reported = result["modelUsage"].as_object().and_then(|models| {
            models
                .iter()
                .max_by_key(|(_, v)| {
                    v["inputTokens"].as_u64().unwrap_or(0) + v["outputTokens"].as_u64().unwrap_or(0)
                })
                .map(|(name, _)| name.clone())
        });
        Ok(response)
    }

    async fn auth_status(&self, ctx: &InvokeContext, executable: &Path) -> AuthState {
        match run_probe(ctx, executable, &["auth", "status"]).await {
            Some(out) => parse_auth_status(&out.stdout),
            None => AuthState::Unknown {
                reason: "could not run `claude auth status`".to_string(),
            },
        }
    }
}

/// URLs in the results of Claude's web tools. Tool calls and their results are matched by id; the
/// URL a `WebFetch` call asked for is also reported (the model chose to consult it). The shape of
/// the results is not assumed: every URL in a web-tool result counts as reported.
fn web_tool_urls(events: &[Value]) -> Vec<ReportedUrl> {
    let mut names: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut out: Vec<ReportedUrl> = Vec::new();
    let push = |out: &mut Vec<ReportedUrl>, tool: &str, url: String| {
        if !out.iter().any(|r| r.url == url) {
            out.push(ReportedUrl {
                url,
                tool: tool.to_string(),
            });
        }
    };
    for event in events {
        let Some(blocks) = event["message"]["content"].as_array() else {
            continue;
        };
        for block in blocks {
            match block["type"].as_str() {
                Some("tool_use") => {
                    let (Some(id), Some(name)) = (block["id"].as_str(), block["name"].as_str())
                    else {
                        continue;
                    };
                    if name == "WebSearch" || name == "WebFetch" {
                        names.insert(id.to_string(), name.to_string());
                        if name == "WebFetch"
                            && let Some(u) = block["input"]["url"].as_str()
                        {
                            push(&mut out, name, u.to_string());
                        }
                    }
                }
                Some("tool_result") => {
                    let Some(tool) = block["tool_use_id"].as_str().and_then(|id| names.get(id))
                    else {
                        continue;
                    };
                    let mut urls = Vec::new();
                    urls_in_value(&block["content"], &mut urls);
                    for u in urls {
                        push(&mut out, tool, u);
                    }
                }
                _ => {}
            }
        }
    }
    out
}

/// Parses `claude auth status` (JSON by default). Only the login method is
/// kept; account identifiers in the output are intentionally ignored.
pub(crate) fn parse_auth_status(stdout: &str) -> AuthState {
    let Ok(v) = serde_json::from_str::<Value>(stdout.trim()) else {
        return AuthState::Unknown {
            reason: "unrecognized `claude auth status` output".to_string(),
        };
    };
    match v["loggedIn"].as_bool() {
        Some(true) => {
            let method = v["authMethod"].as_str().unwrap_or("unknown").to_string();
            let plan = v["subscriptionType"].as_str();
            // `claude.ai` / `oauth` style logins are subscription logins.
            let lower = method.to_lowercase();
            let uses_api_key = lower.contains("api") || lower.contains("key");
            AuthState::LoggedIn {
                method: match plan {
                    Some(p) => format!("{method} ({p} plan)"),
                    None => method,
                },
                uses_api_key,
            }
        }
        Some(false) => AuthState::NotLoggedIn {
            hint: "run `claude auth login`".to_string(),
        },
        None => AuthState::Unknown {
            reason: "`loggedIn` missing from `claude auth status`".to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter() -> ClaudeAdapter {
        ClaudeAdapter::new(ProviderConfig::default())
    }

    #[test]
    fn web_research_adds_only_the_two_read_only_web_tools_and_asks_for_events() {
        let inv = adapter()
            .build_invocation(&AgentRequest::new("q").with_web(true))
            .unwrap();
        let at = |flag: &str| inv.args.iter().position(|a| a == flag).unwrap();
        assert_eq!(inv.args[at("--output-format") + 1], "stream-json");
        assert!(inv.args.contains(&"--verbose".to_string()));
        assert_eq!(
            inv.args[at("--tools") + 1],
            "Read,Grep,Glob,WebSearch,WebFetch"
        );
        assert_eq!(inv.args[at("--allowedTools") + 1], "WebSearch,WebFetch");
        for forbidden in [
            "Bash",
            "Edit",
            "Write",
            "acceptEdits",
            "bypassPermissions",
            "--dangerously-skip-permissions",
        ] {
            assert!(
                !inv.args.iter().any(|a| a.contains(forbidden)),
                "{forbidden}"
            );
        }
        // Without the flag nothing about the web is present.
        let plain = adapter().build_invocation(&AgentRequest::new("q")).unwrap();
        assert!(
            !plain
                .args
                .iter()
                .any(|a| a.contains("Web") || a == "--verbose" || a == "stream-json")
        );
    }

    #[test]
    fn urls_are_harvested_only_from_web_tool_results_whatever_their_shape() {
        let events = [
            r#"{"type":"system","subtype":"init","tools":["Read","WebSearch"]}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"WebSearch","input":{"query":"model x bandwidth"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"Links: [{\"title\":\"Spec\",\"url\":\"https://maker.example/spec\"},{\"title\":\"Test\",\"url\":\"https://lab.example/test\"}]"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t2","name":"WebFetch","input":{"url":"https://maker.example/spec","prompt":"p"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t2","content":[{"type":"text","text":"The page says see https://maker.example/downloads."}]}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t3","name":"Read","input":{"file_path":"/tmp/x"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t3","content":"file mentions https://not-a-web-tool.example/x"}]}}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"result":"Final answer [1].","session_id":"s"}"#,
        ]
        .join("\n");
        let r = adapter().parse_output(&events).unwrap();
        assert_eq!(r.text, "Final answer [1].");
        let urls: Vec<(&str, &str)> = r
            .reported_urls
            .iter()
            .map(|u| (u.url.as_str(), u.tool.as_str()))
            .collect();
        assert_eq!(
            urls,
            vec![
                ("https://maker.example/spec", "WebSearch"),
                ("https://lab.example/test", "WebSearch"),
                ("https://maker.example/downloads", "WebFetch"),
            ],
            "no duplicates, no URLs from other tools"
        );
        // A plain single-object reply (no web) still parses and reports nothing.
        let plain = adapter()
            .parse_output(r#"{"type":"result","is_error":false,"result":"hi"}"#)
            .unwrap();
        assert!(plain.reported_urls.is_empty());
        // Garbage is still a clean error.
        assert!(adapter().parse_output("not json at all").is_err());
    }

    #[test]
    fn read_only_invocation_has_no_write_capable_tools() {
        let inv = adapter()
            .build_invocation(&AgentRequest::new("hello $(rm -rf /)"))
            .unwrap();
        assert_eq!(inv.stdin.as_deref(), Some("hello $(rm -rf /)"));
        assert!(
            !inv.args.iter().any(|a| a.contains("hello")),
            "prompt stays off argv"
        );
        let tools = inv.args.iter().position(|a| a == "--tools").unwrap();
        assert_eq!(inv.args[tools + 1], "Read,Grep,Glob");
        assert!(!inv.args.contains(&"acceptEdits".to_string()));
        assert!(!inv.args.contains(&"--bare".to_string()));
        assert!(
            !inv.args
                .contains(&"--dangerously-skip-permissions".to_string())
        );
        assert!(inv.args.contains(&"--strict-mcp-config".to_string()));
    }

    #[test]
    fn write_access_must_be_requested() {
        let req = AgentRequest::new("x").with_access(AccessMode::AllowWrite);
        let inv = adapter().build_invocation(&req).unwrap();
        let pm = inv
            .args
            .iter()
            .position(|a| a == "--permission-mode")
            .unwrap();
        assert_eq!(inv.args[pm + 1], "acceptEdits");
        assert!(!inv.args.contains(&"--tools".to_string()));
    }

    #[test]
    fn model_precedence_request_over_config_and_extra_args_last() {
        let cfg = ProviderConfig {
            model: Some("sonnet".into()),
            extra_args: vec!["--effort".into(), "low".into()],
            ..Default::default()
        };
        let a = ClaudeAdapter::new(cfg);
        let inv = a.build_invocation(&AgentRequest::new("x")).unwrap();
        let m = inv.args.iter().position(|x| x == "--model").unwrap();
        assert_eq!(inv.args[m + 1], "sonnet");
        assert_eq!(&inv.args[inv.args.len() - 2..], ["--effort", "low"]);
        let inv = a
            .build_invocation(&AgentRequest::new("x").with_model("opus"))
            .unwrap();
        let m = inv.args.iter().position(|x| x == "--model").unwrap();
        assert_eq!(inv.args[m + 1], "opus");
    }

    #[test]
    fn parses_success_result() {
        let r = adapter()
            .parse_output(
                r#"{"type":"result","subtype":"success","is_error":false,"result":"Ownership is...\n",
                    "session_id":"abc","total_cost_usd":0.0123,
                    "usage":{"input_tokens":10,"output_tokens":20,"cache_read_input_tokens":5}}"#,
            )
            .unwrap();
        assert_eq!(r.text, "Ownership is...");
        assert_eq!(r.session_id.as_deref(), Some("abc"));
        assert_eq!(r.reported_cost_usd, Some(0.0123));
        let u = r.usage.unwrap();
        assert_eq!(
            (u.input_tokens, u.output_tokens, u.cached_input_tokens),
            (Some(10), Some(20), Some(5))
        );
    }

    #[test]
    fn parses_array_form_using_last_result() {
        let r = adapter()
            .parse_output(
                r#"[{"type":"system"},{"type":"assistant"},{"type":"result","result":"final","is_error":false}]"#,
            )
            .unwrap();
        assert_eq!(r.text, "final");
    }

    #[test]
    fn reports_agent_errors_and_bad_output() {
        let a = adapter();
        let e = a
            .parse_output(r#"{"type":"result","subtype":"success","is_error":true,"result":"Invalid API key"}"#)
            .unwrap_err();
        assert!(
            matches!(e, OrchestraError::AgentError { ref message, .. } if message == "Invalid API key")
        );
        let e = a
            .parse_output(r#"{"type":"result","subtype":"error_max_turns"}"#)
            .unwrap_err();
        assert!(matches!(e, OrchestraError::AgentError { .. }));
        assert!(matches!(
            a.parse_output("not json").unwrap_err(),
            OrchestraError::MalformedOutput { .. }
        ));
        assert!(matches!(
            a.parse_output("[]").unwrap_err(),
            OrchestraError::MalformedOutput { .. }
        ));
        assert!(matches!(
            a.parse_output("42").unwrap_err(),
            OrchestraError::MalformedOutput { .. }
        ));
        assert!(matches!(
            a.parse_output(r#"{"is_error":false}"#).unwrap_err(),
            OrchestraError::MalformedOutput { .. }
        ));
        assert!(matches!(
            a.parse_output(r#"{"is_error":false,"result":"  "}"#)
                .unwrap_err(),
            OrchestraError::EmptyResponse { .. }
        ));
    }

    #[test]
    fn auth_status_parsing_keeps_only_the_method() {
        match parse_auth_status(
            r#"{"loggedIn":true,"authMethod":"claude.ai","email":"a@b.c","subscriptionType":"pro"}"#,
        ) {
            AuthState::LoggedIn {
                method,
                uses_api_key,
            } => {
                assert_eq!(method, "claude.ai (pro plan)");
                assert!(!uses_api_key);
                assert!(!method.contains('@'));
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            parse_auth_status(r#"{"loggedIn":true,"authMethod":"api_key"}"#),
            AuthState::LoggedIn {
                uses_api_key: true,
                ..
            }
        ));
        assert!(matches!(
            parse_auth_status(r#"{"loggedIn":false}"#),
            AuthState::NotLoggedIn { .. }
        ));
        assert!(matches!(
            parse_auth_status("garbage"),
            AuthState::Unknown { .. }
        ));
    }

    #[test]
    fn model_and_effort_are_passed_and_validated() {
        let cfg = ProviderConfig {
            model: Some("sonnet".into()),
            effort: Some("high".into()),
            ..Default::default()
        };
        let a = ClaudeAdapter::new(cfg);
        let inv = a.build_invocation(&AgentRequest::new("x")).unwrap();
        let e = inv.args.iter().position(|x| x == "--effort").unwrap();
        assert_eq!(inv.args[e + 1], "high");
        // A request-level value wins over the config.
        let inv = a
            .build_invocation(&AgentRequest::new("x").with_effort("low").with_model("opus"))
            .unwrap();
        let e = inv.args.iter().position(|x| x == "--effort").unwrap();
        assert_eq!(inv.args[e + 1], "low");
        let m = inv.args.iter().position(|x| x == "--model").unwrap();
        assert_eq!(inv.args[m + 1], "opus");
        // Nothing configured: the CLI's own defaults apply, no flags are added.
        let bare = adapter().build_invocation(&AgentRequest::new("x")).unwrap();
        assert!(!bare.args.contains(&"--effort".to_string()));
        assert!(!bare.args.contains(&"--model".to_string()));
        // Invalid values are rejected before anything is spawned.
        for bad in ["turbo", "HIGH", "", "high --bare"] {
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
        for bad in ["--bare", "two words", ""] {
            assert!(
                matches!(
                    adapter()
                        .build_invocation(&AgentRequest::new("x").with_model(bad))
                        .unwrap_err(),
                    OrchestraError::Config(_)
                ),
                "{bad}"
            );
        }
        for ok in ["low", "medium", "high", "xhigh", "max"] {
            adapter()
                .build_invocation(&AgentRequest::new("x").with_effort(ok))
                .unwrap();
        }
    }

    #[test]
    fn reported_model_is_the_one_that_handled_the_most_tokens() {
        let a = adapter();
        let r = a
            .parse_output(
                r#"{"is_error":false,"result":"ok","modelUsage":{
                    "claude-haiku-4-5":{"inputTokens":900,"outputTokens":10},
                    "claude-sonnet-5-5":{"inputTokens":5000,"outputTokens":700}}}"#,
            )
            .unwrap();
        assert_eq!(r.model_reported.as_deref(), Some("claude-sonnet-5-5"));
        // Absent or malformed `modelUsage` simply means "not reported".
        for body in [
            r#"{"is_error":false,"result":"ok"}"#,
            r#"{"is_error":false,"result":"ok","modelUsage":[]}"#,
            r#"{"is_error":false,"result":"ok","modelUsage":{}}"#,
        ] {
            assert_eq!(a.parse_output(body).unwrap().model_reported, None, "{body}");
        }
    }
}
