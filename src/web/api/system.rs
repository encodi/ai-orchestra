//! Health, capabilities (the settings schema), saved defaults, interface preferences, providers.
//!
//! Nothing here calls a model. The provider page runs only `--version` (a local diagnostic, cached),
//! and reports authentication as `unknown`: it cannot be known without a real call, and a page load
//! must never spend quota.

use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Body, S, validate_settings};
use crate::consult::ConsultMode;
use crate::debate::QUESTION_MAX_BYTES;
use crate::history::now_ms;
use crate::provider::AgentProvider;
use crate::ux::settings::{Key, SAFETY_NOTES, Settings};
use crate::web::AppState;
use crate::web::assets;
use crate::web::error::{ApiError, ApiResult};

pub async fn health(State(state): S) -> Json<Value> {
    Json(json!({
        "ok": true, "version": env!("CARGO_PKG_VERSION"), "uptime_ms": state.started.elapsed().as_millis() as u64,
        "started_ms": state.started_ms, "running_consultations": state.jobs.running(), "frontend_built": assets::has_frontend(),
    }))
}

fn agent_choices(state: &AppState) -> Vec<Value> {
    let ctx = state.orch.context();
    state
        .orch
        .registry()
        .all()
        .iter()
        .map(|p| {
            let usable = p.availability(ctx);
            json!({"value": p.id().to_string(), "label": p.display_name(), "available": usable.is_ok(), "reason": usable.err()})
        })
        .collect()
}

fn choices(values: &[&str]) -> Vec<Value> {
    values
        .iter()
        .map(|v| json!({"value": v, "label": v, "available": true}))
        .collect()
}

/// The schema of every consultation setting, derived from the same `Key` list the CLI chat uses.
fn fields(state: &AppState) -> Vec<Value> {
    let agents = agent_choices(state);
    let web_capable = state.orch.registry().all().iter().any(|p| {
        p.config().enabled
            && matches!(
                p.research_capabilities().web_search,
                crate::research::capabilities::Support::Verified
                    | crate::research::capabilities::Support::Documented
            )
    });
    let memory_on = state.orch.memory().is_some();
    let mut out = Vec::new();
    for key in Key::ALL {
        let (kind, mut ch, min, max): (&str, Vec<Value>, Option<u64>, Option<u64>) = match key {
            Key::Mode => (
                "choice",
                ConsultMode::ALL
                    .iter()
                    .map(|m| json!({"value": m.as_str(), "label": m.as_str(), "available": true}))
                    .collect(),
                None,
                None,
            ),
            Key::Coordinator => ("choice", agents.clone(), None, None),
            Key::Agents => ("multi", agents.clone(), None, None),
            Key::Rounds => ("choice", choices(&["1", "2"]), None, None),
            Key::Depth => (
                "choice",
                choices(&["quick", "standard", "deep"]),
                None,
                None,
            ),
            Key::Web => ("choice", choices(&["on", "off"]), None, None),
            Key::Strategy => (
                "choice",
                choices(&["fast", "balanced", "thorough"]),
                None,
                None,
            ),
            Key::Length => ("choice", choices(&["brief", "full"]), None, None),
            Key::Memory => (
                "choice",
                choices(&["off", "session", "project", "global"]),
                None,
                None,
            ),
            Key::View => ("choice", choices(&["plain", "detailed"]), None, None),
            Key::Timeout => ("integer", vec![], Some(1), Some(3600)),
            Key::MaxCalls => ("integer", vec![], Some(1), Some(50)),
            Key::ContextBudget => ("integer", vec![], Some(1), Some(100_000)),
            Key::MaxSources => ("integer", vec![], Some(1), Some(20)),
            Key::FreshnessDays => ("integer", vec![], Some(1), Some(3650)),
            Key::MaxDuration => ("integer", vec![], Some(1), Some(3600)),
            Key::MaxParallel => ("integer", vec![], Some(1), Some(8)),
        };
        let mut disabled_reason: Option<&str> = None;
        if key == Key::Web && !web_capable {
            for c in &mut ch {
                if c["value"] == "on" {
                    c["available"] = json!(false);
                    c["reason"] = json!(
                        "no enabled provider supports web search; pages can still be given as URLs"
                    );
                }
            }
        }
        if key == Key::Memory && !memory_on {
            disabled_reason = Some(
                "memory is not available in this server (switched off, or its location is unknown)",
            );
        }
        out.push(json!({
            "key": key.name(), "kind": kind, "choices": ch, "min": min, "max": max, "accepts": key.accepts(),
            "disabled_reason": disabled_reason,
        }));
    }
    out
}

