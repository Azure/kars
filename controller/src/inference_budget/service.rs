// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use kube::Client;
use serde_json::json;
use std::sync::Arc;

use super::{
    auth,
    config::Settings,
    store::{Store, StoreError},
};
use crate::inference_budget_contract::{
    AttemptCommand, BrokerRequest, BudgetError, ReserveRequest, SessionRequest, Settlement,
};

#[derive(Clone)]
pub struct Broker {
    pub client: Client,
    pub settings: Settings,
    pub store: Store,
}

pub fn router(broker: Broker) -> Router {
    Router::new()
        .route("/v1/catalog", post(catalog))
        .route("/v1/session", post(session))
        .route("/v1/reserve", post(reserve))
        .route("/v1/begin", post(begin))
        .route("/v1/settle", post(settle))
        .layer(DefaultBodyLimit::max(32_768))
        .with_state(Arc::new(broker))
}

struct Failure(StoreError);

impl From<StoreError> for Failure {
    fn from(error: StoreError) -> Self {
        Self(error)
    }
}

impl From<BudgetError> for Failure {
    fn from(error: BudgetError) -> Self {
        Self(error.into())
    }
}

impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        let (status, code) = match &self.0 {
            StoreError::Ledger(BudgetError::Exhausted) => {
                (StatusCode::TOO_MANY_REQUESTS, "inference_budget_exhausted")
            }
            StoreError::Ledger(BudgetError::Capacity) => {
                (StatusCode::SERVICE_UNAVAILABLE, "inference_budget_capacity")
            }
            StoreError::Ledger(BudgetError::Identity | BudgetError::Authorization) => {
                (StatusCode::FORBIDDEN, "inference_budget_authority")
            }
            StoreError::Ledger(
                BudgetError::AlreadyDispatched | BudgetError::Sequence | BudgetError::Expired,
            ) => (StatusCode::CONFLICT, "inference_budget_attempt_state"),
            StoreError::Ledger(BudgetError::Contract) => {
                (StatusCode::SERVICE_UNAVAILABLE, "inference_budget_contract")
            }
            StoreError::Ledger(BudgetError::Closed) => {
                (StatusCode::FORBIDDEN, "inference_budget_closed")
            }
            StoreError::Ledger(
                BudgetError::Breach | BudgetError::Corrupt | BudgetError::Overflow,
            ) => (StatusCode::SERVICE_UNAVAILABLE, "inference_budget_frozen"),
            _ => (
                StatusCode::SERVICE_UNAVAILABLE,
                "inference_budget_unavailable",
            ),
        };
        tracing::warn!(
            budget_stage = code,
            http_status = status.as_u16(),
            "Governed inference broker operation denied"
        );
        // Deliberately neither the token, wire request, nor API response body.
        (
            status,
            Json(json!({"error": {"code": code, "message": self.0.to_string()}})),
        )
            .into_response()
    }
}

async fn catalog(
    State(broker): State<Arc<Broker>>,
    headers: HeaderMap,
    Json(request): Json<BrokerRequest<SessionRequest>>,
) -> Result<Json<serde_json::Value>, Failure> {
    let account = broker
        .store
        .read(&request.root, &request.payload.account_uid)
        .await?;
    auth::authenticate(
        &broker.client,
        &headers,
        &account,
        &request.payload.identity,
        true,
    )
    .await?;
    let catalog = broker
        .settings
        .catalog(&broker.client, chrono::Utc::now().timestamp())
        .await?;
    Ok(Json(json!({
        "catalog": catalog.catalog, "uid": catalog.uid, "resourceVersion": catalog.resource_version,
        "digest": catalog.digest, "scope": "GovernedInference",
        "moneyRequired": account.status.as_ref().and_then(|status| status.ledger.as_ref())
            .ok_or(StoreError::Missing)?.requires_price(&request.payload.identity.task_uid)?,
    })))
}

