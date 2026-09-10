// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::inference_budget_contract::{
    AttemptCommand, BrokerRequest, ExecutionIdentity, ReserveRequest, RouterBinding,
    SessionRequest, Settlement, Usage,
    catalog::Catalog,
    ledger::{Reservation, Session, SettlementResult},
    tariffs::{Operation, Quote},
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

#[derive(Debug, thiserror::Error)]
#[error(
    "Governed inference budget {stage} unavailable/denied (HTTP {status:?}); no provider fallback"
)]
pub struct Error {
    pub stage: &'static str,
    pub status: Option<u16>,
}

#[track_caller]
fn failure(stage: &'static str) -> Error {
    tracing::warn!(
        budget_stage = "router-budget-client",
        source_line = std::panic::Location::caller().line(),
        "Governed inference client operation unavailable"
    );
    Error {
        stage,
        status: None,
    }
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
        let certificate =
            reqwest::Certificate::from_pem(&ca).map_err(|_| failure("CA material"))?;
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .tls_built_in_root_certs(false)
            .add_root_certificate(certificate)
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|_| failure("TLS client"))?;
        Ok(Some(Arc::new(Self {
            binding,
            identity,
            endpoint: endpoint.trim_end_matches('/').into(),
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
        let token = tokio::fs::read_to_string(&self.token_path)
            .await
            .map_err(|_| failure("private audience token"))?;
        if token.trim().is_empty() {
            return Err(failure("private audience token"));
        }
        let request = BrokerRequest {
            root: self.binding.task.root.clone(),
            payload,
        };
        let response = self
            .http
            .post(format!("{}{path}", self.endpoint))
            .bearer_auth(token.trim())
            .json(&request)
            .send()
            .await
            .map_err(|_| failure(path))?;
        let status = response.status();
        if !status.is_success() {
            tracing::warn!(
                budget_stage = "router-broker-http",
                http_status = status.as_u16(),
                "Governed inference broker rejected request"
            );
            return Err(Error {
                stage: path,
                status: Some(status.as_u16()),
            });
        }
        response
            .json()
            .await
            .map_err(|_| failure("broker response"))
    }

    fn session_request(&self) -> SessionRequest {
        SessionRequest {
            account_uid: self.binding.task.account.uid.clone(),
            identity: self.identity.clone(),
        }
    }

    pub async fn catalog(&self) -> Result<Catalog, Error> {
        let response: CatalogResponse = self.rpc("/v1/catalog", self.session_request()).await?;
        response
            .catalog
            .validate(chrono::Utc::now().timestamp())
            .map_err(|_| failure("operator contracts"))?;
        Ok(response.catalog)
    }

    pub async fn ready_for(&self, upstream: &crate::proxy::UpstreamConfig) -> Result<(), Error> {
        let response: CatalogResponse = self.rpc("/v1/catalog", self.session_request()).await?;
        let now = chrono::Utc::now().timestamp();
        response
            .catalog
            .validate(now)
            .map_err(|_| failure("operator contracts"))?;
        let applicable = response.catalog.contracts.iter().any(|contract| {
            contract.provider_id == upstream.telemetry_provider()
                && contract.endpoint.trim_end_matches('/')
                    == upstream.endpoint.trim_end_matches('/')
                && contract.model == upstream.deployment
                && contract.validate(now).is_ok()
                && (!response.money_required || contract.maximum_price.is_some())
        });
        if !applicable {
            for contract in &response.catalog.contracts {
                tracing::warn!(
                    budget_stage = "router-contract-match",
                    provider_matches = contract.provider_id == upstream.telemetry_provider(),
                    endpoint_matches = contract.endpoint.trim_end_matches('/')
                        == upstream.endpoint.trim_end_matches('/'),
                    model_matches = contract.model == upstream.deployment,
                    bounds_valid = contract.validate(now).is_ok(),
                    price_available = !response.money_required || contract.maximum_price.is_some(),
                    "Governed inference selected contract mismatch"
                );
            }
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
        let catalog = self.catalog().await?;
        let contract = catalog
            .select(
                provider_id,
                endpoint,
                model,
                operation,
                chrono::Utc::now().timestamp(),
            )
            .map_err(|_| failure("provider/operation contract"))?;
        // The broker independently checks every ancestor's price requirement.
        // This normalization never invents a price for a token-only contract.
        let (body, quote) = contract
            .normalize(wire, chrono::Utc::now().timestamp(), false)
            .map_err(|_| failure("wire request bounds"))?;
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
            || reserved.maximum != quote.maximum
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
            || begun.maximum != quote.maximum
            || begun.phase != crate::inference_budget_contract::AttemptPhase::InFlight
            || begun.expires_at != reserved.expires_at
            || begun.expires_at <= chrono::Utc::now().timestamp()
            || quote
                .contract
                .validate(chrono::Utc::now().timestamp())
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
    pub quote: Quote,
    finalized: AtomicBool,
}

impl AttemptGuard {
    pub async fn finish(&self, usage: Option<Usage>) -> Result<SettlementResult, Error> {
        if self.finalized.swap(true, Ordering::AcqRel) {
            return Err(failure("duplicate local settlement"));
        }
        let result = self.client.settle(self.command.clone(), usage).await;
        if result.is_err() {
            self.finalized.store(false, Ordering::Release);
        }
        result
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
