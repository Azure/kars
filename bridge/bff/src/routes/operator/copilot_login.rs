// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::additional_providers::{INFERENCE_PROVIDERS_NS, INFERENCE_PROVIDERS_SECRET};
use super::copilot_transport::{CopilotClient, CopilotEndpoint};
use super::providers::{
    DiscoveredModelDto, copilot_jwt_with_client, fetch_copilot_models_with_client,
    invalidate_copilot_catalog_cache,
};
use crate::{kars::cluster::Cluster, state::AppState};

const CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";
const MAX_SECONDS: u64 = 900;

#[derive(Deserialize, Serialize)]
pub struct CopilotLoginStart {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub interval: u64,
    pub expires_in: u64,
}

#[derive(Deserialize)]
pub struct CopilotLoginPollRequest {
    pub device_code: String,
    #[serde(default)]
    pub interval: Option<u64>,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CopilotLoginPoll {
    Pending {
        interval: u64,
        reason: PendingReason,
    },
    Authorized {
        models: Vec<DiscoveredModelDto>,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingReason {
    AuthorizationPending,
    SlowDown,
}

#[derive(Debug)]
pub enum CopilotLoginError {
    NotReady,
    InvalidRequest,
    Upstream,
    Malformed,
    Expired,
    Denied,
    RateLimited,
    SeatUnavailable,
    StorageUnconfirmed,
}

impl IntoResponse for CopilotLoginError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            Self::NotReady => (
                StatusCode::CONFLICT,
                "copilot_not_ready",
                "Copilot credential storage is not ready. An operator must repair the workspace credential grant and enrolled provider store before sign-in. No GitHub token was requested.",
            ),
            Self::InvalidRequest => (
                StatusCode::BAD_REQUEST,
                "copilot_invalid_request",
                "The sign-in request is invalid.",
            ),
            Self::Upstream => (
                StatusCode::BAD_GATEWAY,
                "copilot_upstream",
                "GitHub sign-in could not be reached. Polling stopped; check connectivity before trying again.",
            ),
            Self::Malformed => (
                StatusCode::BAD_GATEWAY,
                "copilot_invalid_response",
                "GitHub returned an invalid sign-in response. Polling stopped.",
            ),
            Self::Expired => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "copilot_expired",
                "The sign-in code expired. A new sign-in is required.",
            ),
            Self::Denied => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "copilot_denied",
                "Sign-in was cancelled on GitHub.",
            ),
            Self::RateLimited => (
                StatusCode::TOO_MANY_REQUESTS,
                "copilot_rate_limited",
                "GitHub rate-limited sign-in. Polling stopped; wait before trying again.",
            ),
            Self::SeatUnavailable => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "copilot_seat_unavailable",
                "Copilot eligibility could not be verified. Check the account's Copilot seat and Chat access before trying again.",
            ),
            Self::StorageUnconfirmed => (
                StatusCode::CONFLICT,
                "copilot_storage_unconfirmed",
                "GitHub approved sign-in, but credential storage could not be confirmed. An operator must check the provider store and grant. Do not repeat approval: the token may already have been consumed and a new sign-in may be required.",
            ),
        };
        // Only fixed text leaves this endpoint, including logs and unknown upstream errors.
        (
            status,
            Json(json!({"error": {"code": code, "message": message}})),
        )
            .into_response()
    }
}

struct OAuth {
    client: CopilotClient,
}

impl OAuth {
    fn github() -> Result<Self, CopilotLoginError> {
        Ok(Self {
            client: CopilotClient::github().map_err(|_| CopilotLoginError::Upstream)?,
        })
    }

    async fn post(
        &self,
        endpoint: CopilotEndpoint,
        input: Value,
    ) -> Result<Value, CopilotLoginError> {
        let response = self
            .client
            .request(endpoint)
            .header("Accept", "application/json")
            .header("User-Agent", "kars-bridge")
            .json(&input)
            .send()
            .await
            .map_err(|_| CopilotLoginError::Upstream)?;
        if response.status() == StatusCode::TOO_MANY_REQUESTS {
            return Err(CopilotLoginError::RateLimited);
        }
        if !response.status().is_success() {
            return Err(CopilotLoginError::Upstream);
        }
        response
            .json()
            .await
            .map_err(|_| CopilotLoginError::Malformed)
    }
}

fn opaque(value: &str) -> bool {
    !value.is_empty() && value.len() <= 4096 && value.bytes().all(|b| b.is_ascii_graphic())
}

async fn preflight(cluster: &Cluster) -> Result<(), CopilotLoginError> {
    let (_, secret) = cluster
        .integration_store(INFERENCE_PROVIDERS_NS, INFERENCE_PROVIDERS_SECRET)
        .await
        .map_err(|_| CopilotLoginError::NotReady)?;
    if secret
        .metadata
        .resource_version
        .as_deref()
        .is_none_or(str::is_empty)
        || secret
            .data
            .as_ref()
            .is_some_and(|data| data.values().any(|v| std::str::from_utf8(&v.0).is_err()))
    {
        return Err(CopilotLoginError::NotReady);
    }
    Ok(())
}