async fn session(
    State(broker): State<Arc<Broker>>,
    headers: HeaderMap,
    Json(request): Json<BrokerRequest<SessionRequest>>,
) -> Result<Json<crate::inference_budget_contract::ledger::Session>, Failure> {
    let account = broker
        .store
        .read(&request.root, &request.payload.account_uid)
        .await?;
    auth::authenticate(
        &broker.client,
        &headers,
        &account,
        &request.payload.identity,
        true,
    )
    .await?;
    let result = broker
        .store
        .transact(&request.root, &request.payload.account_uid, |ledger| {
            ledger.register_session(request.payload.identity.clone())
        })
        .await?;
    Ok(Json(result))
}

async fn reserve(
    State(broker): State<Arc<Broker>>,
    headers: HeaderMap,
    Json(request): Json<BrokerRequest<ReserveRequest>>,
) -> Result<Json<crate::inference_budget_contract::ledger::Reservation>, Failure> {
    let account = broker
        .store
        .read(&request.root, &request.payload.account_uid)
        .await?;
    auth::authenticate(
        &broker.client,
        &headers,
        &account,
        &request.payload.identity,
        true,
    )
    .await?;
    let now = chrono::Utc::now().timestamp();
    let catalog = broker.settings.catalog(&broker.client, now).await?;
    let ledger = account
        .status
        .as_ref()
        .and_then(|status| status.ledger.as_ref())
        .ok_or(StoreError::Missing)?;
    catalog.catalog.accepts_quote(
        &request.payload.quote,
        now,
        ledger.requires_price(&request.payload.identity.task_uid)?,
    )?;
    broker
        .store
        .transact(&request.root, &request.payload.account_uid, |ledger| {
            ledger.expire_undispatched(now)
        })
        .await?;
    let result = broker
        .store
        .transact(&request.root, &request.payload.account_uid, |ledger| {
            ledger.reserve(&request.payload, chrono::Utc::now().timestamp())
        })
        .await?;
    Ok(Json(result))
}

async fn begin(
    State(broker): State<Arc<Broker>>,
    headers: HeaderMap,
    Json(request): Json<BrokerRequest<AttemptCommand>>,
) -> Result<Json<crate::inference_budget_contract::ledger::Reservation>, Failure> {
    let account = broker
        .store
        .read(&request.root, &request.payload.account_uid)
        .await?;
    auth::authenticate(
        &broker.client,
        &headers,
        &account,
        &request.payload.identity,
        true,
    )
    .await?;
    let now = chrono::Utc::now().timestamp();
    let catalog = broker.settings.catalog(&broker.client, now).await?;
    let ledger = account
        .status
        .as_ref()
        .and_then(|status| status.ledger.as_ref())
        .ok_or(StoreError::Missing)?;
    let attempt = ledger
        .attempts
        .get(&request.payload.key.storage_key())
        .ok_or(BudgetError::Sequence)?;
    catalog.catalog.accepts_quote(
        &attempt.quote,
        now,
        ledger.requires_price(&request.payload.identity.task_uid)?,
    )?;
    let result = broker
        .store
        .transact(&request.root, &request.payload.account_uid, |ledger| {
            ledger.begin_dispatch(&request.payload, chrono::Utc::now().timestamp())
        })
        .await?;
    Ok(Json(result))
}

async fn settle(
    State(broker): State<Arc<Broker>>,
    headers: HeaderMap,
    Json(request): Json<BrokerRequest<Settlement>>,
) -> Result<Json<crate::inference_budget_contract::ledger::SettlementResult>, Failure> {
    let command = &request.payload.attempt;
    let account = broker
        .store
        .read(&request.root, &command.account_uid)
        .await?;
    auth::authenticate(&broker.client, &headers, &account, &command.identity, false).await?;
    // Settlement uses the accepted contract snapshot. A later tariff expiry or
    // policy change cannot erase already accepted liability.
    let result = broker
        .store
        .transact(&request.root, &command.account_uid, |ledger| {
            ledger.settle(&request.payload)
        })
        .await?;
    if result.breach {
        tracing::error!(account_uid = %command.account_uid, "Governed inference contract bound breached; account frozen");
    }
    Ok(Json(result))
}
