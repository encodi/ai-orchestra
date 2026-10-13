//! Consultations: create (or plan), watch, cancel, replay, research, history, feedback.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Body, Q, S, validate_settings};
use crate::debate::{DebateOutcome, QUESTION_MAX_BYTES};
use crate::error::OrchestraError;
use crate::eval::feedback::{Feedback, Rating};
use crate::history::now_ms;
use crate::memory::model::MemoryMode;
use crate::memory::service::MemoryRequest;
use crate::store::redact_secrets;
use crate::ux::settings::{Key, Layers, Origin, Settings};
use crate::web::AppState;
use crate::web::error::{ApiError, ApiResult, kind_of};
use crate::web::jobs::{Job, map_event};
use crate::web::theater;

const MAX_URLS: usize = 20;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateReq {
    pub question: String,
    /// Settings that apply to this browser session (the "current session overrides").
    #[serde(default)]
    pub session_settings: Settings,
    /// Settings for this consultation only.
    #[serde(default)]
    pub settings: Settings,
    #[serde(default)]
    pub urls: Vec<String>,
    /// Continue this AI Orchestra session.
    pub session: Option<String>,
    #[serde(default)]
    pub new_session: bool,
    /// Remember this consultation as session memory (default true; `false` is `--no-save` for memory).
    #[serde(default = "yes")]
    pub remember: bool,
    /// Plan only: say what would happen and what would be recalled; start nothing.
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub explain_context: bool,
}

fn yes() -> bool {
    true
}

fn settings_in_force(resolved: &crate::ux::settings::Resolved) -> Value {
    let mut m = serde_json::Map::new();
    for key in Key::ALL {
        let origin = resolved.origins.get(&key).copied().unwrap_or(Origin::Auto);
        m.insert(
            key.name().to_string(),
            json!({"value": resolved.settings.get(key), "origin": origin, "origin_label": origin.label()}),
        );
    }
    Value::Object(m)
}

pub fn load_layers(state: &AppState, session: Settings) -> ApiResult<Layers> {
    let preferences = match &state.prefs_path {
        Some(p) => crate::ux::prefs::load(p).map_err(ApiError::from)?,
        None => Settings::default(),
    };
    Ok(Layers {
        preferences,
        session,
    })
}

