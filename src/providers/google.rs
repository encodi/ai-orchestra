//! Adapter for Google Antigravity CLI (`agy`).
//!
//! Verified against `agy --help` (1.3.x): `-p/--print`, `--output-format json`,
//! `--mode plan|accept-edits`, `--model`.
//!
//! NOT verified against a live run (none is authorized yet): that `-p` takes
//! the prompt as its value, and the JSON envelope shape. Both come from
//! third-party documentation, which describes `agy -p "<prompt>"
//! --output-format json` returning `{conversation_id, status, response,
//! usage}` with `status == "SUCCESS"` on success. The parser is lenient about
//! everything except `response` and `status`; confirm with the live test in
//! the README.
//!
//! `agy` has no auth-status subcommand, so login state is reported as unknown.

use async_trait::async_trait;
use serde_json::Value;

use super::snippet;
use crate::config::ProviderConfig;
use crate::error::{OrchestraError, Result};
use crate::provider::{AgentProvider, Invocation, check_effort_in};
use crate::research::capabilities::{ResearchCapabilities, Support};
use crate::types::{AccessMode, AgentRequest, AgentResponse, ProviderId, Usage};

/// From `agy --help` (1.3.x): `--effort (low|medium|high|xhigh|max)`.
const GOOGLE_EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

/// The prompt travels as an argument (no stdin mode is documented), so keep it
/// well below the OS argument-size limit.
pub const MAX_ARGV_PROMPT_BYTES: usize = 128 * 1024;

pub struct GoogleAdapter {
    id: ProviderId,
    config: ProviderConfig,
}

impl GoogleAdapter {
    pub fn new(config: ProviderConfig) -> Self {
        Self {
            id: ProviderId::new("google"),
            config,
        }
    }
}

#[async_trait]
impl AgentProvider for GoogleAdapter {
    fn id(&self) -> &ProviderId {
        &self.id
    }

    fn display_name(&self) -> &str {
        "Google Antigravity CLI"
    }

    fn aliases(&self) -> &[&str] {
        &["agy", "antigravity"]
    }

    fn config(&self) -> &ProviderConfig {
        &self.config
    }

    fn research_capabilities(&self) -> ResearchCapabilities {
        ResearchCapabilities {
            web_search: Support::Unsupported,
            web_fetch: Support::Unsupported,
            exposes_source_urls: Support::Unverified,
            how: "not enabled: headless `agy -p` soft-denies tools that need approval".to_string(),
            notes: "The installed CLI has built-in `search_web` and `read_url_content` tools (named in its binary and \
                    changelog), and print mode runs a tool that needs approval only if `permissions.allow` in \
                    ~/.gemini/antigravity-cli/settings.json has a matching rule. The rule syntax for these web \
                    tools is not in the official headless documentation, and `--dangerously-skip-permissions` is \
                    never used, so web search through agy is not enabled and agy takes part in research as an \
                    analyst of retrieved evidence only. `--output-format stream-json` is documented to carry each \
                    tool step's name, parameters and output, which could expose source URLs once the rule is known."
                .to_string(),
        }
    }

    fn default_executable(&self) -> &str {
        "agy"
    }

    fn check_effort(&self, effort: &str) -> Result<()> {
        check_effort_in(&self.id, effort, GOOGLE_EFFORTS)
    }

    fn build_invocation(&self, req: &AgentRequest) -> Result<Invocation> {
        if req.prompt.len() > MAX_ARGV_PROMPT_BYTES {
            return Err(OrchestraError::PromptTooLarge {
                len: req.prompt.len(),
                max: MAX_ARGV_PROMPT_BYTES,
                target: "google (agy takes the prompt as a command-line argument)".to_string(),
            });
        }
        let mode = match req.access {
            AccessMode::ReadOnly => "plan",
            AccessMode::AllowWrite => "accept-edits",
        };
        let mut args: Vec<String> = ["--output-format", "json", "--mode", mode]
            .map(String::from)
            .to_vec();
        let (model, effort) = self.resolve_settings(req)?;
        if let Some(model) = model {
            args.extend(["--model".to_string(), model]);
        }
        if let Some(effort) = effort {
            args.extend(["--effort".to_string(), effort]);
        }
        args.extend(self.config.extra_args.iter().cloned());
        // Last on purpose: flag parsers commonly stop at the first positional.
        args.extend(["-p".to_string(), req.prompt.clone()]);
        Ok(Invocation { args, stdin: None })
    }

