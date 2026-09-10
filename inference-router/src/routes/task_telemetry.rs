// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{
    AppState,
    access_request::{error, scope_header},
};
use crate::access_request::Error;
use axum::{
    Json, Router,
    extract::{Query, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::json;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/telemetry/cursor", get(cursor))
        .route("/telemetry/trace", get(trace))
        .route("/telemetry/tool", post(tool))
        .route("/telemetry/budget", get(budget))
}
async fn cursor(State(state): State<AppState>) -> Response {
    let (scope_id, cursor) = state.services.telemetry.cursor();
    Json(json!({"scope_id":scope_id,"cursor":cursor,"durable":false})).into_response()
}
#[derive(Deserialize)]
struct Since {
    #[serde(default)]
    since: u64,
}
async fn trace(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<Since>,
) -> Response {
    let scope = match scope_header(&headers) {
        Ok(scope) => scope,
        Err(err) => return error(err),
    };
    match state.services.telemetry.snapshot(scope, query.since) {
        Some(trace) => Json(trace).into_response(),
        None => error(Error::StaleScope),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Tool {
    scope_id: String,
    call_id: String,
    ok: bool,
    latency_ms: u64,
}
async fn tool(State(state): State<AppState>, Json(body): Json<Tool>) -> Response {
    if state.services.telemetry.complete_harness_tool(
        &body.scope_id,
        &body.call_id,
        body.ok,
        body.latency_ms,
    ) {
        Json(json!({"recorded":true,"source":"harness-reported","enforcement_changed":false}))
            .into_response()
    } else {
        error(Error::Terminal)
    }
}
async fn budget(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let scope = match scope_header(&headers) {
        Ok(scope) => scope,
        Err(err) => return error(err),
    };
    if state.services.telemetry.cursor().0 != scope {
        return error(Error::StaleScope);
    }
    let (daily, monthly) = state.budget.get_usage(&state.sandbox_name).await;
    let policy = crate::inference_policy_loader::current_snapshot(&state.inference_policy).await;
    let daily_limit = policy
        .daily_tokens
        .unwrap_or(state.config.token_budget_daily);
    Json(
        json!({"scope_id":scope,"daily_observed_tokens":daily,"monthly_observed_tokens":monthly,
        "daily_limit":(daily_limit>0).then_some(daily_limit),
        "monthly_limit":policy.monthly_tokens.filter(|limit|*limit>0),
        "shared_task_budget_enforced":false,"coverage":"existing-per-sandbox-counter-only",
        "assignment_reset_resets_budget":false}),
    )
    .into_response()
}