pub async fn create(
    State(state): S,
    Body(req): Body<CreateReq>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let question = req.question.trim().to_string();
    if question.is_empty() {
        return Err(ApiError::validation("the question is empty").field("question"));
    }
    if question.len() > QUESTION_MAX_BYTES {
        return Err(ApiError::validation(format!(
            "the question is longer than {QUESTION_MAX_BYTES} bytes"
        ))
        .field("question"));
    }
    if req.urls.len() > MAX_URLS {
        return Err(ApiError::validation(format!("at most {MAX_URLS} URLs")).field("urls"));
    }
    for u in &req.urls {
        if !u.starts_with("https://") || u.len() > 2000 || u.chars().any(char::is_whitespace) {
            return Err(ApiError::validation(format!("`{u}` is not an HTTPS URL")).field("urls"));
        }
    }
    let session_settings = validate_settings(&state, &req.session_settings, "session_settings")?;
    let request_settings = validate_settings(&state, &req.settings, "settings")?;
    let layers = load_layers(&state, session_settings)?;
    let resolved = layers.resolve(&request_settings);
    if resolved.settings.web.as_deref() == Some("off") && !req.urls.is_empty() {
        return Err(ApiError::validation("URLs were given but web access is off").field("urls"));
    }
    let mode = resolved
        .memory_mode(&state.config)
        .map_err(ApiError::from)?;
    let memory = match (mode, state.orch.memory()) {
        (MemoryMode::Off, _) | (_, None) => None,
        (mode, Some(_)) => {
            let mut m = MemoryRequest::new(mode);
            m.session = req.session.clone();
            m.new_session = req.new_session && req.session.is_none();
            m.context_budget_tokens = resolved.settings.context_budget;
            m.explain = req.explain_context;
            m.write = req.remember;
            m.project = state.project.clone();
            Some(m)
        }
    };
    if memory.is_none()
        && (req.session.is_some() || resolved.settings.context_budget.is_some())
        && mode == MemoryMode::Off
    {
        return Err(ApiError::validation(
            "a session or a context budget needs memory, and memory is off",
        )
        .field("memory"));
    }
    let mut options = resolved
        .to_options(question.clone(), &state.config, memory)
        .map_err(ApiError::from)?;
    options.research.urls = req.urls.clone();
    options.style = super::characters::style_for(&state);
    state.orch.check_not_nested().map_err(ApiError::from)?;
    // Routing is deterministic and free: reject an impossible request before anything starts.
    let decision = state
        .orch
        .plan_consultation(&options)
        .map_err(ApiError::from)?;
    let plan = json!({"plan": decision, "plan_text": decision.render()});

    if req.dry_run {
        let context = if req.explain_context || options.memory.is_some() {
            match state.orch.preview_context(&options).await {
                Ok(Some(p)) => Some(json!(p.report)),
                Ok(None) => None,
                Err(e) => return Err(e.into()),
            }
        } else {
            None
        };
        return Ok((
            StatusCode::OK,
            Json(
                json!({"dry_run": true, "plan": plan["plan"], "plan_text": plan["plan_text"], "context": context, "settings_in_force": settings_in_force(&resolved), "note": "Nothing was run: no agent was called and no page was fetched."}),
            ),
        ));
    }

    let in_force = settings_in_force(&resolved);
    let Some(job) = state.jobs.create(
        &question,
        in_force.clone(),
        state.config.web.max_concurrent_consultations,
    ) else {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "busy",
            format!(
                "{} consultation(s) are already running; wait for one to finish or cancel it",
                state.config.web.max_concurrent_consultations
            ),
        ));
    };
    job.push(
        "planning_started",
        json!({"question_chars": question.chars().count(), "settings": in_force}),
    );
    job.push("strategy_selected", plan.clone());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    options.events = Some(tx);
    let orch = state.orch.clone();
    let runner = job.clone();
    tokio::spawn(async move {
        let cancel = runner.cancel.clone();
        let result = {
            let fut = orch.consult(options, &cancel);
            tokio::pin!(fut);
            loop {
                tokio::select! {
                    res = &mut fut => break res,
                    Some(ev) = rx.recv() => record(&runner, &ev),
                }
            }
        };
        while let Ok(ev) = rx.try_recv() {
            record(&runner, &ev);
        }
        match result {
            Ok(outcome) => runner.finish_with_outcome(outcome),
            Err(e) => {
                let cancelled = matches!(e, OrchestraError::Cancelled { .. });
                runner.finish_with_error(kind_of(&e), &e.to_string(), cancelled);
            }
        }
    });
    Ok((
        StatusCode::ACCEPTED,
        Json(
            json!({"job_id": job.id, "status": "running", "events_url": format!("/api/v1/consultations/{}/events", job.id), "plan": plan["plan"], "plan_text": plan["plan_text"], "settings_in_force": in_force}),
        ),
    ))
}

fn record(job: &Job, ev: &crate::debate::DebateEvent) {
    for (kind, data) in map_event(ev) {
        if kind == "consultation_created"
            && let Some(id) = data.get("consultation_id").and_then(Value::as_str)
        {
            job.set_consultation_id(id);
        }
        job.push(kind, data);
    }
}

// ------------------------------------------------------------------------------------ one consultation

