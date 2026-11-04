// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::inference_budget_contract::{
    AttemptCommand, BrokerRequest, ExecutionIdentity, ReserveRequest, RouterBinding,
    SessionRequest, Settlement, Usage,
    catalog::Catalog,
    ledger::{Reservation, Session, SettlementResult},
    tariffs::{AttemptQuote, Operation},
};
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::Mutex;

const TOKEN_PATH: &str = "/var/run/kars/inference-budget/token";

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    Exhausted,
    Capacity,
}

impl Refusal {
    pub fn code(self) -> &'static str {
        match self {
            Self::Exhausted => "inference_budget_exhausted",
            Self::Capacity => "inference_budget_capacity",
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::Exhausted => {
                "The required maximum input plus output reservation does not fit the remaining token or configured-price budget at the account or an ancestor limit. Review the budget authorization before resubmitting; unchanged automatic retries cannot resolve this refusal."
            }
            Self::Capacity => {
                "The governed inference ledger has reached its retained-identity, session, attempt, or replay-window capacity. Review account capacity before resubmitting; automatic retries are not a recovery action."
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error(
    "Governed inference budget {stage}: {reason} (HTTP {status:?}); no provider fallback",
    reason = .refusal.map_or("unavailable or denied", Refusal::message)
)]
pub struct Error {
    pub stage: &'static str,
    pub status: Option<u16>,
    pub refusal: Option<Refusal>,
}

fn failure(stage: &'static str) -> Error {
    Error {
        stage,
        status: None,
        refusal: None,
    }
}

pub(super) enum BrokerDenial {
    Refusal(Refusal),
    PrivacyPending,
    ReserveContention,
}

pub(super) async fn broker_denial(
    path: &str,
    mut response: reqwest::Response,
) -> Option<BrokerDenial> {
    let status = response.status().as_u16();
    if !matches!(status, 429 | 503) {
        return None;
    }
    // Read only a bounded private-broker error envelope. Never surface its free
    // text: an intermediary may return HTML, secrets, or an unrelated throttling error.
    const MAX_ERROR_BYTES: usize = 8192;
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        if chunk.len() > MAX_ERROR_BYTES - body.len() {
            return None;
        }
        body.extend_from_slice(&chunk);
    }
    let body: serde_json::Value = serde_json::from_slice(&body).ok()?;
    match (path, status, body.pointer("/error/code")?.as_str()?) {
        (
            "/v1/catalog"
            | "/v1/session"
            | "/v1/reserve"
            | "/v1/begin"
            | "/v1/calendar/read"
            | "/v1/calendar/commit",
            503,
            "inference_privacy_pending",
        ) => Some(BrokerDenial::PrivacyPending),
        ("/v1/reserve", 503, "inference_budget_contention") => {
            Some(BrokerDenial::ReserveContention)
        }
        ("/v1/session" | "/v1/reserve", 429, "inference_budget_exhausted") => {
            Some(BrokerDenial::Refusal(Refusal::Exhausted))
        }
        ("/v1/session" | "/v1/reserve", 503, "inference_budget_capacity") => {
            Some(BrokerDenial::Refusal(Refusal::Capacity))
        }
        _ => None,
    }
}

pub(super) fn transport_from_env() -> Result<(String, reqwest::Client), Error> {
    let endpoint =
        std::env::var("KARS_INFERENCE_BUDGET_ENDPOINT").map_err(|_| failure("endpoint"))?;
    let url = reqwest::Url::parse(&endpoint).map_err(|_| failure("endpoint"))?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(failure("TLS endpoint"));
    }
    let ca_path =
        std::env::var("KARS_INFERENCE_BUDGET_CA").map_err(|_| failure("CA configuration"))?;
    let ca = std::fs::read(ca_path).map_err(|_| failure("CA material"))?;
    let certificate = reqwest::Certificate::from_pem(&ca).map_err(|_| failure("CA material"))?;
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .tls_built_in_root_certs(false)
        .add_root_certificate(certificate)
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|_| failure("TLS client"))?;
    Ok((endpoint.trim_end_matches('/').into(), http))
}

