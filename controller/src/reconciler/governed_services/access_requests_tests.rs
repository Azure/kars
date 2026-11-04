// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

fn fixture(kind: &str) -> (KarsTask, RequestScope, RequestEntry) {
    let mut task: KarsTask = serde_json::from_value(json!({
        "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTask",
        "metadata":{"name":"task","namespace":"workspace","uid":"task-uid","generation":1,
            "annotations":{"kars.azure.com/owner-sub":"customer-subject"},"labels":{"kars.azure.com/team":"customer-team"}},
        "spec":{"objective":"inspect","envelope":{"tier":1,"authorityCeiling":1,"delegationDepth":0}},
        "status":{"phase":"Ready","observedGeneration":1,"conditions":[{"type":"Ready","status":"True",
            "reason":"Valid","message":"validated","lastTransitionTime":"2026-10-22T00:00:00Z"}]}
    })).unwrap();
    task.status.as_mut().unwrap().envelope_digest = Some(task.envelope_digest());
    let id = format!("{}:0", "a".repeat(32));
    let scope = RequestScope {
        id: id.clone(),
        assignment_id: Some("assignment-one".into()),
        identity: json!({"sandbox":{"namespace":"workspace","name":"sandbox","uid":"sandbox-uid"},
            "namespace_uid":"namespace-uid","task":{"namespace":"workspace","name":"task","uid":"task-uid"},
            "task_generation":1,"task_authorization":task.envelope_digest(),"managed":true}),
    };
    let entry = RequestEntry {
        request_id: format!("{id}:1"),
        scope_id: id,
        kind: kind.into(),
        target: "approved.example".into(),
        port: if kind == "egress" { Some(443) } else { None },
        reason: "Which report should I prepare?".into(),
        tier: None,
        status: "pending".into(),
        expires_at_unix: (chrono::Utc::now().timestamp() + 300) as u64,
    };
    (task, scope, entry)
}

fn reviewed(
    expected: &KarsApproval,
    task: &KarsTask,
    approve: bool,
    answer: Option<String>,
) -> KarsApproval {
    let mut approval = expected.clone();
    approval.metadata.generation = Some(2);
    approval.spec.decision = Some(crate::kars_approval::ApprovalDecision {
        verdict: if approve { "approve" } else { "deny" }.into(),
        decider: "Human reviewer".into(),
        decider_subject: Some("reviewer-subject".into()),
        decider_roles: vec!["operator".into()],
        reason: answer,
    });
    approval.status = Some(serde_json::from_value(json!({"phase":if approve {"Approved"} else {"Denied"},
        "observedGeneration":2,"decider":"Human reviewer","decidedAt":"2026-10-22T12:00:00Z",
        "boundTaskUid":task.uid(),"boundEnvelopeDigest":task.envelope_digest(),"boundRequest":request_snapshot(&approval.spec)
    })).unwrap());
    approval
}

#[test]
fn native_tier_request_preserves_exact_target_and_explains_consumed_authority() {
    let (task, scope, mut entry) = fixture("tier");
    for tier in [None, Some(0), Some(1), Some(6)] {
        entry.tier = tier;
        assert!(approval_document(&task, &scope, &entry).is_err());
    }
    entry.tier = Some(3);
    let approval = approval_document(&task, &scope, &entry).unwrap();
    assert_eq!(approval.spec.action.kind, "tierRaise");
    assert_eq!(approval.spec.action.requested_tier, Some(3));
    assert!(
        approval
            .spec
            .action
            .detail
            .unwrap()
            .contains("fresh authorized run")
    );
}