fn view(o: &DebateOutcome, saved: bool, now: u64) -> Value {
    let meta = o.consultation.as_ref();
    let research = meta.and_then(|m| m.research.as_ref());
    let mut meta_json = serde_json::to_value(meta).unwrap_or(Value::Null);
    if let Some(obj) = meta_json.as_object_mut() {
        // The research record is served by its own endpoint; only its size is here.
        obj.remove("research");
    }
    json!({
        "id": o.id, "kind": "consultation", "saved": saved, "status": o.status.as_str(),
        "question": redact_secrets(&o.question), "created_at_ms": o.created_at_ms, "duration_ms": o.duration_ms,
        "calls": o.calls, "coordinator": o.coordinator.to_string(),
        "participants": o.participants.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "skipped": o.skipped.iter().map(|s| json!({"agent": s.provider.to_string(), "reason": s.reason})).collect::<Vec<_>>(),
        "synthesized": o.synthesized, "final_answer": redact_secrets(&o.final_answer),
        "warnings": o.warnings.iter().map(|w| redact_secrets(w)).collect::<Vec<_>>(),
        "mode": meta.and_then(|m| m.mode).map(|m| m.to_string()),
        "strategy": meta.map(|m| m.strategy.clone()),
        "routing_text": meta.and_then(|m| m.routing.as_ref()).map(|r| r.render()),
        "meta": meta_json,
        "research": research.map(|r| json!({"sources": r.sources.len(), "retrieved": r.retrieved(), "claims": r.claims.len()})),
        "summaries": o.summaries,
        "age_days": now.saturating_sub(o.created_at_ms) / 86_400_000,
        "steps": o.steps.len(),
    })
}

fn job_header(job: &Job) -> Value {
    let (events, dropped) = job.event_count();
    json!({
        "job_id": job.id, "status": job.status(), "created_ms": job.created_ms, "finished_ms": job.finished_ms(),
        "question_preview": job.question_preview, "consultation_id": job.consultation_id(),
        "settings_in_force": job.settings_in_force, "error": job.error(), "events": events, "events_dropped": dropped,
    })
}

enum Found {
    Job(std::sync::Arc<Job>),
    Saved(Box<DebateOutcome>),
}

fn find(state: &AppState, id: &str) -> ApiResult<Found> {
    if let Some(job) = state.jobs.find(id) {
        return Ok(Found::Job(job));
    }
    let store = state.orch.history().ok_or_else(|| {
        ApiError::unavailable(
            "consultations are not being saved (debate.save = false), so there is no history",
        )
    })?;
    store
        .load(id)
        .map(|o| Found::Saved(Box::new(o)))
        .map_err(|e| ApiError::not_found(e.to_string()))
}

pub async fn get_one(State(state): S, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let now = now_ms();
    match find(&state, &id)? {
        Found::Job(job) => {
            let mut v = json!({"kind": "job", "job": job_header(&job)});
            if let Some(o) = job.outcome() {
                // The saved record is the newest (a summary written later is only there).
                let saved = state.orch.history().and_then(|h| h.load(&o.id).ok());
                let mut c = view(
                    &saved.clone().unwrap_or_else(|| o.clone()),
                    saved.is_some(),
                    now,
                );
                c["steps"] = json!(o.steps.len());
                v["consultation"] = c;
            }
            Ok(Json(v))
        }
        Found::Saved(o) => {
            let mut v = json!({"kind": "saved", "consultation": view(&o, true, now)});
            if let Some(fb) = &state.feedback {
                v["feedback"] = json!(fb.for_consultation(&o.id).unwrap_or_default());
            }
            Ok(Json(v))
        }
    }
}

pub async fn cancel(
    State(state): S,
    Path(id): Path<String>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let Some(job) = state.jobs.find(&id) else {
        return Err(ApiError::not_found(
            "no running consultation with that id (finished consultations cannot be cancelled)",
        ));
    };
    if job.status().is_finished() {
        return Err(ApiError::conflict("that consultation has already finished"));
    }
    job.cancel.cancel();
    Ok((
        StatusCode::ACCEPTED,
        Json(
            json!({"job_id": job.id, "cancelling": true, "note": "Running agents are being terminated; the result will be recorded as cancelled, never as complete."}),
        ),
    ))
}