pub struct Client {
    pub binding: RouterBinding,
    pub identity: ExecutionIdentity,
    endpoint: String,
    token_path: PathBuf,
    http: reqwest::Client,
    reservation_lock: Mutex<()>,
}

#[derive(Deserialize)]
struct CatalogResponse {
    catalog: Catalog,
    #[serde(rename = "moneyRequired")]
    money_required: bool,
    #[serde(default, rename = "unboundedTokens")]
    unbounded_tokens: bool,
    #[serde(default, rename = "memoryServiceAllowed")]
    memory_service_allowed: bool,
    #[serde(default, rename = "recordOnlyAllowed")]
    record_only_allowed: bool,
}

impl Client {
    pub fn from_env() -> Result<Option<Arc<Self>>, Error> {
        match std::env::var("KARS_INFERENCE_BUDGET_REQUIRED")
            .unwrap_or_default()
            .as_str()
        {
            "" | "false" => return Ok(None),
            "true" => {}
            _ => return Err(failure("required-mode configuration")),
        }
        let binding: RouterBinding = serde_json::from_str(
            &std::env::var("KARS_INFERENCE_BUDGET_BINDING").map_err(|_| failure("binding"))?,
        )
        .map_err(|_| failure("binding"))?;
        let identity = ExecutionIdentity {
            task_uid: binding.task.task_uid.clone(),
            authorization_digest: binding.task.authorization_digest.clone(),
            sandbox: binding.sandbox.clone(),
            runtime_namespace_uid: binding.runtime_namespace_uid.clone(),
            pod_name: std::env::var("POD_NAME").map_err(|_| failure("Pod name"))?,
            pod_uid: std::env::var("POD_UID").map_err(|_| failure("Pod UID"))?,
        };
        identity.validate().map_err(|_| failure("identity"))?;
        let (endpoint, http) = transport_from_env()?;
        Ok(Some(Arc::new(Self {
            binding,
            identity,
            endpoint,
            token_path: TOKEN_PATH.into(),
            http,
            reservation_lock: Mutex::new(()),
        })))
    }

    async fn rpc<T: Serialize, R: serde::de::DeserializeOwned>(
        &self,
        path: &'static str,
        payload: T,
    ) -> Result<R, Error> {
        // Freeze the wire envelope too: retries may refresh credentials, never
        // the root, identity, sequence, quote or digest of an ambiguous reserve.
        let request = bytes::Bytes::from(
            serde_json::to_vec(&BrokerRequest {
                root: self.binding.task.root.clone(),
                payload,
            })
            .map_err(|_| failure(path))?,
        );
        loop {
            let token = tokio::fs::read_to_string(&self.token_path)
                .await
                .map_err(|_| failure("private audience token"))?;
            if token.trim().is_empty() {
                return Err(failure("private audience token"));
            }
            let response = self
                .http
                .post(format!("{}{path}", self.endpoint))
                .bearer_auth(token.trim())
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(request.clone())
                .send()
                .await
                .map_err(|_| failure(path))?;
            let status = response.status();
            if status.is_success() {
                return response
                    .json()
                    .await
                    .map_err(|_| failure("broker response"));
            }
            let refusal = match broker_denial(path, response).await {
                Some(BrokerDenial::PrivacyPending) => {
                    // This explicit response precedes ledger mutation/provider dispatch.
                    // Preserve the request identity and recheck live authority after migration;
                    // dropping this future cancels the wait without spawning detached retries.
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    continue;
                }
                Some(BrokerDenial::ReserveContention) => {
                    // The write may have committed. Replay the identical reservation,
                    // never a fresh sequence or provider call; live authority is checked
                    // again and the admission worker still owns timeout/cancellation.
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    continue;
                }
                Some(BrokerDenial::Refusal(refusal)) => Some(refusal),
                None => None,
            };
            return Err(Error {
                stage: path,
                status: Some(status.as_u16()),
                refusal,
            });
        }
    }

    fn session_request(&self) -> SessionRequest {
        SessionRequest {
            account_uid: self.binding.task.account.uid.clone(),
            identity: self.identity.clone(),
        }
    }

