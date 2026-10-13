//! The local web interface: a loopback HTTP server over the same Rust services as the CLI.
//!
//! * `api` - the versioned JSON API (`/api/v1/...`) and the server-sent events stream;
//! * `jobs` - consultations running on behalf of the browser: recorded events, replay, cancellation;
//! * `theater` - the real recorded steps of a consultation, for the Agent Theater;
//! * `security` - Host validation, token and origin checks, response headers;
//! * `assets` - the compiled frontend, embedded in the binary.
//!
//! Nothing here orchestrates anything: every consultation is `Orchestrator::consult`, memory is
//! `MemoryService`, evaluation is `eval::*`, preferences are `ux::prefs`. The browser never calls a model,
//! and loading or refreshing a page never starts an agent: only an explicit `POST /consultations` does.

pub mod api;
pub mod assets;
pub mod error;
pub mod jobs;
pub mod security;
pub mod theater;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::Router;
use axum::middleware;
use serde_json::Value;

use crate::config::Config;
use crate::error::{OrchestraError, Result};
use crate::eval::feedback::FeedbackStore;
use crate::eval::run::EvalStore;
use crate::memory::service::ProjectRef;
use crate::orchestrator::Orchestrator;

pub struct AppState {
    pub orch: Arc<Orchestrator>,
    pub config: Config,
    pub token: String,
    pub port: u16,
    pub extra_hosts: Vec<String>,
    pub jobs: jobs::JobStore,
    pub started: Instant,
    pub started_ms: u64,
    /// Saved consultation defaults (`ux::prefs`) and interface preferences (appearance, theater).
    pub prefs_path: Option<PathBuf>,
    pub ui_path: Option<PathBuf>,
    /// Names, profiles and look of the agents' characters, and the reply-language preference.
    pub characters_path: Option<PathBuf>,
    /// Consultations whose summary is being written (a second request is refused, never duplicated).
    pub summarizing: Mutex<std::collections::BTreeSet<String>>,
    pub feedback: Option<FeedbackStore>,
    pub evals: Option<EvalStore>,
    pub project: Option<ProjectRef>,
    pub provider_cache: Mutex<Option<(Instant, Value)>>,
}

impl AppState {
    pub fn new(orch: Arc<Orchestrator>, config: Config, port: u16) -> Arc<Self> {
        let data = config
            .resolved_memory_path()
            .and_then(|p| p.parent().map(PathBuf::from));
        let project = orch.memory().map(|m| m.default_project(None));
        Arc::new(Self {
            extra_hosts: config.web.extra_allowed_hosts.clone(),
            prefs_path: crate::ux::prefs::path_for(&config),
            ui_path: data.as_ref().map(|d| d.join("web-ui.json")),
            characters_path: data.as_ref().map(|d| d.join("characters.json")),
            summarizing: Mutex::new(std::collections::BTreeSet::new()),
            feedback: crate::eval::feedback_path(&config).map(FeedbackStore::new),
            evals: crate::eval::runs_dir(&config).map(EvalStore::new),
            orch,
            config,
            token: security::new_token(),
            port,
            jobs: jobs::JobStore::default(),
            started: Instant::now(),
            started_ms: crate::history::now_ms(),
            project,
            provider_cache: Mutex::new(None),
        })
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .merge(api::routes())
        .fallback(api::static_files)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            security::guard,
        ))
        .with_state(state)
}

pub struct ServeOptions {
    pub host: IpAddr,
    pub port: u16,
    pub allow_non_local: bool,
}

impl Default for ServeOptions {
    fn default() -> Self {
        Self {
            host: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 8765,
            allow_non_local: false,
        }
    }
}

/// Binds the server. Refuses a non-loopback address unless it was explicitly allowed.
pub async fn bind(opts: &ServeOptions) -> Result<tokio::net::TcpListener> {
    if !opts.host.is_loopback() && !opts.allow_non_local {
        return Err(OrchestraError::Config(format!(
            "{} is not a loopback address. The web interface has no user accounts; it is meant for this machine only. Use --allow-non-local only on a network you fully trust, and add the host name you will use to [web] extra_allowed_hosts",
            opts.host
        )));
    }
    tokio::net::TcpListener::bind(SocketAddr::new(opts.host, opts.port))
        .await
        .map_err(|e| {
            OrchestraError::Config(format!("cannot listen on {}:{}: {e}", opts.host, opts.port))
        })
}

/// Serves until `shutdown` is cancelled, then cancels running consultations and waits for them to
/// finish recording (so stopping the server never leaves a half-written record).
pub async fn serve(
    listener: tokio::net::TcpListener,
    state: Arc<AppState>,
    shutdown: tokio_util::sync::CancellationToken,
) -> Result<()> {
    let app = router(state.clone());
    let stop = shutdown.clone();
    let server =
        axum::serve(listener, app).with_graceful_shutdown(async move { stop.cancelled().await });
    let result = server.await;
    state.jobs.cancel_all();
    // Give running jobs a moment to record their cancelled outcome.
    for _ in 0..100 {
        if state.jobs.running() == 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    result.map_err(OrchestraError::Io)
}
