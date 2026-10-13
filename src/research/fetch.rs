//! The source fetcher: retrieves public HTML and plain-text pages safely.
//!
//! Safety properties (each is tested):
//! * only public destinations: every URL, every redirect hop and every DNS answer is checked
//!   ([`crate::research::net`]), and the connection uses exactly the checked addresses;
//! * redirects are followed by hand, a bounded number of times, never to another scheme or a
//!   refused destination; proxies from the environment are ignored so name resolution cannot be
//!   bypassed;
//! * bounded time and size: a timeout for the whole fetch, a cap on the body, a cap on the text;
//! * only HTML and plain text are read; nothing is executed, scripts are dropped;
//! * `robots.txt` is honored (a server error there means "do not fetch");
//! * nothing about the user (question, files, history) is ever sent: a fetch is a plain GET of the
//!   URL with a fixed `User-Agent` and `Accept` header.
//!
//! A page that cannot be retrieved is an error value, never "verified".

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use reqwest::header::{ACCEPT, CONTENT_TYPE, LOCATION};
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use url::Url;

use super::html::{Extracted, extract_html, meta_refresh};
use super::net::{
    FetchError, FetchPolicy, HostResolver, SafeResolver, SystemResolver, parse_public_url,
    validate_url,
};
use super::robots::Robots;
use crate::history::now_ms;

/// A page that was retrieved.
#[derive(Debug, Clone)]
pub struct FetchedPage {
    pub requested_url: String,
    /// The URL that finally answered, after redirects.
    pub final_url: String,
    pub redirects: Vec<String>,
    pub status: u16,
    pub content_type: String,
    /// Bytes read from the network (after decompression).
    pub bytes: usize,
    /// SHA-256 of the bytes read, lower-case hex.
    pub content_hash: String,
    pub retrieved_at_ms: u64,
    pub extracted: Extracted,
}

struct Raw {
    status: u16,
    content_type: String,
    body: Vec<u8>,
    final_url: Url,
    redirects: Vec<String>,
}

impl std::fmt::Debug for SourceFetcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceFetcher")
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

