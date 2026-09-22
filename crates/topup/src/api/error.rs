//! Stable API error responses.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use utoipa::ToSchema;

/// Machine-readable error envelope returned by every API failure.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct ErrorResponse {
    /// Error details.
    pub error: ErrorDetail,
}

/// Stable error fields safe to expose to callers.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct ErrorDetail {
    /// Stable machine-readable code.
    pub code: &'static str,
    /// Human-readable summary without internal details.
    pub message: String,
    /// Work package that owns a deliberately deferred handler.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work_package: Option<&'static str>,
}

/// API failure with an HTTP status and stable public body.
#[derive(Clone, Debug)]
pub struct ApiError {
    status: StatusCode,
    detail: ErrorDetail,
}

impl ApiError {
    /// Returns a request-validation failure.
    #[must_use]
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_request", message)
    }

    /// Returns an authentication failure.
    #[must_use]
    pub fn unauthorized() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "request signature verification failed",
        )
    }

    /// Returns a tenant-safe missing-resource failure.
    #[must_use]
    pub fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", "resource not found")
    }

    /// Returns a conflict caused by current persisted state.
    #[must_use]
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "conflict", message)
    }

    /// Returns a conflict for an already consumed request signature.
    #[must_use]
    pub fn signature_replayed() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "signature_replayed",
            "request signature has already been used",
        )
    }

    /// Returns a locked response for an operation blocked by a pause scope.
    #[must_use]
    pub fn paused(message: impl Into<String>) -> Self {
        Self::new(StatusCode::LOCKED, "paused", message)
    }

    /// Returns a temporary dependency or pause failure.
    #[must_use]
    pub fn service_unavailable(message: impl Into<String>) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, "unavailable", message)
    }

    /// Returns a deliberately deferred work-package response.
    #[must_use]
    pub fn not_implemented(work_package: &'static str) -> Self {
        Self {
            status: StatusCode::NOT_IMPLEMENTED,
            detail: ErrorDetail {
                code: "not_implemented",
                message: format!("handler is owned by work package {work_package}"),
                work_package: Some(work_package),
            },
        }
    }

    /// Returns an internal failure without exposing its cause.
    #[must_use]
    pub fn internal() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "request could not be completed",
        )
    }

    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            detail: ErrorDetail {
                code,
                message: message.into(),
                work_package: None,
            },
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(ErrorResponse { error: self.detail })).into_response()
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(error: sqlx::Error) -> Self {
        tracing::error!(%error, "database operation failed");
        Self::internal()
    }
}
