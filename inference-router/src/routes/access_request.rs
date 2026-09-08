// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::AppState;
use crate::access_request::{Error, Request, Status};
use axum::{
    Json, Router,
    extract::{ConnectInfo, DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::json;
use std::{net::SocketAddr, time::Duration};

pub fn routes(state: AppState) -> Router<AppState> {
    let agent = Router::new()
        .route("/v1/access-request", post(create))
        .route("/v1/access-requests", get(list_agent))
        .route("/v1/access-requests/{id}/cancel", post(cancel))
        .route("/v1/access-requests/{id}/wait", get(wait))
        .merge(super::task_telemetry::routes())
        .route_layer(middleware::from_fn_with_state(state.clone(), agent_auth))
        .layer(tower::limit::ConcurrencyLimitLayer::new(24));
    let admin = Router::new()
        .route("/internal/access-requests", get(inspect))
        .route("/internal/access-requests/reset", post(reset))
        .route("/internal/access-requests/decision", post(decide))
        .route_layer(middleware::from_fn_with_state(state, control_auth))
        .layer(tower::limit::ConcurrencyLimitLayer::new(8));
    agent.merge(admin).layer(DefaultBodyLimit::max(8192))
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("authorization")
        .and_then(|header| header.to_str().ok())
        .and_then(|header| header.strip_prefix("Bearer "))
}

async fn control_auth(
    State(state): State<AppState>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    if !state.services.identity_valid {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "Service identity is unavailable",
        )
            .into_response();
    }
    if !state.services.control_configured() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "Service control credential is not configured",
        )
            .into_response();
    }
    if !state.services.control_authorized(bearer(request.headers())) {
        return (
            StatusCode::UNAUTHORIZED,
            "Service control credential required; agent admin credentials are not accepted",
        )
            .into_response();
    }
    if let Some(allowed) = &state.services.allow_ips {
        let remote = request
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|info| info.0.ip());
        if remote.is_none_or(|remote| !allowed.contains(&remote)) {
            return (StatusCode::FORBIDDEN, "Control origin is not allowed").into_response();
        }
    }
    next.run(request).await
}

async fn agent_auth(
    State(state): State<AppState>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    if !state.services.identity_valid {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "Service identity is unavailable",
        )
            .into_response();
    }
    let local = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .is_some_and(|info| info.0.ip().is_loopback());
    if !local && !state.services.control_authorized(bearer(request.headers())) {
        return (
            StatusCode::UNAUTHORIZED,
            "Same-router agent or service control authentication required",
        )
            .into_response();
    }
    if !local && let Some(allowed) = &state.services.allow_ips {
        let remote = request
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|info| info.0.ip());
        if remote.is_none_or(|ip| !allowed.contains(&ip)) {
            return (StatusCode::FORBIDDEN, "Control origin is not allowed").into_response();
        }
    }
    next.run(request).await
}

pub(super) fn error(error: Error) -> Response {
    let (status, code) = match error {
        Error::Invalid => (StatusCode::BAD_REQUEST, "invalid_request"),
        Error::StaleScope => (StatusCode::CONFLICT, "stale_scope"),
        Error::Full => (StatusCode::TOO_MANY_REQUESTS, "capacity_exhausted"),
        Error::RateLimited => (StatusCode::TOO_MANY_REQUESTS, "rate_limited"),
        Error::Missing => (StatusCode::NOT_FOUND, "request_not_found"),
        Error::Expired => (StatusCode::GONE, "request_expired"),
        Error::WaitTimeout => (StatusCode::REQUEST_TIMEOUT, "wait_timed_out"),
        Error::Terminal => (StatusCode::CONFLICT, "request_terminal"),
        Error::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, "service_unavailable"),
    };
    (status, Json(json!({"error":code}))).into_response()
}
pub(super) fn scope_header(headers: &HeaderMap) -> Result<&str, Error> {
    headers
        .get("x-kars-service-scope")
        .and_then(|header| header.to_str().ok())
        .filter(|id| id.len() <= 128)
        .ok_or(Error::StaleScope)
}

async fn create(State(state): State<AppState>, Json(request): Json<Request>) -> Response {
    match state.services.requests.record(request) {
        Ok((entry, new)) => (
            StatusCode::ACCEPTED,
            Json(json!({"status":"queued","new":new,
            "kind":entry.kind,"target":entry.target,"request":entry,"enforcement_changed":false})),
        )
            .into_response(),
        Err(err) => error(err),
    }
}
async fn list_agent(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let scope = match state.services.requests.scope() {
        Ok(scope) => scope,
        Err(err) => return error(err),
    };
    if let Some(requested) = headers.get("x-kars-service-scope")
        && requested.to_str().ok() != Some(scope.id.as_str())
    {
        return error(Error::StaleScope);
    }
    match state.services.requests.snapshot(&scope.id) {
        Ok(entries) => Json(json!({"scope":scope,"requests":entries,"enforcement_changed":false}))
            .into_response(),
        Err(err) => error(err),
    }
}
async fn inspect(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let scope = match state.services.requests.scope() {
        Ok(scope) => scope,
        Err(err) => return error(err),
    };
    if let Some(requested) = headers.get("x-kars-service-scope")
        && requested.to_str().ok() != Some(scope.id.as_str())
    {
        return error(Error::StaleScope);
    }
    match state.services.requests.snapshot(&scope.id) {
        Ok(entries) => Json(
            json!({"schema_version":1,"scope":scope,"sandbox":state.sandbox_name.as_str(),
            "count":entries.len(),"entries":entries,"enforcement_changed":false}),
        )
        .into_response(),
        Err(err) => error(err),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopeBody {
    scope_id: String,
}
async fn cancel(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<ScopeBody>,
) -> Response {
    match state
        .services
        .requests
        .transition(&body.scope_id, &id, Status::Cancelled)
    {
        Ok(entry) => Json(entry).into_response(),
        Err(err) => error(err),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reset {
    scope_id: String,
    assignment_id: Option<String>,
}
async fn reset(State(state): State<AppState>, Json(body): Json<Reset>) -> Response {
    match state.services.reset(&body.scope_id, body.assignment_id) {
        Ok((scope, cleared)) => {
            Json(json!({"scope":scope,"cleared":cleared,"enforcement_changed":false}))
                .into_response()
        }
        Err(err) => error(err),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Decision {
    scope_id: String,
    request_id: String,
    verdict: Status,
}
async fn decide(State(state): State<AppState>, Json(body): Json<Decision>) -> Response {
    if !matches!(body.verdict, Status::Approved | Status::Denied) {
        return error(Error::Invalid);
    }
    match state
        .services
        .requests
        .transition(&body.scope_id, &body.request_id, body.verdict)
    {
        Ok(entry) => Json(json!({"request":entry,"enforcement_changed":false})).into_response(),
        Err(err) => error(err),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wait {
    scope_id: String,
    #[serde(default = "default_wait")]
    timeout_ms: u64,
}
fn default_wait() -> u64 {
    30_000
}
async fn wait(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<Wait>,
) -> Response {
    if query.timeout_ms == 0 || query.timeout_ms > 300_000 {
        return error(Error::Invalid);
    }
    match state
        .services
        .wait_for_decision(
            &query.scope_id,
            &id,
            Duration::from_millis(query.timeout_ms),
        )
        .await
    {
        Ok(entry) => Json(entry).into_response(),
        Err(err) => error(err),
    }
}
