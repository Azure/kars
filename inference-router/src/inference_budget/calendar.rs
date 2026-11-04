// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::inference_budget_contract::calendar::{
    BINDING_ENV, Binding, Commit, Event, Identity, Observation, PENDING_HEADER, POLICY_HEADER,
    Policy, Read,
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
pub(super) mod tests {
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

    pub(crate) async fn accounting_server() -> (MockServer, Arc<Client>) {
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
        assert_eq!(first.read().await.unwrap().usage.monthly, 39);
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
        assert_eq!(first.read().await.unwrap().usage.monthly, 234);
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
        assert_eq!(first.read().await.unwrap().usage.monthly, 234);
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

    struct PolicyState {
        ledger: Ledger,
        policy: Policy,
        lose_finish_ack: bool,
        revoke_on_begin: bool,
    }

    async fn policy_server() -> (MockServer, Arc<Client>, Arc<StdMutex<PolicyState>>) {
        let server = MockServer::start().await;
        let first = client(server.uri()).await;
        let state = Arc::new(StdMutex::new(PolicyState {
            ledger: Ledger::new(
                first.identity.binding.sandbox.clone(),
                "namespace-uid".into(),
                0,
                0,
            ),
            policy: Policy::RecordOnly,
            lose_finish_ack: false,
            revoke_on_begin: false,
        }));
        let retained = state.clone();
        Mock::given(wiremock::matchers::method("POST"))
            .respond_with(move |request: &Request| {
                let mut state = retained.lock().unwrap();
                let mut policy = state.policy;
                if request.url.path() == "/v1/calendar/commit" {
                    let command: Commit = serde_json::from_slice(&request.body).unwrap();
                    if command.event == Event::Begin && state.revoke_on_begin {
                        state.policy = Policy::Enforce;
                        policy = Policy::Enforce;
                    }
                    let now = Utc::now();
                    if state
                        .ledger
                        .commit_with_policy(
                            &command,
                            now.date_naive().num_days_from_ce() as u32,
                            now.year() as u32 * 12 + now.month0(),
                            false,
                            policy,
                        )
                        .is_err()
                    {
                        return ResponseTemplate::new(503);
                    }
                    if command.event == Event::Finish && state.lose_finish_ack {
                        return ResponseTemplate::new(503);
                    }
                } else {
                    let read: Read = serde_json::from_slice(&request.body).unwrap();
                    if state
                        .ledger
                        .check_reader_with_policy(&read.writer, policy)
                        .is_err()
                    {
                        return ResponseTemplate::new(503);
                    }
                }
                let observation = state.ledger.observation(policy).unwrap();
                ResponseTemplate::new(200)
                    .insert_header(POLICY_HEADER, policy.as_str())
                    .insert_header(
                        PENDING_HEADER,
                        observation.pending_requests.unwrap().to_string(),
                    )
                    .set_body_json(&observation.usage)
            })
            .mount(&server)
            .await;
        (server, first, state)
    }

    fn privacy_pending() -> ResponseTemplate {
        ResponseTemplate::new(503).set_body_json(serde_json::json!({
            "error": {"code": "inference_privacy_pending"}
        }))
    }

    #[tokio::test]
    async fn privacy_pending_preserves_calendar_identity_and_refreshes_token() {
        for policy in [Policy::Enforce, Policy::RecordOnly] {
            for path in ["/v1/calendar/read", "/v1/calendar/commit"] {
                let (server, first, state) = policy_server().await;
                state.lock().unwrap().policy = policy;
                let retained = state.clone();
                let token_path = first.token_path.clone();
                Mock::given(wiremock::matchers::path(path))
                    .respond_with(move |_: &Request| {
                        assert_eq!(
                            retained
                                .lock()
                                .unwrap()
                                .ledger
                                .observation(policy)
                                .unwrap()
                                .pending_requests,
                            Some(0)
                        );
                        std::fs::write(&token_path, b"refreshed-private-token").unwrap();
                        privacy_pending()
                    })
                    .with_priority(1)
                    .up_to_n_times(1)
                    .mount(&server)
                    .await;
                if path == "/v1/calendar/read" {
                    assert_eq!(first.read().await.unwrap().pending_requests, Some(0));
                }
                let attempt = first.begin().await.unwrap();
                let requests = server.received_requests().await.unwrap();
                assert_eq!(requests.len(), if path.ends_with("read") { 3 } else { 2 });
                assert_eq!(requests[0].body, requests[1].body);
                assert_eq!(
                    requests[1].headers.get("authorization").unwrap(),
                    "Bearer refreshed-private-token"
                );
                assert_eq!(first.sequence.lock().await.next, 2);
                assert_eq!(
                    state
                        .lock()
                        .unwrap()
                        .ledger
                        .observation(policy)
                        .unwrap()
                        .pending_requests,
                    Some(1)
                );
                attempt.finish(7).await.unwrap();
                let observation = first.read().await.unwrap();
                assert_eq!(observation.pending_requests, Some(0));
                assert_eq!(observation.usage.monthly, 7);
                assert!(!first.sequence.lock().await.failed);
                tokio::fs::remove_file(&first.token_path).await.unwrap();
            }
        }
    }

    #[tokio::test]
    async fn privacy_pending_does_not_override_calendar_revocation() {
        for path in ["/v1/calendar/read", "/v1/calendar/commit"] {
            let (server, first, state) = policy_server().await;
            Mock::given(wiremock::matchers::path(path))
                .respond_with(privacy_pending())
                .with_priority(1)
                .up_to_n_times(1)
                .mount(&server)
                .await;
            Mock::given(wiremock::matchers::path(path))
                .respond_with(ResponseTemplate::new(403))
                .with_priority(2)
                .mount(&server)
                .await;
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                if path.ends_with("read") {
                    assert!(first.read().await.is_err());
                } else {
                    assert!(first.begin().await.is_err());
                }
            })
            .await
            .unwrap();
            let requests = server.received_requests().await.unwrap();
            // Commit retains its existing one-shot lost-acknowledgement retry.
            assert_eq!(requests.len(), if path.ends_with("read") { 2 } else { 3 });
            assert!(
                requests
                    .iter()
                    .all(|request| request.body == requests[0].body)
            );
            assert_eq!(
                state
                    .lock()
                    .unwrap()
                    .ledger
                    .observation(Policy::Enforce)
                    .unwrap()
                    .pending_requests,
                Some(0)
            );
            assert_eq!(first.sequence.lock().await.next, 1);
            tokio::fs::remove_file(&first.token_path).await.unwrap();
        }
    }

    #[tokio::test]
    async fn cancelling_calendar_privacy_wait_leaves_no_detached_retries() {
        let (server, first, state) = policy_server().await;
        Mock::given(wiremock::matchers::path("/v1/calendar/read"))
            .respond_with(privacy_pending())
            .with_priority(1)
            .mount(&server)
            .await;
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(200), first.read())
                .await
                .is_err()
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
        assert_eq!(
            state
                .lock()
                .unwrap()
                .ledger
                .observation(Policy::Enforce)
                .unwrap()
                .pending_requests,
            Some(0)
        );
        assert_eq!(first.sequence.lock().await.next, 1);
        assert!(!first.sequence.lock().await.failed);
        tokio::fs::remove_file(&first.token_path).await.unwrap();
    }

    #[tokio::test]
    async fn calendar_privacy_wait_excludes_finish_and_unclassified_errors() {
        let (server, first, state) = policy_server().await;
        let attempt = first.begin().await.unwrap();
        server.reset().await;
        Mock::given(wiremock::matchers::path("/v1/calendar/commit"))
            .respond_with(privacy_pending())
            .mount(&server)
            .await;
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(750), attempt.finish(7))
                .await
                .unwrap()
                .is_err()
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
        assert!(first.sequence.lock().await.failed);
        assert_eq!(
            state
                .lock()
                .unwrap()
                .ledger
                .observation(Policy::Enforce)
                .unwrap()
                .pending_requests,
            Some(1)
        );
        tokio::fs::remove_file(&first.token_path).await.unwrap();

        let first = client(server.uri()).await;
        for (status, body) in [
            (
                403,
                r#"{"error":{"code":"inference_privacy_pending"}}"#.to_string(),
            ),
            (
                429,
                r#"{"error":{"code":"inference_privacy_pending"}}"#.to_string(),
            ),
            (
                503,
                r#"{"error":{"code":"inference_budget_authority"}}"#.to_string(),
            ),
            (503, "invalid".to_string()),
            (503, "x".repeat(8193)),
        ] {
            server.reset().await;
            Mock::given(wiremock::matchers::path("/v1/calendar/read"))
                .respond_with(ResponseTemplate::new(status).set_body_string(body))
                .mount(&server)
                .await;
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(750), first.read())
                    .await
                    .unwrap()
                    .is_err()
            );
            assert_eq!(server.received_requests().await.unwrap().len(), 1);
        }
        tokio::fs::remove_file(&first.token_path).await.unwrap();
    }

    pub(crate) async fn record_only_server() -> (MockServer, Arc<Client>) {
        let (server, client, _) = policy_server().await;
        (server, client)
    }

    #[tokio::test]
    async fn pending_refusal_requires_current_record_only_policy_and_acknowledged_liability() {
        for policy in [Policy::Enforce, Policy::RecordOnly] {
            let (_server, first, state) = policy_server().await;
            let attempt = first.begin().await.unwrap();
            state.lock().unwrap().policy = policy;
            assert_eq!(
                attempt.confirm_record_only_pending().await.is_ok(),
                policy == Policy::RecordOnly
            );
            assert!(first.uncertain.load(Ordering::Acquire));
            let state = state.lock().unwrap();
            assert_eq!(state.ledger.usage.daily, 0);
            assert_eq!(
                state
                    .ledger
                    .observation(Policy::RecordOnly)
                    .unwrap()
                    .pending_requests,
                Some(1)
            );
            drop(state);
            tokio::fs::remove_file(&first.token_path).await.unwrap();
        }
    }

    #[tokio::test]
    async fn pending_refusal_never_hides_failed_reads_missing_liability_or_failed_commits() {
        for failure in ["read", "pending", "commit"] {
            let (server, first, _state) = policy_server().await;
            let attempt = first.begin().await.unwrap();
            if failure == "commit" {
                let other = first.begin().await.unwrap();
                Mock::given(wiremock::matchers::path("/v1/calendar/commit"))
                    .respond_with(ResponseTemplate::new(503))
                    .with_priority(1)
                    .mount(&server)
                    .await;
                assert!(other.finish(7).await.is_err());
            } else {
                let response = if failure == "read" {
                    ResponseTemplate::new(503)
                } else {
                    ResponseTemplate::new(200)
                        .insert_header(POLICY_HEADER, "record-only")
                        .insert_header(PENDING_HEADER, "0")
                        .set_body_json(serde_json::json!({"day":0,"month":0,"daily":0,"monthly":0}))
                };
                Mock::given(wiremock::matchers::path("/v1/calendar/read"))
                    .respond_with(response)
                    .with_priority(1)
                    .mount(&server)
                    .await;
            }
            assert!(attempt.confirm_record_only_pending().await.is_err());
            assert!(first.uncertain.load(Ordering::Acquire));
            tokio::fs::remove_file(&first.token_path).await.unwrap();
        }
    }

    #[tokio::test]
    async fn record_only_pending_liability_does_not_make_other_responses_successful() {
        use crate::inference_budget::dispatch;
        for (status, body) in [
            (200, r#"{"error":{"code":"content_filter"}}"#),
            (404, r#"{"error":{"code":"content_filter"}}"#),
            (400, r#"{"error":{"code":"invalid_request"}}"#),
            (403, "not JSON"),
        ] {
            let (_server, first, state) = policy_server().await;
            let mut upstream = crate::proxy::UpstreamConfig::azure(
                "https://unused.invalid".into(),
                "model".into(),
                "sandbox".into(),
            );
            upstream.calendar_budget = Some(first.clone());
            let (_, attempt) = dispatch::begin(
                &upstream,
                &axum::http::Method::POST,
                "/chat/completions",
                bytes::Bytes::from_static(b"{}"),
            )
            .await
            .unwrap();
            let error = dispatch::finish(
                attempt,
                axum::http::StatusCode::from_u16(status).unwrap(),
                body.as_bytes(),
            )
            .await
            .unwrap_err();
            assert!(!crate::proxy::failure::retryable_failure(&error));
            assert_eq!(first.read().await.unwrap().pending_requests, Some(1));
            first.begin().await.unwrap().finish(39).await.unwrap();
            assert_eq!(first.read().await.unwrap().usage.daily, 39);
            assert_eq!(first.read().await.unwrap().pending_requests, Some(1));
            state.lock().unwrap().policy = Policy::Enforce;
            assert!(first.begin().await.is_err());
            tokio::fs::remove_file(&first.token_path).await.unwrap();
        }
    }

    #[tokio::test]
    async fn record_only_retains_unknown_requests_records_new_usage_and_reverts() {
        let (server, first, state) = policy_server().await;
        drop(first.begin().await.unwrap());
        let retained = serde_json::to_value(&state.lock().unwrap().ledger).unwrap()["writers"]
            [&first.writer]
            .clone();
        let second = client(server.uri()).await;
        drop(second.begin().await.unwrap());
        second.begin().await.unwrap().finish(39).await.unwrap();
        let tracker = TokenBudgetTracker::with_calendar(1, 16, second.clone());
        tracker
            .check_budget("sandbox", Some(1), Some(1))
            .await
            .unwrap();
        let ((daily, monthly), policy, pending) =
            tracker.observed_accounting("sandbox").await.unwrap();
        assert_eq!(
            (daily, monthly, policy, pending),
            (39, 39, Policy::RecordOnly, Some(2))
        );
        assert_eq!(
            serde_json::to_value(&state.lock().unwrap().ledger).unwrap()["writers"][&first.writer],
            retained
        );
        state.lock().unwrap().policy = Policy::Enforce;
        assert!(second.read().await.is_err());
        assert!(second.begin().await.is_err());
        assert!(tracker.check_budget("sandbox", None, None).await.is_err());
        tokio::fs::remove_file(&first.token_path).await.unwrap();
        tokio::fs::remove_file(&second.token_path).await.unwrap();
    }

    #[tokio::test]
    async fn record_only_never_allows_unacknowledged_commits_or_revoked_dispatch() {
        let (_server, first, state) = policy_server().await;
        drop(first.begin().await.unwrap());
        state.lock().unwrap().revoke_on_begin = true;
        assert!(first.begin().await.is_err());
        assert!(!first.sequence.lock().await.failed);
        assert_eq!(
            state
                .lock()
                .unwrap()
                .ledger
                .observation(Policy::Enforce)
                .unwrap()
                .pending_requests,
            Some(2)
        );
        {
            let mut state = state.lock().unwrap();
            state.policy = Policy::RecordOnly;
            state.revoke_on_begin = false;
            state.lose_finish_ack = true;
        }
        assert!(first.begin().await.unwrap().finish(7).await.is_err());
        assert!(first.sequence.lock().await.failed);
        assert!(first.read().await.is_err());
        assert!(first.begin().await.is_err());
        assert_eq!(state.lock().unwrap().ledger.usage.monthly, 7);
        tokio::fs::remove_file(&first.token_path).await.unwrap();
    }

    #[tokio::test]
    async fn accounting_metadata_is_explicit_and_malformed_values_fail_closed() {
        let server = MockServer::start().await;
        let first = client(server.uri()).await;
        let usage = serde_json::json!({"day":0,"month":0,"daily":0,"monthly":0});
        for (policy, pending, valid) in [
            (None, None, true),
            (Some("enforce"), None, true),
            (Some("record-only"), Some("3"), true),
            (Some("record-only"), None, false),
            (Some("invalid"), Some("0"), false),
            (Some("record-only"), Some("-1"), false),
            (Some("record-only"), Some("+1"), false),
            (Some("record-only"), Some("18446744073709551616"), false),
        ] {
            server.reset().await;
            let mut response = ResponseTemplate::new(200).set_body_json(&usage);
            if let Some(policy) = policy {
                response = response.insert_header(POLICY_HEADER, policy);
            }
            if let Some(pending) = pending {
                response = response.insert_header(PENDING_HEADER, pending);
            }
            Mock::given(wiremock::matchers::method("POST"))
                .respond_with(response)
                .mount(&server)
                .await;
            let result = first.read().await;
            assert_eq!(result.is_ok(), valid, "{policy:?} {pending:?}");
            if policy.is_none() {
                let observed = result.unwrap();
                assert_eq!(observed.policy, Policy::Enforce);
                assert_eq!(observed.pending_requests, None);
            }
        }
        tokio::fs::remove_file(&first.token_path).await.unwrap();
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
    pub(super) async fn confirm_record_only_pending(self) -> Result<(), Error> {
        let observation = self.client.read().await?;
        if observation.policy != Policy::RecordOnly
            || !observation
                .pending_requests
                .is_some_and(|pending| pending > 0)
        {
            return Err(Error(
                "incomplete provider usage; pending liability retained",
            ));
        }
        // Do not finish or clear uncertainty: the acknowledged Begin remains pending.
        Ok(())
    }

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

    async fn rpc(
        &self,
        path: &'static str,
        body: &impl Serialize,
        wait_for_privacy: bool,
    ) -> Result<Observation, Error> {
        let mut response = loop {
            let token = tokio::fs::read_to_string(&self.token_path)
                .await
                .map_err(|_| Error("private token"))?;
            if token.trim().is_empty() {
                return Err(Error("private token"));
            }
            let response = self
                .http
                .post(format!("{}{path}", self.endpoint))
                .bearer_auth(token.trim())
                .json(body)
                .send()
                .await
                .map_err(|_| Error(path))?;
            if response.status().is_success() {
                break response;
            }
            if wait_for_privacy
                && matches!(
                    super::client::broker_denial(path, response).await,
                    Some(super::client::BrokerDenial::PrivacyPending)
                )
            {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                continue;
            }
            return Err(Error(path));
        };
        let headers = response.headers();
        if headers.get_all(POLICY_HEADER).iter().count() > 1
            || headers.get_all(PENDING_HEADER).iter().count() > 1
        {
            return Err(Error("duplicate accounting metadata"));
        }
        let policy = Policy::parse(
            headers
                .get(POLICY_HEADER)
                .map(|value| value.to_str())
                .transpose()
                .map_err(|_| Error("accounting policy"))?,
        )
        .map_err(|_| Error("accounting policy"))?;
        let pending_requests = headers
            .get(PENDING_HEADER)
            .map(|value| {
                let value = value.to_str().map_err(|_| Error("pending requests"))?;
                if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                    return Err(Error("pending requests"));
                }
                value.parse::<u64>().map_err(|_| Error("pending requests"))
            })
            .transpose()?;
        if policy == Policy::RecordOnly && pending_requests.is_none() {
            return Err(Error("missing pending requests"));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| Error("response"))? {
            if bytes.len() + chunk.len() > 4096 {
                return Err(Error("response bound"));
            }
            bytes.extend_from_slice(&chunk);
        }
        let usage = serde_json::from_slice(&bytes).map_err(|_| Error("response schema"))?;
        Ok(Observation {
            usage,
            policy,
            pending_requests,
        })
    }

    async fn read_observation(&self) -> Result<Observation, Error> {
        let observation = self
            .rpc(
                "/v1/calendar/read",
                &Read {
                    identity: self.identity.clone(),
                    writer: self.writer.clone(),
                },
                true,
            )
            .await?;
        if self.uncertain.load(Ordering::Acquire) && observation.policy != Policy::RecordOnly {
            return Err(Error("unresolved usage"));
        }
        Ok(observation)
    }

    pub async fn read(&self) -> Result<Observation, Error> {
        let sequence = self.sequence.lock().await;
        if sequence.failed {
            return Err(Error("unacknowledged usage"));
        }
        self.read_observation().await
    }

    pub async fn begin(self: &Arc<Self>) -> Result<Attempt, Error> {
        self.event(Event::Begin, 0).await?;
        Ok(Attempt {
            client: self.clone(),
            finished: false,
        })
    }

    async fn event(&self, event: Event, tokens: u64) -> Result<Observation, Error> {
        let mut sequence = self.sequence.lock().await;
        if sequence.failed {
            return Err(Error("unacknowledged usage"));
        }
        if event == Event::Begin && self.uncertain.load(Ordering::Acquire) {
            self.read_observation().await?;
        }
        let request = Commit {
            identity: self.identity.clone(),
            writer: self.writer.clone(),
            sequence: sequence.next,
            tokens,
            event,
        };
        // The durable per-writer sequence makes a lost acknowledgement safe to retry.
        let mut result = self
            .rpc("/v1/calendar/commit", &request, event == Event::Begin)
            .await;
        if result.is_err() {
            result = self
                .rpc("/v1/calendar/commit", &request, event == Event::Begin)
                .await;
        }
        match result {
            Ok(usage) => {
                let Some(next) = sequence.next.checked_add(1) else {
                    sequence.failed = true;
                    return Err(Error("sequence overflow"));
                };
                sequence.next = next;
                // A policy revocation between the read and Begin must still prevent dispatch.
                if event == Event::Begin
                    && self.uncertain.load(Ordering::Acquire)
                    && usage.policy != Policy::RecordOnly
                {
                    return Err(Error("unresolved usage"));
                }
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
