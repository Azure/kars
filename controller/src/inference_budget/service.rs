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
        .route("/v1/calendar/read", post(calendar_read))
        .route("/v1/calendar/commit", post(calendar_commit))
        .layer(DefaultBodyLimit::max(32_768))
        .with_state(Arc::new(broker))
}

async fn calendar_read(
    State(broker): State<Arc<Broker>>,
    headers: HeaderMap,
    Json(request): Json<crate::inference_budget_contract::calendar::Read>,
) -> Result<Response, Failure> {
    let authorization = super::calendar::authenticate(
        &broker.client,
        &broker.settings,
        &headers,
        &request.identity,
        true,
    )
    .await?;
    Ok(calendar_response(
        super::calendar::read(
            &broker.client,
            &request.identity.binding,
            &request.writer,
            &authorization,
        )
        .await?,
    ))
}

async fn calendar_commit(
    State(broker): State<Arc<Broker>>,
    headers: HeaderMap,
    Json(request): Json<crate::inference_budget_contract::calendar::Commit>,
) -> Result<Response, Failure> {
    let authorization = super::calendar::authenticate(
        &broker.client,
        &broker.settings,
        &headers,
        &request.identity,
        request.event == crate::inference_budget_contract::calendar::Event::Begin,
    )
    .await?;
    Ok(calendar_response(
        super::calendar::commit(&broker.client, &request, &authorization).await?,
    ))
}

fn calendar_response(
    observation: crate::inference_budget_contract::calendar::Observation,
) -> Response {
    use crate::inference_budget_contract::calendar::{PENDING_HEADER, POLICY_HEADER};
    let mut response = Json(observation.usage).into_response();
    response.headers_mut().insert(
        POLICY_HEADER,
        axum::http::HeaderValue::from_static(observation.policy.as_str()),
    );
    if let Some(pending) = observation.pending_requests {
        response
            .headers_mut()
            .insert(PENDING_HEADER, pending.into());
    }
    response
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
            StoreError::PrivacyPending => {
                (StatusCode::SERVICE_UNAVAILABLE, "inference_privacy_pending")
            }
            StoreError::Contention => (
                StatusCode::SERVICE_UNAVAILABLE,
                "inference_budget_contention",
            ),
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
        // StoreError contains only contract errors or fixed API stages/statuses.
        tracing::warn!(
            code,
            status = status.as_u16(),
            error = %self.0,
            "Governed inference budget request rejected"
        );
        // Deliberately neither the token, wire request, nor API response body.
        (
            status,
            Json(json!({"error": {"code": code, "message": self.0.to_string()}})),
        )
            .into_response()
    }
}

pub(super) async fn authenticated_account(
    broker: &Broker,
    headers: &HeaderMap,
    root: &crate::inference_budget_contract::RootIdentity,
    account_uid: &str,
    identity: &crate::inference_budget_contract::ExecutionIdentity,
    dispatch: bool,
) -> Result<super::account::KarsBudgetAccount, StoreError> {
    let account = broker.store.read(root, account_uid).await?;
    let ledger = Store::ledger(&account)?;
    if ledger.nodes.contains_key(&identity.task_uid)
        && (dispatch || ledger.sessions.contains_key(&identity.pod_uid))
    {
        auth::authenticate(&broker.client, headers, &account, identity, dispatch).await?;
        return Ok(account);
    }
    let (sandbox, binding) =
        auth::authenticate_runtime(&broker.client, headers, &account, identity, dispatch).await?;
    let account = broker
        .store
        .for_identity(identity)
        .read(root, account_uid)
        .await?;
    auth::authenticate_ledger(
        &broker.client,
        &account,
        identity,
        &sandbox,
        &binding,
        dispatch,
    )
    .await?;
    Ok(account)
}

