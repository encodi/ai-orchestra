//! Subprocess execution: bounded concurrency, structured arguments (never a
//! shell), capped output capture, timeouts, cancellation and cleanup of the
//! whole process group.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// stderr is only ever used for diagnostics, so it gets a small fixed cap.
const MAX_STDERR_BYTES: usize = 64 * 1024;
/// How long a child gets to exit after SIGTERM before it is killed.
const DEFAULT_KILL_GRACE: Duration = Duration::from_secs(2);
/// How long to wait for pipe readers after the child exited. Grandchildren
/// that inherited the pipes must not be able to hang us.
const READER_JOIN_TIMEOUT: Duration = Duration::from_secs(2);

/// Environment variables that switch the official CLIs from their
/// subscription login to usage-billed API access (or alternative billing
/// backends). Stripped from child processes by default.
pub const BILLING_ENV_VARS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "OPENAI_API_KEY",
    "CODEX_API_KEY",
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
    "GOOGLE_GENAI_USE_VERTEXAI",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
];

/// A fully specified child process. Arguments are passed as a vector and
/// never interpreted by a shell.
#[derive(Debug, Clone)]
pub struct CommandSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Written to the child's stdin, which is then closed.
    pub stdin: Option<String>,
    pub cwd: Option<PathBuf>,
    /// Variable names removed from the inherited environment.
    pub env_remove: Vec<String>,
    /// Variables added to the child's environment.
    pub env_set: Vec<(String, String)>,
    pub timeout: Duration,
    pub max_stdout_bytes: usize,
}