#[tokio::test]
async fn native_tier_review_reuses_ticket_but_does_not_grant_consumed_assignment_authority() {
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };
    let (mut task, scope, mut entry) = fixture("tier");
    task.metadata.resource_version = Some("10".into());
    task.annotations_mut()
        .insert("kars.azure.com/run-requested".into(), "run-one".into());
    entry.tier = Some(3);
    let expected = approval_document(&task, &scope, &entry).unwrap();
    let mut approval = reviewed(&expected, &task, true, None);
    approval.metadata.uid = Some("approval-uid".into());
    approval.metadata.resource_version = Some("20".into());
    let now = chrono::Utc::now();
    let status = approval.status.as_mut().unwrap();
    status.requested_at = Some((now - chrono::Duration::minutes(2)).to_rfc3339());
    status.decided_at = Some((now - chrono::Duration::minutes(1)).to_rfc3339());
    status.expires_at = Some((now + chrono::Duration::minutes(2)).to_rfc3339());
    assert_eq!(
        decision(&approval, &expected, &task, &entry).unwrap(),
        Some((true, None))
    );
    let server = MockServer::start().await;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
    Mock::given(method("PATCH"))
        .and(path(format!(
            "/apis/kars.azure.com/v1alpha1/namespaces/workspace/karsapprovals/{}",
            approval.name_any()
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(&approval))
        .expect(1)
        .mount(&server)
        .await;
    assert!(
        !crate::kars_task_reconciler::promotion::apply_approved(&client, &task, &approval, 3)
            .await
            .unwrap()
    );
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let patch: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(
        patch["metadata"]["annotations"]["kars.azure.com/promotion-result"],
        "requires-fresh-run"
    );
    assert!(patch.get("spec").is_none());
}

fn reviewed_tier_fixture(
    controller_owned: bool,
) -> (KarsTask, RequestScope, RequestEntry, KarsApproval) {
    let (mut task, scope, mut entry) = fixture("tier");
    task.metadata.resource_version = Some("10".into());
    if controller_owned {
        task.metadata.owner_references = Some(vec![
            serde_json::from_value(json!({
                "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTeam","name":"team",
                "uid":"team-uid","controller":true
            }))
            .unwrap(),
        ]);
    } else {
        task.annotations_mut()
            .insert("kars.azure.com/run-requested".into(), "run-one".into());
    }
    entry.tier = Some(3);
    entry.target.clear();
    let expected = approval_document(&task, &scope, &entry).unwrap();
    let mut approval = reviewed(&expected, &task, true, None);
    approval.metadata.uid = Some("approval-uid".into());
    approval.metadata.resource_version = Some("20".into());
    let now = chrono::Utc::now();
    let status = approval.status.as_mut().unwrap();
    status.requested_at = Some((now - chrono::Duration::minutes(2)).to_rfc3339());
    status.decided_at = Some((now - chrono::Duration::minutes(1)).to_rfc3339());
    status.expires_at = Some((now + chrono::Duration::minutes(2)).to_rfc3339());
    (task, scope, entry, approval)
}

fn queue_snapshot(scope: &RequestScope, entry: &RequestEntry) -> serde_json::Value {
    json!({"schema_version":1,"sandbox":"sandbox",
        "scope":{"id":scope.id,"identity":scope.identity,"assignment_id":scope.assignment_id},
        "entries":[{"request_id":entry.request_id,"scope_id":entry.scope_id,"kind":entry.kind,
            "target":entry.target,"reason":entry.reason,"tier":entry.tier,"port":entry.port,
            "status":entry.status,"expires_at_unix":entry.expires_at_unix}]})
}

#[tokio::test]
async fn reviewed_consumed_and_controller_owned_tiers_cancel_queue_and_retry_without_reapproval() {
    use crate::kars_task_reconciler::promotion::{DETAIL, RESULT};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path},
    };
    for controller_owned in [false, true] {
        let (task, scope, entry, mut approval) = reviewed_tier_fixture(controller_owned);
        let expected = approval_document(&task, &scope, &entry).unwrap();
        assert_eq!(
            decision(&approval, &expected, &task, &entry).unwrap(),
            Some((true, None))
        );
        let server = MockServer::start().await;
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
        let control = ControlClient::for_test(&server.uri(), scope.identity.clone());
        Mock::given(method("PATCH"))
            .and(path(format!(
                "/apis/kars.azure.com/v1alpha1/namespaces/workspace/karsapprovals/{}",
                approval.name_any()
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(&approval))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/internal/access-requests"))
            .and(header("authorization", "Bearer test-control-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(queue_snapshot(&scope, &entry)))
            .expect(2)
            .mount(&server)
            .await;
        let posts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        Mock::given(method("POST"))
            .and(path("/internal/access-requests/decision"))
            .and(header("authorization", "Bearer test-control-token"))
            .respond_with(move |_: &wiremock::Request| {
                ResponseTemplate::new(
                    if posts.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                        503
                    } else {
                        200
                    },
                )
            })
            .expect(2)
            .mount(&server)
            .await;
        let before = serde_json::to_value(&task).unwrap();
        assert!(
            finish_reviewed_tier(&client, &control, &task, &approval, &scope, &entry)
                .await
                .is_err()
        );
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 3);
        let patch: serde_json::Value = requests[0].body_json().unwrap();
        assert_eq!(patch["metadata"]["uid"], "approval-uid");
        assert_eq!(patch["metadata"]["resourceVersion"], "20");
        assert_eq!(
            patch["metadata"]["annotations"][RESULT],
            "requires-fresh-run"
        );
        assert!(patch.get("spec").is_none());
        assert!(patch.get("status").is_none());
        let detail = patch["metadata"]["annotations"][DETAIL].as_str().unwrap();
        approval
            .annotations_mut()
            .insert(RESULT.into(), "requires-fresh-run".into());
        approval
            .annotations_mut()
            .insert(DETAIL.into(), detail.into());
        approval.metadata.resource_version = Some("21".into());
        finish_reviewed_tier(&client, &control, &task, &approval, &scope, &entry)
            .await
            .unwrap();
        assert_eq!(serde_json::to_value(&task).unwrap(), before);
        assert_eq!(approval.spec.decision.as_ref().unwrap().verdict, "approve");
        assert_eq!(
            approval.status.as_ref().unwrap().phase.as_deref(),
            Some("Approved")
        );
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 5);
        for post in requests.iter().filter(|request| request.method == "POST") {
            let body: serde_json::Value = post.body_json().unwrap();
            assert_eq!(body["scope_id"], scope.id);
            assert_eq!(body["request_id"], entry.request_id);
            assert_eq!(body["verdict"], "cancelled");
            assert!(body["answer"].is_null());
            let explanation = body["explanation"].as_str().unwrap();
            assert!(explanation.starts_with("requires-fresh-run:"));
            assert!(explanation.contains("Human review approved Tier 3"));
            assert!(explanation.contains("no authority was granted"));
            assert!(explanation.contains("fresh authorized run"));
            assert!(explanation.contains(detail));
        }
    }
}