pub struct SourceFetcher {
    client: reqwest::Client,
    policy: FetchPolicy,
    robots: Mutex<HashMap<String, Arc<Robots>>>,
    permits: Arc<Semaphore>,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Walks the error chain for one of our own errors (raised by the resolver).
fn fetch_error_from(e: &reqwest::Error, timeout_secs: u64) -> FetchError {
    let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(c) = cause {
        if let Some(f) = c.downcast_ref::<FetchError>() {
            return f.clone();
        }
        cause = c.source();
    }
    if e.is_timeout() {
        return FetchError::Timeout(timeout_secs);
    }
    // Keep the message short and free of anything but the failing step.
    let step = if e.is_connect() {
        "could not connect"
    } else if e.is_request() {
        "request failed"
    } else {
        "failed"
    };
    FetchError::Network(step.to_string())
}

fn decode(bytes: &[u8], content_type: &str) -> String {
    let label = content_type.split(';').skip(1).find_map(|p| {
        p.trim()
            .strip_prefix("charset=")
            .map(|c| c.trim_matches('"').to_string())
    });
    let encoding = encoding_rs::Encoding::for_bom(bytes)
        .map(|(e, _)| e)
        .or_else(|| {
            label
                .as_deref()
                .and_then(|l| encoding_rs::Encoding::for_label(l.as_bytes()))
        })
        .or_else(|| {
            // <meta charset=...> in the first bytes of an HTML page.
            let head =
                String::from_utf8_lossy(&bytes[..bytes.len().min(2048)]).to_ascii_lowercase();
            let i = head.find("charset=")?;
            let rest = head[i + 8..].trim_start_matches(['"', '\'']);
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
                .unwrap_or(rest.len());
            encoding_rs::Encoding::for_label(&rest.as_bytes()[..end])
        })
        .unwrap_or(encoding_rs::UTF_8);
    let (text, _, _) = encoding.decode(bytes);
    text.into_owned()
}

fn media_type(content_type: &str) -> String {
    content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

impl SourceFetcher {
    pub fn new(policy: FetchPolicy) -> Result<Self, FetchError> {
        Self::with_resolver(policy, Arc::new(SystemResolver))
    }

    /// With a custom DNS resolver (tests script the answers).
    pub fn with_resolver(
        policy: FetchPolicy,
        resolver: Arc<dyn HostResolver>,
    ) -> Result<Self, FetchError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            // A proxy would resolve names itself and bypass the address checks.
            .no_proxy()
            .dns_resolver(Arc::new(SafeResolver::new(resolver, &policy)))
            .user_agent(policy.user_agent.clone())
            .connect_timeout(Duration::from_secs(policy.timeout_secs.min(10)))
            // No connection reuse: every request resolves and is checked again.
            .pool_max_idle_per_host(0)
            .build()
            .map_err(|e| FetchError::Network(format!("cannot build the HTTP client: {e}")))?;
        Ok(Self {
            client,
            permits: Arc::new(Semaphore::new(policy.max_concurrent.max(1))),
            policy,
            robots: Mutex::new(HashMap::new()),
        })
    }

    pub fn policy(&self) -> &FetchPolicy {
        &self.policy
    }

    /// GET with manual, validated redirects and a capped body. `accept_types`: media types to
    /// accept (checked before the body is read); `None` accepts anything.
    async fn get(
        &self,
        start: &Url,
        max_bytes: usize,
        accept_types: Option<&[&str]>,
    ) -> Result<Raw, FetchError> {
        let mut url = start.clone();
        let mut redirects = Vec::new();
        for _ in 0..=self.policy.max_redirects {
            validate_url(&url, &self.policy)?;
            let mut resp = self
                .client
                .get(url.clone())
                .header(
                    ACCEPT,
                    "text/html,application/xhtml+xml,text/plain;q=0.9,*/*;q=0.1",
                )
                .send()
                .await
                .map_err(|e| fetch_error_from(&e, self.policy.timeout_secs))?;
            let status = resp.status();
            if status.is_redirection() {
                let Some(location) = resp.headers().get(LOCATION).and_then(|v| v.to_str().ok())
                else {
                    return Err(FetchError::HttpStatus(status.as_u16()));
                };
                let next = url.join(location).map_err(|e| {
                    FetchError::RedirectRefused(location.to_string(), e.to_string())
                })?;
                validate_url(&next, &self.policy)
                    .map_err(|e| FetchError::RedirectRefused(next.to_string(), e.to_string()))?;
                redirects.push(next.to_string());
                url = next;
                continue;
            }
            let content_type = resp
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            if !status.is_success() {
                return Ok(Raw {
                    status: status.as_u16(),
                    content_type,
                    body: Vec::new(),
                    final_url: url,
                    redirects,
                });
            }
            if let Some(types) = accept_types {
                let m = media_type(&content_type);
                if !types.contains(&m.as_str()) {
                    return Err(FetchError::UnsupportedContentType(if m.is_empty() {
                        "unknown".into()
                    } else {
                        m
                    }));
                }
            }
            if resp.content_length().is_some_and(|n| n > max_bytes as u64) {
                return Err(FetchError::TooLarge(max_bytes));
            }
            let mut body = Vec::new();
            while let Some(chunk) = resp
                .chunk()
                .await
                .map_err(|e| fetch_error_from(&e, self.policy.timeout_secs))?
            {
                if body.len() + chunk.len() > max_bytes {
                    return Err(FetchError::TooLarge(max_bytes));
                }
                body.extend_from_slice(&chunk);
            }
            return Ok(Raw {
                status: status.as_u16(),
                content_type,
                body,
                final_url: url,
                redirects,
            });
        }
        Err(FetchError::TooManyRedirects(self.policy.max_redirects))
    }

    /// The robots rules for `url`'s origin (cached for the life of the fetcher).
    async fn robots_for(&self, url: &Url) -> Result<Arc<Robots>, FetchError> {
        let origin = format!("{}://{}", url.scheme(), url.host_str().unwrap_or(""));
        let origin = match url.port() {
            Some(p) => format!("{origin}:{p}"),
            None => origin,
        };
        if let Some(found) = self
            .robots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&origin)
        {
            return Ok(found.clone());
        }
        let rules = match Url::parse(&format!("{origin}/robots.txt")) {
            Ok(robots_url) => match self.get(&robots_url, 512 * 1024, None).await {
                Ok(raw) if (200..300).contains(&raw.status) => {
                    Robots::parse(&decode(&raw.body, &raw.content_type))
                }
                // 404 and other client errors: no restrictions.
                Ok(raw) if (400..500).contains(&raw.status) => Robots::allow_all(),
                // A refused destination is refused for the page too: report the real reason,
                // not a robots rule.
                Err(
                    e @ (FetchError::BlockedAddress { .. }
                    | FetchError::RedirectRefused(..)
                    | FetchError::SchemeNotAllowed(..)
                    | FetchError::PortNotAllowed(_)
                    | FetchError::Credentials
                    | FetchError::InvalidUrl(_)
                    | FetchError::Cancelled),
                ) => return Err(e),
                // Server errors, timeouts and unreachable servers: RFC 9309 says to assume the
                // site is off limits, and the reason says so.
                Ok(raw) => {
                    return Err(FetchError::RobotsUnavailable(format!(
                        "HTTP status {}",
                        raw.status
                    )));
                }
                Err(e) => return Err(FetchError::RobotsUnavailable(e.to_string())),
            },
            Err(_) => return Err(FetchError::RobotsUnavailable("bad origin".to_string())),
        };
        let rules = Arc::new(rules);
        self.robots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(origin, rules.clone());
        Ok(rules)
    }

