// Copyright (c) Pal Lakatos-Toth.
// kars Bridge BFF — typed error handling.
//
// Errors map to HTTP responses with a stable JSON shape so the web app can
// render them consistently. Internal detail is logged, never leaked to the
// browser.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// Application error type returned by route handlers.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// The requested resource does not exist.
    #[error("not found")]
    NotFound,
    /// No cluster connection is configured — the BFF is running web-only.
    #[error("cluster unavailable")]
    ClusterUnavailable,
    /// An upstream dependency (the cluster API) failed. The detail is logged
    /// and a sanitized message is returned to the browser.
    #[error("upstream error: {0}")]
    Upstream(String),
    /// The request was rejected by the cluster's admission/validation rules
    /// (e.g. a CEL rule on the CRD). The message is safe to show the user —
    /// it is the API server's own validation message, not internal detail.
    #[error("rejected: {0}")]
    Rejected(String),
    /// The request was malformed (missing/invalid input). The message is safe
    /// to show the user.
    #[error("bad request: {0}")]
    BadRequest(String),
    /// Optimistic-concurrency or already-decided conflict.
    #[error("conflict: {0}")]
    Conflict(String),
    /// A confirmed, UID-bound source write may be reviewed for binding-only continuation.
    #[error("conflict: explicitly refresh and review credential metadata before resubmitting")]
    CredentialConflict(Box<crate::routes::credential_review::CredentialContinuation>),
    /// Authenticated principal lacks the required persona/authority.
    #[error("forbidden: {0}")]
    Forbidden(String),
    /// An unexpected internal error.
    #[error("internal error")]
    Internal(#[from] anyhow::Error),
}

impl AppError {
    fn status(&self) -> StatusCode {
        match self {
            AppError::NotFound => StatusCode::NOT_FOUND,
            AppError::ClusterUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            AppError::Upstream(_) => StatusCode::BAD_GATEWAY,
            AppError::Rejected(_) => StatusCode::UNPROCESSABLE_ENTITY,
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AppError::Conflict(_) => StatusCode::CONFLICT,
            AppError::CredentialConflict(_) => StatusCode::CONFLICT,
            AppError::Forbidden(_) => StatusCode::FORBIDDEN,
            AppError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Stable, client-safe error code.
    fn code(&self) -> &'static str {
        match self {
            AppError::NotFound => "not_found",
            AppError::ClusterUnavailable => "cluster_unavailable",
            AppError::Upstream(_) => "upstream_error",
            AppError::Rejected(_) => "rejected",
            AppError::BadRequest(_) => "bad_request",
            AppError::Conflict(_) => "conflict",
            AppError::CredentialConflict(_) => "conflict",
            AppError::Forbidden(_) => "forbidden",
            AppError::Internal(_) => "internal_error",
        }
    }

    /// Client-safe message. Upstream/internal detail is logged, not returned;
    /// a `Rejected` message is the API server's own validation text and is
    /// intentionally surfaced so the user can correct their input.
    fn client_message(&self) -> String {
        match self {
            AppError::Internal(_) => "internal error".to_string(),
            AppError::Upstream(_) => "upstream dependency failed".to_string(),
            AppError::Rejected(msg) => msg.clone(),
            other => other.to_string(),
        }
    }
}

#[derive(Serialize)]
struct ErrorBody {
    error: ErrorDetail,
}

#[derive(Serialize)]
struct ErrorDetail {
    code: &'static str,
    message: String,
    #[serde(
        rename = "credentialContinuation",
        skip_serializing_if = "Option::is_none"
    )]
    credential_continuation: Option<crate::routes::credential_review::CredentialContinuation>,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status();
        if status.is_server_error() {
            tracing::error!(error = %self, "request failed");
        } else {
            tracing::debug!(error = %self, "request rejected");
        }
        let body = ErrorBody {
            error: ErrorDetail {
                code: self.code(),
                message: self.client_message(),
                credential_continuation: match &self {
                    AppError::CredentialConflict(continuation) => Some((**continuation).clone()),
                    _ => None,
                },
            },
        };
        (status, Json(body)).into_response()
    }
}

/// Convenience result alias for handlers.
pub type AppResult<T> = Result<T, AppError>;
