//! Source discovery: how candidate URLs are found, behind a small interface so a later milestone
//! can add methods (a locally configured, authorized search adapter, for example) without touching
//! the rest of the research workflow.
//!
//! Every candidate carries the method that produced it. None of them is evidence: a candidate
//! becomes a source only when [`super::fetch::SourceFetcher`] retrieves it. Search engines are
//! never scraped, and no commercial search API is required.
//!
//! Methods shipped:
//! * [`UserUrlDiscovery`]: the URLs the user gave. Always available, and enough on their own.
//! * [`ProviderSearchDiscovery`]: an agent whose CLI has its own web search is asked to find pages.
//!   URLs that appeared in its tool output are `ProviderToolReported`; URLs it only wrote in its
//!   reply are `ProviderTextReported` (the weakest, since a model can invent a URL).

use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use super::engine::{PROVIDER_URL_MAX, parse_plan, planning_prompt};
use super::source::{Discovery, DiscoveryMethod};
use crate::debate::{Role, Runner, first_line};
use crate::error::{OrchestraError, Result};
use crate::provider::AgentProvider;
use crate::types::AgentRequest;

/// What to find.
#[derive(Debug, Clone)]
pub struct DiscoveryRequest<'a> {
    pub question: &'a str,
    pub max_sources: usize,
    pub freshness_days: Option<u32>,
    pub user_urls: &'a [String],
    /// The text is a claim to verify: look for pages that confirm it and pages that contradict it.
    pub claim_check: bool,
}

/// A URL found by a discovery method, with how it was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub url: String,
    pub discovery: Discovery,
}

#[derive(Debug, Clone, Default)]
pub struct DiscoveryOutcome {
    pub candidates: Vec<Candidate>,
    pub subquestions: Vec<String>,
    pub queries: Vec<String>,
    /// What could not be done, in plain words.
    pub limitations: Vec<String>,
    /// Agent calls the method made.
    pub agent_calls: usize,
    /// A step that was meant to work failed (the search agent errored).
    pub failed: bool,
}

#[async_trait]
pub trait SourceDiscovery: Send + Sync {
    fn name(&self) -> String;
    async fn discover(
        &self,
        request: &DiscoveryRequest<'_>,
        cancel: &CancellationToken,
    ) -> Result<DiscoveryOutcome>;
}

/// The URLs the user supplied.
pub struct UserUrlDiscovery;

#[async_trait]
impl SourceDiscovery for UserUrlDiscovery {
    fn name(&self) -> String {
        "user-provided URLs".to_string()
    }

    async fn discover(
        &self,
        request: &DiscoveryRequest<'_>,
        _cancel: &CancellationToken,
    ) -> Result<DiscoveryOutcome> {
        Ok(DiscoveryOutcome {
            candidates: request
                .user_urls
                .iter()
                .map(|u| Candidate {
                    url: u.clone(),
                    discovery: Discovery {
                        method: DiscoveryMethod::UserProvided,
                        provider: None,
                    },
                })
                .collect(),
            ..Default::default()
        })
    }
}

/// An agent with its own web search, asked to find pages. One call, read-only.
pub struct ProviderSearchDiscovery<'a> {
    pub(crate) agent: Arc<dyn AgentProvider>,
    pub(crate) runner: &'a Runner,
    pub(crate) request: &'a (dyn Fn(String) -> AgentRequest + Sync),
}

