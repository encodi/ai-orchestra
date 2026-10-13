//! The versioned local API. Every handler is a thin call into the same services the CLI uses.

mod characters;
mod consultations;
mod memory;
mod reports;
mod system;

use std::sync::Arc;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};

use super::assets;
use super::error::ApiError;
use super::{AppState, security};
use crate::ux::settings::{Key, Settings};

pub type S = State<Arc<AppState>>;

/// A JSON request body whose parse failures are reported in the API's own error shape.
pub struct Body<T>(pub T);

impl<St, T> axum::extract::FromRequest<St> for Body<T>
where
    T: serde::de::DeserializeOwned,
    St: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &St) -> Result<Self, ApiError> {
        match axum::Json::<T>::from_request(req, state).await {
            Ok(axum::Json(v)) => Ok(Body(v)),
            Err(e) => Err(ApiError::validation(format!(
                "the request body is not valid: {}",
                e.body_text()
            ))),
        }
    }
}

/// Query-string parameters, with parse failures reported in the API's own error shape.
pub struct Q<T>(pub T);

impl<St, T> axum::extract::FromRequestParts<St> for Q<T>
where
    T: serde::de::DeserializeOwned,
    St: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &St,
    ) -> Result<Self, ApiError> {
        match axum::extract::Query::<T>::from_request_parts(parts, state).await {
            Ok(axum::extract::Query(v)) => Ok(Q(v)),
            Err(e) => Err(ApiError::validation(format!(
                "the query string is not valid: {}",
                e.body_text()
            ))),
        }
    }
}

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/health", get(system::health))
        .route("/api/v1/capabilities", get(system::capabilities))
        .route(
            "/api/v1/settings",
            get(system::get_settings).put(system::put_settings),
        )
        .route(
            "/api/v1/ui-preferences",
            get(system::get_ui).put(system::put_ui),
        )
        .route("/api/v1/providers", get(system::providers))
        .route(
            "/api/v1/characters",
            get(characters::get).put(characters::put),
        )
        .route(
            "/api/v1/consultations",
            post(consultations::create).get(consultations::list),
        )
        .route(
            "/api/v1/consultations/{id}",
            get(consultations::get_one).delete(consultations::remove),
        )
        .route(
            "/api/v1/consultations/{id}/events",
            get(consultations::events),
        )
        .route(
            "/api/v1/consultations/{id}/cancel",
            post(consultations::cancel),
        )
        .route(
            "/api/v1/consultations/{id}/theater",
            get(consultations::theater),
        )
        .route(
            "/api/v1/consultations/{id}/summary",
            post(consultations::summarize),
        )
        .route(
            "/api/v1/consultations/{id}/research",
            get(consultations::research),
        )
        .route(
            "/api/v1/consultations/{id}/feedback",
            post(consultations::add_feedback),
        )
        .route("/api/v1/feedback", get(consultations::list_feedback))
        .route(
            "/api/v1/feedback/{id}",
            delete(consultations::delete_feedback),
        )
        .route("/api/v1/sessions", get(memory::sessions))
        .route(
            "/api/v1/sessions/{id}",
            get(memory::session).delete(memory::clear_session),
        )
        .route("/api/v1/memory", get(memory::list).post(memory::add))
        .route("/api/v1/memory/export", get(memory::export))
        .route(
            "/api/v1/memory/{id}",
            get(memory::show)
                .put(memory::correct)
                .delete(memory::forget),
        )
        .route("/api/v1/context/preview", post(memory::preview_context))
        .route("/api/v1/reports", get(reports::list))
        .route("/api/v1/reports/{id}", get(reports::one))
        .route("/api/v1/analytics", get(reports::analytics))
        .route(
            "/api/v1/{*rest}",
            put(not_found_api)
                .get(not_found_api)
                .post(not_found_api)
                .delete(not_found_api),
        )
}

async fn not_found_api() -> ApiError {
    ApiError::not_found("no such API endpoint")
}

/// Re-validates every value of a `Settings` through the same parsers the CLI and the chat use, and checks
/// that named agents exist here. Nothing the browser sends is trusted to be well-formed.
pub fn validate_settings(
    state: &AppState,
    s: &Settings,
    scope: &str,
) -> Result<Settings, ApiError> {
    let mut out = Settings::default();
    for key in Key::ALL {
        if let Some(v) = s.get(key) {
            out.set(key, &v).map_err(|e| {
                ApiError::validation(e.to_string()).field(&format!("{scope}.{}", key.name()))
            })?;
        }
    }
    let names: Vec<String> = out
        .coordinator
        .iter()
        .cloned()
        .chain(out.agents.clone().unwrap_or_default())
        .collect();
    for n in names {
        state
            .orch
            .registry()
            .resolve(&n)
            .map_err(|e| ApiError::validation(e.to_string()).field(&format!("{scope}.agents")))?;
    }
    Ok(out)
}

/// Serves the embedded frontend (or a page explaining how to build it). Only embedded names are served.
pub async fn static_files(State(state): S, req: Request) -> Response {
    let path = req.uri().path().to_string();
    if path.starts_with("/api/") {
        return ApiError::not_found("no such API endpoint").into_response();
    }
    if req.method() != axum::http::Method::GET && req.method() != axum::http::Method::HEAD {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    if !assets::has_frontend() {
        return (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            assets::NOT_BUILT_PAGE,
        )
            .into_response();
    }
    let name = path.trim_start_matches('/');
    let name = if name.is_empty() { "index.html" } else { name };
    // A request is looked up in the embedded table; `..` and friends simply do not match anything.
    let (name, bytes) = match assets::get(name) {
        Some(b) => (name, b),
        None if !name.contains('.') => {
            ("index.html", assets::get("index.html").unwrap_or_default())
        }
        None => return StatusCode::NOT_FOUND.into_response(),
    };
    let mime = assets::mime_for(name);
    if name == "index.html" {
        let html = String::from_utf8_lossy(bytes).replace(assets::TOKEN_PLACEHOLDER, &state.token);
        return (
            [
                (header::CONTENT_TYPE, mime),
                (header::CACHE_CONTROL, "no-store"),
            ],
            html,
        )
            .into_response();
    }
    let cache = if name.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "public, max-age=3600"
    };
    (
        [(header::CONTENT_TYPE, mime), (header::CACHE_CONTROL, cache)],
        bytes,
    )
        .into_response()
}

pub fn token_header_name() -> &'static str {
    security::TOKEN_HEADER
}
