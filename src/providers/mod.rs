//! Concrete provider adapters. Each file isolates everything specific to one
//! CLI: flags, output format and auth probing.

use std::path::Path;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::process::{CommandSpec, ProcessOutput};
use crate::provider::{AgentProvider, InvokeContext, PROBE_TIMEOUT, ProviderRegistry};

pub mod claude;
pub mod codex;
pub mod google;
pub mod mock;

pub use claude::ClaudeAdapter;
pub use codex::CodexAdapter;
pub use google::GoogleAdapter;

/// Registry containing the three built-in adapters, configured from `config`.
pub fn builtin_registry(config: &Config) -> ProviderRegistry {
    let providers: Vec<Arc<dyn AgentProvider>> = vec![
        Arc::new(ClaudeAdapter::new(config.provider("claude"))),
        Arc::new(CodexAdapter::new(config.provider("codex"))),
        Arc::new(GoogleAdapter::new(config.provider("google"))),
    ];
    ProviderRegistry::new(providers)
}

/// Runs a short, non-AI diagnostic command (`--version`, auth status).
/// Returns `None` if it could not be run at all.
pub(crate) async fn run_probe(
    ctx: &InvokeContext,
    executable: &Path,
    args: &[&str],
) -> Option<ProcessOutput> {
    let mut spec = CommandSpec::new(
        executable,
        args.iter().map(|a| a.to_string()).collect(),
        PROBE_TIMEOUT,
    );
    spec.env_remove = ctx.scrub_env.clone();
    spec.max_stdout_bytes = 64 * 1024;
    ctx.process.run(spec, &CancellationToken::new()).await.ok()
}

/// URLs found in free text: `http(s)://` up to whitespace or a closing delimiter, with trailing
/// punctuation removed. Duplicates are dropped. Only the text is read; nothing is fetched.
pub(crate) fn urls_in_text(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find("http") {
        let tail = &rest[i..];
        if tail.starts_with("http://") || tail.starts_with("https://") {
            let end = tail
                .find(|c: char| {
                    c.is_whitespace()
                        || matches!(c, '"' | '\'' | '<' | '>' | ')' | ']' | '}' | '`' | '\\')
                })
                .unwrap_or(tail.len());
            let url = tail[..end].trim_end_matches(['.', ',', ';', ':', '!', '?', '*']);
            if url.len() > "https://".len() && !out.iter().any(|u| u == url) {
                out.push(url.to_string());
            }
            rest = &tail[end.max(4)..];
        } else {
            rest = &tail[4..];
        }
    }
    out
}

/// Every URL in any string inside a JSON value, in order of appearance. Shape-agnostic on
/// purpose: the structure of the web-tool events of the provider CLIs is not documented, so no
/// particular field names are assumed.
pub(crate) fn urls_in_value(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(s) => {
            for u in urls_in_text(s) {
                if !out.contains(&u) {
                    out.push(u);
                }
            }
        }
        serde_json::Value::Array(items) => items.iter().for_each(|v| urls_in_value(v, out)),
        serde_json::Value::Object(map) => map.values().for_each(|v| urls_in_value(v, out)),
        _ => {}
    }
}

/// Shortens a model-supplied or CLI-supplied string for use in diagnostics.
pub(crate) fn snippet(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let head: String = text.chars().take(max).collect();
        format!("{head}...")
    }
}