    /// Fetches one page. Never retried.
    pub async fn fetch(
        &self,
        input: &str,
        cancel: &CancellationToken,
    ) -> Result<FetchedPage, FetchError> {
        let start = parse_public_url(input, &self.policy)?;
        let _permit = tokio::select! {
            p = self.permits.acquire() => p.map_err(|_| FetchError::Cancelled)?,
            _ = cancel.cancelled() => return Err(FetchError::Cancelled),
        };
        let work = async {
            let types = ["text/html", "application/xhtml+xml", "text/plain"];
            let agent = self
                .policy
                .user_agent
                .split(['/', ' '])
                .next()
                .unwrap_or("ai-orchestra")
                .to_string();
            let mut target = start.clone();
            let mut meta_redirects: Vec<String> = Vec::new();
            loop {
                if self.policy.respect_robots {
                    let rules = self.robots_for(&target).await?;
                    let path = match target.query() {
                        Some(q) => format!("{}?{q}", target.path()),
                        None => target.path().to_string(),
                    };
                    if !rules.allowed(&agent, &path) {
                        return Err(FetchError::RobotsDisallowed);
                    }
                }
                let raw = self
                    .get(&target, self.policy.max_page_bytes, Some(&types))
                    .await?;
                if !(200..300).contains(&raw.status) {
                    return Err(FetchError::HttpStatus(raw.status));
                }
                let media = media_type(&raw.content_type);
                let text = decode(&raw.body, &raw.content_type);
                let extracted = if media == "text/plain" {
                    let mut t = text.trim().to_string();
                    let truncated = t.len() > self.policy.max_text_bytes;
                    if truncated {
                        let mut end = self.policy.max_text_bytes;
                        while !t.is_char_boundary(end) {
                            end -= 1;
                        }
                        t.truncate(end);
                    }
                    Extracted {
                        text: t,
                        text_truncated: truncated,
                        ..Default::default()
                    }
                } else {
                    extract_html(&text, &raw.final_url, self.policy.max_text_bytes)
                };
                // A tiny page whose only job is `<meta http-equiv="refresh">` is a redirect: follow it
                // (at most a few times) through the same checks as any other hop.
                if media != "text/plain"
                    && extracted.text.chars().count() < 200
                    && meta_redirects.len() < 3
                    && let Some(next) = meta_refresh(&text, &raw.final_url)
                {
                    validate_url(&next, &self.policy).map_err(|e| {
                        FetchError::RedirectRefused(next.to_string(), e.to_string())
                    })?;
                    meta_redirects.extend(raw.redirects);
                    meta_redirects.push(next.to_string());
                    target = next;
                    continue;
                }
                let mut redirects = meta_redirects;
                redirects.extend(raw.redirects);
                return Ok(FetchedPage {
                    requested_url: start.to_string(),
                    final_url: raw.final_url.to_string(),
                    redirects,
                    status: raw.status,
                    content_type: raw.content_type,
                    bytes: raw.body.len(),
                    content_hash: hex(&Sha256::digest(&raw.body)),
                    retrieved_at_ms: now_ms(),
                    extracted,
                });
            }
        };
        tokio::select! {
            r = tokio::time::timeout(Duration::from_secs(self.policy.timeout_secs), work) => {
                r.unwrap_or(Err(FetchError::Timeout(self.policy.timeout_secs)))
            }
            _ = cancel.cancelled() => Err(FetchError::Cancelled),
        }
    }