    async fn catalog_response(&self) -> Result<CatalogResponse, Error> {
        let response: CatalogResponse = self.rpc("/v1/catalog", self.session_request()).await?;
        response
            .catalog
            .validate(chrono::Utc::now().timestamp())
            .map_err(|_| failure("operator contracts"))?;
        if response.record_only_allowed && response.money_required {
            return Err(failure("inconsistent recording authority"));
        }
        Ok(response)
    }

    pub async fn catalog(&self) -> Result<Catalog, Error> {
        Ok(self.catalog_response().await?.catalog)
    }

    pub async fn tokens_unbounded(&self, sandbox: &str) -> Result<bool, Error> {
        if sandbox != self.identity.sandbox.name {
            return Err(failure("calendar allowance identity"));
        }
        // Never cache authority: deleting an operator receipt revokes the override.
        Ok(self.catalog_response().await?.unbounded_tokens)
    }

    /// Memory's internal model work has no provider-enforced token bound.
    /// Permit it only under current, authenticated unbounded funding with no
    /// monetary ceiling; the caller must separately authorize and audit memory.
    pub async fn allows_bound_memory(&self, sandbox: &str) -> Result<bool, Error> {
        if sandbox != self.identity.sandbox.name {
            return Err(failure("memory allowance identity"));
        }
        let authority = self.catalog_response().await?;
        Ok(authority.memory_service_allowed && !authority.money_required)
    }

    pub async fn requires_responses_contract(
        &self,
        upstream: &crate::proxy::UpstreamConfig,
    ) -> Result<bool, Error> {
        let catalog = self.catalog().await?;
        let now = chrono::Utc::now().timestamp();
        let supports = |operation| {
            catalog
                .select(
                    upstream.telemetry_provider(),
                    &upstream.endpoint,
                    &upstream.deployment,
                    operation,
                    now,
                )
                .is_ok()
        };
        Ok(!supports(Operation::ChatCompletions) && supports(Operation::Responses))
    }

    pub async fn model_limits(
        &self,
        upstream: &crate::proxy::UpstreamConfig,
    ) -> Result<Option<(u64, u64)>, Error> {
        let authority = self.catalog_response().await?;
        let catalog = authority.catalog;
        let now = chrono::Utc::now().timestamp();
        let operations: &[Operation] =
            if upstream.provider == crate::provider::ProviderKind::Anthropic {
                &[Operation::AnthropicMessages]
            } else {
                &[Operation::ChatCompletions, Operation::Responses]
            };
        let contract = operations.iter().find_map(|operation| {
            catalog
                .select(
                    upstream.telemetry_provider(),
                    &upstream.endpoint,
                    &upstream.deployment,
                    *operation,
                    now,
                )
                .ok()
        });
        let Some(contract) = contract else {
            return if authority.record_only_allowed {
                Ok(None)
            } else {
                Err(failure("selected model bounds"))
            };
        };
        // The attested complete-input bound is a conservative runtime context window,
        // not a guessed provider-family default or a sum of independent maxima.
        Ok(Some((
            contract.maximum_input_tokens,
            contract.maximum_output_tokens,
        )))
    }

    pub async fn ready_for(&self, upstream: &crate::proxy::UpstreamConfig) -> Result<(), Error> {
        let response = self.catalog_response().await?;
        if response.record_only_allowed {
            return Ok(());
        }
        let now = chrono::Utc::now().timestamp();
        let applicable = response.catalog.contracts.iter().any(|contract| {
            contract.provider_id == upstream.telemetry_provider()
                && contract.endpoint.trim_end_matches('/')
                    == upstream.endpoint.trim_end_matches('/')
                && contract.model == upstream.deployment
                && contract.validate(now).is_ok()
                && (!response.money_required || contract.maximum_price.is_some())
        });
        if !applicable {
            return Err(failure("selected model bounds/maximum prices"));
        }
        Ok(())
    }