pub async fn capabilities(State(state): S) -> Json<Value> {
    let providers: Vec<Value> = state
        .orch
        .registry()
        .all()
        .iter()
        .map(|p| {
            let usable = p.availability(state.orch.context());
            json!({
                "id": p.id().to_string(), "display_name": p.display_name(), "aliases": p.aliases(), "enabled": p.config().enabled,
                "installed": p.resolve_executable().is_ok(), "usable": usable.is_ok(), "reason": usable.err(),
                "web_search": p.research_capabilities().web_search,
            })
        })
        .collect();
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "project": state.project,
        "providers": providers,
        "modes": ConsultMode::ALL.iter().map(|m| m.as_str()).collect::<Vec<_>>(),
        "fields": fields(&state),
        "safety": SAFETY_NOTES,
        "limits": {"max_concurrent_consultations": state.config.web.max_concurrent_consultations, "max_question_bytes": QUESTION_MAX_BYTES, "max_urls": 20},
        "features": {
            "memory": state.orch.memory().is_some(), "history": state.orch.history().is_some(),
            "evaluation": state.evals.is_some(), "feedback": state.feedback.is_some(),
            "preferences": state.prefs_path.is_some(),
        },
        "config_defaults": {
            "memory": state.config.memory.default, "context_budget_tokens": state.config.memory.context_budget_tokens,
            "default_strategy": state.config.smart.default_strategy, "timeout_secs": state.config.general.timeout_secs,
        },
        "precedence": ["safety rules", "this consultation", "this session", "saved defaults", "automatic"],
    }))
}

// ------------------------------------------------------------------------------------- saved defaults

