// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::kars_approval::request_snapshot;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

fn task() -> KarsTask {
    let mut task: KarsTask = serde_json::from_value(json!({
        "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTask",
        "metadata":{"name":"mission","namespace":"workspace","uid":"task-uid","resourceVersion":"10","generation":2,
            "annotations":{"kars.azure.com/owner-sub":"customer"}},
        "spec":{"objective":"Prepare a report","requestedTier":3,"envelope":{"tier":1,"authorityCeiling":1,"delegationDepth":0}},
        "status":{"phase":"Ready","observedGeneration":2,"executionPhase":"Idle","conditions":[{
            "type":"Ready","status":"True","reason":"Valid","message":"validated","lastTransitionTime":"2026-10-22T00:00:00Z"}]}
    })).unwrap();
    task.status.as_mut().unwrap().envelope_digest = Some(task.envelope_digest());
    task
}

fn approved(task: &KarsTask) -> KarsApproval {
    let mut approval = document(task, 3);
    approval.metadata.uid = Some("approval-uid".into());
    approval.metadata.resource_version = Some("20".into());
    approval.metadata.generation = Some(2);
    approval.spec.decision = Some(
        serde_json::from_value(json!({"verdict":"approve","decider":"Operator",
        "deciderSubject":"operator-sub","deciderRoles":["operator"]}))
        .unwrap(),
    );
    let now = Utc::now();
    approval.status = Some(serde_json::from_value(json!({"phase":"Approved","observedGeneration":2,
        "boundTaskUid":task.uid(),"boundEnvelopeDigest":task.envelope_digest(),"boundRequest":request_snapshot(&approval.spec),
        "requestedAt":(now-chrono::Duration::minutes(5)).to_rfc3339(),
        "decidedAt":(now-chrono::Duration::minutes(1)).to_rfc3339(),
        "expiresAt":(now+chrono::Duration::minutes(5)).to_rfc3339(),"decider":"Operator"})).unwrap());
    approval
}

fn client(server: &MockServer) -> Client {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap()
}
fn task_path() -> &'static str {
    "/apis/kars.azure.com/v1alpha1/namespaces/workspace/karstasks/mission"
}
fn approval_path(approval: &KarsApproval) -> String {
    format!(
        "/apis/kars.azure.com/v1alpha1/namespaces/workspace/karsapprovals/{}",
        approval.name_any()
    )
}

#[test]
fn promotion_ticket_binds_exact_authority_owner_target_and_timely_decision() {
    let task = task();
    let approval = approved(&task);
    assert!(authorized(&task, &approval, 3));
    assert!(!authorized(&task, &approval, 4));
    let before = task.envelope_digest();
    let mut request_only = task.clone();
    request_only.spec.requested_tier = Some(5);
    assert_eq!(before, request_only.envelope_digest());
    assert!(!authorized(&request_only, &approval, 3));
    for field in [
        "uid",
        "readiness",
        "generation",
        "digest",
        "consumed",
        "owner",
    ] {
        let mut changed = task.clone();
        match field {
            "uid" => changed.metadata.uid = Some("replacement".into()),
            "readiness" => changed.status.as_mut().unwrap().phase = Some("Pending".into()),
            "generation" => changed.metadata.generation = Some(3),
            "digest" => changed.spec.envelope.authority_ceiling = 0,
            "consumed" => {
                changed
                    .annotations_mut()
                    .insert(CONSUMED.into(), "approval-uid".into());
            }
            "owner" => {
                changed
                    .annotations_mut()
                    .insert("kars.azure.com/owner-sub".into(), "other".into());
            }
            _ => unreachable!(),
        }
        assert!(!authorized(&changed, &approval, 3), "{field}");
    }
    for field in [
        "owner", "request", "late", "future", "denied", "stale", "observed", "decider",
    ] {
        let mut changed = approval.clone();
        match field {
            "owner" => {
                changed.metadata.owner_references.as_mut().unwrap()[0].uid = "replacement".into()
            }
            "request" => changed.spec.action.requested_tier = Some(4),
            "late" => {
                changed.status.as_mut().unwrap().decided_at =
                    changed.status.as_ref().unwrap().expires_at.clone()
            }
            "future" => {
                changed.status.as_mut().unwrap().decided_at =
                    Some((Utc::now() + chrono::Duration::minutes(1)).to_rfc3339())
            }
            "denied" => changed.spec.decision.as_mut().unwrap().verdict = "deny".into(),
            "stale" => changed.status.as_mut().unwrap().phase = Some("Stale".into()),
            "observed" => changed.status.as_mut().unwrap().observed_generation = Some(1),
            "decider" => changed.status.as_mut().unwrap().decider = Some("another".into()),
            _ => unreachable!(),
        }
        assert!(!authorized(&task, &changed, 3), "{field}");
    }
    let mut expired_after_decision = approval.clone();
    expired_after_decision.status.as_mut().unwrap().expires_at =
        Some((Utc::now() - chrono::Duration::seconds(30)).to_rfc3339());
    assert!(authorized(&task, &expired_after_decision, 3));
}