pub async fn remove(
    State(state): S,
    Path(id): Path<String>,
    Q(q): Q<ConfirmQuery>,
) -> ApiResult<Json<Value>> {
    if !q.confirm {
        return Err(
            ApiError::validation("deleting a saved consultation needs confirm=true")
                .field("confirm"),
        );
    }
    if state
        .jobs
        .find(&id)
        .is_some_and(|j| !j.status().is_finished())
    {
        return Err(ApiError::conflict("cancel the running consultation first"));
    }
    let store = state
        .orch
        .history()
        .ok_or_else(|| ApiError::unavailable("consultations are not being saved"))?;
    let full = store
        .load(&id)
        .map_err(|e| ApiError::not_found(e.to_string()))?;
    store.delete(&full.id).map_err(ApiError::from)?;
    state.jobs.forget_consultation(&full.id);
    Ok(Json(
        json!({"deleted": full.id, "note": "Only this consultation's saved record was deleted. Memories remembered from it are separate (see Memory)."}),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfirmQuery {
    #[serde(default)]
    pub confirm: bool,
}

// ------------------------------------------------------------------------------------------ events (SSE)

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventsQuery {
    pub after: Option<u64>,
}

/// Server-sent events of a running (or recently finished) job. `Last-Event-ID` (or `?after=`) resumes after
/// the last event the client saw; connecting again only reads recorded events, it never starts a call.
pub async fn events(
    State(state): S,
    Path(id): Path<String>,
    headers: HeaderMap,
    Q(q): Q<EventsQuery>,
) -> ApiResult<Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>>> {
    let Some(job) = state.jobs.find(&id) else {
        return Err(ApiError::not_found(
            "no such running or recent consultation on this server; open a saved consultation through /theater instead",
        ));
    };
    let after = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .or(q.after);
    struct St {
        job: std::sync::Arc<Job>,
        after: Option<u64>,
        buffer: VecDeque<crate::web::jobs::EventRecord>,
    }
    let init = St {
        job,
        after,
        buffer: VecDeque::new(),
    };
    let stream = futures_util::stream::unfold(init, |mut st| async move {
        loop {
            if let Some(e) = st.buffer.pop_front() {
                let ev = Event::default()
                    .id(e.seq.to_string())
                    .event(e.kind)
                    .json_data(
                        json!({"seq": e.seq, "ts_ms": e.ts_ms, "kind": e.kind, "data": e.data}),
                    )
                    .unwrap_or_else(|_| Event::default().comment("unserialisable event"));
                return Some((Ok(ev), st));
            }
            let notified = st.job.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let (events, finished) = st.job.events_after(st.after);
            if let Some(last) = events.last() {
                st.after = Some(last.seq);
                st.buffer.extend(events);
                continue;
            }
            if finished {
                return None;
            }
            let _ = tokio::time::timeout(Duration::from_secs(20), notified).await;
        }
    });
    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    ))
}

// ---------------------------------------------------------------------------------------- theater, research

fn outcome_of(state: &AppState, id: &str) -> ApiResult<DebateOutcome> {
    match find(state, id)? {
        Found::Saved(o) => Ok(*o),
        Found::Job(job) => match job.outcome() {
            Some(o) => Ok(o),
            None if job.status().is_finished() => {
                Err(ApiError::conflict("that consultation produced no result"))
            }
            None => Err(ApiError::conflict(
                "that consultation is still running; watch its events instead",
            )),
        },
    }
}

pub async fn theater(State(state): S, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let o = outcome_of(&state, &id)?;
    Ok(Json(json!(theater::timeline(&o))))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SummaryReq {
    /// The agent that writes it; the coordinator when absent.
    pub agent: Option<String>,
}

/// Removes the consultation from the "being summarised" set when the request ends, however it ends.
struct Release(std::sync::Arc<AppState>, String);

impl Drop for Release {
    fn drop(&mut self) {
        self.0
            .summarizing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.1);
    }
}

/// One explicit call to the coordinator for a short summary of the report. The client leaving (a cancelled
/// request) cancels the call and terminates the agent process.
pub async fn summarize(
    State(state): S,
    Path(id): Path<String>,
    Body(req): Body<SummaryReq>,
) -> ApiResult<Json<Value>> {
    let o = outcome_of(&state, &id)?;
    if o.final_answer.trim().is_empty()
        || matches!(
            o.status,
            crate::debate::DebateStatus::Failed | crate::debate::DebateStatus::Cancelled
        )
    {
        return Err(ApiError::conflict(
            "that consultation produced no report to summarise",
        ));
    }
    if let Some(a) = req.agent.as_deref() {
        let wanted = state
            .orch
            .registry()
            .resolve(a)
            .map_err(|e| ApiError::validation(e.to_string()).field("agent"))?;
        let took_part = *wanted.id() == o.coordinator || o.participants.contains(wanted.id());
        if !took_part {
            return Err(ApiError::validation(
                "only the coordinator or an agent that took part can write the summary",
            )
            .field("agent"));
        }
    }
    {
        let mut busy = state.summarizing.lock().unwrap_or_else(|e| e.into_inner());
        if !busy.insert(o.id.clone()) {
            return Err(ApiError::conflict(
                "a summary of this consultation is already being written",
            ));
        }
    }
    let _release = Release(state.clone(), o.id.clone());
    let cancel = tokio_util::sync::CancellationToken::new();
    let _on_disconnect = cancel.clone().drop_guard();
    let style = super::characters::style_for(&state);
    let summary = state
        .orch
        .summarize_consultation(&o.id, req.agent.as_deref(), style.as_deref(), &cancel)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(json!({
        "summary": summary,
        "note": "Written by an AI agent from the report only, on your request. It is a condensation, not a check: the report and its sources remain the reference.",
    })))
}

