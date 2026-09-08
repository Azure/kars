// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::AppState;
use crate::service_observer::CAPABILITY;
use axum::{
    Json, Router,
    extract::{ConnectInfo, Request, State},
    http::{HeaderMap, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use serde_json::json;
use std::net::SocketAddr;

const SCOPE: &str = "/internal/observations/scope";
const LEARNED: &str = "/internal/observations/egress/learned";

#[cfg(test)]
#[path = "observation_tests.rs"]
mod tests;

fn bearer(headers: &HeaderMap) -> Option<&str> {
    if headers.get_all("authorization").iter().count() != 1 {
        return None;
    }
    headers
        .get("authorization")?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route(SCOPE, get(scope))
        .route(LEARNED, get(learned))
        .route_layer(middleware::from_fn_with_state(state, authorize))
        .layer(tower::limit::ConcurrencyLimitLayer::new(8))
}

async fn authorize(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let Some(observer) = state.services.observer.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"private_observation_unavailable"})),
        )
            .into_response();
    };
    let current = match state.services.requests.scope() {
        Ok(scope) => scope,
        Err(_) => {
            return (StatusCode::SERVICE_UNAVAILABLE, "Service scope unavailable").into_response();
        }
    };
    if !state.services.identity_valid
        || observer
            .authorized(bearer(request.headers()), &current)
            .await
            .is_err()
    {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error":"observation_authority_unavailable"})),
        )
            .into_response();
    }
    if let Some(allowed) = &state.services.allow_ips {
        let remote = request
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|peer| peer.0.ip());
        if remote.is_none_or(|ip| !allowed.contains(&ip)) {
            return (StatusCode::FORBIDDEN, "Observation origin is not allowed").into_response();
        }
    }
    next.run(request).await
}

async fn scope(State(state): State<AppState>) -> Response {
    match state.services.requests.scope() {
        Ok(scope) => {
            Json(json!({"capability":CAPABILITY,"scope_id":scope.id,"identity":scope.identity}))
                .into_response()
        }
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "Service scope unavailable").into_response(),
    }
}

async fn learned(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let current = match state.services.requests.scope() {
        Ok(scope) => scope,
        Err(_) => {
            return (StatusCode::SERVICE_UNAVAILABLE, "Service scope unavailable").into_response();
        }
    };
    if headers
        .get("x-kars-service-scope")
        .and_then(|value| value.to_str().ok())
        != Some(current.id.as_str())
    {
        return (StatusCode::CONFLICT, Json(json!({"error":"stale_scope"}))).into_response();
    }
    let mut value = super::egress::learned_projection(&state).await;
    if !state.services.requests.scope().is_ok_and(|scope| scope.id == current.id) {
        return (StatusCode::CONFLICT, Json(json!({"error":"stale_scope"}))).into_response();
    }
    value["capability"] = CAPABILITY.into();
    value["scope_id"] = current.id.into();
    Json(value).into_response()
}

/// The observation token cannot become an admin credential, even on a legacy
/// route that otherwise permits loopback callers.
pub async fn purpose_boundary(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    if state
        .services
        .observer
        .as_ref()
        .is_some_and(|observer| observer.recognizes(bearer(request.headers())))
        && (request.method() != Method::GET || ![SCOPE, LEARNED].contains(&request.uri().path()))
    {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error":"observation_token_is_read_only"})),
        )
            .into_response();
    }
    next.run(request).await
}