async fn catalog(
    State(broker): State<Arc<Broker>>,
    headers: HeaderMap,
    Json(request): Json<BrokerRequest<SessionRequest>>,
) -> Result<Json<serde_json::Value>, Failure> {
    let account = authenticated_account(
        &broker,
        &headers,
        &request.root,
        &request.payload.account_uid,
        &request.payload.identity,
        true,
    )
    .await?;
    let catalog = broker
        .settings
        .catalog(&broker.client, chrono::Utc::now().timestamp())
        .await?;
    let ledger = Store::ledger(&account)?;
    let unbounded_tokens = ledger.unbounded_tokens.is_some();
    super::topups::authorize_read(&broker.client, &broker.settings, ledger).await?;
    let memory_service_allowed =
        ledger.allows_memory_service(&request.payload.identity, chrono::Utc::now().timestamp())?;
    Ok(Json(json!({
        "catalog": catalog.catalog, "uid": catalog.uid, "resourceVersion": catalog.resource_version,
        "digest": catalog.digest, "scope": "GovernedInference",
        "moneyRequired": ledger.requires_price(&request.payload.identity.task_uid)?,
        "unboundedTokens": unbounded_tokens,
        "memoryServiceAllowed": memory_service_allowed,
        "recordOnlyAllowed": ledger.allows_unbounded_inference(
            &request.payload.identity, chrono::Utc::now().timestamp()
        )?,
    })))
}

async fn session(
    State(broker): State<Arc<Broker>>,
    headers: HeaderMap,
    Json(request): Json<BrokerRequest<SessionRequest>>,
) -> Result<Json<crate::inference_budget_contract::ledger::Session>, Failure> {
    authenticated_account(
        &broker,
        &headers,
        &request.root,
        &request.payload.account_uid,
        &request.payload.identity,
        true,
    )
    .await?;
    let result = broker
        .store
        .for_identity(&request.payload.identity)
        .transact(&request.root, &request.payload.account_uid, |ledger| {
            ledger.register_session(request.payload.identity.clone())
        })
        .await?;
    Ok(Json(result))
}

fn authorize_quote(
    catalog: &crate::inference_budget_contract::catalog::Catalog,
    ledger: &crate::inference_budget_contract::ledger::Ledger,
    identity: &crate::inference_budget_contract::ExecutionIdentity,
    quote: &crate::inference_budget_contract::tariffs::AttemptQuote,
    now: i64,
) -> Result<(), BudgetError> {
    catalog.validate(now)?;
    if let Some(bounded) = quote.bounded() {
        catalog.accepts_quote(bounded, now, ledger.requires_price(&identity.task_uid)?)
    } else if ledger.allows_unbounded_inference(identity, now)? {
        quote.validate(now, false)
    } else {
        Err(BudgetError::Authorization)
    }
}

async fn reserve(
    State(broker): State<Arc<Broker>>,
    headers: HeaderMap,
    Json(request): Json<BrokerRequest<ReserveRequest>>,
) -> Result<Json<crate::inference_budget_contract::ledger::Reservation>, Failure> {
    authenticated_account(
        &broker,
        &headers,
        &request.root,
        &request.payload.account_uid,
        &request.payload.identity,
        true,
    )
    .await?;
    let now = chrono::Utc::now().timestamp();
    let catalog = broker.settings.catalog(&broker.client, now).await?;
    broker
        .store
        .transact(&request.root, &request.payload.account_uid, |ledger| {
            ledger.expire_undispatched(now)
        })
        .await?;
    let result = super::topups::transact(
        &broker.client,
        &broker.settings,
        &broker.store,
        &request.root,
        &request.payload.account_uid,
        |ledger| {
            let now = chrono::Utc::now().timestamp();
            authorize_quote(
                &catalog.catalog,
                ledger,
                &request.payload.identity,
                &request.payload.quote,
                now,
            )?;
            ledger.reserve(&request.payload, now)
        },
    )
    .await?;
    Ok(Json(result))
}