#[test]
fn promotion_never_rewrites_consumed_or_controller_owned_task_authority() {
    assert!(unavailable(&task()).is_none());
    for key in [
        "kars.azure.com/run-requested",
        "kars.azure.com/reviewed-state",
        "kars.azure.com/reviewed-root",
        "kars.azure.com/qualification-run",
        "kars.azure.com/halted",
    ] {
        let mut task = task();
        task.annotations_mut().insert(key.into(), "bound".into());
        assert!(unavailable(&task).is_some(), "{key}");
    }
    let mut task = task();
    task.spec.execution = Some(crate::kars_task::TaskExecution {
        launch: true,
        runtime: None,
    });
    assert!(unavailable(&task).is_some());
    task.spec.execution = None;
    task.metadata.owner_references = Some(vec![
        serde_json::from_value(json!({"apiVersion":"kars.azure.com/v1alpha1",
        "kind":"KarsTeam","name":"team","uid":"team-uid","controller":true}))
        .unwrap(),
    ]);
    assert!(unavailable(&task).unwrap().contains("source Team/package"));
}

#[tokio::test]
async fn approved_task_promotion_is_one_cas_preserving_all_other_authority() {
    let server = MockServer::start().await;
    let task = task();
    let approval = approved(&task);
    Mock::given(method("GET"))
        .and(path(task_path()))
        .respond_with(ResponseTemplate::new(200).set_body_json(&task))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path(task_path()))
        .respond_with(ResponseTemplate::new(200).set_body_json(&task))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path(approval_path(&approval)))
        .respond_with(ResponseTemplate::new(200).set_body_json(&approval))
        .expect(1)
        .mount(&server)
        .await;
    assert!(
        apply_approved(&client(&server), &task, &approval, 3)
            .await
            .unwrap()
    );
    let requests = server.received_requests().await.unwrap();
    let write = requests
        .iter()
        .find(|r| r.method == "PATCH" && r.url.path() == task_path())
        .unwrap();
    let patch: serde_json::Value = serde_json::from_slice(&write.body).unwrap();
    assert_eq!(patch["metadata"]["uid"], "task-uid");
    assert_eq!(patch["metadata"]["resourceVersion"], "10");
    assert_eq!(patch["metadata"]["annotations"][CONSUMED], "approval-uid");
    assert_eq!(
        patch["spec"],
        json!({"envelope":{"tier":3},"requestedTier":null})
    );
    assert!(patch.get("status").is_none());
}

#[tokio::test]
async fn interrupted_promotion_outcome_recovers_without_reapplying_authority() {
    let server = MockServer::start().await;
    let mut task = task();
    let approval = approved(&task);
    task.spec.envelope.tier = 3;
    task.spec.requested_tier = None;
    task.metadata.generation = Some(3);
    task.metadata.resource_version = Some("11".into());
    task.annotations_mut()
        .insert(CONSUMED.into(), "approval-uid".into());
    task.annotations_mut()
        .insert(PENDING_RESULT.into(), approval.name_any());
    Mock::given(method("GET"))
        .and(path(approval_path(&approval)))
        .respond_with(ResponseTemplate::new(200).set_body_json(&approval))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path(approval_path(&approval)))
        .respond_with(ResponseTemplate::new(200).set_body_json(&approval))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path(task_path()))
        .respond_with(ResponseTemplate::new(200).set_body_json(&task))
        .expect(1)
        .mount(&server)
        .await;
    assert!(process(&client(&server), &task).await.unwrap());
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 3);
    for request in requests.iter().filter(|request| request.method == "PATCH") {
        let patch: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        assert!(patch.get("spec").is_none());
        assert!(patch.get("status").is_none());
    }
    let outcome: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
    assert_eq!(outcome["metadata"]["annotations"][RESULT], "applied");
    let cleanup: serde_json::Value = serde_json::from_slice(&requests[2].body).unwrap();
    assert_eq!(cleanup["metadata"]["resourceVersion"], "11");
    assert_eq!(
        cleanup["metadata"]["annotations"][PENDING_RESULT],
        serde_json::Value::Null
    );
}