impl CommandSpec {
    pub fn new(program: impl Into<PathBuf>, args: Vec<String>, timeout: Duration) -> Self {
        Self {
            program: program.into(),
            args,
            stdin: None,
            cwd: None,
            env_remove: Vec::new(),
            env_set: Vec::new(),
            timeout,
            max_stdout_bytes: 4 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProcessOutput {
    pub stdout: String,
    pub stderr: String,
    /// `None` when the process was terminated by a signal.
    pub exit_code: Option<i32>,
    pub success: bool,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    /// The child exited but something it started still held its stdout open, so reading
    /// stopped after a grace period. `stdout` then holds what had been read by then and may
    /// be incomplete.
    pub stdout_held_open: bool,
    pub duration: Duration,
}

/// What a pipe reader has captured so far. Shared so that a reader that has to be abandoned
/// does not take the bytes it already read with it.
#[derive(Debug, Default)]
struct Captured {
    data: Vec<u8>,
    truncated: bool,
}

type Shared = Arc<Mutex<Captured>>;

/// A running pipe reader and its capture buffer.
struct Reader {
    task: JoinHandle<()>,
    shared: Shared,
}

fn spawn_reader<R: AsyncRead + Unpin + Send + 'static>(reader: R, cap: usize) -> Reader {
    let shared = Shared::default();
    let task = tokio::spawn(read_capped(reader, cap, shared.clone()));
    Reader { task, shared }
}

#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    #[error("failed to start process: {0}")]
    Spawn(std::io::Error),
    #[error("timed out after {}s", .0.as_secs())]
    Timeout(Duration),
    #[error("cancelled")]
    Cancelled,
    #[error("I/O error while talking to the process: {0}")]
    Io(std::io::Error),
}

/// Runs child processes with a global concurrency limit.
#[derive(Debug, Clone)]
pub struct ProcessManager {
    permits: Arc<Semaphore>,
    kill_grace: Duration,
}

enum Outcome {
    Exited(std::io::Result<std::process::ExitStatus>),
    TimedOut,
    Cancelled,
}

impl ProcessManager {
    pub fn new(max_concurrent: usize) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(max_concurrent.max(1))),
            kill_grace: DEFAULT_KILL_GRACE,
        }
    }

    #[cfg(test)]
    fn with_kill_grace(mut self, grace: Duration) -> Self {
        self.kill_grace = grace;
        self
    }

    /// Runs `spec` to completion. The timeout starts once a concurrency slot
    /// has been acquired. On timeout or cancellation the whole process group
    /// is terminated before returning.
    pub async fn run(
        &self,
        spec: CommandSpec,
        cancel: &CancellationToken,
    ) -> Result<ProcessOutput, ProcessError> {
        let _permit = tokio::select! {
            permit = self.permits.acquire() => match permit {
                Ok(p) => p,
                Err(_) => return Err(ProcessError::Cancelled),
            },
            _ = cancel.cancelled() => return Err(ProcessError::Cancelled),
        };

        let mut cmd = Command::new(&spec.program);
        cmd.args(&spec.args)
            .stdin(if spec.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for key in &spec.env_remove {
            cmd.env_remove(key);
        }
        for (key, value) in &spec.env_set {
            cmd.env(key, value);
        }
        if let Some(cwd) = &spec.cwd {
            cmd.current_dir(cwd);
        }
        // Own process group, so the terminal's Ctrl-C does not reach the child
        // behind our back and so we can terminate the child's descendants too.
        #[cfg(unix)]
        cmd.process_group(0);

        // Only the program and argument count are logged: arguments may carry
        // the prompt.
        debug!(program = %spec.program.display(), args = spec.args.len(), "spawning");
        let started = Instant::now();
        let mut child = cmd.spawn().map_err(ProcessError::Spawn)?;

        let stdin_task = match (child.stdin.take(), spec.stdin) {
            (Some(mut pipe), Some(data)) => Some(tokio::spawn(async move {
                // A child that exits without reading its input is not an error here.
                let _ = pipe.write_all(data.as_bytes()).await;
                let _ = pipe.shutdown().await;
            })),
            _ => None,
        };
        let stdout_task = child
            .stdout
            .take()
            .map(|r| spawn_reader(r, spec.max_stdout_bytes));
        let stderr_task = child
            .stderr
            .take()
            .map(|r| spawn_reader(r, MAX_STDERR_BYTES));

        let outcome = tokio::select! {
            status = child.wait() => Outcome::Exited(status),
            _ = tokio::time::sleep(spec.timeout) => Outcome::TimedOut,
            _ = cancel.cancelled() => Outcome::Cancelled,
        };

        let result = match outcome {
            Outcome::Exited(status) => {
                let status = status.map_err(ProcessError::Io);
                match status {
                    Ok(status) => {
                        // Both pipes are usually held by the same stray process: wait once.
                        let (stdout, stderr) =
                            tokio::join!(join_reader(stdout_task), join_reader(stderr_task));
                        Ok(ProcessOutput {
                            stdout: String::from_utf8_lossy(&stdout.data).into_owned(),
                            stderr: String::from_utf8_lossy(&stderr.data).into_owned(),
                            exit_code: status.code(),
                            success: status.success(),
                            stdout_truncated: stdout.truncated,
                            stderr_truncated: stderr.truncated,
                            stdout_held_open: stdout.held_open,
                            duration: started.elapsed(),
                        })
                    }
                    Err(e) => Err(e),
                }
            }
            Outcome::TimedOut => {
                self.terminate(&mut child).await;
                abort(stdout_task);
                abort(stderr_task);
                Err(ProcessError::Timeout(spec.timeout))
            }
            Outcome::Cancelled => {
                self.terminate(&mut child).await;
                abort(stdout_task);
                abort(stderr_task);
                Err(ProcessError::Cancelled)
            }
        };
        if let Some(task) = stdin_task {
            task.abort();
        }
        result
    }

    /// SIGTERM the process group, escalate to SIGKILL after the grace period,
    /// then reap the child. The group is only signalled while the child has
    /// not been reaped, so the pid cannot have been recycled.
    async fn terminate(&self, child: &mut Child) {
        let pid = child.id();
        signal_group(pid, Signal::Term, child);
        match tokio::time::timeout(self.kill_grace, child.wait()).await {
            Ok(_) => {}
            Err(_) => {
                warn!("child ignored SIGTERM; killing");
                signal_group(pid, Signal::Kill, child);
                let _ = child.wait().await;
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Signal {
    Term,
    Kill,
}

#[cfg(unix)]
fn signal_group(pid: Option<u32>, signal: Signal, child: &mut Child) {
    let sig = match signal {
        Signal::Term => libc::SIGTERM,
        Signal::Kill => libc::SIGKILL,
    };
    let group = pid.and_then(|p| i32::try_from(p).ok());
    match group {
        Some(pgid) => {
            // SAFETY: `kill` has no memory-safety preconditions. The child was
            // spawned with `process_group(0)`, so its pid is its group id, and
            // the caller has not reaped it yet.
            let rc = unsafe { libc::kill(-pgid, sig) };
            if rc != 0 {
                let _ = child.start_kill();
            }
        }
        None => {
            let _ = child.start_kill();
        }
    }
}

#[cfg(not(unix))]
fn signal_group(_pid: Option<u32>, _signal: Signal, child: &mut Child) {
    let _ = child.start_kill();
}

fn abort(reader: Option<Reader>) {
    if let Some(reader) = reader {
        reader.task.abort();
    }
}

struct Joined {
    data: Vec<u8>,
    /// More output than the cap was produced and the excess was dropped.
    truncated: bool,
    /// The reader had to be abandoned before end of file.
    held_open: bool,
}

/// Waits briefly for a reader to reach end of file. A grandchild that inherited the pipe
/// can keep it open forever; then the reader is abandoned but its captured bytes are kept.
async fn join_reader(reader: Option<Reader>) -> Joined {
    let Some(mut reader) = reader else {
        return Joined {
            data: Vec::new(),
            truncated: false,
            held_open: false,
        };
    };
    let held_open = tokio::time::timeout(READER_JOIN_TIMEOUT, &mut reader.task)
        .await
        .is_err();
    if held_open {
        reader.task.abort();
    }
    let mut captured = reader.shared.lock().unwrap_or_else(|e| e.into_inner());
    Joined {
        data: std::mem::take(&mut captured.data),
        truncated: captured.truncated,
        held_open,
    }
}

/// Reads to EOF but keeps at most `cap` bytes. The remainder is drained and
/// discarded so the child never blocks on a full pipe.
async fn read_capped<R: AsyncRead + Unpin>(mut reader: R, cap: usize, shared: Shared) {
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let mut captured = shared.lock().unwrap_or_else(|e| e.into_inner());
                let room = cap.saturating_sub(captured.data.len());
                if n > room {
                    captured.truncated = true;
                }
                captured.data.extend_from_slice(&buf[..n.min(room)]);
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn sh(script: &str, timeout: Duration) -> CommandSpec {
        CommandSpec::new(
            "/bin/sh",
            vec!["-c".to_string(), script.to_string()],
            timeout,
        )
    }

    fn mgr() -> ProcessManager {
        ProcessManager::new(4).with_kill_grace(Duration::from_millis(300))
    }

    #[tokio::test]
    async fn captures_stdout_stderr_and_exit_code() {
        let out = mgr()
            .run(
                sh("echo out; echo err >&2; exit 3", Duration::from_secs(5)),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(out.stdout.trim(), "out");
        assert_eq!(out.stderr.trim(), "err");
        assert_eq!(out.exit_code, Some(3));
        assert!(!out.success);
    }

    #[tokio::test]
    async fn writes_stdin_without_shell_interpolation() {
        let mut spec = sh("cat", Duration::from_secs(5));
        spec.stdin = Some("$(echo injected) `id` ; rm -rf /".to_string());
        let out = mgr().run(spec, &CancellationToken::new()).await.unwrap();
        assert_eq!(out.stdout, "$(echo injected) `id` ; rm -rf /");
    }

    #[tokio::test]
    async fn args_are_not_shell_interpreted() {
        let spec = CommandSpec::new(
            "/bin/echo",
            vec!["$(echo injected);".to_string()],
            Duration::from_secs(5),
        );
        let out = mgr().run(spec, &CancellationToken::new()).await.unwrap();
        assert_eq!(out.stdout.trim(), "$(echo injected);");
    }

    #[tokio::test]
    async fn caps_stdout_and_keeps_draining() {
        let mut spec = sh(
            "head -c 200000 /dev/zero | tr '\\0' 'a'",
            Duration::from_secs(10),
        );
        spec.max_stdout_bytes = 1000;
        let out = mgr().run(spec, &CancellationToken::new()).await.unwrap();
        assert_eq!(out.stdout.len(), 1000);
        assert!(out.stdout_truncated);
        assert!(out.success, "child must not block on a full pipe");
    }

    #[tokio::test]
    async fn output_already_read_survives_a_descendant_that_keeps_the_pipe_open() {
        // The shell prints, starts a background job that inherits stdout, and exits at once.
        // Waiting for end of file would hang, and giving up used to throw the output away.
        let started = Instant::now();
        let out = mgr()
            .run(
                sh("echo hello; sleep 5 &", Duration::from_secs(10)),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(out.stdout.trim(), "hello", "what was read is kept");
        assert!(out.success && out.stdout_held_open && !out.stdout_truncated);
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "{:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn a_normal_exit_is_not_reported_as_held_open() {
        let out = mgr()
            .run(
                sh("echo done", Duration::from_secs(5)),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(!out.stdout_held_open && !out.stdout_truncated);
    }

    #[tokio::test]
    async fn timeout_kills_the_process_group() {
        // The grandchild writes its pid, then both would sleep for a minute.
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("pid");
        let script = format!("sleep 60 & echo $! > {}; wait", pidfile.display());
        let started = Instant::now();
        let err = mgr()
            .run(
                sh(&script, Duration::from_millis(400)),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ProcessError::Timeout(_)));
        assert!(started.elapsed() < Duration::from_secs(10));

        let pid: i32 = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // Give the kernel a moment to deliver the signal.
        tokio::time::sleep(Duration::from_millis(200)).await;
        // SAFETY: signal 0 only checks for existence.
        let alive = unsafe { libc::kill(pid, 0) } == 0;
        assert!(!alive, "grandchild {pid} survived the group kill");
    }

    #[tokio::test]
    async fn cancellation_terminates_and_reports_cancelled() {
        let cancel = CancellationToken::new();
        let c2 = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            c2.cancel();
        });
        let started = Instant::now();
        let err = mgr()
            .run(sh("sleep 60", Duration::from_secs(60)), &cancel)
            .await
            .unwrap_err();
        assert!(matches!(err, ProcessError::Cancelled));
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[tokio::test]
    async fn sigterm_ignoring_child_is_killed_after_grace() {
        let started = Instant::now();
        let err = mgr()
            .run(
                sh("trap '' TERM; sleep 60", Duration::from_millis(200)),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ProcessError::Timeout(_)));
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[tokio::test]
    async fn missing_binary_is_a_spawn_error() {
        let spec = CommandSpec::new("/no/such/binary", vec![], Duration::from_secs(1));
        let err = mgr()
            .run(spec, &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(err, ProcessError::Spawn(_)));
    }

    #[tokio::test]
    async fn env_vars_are_removed() {
        // HOME is set in any realistic test environment; first prove the
        // child sees it, then prove removal hides it.
        let script = "echo \"[${HOME:-unset}]\"";
        let kept = mgr()
            .run(
                sh(script, Duration::from_secs(5)),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_ne!(kept.stdout.trim(), "[unset]", "HOME must be inherited");

        let mut spec = sh(script, Duration::from_secs(5));
        spec.env_remove = vec!["HOME".to_string()];
        let out = mgr().run(spec, &CancellationToken::new()).await.unwrap();
        assert_eq!(out.stdout.trim(), "[unset]");
    }

    #[tokio::test]
    async fn concurrency_is_bounded() {
        let mgr = Arc::new(ProcessManager::new(2));
        let dir = tempfile::tempdir().unwrap();
        // Each job records start/end markers; with 2 permits no more than 2
        // can ever be between "start" and "end".
        let log = dir.path().join("log");
        let mut tasks = Vec::new();
        for i in 0..6 {
            let mgr = mgr.clone();
            let script = format!(
                "echo start{i} >> {l}; sleep 0.2; echo end{i} >> {l}",
                l = log.display()
            );
            tasks.push(tokio::spawn(async move {
                mgr.run(
                    sh(&script, Duration::from_secs(10)),
                    &CancellationToken::new(),
                )
                .await
                .unwrap();
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }
        let mut running = 0i32;
        let mut peak = 0i32;
        for line in std::fs::read_to_string(&log).unwrap().lines() {
            if line.starts_with("start") {
                running += 1;
                peak = peak.max(running);
            } else {
                running -= 1;
            }
        }
        assert!(peak <= 2, "peak concurrency was {peak}");
        assert!(peak >= 1);
    }

    #[tokio::test]
    async fn env_vars_can_be_added() {
        let mut spec = sh("echo \"[$ORCHESTRA_PROBE]\"", Duration::from_secs(5));
        spec.env_set = vec![("ORCHESTRA_PROBE".to_string(), "yes".to_string())];
        let out = mgr().run(spec, &CancellationToken::new()).await.unwrap();
        assert_eq!(out.stdout.trim(), "[yes]");
    }
}
