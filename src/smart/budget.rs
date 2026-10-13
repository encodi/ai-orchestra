//! The resource budget of one consultation, enforced while it runs.
//!
//! One [`ResourceBudgetManager`] is shared by every agent call of a consultation (the Milestone 2
//! debate engine included). A call must [`acquire`](ResourceBudgetManager::acquire) a permit before
//! it starts: the permit is refused once a limit is reached, so the consultation stops launching
//! new work, keeps what already finished, and reports the stop. There is a single clock: the
//! deadline is the consultation's own wall-clock limit, and per-call timeouts are clipped to what
//! is left of it instead of running beside it as a second timeout system.
//!
//! The manager also bounds how many of this consultation's calls run at once, so one large
//! consultation cannot occupy every process slot while a small one waits (the process manager's
//! queue is first-in first-out, so a consultation that only ever has `max_parallel` calls queued
//! interleaves fairly with the others).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// The limits of one consultation. `None` means "no limit of this kind".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetLimits {
    /// Most agent calls that may be started, research planning and verification included.
    pub max_agent_calls: Option<usize>,
    /// Wall-clock limit for the whole consultation, in seconds.
    pub max_duration_secs: Option<u64>,
    /// Most of this consultation's calls running at the same time. `None`: bounded only by the
    /// process manager's global limit.
    pub max_parallel: Option<usize>,
    /// Per-call timeout, in seconds, before it is clipped to the time left.
    pub call_timeout_secs: Option<u64>,
    /// Most (estimated) tokens of recalled memory placed before a question.
    #[serde(default)]
    pub context_tokens: Option<usize>,
    /// Most characters of agent output accepted in total.
    pub max_response_chars: Option<u64>,
}

impl BudgetLimits {
    /// The limits that were set, in words. The per-call timeout and the output cap are ceilings
    /// that always apply, so they are listed separately.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if let Some(n) = self.max_agent_calls {
            parts.push(format!("{n} agent call(s)"));
        }
        if let Some(s) = self.max_duration_secs {
            parts.push(format!("{s} s in total"));
        }
        if let Some(n) = self.max_parallel {
            parts.push(format!("{n} at a time"));
        }
        let mut ceilings = Vec::new();
        if let Some(s) = self.call_timeout_secs {
            ceilings.push(format!("{s} s per call"));
        }
        if let Some(c) = self.max_response_chars {
            ceilings.push(format!("{c} characters of output"));
        }
        if let Some(t) = self.context_tokens {
            parts.push(format!("~{t} tokens of recalled context"));
        }
        let set = if parts.is_empty() {
            "no limit set".to_string()
        } else {
            parts.join(", ")
        };
        if ceilings.is_empty() {
            set
        } else {
            format!("{set} (always at most {})", ceilings.join(" and "))
        }
    }
}

/// Why new work stopped being launched. The first limit reached is the one reported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reached", rename_all = "snake_case")]
pub enum BudgetStop {
    Calls { limit: usize },
    Duration { limit_secs: u64 },
    ResponseSize { limit_chars: u64 },
    Cancelled,
}

impl std::fmt::Display for BudgetStop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BudgetStop::Calls { limit } => {
                write!(f, "the limit of {limit} agent call(s) was reached")
            }
            BudgetStop::Duration { limit_secs } => {
                write!(f, "the time limit of {limit_secs} s was reached")
            }
            BudgetStop::ResponseSize { limit_chars } => {
                write!(
                    f,
                    "the limit of {limit_chars} characters of agent output was reached"
                )
            }
            BudgetStop::Cancelled => f.write_str("the consultation was cancelled"),
        }
    }
}

/// What was used, as measured by the manager (not estimated).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetUsage {
    pub calls_started: usize,
    pub calls_reused: usize,
    pub peak_parallel: usize,
    /// Time calls spent waiting for one of this consultation's parallel slots.
    pub queue_ms: u64,
    /// Characters sent to agents in prompts.
    pub prompt_chars: u64,
    /// Characters of agent output received.
    pub response_chars: u64,
    /// Characters of recalled memory placed before the question, and its estimated tokens.
    #[serde(default)]
    pub context_chars: u64,
    #[serde(default)]
    pub context_tokens_estimated: u64,
    pub elapsed_ms: u64,
    pub stopped_by: Option<BudgetStop>,
}