    fn parse_output(&self, stdout: &str) -> Result<AgentResponse> {
        let malformed = |detail: String| OrchestraError::MalformedOutput {
            provider: self.id.clone(),
            detail,
        };
        let v: Value = serde_json::from_str(stdout.trim())
            .map_err(|e| malformed(format!("invalid JSON ({e}): {}", snippet(stdout, 200))))?;
        if !v.is_object() {
            return Err(malformed("expected a JSON object".to_string()));
        }

        if let Some(status) = v["status"].as_str()
            && !status.eq_ignore_ascii_case("success")
        {
            let detail = v["error"]["message"]
                .as_str()
                .or_else(|| v["error"].as_str())
                .or_else(|| v["message"].as_str())
                .or_else(|| v["response"].as_str())
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|| format!("status {status}"));
            return Err(OrchestraError::AgentError {
                provider: self.id.clone(),
                message: detail,
            });
        }

        let text = v["response"]
            .as_str()
            .ok_or_else(|| malformed("missing `response` field".to_string()))?;
        if text.trim().is_empty() {
            // Say what the envelope did contain (never text), so an empty reply can be told apart: a model that
            // produced nothing (0 output tokens) from one that spent its turn on something else.
            let keys: Vec<&str> = v
                .as_object()
                .map(|o| o.keys().map(String::as_str).collect())
                .unwrap_or_default();
            let num = |k: &str| {
                v["usage"][k]
                    .as_u64()
                    .map_or("?".to_string(), |n| n.to_string())
            };
            let detail = format!(
                "status {}, fields {}, tokens in {} / out {}",
                v["status"].as_str().unwrap_or("none"),
                keys.join(","),
                num("input_tokens"),
                num("output_tokens")
            );
            tracing::warn!(provider = "google", %detail, "agy returned an empty response");
            return Err(OrchestraError::EmptyResponse {
                provider: self.id.clone(),
                detail: Some(detail),
            });
        }
        let mut response = AgentResponse::new(self.id.clone(), text.trim_end());
        response.session_id = v["conversation_id"].as_str().map(String::from);
        let usage = Usage {
            input_tokens: v["usage"]["input_tokens"].as_u64(),
            output_tokens: v["usage"]["output_tokens"].as_u64(),
            cached_input_tokens: v["usage"]["cached_input_tokens"].as_u64(),
        };
        response.usage = (!usage.is_empty()).then_some(usage);
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter() -> GoogleAdapter {
        GoogleAdapter::new(ProviderConfig::default())
    }

    #[test]
    fn read_only_plan_mode_and_prompt_is_last() {
        let inv = adapter()
            .build_invocation(&AgentRequest::new("find problems"))
            .unwrap();
        let m = inv.args.iter().position(|a| a == "--mode").unwrap();
        assert_eq!(inv.args[m + 1], "plan");
        assert_eq!(&inv.args[inv.args.len() - 2..], ["-p", "find problems"]);
        assert!(inv.stdin.is_none());
        assert!(
            !inv.args
                .contains(&"--dangerously-skip-permissions".to_string())
        );
    }

    #[test]
    fn write_access_maps_to_accept_edits() {
        let inv = adapter()
            .build_invocation(&AgentRequest::new("x").with_access(AccessMode::AllowWrite))
            .unwrap();
        let m = inv.args.iter().position(|a| a == "--mode").unwrap();
        assert_eq!(inv.args[m + 1], "accept-edits");
    }

