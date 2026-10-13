//! One structured error shape for the whole API, and the mapping from the engine's errors.
//!
//! `{"error": {"kind": "...", "message": "...", "field": "..."}}` - the kinds the interface can tell
//! apart: `validation` (a request that cannot be honoured, nothing was started), `forbidden` (missing or
//! wrong token, host or origin), `not_found`, `conflict`, `busy`, `provider_auth`, `provider_quota`,
//! `provider_timeout`, `provider_missing`, `provider_failure`, `cancelled`, `partial` (a result exists but
//! something failed), `unavailable` (a service is switched off or unknown) and `internal`.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use crate::error::OrchestraError;
use crate::store::redact_secrets;

#[derive(Debug, Clone, Serialize)]
pub struct ErrorBody {
    pub kind: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: StatusCode,
    pub body: ErrorBody,
}

impl ApiError {
    pub fn new(status: StatusCode, kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            body: ErrorBody {
                kind,
                message: redact_secrets(&message.into()),
                field: None,
            },
        }
    }

    pub fn validation(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, "validation", message)
    }

    pub fn field(mut self, field: &str) -> Self {
        self.body.field = Some(field.to_string());
        self
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "conflict", message)
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, "unavailable", message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(serde_json::json!({ "error": self.body }))).into_response()
    }
}

/// The kind a provider or engine error is shown as.
pub fn kind_of(e: &OrchestraError) -> &'static str {
    match e {
        OrchestraError::Auth { .. } => "provider_auth",
        OrchestraError::Quota { .. } => "provider_quota",
        OrchestraError::Timeout { .. } | OrchestraError::DebateTimeout { .. } => "provider_timeout",
        OrchestraError::BinaryNotFound { .. } | OrchestraError::ProviderDisabled { .. } => {
            "provider_missing"
        }
        OrchestraError::Cancelled { .. } => "cancelled",
        OrchestraError::Config(_)
        | OrchestraError::UnknownProvider { .. }
        | OrchestraError::PromptTooLarge { .. }
        | OrchestraError::NestedInvocation { .. } => "validation",
        OrchestraError::History(_) => "not_found",
        OrchestraError::Memory(_) => "validation",
        _ => "provider_failure",
    }
}

impl From<OrchestraError> for ApiError {
    fn from(e: OrchestraError) -> Self {
        let kind = kind_of(&e);
        let status = match kind {
            "validation" => StatusCode::UNPROCESSABLE_ENTITY,
            "not_found" => StatusCode::NOT_FOUND,
            "provider_quota" => StatusCode::TOO_MANY_REQUESTS,
            "provider_timeout" => StatusCode::GATEWAY_TIMEOUT,
            "cancelled" => StatusCode::CONFLICT,
            "provider_auth" | "provider_missing" | "provider_failure" => StatusCode::BAD_GATEWAY,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        Self::new(status, kind, e.to_string())
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
