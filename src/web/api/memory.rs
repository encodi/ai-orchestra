//! Sessions and memory (Milestone 7) through the browser. Same service, same rules:
//! persistent memory is only ever created by an explicit, confirmed request; records marked sensitive are
//! masked unless asked for; sessions and project memory never cross projects.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use super::consultations::load_layers;
use super::{Body, Q, S, validate_settings};
use crate::memory::model::{MemoryKind, MemoryRecord, MemoryScope};
use crate::memory::service::{AddInput, MemoryRequest, MemoryService, ProjectRef};
use crate::ux::settings::Settings;
use crate::web::AppState;
use crate::web::error::{ApiError, ApiResult};

fn svc(state: &AppState) -> ApiResult<(&std::sync::Arc<MemoryService>, ProjectRef)> {
    let s = state.orch.memory().ok_or_else(|| {
        ApiError::unavailable(
            "memory is not available in this server (switched off, or its location is unknown)",
        )
    })?;
    Ok((
        s,
        state
            .project
            .clone()
            .unwrap_or_else(|| s.default_project(None)),
    ))
}

/// A record as the browser sees it: sensitive text is masked unless `reveal` was asked for.
fn view(r: &MemoryRecord, reveal: bool) -> Value {
    let mut v = serde_json::to_value(r).unwrap_or(Value::Null);
    if r.sensitive
        && !reveal
        && let Some(o) = v.as_object_mut()
    {
        o.insert(
            "text".into(),
            json!("[marked sensitive: hidden. It is never sent to an agent.]"),
        );
        o.insert("short".into(), Value::Null);
        o.insert("masked".into(), json!(true));
    }
    v
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListQuery {
    pub q: Option<String>,
    pub scope: Option<String>,
    pub kind: Option<String>,
    #[serde(default)]
    pub all_versions: bool,
    pub limit: Option<usize>,
    #[serde(default)]
    pub reveal: bool,
}

pub async fn list(State(state): S, Q(q): Q<ListQuery>) -> ApiResult<Json<Value>> {
    let (svc, project) = svc(&state)?;
    let limit = q.limit.unwrap_or(30).clamp(1, 100);
    let scope = q
        .scope
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(str::parse::<MemoryScope>)
        .transpose()
        .map_err(|e| ApiError::validation(e.to_string()).field("scope"))?;
    let kind = q
        .kind
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(str::parse::<MemoryKind>)
        .transpose()
        .map_err(|e| ApiError::validation(e.to_string()).field("kind"))?;
    let rows: Vec<Value> = match q.q.as_deref().filter(|s| !s.trim().is_empty()) {
        Some(text) => svc
            .search(&project, text, scope, q.all_versions, limit)
            .await?
            .into_iter()
            .filter(|h| kind.is_none_or(|k| h.record.kind == k))
            .map(|h| {
                let mut v = view(&h.record, q.reveal);
                v["score"] = json!(h.score);
                v
            })
            .collect(),
        None => svc
            .list(&project, scope, kind, q.all_versions, limit)
            .await?
            .iter()
            .map(|r| view(r, q.reveal))
            .collect(),
    };
    Ok(Json(
        json!({"items": rows, "project": project, "note": "Scope is shown on every record. Records marked sensitive are masked here and are never sent to an agent."}),
    ))
}

pub async fn show(
    State(state): S,
    Path(id): Path<String>,
    Q(q): Q<RevealQuery>,
) -> ApiResult<Json<Value>> {
    let (svc, project) = svc(&state)?;
    let d = svc.show(&project, &id).await?;
    Ok(Json(json!({
        "record": view(&d.record, q.reveal), "links": d.links, "evidence": d.evidence,
        "original_answer": if d.record.sensitive && !q.reveal { None } else { d.original_answer },
    })))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevealQuery {
    #[serde(default)]
    pub reveal: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddReq {
    pub text: String,
    /// `project` (default), `global` or `session`.
    pub scope: Option<String>,
    pub kind: Option<String>,
    pub topic: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub session: Option<String>,
    pub expires_days: Option<u32>,
    #[serde(default)]
    pub allow_sensitive: bool,
    /// The explicit authorization to keep this. Nothing is remembered without it.
    #[serde(default)]
    pub confirm: bool,
}

pub async fn add(State(state): S, Body(req): Body<AddReq>) -> ApiResult<(StatusCode, Json<Value>)> {
    let (svc, project) = svc(&state)?;
    if !req.confirm {
        return Err(ApiError::validation(
            "remembering something needs your explicit confirmation (confirm = true)",
        )
        .field("confirm"));
    }
    if req.text.trim().is_empty() || req.text.len() > 4000 {
        return Err(
            ApiError::validation("the text must be between 1 and 4000 bytes").field("text"),
        );
    }
    let scope: MemoryScope = req.scope.as_deref().unwrap_or("project").parse().map_err(
        |e: crate::error::OrchestraError| ApiError::validation(e.to_string()).field("scope"),
    )?;
    let kind: MemoryKind = req.kind.as_deref().unwrap_or("note").parse().map_err(
        |e: crate::error::OrchestraError| ApiError::validation(e.to_string()).field("kind"),
    )?;
    let (out, rec) = svc
        .add(AddInput {
            project,
            scope,
            kind,
            text: req.text,
            topic: req.topic,
            tags: req.tags.into_iter().take(8).collect(),
            session: req.session,
            allow_sensitive: req.allow_sensitive,
            expires_days: req.expires_days,
        })
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(
            json!({"id": out.id, "duplicate": out.duplicate, "replaced": out.superseded, "record": view(&rec, false)}),
        ),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorrectReq {
    pub text: String,
    #[serde(default)]
    pub confirm: bool,
}

pub async fn correct(
    State(state): S,
    Path(id): Path<String>,
    Body(req): Body<CorrectReq>,
) -> ApiResult<Json<Value>> {
    let (svc, project) = svc(&state)?;
    if !req.confirm {
        return Err(
            ApiError::validation("correcting a memory needs confirm = true").field("confirm"),
        );
    }
    let out = svc.correct(&project, &id, &req.text).await?;
    Ok(Json(
        json!({"id": out.id, "replaces": id, "note": "The old version is kept, marked as replaced, and is no longer recalled."}),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfirmQuery {
    #[serde(default)]
    pub confirm: bool,
}

pub async fn forget(
    State(state): S,
    Path(id): Path<String>,
    Q(q): Q<ConfirmQuery>,
) -> ApiResult<Json<Value>> {
    let (svc, project) = svc(&state)?;
    if !q.confirm {
        return Err(ApiError::validation("deleting a memory needs confirm=true").field("confirm"));
    }
    let r = svc.forget(&project, &id).await?;
    Ok(Json(
        json!({"deleted": r.id, "scope": r.scope, "kind": r.kind, "note": "Its content, index entry and links were deleted."}),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportQuery {
    #[serde(default)]
    pub all_projects: bool,
}

pub async fn export(State(state): S, Q(q): Q<ExportQuery>) -> ApiResult<Json<Value>> {
    let (svc, project) = svc(&state)?;
    let bundle = svc
        .export(if q.all_projects { None } else { Some(&project) })
        .await?;
    Ok(Json(json!(bundle)))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionsQuery {
    pub limit: Option<usize>,
}

pub async fn sessions(State(state): S, Q(q): Q<SessionsQuery>) -> ApiResult<Json<Value>> {
    let (svc, project) = svc(&state)?;
    let rows = svc
        .sessions(&project, q.limit.unwrap_or(30).clamp(1, 200))
        .await?;
    Ok(Json(
        json!({"items": rows, "project": project, "note": "These are AI Orchestra's own sessions. No provider CLI session is resumed."}),
    ))
}

pub async fn session(State(state): S, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let (svc, project) = svc(&state)?;
    let d = svc.session_detail(&project, Some(&id)).await?;
    Ok(Json(json!({
        "session": d.session, "consultations": d.consultations, "summaries": d.summaries,
        "memories": d.memories.iter().map(|m| view(m, false)).collect::<Vec<_>>(),
    })))
}

pub async fn clear_session(
    State(state): S,
    Path(id): Path<String>,
    Q(q): Q<ConfirmQuery>,
) -> ApiResult<Json<Value>> {
    let (svc, project) = svc(&state)?;
    if !q.confirm {
        return Err(ApiError::validation("clearing a session needs confirm=true").field("confirm"));
    }
    let (sid, n) = svc.clear_session(&project, Some(&id)).await?;
    Ok(Json(json!({"cleared": sid, "memories_deleted": n})))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreviewReq {
    pub question: String,
    #[serde(default)]
    pub session_settings: Settings,
    #[serde(default)]
    pub settings: Settings,
    pub session: Option<String>,
    #[serde(default)]
    pub new_session: bool,
}

/// What a consultation of this question would recall from memory (nothing is sent, run or written).
pub async fn preview_context(
    State(state): S,
    Body(req): Body<PreviewReq>,
) -> ApiResult<Json<Value>> {
    let (svc, project) = svc(&state)?;
    if req.question.trim().is_empty() {
        return Err(ApiError::validation("the question is empty").field("question"));
    }
    let session_settings = validate_settings(&state, &req.session_settings, "session_settings")?;
    let request = validate_settings(&state, &req.settings, "settings")?;
    let resolved = load_layers(&state, session_settings)?.resolve(&request);
    let mode = resolved.memory_mode(&state.config)?;
    let mut m = MemoryRequest::new(mode);
    m.session = req.session;
    m.new_session = req.new_session;
    m.context_budget_tokens = resolved.settings.context_budget;
    m.project = Some(project);
    let _ = svc;
    let options = resolved.to_options(req.question, &state.config, Some(m))?;
    match state.orch.preview_context(&options).await? {
        Some(p) => Ok(Json(
            json!({"report": p.report, "block": p.text, "session": p.session.map(|s| s.id), "note": "Nothing was sent to an agent."}),
        )),
        None => Ok(Json(
            json!({"report": null, "block": "", "note": "Memory is off for this request, so nothing would be recalled."}),
        )),
    }
}