#[async_trait]
impl SourceDiscovery for ProviderSearchDiscovery<'_> {
    fn name(&self) -> String {
        format!("web search by {}", self.agent.id())
    }

    async fn discover(
        &self,
        request: &DiscoveryRequest<'_>,
        cancel: &CancellationToken,
    ) -> Result<DiscoveryOutcome> {
        let name = self.agent.id().to_string();
        let prompt = planning_prompt(
            request.question,
            request.max_sources,
            request.freshness_days,
            request.user_urls,
            request.claim_check,
        );
        let (result, _) = self
            .runner
            .call(
                self.agent.clone(),
                (self.request)(prompt).with_web(true),
                0,
                Role::Planning,
                cancel,
            )
            .await;
        let mut out = DiscoveryOutcome {
            agent_calls: 1,
            ..Default::default()
        };
        match result {
            Ok(resp) => {
                let plan = parse_plan(&resp.text);
                out.subquestions = plan.subquestions;
                out.queries = plan.queries;
                let (mut ignored, mut from_tool, mut from_text) = (0usize, 0usize, 0usize);
                for r in &resp.reported_urls {
                    if r.url.len() > PROVIDER_URL_MAX {
                        ignored += 1;
                    } else {
                        from_tool += 1;
                        out.candidates.push(Candidate {
                            url: r.url.clone(),
                            discovery: Discovery {
                                method: DiscoveryMethod::ProviderToolReported,
                                provider: Some(name.clone()),
                            },
                        });
                    }
                }
                for u in &plan.candidates {
                    if u.len() > PROVIDER_URL_MAX {
                        ignored += 1;
                    } else {
                        from_text += 1;
                        out.candidates.push(Candidate {
                            url: u.clone(),
                            discovery: Discovery {
                                method: DiscoveryMethod::ProviderTextReported,
                                provider: Some(name.clone()),
                            },
                        });
                    }
                }
                if from_tool == 0 && from_text == 0 {
                    out.limitations.push(format!(
                        "the search agent ({name}) returned no usable source URLs"
                    ));
                } else if from_tool == 0 {
                    out.limitations.push(format!(
                        "the CLI of {name} did not expose its search results, so the sources are URLs the agent wrote in its reply; each was fetched and checked independently before being used"
                    ));
                }
                if ignored > 0 {
                    out.limitations
                        .push(format!("{ignored} reported URL(s) were ignored (too long)"));
                }
            }
            Err(OrchestraError::Cancelled { provider }) if cancel.is_cancelled() => {
                return Err(OrchestraError::Cancelled { provider });
            }
            Err(e) => {
                out.failed = true;
                out.limitations.push(format!(
                    "source discovery by {name} failed: {}",
                    first_line(&e.to_string())
                ));
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::debate::Emitter;
    use crate::providers::mock::{MockProvider, MockReply};
    use crate::store::ConversationStore;

    fn runner() -> Runner {
        use std::sync::Mutex;
        let store = Arc::new(ConversationStore::new(100));
        let conversation = store.new_conversation();
        Runner {
            emit: Emitter(None),
            store,
            ctx: crate::provider::InvokeContext::from_config(&crate::config::Config::default()),
            conversation,
            overrides: Arc::new(Default::default()),
            steps: Arc::new(Mutex::new(Vec::new())),
            started: std::time::Instant::now(),
            smart: None,
            style: None,
        }
    }

    fn req() -> DiscoveryRequest<'static> {
        DiscoveryRequest {
            question: "q",
            max_sources: 3,
            freshness_days: None,
            user_urls: &[],
            claim_check: false,
        }
    }

    #[tokio::test]
    async fn user_urls_become_user_provided_candidates() {
        let urls = vec![
            "https://a.example/x".to_string(),
            "https://b.example/y".to_string(),
        ];
        let r = DiscoveryRequest {
            user_urls: &urls,
            ..req()
        };
        let out = UserUrlDiscovery
            .discover(&r, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(out.candidates.len(), 2);
        assert!(
            out.candidates
                .iter()
                .all(|c| c.discovery.method == DiscoveryMethod::UserProvided
                    && c.discovery.provider.is_none())
        );
        assert_eq!(out.agent_calls, 0);
    }

    #[tokio::test]
    async fn provider_search_separates_tool_reported_from_text_written_urls() {
        let agent: Arc<dyn AgentProvider> = Arc::new(MockProvider::new("searcher").with_web_search().with_reply(MockReply::WebText {
            text: r#"{"sources":[{"url":"https://written.example/a"},{"url":"https://written.example/b"}]}"#.into(),
            urls: vec!["https://tool.example/found".into()],
        }));
        let runner = runner();
        let build = |p: String| AgentRequest::new(p);
        let d = ProviderSearchDiscovery {
            agent,
            runner: &runner,
            request: &build,
        };
        let out = d.discover(&req(), &CancellationToken::new()).await.unwrap();
        let methods: Vec<_> = out
            .candidates
            .iter()
            .map(|c| (c.url.as_str(), c.discovery.method))
            .collect();
        assert_eq!(
            methods,
            [
                (
                    "https://tool.example/found",
                    DiscoveryMethod::ProviderToolReported
                ),
                (
                    "https://written.example/a",
                    DiscoveryMethod::ProviderTextReported
                ),
                (
                    "https://written.example/b",
                    DiscoveryMethod::ProviderTextReported
                ),
            ]
        );
        assert!(
            out.candidates
                .iter()
                .all(|c| c.discovery.provider.as_deref() == Some("searcher"))
        );
        assert_eq!((out.agent_calls, out.failed), (1, false));
        assert!(out.limitations.is_empty());
    }

    #[tokio::test]
    async fn a_search_that_exposes_nothing_or_fails_says_so() {
        let runner = runner();
        let build = |p: String| AgentRequest::new(p);
        // Only URLs written in the reply: a limitation explains how weak that is.
        let only_text: Arc<dyn AgentProvider> = Arc::new(MockProvider::new("s").with_reply(
            MockReply::Text(r#"{"sources":[{"url":"https://written.example/a"}]}"#.into()),
        ));
        let out = ProviderSearchDiscovery {
            agent: only_text,
            runner: &runner,
            request: &build,
        }
        .discover(&req(), &CancellationToken::new())
        .await
        .unwrap();
        assert!(
            out.limitations[0].contains("did not expose its search results"),
            "{:?}",
            out.limitations
        );
        // Nothing at all.
        let nothing: Arc<dyn AgentProvider> = Arc::new(
            MockProvider::new("s").with_reply(MockReply::Text("I could not find anything".into())),
        );
        let out = ProviderSearchDiscovery {
            agent: nothing,
            runner: &runner,
            request: &build,
        }
        .discover(&req(), &CancellationToken::new())
        .await
        .unwrap();
        assert!(
            out.candidates.is_empty()
                && out.limitations[0].contains("returned no usable source URLs")
        );
        // The agent fails (quota): reported, flagged as a failure, not retried.
        let quota = Arc::new(MockProvider::new("s").with_reply(MockReply::Quota("limit".into())));
        let out = ProviderSearchDiscovery {
            agent: quota.clone(),
            runner: &runner,
            request: &build,
        }
        .discover(&req(), &CancellationToken::new())
        .await
        .unwrap();
        assert!(
            out.failed && out.limitations[0].contains("source discovery by s failed"),
            "{:?}",
            out.limitations
        );
        assert_eq!(quota.requests().len(), 1);
    }

    #[tokio::test]
    async fn overlong_urls_from_a_provider_are_ignored() {
        let runner = runner();
        let build = |p: String| AgentRequest::new(p);
        let long = format!("https://x.example/{}", "a".repeat(PROVIDER_URL_MAX));
        let agent: Arc<dyn AgentProvider> =
            Arc::new(MockProvider::new("s").with_reply(MockReply::WebText {
                text: "{}".into(),
                urls: vec![long, "https://ok.example/".into()],
            }));
        let out = ProviderSearchDiscovery {
            agent,
            runner: &runner,
            request: &build,
        }
        .discover(&req(), &CancellationToken::new())
        .await
        .unwrap();
        assert_eq!(out.candidates.len(), 1);
        assert!(
            out.limitations
                .iter()
                .any(|l| l.contains("1 reported URL(s) were ignored"))
        );
    }

    #[tokio::test]
    async fn the_search_request_enables_the_web_and_stays_read_only() {
        let runner = runner();
        let build = |p: String| AgentRequest::new(p);
        let m = Arc::new(
            MockProvider::new("s")
                .with_web_search()
                .with_reply(MockReply::Text("{}".into())),
        );
        let agent: Arc<dyn AgentProvider> = m.clone();
        ProviderSearchDiscovery {
            agent,
            runner: &runner,
            request: &build,
        }
        .discover(&req(), &CancellationToken::new())
        .await
        .unwrap();
        let seen = m.requests();
        assert!(seen[0].web && seen[0].access == crate::types::AccessMode::ReadOnly);
        assert!(seen[0].prompt.contains("ONLY pages you actually found"));
    }
}