pub async fn get_settings(State(state): S) -> ApiResult<Json<Value>> {
    let defaults = match &state.prefs_path {
        Some(p) => crate::ux::prefs::load(p).map_err(ApiError::from)?,
        None => Settings::default(),
    };
    Ok(Json(
        json!({"defaults": defaults, "path_known": state.prefs_path.is_some(), "note": "Saved defaults are the starting point of every consultation, in the browser and in the terminal chat. Everything left empty is AUTO."}),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PutSettings {
    pub defaults: Settings,
}

pub async fn put_settings(State(state): S, Body(req): Body<PutSettings>) -> ApiResult<Json<Value>> {
    let path = state.prefs_path.as_ref().ok_or_else(|| {
        ApiError::unavailable("the preferences location is unknown (no home directory)")
    })?;
    let valid = validate_settings(&state, &req.defaults, "defaults")?;
    if valid.is_empty() {
        crate::ux::prefs::clear(path).map_err(ApiError::from)?;
    } else {
        crate::ux::prefs::save(path, &valid).map_err(ApiError::from)?;
    }
    Ok(Json(json!({"defaults": valid})))
}

// ------------------------------------------------------------------------------- interface preferences

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct UiPrefs {
    /// `dark`, `light` or `system`.
    pub theme: String,
    /// `es` or `en`.
    pub language: String,
    /// `system`, `on` or `off`.
    pub reduced_motion: String,
    /// Open the Agent Theater view by default.
    pub theater_default: bool,
    /// Show the animated stage above the chat while a consultation runs.
    pub show_stage: bool,
}

impl Default for UiPrefs {
    fn default() -> Self {
        Self {
            theme: "dark".into(),
            language: "es".into(),
            reduced_motion: "system".into(),
            theater_default: false,
            show_stage: true,
        }
    }
}

impl UiPrefs {
    fn validate(&self) -> Result<(), ApiError> {
        let ok = |v: &str, allowed: &[&str], field: &str| {
            if allowed.contains(&v) {
                Ok(())
            } else {
                Err(
                    ApiError::validation(format!("{field} must be one of: {}", allowed.join(", ")))
                        .field(field),
                )
            }
        };
        ok(&self.theme, &["dark", "light", "system"], "theme")?;
        ok(&self.language, &["es", "en"], "language")?;
        ok(
            &self.reduced_motion,
            &["system", "on", "off"],
            "reduced_motion",
        )
    }
}

fn load_ui(state: &AppState) -> UiPrefs {
    state
        .ui_path
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub async fn get_ui(State(state): S) -> Json<Value> {
    Json(json!({"preferences": load_ui(&state)}))
}

pub async fn put_ui(State(state): S, Body(prefs): Body<UiPrefs>) -> ApiResult<Json<Value>> {
    prefs.validate()?;
    let path = state
        .ui_path
        .as_ref()
        .ok_or_else(|| ApiError::unavailable("the preferences location is unknown"))?;
    {
        use std::io::Write as _;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| ApiError::internal(e.to_string()))?;
        }
        let tmp = path.with_extension("json.tmp");
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts
            .open(&tmp)
            .map_err(|e| ApiError::internal(e.to_string()))?;
        f.write_all(
            serde_json::to_string_pretty(&prefs)
                .unwrap_or_default()
                .as_bytes(),
        )
        .map_err(|e| ApiError::internal(e.to_string()))?;
        f.sync_all()
            .map_err(|e| ApiError::internal(e.to_string()))?;
        std::fs::rename(&tmp, path).map_err(|e| ApiError::internal(e.to_string()))?;
    }
    Ok(Json(json!({"preferences": prefs})))
}

// ------------------------------------------------------------------------------------------- providers

const VERSION_TTL: Duration = Duration::from_secs(60);

async fn probe_version(state: &AppState, p: &dyn AgentProvider) -> Option<String> {
    let path = p.resolve_executable().ok()?;
    let args = p.version_args();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = crate::providers::run_probe(state.orch.context(), &path, &args).await?;
    if !out.success {
        return None;
    }
    let text = if out.stdout.trim().is_empty() {
        out.stderr
    } else {
        out.stdout
    };
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.chars().take(100).collect())
}

pub async fn providers(State(state): S) -> Json<Value> {
    if let Some((at, v)) = state
        .provider_cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        && at.elapsed() < VERSION_TTL
    {
        return Json(v.clone());
    }
    let (snapshot, note) = state.orch.profile_snapshot();
    let mut rows = Vec::new();
    for p in state.orch.registry().all() {
        let usable = p.availability(state.orch.context());
        let path = p.resolve_executable().ok();
        let version = if path.is_some() {
            probe_version(&state, p.as_ref()).await
        } else {
            None
        };
        let profile = snapshot.profile(p.id().as_str()).map(|pp| {
            json!({
                "invocations": pp.invocations, "successes": pp.successes, "failures": pp.failures, "timeouts": pp.timeouts,
                "quota_errors": pp.quota_errors, "auth_errors": pp.auth_errors,
                "latency_ms": pp.latency_ms.iter().map(|(k, e)| (k.clone(), json!({"mean": e.mean.round(), "samples": e.samples}))).collect::<serde_json::Map<_, _>>(),
                "last_success_ms": pp.last_success_ms, "last_failure_ms": pp.last_failure_ms, "last_failure_kind": pp.last_failure_kind,
                "failure_streak": pp.failure_streak, "tokens_reported": {"calls": pp.calls_reporting_tokens, "input": pp.input_tokens, "output": pp.output_tokens},
            })
        });
        rows.push(json!({
            "id": p.id().to_string(), "display_name": p.display_name(), "aliases": p.aliases(), "enabled": p.config().enabled,
            "executable": path.as_ref().map(|x| x.display().to_string()), "installed": path.is_some(), "version": version,
            "usable": usable.is_ok(), "reason": usable.err(),
            "authentication": {"status": "unknown", "note": "Whether you are signed in cannot be known without a real model call, and opening this page never makes one. Run `ai-orchestra doctor` for the CLI's own login status."},
            "capabilities": p.research_capabilities(), "configured_model": p.config().model, "configured_effort": p.config().effort,
            "history": profile,
        }));
    }
    let v = json!({"providers": rows, "history_note": note, "checked_ms": now_ms(), "cached_for_secs": VERSION_TTL.as_secs()});
    *state
        .provider_cache
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), v.clone()));
    Json(v)
}