    /// Fetches many pages with bounded concurrency; results keep the input order.
    pub async fn fetch_all(
        self: &Arc<Self>,
        urls: &[String],
        cancel: &CancellationToken,
    ) -> Vec<(String, Result<FetchedPage, FetchError>)> {
        let mut tasks = tokio::task::JoinSet::new();
        for (i, u) in urls.iter().cloned().enumerate() {
            let (me, cancel) = (self.clone(), cancel.clone());
            tasks.spawn(async move {
                let r = me.fetch(&u, &cancel).await;
                (i, u, r)
            });
        }
        let mut out: Vec<Option<(String, Result<FetchedPage, FetchError>)>> =
            (0..urls.len()).map(|_| None).collect();
        while let Some(done) = tasks.join_next().await {
            if let Ok((i, u, r)) = done {
                out[i] = Some((u, r));
            }
        }
        out.into_iter()
            .enumerate()
            .map(|(i, o)| {
                o.unwrap_or_else(|| {
                    (
                        urls[i].clone(),
                        Err(FetchError::Network("fetch task failed".into())),
                    )
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    use async_trait::async_trait;

    use super::super::testserver::{Route, TestServer};
    use super::*;

    fn fetcher(policy: FetchPolicy) -> Arc<SourceFetcher> {
        Arc::new(SourceFetcher::new(policy).unwrap())
    }

    fn local() -> FetchPolicy {
        FetchPolicy {
            respect_robots: false,
            ..FetchPolicy::for_local_tests()
        }
    }

    const PAGE: &str = r#"<html><head><title>Oscilloscope X</title><meta name="author" content="Ada"><meta property="article:published_time" content="2025-03-04"></head>
        <body><main><h1>Oscilloscope X</h1><p>The analog bandwidth is 100 MHz.</p></main></body></html>"#;

    #[tokio::test]
    async fn a_public_page_is_retrieved_with_metadata_hash_and_timestamp() {
        let server = TestServer::start(vec![("/spec", Route::Html(PAGE.into()))]).await;
        let page = fetcher(local())
            .fetch(&server.url("/spec"), &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(page.status, 200);
        assert_eq!(page.extracted.title.as_deref(), Some("Oscilloscope X"));
        assert_eq!(page.extracted.author.as_deref(), Some("Ada"));
        assert_eq!(page.extracted.published.as_deref(), Some("2025-03-04"));
        assert_eq!(
            page.extracted.updated, None,
            "no update date is stated, so none is invented"
        );
        assert!(page.extracted.text.contains("analog bandwidth is 100 MHz"));
        assert_eq!(page.content_hash.len(), 64);
        assert!(page.retrieved_at_ms > 1_700_000_000_000);
        assert_eq!(page.requested_url, page.final_url);
        // Same bytes, same hash.
        let again = fetcher(local())
            .fetch(&server.url("/spec"), &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(page.content_hash, again.content_hash);
    }

    #[tokio::test]
    async fn plain_text_is_accepted_and_other_formats_are_unsupported() {
        let server = TestServer::start(vec![
            ("/notes.txt", Route::Text("plain facts".into())),
            (
                "/data.json",
                Route::Raw {
                    status: 200,
                    content_type: "application/json".into(),
                    body: b"{}".to_vec(),
                    extra_headers: vec![],
                },
            ),
            (
                "/doc.pdf",
                Route::Raw {
                    status: 200,
                    content_type: "application/pdf".into(),
                    body: b"%PDF-1.7".to_vec(),
                    extra_headers: vec![],
                },
            ),
            (
                "/blob",
                Route::Raw {
                    status: 200,
                    content_type: String::new(),
                    body: b"x".to_vec(),
                    extra_headers: vec![],
                },
            ),
        ])
        .await;
        let f = fetcher(local());
        let c = CancellationToken::new();
        assert_eq!(
            f.fetch(&server.url("/notes.txt"), &c)
                .await
                .unwrap()
                .extracted
                .text,
            "plain facts"
        );
        for p in ["/data.json", "/doc.pdf", "/blob"] {
            let e = f.fetch(&server.url(p), &c).await.unwrap_err();
            assert!(
                matches!(e, FetchError::UnsupportedContentType(_)),
                "{p}: {e:?}"
            );
        }
    }

    #[tokio::test]
    async fn http_errors_are_errors_not_evidence() {
        let server = TestServer::start(vec![
            (
                "/gone",
                Route::Raw {
                    status: 404,
                    content_type: "text/html".into(),
                    body: b"<p>nothing</p>".to_vec(),
                    extra_headers: vec![],
                },
            ),
            (
                "/boom",
                Route::Raw {
                    status: 500,
                    content_type: "text/html".into(),
                    body: b"<p>oops</p>".to_vec(),
                    extra_headers: vec![],
                },
            ),
            (
                "/limit",
                Route::Raw {
                    status: 429,
                    content_type: "text/html".into(),
                    body: b"slow down".to_vec(),
                    extra_headers: vec![],
                },
            ),
        ])
        .await;
        let f = fetcher(local());
        let c = CancellationToken::new();
        assert_eq!(
            f.fetch(&server.url("/gone"), &c).await.unwrap_err(),
            FetchError::HttpStatus(404)
        );
        assert_eq!(
            f.fetch(&server.url("/boom"), &c).await.unwrap_err(),
            FetchError::HttpStatus(500)
        );
        assert_eq!(
            f.fetch(&server.url("/limit"), &c).await.unwrap_err(),
            FetchError::HttpStatus(429)
        );
        assert_eq!(
            server.hits("/limit"),
            1,
            "a rate-limit answer is not retried"
        );
    }

    #[tokio::test]
    async fn a_slow_server_times_out() {
        let server = TestServer::start(vec![(
            "/slow",
            Route::Slow(Duration::from_secs(5), Box::new(Route::Html(PAGE.into()))),
        )])
        .await;
        let f = fetcher(FetchPolicy {
            timeout_secs: 1,
            ..local()
        });
        let t = std::time::Instant::now();
        let e = f
            .fetch(&server.url("/slow"), &CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(e, FetchError::Timeout(1));
        assert!(t.elapsed() < Duration::from_secs(4));
    }

    #[tokio::test]
    async fn an_oversized_body_is_refused() {
        let server = TestServer::start(vec![("/huge", Route::Huge(50_000))]).await;
        let f = fetcher(FetchPolicy {
            max_page_bytes: 10_000,
            ..local()
        });
        assert_eq!(
            f.fetch(&server.url("/huge"), &CancellationToken::new())
                .await
                .unwrap_err(),
            FetchError::TooLarge(10_000)
        );
    }

    #[tokio::test]
    async fn a_body_that_lies_about_its_length_is_still_capped_while_streaming() {
        // No Content-Length check can help here, so the streaming cap must.
        let big = vec![b'a'; 50_000];
        let server = TestServer::start(vec![(
            "/chunked",
            Route::Raw {
                status: 200,
                content_type: "text/plain".into(),
                body: big,
                extra_headers: vec![],
            },
        )])
        .await;
        let f = fetcher(FetchPolicy {
            max_page_bytes: 5_000,
            ..local()
        });
        assert!(matches!(
            f.fetch(&server.url("/chunked"), &CancellationToken::new())
                .await
                .unwrap_err(),
            FetchError::TooLarge(_)
        ));
    }

    #[tokio::test]
    async fn extracted_text_is_capped_separately_from_the_body() {
        let body = format!(
            "<html><body><p>{}</p></body></html>",
            "word ".repeat(10_000)
        );
        let server = TestServer::start(vec![("/long", Route::Html(body))]).await;
        let f = fetcher(FetchPolicy {
            max_text_bytes: 2_000,
            ..local()
        });
        let page = f
            .fetch(&server.url("/long"), &CancellationToken::new())
            .await
            .unwrap();
        assert!(page.extracted.text_truncated && page.extracted.text.len() <= 2_000);
    }

    #[tokio::test]
    async fn charsets_are_decoded() {
        let latin1 = Route::Raw {
            status: 200,
            content_type: "text/html; charset=iso-8859-1".into(),
            body: b"<html><body><p>caf\xe9 cr\xe8me</p></body></html>".to_vec(),
            extra_headers: vec![],
        };
        let sniffed = Route::Raw { status: 200, content_type: "text/html".into(), body: b"<html><head><meta charset=\"windows-1252\"></head><body><p>na\xefve</p></body></html>".to_vec(), extra_headers: vec![] };
        let server = TestServer::start(vec![("/latin1", latin1), ("/sniffed", sniffed)]).await;
        let f = fetcher(local());
        let c = CancellationToken::new();
        assert!(
            f.fetch(&server.url("/latin1"), &c)
                .await
                .unwrap()
                .extracted
                .text
                .contains("café crème")
        );
        assert!(
            f.fetch(&server.url("/sniffed"), &c)
                .await
                .unwrap()
                .extracted
                .text
                .contains("naïve")
        );
    }

    #[tokio::test]
    async fn safe_redirects_are_followed_and_recorded_with_the_final_url() {
        let server = TestServer::start(vec![
            ("/old", Route::Redirect(301, "/new".into())),
            ("/new", Route::Html(PAGE.into())),
        ])
        .await;
        let page = fetcher(local())
            .fetch(&server.url("/old"), &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(page.requested_url, server.url("/old"));
        assert_eq!(page.final_url, server.url("/new"));
        assert_eq!(page.redirects, vec![server.url("/new")]);
    }

    #[tokio::test]
    async fn redirects_to_private_destinations_or_other_schemes_are_refused() {
        let server = TestServer::start(vec![
            (
                "/to-metadata",
                Route::Redirect(302, "http://169.254.169.254/latest/meta-data/".into()),
            ),
            (
                "/to-lan",
                Route::Redirect(302, "http://10.0.0.5/admin".into()),
            ),
            (
                "/to-localhost",
                Route::Redirect(302, "http://localhost/admin".into()),
            ),
            (
                "/to-file",
                Route::Redirect(302, "file:///etc/passwd".into()),
            ),
            (
                "/to-userinfo",
                Route::Redirect(302, "http://user:pw@example.com/".into()),
            ),
            (
                "/to-port",
                Route::Redirect(302, "http://example.com:6379/".into()),
            ),
        ])
        .await;
        // Hop 1 is the local test server (its port is allowed through the test policy only for it).
        let f = Arc::new(
            SourceFetcher::new(FetchPolicy {
                allow_any_port: true,
                ..local()
            })
            .unwrap(),
        );
        for path in [
            "/to-metadata",
            "/to-lan",
            "/to-localhost",
            "/to-file",
            "/to-userinfo",
        ] {
            let e = f
                .fetch(&server.url(path), &CancellationToken::new())
                .await
                .unwrap_err();
            assert!(
                matches!(e, FetchError::RedirectRefused(..)),
                "{path}: {e:?}"
            );
        }
        // A redirect to a non-standard port is refused when ports are restricted. The first hop
        // needs the test port, so check the redirect rule directly.
        let strict = FetchPolicy {
            allow_any_port: false,
            ..FetchPolicy::default()
        };
        assert!(validate_url(&Url::parse("http://example.com:6379/").unwrap(), &strict).is_err());
        assert_eq!(server.hits("/latest/meta-data/"), 0);
    }

    #[tokio::test]
    async fn redirect_loops_are_cut_off() {
        let server = TestServer::start(vec![
            ("/a", Route::Redirect(302, "/b".into())),
            ("/b", Route::Redirect(302, "/a".into())),
        ])
        .await;
        let f = fetcher(FetchPolicy {
            max_redirects: 3,
            ..local()
        });
        assert_eq!(
            f.fetch(&server.url("/a"), &CancellationToken::new())
                .await
                .unwrap_err(),
            FetchError::TooManyRedirects(3)
        );
    }

    #[tokio::test]
    async fn the_default_policy_refuses_local_servers_before_any_request() {
        let server = TestServer::start(vec![("/secret", Route::Html(PAGE.into()))]).await;
        let f = fetcher(FetchPolicy::default());
        for url in [
            server.url("/secret"),
            format!("https://127.0.0.1:{}/secret", server.addr.port()),
            "http://localhost/".to_string(),
            "file:///etc/passwd".to_string(),
        ] {
            assert!(
                f.fetch(&url, &CancellationToken::new()).await.is_err(),
                "{url}"
            );
        }
        assert_eq!(server.total_hits(), 0, "nothing reached the server");
    }

    struct Fixed(Vec<IpAddr>);

    #[async_trait]
    impl HostResolver for Fixed {
        async fn lookup(&self, _h: &str, _p: u16) -> std::io::Result<Vec<IpAddr>> {
            Ok(self.0.clone())
        }
    }

    #[tokio::test]
    async fn a_name_that_resolves_to_a_private_address_is_refused_at_connection_time() {
        let server = TestServer::start(vec![("/x", Route::Html(PAGE.into()))]).await;
        // Policy without the test exception: 127.0.0.1 is not public.
        let policy = FetchPolicy {
            allow_http: true,
            allow_any_port: true,
            respect_robots: false,
            ..FetchPolicy::default()
        };
        let f = Arc::new(
            SourceFetcher::with_resolver(
                policy,
                Arc::new(Fixed(vec!["127.0.0.1".parse().unwrap()])),
            )
            .unwrap(),
        );
        let e = f
            .fetch(
                &server.named_url("innocent.example.com", "/x"),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(matches!(e, FetchError::BlockedAddress { .. }), "{e:?}");
        assert_eq!(server.total_hits(), 0);
    }

    #[tokio::test]
    async fn a_private_destination_is_reported_as_such_even_with_robots_checking_on() {
        // The robots.txt request goes through the same checks: the page is refused for its address,
        // and the error says so instead of blaming a robots rule.
        let server = TestServer::start(vec![("/x", Route::Html(PAGE.into()))]).await;
        let policy = FetchPolicy {
            allow_http: true,
            allow_any_port: true,
            respect_robots: true,
            ..FetchPolicy::default()
        };
        let f = Arc::new(
            SourceFetcher::with_resolver(
                policy,
                Arc::new(Fixed(vec!["10.0.0.7".parse().unwrap()])),
            )
            .unwrap(),
        );
        let e = f
            .fetch(
                &server.named_url("intranet-lookalike.example.com", "/x"),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(matches!(e, FetchError::BlockedAddress { .. }), "{e:?}");
        assert_eq!(server.total_hits(), 0);
    }

    #[tokio::test]
    async fn a_meta_refresh_page_is_followed_through_the_same_checks() {
        let server = TestServer::start(vec![
            ("/old", Route::Html("<html><head><meta http-equiv=\"refresh\" content=\"0; url=/new\"></head><body>Click here to be redirected.</body></html>".into())),
            ("/new", Route::Html(PAGE.into())),
            ("/evil", Route::Html("<html><head><meta http-equiv=\"refresh\" content=\"0; url=http://169.254.169.254/latest/meta-data/\"></head><body>Redirecting</body></html>".into())),
            ("/loop", Route::Html("<html><head><meta http-equiv=\"refresh\" content=\"0; url=/loop\"></head><body>Again</body></html>".into())),
        ])
        .await;
        let f = fetcher(local());
        let c = CancellationToken::new();
        let page = f.fetch(&server.url("/old"), &c).await.unwrap();
        assert_eq!(page.final_url, server.url("/new"));
        assert_eq!(page.redirects, vec![server.url("/new")]);
        assert!(page.extracted.text.contains("analog bandwidth is 100 MHz"));
        let e = f.fetch(&server.url("/evil"), &c).await.unwrap_err();
        assert!(matches!(e, FetchError::RedirectRefused(..)), "{e:?}");
        // A page that refreshes to itself ends after a few hops with what it has.
        let page = f.fetch(&server.url("/loop"), &c).await.unwrap();
        assert!(server.hits("/loop") <= 4, "{}", server.hits("/loop"));
        assert!(page.extracted.text.contains("Again"));
    }

    struct Rebinding(Mutex<usize>);

    #[async_trait]
    impl HostResolver for Rebinding {
        async fn lookup(&self, _h: &str, _p: u16) -> std::io::Result<Vec<IpAddr>> {
            let mut n = self.0.lock().unwrap();
            *n += 1;
            // First answer: a public address (what a validator would see). Later: loopback.
            Ok(vec![if *n == 1 {
                "93.184.216.34".parse().unwrap()
            } else {
                "127.0.0.1".parse().unwrap()
            }])
        }
    }

    #[tokio::test]
    async fn dns_rebinding_cannot_slip_a_private_address_past_the_check() {
        // The connection is made to what the checked resolver returned. With a public first answer
        // the client tries 93.184.216.34 (unreachable in a test, so it fails to connect or times
        // out) and NEVER reaches the local server, because the loopback answer would be refused.
        let server = TestServer::start(vec![("/x", Route::Html(PAGE.into()))]).await;
        let policy = FetchPolicy {
            allow_http: true,
            allow_any_port: true,
            respect_robots: false,
            timeout_secs: 2,
            ..FetchPolicy::default()
        };
        let f = Arc::new(
            SourceFetcher::with_resolver(policy, Arc::new(Rebinding(Mutex::new(0)))).unwrap(),
        );
        let r = f
            .fetch(
                &server.named_url("rebind.example.com", "/x"),
                &CancellationToken::new(),
            )
            .await;
        assert!(r.is_err());
        assert_eq!(server.total_hits(), 0);
        // Second fetch: the (now loopback) answer is refused outright.
        let e = f
            .fetch(
                &server.named_url("rebind.example.com", "/x"),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(matches!(e, FetchError::BlockedAddress { .. }), "{e:?}");
        assert_eq!(server.total_hits(), 0);
    }

    #[tokio::test]
    async fn robots_txt_is_honored() {
        let server = TestServer::start(vec![
            (
                "/robots.txt",
                Route::Text("User-agent: *\nDisallow: /private/\n".into()),
            ),
            ("/private/page", Route::Html(PAGE.into())),
            ("/open/page", Route::Html(PAGE.into())),
        ])
        .await;
        let f = fetcher(FetchPolicy::for_local_tests());
        let c = CancellationToken::new();
        assert!(f.fetch(&server.url("/open/page"), &c).await.is_ok());
        assert_eq!(
            f.fetch(&server.url("/private/page"), &c).await.unwrap_err(),
            FetchError::RobotsDisallowed
        );
        assert_eq!(
            server.hits("/private/page"),
            0,
            "a disallowed page is never requested"
        );
        assert_eq!(
            server.hits("/robots.txt"),
            1,
            "robots.txt is fetched once per origin"
        );
    }

    #[tokio::test]
    async fn a_missing_robots_file_allows_and_a_server_error_forbids() {
        let ok = TestServer::start(vec![("/page", Route::Html(PAGE.into()))]).await;
        assert!(
            fetcher(FetchPolicy::for_local_tests())
                .fetch(&ok.url("/page"), &CancellationToken::new())
                .await
                .is_ok()
        );
        let broken = TestServer::start(vec![
            (
                "/robots.txt",
                Route::Raw {
                    status: 503,
                    content_type: "text/plain".into(),
                    body: vec![],
                    extra_headers: vec![],
                },
            ),
            ("/page", Route::Html(PAGE.into())),
        ])
        .await;
        let e = fetcher(FetchPolicy::for_local_tests())
            .fetch(&broken.url("/page"), &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(
            matches!(e, FetchError::RobotsUnavailable(ref d) if d.contains("503")),
            "{e:?}"
        );
        assert_eq!(broken.hits("/page"), 0);
    }

    #[tokio::test]
    async fn fetch_all_keeps_order_reports_partial_failures_and_bounds_concurrency() {
        let server = TestServer::start(vec![
            ("/a", Route::Html(PAGE.into())),
            (
                "/b",
                Route::Raw {
                    status: 404,
                    content_type: "text/html".into(),
                    body: vec![],
                    extra_headers: vec![],
                },
            ),
            ("/c", Route::Text("third".into())),
        ])
        .await;
        let f = fetcher(FetchPolicy {
            max_concurrent: 1,
            ..local()
        });
        let urls = vec![
            server.url("/a"),
            server.url("/b"),
            server.url("/c"),
            "ftp://example.com/x".to_string(),
        ];
        let out = f.fetch_all(&urls, &CancellationToken::new()).await;
        assert_eq!(out.len(), 4);
        assert!(out[0].1.is_ok() && out[2].1.is_ok());
        assert_eq!(out[1].1.as_ref().unwrap_err(), &FetchError::HttpStatus(404));
        assert!(matches!(
            out[3].1.as_ref().unwrap_err(),
            FetchError::SchemeNotAllowed(..)
        ));
        for (i, (u, _)) in out.iter().enumerate() {
            assert_eq!(u, &urls[i]);
        }
    }

    #[tokio::test]
    async fn cancellation_stops_a_fetch_promptly() {
        let server = TestServer::start(vec![(
            "/slow",
            Route::Slow(Duration::from_secs(10), Box::new(Route::Html(PAGE.into()))),
        )])
        .await;
        let f = fetcher(local());
        let cancel = CancellationToken::new();
        let c2 = cancel.clone();
        let url = server.url("/slow");
        let task = tokio::spawn(async move { f.fetch(&url, &c2).await });
        while server.hits("/slow") == 0 {
            tokio::task::yield_now().await;
        }
        cancel.cancel();
        let r = tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .expect("prompt")
            .unwrap();
        assert_eq!(r.unwrap_err(), FetchError::Cancelled);
    }

    #[test]
    fn decoding_defaults_to_utf8_and_never_panics_on_garbage() {
        assert_eq!(decode("héllo".as_bytes(), "text/html"), "héllo");
        assert!(!decode(&[0xff, 0xfe, 0x00, 0x41], "text/html; charset=nonsense").is_empty());
        assert_eq!(decode(b"", "text/plain"), "");
    }
}
