// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::access_request::{Identity, Request, Status};
use serde_json::{Value, json};

async fn setup() -> (Blocklist, Arc<GovernedServices>, ScopedEgressGrant) {
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
            reason: "Needed for the task".into(),
            tier: None,
            port: Some(443),
        })
        .unwrap();
    let grant = ScopedEgressGrant {
        scope_id: scope,
        request_id: entry.request_id,
        endpoints: vec![("approved.example".into(), 443)],
        expires_at: chrono::Utc::now().timestamp() + 300,
    };
    (blocklist, services, grant)
}

#[tokio::test]
async fn requires_both_current_decision_and_installed_exact_endpoint() {
    let (blocklist, services, grant) = setup().await;
    let scope = &grant.scope_id;
    let allowed = "https://approved.example/document";
    blocklist
        .replace_allowlist_with_scoped_grants(vec![], vec![grant.clone()])
        .await;
    assert!(
        blocklist
            .check_egress_in_scope(allowed, "sandbox", scope)
            .await
            .is_err()
    );
    services
        .requests
        .transition(scope, &grant.request_id, Status::Approved)
        .unwrap();
    assert_eq!(
        blocklist
            .check_egress_in_scope(allowed, "sandbox", scope)
            .await
            .unwrap(),
        Some(grant.request_id.clone())
    );
    assert!(blocklist.check_egress(allowed, "sandbox").await.is_err());
    assert!(blocklist.get_allowlist().await.is_empty());
    for denied in [
        "https://child.approved.example",
        "http://approved.example",
        "https://approved.example:8443",
    ] {
        assert!(
            blocklist
                .check_egress_in_scope(denied, "sandbox", scope)
                .await
                .is_err(),
            "{denied}"
        );
    }
    assert!(
        blocklist
            .check_egress_in_scope(allowed, "sandbox", "another-scope")
            .await
            .is_err()
    );
    blocklist.replace_allowlist(vec![]).await;
    assert!(
        blocklist
            .check_egress_in_scope(allowed, "sandbox", scope)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn cancel_reset_expiry_and_shutdown_revoke_without_mount_changes() {
    for action in ["cancel", "reset", "expire", "shutdown"] {
        let (blocklist, services, mut grant) = setup().await;
        let scope = grant.scope_id.clone();
        services
            .requests
            .transition(&scope, &grant.request_id, Status::Approved)
            .unwrap();
        if action == "expire" {
            grant.expires_at = chrono::Utc::now().timestamp() - 1;
        }
        blocklist
            .replace_allowlist_with_scoped_grants(vec![], vec![grant.clone()])
            .await;
        match action {
            "cancel" => {
                services
                    .requests
                    .transition(&scope, &grant.request_id, Status::Cancelled)
                    .unwrap();
            }
            "reset" => {
                services
                    .requests
                    .reset(&scope, Some("next-assignment".into()))
                    .unwrap();
            }
            "shutdown" => services.shutdown.cancel(),
            _ => {}
        }
        assert!(
            blocklist
                .check_egress_in_scope("https://approved.example", "sandbox", &scope)
                .await
                .is_err(),
            "{action}"
        );
        assert!(blocklist.applicable_grant_endpoints(&[grant]).is_empty());
    }
}

#[tokio::test]
async fn repeated_grant_dispatches_keep_reset_fenced_until_every_send_finishes() {
    let (blocklist, services, grant) = setup().await;
    let scope = &grant.scope_id;
    services
        .requests
        .transition(scope, &grant.request_id, Status::Approved)
        .unwrap();
    blocklist
        .replace_allowlist_with_scoped_grants(vec![], vec![grant.clone()])
        .await;
    let first = services
        .requests
        .claim_dispatch(scope, Some(&grant.request_id), &services.shutdown)
        .unwrap();
    let second = services
        .requests
        .claim_grant_dispatch(scope, &grant.request_id, &services.shutdown)
        .unwrap();
    assert!(
        services
            .requests
            .claim_dispatch(scope, Some(&grant.request_id), &services.shutdown)
            .is_err()
    );
    drop(first);
    assert!(
        services
            .requests
            .entry(scope, &grant.request_id)
            .unwrap()
            .dispatch_active
    );
    assert!(services.requests.reset(scope, None).is_err());
    assert!(
        blocklist
            .check_egress_in_scope("https://approved.example", "sandbox", scope)
            .await
            .is_ok()
    );
    drop(second);
    assert!(
        !services
            .requests
            .entry(scope, &grant.request_id)
            .unwrap()
            .dispatch_active
    );
    services.requests.reset(scope, None).unwrap();
    assert!(
        blocklist
            .check_egress_in_scope("https://approved.example", "sandbox", scope)
            .await
            .is_err()
    );
}

fn document() -> Value {
    let scope = format!("{}:0", "a".repeat(32));
    json!({"schemaVersion":2,"requestScope":scope,"requestId":format!("{scope}:1"),
        "scopedHosts":[{"host":"approved.example","port":443}],
        "effectiveAt":(chrono::Utc::now()-chrono::Duration::seconds(10)).to_rfc3339(),
        "expiresAt":(chrono::Utc::now()+chrono::Duration::seconds(300)).to_rfc3339()})
}

#[test]
fn scoped_mount_parser_rejects_legacy_hosts_missing_binding_and_invalid_endpoints() {
    assert!(ScopedEgressGrant::from_document(&document()).is_some());
    for (key, value) in [
        ("hosts", json!([{"host":"approved.example","port":443}])),
        ("schemaVersion", json!(1)),
        ("requestScope", json!("bad")),
        ("requestId", json!("other:1")),
        ("expiresAt", json!("2000-01-01T00:00:00Z")),
        ("effectiveAt", json!("2999-01-01T00:00:00Z")),
        ("scopedHosts", json!([{"host":"approved.example","port":0}])),
        ("scopedHosts", json!([{"host":"approved.example"}])),
        ("scopedHosts", json!([])),
    ] {
        let mut doc = document();
        doc[key] = value;
        assert!(ScopedEgressGrant::from_document(&doc).is_none(), "{key}");
    }
    for key in [
        "schemaVersion",
        "requestScope",
        "requestId",
        "effectiveAt",
        "expiresAt",
        "scopedHosts",
    ] {
        let mut doc = document();
        doc.as_object_mut().unwrap().remove(key);
        assert!(ScopedEgressGrant::from_document(&doc).is_none(), "{key}");
    }
}