#[derive(Default)]
struct State {
    calls_started: usize,
    calls_reused: usize,
    running: usize,
    peak: usize,
    queue_ms: u64,
    prompt_chars: u64,
    response_chars: u64,
    context_chars: u64,
    context_tokens: u64,
    stopped: Option<BudgetStop>,
}

pub struct ResourceBudgetManager {
    limits: BudgetLimits,
    started: Instant,
    deadline: Option<Instant>,
    parallel: Arc<Semaphore>,
    state: Mutex<State>,
}

impl std::fmt::Debug for ResourceBudgetManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResourceBudgetManager")
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl ResourceBudgetManager {
    pub fn new(limits: BudgetLimits) -> Arc<Self> {
        let started = Instant::now();
        let deadline = limits
            .max_duration_secs
            .map(|s| started + Duration::from_secs(s));
        let slots = limits.max_parallel.map_or(Semaphore::MAX_PERMITS, |n| {
            n.clamp(1, Semaphore::MAX_PERMITS)
        });
        Arc::new(Self {
            limits,
            started,
            deadline,
            parallel: Arc::new(Semaphore::new(slots)),
            state: Mutex::new(State::default()),
        })
    }

    pub fn limits(&self) -> &BudgetLimits {
        &self.limits
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Time left before the deadline, if there is one.
    pub fn remaining_time(&self) -> Option<Duration> {
        self.deadline
            .map(|d| d.saturating_duration_since(Instant::now()))
    }

    /// Calls that can still be started, if there is a limit.
    pub fn remaining_calls(&self) -> Option<usize> {
        self.limits
            .max_agent_calls
            .map(|m| m.saturating_sub(self.state().calls_started))
    }

    /// Whether `calls` more calls could still be started (and there is time to start them).
    pub fn can_afford(&self, calls: usize) -> bool {
        self.stopped().is_none()
            && self.remaining_calls().is_none_or(|left| left >= calls)
            && self.remaining_time().is_none_or(|t| !t.is_zero())
    }

    /// The limit that stopped new work, if one did.
    pub fn stopped(&self) -> Option<BudgetStop> {
        let mut st = self.state();
        if st.stopped.is_none() && self.deadline.is_some_and(|d| Instant::now() >= d) {
            st.stopped = Some(BudgetStop::Duration {
                limit_secs: self.limits.max_duration_secs.unwrap_or(0),
            });
        }
        st.stopped.clone()
    }

    /// Completes when the deadline passes; never completes when there is none.
    pub async fn deadline_passed(&self) {
        match self.deadline {
            Some(d) => tokio::time::sleep_until(d).await,
            None => std::future::pending().await,
        }
    }

    /// A call's own timeout, clipped to the time left before the deadline. `None` when there is no
    /// deadline: the call keeps whatever timeout it would have had.
    pub fn clip_timeout(&self, own: Duration) -> Option<Duration> {
        self.remaining_time()
            .map(|left| own.min(left.max(Duration::from_secs(1))))
    }

    fn refuse(&self, stop: BudgetStop) -> BudgetStop {
        let mut st = self.state();
        st.stopped.get_or_insert(stop.clone());
        st.stopped.clone().unwrap_or(stop)
    }

    /// Waits for a parallel slot and reserves one agent call. Refused (never started) when a limit
    /// has been reached or the consultation is cancelled while it waits.
    pub async fn acquire(
        self: &Arc<Self>,
        prompt_chars: usize,
        cancel: &CancellationToken,
    ) -> Result<CallPermit, BudgetStop> {
        if let Some(stop) = self.stopped() {
            return Err(stop);
        }
        let waited = Instant::now();
        let permit = tokio::select! {
            p = self.parallel.clone().acquire_owned() => match p {
                Ok(p) => p,
                Err(_) => return Err(self.refuse(BudgetStop::Cancelled)),
            },
            _ = cancel.cancelled() => return Err(self.refuse(BudgetStop::Cancelled)),
            _ = self.deadline_passed() => {
                return Err(self.refuse(BudgetStop::Duration {
                    limit_secs: self.limits.max_duration_secs.unwrap_or(0),
                }));
            }
        };
        let queued = u64::try_from(waited.elapsed().as_millis()).unwrap_or(u64::MAX);
        let mut st = self.state();
        if let Some(stop) = st.stopped.clone() {
            return Err(stop);
        }
        if let Some(limit) = self.limits.max_agent_calls
            && st.calls_started >= limit
        {
            let stop = BudgetStop::Calls { limit };
            st.stopped = Some(stop.clone());
            return Err(stop);
        }
        st.calls_started += 1;
        st.running += 1;
        st.peak = st.peak.max(st.running);
        st.queue_ms = st.queue_ms.saturating_add(queued);
        st.prompt_chars = st.prompt_chars.saturating_add(prompt_chars as u64);
        drop(st);
        Ok(CallPermit {
            manager: self.clone(),
            _slot: permit,
            finished: false,
        })
    }

    /// Records the recalled memory placed before the question (estimated tokens, never exact).
    pub fn note_context(&self, chars: u64, estimated_tokens: u64) {
        let mut st = self.state();
        st.context_chars = chars;
        st.context_tokens = estimated_tokens;
    }

    /// Records that a call was answered from an identical call of this consultation.
    pub fn note_reused(&self) {
        self.state().calls_reused += 1;
    }

    pub fn usage(&self) -> BudgetUsage {
        let stopped_by = self.stopped();
        let st = self.state();
        BudgetUsage {
            calls_started: st.calls_started,
            calls_reused: st.calls_reused,
            peak_parallel: st.peak,
            queue_ms: st.queue_ms,
            prompt_chars: st.prompt_chars,
            response_chars: st.response_chars,
            context_chars: st.context_chars,
            context_tokens_estimated: st.context_tokens,
            elapsed_ms: u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX),
            stopped_by,
        }
    }
}