    /// Called for each actual final provider/model/wire SEND. Resolving provider
    /// credentials/configuration happens before this boundary, not after begin.
    pub async fn begin(
        self: &Arc<Self>,
        provider_id: &str,
        endpoint: &str,
        model: &str,
        operation: Operation,
        wire: &[u8],
    ) -> Result<(bytes::Bytes, AttemptGuard), Error> {
        let authority = self.catalog_response().await?;
        let now = chrono::Utc::now().timestamp();
        let (body, quote) = if authority.record_only_allowed {
            crate::inference_budget_dispatch::normalize_record_only(
                provider_id,
                endpoint,
                model,
                operation,
                wire,
                now,
            )
            .map_err(|_| failure("recorded wire request"))?
        } else {
            let contract = authority
                .catalog
                .select(provider_id, endpoint, model, operation, now)
                .map_err(|_| failure("provider/operation contract"))?;
            let (body, quote) = contract
                .normalize(wire, now, authority.money_required)
                .map_err(|_| failure("wire request bounds"))?;
            (body, AttemptQuote::from(quote))
        };
        let wire_digest = format!("sha256:{}", crate::providers::signing::sha256_hex(&body));
        let lock = self.reservation_lock.lock().await;
        // Refresh the durable sequence rather than resetting a process-local
        // counter after restart or a lost reserve acknowledgement.
        let session: Session = self.rpc("/v1/session", self.session_request()).await?;
        if session.identity != self.identity || session.closed {
            return Err(failure("session identity"));
        }
        let reserve = ReserveRequest {
            account_uid: self.binding.task.account.uid.clone(),
            identity: self.identity.clone(),
            sequence: session.next_sequence,
            wire_digest: wire_digest.clone(),
            quote: quote.clone(),
        };
        let reserved: Reservation = self.rpc("/v1/reserve", reserve).await?;
        if reserved.key.pod_uid != self.identity.pod_uid
            || reserved.key.sequence != session.next_sequence
            || reserved.maximum != quote.maximum()
            || reserved.phase != crate::inference_budget_contract::AttemptPhase::Reserved
        {
            return Err(failure("reservation identity"));
        }
        drop(lock);
        let command = AttemptCommand {
            account_uid: self.binding.task.account.uid.clone(),
            key: reserved.key,
            identity: self.identity.clone(),
            wire_digest,
        };
        let begun: Reservation = self.rpc("/v1/begin", command.clone()).await?;
        if begun.key != command.key
            || begun.maximum != quote.maximum()
            || begun.phase != crate::inference_budget_contract::AttemptPhase::InFlight
            || begun.expires_at != reserved.expires_at
            || begun.expires_at <= chrono::Utc::now().timestamp()
            || quote
                .validate(chrono::Utc::now().timestamp(), authority.money_required)
                .is_err()
        {
            return Err(failure("dispatch identity"));
        }
        Ok((
            body.into(),
            AttemptGuard {
                client: self.clone(),
                command,
                quote,
                finalized: AtomicBool::new(false),
            },
        ))
    }

    async fn settle(
        &self,
        command: AttemptCommand,
        usage: Option<Usage>,
    ) -> Result<SettlementResult, Error> {
        self.rpc(
            "/v1/settle",
            Settlement {
                attempt: command,
                usage,
            },
        )
        .await
    }
}

pub struct AttemptGuard {
    client: Arc<Client>,
    pub command: AttemptCommand,
    pub quote: AttemptQuote,
    finalized: AtomicBool,
}

impl AttemptGuard {
    pub async fn finish(self, usage: Option<Usage>) -> Result<SettlementResult, Error> {
        if self.finalized.swap(true, Ordering::AcqRel) {
            return Err(failure("duplicate local settlement"));
        }
        // The paired dispatch worker owns this RPC through caller cancellation.
        // Failure or worker expiry leaves recovery to the durable broker rather
        // than launching an unobserved second settlement from this guard's Drop.
        self.client.settle(self.command.clone(), usage).await
    }
}

impl Drop for AttemptGuard {
    fn drop(&mut self) {
        if self.finalized.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let client = self.client.clone();
            let command = self.command.clone();
            runtime.spawn(async move {
                if client.settle(command, None).await.is_err() {
                    // The durable InFlight reservation stays charged/held.
                    // The broker's recovery loop conservatively finalizes it.
                    tracing::warn!(
                        "Governed inference settlement pending; reservation remains funded"
                    );
                }
            });
        }
    }
}
