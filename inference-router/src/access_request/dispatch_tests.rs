// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::governed_services::GovernedServices;
use std::sync::Arc;
use tokio::sync::{RwLock, oneshot};
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

fn approved(port: u16) -> (Arc<GovernedServices>, String, String) {
    let services = Arc::new(GovernedServices::new(Identity::standalone("test"), None));
    let scope = services.requests.scope().unwrap().id;
    let (entry, _) = services
        .requests
        .record(Request {
            scope_id: scope.clone(),
            kind: "egress".into(),
            target: "dispatch.example".into(),
            reason: String::new(),
            tier: None,
            port: Some(port),
        })
        .unwrap();
    services
        .requests
        .transition(&scope, &entry.request_id, Status::Approved)
        .unwrap();
    (services, scope, entry.request_id)
}

#[tokio::test]
async fn cancellation_expiry_reset_and_shutdown_during_policy_await_prevent_post_dispatch() {
    for action in ["cancel", "expire", "reset", "shutdown"] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        let address = reqwest::Url::parse(&server.uri())
            .unwrap()
            .socket_addrs(|| None)
            .unwrap()[0];
        let target = format!("http://dispatch.example:{}/side-effect", address.port());
        let client = reqwest::Client::builder()
            .no_proxy()
            .resolve("dispatch.example", address)
            .build()
            .unwrap();
        let (services, scope, id) = approved(address.port());
        let policy = Arc::new(RwLock::new(false));
        let mut held = policy.write().await;
        let (entered, observing) = oneshot::channel();
        let entered = Arc::new(Mutex::new(Some(entered)));
        let worker = services.clone();
        let worker_scope = scope.clone();
        let worker_id = id.clone();
        let policy_for_worker = policy.clone();
        let task = tokio::spawn(async move {
            worker
                .wait_for_egress_check(
                    &worker_scope,
                    &worker_id,
                    &target,
                    "test",
                    Duration::from_secs(5),
                    || {
                        let policy = policy_for_worker.clone();
                        let entered = entered.clone();
                        async move {
                            if let Some(sender) = entered.lock().unwrap().take() {
                                let _ = sender.send(());
                            }
                            if *policy.read().await {
                                Ok(())
                            } else {
                                Err("blocked".to_string())
                            }
                        }
                    },
                )
                .await?;
            let _claim = worker.claim_egress_dispatch(&worker_scope, Some(&worker_id))?;
            client
                .post(&target)
                .body("side-effect")
                .send()
                .await
                .map_err(|_| Error::Unavailable)?;
            Ok::<(), Error>(())
        });
        observing.await.unwrap();
        let expected = match action {
            "cancel" => {
                let cancelled = services
                    .requests
                    .transition(&scope, &id, Status::Cancelled)
                    .unwrap();
                assert_eq!(cancelled.status, Status::Cancelled);
                Error::Terminal
            }
            "expire" => {
                // Fixture mutation advances expiry only after the approved
                // waiter is demonstrably blocked in its asynchronous check.
                services
                    .requests
                    .inner
                    .lock()
                    .unwrap()
                    .requests
                    .iter_mut()
                    .find(|entry| entry.request_id == id)
                    .unwrap()
                    .expires = Instant::now();
                Error::Expired
            }
            "reset" => {
                services
                    .reset(&scope, Some("next-assignment".into()))
                    .unwrap();
                Error::StaleScope
            }
            _ => {
                services.shutdown.cancel();
                Error::Unavailable
            }
        };
        *held = true;
        drop(held);
        assert_eq!(task.await.unwrap(), Err(expected), "{action}");
        assert!(
            server.received_requests().await.unwrap().is_empty(),
            "{action}"
        );
    }
}

#[test]
fn acknowledged_cancel_or_reset_before_claim_wins_and_claims_are_single_use() {
    let (services, scope, id) = approved(443);
    services
        .requests
        .validate_dispatch(&scope, &id, &services.shutdown)
        .unwrap();
    services
        .requests
        .transition(&scope, &id, Status::Cancelled)
        .unwrap();
    assert!(matches!(
        services.claim_egress_dispatch(&scope, Some(&id)),
        Err(Error::Terminal)
    ));

    let (services, scope, id) = approved(443);
    services.reset(&scope, Some("next".into())).unwrap();
    assert!(matches!(
        services.claim_egress_dispatch(&scope, Some(&id)),
        Err(Error::StaleScope)
    ));

    let (services, scope, id) = approved(443);
    let claim = services.claim_egress_dispatch(&scope, Some(&id)).unwrap();
    assert_eq!(
        services.requests.entry(&scope, &id).unwrap().status,
        Status::DispatchClaimed
    );
    assert!(
        services
            .requests
            .entry(&scope, &id)
            .unwrap()
            .dispatch_active
    );
    assert!(matches!(
        services.requests.transition(&scope, &id, Status::Cancelled),
        Err(Error::Terminal)
    ));
    assert!(matches!(
        services.claim_egress_dispatch(&scope, Some(&id)),
        Err(Error::Terminal)
    ));
    assert!(matches!(services.reset(&scope, None), Err(Error::Terminal)));
    drop(claim);
    assert!(
        !services
            .requests
            .entry(&scope, &id)
            .unwrap()
            .dispatch_active
    );
    assert!(matches!(
        services.requests.transition(&scope, &id, Status::Cancelled),
        Err(Error::Terminal)
    ));
    services.reset(&scope, None).unwrap();
}

#[test]
fn expiry_and_shutdown_between_readiness_and_claim_still_prevent_dispatch() {
    for expired in [true, false] {
        let (services, scope, id) = approved(443);
        services
            .requests
            .validate_dispatch(&scope, &id, &services.shutdown)
            .unwrap();
        if expired {
            services.requests.inner.lock().unwrap().requests[0].expires = Instant::now();
        } else {
            services.shutdown.cancel();
        }
        assert!(matches!(
            services.claim_egress_dispatch(&scope, Some(&id)),
            Err(Error::Expired | Error::Unavailable)
        ));
    }
}

#[test]
fn scope_only_dispatch_also_prevents_successful_reset_until_released() {
    let (services, scope, _) = approved(443);
    let claim = services.claim_egress_dispatch(&scope, None).unwrap();
    assert!(matches!(services.reset(&scope, None), Err(Error::Terminal)));
    drop(claim);
    services.reset(&scope, None).unwrap();
}
