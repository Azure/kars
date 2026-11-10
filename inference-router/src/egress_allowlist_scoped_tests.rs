// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::access_request::{Identity, Request, Status};
use crate::governed_services::GovernedServices;
use serde_json::json;

#[tokio::test]
async fn unchanged_mount_refreshes_digest_after_decision_and_scope_reset() {
    let dir = tempfile::tempdir().unwrap();
    let adir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("allowlist.json"),
        br#"{"schemaVersion":1,"endpoints":[]}"#,
    )
    .unwrap();
    let services = Arc::new(GovernedServices::new(Identity::standalone("sandbox"), None));
    let blocklist = Blocklist::new(None).await;
    blocklist.bind_governed_services(services.clone()).unwrap();
    let scope = services.requests.scope().unwrap().id;
    let (entry, _) = services
        .requests
        .record(Request {
            scope_id: scope.clone(),
            kind: "egress".into(),
            target: "approved.example".into(),
            reason: "read report".into(),
            tier: None,
            port: Some(443),
        })
        .unwrap();
    let file = adir.path().join("approval-request.json");
    std::fs::write(
        &file,
        serde_json::to_vec(
            &json!({"schemaVersion":2,"requestScope":scope,"requestId":entry.request_id,
        "scopedHosts":[{"host":"approved.example","port":443}],
        "effectiveAt":(chrono::Utc::now()-chrono::Duration::seconds(10)).to_rfc3339(),
        "expiresAt":(chrono::Utc::now()+chrono::Duration::seconds(300)).to_rfc3339()}),
        )
        .unwrap(),
    )
    .unwrap();
    let mtime = std::fs::metadata(&file).unwrap().modified().unwrap();
    let registry = PolicyStatusRegistry::new();
    let handle = empty_handle();
    load_and_install_with_approvals(
        dir.path().to_str().unwrap(),
        Some(adir.path().to_str().unwrap()),
        &registry,
        &handle,
        &blocklist,
    )
    .await;
    let denied_digest = registry.get(PolicyKind::EgressApproval).unwrap().digest;
    assert!(
        blocklist
            .check_egress_in_scope("https://approved.example", "sandbox", &scope)
            .await
            .is_err()
    );
    services
        .requests
        .transition(&scope, &entry.request_id, Status::Approved)
        .unwrap();
    load_and_install_with_approvals(
        dir.path().to_str().unwrap(),
        Some(adir.path().to_str().unwrap()),
        &registry,
        &handle,
        &blocklist,
    )
    .await;
    assert_ne!(
        registry.get(PolicyKind::EgressApproval).unwrap().digest,
        denied_digest
    );
    assert!(
        blocklist
            .check_egress_in_scope("https://approved.example", "sandbox", &scope)
            .await
            .is_ok()
    );
    assert!(
        blocklist
            .check_egress("https://approved.example", "sandbox")
            .await
            .is_err()
    );
    assert!(blocklist.get_allowlist().await.is_empty());
    services
        .requests
        .reset(&scope, Some("next-assignment".into()))
        .unwrap();
    load_and_install_with_approvals(
        dir.path().to_str().unwrap(),
        Some(adir.path().to_str().unwrap()),
        &registry,
        &handle,
        &blocklist,
    )
    .await;
    assert_eq!(
        registry.get(PolicyKind::EgressApproval).unwrap().digest,
        denied_digest
    );
    assert_eq!(std::fs::metadata(&file).unwrap().modified().unwrap(), mtime);
    assert!(
        blocklist
            .check_egress_in_scope("https://approved.example", "sandbox", &scope)
            .await
            .is_err()
    );
}
