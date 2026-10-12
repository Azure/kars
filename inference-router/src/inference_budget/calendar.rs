// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::inference_budget_contract::calendar::{
    BINDING_ENV, Binding, Commit, Event, Identity, Read, Usage,
};
use serde::Serialize;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::Mutex;

#[derive(Debug, thiserror::Error)]
#[error("Calendar token accounting unavailable at {0}; inference remains blocked")]
pub struct Error(pub(crate) &'static str);

#[derive(Debug)]
struct Sequence {
    next: u64,
    failed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        budget::TokenBudgetTracker,
        inference_budget_contract::{ResourceIdentity, calendar::Ledger},
    };
    use chrono::{Datelike, Utc};
    use std::sync::Mutex as StdMutex;
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    async fn client(endpoint: String) -> Arc<Client> {
        let token_path = std::env::temp_dir().join(format!(
            "calendar-token-{}",
            hex::encode(rand::random::<[u8; 16]>())
        ));
        tokio::fs::write(&token_path, b"test-private-token")
            .await
            .unwrap();
        Arc::new(Client {
            identity: Identity {
                binding: Binding {
                    sandbox: ResourceIdentity {
                        namespace: "workspace".into(),
                        name: "sandbox".into(),
                        uid: "sandbox-uid".into(),
                    },
                    runtime_namespace_uid: "namespace-uid".into(),
                    account: ResourceIdentity {
                        namespace: "accounting".into(),
                        name: "account".into(),
                        uid: "account-uid".into(),
                    },
                    privacy_epoch: None,
                },
                pod_name: "pod".into(),
                pod_uid: "pod-uid".into(),
            },
            writer: hex::encode(rand::random::<[u8; 16]>()),
            endpoint,
            uncertain: AtomicBool::new(false),
            http: reqwest::Client::new(),
            token_path,
            sequence: Mutex::new(Sequence {
                next: 1,
                failed: false,
            }),
        })
    }

    async fn accounting_server() -> (MockServer, Arc<Client>) {
        let server = MockServer::start().await;
        let first = client(server.uri()).await;
        let ledger = Arc::new(StdMutex::new((
            Ledger::new(
                first.identity.binding.sandbox.clone(),
                "namespace-uid".into(),
                0,
                0,
            ),
            false,
        )));
        let state = ledger.clone();
        Mock::given(wiremock::matchers::method("POST"))
            .respond_with(move |request: &Request| {
                assert_eq!(
                    request.headers.get("authorization").unwrap(),
                    "Bearer test-private-token"
                );
                let mut state = state.lock().unwrap();
                if request.url.path() == "/v1/calendar/commit" {
                    let command: Commit = serde_json::from_slice(&request.body).unwrap();
                    let now = Utc::now();
                    state
                        .0
                        .commit(
                            &command,
                            now.date_naive().num_days_from_ce() as u32,
                            now.year() as u32 * 12 + now.month0(),
                        )
                        .unwrap();
                    if command.event == Event::Finish && !state.1 {
                        state.1 = true;
                        return ResponseTemplate::new(503);
                    }
                } else {
                    let query: Read = serde_json::from_slice(&request.body).unwrap();
                    if state.0.check_reader(&query.writer).is_err() {
                        return ResponseTemplate::new(503);
                    }
                }
                ResponseTemplate::new(200).set_body_json(&state.0.usage)
            })
            .mount(&server)
            .await;
        (server, first)
    }

    #[tokio::test]
    async fn calendar_backend_survives_router_replacement_and_lost_commit_acknowledgement() {
        let (server, first) = accounting_server().await;
        let tracker = TokenBudgetTracker::with_calendar(1, 16, first.clone());
        assert!(
            tracker
                .check_budget("sandbox", Some(1), Some(512))
                .await
                .is_ok()
        );
        let mut upstream = crate::proxy::UpstreamConfig::azure(
            "https://unused.invalid".into(),
            "model".into(),
            "sandbox".into(),
        );
        upstream.calendar_budget = Some(first.clone());
        let (_, guard) = super::super::dispatch::begin(
            &upstream,
            &axum::http::Method::POST,
            "/chat/completions",
            bytes::Bytes::from_static(b"{}"),
        )
        .await
        .unwrap();
        super::super::dispatch::finish(
            guard,
            axum::http::StatusCode::OK,
            br#"{"usage":{"prompt_tokens":34,"completion_tokens":5,"total_tokens":39}}"#,
        )
        .await
        .unwrap();
        tracker.record_usage("sandbox", 39).await;
        assert!(
            tracker
                .check_budget("sandbox", Some(1), Some(512))
                .await
                .unwrap_err()
                .contains("Daily")
        );
        let second = client(server.uri()).await;
        let restarted = TokenBudgetTracker::with_calendar(0, 16, second.clone());
        assert!(
            restarted
                .check_budget("sandbox", Some(0), Some(39))
                .await
                .unwrap_err()
                .contains("Monthly")
        );
        assert_eq!(restarted.observed_usage("sandbox").await.unwrap(), (39, 39));
        assert_eq!(first.read().await.unwrap().monthly, 39);
        for (path, body) in [
            (
                "/responses",
                r#"{"usage":{"input_tokens":34,"output_tokens":5,"total_tokens":39}}"#,
            ),
            (
                "/v1/messages",
                r#"{"usage":{"input_tokens":30,"cache_read_input_tokens":4,"output_tokens":5}}"#,
            ),
        ] {
            let (_, guard) = super::super::dispatch::begin(
                &upstream,
                &axum::http::Method::POST,
                path,
                bytes::Bytes::from_static(b"{}"),
            )
            .await
            .unwrap();
            super::super::dispatch::finish(guard, axum::http::StatusCode::OK, body.as_bytes())
                .await
                .unwrap();
        }
        use futures::StreamExt;
        for (path, body) in [
            (
                "/chat/completions",
                "data: {\"usage\":{\"prompt_tokens\":34,\"completion_tokens\":5,\"total_tokens\":39}}\n\ndata: [DONE]\n\n",
            ),
            (
                "/responses",
                "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":34,\"output_tokens\":5,\"total_tokens\":39}}}\n\n",
            ),
            (
                "/v1/messages",
                "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":34,\"output_tokens\":0}}}\n\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"text\":\"ok\"}}\n\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":5}}\n\ndata: {\"type\":\"message_stop\"}\n\n",
            ),
        ] {
            let (_, guard) = super::super::dispatch::begin(
                &upstream,
                &axum::http::Method::POST,
                path,
                bytes::Bytes::from_static(b"{}"),
            )
            .await
            .unwrap();
            let source =
                futures::stream::iter([Ok(bytes::Bytes::copy_from_slice(body.as_bytes()))]).boxed();
            let mut stream = super::super::dispatch::stream(source, guard, true);
            assert_eq!(stream.next().await.unwrap().unwrap(), body.as_bytes());
            drop(stream);
        }
        assert_eq!(first.read().await.unwrap().monthly, 234);
        let (_, guard) = super::super::dispatch::begin(
            &upstream,
            &axum::http::Method::POST,
            "/chat/completions",
            bytes::Bytes::from_static(b"{}"),
        )
        .await
        .unwrap();
        super::super::dispatch::finish(
            guard,
            axum::http::StatusCode::BAD_REQUEST,
            br#"{"error":{"code":"unsupported_api_for_model"}}"#,
        )
        .await
        .unwrap();
        assert_eq!(first.read().await.unwrap().monthly, 234);
        let (_, guard) = super::super::dispatch::begin(
            &upstream,
            &axum::http::Method::POST,
            "/chat/completions",
            bytes::Bytes::from_static(b"{}"),
        )
        .await
        .unwrap();
        let unfinished =
            super::super::dispatch::stream(futures::stream::pending().boxed(), guard, true);
        assert!(restarted.observed_usage("sandbox").await.is_err());
        drop(unfinished);
        let third = client(server.uri()).await;
        assert!(third.read().await.is_err());
        assert!(first.read().await.is_err());
        tokio::fs::remove_file(&first.token_path).await.unwrap();
        tokio::fs::remove_file(&second.token_path).await.unwrap();
        tokio::fs::remove_file(&third.token_path).await.unwrap();
    }

    #[tokio::test]
    async fn calendar_unknown_outcomes_and_incomplete_streams_keep_durable_liability() {
        use axum::http::{Method, StatusCode};
        use bytes::Bytes;
        use futures::StreamExt;
        for (status, body, streaming) in [
            (StatusCode::OK, "{}", false),
            (
                StatusCode::BAD_REQUEST,
                r#"{"error":{"message":"unsupported_api_for_model"}}"#,
                false,
            ),
            (
                StatusCode::SERVICE_UNAVAILABLE,
                r#"{"error":{"code":"unsupported_api_for_model"}}"#,
                false,
            ),
            (StatusCode::OK, "data: {}\n\ndata: [DONE]\n\n", true),
            (
                StatusCode::OK,
                "data: {\"usage\":{\"prompt_tokens\":34,\"completion_tokens\":5,\"total_tokens\":39}}\n\n",
                true,
            ),
        ] {
            let (server, first) = accounting_server().await;
            let mut upstream = crate::proxy::UpstreamConfig::azure(
                "https://unused.invalid".into(),
                "model".into(),
                "sandbox".into(),
            );
            upstream.calendar_budget = Some(first.clone());
            let (_, guard) = super::super::dispatch::begin(
                &upstream,
                &Method::POST,
                "/chat/completions",
                Bytes::from_static(b"{}"),
            )
            .await
            .unwrap();
            if streaming {
                let source =
                    futures::stream::iter([Ok(Bytes::copy_from_slice(body.as_bytes()))]).boxed();
                let mut stream = super::super::dispatch::stream(source, guard, true);
                let mut failed = false;
                while let Some(chunk) = stream.next().await {
                    if chunk.is_err() {
                        failed = true;
                        break;
                    }
                }
                assert!(
                    failed,
                    "missing final usage must reach the consumer as an error"
                );
            } else {
                let error = super::super::dispatch::finish(guard, status, body.as_bytes())
                    .await
                    .unwrap_err();
                assert_eq!(
                    super::super::response::denial(&error).unwrap().status(),
                    StatusCode::SERVICE_UNAVAILABLE
                );
                assert!(!crate::proxy::failure::retryable_failure(&error));
            }
            assert!(first.read().await.is_err());
            assert!(client(server.uri()).await.read().await.is_err());
        }
    }

    #[tokio::test]
    async fn missing_or_unacknowledged_calendar_state_blocks_inference_and_readiness() {
        let server = MockServer::start().await;
        let client = client(server.uri()).await;
        Mock::given(wiremock::matchers::method("POST"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        let tracker = TokenBudgetTracker::with_calendar(1, 16, client.clone());
        assert_eq!(
            tracker
                .check_budget("sandbox", None, None)
                .await
                .unwrap_err()
                .status(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );
        assert!(client.begin().await.is_err());
        server.reset().await;
        assert!(tracker.observed_usage("sandbox").await.is_err());
        assert!(server.received_requests().await.unwrap().is_empty());
        tokio::fs::remove_file(&client.token_path).await.unwrap();
    }

    #[tokio::test]
    async fn invalid_calendar_responses_and_sandbox_aliases_are_not_zero_usage() {
        let server = MockServer::start().await;
        let client = client(server.uri()).await;
        let tracker = TokenBudgetTracker::with_calendar(1, 16, client.clone());
        assert!(tracker.observed_usage("another-sandbox").await.is_err());
        assert!(server.received_requests().await.unwrap().is_empty());
        for body in ["{}".to_string(), "x".repeat(4097)] {
            server.reset().await;
            Mock::given(wiremock::matchers::method("POST"))
                .respond_with(ResponseTemplate::new(200).set_body_string(body))
                .mount(&server)
                .await;
            assert_eq!(
                tracker
                    .check_budget("sandbox", None, None)
                    .await
                    .unwrap_err()
                    .status(),
                axum::http::StatusCode::SERVICE_UNAVAILABLE
            );
        }
        tokio::fs::remove_file(&client.token_path).await.unwrap();
    }
}

#[derive(Debug)]
pub struct Client {
    identity: Identity,
    writer: String,
    endpoint: String,
    http: reqwest::Client,
    token_path: PathBuf,
    sequence: Mutex<Sequence>,
    uncertain: AtomicBool,
}

pub struct Attempt {
    client: Arc<Client>,
    finished: bool,
}

impl Attempt {
    pub async fn finish(mut self, tokens: u64) -> Result<(), Error> {
        self.client.event(Event::Finish, tokens).await?;
        self.finished = true;
        Ok(())
    }
}

impl Drop for Attempt {
    fn drop(&mut self) {
        if !self.finished {
            self.client.uncertain.store(true, Ordering::Release);
            tracing::error!(
                "Calendar dispatch has unresolved usage; durable pending liability retained"
            );
        }
    }
}

impl Client {
    pub fn from_env() -> Result<Option<Arc<Self>>, Error> {
        let encoded = match std::env::var(BINDING_ENV) {
            Ok(encoded) => encoded,
            Err(std::env::VarError::NotPresent) => return Ok(None),
            Err(_) => return Err(Error("binding configuration")),
        };
        let binding: Binding = serde_json::from_str(&encoded).map_err(|_| Error("binding"))?;
        let identity = Identity {
            binding,
            pod_name: std::env::var("POD_NAME").map_err(|_| Error("Pod name"))?,
            pod_uid: std::env::var("POD_UID").map_err(|_| Error("Pod UID"))?,
        };
        identity.validate().map_err(|_| Error("identity"))?;
        let (endpoint, http) =
            super::client::transport_from_env().map_err(|_| Error("TLS transport"))?;
        Ok(Some(Arc::new(Self {
            identity,
            writer: hex::encode(rand::random::<[u8; 16]>()),
            uncertain: AtomicBool::new(false),
            endpoint,
            http,
            token_path: "/var/run/kars/inference-budget/token".into(),
            sequence: Mutex::new(Sequence {
                next: 1,
                failed: false,
            }),
        })))
    }

    pub fn sandbox(&self) -> &str {
        &self.identity.binding.sandbox.name
    }

    async fn rpc(&self, path: &'static str, body: &impl Serialize) -> Result<Usage, Error> {
        let token = tokio::fs::read_to_string(&self.token_path)
            .await
            .map_err(|_| Error("private token"))?;
        if token.trim().is_empty() {
            return Err(Error("private token"));
        }
        let mut response = self
            .http
            .post(format!("{}{path}", self.endpoint))
            .bearer_auth(token.trim())
            .json(body)
            .send()
            .await
            .map_err(|_| Error(path))?;
        if !response.status().is_success() {
            return Err(Error(path));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| Error("response"))? {
            if bytes.len() + chunk.len() > 4096 {
                return Err(Error("response bound"));
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| Error("response schema"))
    }

    pub async fn read(&self) -> Result<Usage, Error> {
        let sequence = self.sequence.lock().await;
        if sequence.failed || self.uncertain.load(Ordering::Acquire) {
            return Err(Error("unacknowledged usage"));
        }
        self.rpc(
            "/v1/calendar/read",
            &Read {
                identity: self.identity.clone(),
                writer: self.writer.clone(),
            },
        )
        .await
    }

    pub async fn begin(self: &Arc<Self>) -> Result<Attempt, Error> {
        self.event(Event::Begin, 0).await?;
        Ok(Attempt {
            client: self.clone(),
            finished: false,
        })
    }

    async fn event(&self, event: Event, tokens: u64) -> Result<Usage, Error> {
        let mut sequence = self.sequence.lock().await;
        if sequence.failed || (event == Event::Begin && self.uncertain.load(Ordering::Acquire)) {
            return Err(Error("unacknowledged usage"));
        }
        let request = Commit {
            identity: self.identity.clone(),
            writer: self.writer.clone(),
            sequence: sequence.next,
            tokens,
            event,
        };
        // The durable per-writer sequence makes a lost acknowledgement safe to retry.
        let mut result = self.rpc("/v1/calendar/commit", &request).await;
        if result.is_err() {
            result = self.rpc("/v1/calendar/commit", &request).await;
        }
        match result {
            Ok(usage) => {
                let Some(next) = sequence.next.checked_add(1) else {
                    sequence.failed = true;
                    return Err(Error("sequence overflow"));
                };
                sequence.next = next;
                Ok(usage)
            }
            Err(error) => {
                sequence.failed = true;
                tracing::error!(error = %error, "Calendar usage could not be persisted; further inference blocked");
                Err(error)
            }
        }
    }
}
