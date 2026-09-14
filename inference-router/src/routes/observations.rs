// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::AppState;
use crate::service_observer::CAPABILITY;
use axum::{
    Json, Router,
    extract::{ConnectInfo, Extension, Request, State},
    http::{HeaderMap, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use serde_json::json;
use std::net::SocketAddr;

const SCOPE: &str = "/internal/observations/scope";
const LEARNED: &str = "/internal/observations/egress/learned";

#[derive(Clone)]
struct VerifiedScope(String);

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

async fn authorize(State(state): State<AppState>, mut request: Request, next: Next) -> Response {
    let mut diagnostic = crate::observation_privacy::Readiness::new("observer_configuration");
    let Some(observer) = state.services.observer.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"private_observation_unavailable"})),
        )
            .into_response();
    };
    diagnostic.stage("observer_route_scope");
    let current = match state.services.requests.scope() {
        Ok(scope) => scope,
        Err(_) => {
            return (StatusCode::SERVICE_UNAVAILABLE, "Service scope unavailable").into_response();
        }
    };
    let operation = if request.uri().path() == SCOPE {
        crate::observation_privacy::Operation::Scope
    } else {
        crate::observation_privacy::Operation::Learned
    };
    diagnostic.stage("observer_route_identity");
    let authorized = if state.services.identity_valid {
        diagnostic.stage("observer_route_authorization");
        match tokio::time::timeout(
            std::time::Duration::from_secs(12),
            observer.authorized(bearer(request.headers()), &current, operation),
        )
        .await
        {
            Ok(result) => result.is_ok(),
            Err(_) => {
                diagnostic.deadline();
                false
            }
        }
    } else {
        false
    };
    if !authorized {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error":"observation_authority_unavailable"})),
        )
            .into_response();
    }
    diagnostic.stage("observer_origin");
    if let Some(allowed) = &state.services.allow_ips {
        let remote = request
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|peer| peer.0.ip());
        if remote.is_none_or(|ip| !allowed.contains(&ip)) {
            return (StatusCode::FORBIDDEN, "Observation origin is not allowed").into_response();
        }
    }
    diagnostic.stage("observer_scope_current");
    if !state
        .services
        .requests
        .scope()
        .is_ok_and(|scope| scope.id == current.id)
    {
        return (StatusCode::CONFLICT, Json(json!({"error":"stale_scope"}))).into_response();
    }
    request.extensions_mut().insert(VerifiedScope(current.id));
    diagnostic.finish();
    next.run(request).await
}

async fn scope(
    State(state): State<AppState>,
    Extension(verified): Extension<VerifiedScope>,
) -> Response {
    match state.services.requests.scope() {
        Ok(scope) => {
            if scope.id != verified.0 {
                return (StatusCode::CONFLICT, Json(json!({"error":"stale_scope"})))
                    .into_response();
            }
            Json(json!({"capability":CAPABILITY,"privacy_verifier":crate::observation_privacy::CAPABILITY,
                "scope_id":scope.id,"identity":scope.identity}))
                .into_response()
        }
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "Service scope unavailable").into_response(),
    }
}

async fn learned(
    State(state): State<AppState>,
    Extension(verified): Extension<VerifiedScope>,
    headers: HeaderMap,
) -> Response {
    let current = match state.services.requests.scope() {
        Ok(scope) => scope,
        Err(_) => {
            return (StatusCode::SERVICE_UNAVAILABLE, "Service scope unavailable").into_response();
        }
    };
    if current.id != verified.0
        || headers
            .get("x-kars-service-scope")
            .and_then(|value| value.to_str().ok())
            != Some(current.id.as_str())
    {
        return (StatusCode::CONFLICT, Json(json!({"error":"stale_scope"}))).into_response();
    }
    let mut value = super::egress::learned_projection(&state).await;
    if !state
        .services
        .requests
        .scope()
        .is_ok_and(|scope| scope.id == current.id)
    {
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
    if state.services.observer.as_ref().is_some_and(|observer| {
        request
            .headers()
            .get_all("authorization")
            .iter()
            .any(|value| {
                observer.recognizes(
                    value
                        .to_str()
                        .ok()
                        .and_then(|value| value.strip_prefix("Bearer ")),
                )
            })
    }) && (request.method() != Method::GET || ![SCOPE, LEARNED].contains(&request.uri().path()))
    {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error":"observation_token_is_read_only"})),
        )
            .into_response();
    }
    next.run(request).await
}