/// A reserved call. Dropping it without [`finish`](CallPermit::finish) releases the slot.
pub struct CallPermit {
    manager: Arc<ResourceBudgetManager>,
    _slot: OwnedSemaphorePermit,
    finished: bool,
}

impl CallPermit {
    /// The call ended; `response_chars` of output came back (0 for a failure).
    pub fn finish(mut self, response_chars: usize) {
        self.finished = true;
        let mut st = self.manager.state();
        st.running = st.running.saturating_sub(1);
        st.response_chars = st.response_chars.saturating_add(response_chars as u64);
        if let Some(limit) = self.manager.limits.max_response_chars
            && st.response_chars >= limit
        {
            st.stopped
                .get_or_insert(BudgetStop::ResponseSize { limit_chars: limit });
        }
    }
}

impl Drop for CallPermit {
    fn drop(&mut self) {
        if !self.finished {
            let mut st = self.manager.state();
            st.running = st.running.saturating_sub(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(calls: Option<usize>, parallel: Option<usize>) -> BudgetLimits {
        BudgetLimits {
            max_agent_calls: calls,
            max_parallel: parallel,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn the_call_limit_refuses_the_call_after_the_last_allowed_one_and_stays_refused() {
        let b = ResourceBudgetManager::new(limits(Some(2), None));
        let cancel = CancellationToken::new();
        b.acquire(10, &cancel).await.unwrap().finish(5);
        b.acquire(10, &cancel).await.unwrap().finish(5);
        assert_eq!(b.remaining_calls(), Some(0));
        assert!(!b.can_afford(1));
        assert_eq!(
            b.acquire(10, &cancel).await.err(),
            Some(BudgetStop::Calls { limit: 2 })
        );
        // Sticky: the same stop is reported afterwards, and the refused call was not counted.
        assert_eq!(b.stopped(), Some(BudgetStop::Calls { limit: 2 }));
        let u = b.usage();
        assert_eq!(
            (u.calls_started, u.prompt_chars, u.response_chars),
            (2, 20, 10)
        );
    }

    #[tokio::test]
    async fn parallel_slots_bound_concurrency_and_queued_calls_start_when_one_frees() {
        let b = ResourceBudgetManager::new(limits(None, Some(2)));
        let cancel = CancellationToken::new();
        let first = b.acquire(1, &cancel).await.unwrap();
        let second = b.acquire(1, &cancel).await.unwrap();
        let waiting = {
            let (b, cancel) = (b.clone(), cancel.clone());
            tokio::spawn(async move { b.acquire(1, &cancel).await.map(|p| p.finish(0)) })
        };
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished(), "a third call waits for a slot");
        first.finish(0);
        waiting.await.unwrap().unwrap();
        second.finish(0);
        let u = b.usage();
        assert_eq!(u.peak_parallel, 2);
        assert_eq!(u.calls_started, 3);
    }

    #[tokio::test]
    async fn cancelling_while_queued_refuses_the_call() {
        let b = ResourceBudgetManager::new(limits(None, Some(1)));
        let cancel = CancellationToken::new();
        let _held = b.acquire(1, &cancel).await.unwrap();
        let queued = {
            let (b, cancel) = (b.clone(), cancel.clone());
            tokio::spawn(async move { b.acquire(1, &cancel).await.err() })
        };
        tokio::task::yield_now().await;
        cancel.cancel();
        assert_eq!(queued.await.unwrap(), Some(BudgetStop::Cancelled));
    }

    #[tokio::test(start_paused = true)]
    async fn the_deadline_stops_new_work_and_clips_call_timeouts_to_the_time_left() {
        let b = ResourceBudgetManager::new(BudgetLimits {
            max_duration_secs: Some(60),
            ..Default::default()
        });
        let cancel = CancellationToken::new();
        assert_eq!(
            b.clip_timeout(Duration::from_secs(600)),
            Some(Duration::from_secs(60)),
            "a call cannot outlive the consultation"
        );
        assert_eq!(
            b.clip_timeout(Duration::from_secs(10)),
            Some(Duration::from_secs(10))
        );
        let unbounded = ResourceBudgetManager::new(BudgetLimits::default());
        assert_eq!(
            unbounded.clip_timeout(Duration::from_secs(10)),
            None,
            "no deadline, no clipping"
        );
        b.acquire(1, &cancel).await.unwrap().finish(0);
        tokio::time::advance(Duration::from_secs(61)).await;
        assert_eq!(
            b.acquire(1, &cancel).await.err(),
            Some(BudgetStop::Duration { limit_secs: 60 })
        );
        assert!(!b.can_afford(1));
        b.deadline_passed().await; // already past: completes at once
    }

    #[tokio::test]
    async fn the_response_size_limit_stops_launching_after_the_output_that_crossed_it() {
        let b = ResourceBudgetManager::new(BudgetLimits {
            max_response_chars: Some(100),
            ..Default::default()
        });
        let cancel = CancellationToken::new();
        b.acquire(1, &cancel).await.unwrap().finish(120);
        assert_eq!(
            b.acquire(1, &cancel).await.err(),
            Some(BudgetStop::ResponseSize { limit_chars: 100 })
        );
    }

    #[tokio::test]
    async fn without_limits_nothing_is_refused_and_a_dropped_permit_frees_its_slot() {
        let b = ResourceBudgetManager::new(BudgetLimits::default());
        let cancel = CancellationToken::new();
        for _ in 0..50 {
            drop(b.acquire(1, &cancel).await.unwrap());
        }
        assert!(b.can_afford(1_000));
        assert_eq!(b.remaining_calls(), None);
        assert_eq!(
            b.usage().peak_parallel,
            1,
            "dropped permits do not stay running"
        );
        b.note_reused();
        assert_eq!(b.usage().calls_reused, 1);
        assert_eq!(BudgetLimits::default().describe(), "no limit set");
        let described = BudgetLimits {
            max_agent_calls: Some(4),
            max_parallel: Some(2),
            call_timeout_secs: Some(300),
            ..Default::default()
        }
        .describe();
        assert_eq!(
            described,
            "4 agent call(s), 2 at a time (always at most 300 s per call)"
        );
    }
}
