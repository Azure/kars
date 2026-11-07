// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::governed_services::GovernedServices;

const EXPLANATION: &str = "requires-fresh-run: Human review approved Tier 3, but current authority is unchanged. A fresh authorized run is required.";

fn fixture(kind: &str) -> (GovernedServices, Request, Entry) {
    let services = GovernedServices::new(Identity::standalone("test"), None);
    let request = Request {
        scope_id: services.requests.scope().unwrap().id,
        kind: kind.into(),
        target: if kind == "tier" { "" } else { "example.com" }.into(),
        reason: "Need more autonomy".into(),
        tier: (kind == "tier").then_some(3),
        port: (kind == "egress").then_some(443),
    };
    let entry = services.requests.record(request.clone()).unwrap().0;
    (services, request, entry)
}

#[tokio::test]
async fn reviewed_tier_cancellation_wakes_waiter_with_non_grant_explanation_and_deduplicates() {
    let (services, request, pending) = fixture("tier");
    let buffer = &services.requests;
    let scope = &request.scope_id;
    let id = &pending.request_id;
    let mut changed = buffer.subscribe();
    let waiter = services.wait_for_decision(scope, id, Duration::from_secs(5));
    tokio::pin!(waiter);
    tokio::select! {
        biased;
        _ = &mut waiter => panic!("Pending request returned before its outcome"),
        _ = std::future::ready(()) => {},
    }
    let cancelled = buffer
        .cancel_reviewed_tier(scope, id, EXPLANATION.into())
        .unwrap();
    tokio::time::timeout(Duration::from_millis(100), changed.changed())
        .await
        .unwrap()
        .unwrap();
    let result = tokio::time::timeout(Duration::from_millis(100), waiter)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.status, Status::Cancelled);
    assert_eq!(result.explanation.as_deref(), Some(EXPLANATION));
    assert_eq!(result.decision, None);
    assert_eq!(result.answer, None);
    assert_eq!(result.reason, pending.reason);
    assert_eq!(result.expires_at_unix, pending.expires_at_unix);
    assert_eq!(cancelled.explanation, result.explanation);
    let (duplicate, new) = buffer.record(request.clone()).unwrap();
    assert!(!new);
    assert_eq!(duplicate.request_id, pending.request_id);
    assert_eq!(duplicate.status, Status::Cancelled);
    assert_eq!(duplicate.explanation.as_deref(), Some(EXPLANATION));
    assert_eq!(
        serde_json::to_value(&duplicate).unwrap()["explanation"],
        EXPLANATION
    );
    for status in [Status::Approved, Status::Denied, Status::Cancelled] {
        assert!(matches!(
            buffer.transition(scope, id, status),
            Err(Error::Terminal)
        ));
    }
    assert!(services.claim_egress_dispatch(scope, Some(id)).is_err());
}

#[test]
fn reviewed_tier_cancellation_requires_bounded_explanation_and_exact_pending_tier() {
    let (services, request, entry) = fixture("tier");
    let buffer = &services.requests;
    for explanation in [String::new(), " \n".into(), "é".repeat(4097)] {
        assert!(matches!(
            buffer.cancel_reviewed_tier(&request.scope_id, &entry.request_id, explanation),
            Err(Error::Invalid)
        ));
    }
    assert!(matches!(
        buffer.cancel_reviewed_tier("stale", &entry.request_id, EXPLANATION.into()),
        Err(Error::StaleScope)
    ));
    assert!(matches!(
        buffer.cancel_reviewed_tier(&request.scope_id, "missing", EXPLANATION.into()),
        Err(Error::Missing)
    ));
    assert_eq!(
        buffer
            .entry(&request.scope_id, &entry.request_id)
            .unwrap()
            .status,
        Status::Pending
    );
    assert!(
        buffer
            .entry(&request.scope_id, &entry.request_id)
            .unwrap()
            .explanation
            .is_none()
    );
    let limit = "é".repeat(4096);
    assert_eq!(
        buffer
            .cancel_reviewed_tier(&request.scope_id, &entry.request_id, limit.clone())
            .unwrap()
            .explanation,
        Some(limit)
    );

    for kind in ["egress", "clarification", "permission"] {
        let (services, request, entry) = fixture(kind);
        assert!(matches!(
            services.requests.cancel_reviewed_tier(
                &request.scope_id,
                &entry.request_id,
                EXPLANATION.into()
            ),
            Err(Error::Invalid)
        ));
        assert_eq!(
            services
                .requests
                .entry(&request.scope_id, &entry.request_id)
                .unwrap()
                .status,
            Status::Pending
        );
    }
    for status in [
        Status::Approved,
        Status::Denied,
        Status::Cancelled,
        Status::DispatchClaimed,
        Status::Expired,
    ] {
        let (services, request, entry) = fixture("tier");
        {
            let mut inner = services.requests.inner.lock().unwrap();
            let current = inner.requests.front_mut().unwrap();
            current.status = status;
            if status == Status::Expired {
                current.expires = Instant::now();
            }
        }
        assert!(
            services
                .requests
                .cancel_reviewed_tier(&request.scope_id, &entry.request_id, EXPLANATION.into())
                .is_err()
        );
        let current = services
            .requests
            .entry(&request.scope_id, &entry.request_id)
            .unwrap();
        assert_eq!(current.status, status);
        assert!(current.explanation.is_none());
    }
}
