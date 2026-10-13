//! Session-scoped reuse of identical agent calls.
//!
//! Inside one consultation, two calls with the same agent, model settings and prompt would pay for
//! the same answer twice. This cache makes the second one wait for the first and share its reply.
//! Nothing is persisted, an AI reply is never turned into a verified fact, and a failed call is
//! never re-asked on anyone's behalf: whoever shared it receives the failure.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};
use tokio::sync::OnceCell;

use crate::error::OrchestraError;
use crate::types::{AgentRequest, AgentResponse, ProviderId};

type Slot = Arc<OnceCell<Result<AgentResponse, String>>>;

#[derive(Default)]
pub struct CallCache {
    slots: Mutex<HashMap<String, Slot>>,
}

impl std::fmt::Debug for CallCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallCache").finish_non_exhaustive()
    }
}

/// Identifies a call by everything that can change its answer.
pub fn call_key(provider: &ProviderId, req: &AgentRequest) -> String {
    let mut h = Sha256::new();
    let mut part = |label: &str, value: &str| {
        h.update(label.as_bytes());
        h.update((value.len() as u64).to_le_bytes());
        h.update(value.as_bytes());
    };
    part("provider", provider.as_str());
    part("prompt", &req.prompt);
    part("model", req.model.as_deref().unwrap_or(""));
    part("effort", req.effort.as_deref().unwrap_or(""));
    part(
        "cwd",
        &req.cwd
            .as_ref()
            .map_or(String::new(), |p| p.display().to_string()),
    );
    part("access", &format!("{:?}", req.access));
    part("isolate", &req.isolate.to_string());
    h.finalize().iter().fold(String::new(), |mut s, b| {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
        s
    })
}

impl CallCache {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Runs `call` unless an identical call is running or already finished in this consultation.
    /// Returns the reply and whether it was shared with an earlier call. A failed call is
    /// forgotten afterwards, so a later separate request is not answered by the failure.
    pub async fn get_or_run<F, Fut>(
        &self,
        key: String,
        provider: &ProviderId,
        call: F,
    ) -> (Result<AgentResponse, OrchestraError>, bool)
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<AgentResponse, OrchestraError>>,
    {
        let slot = {
            let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
            slots.entry(key.clone()).or_default().clone()
        };
        let mut own_error: Option<OrchestraError> = None;
        let mut ran = false;
        let stored = slot
            .get_or_init(|| async {
                ran = true;
                match call().await {
                    Ok(r) => Ok(r),
                    Err(e) => {
                        let text = e.to_string();
                        own_error = Some(e);
                        Err(text)
                    }
                }
            })
            .await;
        match stored {
            Ok(r) => (Ok(r.clone()), !ran),
            Err(text) => {
                if let Some(e) = own_error {
                    self.slots
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&key);
                    (Err(e), false)
                } else {
                    let first = text.lines().next().unwrap_or("").to_string();
                    (
                        Err(OrchestraError::AgentError {
                            provider: provider.clone(),
                            message: format!(
                                "an identical call in this consultation failed: {first}"
                            ),
                        }),
                        true,
                    )
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn response(text: &str) -> AgentResponse {
        AgentResponse::new(ProviderId::new("a"), text)
    }

    #[test]
    fn the_key_changes_with_anything_that_can_change_the_answer() {
        let a = ProviderId::new("a");
        let base = AgentRequest::new("hello");
        let key = call_key(&a, &base);
        assert_eq!(key, call_key(&a, &AgentRequest::new("hello")));
        assert_ne!(key, call_key(&ProviderId::new("b"), &base));
        assert_ne!(key, call_key(&a, &AgentRequest::new("hello!")));
        let mut with_model = AgentRequest::new("hello");
        with_model.model = Some("m".into());
        assert_ne!(key, call_key(&a, &with_model));
        let mut isolated = AgentRequest::new("hello");
        isolated.isolate = true;
        assert_ne!(key, call_key(&a, &isolated));
        assert_eq!(key.len(), 64);
    }

    #[tokio::test]
    async fn concurrent_identical_calls_run_once_and_share_the_reply() {
        let cache = CallCache::new();
        let runs = Arc::new(AtomicUsize::new(0));
        let a = ProviderId::new("a");
        let one = |cache: Arc<CallCache>, runs: Arc<AtomicUsize>, a: ProviderId| async move {
            cache
                .get_or_run("k".into(), &a, || async move {
                    runs.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    Ok(response("shared"))
                })
                .await
        };
        let (x, y) = tokio::join!(
            one(cache.clone(), runs.clone(), a.clone()),
            one(cache.clone(), runs.clone(), a.clone())
        );
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert_eq!(x.0.unwrap().text, "shared");
        assert_eq!(y.0.unwrap().text, "shared");
        assert_eq!(
            [x.1, y.1].iter().filter(|r| **r).count(),
            1,
            "exactly one was shared"
        );
        // A third, later identical call is answered from the stored reply.
        let (z, shared) = cache
            .get_or_run("k".into(), &a, || async { Ok(response("never")) })
            .await;
        assert!(shared);
        assert_eq!(z.unwrap().text, "shared");
    }

    #[tokio::test]
    async fn different_calls_are_independent() {
        let cache = CallCache::new();
        let a = ProviderId::new("a");
        let (x, sx) = cache
            .get_or_run("1".into(), &a, || async { Ok(response("one")) })
            .await;
        let (y, sy) = cache
            .get_or_run("2".into(), &a, || async { Ok(response("two")) })
            .await;
        assert_eq!(
            (x.unwrap().text.as_str(), sx, y.unwrap().text.as_str(), sy),
            ("one", false, "two", false)
        );
    }

    #[tokio::test]
    async fn a_failure_is_reported_to_its_own_caller_and_is_not_kept_for_later() {
        let cache = CallCache::new();
        let a = ProviderId::new("a");
        let (r, shared) = cache
            .get_or_run("k".into(), &a, || async {
                Err(OrchestraError::Quota {
                    provider: ProviderId::new("a"),
                    detail: "limit".into(),
                })
            })
            .await;
        assert!(!shared);
        assert!(
            matches!(r, Err(OrchestraError::Quota { .. })),
            "its own typed error"
        );
        // A later, separate call runs for itself (it is a new request, not a retry on its behalf).
        let (r, shared) = cache
            .get_or_run("k".into(), &a, || async { Ok(response("fresh")) })
            .await;
        assert!(!shared);
        assert_eq!(r.unwrap().text, "fresh");
    }

    #[tokio::test]
    async fn a_caller_that_waited_on_a_failing_call_gets_the_failure_not_a_retry() {
        let cache = CallCache::new();
        let runs = Arc::new(AtomicUsize::new(0));
        let a = ProviderId::new("a");
        let call = |cache: Arc<CallCache>, runs: Arc<AtomicUsize>, a: ProviderId| async move {
            cache
                .get_or_run("k".into(), &a, || async move {
                    runs.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    Err(OrchestraError::AgentError {
                        provider: ProviderId::new("a"),
                        message: "broken".into(),
                    })
                })
                .await
        };
        let (x, y) = tokio::join!(
            call(cache.clone(), runs.clone(), a.clone()),
            call(cache.clone(), runs.clone(), a.clone())
        );
        assert_eq!(runs.load(Ordering::SeqCst), 1, "no second attempt was made");
        assert!(x.0.is_err() && y.0.is_err());
        let texts = [x.0.unwrap_err().to_string(), y.0.unwrap_err().to_string()];
        assert!(
            texts
                .iter()
                .any(|t| t.contains("an identical call in this consultation failed: ")),
            "{texts:?}"
        );
    }
}
