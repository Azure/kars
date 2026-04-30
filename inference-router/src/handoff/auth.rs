// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Handoff endpoint auth middleware.
//!
//! Extracted from `handoff/mod.rs` per §4.2 hotspot decomposition.
//! Three middleware variants: full (admin + handoff token, no
//! localhost bypass), init (admin only, no localhost bypass — handoff
//! token does not yet exist), status (admin token required for
//! non-localhost, localhost bypass allowed because status is read-only).
//!
//! Behaviour change from the in-`mod.rs` version: **none.** Same
//! header names, same constant-time-equal check via
//! [`super::constant_time_eq`], same audit logging shape, same
//! `state.handoff_tokens.validate(...)` call, same status codes.

use axum::{
    extract::State,
    http::{HeaderMap, Request, StatusCode},
    middleware::Next,
    response::IntoResponse,
};

use super::constant_time_eq;
use crate::routes::AppState;

/// Authentication middleware for handoff endpoints.
///
/// **CRITICAL SECURITY**: Unlike `admin_auth_middleware`, this middleware:
/// 1. Does NOT allow localhost bypass (prompt injection protection)
/// 2. Requires BOTH admin token AND handoff token
/// 3. Validates the handoff token against the in-memory store
///
/// The handoff token exists only in CLI process memory — the agent process
/// inside the pod never sees it, preventing prompt injection attacks from
/// exfiltrating state via localhost calls.
pub async fn handoff_auth_middleware(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Request<axum::body::Body>,
    next: Next,
) -> impl IntoResponse {
    // Extract admin token from Authorization header.
    let admin_token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));

    // Verify admin token — NO localhost bypass.
    let expected_admin = match &state.admin_token {
        Some(token) => token.as_str(),
        None => {
            tracing::error!("handoff auth: no admin token configured");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Server misconfiguration: no admin token",
            )
                .into_response();
        }
    };

    match admin_token {
        Some(provided) if constant_time_eq(provided.as_bytes(), expected_admin.as_bytes()) => {}
        Some(_) => {
            tracing::warn!(
                path = %request.uri().path(),
                "handoff auth: invalid admin token"
            );
            return (StatusCode::UNAUTHORIZED, "Invalid admin token").into_response();
        }
        None => {
            tracing::warn!(
                path = %request.uri().path(),
                "handoff auth: missing admin token"
            );
            return (StatusCode::UNAUTHORIZED, "Admin token required").into_response();
        }
    }

    // Extract and verify handoff token from X-Handoff-Token header.
    let handoff_token = headers.get("x-handoff-token").and_then(|v| v.to_str().ok());

    match handoff_token {
        Some(token) => match state.handoff_tokens.validate(token).await {
            Ok(token_hash) => {
                tracing::info!(
                    path = %request.uri().path(),
                    token_hash = &token_hash[..16],
                    "handoff auth: validated"
                );
            }
            Err(e) => {
                tracing::warn!(
                    path = %request.uri().path(),
                    error = %e,
                    "handoff auth: token validation failed"
                );
                return (StatusCode::UNAUTHORIZED, format!("Handoff token: {e}")).into_response();
            }
        },
        None => {
            tracing::warn!(
                path = %request.uri().path(),
                "handoff auth: missing X-Handoff-Token header"
            );
            return (
                StatusCode::UNAUTHORIZED,
                "X-Handoff-Token header required for handoff endpoints",
            )
                .into_response();
        }
    }

    next.run(request).await.into_response()
}

/// Auth middleware for the handoff/init endpoint — requires admin token only
/// (the handoff token doesn't exist yet; this endpoint creates it).
///
/// NO localhost bypass — only the CLI should call this.
pub async fn handoff_init_auth_middleware(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Request<axum::body::Body>,
    next: Next,
) -> impl IntoResponse {
    let admin_token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));

    let expected_admin = match &state.admin_token {
        Some(token) => token.as_str(),
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Server misconfiguration: no admin token",
            )
                .into_response();
        }
    };

    match admin_token {
        Some(provided) if constant_time_eq(provided.as_bytes(), expected_admin.as_bytes()) => {}
        Some(_) => {
            tracing::warn!(
                path = %request.uri().path(),
                "handoff init auth: invalid admin token"
            );
            return (StatusCode::UNAUTHORIZED, "Invalid admin token").into_response();
        }
        None => {
            tracing::warn!(
                path = %request.uri().path(),
                "handoff init auth: missing admin token"
            );
            return (StatusCode::UNAUTHORIZED, "Admin token required").into_response();
        }
    }

    next.run(request).await.into_response()
}

/// Auth middleware for the handoff/status endpoint — admin token required,
/// but handoff token is optional (read-only, safe to query).
/// Localhost bypass IS allowed for status.
pub async fn handoff_status_auth_middleware(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Request<axum::body::Body>,
    next: Next,
) -> impl IntoResponse {
    // Allow localhost for status (read-only).
    if let Some(connect_info) = request
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        && connect_info.0.ip().is_loopback()
    {
        return next.run(request).await.into_response();
    }

    // Non-localhost: require admin token.
    let admin_token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));

    let expected_admin = match &state.admin_token {
        Some(token) => token.as_str(),
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Server misconfiguration: no admin token",
            )
                .into_response();
        }
    };

    match admin_token {
        Some(provided) if constant_time_eq(provided.as_bytes(), expected_admin.as_bytes()) => {}
        Some(_) => {
            return (StatusCode::UNAUTHORIZED, "Invalid admin token").into_response();
        }
        None => {
            return (StatusCode::UNAUTHORIZED, "Admin token required").into_response();
        }
    }

    next.run(request).await.into_response()
}