#[tokio::test]
async fn invalid_tier_reviews_never_cancel_or_change_authority() {
    let server = wiremock::MockServer::start().await;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
    for change in [
        "uid",
        "digest",
        "tier",
        "consumed",
        "expired",
        "generation",
        "owner",
    ] {
        let (mut task, scope, entry, mut approval) = reviewed_tier_fixture(false);
        match change {
            "uid" => task.metadata.uid = Some("replacement".into()),
            "digest" => {
                approval.status.as_mut().unwrap().bound_envelope_digest = Some("old".into())
            }
            "tier" => approval.spec.action.requested_tier = Some(4),
            "consumed" => {
                task.annotations_mut().insert(
                    crate::kars_task_reconciler::promotion::CONSUMED.into(),
                    "approval-uid".into(),
                );
            }
            "expired" => {
                approval.status.as_mut().unwrap().expires_at =
                    approval.status.as_ref().unwrap().decided_at.clone()
            }
            "generation" => approval.status.as_mut().unwrap().observed_generation = Some(1),
            "owner" => approval.metadata.owner_references = None,
            _ => unreachable!(),
        }
        let control = ControlClient::for_test(&server.uri(), scope.identity.clone());
        finish_reviewed_tier(&client, &control, &task, &approval, &scope, &entry)
            .await
            .unwrap();
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn reviewed_tier_cancellation_waits_for_durable_authority_outcome() {
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };
    let (task, scope, entry, approval) = reviewed_tier_fixture(false);
    let server = MockServer::start().await;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
    let control = ControlClient::for_test(&server.uri(), scope.identity.clone());
    Mock::given(method("PATCH"))
        .and(path(format!("/apis/kars.azure.com/v1alpha1/namespaces/workspace/karsapprovals/{}", approval.name_any())))
        .respond_with(ResponseTemplate::new(409).set_body_json(json!({
            "apiVersion":"v1","kind":"Status","code":409,"reason":"Conflict","message":"review changed"
        }))).expect(1).mount(&server).await;
    assert!(
        finish_reviewed_tier(&client, &control, &task, &approval, &scope, &entry)
            .await
            .is_err()
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn reviewed_tier_control_refuses_replaced_scope_identity_expiry_and_non_tier() {
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };
    for change in ["scope", "identity", "expiry", "kind", "terminal"] {
        let (_, scope, entry, _) = reviewed_tier_fixture(false);
        let mut snapshot = queue_snapshot(&scope, &entry);
        match change {
            "scope" => snapshot["scope"]["id"] = json!(format!("{}:1", "b".repeat(32))),
            "identity" => snapshot["scope"]["identity"]["task"]["uid"] = json!("replacement"),
            "expiry" => snapshot["entries"][0]["expires_at_unix"] = json!(1),
            "kind" => snapshot["entries"][0]["kind"] = json!("egress"),
            "terminal" => snapshot["entries"][0]["status"] = json!("cancelled"),
            _ => unreachable!(),
        }
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/internal/access-requests"))
            .respond_with(ResponseTemplate::new(200).set_body_json(snapshot))
            .expect(1)
            .mount(&server)
            .await;
        let control = ControlClient::for_test(&server.uri(), scope.identity.clone());
        let result = control
            .cancel_reviewed_tier(&scope.id, &entry.request_id, "requires-fresh-run")
            .await;
        assert_eq!(result.is_ok(), change == "terminal", "{change}");
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }
}

#[test]
fn durable_request_is_bound_to_assignment_identity_and_exact_question() {
    let (task, mut scope, entry) = fixture("clarification");
    let expected = approval_document(&task, &scope, &entry).unwrap();
    assert_eq!(
        expected.annotations()["kars.azure.com/owner-sub"],
        "customer-subject"
    );
    assert_eq!(
        expected.annotations()["kars.azure.com/team"],
        "customer-team"
    );
    assert_eq!(expected.spec.action.kind, "clarification");
    assert_eq!(expected.spec.action.summary, entry.reason);
    assert_eq!(
        expected.spec.requested_by.as_ref().unwrap().subject,
        "sandbox:sandbox-uid"
    );
    assert_eq!(
        approval_document(&task, &scope, &entry).unwrap().name_any(),
        expected.name_any()
    );
    scope.assignment_id = Some("assignment-two".into());
    assert_ne!(
        approval_document(&task, &scope, &entry).unwrap().name_any(),
        expected.name_any()
    );
    let mut stale = entry.clone();
    stale.scope_id = "stale".into();
    assert!(approval_document(&task, &scope, &stale).is_err());
}

#[test]
fn only_current_reviewed_decisions_deliver_answers_with_utf8_byte_bounds() {
    let (task, scope, entry) = fixture("clarification");
    let expected = approval_document(&task, &scope, &entry).unwrap();
    assert!(
        decision(&expected, &expected, &task, &entry)
            .unwrap()
            .is_none()
    );
    let approval = reviewed(
        &expected,
        &task,
        true,
        Some("Prepare the quarterly report".into()),
    );
    assert_eq!(
        decision(&approval, &expected, &task, &entry).unwrap(),
        Some((true, Some("Prepare the quarterly report".into())))
    );
    let denied = reviewed(&expected, &task, false, Some("Do not continue".into()));
    assert_eq!(
        decision(&denied, &expected, &task, &entry).unwrap(),
        Some((false, None))
    );
    for answer in [None, Some("  \n".into()), Some("é".repeat(4097))] {
        assert!(
            decision(
                &reviewed(&expected, &task, true, answer),
                &expected,
                &task,
                &entry
            )
            .is_err()
        );
    }
    let mut foreign_owner = approval.clone();
    foreign_owner
        .annotations_mut()
        .insert("kars.azure.com/owner-sub".into(), "another-customer".into());
    assert!(decision(&foreign_owner, &expected, &task, &entry).is_err());
    let mut changed = approval.clone();
    changed.spec.action.detail = Some("different question".into());
    assert!(decision(&changed, &expected, &task, &entry).is_err());
    let mut replacement = task.clone();
    replacement.metadata.uid = Some("replacement-task".into());
    assert!(
        decision(&approval, &expected, &replacement, &entry)
            .unwrap()
            .is_none()
    );
    let mut stale = approval;
    stale.status.as_mut().unwrap().observed_generation = Some(1);
    assert!(
        decision(&stale, &expected, &task, &entry)
            .unwrap()
            .is_none()
    );
}

#[test]
fn only_current_approved_endpoint_contributes_to_scoped_digest() {
    let (_task, scope, entry) = fixture("egress");
    let mut snapshot: Snapshot = serde_json::from_value(json!({"schema_version":1,"sandbox":"sandbox",
        "scope":{"id":scope.id,"identity":scope.identity,"assignment_id":scope.assignment_id},
        "entries":[{"request_id":entry.request_id,"scope_id":entry.scope_id,"kind":"egress","target":entry.target,
            "reason":entry.reason,"port":443,"status":"pending","expires_at_unix":entry.expires_at_unix}]})).unwrap();
    let mut grant: EgressApproval = serde_json::from_value(json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"EgressApproval",
        "metadata":{"name":"grant","annotations":{REQUEST_SCOPE:scope.id,REQUEST_ID:entry.request_id}},
        "spec":{"sandbox":"sandbox","hosts":[{"host":"approved.example","port":443}],"reason":"reviewed","ttl":"PT15M"}})).unwrap();
    assert!(!authorizes_grant(&snapshot, &grant));
    snapshot.entries[0].status = "approved".into();
    assert!(authorizes_grant(&snapshot, &grant));
    snapshot.entries[0].status = "dispatch_claimed".into();
    assert!(authorizes_grant(&snapshot, &grant));
    grant.spec.hosts[0].port = Some(8443);
    assert!(!authorizes_grant(&snapshot, &grant));
    grant.spec.hosts[0].port = Some(443);
    snapshot.scope.id = "next-scope".into();
    assert!(!authorizes_grant(&snapshot, &grant));
    snapshot.scope.id = scope.id;
    snapshot.entries[0].expires_at_unix = 1;
    assert!(!authorizes_grant(&snapshot, &grant));
}