async fn start(cluster: &Cluster, oauth: &OAuth) -> Result<CopilotLoginStart, CopilotLoginError> {
    preflight(cluster).await?;
    let body = oauth
        .post(
            CopilotEndpoint::DeviceCode,
            json!({"client_id": CLIENT_ID, "scope": "read:user"}),
        )
        .await?;
    if body.get("error").is_some() {
        return Err(CopilotLoginError::Malformed);
    }
    let flow: CopilotLoginStart =
        serde_json::from_value(body).map_err(|_| CopilotLoginError::Malformed)?;
    if !opaque(&flow.device_code)
        || flow.user_code.is_empty()
        || flow.user_code.len() > 64
        || !flow
            .user_code
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-')
        || flow.verification_uri != "https://github.com/login/device"
        || !(1..=MAX_SECONDS).contains(&flow.interval)
        || !(1..=MAX_SECONDS).contains(&flow.expires_in)
    {
        return Err(CopilotLoginError::Malformed);
    }
    Ok(flow)
}

async fn poll(
    cluster: &Cluster,
    oauth: &OAuth,
    request: CopilotLoginPollRequest,
) -> Result<CopilotLoginPoll, CopilotLoginError> {
    let interval = request.interval.unwrap_or(5);
    if !opaque(&request.device_code) || !(1..=MAX_SECONDS).contains(&interval) {
        return Err(CopilotLoginError::InvalidRequest);
    }
    // Re-check before each exchange; the write still repeats the grant/UID/CAS checks.
    // This is a preflight, not a reservation or a promise that a consumed token can resume.
    preflight(cluster).await?;
    let body = oauth
        .post(
            CopilotEndpoint::AccessToken,
            json!({
                "client_id": CLIENT_ID, "device_code": request.device_code,
                "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
            }),
        )
        .await?;
    if body.get("access_token").is_some() {
        let token = body["access_token"]
            .as_str()
            .filter(|value| opaque(value))
            .ok_or(CopilotLoginError::Malformed)?;
        if body.get("error").is_some()
            || !body["token_type"]
                .as_str()
                .is_some_and(|kind| kind.eq_ignore_ascii_case("bearer"))
        {
            return Err(CopilotLoginError::Malformed);
        }
        let jwt = copilot_jwt_with_client(token, &oauth.client)
            .await
            .map_err(|_| CopilotLoginError::SeatUnavailable)?;
        if !opaque(&jwt) {
            return Err(CopilotLoginError::SeatUnavailable);
        }
        cluster
            .mutate_secret_keys(INFERENCE_PROVIDERS_NS, INFERENCE_PROVIDERS_SECRET, |keys| {
                keys.insert("COPILOT_GITHUB_TOKEN".into(), token.into());
            })
            .await
            .map_err(|_| CopilotLoginError::StorageUnconfirmed)?;
        invalidate_copilot_catalog_cache();
        let models = fetch_copilot_models_with_client(&jwt, &oauth.client)
            .await
            .unwrap_or_default();
        return Ok(CopilotLoginPoll::Authorized { models });
    }
    match body["error"].as_str() {
        Some("authorization_pending") => Ok(CopilotLoginPoll::Pending {
            interval,
            reason: PendingReason::AuthorizationPending,
        }),
        Some("slow_down") => {
            let advertised = match body.get("interval") {
                None => 0,
                Some(value) => value
                    .as_u64()
                    .filter(|v| (1..=MAX_SECONDS).contains(v))
                    .ok_or(CopilotLoginError::Malformed)?,
            };
            let next = interval.saturating_add(5).max(advertised);
            if next > MAX_SECONDS {
                return Err(CopilotLoginError::RateLimited);
            }
            Ok(CopilotLoginPoll::Pending {
                interval: next,
                reason: PendingReason::SlowDown,
            })
        }
        Some("expired_token") => Err(CopilotLoginError::Expired),
        Some("access_denied") => Err(CopilotLoginError::Denied),
        _ => Err(CopilotLoginError::Malformed),
    }
}

pub async fn copilot_login_start(
    State(state): State<AppState>,
) -> Result<Json<CopilotLoginStart>, CopilotLoginError> {
    let cluster = state.cluster().ok_or(CopilotLoginError::NotReady)?;
    start(cluster, &OAuth::github()?).await.map(Json)
}

pub async fn copilot_login_poll(
    State(state): State<AppState>,
    request: Result<Json<CopilotLoginPollRequest>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<CopilotLoginPoll>, CopilotLoginError> {
    let cluster = state.cluster().ok_or(CopilotLoginError::NotReady)?;
    let Json(request) = request.map_err(|_| CopilotLoginError::InvalidRequest)?;
    poll(cluster, &OAuth::github()?, request).await.map(Json)
}

#[cfg(test)]
#[path = "copilot_login_tests.rs"]
mod tests;