    #[test]
    fn oversized_prompt_is_rejected_not_truncated() {
        let big = "a".repeat(MAX_ARGV_PROMPT_BYTES + 1);
        assert!(matches!(
            adapter()
                .build_invocation(&AgentRequest::new(big))
                .unwrap_err(),
            OrchestraError::PromptTooLarge { .. }
        ));
    }

    #[test]
    fn parses_success_envelope() {
        let r = adapter()
            .parse_output(
                r#"{"conversation_id":"c1","status":"SUCCESS","response":"Hello\n",
                    "usage":{"input_tokens":3,"output_tokens":4}}"#,
            )
            .unwrap();
        assert_eq!(r.text, "Hello");
        assert_eq!(r.session_id.as_deref(), Some("c1"));
        assert_eq!(r.usage.unwrap().output_tokens, Some(4));
    }

    #[test]
    fn non_success_status_and_malformed_output() {
        let a = adapter();
        let e = a
            .parse_output(r#"{"status":"ERROR","error":{"message":"quota exceeded"}}"#)
            .unwrap_err();
        assert!(
            matches!(e, OrchestraError::AgentError { ref message, .. } if message == "quota exceeded")
        );
        assert!(matches!(
            a.parse_output("hello").unwrap_err(),
            OrchestraError::MalformedOutput { .. }
        ));
        assert!(matches!(
            a.parse_output("[]").unwrap_err(),
            OrchestraError::MalformedOutput { .. }
        ));
        assert!(matches!(
            a.parse_output(r#"{"status":"SUCCESS"}"#).unwrap_err(),
            OrchestraError::MalformedOutput { .. }
        ));
        assert!(matches!(
            a.parse_output(r#"{"status":"SUCCESS","response":""}"#)
                .unwrap_err(),
            OrchestraError::EmptyResponse { .. }
        ));
    }

    #[test]
    fn an_empty_reply_says_what_the_envelope_did_contain_but_never_its_text() {
        let err = adapter()
            .parse_output(r#"{"conversation_id":"c1","status":"SUCCESS","response":"  ","usage":{"input_tokens":21000,"output_tokens":0}}"#)
            .unwrap_err()
            .to_string();
        assert!(err.contains("empty response [status SUCCESS"), "{err}");
        assert!(
            err.contains("tokens in 21000 / out 0") && err.contains("conversation_id"),
            "{err}"
        );
        let bare = adapter()
            .parse_output(r#"{"status":"SUCCESS","response":""}"#)
            .unwrap_err()
            .to_string();
        assert!(bare.contains("in ? / out ?"), "{bare}");
    }

    #[test]
    fn answers_to_agy_and_antigravity() {
        assert!(adapter().aliases().contains(&"agy"));
        assert!(adapter().aliases().contains(&"antigravity"));
    }

    #[test]
    fn effort_and_model_precede_the_prompt() {
        let cfg = ProviderConfig {
            model: Some("gemini-3.8-flash-low".into()),
            effort: Some("medium".into()),
            ..Default::default()
        };
        let inv = GoogleAdapter::new(cfg)
            .build_invocation(&AgentRequest::new("hello"))
            .unwrap();
        let e = inv.args.iter().position(|a| a == "--effort").unwrap();
        assert_eq!(inv.args[e + 1], "medium");
        let m = inv.args.iter().position(|a| a == "--model").unwrap();
        assert_eq!(inv.args[m + 1], "gemini-3.8-flash-low");
        assert_eq!(&inv.args[inv.args.len() - 2..], ["-p", "hello"]);
        assert!(matches!(
            adapter()
                .build_invocation(&AgentRequest::new("x").with_effort("turbo"))
                .unwrap_err(),
            OrchestraError::Config(_)
        ));
        assert!(matches!(
            adapter()
                .build_invocation(&AgentRequest::new("x").with_model("-p"))
                .unwrap_err(),
            OrchestraError::Config(_)
        ));
    }
}