pub async fn research(State(state): S, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let o = outcome_of(&state, &id)?;
    let now = now_ms();
    let Some(r) = o.consultation.as_ref().and_then(|m| m.research.as_ref()) else {
        return Ok(Json(json!({
            "consultation_id": o.id, "web_occurred": false,
            "note": "No web research was done for this consultation, so there are no sources and nothing in the answer was checked against external sources.",
        })));
    };
    let sources: Vec<Value> = r
        .sources
        .iter()
        .map(|s| {
            let mut v = serde_json::to_value(s).unwrap_or(Value::Null);
            let retrieved = s.status == crate::research::source::SourceStatus::Retrieved;
            if let Some(obj) = v.as_object_mut() {
                obj.insert("retrieved".into(), json!(retrieved));
                obj.insert("display_url".into(), json!(s.display_url()));
                obj.insert(
                    "age_days".into(),
                    json!(
                        s.retrieved_at_ms
                            .map(|t| now.saturating_sub(t) / 86_400_000)
                    ),
                );
                obj.insert(
                    "provenance".into(),
                    json!(if retrieved {
                        "retrieved by AI Orchestra itself: its text was extracted and is evidence"
                    } else {
                        "reported or found but NOT retrieved: it is not evidence"
                    }),
                );
            }
            v
        })
        .collect();
    Ok(Json(json!({
        "consultation_id": o.id,
        "web_occurred": r.retrieved() > 0 || !r.sources.is_empty(),
        "retrieved_sources": r.retrieved(),
        "depth": r.depth, "web_requested": r.web_requested, "max_sources": r.max_sources, "freshness_days": r.freshness_days,
        "discovery_agent": r.discovery_agent, "subquestions": r.subquestions, "search_queries": r.search_queries,
        "sources": sources, "claims": r.claims, "contradictions": r.contradictions, "citations": r.citations,
        "assessors": r.assessors, "limitations": r.limitations, "budget": r.budget,
        "judgement_note": "Whether a passage supports a claim is judged by an AI model and is labelled model-dependent; the status of a claim is computed from those judgements, never from agreement between models.",
        "freshness_note": "Retrieval dates are when the pages were fetched. Nothing is re-checked when an old consultation is opened.",
    })))
}