async fn begin(
    State(broker): State<Arc<Broker>>,
    headers: HeaderMap,
    Json(request): Json<BrokerRequest<AttemptCommand>>,
) -> Result<Json<crate::inference_budget_contract::ledger::Reservation>, Failure> {
    authenticated_account(
        &broker,
        &headers,
        &request.root,
        &request.payload.account_uid,
        &request.payload.identity,
        true,
    )
    .await?;
    let now = chrono::Utc::now().timestamp();
    let catalog = broker.settings.catalog(&broker.client, now).await?;
    let result = super::topups::transact(
        &broker.client,
        &broker.settings,
        &broker.store,
        &request.root,
        &request.payload.account_uid,
        |ledger| {
            let now = chrono::Utc::now().timestamp();
            let attempt = ledger
                .attempts
                .get(&request.payload.key.storage_key())
                .ok_or(BudgetError::Sequence)?;
            authorize_quote(
                &catalog.catalog,
                ledger,
                &request.payload.identity,
                &attempt.quote,
                now,
            )?;
            ledger.begin_dispatch(&request.payload, now)
        },
    )
    .await?;
    Ok(Json(result))
}

async fn settle(
    State(broker): State<Arc<Broker>>,
    headers: HeaderMap,
    Json(request): Json<BrokerRequest<Settlement>>,
) -> Result<Json<crate::inference_budget_contract::ledger::SettlementResult>, Failure> {
    let command = &request.payload.attempt;
    authenticated_account(
        &broker,
        &headers,
        &request.root,
        &command.account_uid,
        &command.identity,
        false,
    )
    .await?;
    // Settlement uses the accepted contract snapshot. A later tariff expiry or
    // policy change cannot erase already accepted liability.
    let result = broker
        .store
        .for_identity(&command.identity)
        .transact(&request.root, &command.account_uid, |ledger| {
            ledger.settle(&request.payload)
        })
        .await?;
    if result.breach {
        tracing::error!(account_uid = %command.account_uid, "Governed inference contract bound breached; account frozen");
    }
    Ok(Json(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_only_admission_uses_current_authority_not_a_caller_flag_or_contract() {
        use crate::inference_budget_contract::{
            ExecutionIdentity, Limits, RootIdentity, TaskAuthority,
            catalog::Catalog,
            ledger::Ledger,
            tariffs::{AttemptQuote, Operation},
        };
        let root: RootIdentity = serde_json::from_value(json!({
            "kind":"KarsTask", "resource":{"namespace":"workspace", "name":"root", "uid":"root-uid"},
            "workspaceUid":"workspace-uid", "clusterUid":"cluster-uid"
        })).unwrap();
        let digest = format!("sha256:{}", "a".repeat(64));
        let authority: TaskAuthority = serde_json::from_value(json!({
            "task":root.resource, "parentUid":null, "rootTaskUid":"root-uid",
            "authorizationDigest":digest, "effectiveAuthorization":{"reviewed":true}, "limits":{}
        }))
        .unwrap();
        let identity: ExecutionIdentity = serde_json::from_value(json!({
            "taskUid":"root-uid", "authorizationDigest":digest,
            "sandbox":{"namespace":"workspace", "name":"sandbox", "uid":"sandbox-uid"},
            "runtimeNamespaceUid":"namespace-uid", "podName":"pod", "podUid":"pod-uid"
        }))
        .unwrap();
        let ledger = Ledger::new("account-uid".into(), root, Limits::default())
            .unwrap()
            .register_task(authority)
            .unwrap()
            .next;
        let catalog = Catalog {
            version: "v1".into(),
            contracts: vec![],
            non_inference_egress_hosts: vec![],
        };
        let quote = AttemptQuote::record_only(
            "provider",
            "https://models.example",
            "uncontracted",
            Operation::Responses,
        );
        assert!(ledger.unbounded_tokens.is_none());
        authorize_quote(&catalog, &ledger, &identity, &quote, 100).unwrap();
        for limits in [
            Limits {
                tokens: Some(100),
                usd_micros: None,
            },
            Limits {
                tokens: None,
                usd_micros: Some(100),
            },
        ] {
            let mut finite = ledger.clone();
            finite.limits = limits;
            assert!(authorize_quote(&catalog, &finite, &identity, &quote, 100).is_err());
            let mut finite = ledger.clone();
            finite.nodes.get_mut("root-uid").unwrap().authority.limits = limits;
            assert!(authorize_quote(&catalog, &finite, &identity, &quote, 100).is_err());
        }
        let mut revoked = ledger.clone();
        revoked.nodes.get_mut("root-uid").unwrap().active = false;
        assert!(authorize_quote(&catalog, &revoked, &identity, &quote, 100).is_err());
        let mut forged = identity.clone();
        forged.authorization_digest = format!("sha256:{}", "b".repeat(64));
        assert!(authorize_quote(&catalog, &ledger, &forged, &quote, 100).is_err());
        let mut invalid = catalog.clone();
        invalid.non_inference_egress_hosts.push("*.example".into());
        assert!(authorize_quote(&invalid, &ledger, &identity, &quote, 100).is_err());
        let invalid = AttemptQuote::record_only(
            "provider",
            "https://user:secret@models.example",
            "model",
            Operation::Responses,
        );
        assert!(authorize_quote(&catalog, &ledger, &identity, &invalid, 100).is_err());
    }

    #[tokio::test]
    async fn calendar_policy_metadata_preserves_the_legacy_usage_body() {
        use crate::inference_budget_contract::calendar::{
            Observation, PENDING_HEADER, POLICY_HEADER, Policy, Usage,
        };
        let usage = Usage {
            day: 1,
            month: 1,
            daily: 39,
            monthly: 39,
        };
        let response = calendar_response(Observation {
            usage: usage.clone(),
            policy: Policy::RecordOnly,
            pending_requests: Some(2),
        });
        assert_eq!(response.headers()[POLICY_HEADER], "record-only");
        assert_eq!(response.headers()[PENDING_HEADER], "2");
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        assert_eq!(serde_json::from_slice::<Usage>(&body).unwrap(), usage);
    }

    #[tokio::test]
    async fn broker_failures_preserve_status_and_sanitized_error_contracts() {
        for (error, status, code) in [
            (
                StoreError::PrivacyPending,
                StatusCode::SERVICE_UNAVAILABLE,
                "inference_privacy_pending",
            ),
            (
                StoreError::Contention,
                StatusCode::SERVICE_UNAVAILABLE,
                "inference_budget_contention",
            ),
            (
                StoreError::Api {
                    stage: "verify budget Pod",
                    code: Some(404),
                },
                StatusCode::SERVICE_UNAVAILABLE,
                "inference_budget_unavailable",
            ),
            (
                StoreError::Api {
                    stage: "commit ledger transition",
                    code: Some(503),
                },
                StatusCode::SERVICE_UNAVAILABLE,
                "inference_budget_unavailable",
            ),
            (
                StoreError::Api {
                    stage: "commit ledger transition",
                    code: None,
                },
                StatusCode::SERVICE_UNAVAILABLE,
                "inference_budget_unavailable",
            ),
            (
                StoreError::Missing,
                StatusCode::SERVICE_UNAVAILABLE,
                "inference_budget_unavailable",
            ),
            (
                StoreError::Ledger(BudgetError::Authorization),
                StatusCode::FORBIDDEN,
                "inference_budget_authority",
            ),
            (
                StoreError::Ledger(BudgetError::Exhausted),
                StatusCode::TOO_MANY_REQUESTS,
                "inference_budget_exhausted",
            ),
            (
                StoreError::Ledger(BudgetError::Capacity),
                StatusCode::SERVICE_UNAVAILABLE,
                "inference_budget_capacity",
            ),
        ] {
            let message = error.to_string();
            let response = Failure(error).into_response();
            assert_eq!(response.status(), status);
            let body = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
                json!({"error": {"code": code, "message": message}})
            );
        }
    }
}