#[tokio::test]
async fn consumed_task_records_fresh_run_outcome_without_mutating_task_or_opening_second_review() {
    let server = MockServer::start().await;
    let mut task = task();
    task.annotations_mut()
        .insert("kars.azure.com/run-requested".into(), "run-one".into());
    let approval = approved(&task);
    Mock::given(method("PATCH"))
        .and(path(approval_path(&approval)))
        .respond_with(ResponseTemplate::new(200).set_body_json(&approval))
        .expect(1)
        .mount(&server)
        .await;
    assert!(
        !apply_approved(&client(&server), &task, &approval, 3)
            .await
            .unwrap()
    );
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let patch: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(
        patch["metadata"]["annotations"][RESULT],
        "requires-fresh-run"
    );
    assert!(patch.get("spec").is_none());
}

#[tokio::test]
async fn concurrent_task_replacement_never_consumes_ticket() {
    let server = MockServer::start().await;
    let task = task();
    let approval = approved(&task);
    let mut replacement = task.clone();
    replacement.metadata.uid = Some("replacement".into());
    Mock::given(method("GET"))
        .and(path(task_path()))
        .respond_with(ResponseTemplate::new(200).set_body_json(&replacement))
        .expect(1)
        .mount(&server)
        .await;
    assert!(
        !apply_approved(&client(&server), &task, &approval, 3)
            .await
            .unwrap()
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn parent_ceiling_blocks_even_a_human_approved_child_promotion() {
    let server = MockServer::start().await;
    let mut child = task();
    child.spec.parent_ref = Some(crate::mcp_server::LocalObjectRef {
        name: "parent".into(),
    });
    child.status.as_mut().unwrap().envelope_digest = Some(child.envelope_digest());
    let approval = approved(&child);
    let mut parent = task();
    parent.metadata.name = Some("parent".into());
    parent.metadata.uid = Some("parent-uid".into());
    parent.spec.envelope.delegation_depth = 1;
    parent.status.as_mut().unwrap().envelope_digest = Some(parent.envelope_digest());
    Mock::given(method("GET"))
        .and(path(
            "/apis/kars.azure.com/v1alpha1/namespaces/workspace/karstasks/parent",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(&parent))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path(approval_path(&approval)))
        .respond_with(ResponseTemplate::new(200).set_body_json(&approval))
        .expect(1)
        .mount(&server)
        .await;
    assert!(
        !apply_approved(&client(&server), &child, &approval, 3)
            .await
            .unwrap()
    );
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    let patch: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
    assert_eq!(patch["metadata"]["annotations"][RESULT], "parent-ceiling");
}

#[tokio::test]
async fn promotion_request_opens_one_owned_exact_tier_ticket_without_granting_authority() {
    let server = MockServer::start().await;
    let task = task();
    let expected = document(&task, 3);
    Mock::given(method("GET"))
        .and(path(approval_path(&expected)))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
        "apiVersion":"v1","kind":"Status","status":"Failure","reason":"NotFound","code":404})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(
            "/apis/kars.azure.com/v1alpha1/namespaces/workspace/karsapprovals",
        ))
        .respond_with(ResponseTemplate::new(201).set_body_json(&expected))
        .expect(1)
        .mount(&server)
        .await;
    assert!(!process(&client(&server), &task).await.unwrap());
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    let created: KarsApproval = serde_json::from_slice(&requests[1].body).unwrap();
    assert_eq!(
        created.annotations()["kars.azure.com/owner-sub"],
        "customer"
    );
    assert_eq!(created.spec.action.requested_tier, Some(3));
    assert!(created.spec.decision.is_none());
    assert!(created.status.is_none());
}