// ------------------------------------------------------------------------------------------------ history

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListQuery {
    pub q: Option<String>,
    pub mode: Option<String>,
    pub provider: Option<String>,
    pub strategy: Option<String>,
    pub status: Option<String>,
    /// Inclusive bounds as epoch milliseconds.
    pub from: Option<u64>,
    pub to: Option<u64>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

pub async fn list(State(state): S, Q(q): Q<ListQuery>) -> ApiResult<Json<Value>> {
    let limit = q.limit.unwrap_or(20).clamp(1, 100);
    let offset = q.offset.unwrap_or(0);
    let running: Vec<Value> = state
        .jobs
        .all()
        .iter()
        .filter(|j| !j.status().is_finished())
        .map(|j| job_header(j))
        .collect();
    let Some(store) = state.orch.history() else {
        return Ok(Json(
            json!({"items": [], "total": 0, "running": running, "history_enabled": false}),
        ));
    };
    let wanted: Option<std::collections::BTreeSet<String>> =
        q.q.as_deref().filter(|s| !s.trim().is_empty()).map(|text| {
            store
                .search(text, 100_000)
                .into_iter()
                .map(|h| h.summary.id)
                .collect()
        });
    let lower = |s: &Option<String>| {
        s.as_deref()
            .map(str::to_lowercase)
            .filter(|s| !s.is_empty())
    };
    let (mode, provider, strategy, status) = (
        lower(&q.mode),
        lower(&q.provider),
        lower(&q.strategy),
        lower(&q.status),
    );
    let all: Vec<_> = store
        .list_details()
        .into_iter()
        .filter(|d| wanted.as_ref().is_none_or(|w| w.contains(&d.summary.id)))
        .filter(|d| {
            mode.as_ref()
                .is_none_or(|m| d.summary.mode.as_deref() == Some(m.as_str()))
        })
        .filter(|d| {
            strategy
                .as_ref()
                .is_none_or(|s| d.strategy.as_deref() == Some(s.as_str()))
        })
        .filter(|d| {
            status
                .as_ref()
                .is_none_or(|s| d.summary.status.as_str() == s)
        })
        .filter(|d| {
            provider.as_ref().is_none_or(|p| {
                d.participants
                    .iter()
                    .chain(std::iter::once(&d.summary.coordinator))
                    .any(|a| a.to_lowercase() == *p)
            })
        })
        .filter(|d| q.from.is_none_or(|f| d.summary.created_at_ms >= f))
        .filter(|d| q.to.is_none_or(|t| d.summary.created_at_ms <= t))
        .collect();
    let total = all.len();
    let items: Vec<_> = all.into_iter().skip(offset).take(limit).collect();
    Ok(Json(
        json!({"items": items, "total": total, "limit": limit, "offset": offset, "running": running, "history_enabled": true}),
    ))
}

// ----------------------------------------------------------------------------------------------- feedback

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeedbackReq {
    pub rating: String,
    #[serde(default)]
    pub note: String,
}

pub async fn add_feedback(
    State(state): S,
    Path(id): Path<String>,
    Body(req): Body<FeedbackReq>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let fb = state.feedback.as_ref().ok_or_else(|| {
        ApiError::unavailable("feedback cannot be stored: the location is unknown")
    })?;
    let rating = Rating::parse(&req.rating).ok_or_else(|| {
        ApiError::validation(format!(
            "rating must be one of: {}",
            Rating::NAMES.join(", ")
        ))
        .field("rating")
    })?;
    let consultation_id = match find(&state, &id)? {
        Found::Saved(o) => o.id,
        Found::Job(j) => j
            .consultation_id()
            .ok_or_else(|| ApiError::conflict("that consultation has no id yet"))?,
    };
    let fid = fb
        .add(Feedback::new(consultation_id.clone(), rating, &req.note))
        .map_err(ApiError::from)?;
    Ok((
        StatusCode::CREATED,
        Json(
            json!({"feedback_id": fid, "consultation_id": consultation_id, "note": "Stored locally. It changes no setting, routing or ranking; you can delete it from this screen."}),
        ),
    ))
}

pub async fn list_feedback(State(state): S) -> ApiResult<Json<Value>> {
    let fb = state
        .feedback
        .as_ref()
        .ok_or_else(|| ApiError::unavailable("feedback location unknown"))?;
    Ok(Json(json!({"items": fb.list().map_err(ApiError::from)?})))
}

pub async fn delete_feedback(State(state): S, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let fb = state
        .feedback
        .as_ref()
        .ok_or_else(|| ApiError::unavailable("feedback location unknown"))?;
    if fb.delete(&id).map_err(ApiError::from)? {
        Ok(Json(json!({"deleted": id})))
    } else {
        Err(ApiError::not_found("no such feedback"))
    }
}
