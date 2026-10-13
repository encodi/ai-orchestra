//! Bounded asynchronous task execution for the MCP server.
//!
//! MCP clients impose request timeouts, so a long agent call or debate runs as
//! a *task*: it gets an id, can be waited on, inspected and cancelled, and is
//! cleaned up on shutdown. Nothing here spawns an unaccounted-for background
//! job: every task is owned by the [`TaskManager`], runs under a concurrency
//! limit and a time limit, and is awaited when the server stops.
//!
//! States: `pending` (queued for a slot), `running`, `completed`, `failed`,
//! `cancelled` and `timed_out`. The last four, plus `completed`, are terminal.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tokio::sync::{Semaphore, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::history::now_ms;

/// How long a cancelled or timed-out task gets to terminate its subprocesses
/// before the manager stops waiting for it.
const CLEANUP_GRACE: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
    TimedOut,
}

impl TaskState {
    pub fn is_terminal(self) -> bool {
        !matches!(self, TaskState::Pending | TaskState::Running)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ErrorInfo {
    /// `quota`, `auth`, `timeout`, `cancelled`, `malformed`, `missing`, `nested`,
    /// `invalid_request`, `internal`, ...
    pub kind: String,
    pub message: String,
}

/// How a unit of work failed. `state` must be `Failed`, `Cancelled` or `TimedOut`.
#[derive(Debug, Clone)]
pub struct TaskFailure {
    pub state: TaskState,
    pub error: ErrorInfo,
    /// Whatever the work produced before failing, if anything.
    pub partial: Option<Value>,
}

impl TaskFailure {
    pub fn failed(kind: &str, message: impl Into<String>) -> Self {
        Self {
            state: TaskState::Failed,
            error: ErrorInfo {
                kind: kind.to_string(),
                message: message.into(),
            },
            partial: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskInfo {
    pub id: String,
    /// `ask_agent` or `debate`.
    pub kind: String,
    /// Short human-readable description, e.g. the provider name.
    pub label: String,
    pub state: TaskState,
    pub created_at_ms: u64,
    pub started_at_ms: Option<u64>,
    pub finished_at_ms: Option<u64>,
    /// The tool result: on success, or the partial result of a failure.
    pub result: Option<Value>,
    pub error: Option<ErrorInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskLimits {
    pub max_concurrent: usize,
    pub max_queued: usize,
    /// Finished tasks to remember.
    pub retention: usize,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum SubmitError {
    #[error("too many tasks are waiting ({0}); wait for some to finish or cancel one")]
    QueueFull(usize),
    #[error("the server is shutting down")]
    ShuttingDown,
}

struct Entry {
    info: TaskInfo,
    cancel: CancellationToken,
    state_tx: watch::Sender<TaskState>,
    handle: Option<JoinHandle<()>>,
}

#[derive(Default)]
struct Registry {
    tasks: HashMap<String, Entry>,
    /// Task ids, oldest first.
    order: VecDeque<String>,
}

struct Inner {
    registry: Mutex<Registry>,
    slots: Arc<Semaphore>,
    limits: TaskLimits,
    shutdown: CancellationToken,
    counter: AtomicU64,
}

#[derive(Clone)]
pub struct TaskManager {
    inner: Arc<Inner>,
}

impl TaskManager {
    pub fn new(limits: TaskLimits) -> Self {
        Self {
            inner: Arc::new(Inner {
                registry: Mutex::new(Registry::default()),
                slots: Arc::new(Semaphore::new(limits.max_concurrent.max(1))),
                limits,
                shutdown: CancellationToken::new(),
                counter: AtomicU64::new(0),
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Registry> {
        self.inner
            .registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    fn new_id(&self) -> String {
        use std::collections::hash_map::RandomState;
        use std::hash::{BuildHasher, Hasher};
        let n = self.inner.counter.fetch_add(1, Ordering::Relaxed) + 1;
        let mut h = RandomState::new().build_hasher();
        h.write_u64(n);
        h.write_u32(std::process::id());
        format!("task-{n:04x}{:04x}", h.finish() & 0xffff)
    }

    /// Queues `work`. It starts when a slot is free, receives a token that is
    /// cancelled on `cancel`, on timeout and on shutdown, and must stop its
    /// subprocesses when that happens.
    pub fn submit<F, Fut>(
        &self,
        kind: &str,
        label: &str,
        timeout: Duration,
        work: F,
    ) -> Result<String, SubmitError>
    where
        F: FnOnce(CancellationToken) -> Fut + Send + 'static,
        Fut: Future<Output = Result<Value, TaskFailure>> + Send + 'static,
    {
        if self.inner.shutdown.is_cancelled() {
            return Err(SubmitError::ShuttingDown);
        }
        let id = self.new_id();
        let cancel = self.inner.shutdown.child_token();
        let (state_tx, _) = watch::channel(TaskState::Pending);
        {
            let mut reg = self.lock();
            let pending = reg
                .tasks
                .values()
                .filter(|e| e.info.state == TaskState::Pending)
                .count();
            let running = reg
                .tasks
                .values()
                .filter(|e| e.info.state == TaskState::Running)
                .count();
            // Only tasks that would have to wait count against the queue.
            let would_wait =
                (pending + running + 1).saturating_sub(self.inner.limits.max_concurrent);
            if would_wait > self.inner.limits.max_queued {
                return Err(SubmitError::QueueFull(self.inner.limits.max_queued));
            }
            self.evict_finished(&mut reg);
            reg.order.push_back(id.clone());
            reg.tasks.insert(
                id.clone(),
                Entry {
                    info: TaskInfo {
                        id: id.clone(),
                        kind: kind.to_string(),
                        label: label.to_string(),
                        state: TaskState::Pending,
                        created_at_ms: now_ms(),
                        started_at_ms: None,
                        finished_at_ms: None,
                        result: None,
                        error: None,
                    },
                    cancel: cancel.clone(),
                    state_tx,
                    handle: None,
                },
            );
        }

        let manager = self.clone();
        let task_id = id.clone();
        let handle = tokio::spawn(async move {
            manager.run_task(task_id, cancel, timeout, work).await;
        });
        if let Some(entry) = self.lock().tasks.get_mut(&id) {
            entry.handle = Some(handle);
        }
        Ok(id)
    }

    async fn run_task<F, Fut>(
        &self,
        id: String,
        cancel: CancellationToken,
        timeout: Duration,
        work: F,
    ) where
        F: FnOnce(CancellationToken) -> Fut + Send + 'static,
        Fut: Future<Output = Result<Value, TaskFailure>> + Send + 'static,
    {
        let permit = tokio::select! {
            permit = self.inner.slots.clone().acquire_owned() => permit.ok(),
            _ = cancel.cancelled() => None,
        };
        let Some(_permit) = permit else {
            self.finish(
                &id,
                TaskState::Cancelled,
                None,
                Some(ErrorInfo {
                    kind: "cancelled".into(),
                    message: "cancelled before it started".into(),
                }),
            );
            return;
        };
        if cancel.is_cancelled() {
            self.finish(
                &id,
                TaskState::Cancelled,
                None,
                Some(ErrorInfo {
                    kind: "cancelled".into(),
                    message: "cancelled before it started".into(),
                }),
            );
            return;
        }
        self.set_running(&id);

        let fut = work(cancel.clone());
        tokio::pin!(fut);
        let mut timed_out = false;
        let result = tokio::select! {
            r = &mut fut => Some(r),
            _ = tokio::time::sleep(timeout) => { timed_out = true; None }
            _ = cancel.cancelled() => None,
        };
        let result = match result {
            Some(r) => r,
            None => {
                // Stop the work and wait (bounded) for it to terminate its subprocesses.
                cancel.cancel();
                match tokio::time::timeout(CLEANUP_GRACE, &mut fut).await {
                    Ok(r) => r,
                    Err(_) => Err(TaskFailure::failed(
                        "internal",
                        "the task did not stop within the cleanup grace period",
                    )),
                }
            }
        };
        match result {
            Ok(value) if !timed_out => self.finish(&id, TaskState::Completed, Some(value), None),
            Ok(value) => self.finish(
                &id,
                TaskState::TimedOut,
                Some(value),
                Some(ErrorInfo {
                    kind: "timeout".into(),
                    message: format!("exceeded the {}s task limit", timeout.as_secs()),
                }),
            ),
            Err(f) => {
                let state = if timed_out {
                    TaskState::TimedOut
                } else if cancel.is_cancelled() && f.state == TaskState::Failed {
                    TaskState::Cancelled
                } else {
                    f.state
                };
                let error = if timed_out {
                    ErrorInfo {
                        kind: "timeout".into(),
                        message: format!("exceeded the {}s task limit", timeout.as_secs()),
                    }
                } else {
                    f.error
                };
                self.finish(&id, state, f.partial, Some(error));
            }
        }
    }

    fn set_running(&self, id: &str) {
        let mut reg = self.lock();
        if let Some(e) = reg.tasks.get_mut(id) {
            e.info.state = TaskState::Running;
            e.info.started_at_ms = Some(now_ms());
            e.state_tx.send_replace(TaskState::Running);
        }
    }

    fn finish(&self, id: &str, state: TaskState, result: Option<Value>, error: Option<ErrorInfo>) {
        let mut reg = self.lock();
        if let Some(e) = reg.tasks.get_mut(id) {
            e.info.state = state;
            e.info.finished_at_ms = Some(now_ms());
            e.info.result = result;
            e.info.error = error;
            e.state_tx.send_replace(state);
        }
    }

    /// Drops the oldest finished tasks beyond the retention limit. Pending and
    /// running tasks are never dropped.
    fn evict_finished(&self, reg: &mut Registry) {
        let finished: Vec<String> = reg
            .order
            .iter()
            .filter(|id| {
                reg.tasks
                    .get(*id)
                    .is_some_and(|e| e.info.state.is_terminal())
            })
            .cloned()
            .collect();
        let excess = finished
            .len()
            .saturating_sub(self.inner.limits.retention.saturating_sub(1));
        for id in finished.into_iter().take(excess) {
            reg.tasks.remove(&id);
            reg.order.retain(|o| *o != id);
        }
    }

    pub fn status(&self, id: &str) -> Option<TaskInfo> {
        self.lock().tasks.get(id).map(|e| e.info.clone())
    }

    /// Waits until the task is finished or `max` has passed, whichever comes
    /// first, without polling: it sleeps on the task's state channel.
    pub async fn wait(&self, id: &str, max: Duration) -> Option<TaskInfo> {
        let mut rx = self.lock().tasks.get(id).map(|e| e.state_tx.subscribe())?;
        let deadline = tokio::time::Instant::now() + max;
        loop {
            if rx.borrow().is_terminal() {
                break;
            }
            match tokio::time::timeout_at(deadline, rx.changed()).await {
                Ok(Ok(())) => {}
                _ => break,
            }
        }
        self.status(id)
    }

    /// Requests cancellation. A finished task is left as it is. Returns the
    /// task as it is right now; use [`TaskManager::wait`] to see it settle.
    pub fn cancel(&self, id: &str) -> Option<TaskInfo> {
        let reg = self.lock();
        let entry = reg.tasks.get(id)?;
        if !entry.info.state.is_terminal() {
            entry.cancel.cancel();
        }
        Some(entry.info.clone())
    }

    pub fn list(&self) -> Vec<TaskInfo> {
        let reg = self.lock();
        reg.order
            .iter()
            .filter_map(|id| reg.tasks.get(id).map(|e| e.info.clone()))
            .collect()
    }

    /// Cancels everything and waits for the tasks to terminate their subprocesses.
    pub async fn shutdown(&self) {
        self.inner.shutdown.cancel();
        let handles: Vec<JoinHandle<()>> = {
            let mut reg = self.lock();
            reg.tasks
                .values_mut()
                .filter_map(|e| e.handle.take())
                .collect()
        };
        for handle in handles {
            let _ = tokio::time::timeout(CLEANUP_GRACE * 2, handle).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::sync::{Notify, mpsc};

    fn limits(max_concurrent: usize, max_queued: usize, retention: usize) -> TaskLimits {
        TaskLimits {
            max_concurrent,
            max_queued,
            retention,
        }
    }

    const LONG: Duration = Duration::from_secs(60);

    /// Work that reports when it starts, then waits for `release` (or cancellation).
    fn gated(
        started: mpsc::UnboundedSender<()>,
        release: Arc<Notify>,
        value: Value,
    ) -> impl FnOnce(
        CancellationToken,
    )
        -> std::pin::Pin<Box<dyn Future<Output = Result<Value, TaskFailure>> + Send>> {
        move |ct| {
            Box::pin(async move {
                let _ = started.send(());
                tokio::select! {
                    _ = release.notified() => Ok(value),
                    _ = ct.cancelled() => Err(TaskFailure::failed("cancelled", "stopped")),
                }
            })
        }
    }

    #[tokio::test]
    async fn a_task_moves_through_pending_running_completed() {
        let m = TaskManager::new(limits(1, 4, 10));
        let (tx, mut started) = mpsc::unbounded_channel();
        let release = Arc::new(Notify::new());
        let first = m
            .submit(
                "ask_agent",
                "a",
                LONG,
                gated(tx.clone(), release.clone(), json!("one")),
            )
            .unwrap();
        started.recv().await.unwrap();
        // The only slot is taken, so a second task must wait.
        let second = m
            .submit(
                "ask_agent",
                "b",
                LONG,
                gated(tx, release.clone(), json!("two")),
            )
            .unwrap();
        assert_eq!(m.status(&first).unwrap().state, TaskState::Running);
        assert_eq!(m.status(&second).unwrap().state, TaskState::Pending);
        assert!(m.status(&first).unwrap().started_at_ms.is_some());
        assert!(m.status(&second).unwrap().started_at_ms.is_none());

        release.notify_one();
        let done = m.wait(&first, LONG).await.unwrap();
        assert_eq!(done.state, TaskState::Completed);
        assert_eq!(done.result, Some(json!("one")));
        assert!(done.finished_at_ms.unwrap() >= done.started_at_ms.unwrap());
        // The slot is released, so the second task now runs.
        started.recv().await.unwrap();
        assert_eq!(m.status(&second).unwrap().state, TaskState::Running);
        release.notify_one();
        assert_eq!(
            m.wait(&second, LONG).await.unwrap().state,
            TaskState::Completed
        );
    }

    #[tokio::test]
    async fn concurrency_and_queue_are_bounded() {
        let m = TaskManager::new(limits(2, 1, 50));
        let (tx, mut started) = mpsc::unbounded_channel();
        let release = Arc::new(Notify::new());
        let mut ids = Vec::new();
        for i in 0..3 {
            ids.push(
                m.submit(
                    "t",
                    &i.to_string(),
                    LONG,
                    gated(tx.clone(), release.clone(), json!(i)),
                )
                .unwrap(),
            );
        }
        started.recv().await.unwrap();
        started.recv().await.unwrap();
        let states: Vec<TaskState> = ids.iter().map(|i| m.status(i).unwrap().state).collect();
        assert_eq!(
            states.iter().filter(|s| **s == TaskState::Running).count(),
            2,
            "{states:?}"
        );
        assert_eq!(
            states.iter().filter(|s| **s == TaskState::Pending).count(),
            1
        );
        // Two running + one waiting is the limit: the next submission is refused.
        let err = m
            .submit("t", "x", LONG, gated(tx.clone(), release.clone(), json!(9)))
            .unwrap_err();
        assert_eq!(err, SubmitError::QueueFull(1));
        m.shutdown().await;
    }

    #[tokio::test]
    async fn failures_timeouts_and_cancellation_are_told_apart() {
        let m = TaskManager::new(limits(4, 4, 50));
        // Failed: the work returns an error.
        let failed = m
            .submit("t", "f", LONG, |_| async {
                Err(TaskFailure::failed("quota", "limit"))
            })
            .unwrap();
        let info = m.wait(&failed, LONG).await.unwrap();
        assert_eq!(info.state, TaskState::Failed);
        assert_eq!(info.error.unwrap().kind, "quota");

        // Timed out: the work only stops when it is told to.
        let slow = m
            .submit("t", "s", Duration::from_millis(50), |ct| async move {
                ct.cancelled().await;
                Err(TaskFailure::failed("cancelled", "stopped"))
            })
            .unwrap();
        let info = m.wait(&slow, LONG).await.unwrap();
        assert_eq!(info.state, TaskState::TimedOut);
        assert_eq!(info.error.unwrap().kind, "timeout");

        // Cancelled while running.
        let (tx, mut started) = mpsc::unbounded_channel();
        let running = m
            .submit("t", "r", LONG, gated(tx, Arc::new(Notify::new()), json!(0)))
            .unwrap();
        started.recv().await.unwrap();
        assert_eq!(
            m.cancel(&running).unwrap().state,
            TaskState::Running,
            "cancel is a request"
        );
        let info = m.wait(&running, LONG).await.unwrap();
        assert_eq!(info.state, TaskState::Cancelled);
        // Cancelling a finished task changes nothing.
        assert_eq!(m.cancel(&running).unwrap().state, TaskState::Cancelled);
        assert_eq!(m.cancel(&failed).unwrap().state, TaskState::Failed);
        assert!(m.cancel("task-nope").is_none());
    }

    #[tokio::test]
    async fn a_queued_task_can_be_cancelled_without_ever_running() {
        let m = TaskManager::new(limits(1, 4, 50));
        let (tx, mut started) = mpsc::unbounded_channel();
        let release = Arc::new(Notify::new());
        let first = m
            .submit("t", "1", LONG, gated(tx.clone(), release.clone(), json!(1)))
            .unwrap();
        started.recv().await.unwrap();
        let queued = m
            .submit("t", "2", LONG, gated(tx, release.clone(), json!(2)))
            .unwrap();
        m.cancel(&queued);
        let info = m.wait(&queued, LONG).await.unwrap();
        assert_eq!(info.state, TaskState::Cancelled);
        assert!(info.started_at_ms.is_none(), "it never started");
        release.notify_one();
        assert_eq!(
            m.wait(&first, LONG).await.unwrap().state,
            TaskState::Completed
        );
        assert!(started.try_recv().is_err(), "the cancelled task never ran");
    }

    #[tokio::test]
    async fn waiting_does_not_busy_poll_and_respects_its_deadline() {
        let m = TaskManager::new(limits(1, 4, 50));
        let (tx, mut started) = mpsc::unbounded_channel();
        let id = m
            .submit("t", "w", LONG, gated(tx, Arc::new(Notify::new()), json!(0)))
            .unwrap();
        started.recv().await.unwrap();
        let t = tokio::time::Instant::now();
        let info = m.wait(&id, Duration::from_millis(80)).await.unwrap();
        assert_eq!(
            info.state,
            TaskState::Running,
            "returned at the deadline, not finished"
        );
        assert!(t.elapsed() >= Duration::from_millis(80));
        assert!(
            m.wait("task-nope", Duration::from_millis(10))
                .await
                .is_none()
        );
        m.shutdown().await;
    }

    #[tokio::test]
    async fn only_finished_tasks_are_evicted_by_retention() {
        let m = TaskManager::new(limits(4, 4, 3));
        let (tx, mut started) = mpsc::unbounded_channel();
        let running = m
            .submit(
                "t",
                "keep",
                LONG,
                gated(tx, Arc::new(Notify::new()), json!(0)),
            )
            .unwrap();
        started.recv().await.unwrap();
        let mut done = Vec::new();
        for i in 0..5 {
            let id = m
                .submit(
                    "t",
                    &i.to_string(),
                    LONG,
                    move |_| async move { Ok(json!(i)) },
                )
                .unwrap();
            m.wait(&id, LONG).await.unwrap();
            done.push(id);
        }
        assert!(
            m.status(&running).is_some(),
            "a running task is never evicted"
        );
        assert!(
            m.status(&done[0]).is_none(),
            "the oldest finished task was dropped"
        );
        assert!(m.status(done.last().unwrap()).is_some());
        assert!(m.list().len() <= 3 + 1, "{}", m.list().len());
        m.shutdown().await;
    }

    #[tokio::test]
    async fn shutdown_cancels_everything_and_refuses_new_work() {
        let m = TaskManager::new(limits(1, 4, 50));
        let (tx, mut started) = mpsc::unbounded_channel();
        let a = m
            .submit(
                "t",
                "a",
                LONG,
                gated(tx.clone(), Arc::new(Notify::new()), json!(0)),
            )
            .unwrap();
        started.recv().await.unwrap();
        let b = m
            .submit("t", "b", LONG, gated(tx, Arc::new(Notify::new()), json!(0)))
            .unwrap();
        m.shutdown().await;
        assert_eq!(m.status(&a).unwrap().state, TaskState::Cancelled);
        assert_eq!(m.status(&b).unwrap().state, TaskState::Cancelled);
        let err = m
            .submit("t", "c", LONG, |_| async { Ok(json!(0)) })
            .unwrap_err();
        assert_eq!(err, SubmitError::ShuttingDown);
    }
}
