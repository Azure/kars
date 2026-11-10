// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! HTTP-level regression tests against Kubernetes-shaped responses. All mocks
//! are test-only, using the controller's existing wiremock dev dependency.

use super::tests::{approved, principal, team};
use super::*;
use crate::kars_task::{TaskBlueprint, TaskExecution, TaskModel};
use crate::kars_team::TeamRole;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

pub(super) async fn client(server: &MockServer) -> Client {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap()
}

fn task_path(name: &str) -> String {
    format!("/apis/kars.azure.com/v1alpha1/namespaces/tenant-a/karstasks/{name}")
}

fn task_list(tasks: &[KarsTask]) -> serde_json::Value {
    json!({ "apiVersion": "kars.azure.com/v1alpha1", "kind": "KarsTaskList", "metadata": {}, "items": tasks })
}

fn not_found() -> ResponseTemplate {
    ResponseTemplate::new(404).set_body_json(json!({
        "apiVersion": "v1", "kind": "Status", "status": "Failure", "reason": "NotFound", "code": 404,
    }))
}

pub(super) fn owned_task(team: &KarsTeam, role: &TeamRole) -> KarsTask {
    let mut task = KarsTask::new(
        &specs::member_name(team, role),
        specs::member_spec(team, role),
    );
    task.metadata.namespace = team.metadata.namespace.clone();
    task.metadata.uid = Some("member-uid".into());
    task.metadata.resource_version = Some("40".into());
    task.metadata.owner_references = Some(vec![tasks::owner_ref(team).unwrap()]);
    task.metadata.annotations = Some(
        [
            (ANNOT_TEAM_ROLE.into(), "member".into()),
            ("unrelated".into(), "keep".into()),
        ]
        .into(),
    );
    task.metadata.finalizers = Some(vec!["customer.example/finalizer".into()]);
    task
}

#[tokio::test]
async fn promotion_ticket_is_visible_to_team_owner_and_attributes_requester() {
    let server = MockServer::start().await;
    let mut team = team();
    team.spec.requested_tier = Some(5);
    for (key, value) in [
        ("owner-sub", "customer"),
        ("owner-name", "Customer"),
        ("promotion-requested-by-sub", "requester"),
        ("promotion-requested-by", "Requester"),
    ] {
        team.annotations_mut()
            .insert(format!("kars.azure.com/{key}"), value.into());
    }
    let principal = principal(&team);
    let approval = approved(&team, &principal);
    Mock::given(method("GET"))
        .and(path(task_path(&principal.name_any())))
        .respond_with(ResponseTemplate::new(200).set_body_json(&principal))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/apis/kars.azure.com/v1alpha1/namespaces/tenant-a/karsapprovals/{}",
            approval.name_any()
        )))
        .respond_with(not_found())
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(
            "/apis/kars.azure.com/v1alpha1/namespaces/tenant-a/karsapprovals",
        ))
        .respond_with(ResponseTemplate::new(201).set_body_json(&approval))
        .expect(1)
        .mount(&server)
        .await;
    assert!(
        !promotion::process_promotion(&client(&server).await, &team, &principal.name_any())
            .await
            .unwrap()
    );
    let requests = server.received_requests().await.unwrap();
    let created: crate::kars_approval::KarsApproval =
        serde_json::from_slice(&requests[2].body).unwrap();
    assert_eq!(
        created.annotations()["kars.azure.com/owner-sub"],
        "customer"
    );
    assert_eq!(
        created.annotations()["kars.azure.com/owner-name"],
        "Customer"
    );
    assert_eq!(created.annotations()["kars.azure.com/team"], "eng");
    assert_eq!(created.spec.requested_by.unwrap().subject, "requester");
}

#[tokio::test]
async fn legacy_team_promotion_visibility_repair_is_metadata_only_and_foreign_owner_rejected() {
    let server = MockServer::start().await;
    let mut team = team();
    team.spec.requested_tier = Some(5);
    team.annotations_mut()
        .insert("kars.azure.com/owner-sub".into(), "customer".into());
    let principal = principal(&team);
    let mut approval = approved(&team, &principal);
    approval.metadata.resource_version = Some("30".into());
    let approval_path = format!(
        "/apis/kars.azure.com/v1alpha1/namespaces/tenant-a/karsapprovals/{}",
        approval.name_any()
    );
    Mock::given(method("GET"))
        .and(path(task_path(&principal.name_any())))
        .respond_with(ResponseTemplate::new(200).set_body_json(&principal))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(&approval_path))
        .respond_with(ResponseTemplate::new(200).set_body_json(&approval))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path(&approval_path))
        .respond_with(ResponseTemplate::new(200).set_body_json(&approval))
        .expect(1)
        .mount(&server)
        .await;
    assert!(
        !promotion::process_promotion(&client(&server).await, &team, &principal.name_any())
            .await
            .unwrap()
    );
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 3);
    let patch: serde_json::Value = serde_json::from_slice(&requests[2].body).unwrap();
    assert_eq!(patch["metadata"]["uid"], approval.uid().unwrap());
    assert_eq!(patch["metadata"]["resourceVersion"], "30");
    assert_eq!(
        patch["metadata"]["annotations"]["kars.azure.com/owner-sub"],
        "customer"
    );
    assert!(patch.get("spec").is_none());
    assert!(patch.get("status").is_none());
    approval
        .annotations_mut()
        .insert("kars.azure.com/owner-sub".into(), "foreign".into());
    assert!(!promotion::authorized(&team, &principal, &approval, 5));
    approval
        .annotations_mut()
        .insert("kars.azure.com/owner-sub".into(), "customer".into());
    assert!(promotion::authorized(&team, &principal, &approval, 5));
}

#[test]
fn team_observation_status_coalesces_only_fresh_unchanged_facts() {
    let now = parse_rfc3339("2026-10-15T16:35:30Z").unwrap();
    let previous = json!({"phase":"Hibernating", "tokensSpentTotal":42,
        "tokensSpentScope":"lifetime", "tokensObservedAt":"2026-10-15T16:35:00Z"});
    let mut desired = previous.clone();
    desired["tokensObservedAt"] = json!(now.to_rfc3339());
    assert!(!status_needs_update(&previous, &desired, now));
    assert!(!status_needs_update(&previous, &previous, now));
    for (field, value) in [
        ("phase", json!("Active")),
        ("tokensSpentTotal", json!(43)),
        ("tokensSpentScope", json!("synthesis")),
        ("observedGeneration", json!(2)),
        ("runAdmission", json!({"state":"reserved"})),
        ("tokensObservedAt", serde_json::Value::Null),
    ] {
        let mut changed = desired.clone();
        changed[field] = value;
        assert!(status_needs_update(&previous, &changed, now), "{field}");
    }
}

#[test]
fn team_observation_status_refreshes_expired_missing_invalid_or_future_time() {
    let now = parse_rfc3339("2026-10-15T16:36:00Z").unwrap();
    let desired = json!({"tokensSpentTotal":42, "tokensObservedAt":now.to_rfc3339()});
    for timestamp in [
        json!("2026-10-15T16:35:00Z"),
        json!("2026-10-15T16:34:59Z"),
        json!("2026-10-15T16:36:01Z"),
        json!("invalid"),
        serde_json::Value::Null,
    ] {
        let mut previous = desired.clone();
        previous["tokensObservedAt"] = timestamp;
        assert!(status_needs_update(&previous, &desired, now));
    }
    let mut missing = desired.clone();
    missing.as_object_mut().unwrap().remove("tokensObservedAt");
    assert!(status_needs_update(&missing, &desired, now));
}

#[tokio::test]
async fn team_status_skips_hot_loop_but_refreshes_with_identity_fence() {
    let server = MockServer::start().await;
    let mut team = team();
    let now = Utc::now();
    let mut desired = KarsTeamStatus {
        phase: Some(PHASE_HIBERNATING.into()),
        tokens_spent_total: Some(42),
        tokens_observed_at: Some(now.to_rfc3339()),
        ..Default::default()
    };
    team.status = Some(desired.clone());
    team.status.as_mut().unwrap().tokens_observed_at =
        Some((now - chrono::Duration::seconds(1)).to_rfc3339());
    let api = Api::namespaced(client(&server).await, "tenant-a");
    let retained = write_status(&api, &team, desired.clone()).await.unwrap();
    assert_eq!(
        retained.metadata.resource_version,
        team.metadata.resource_version
    );
    assert_eq!(
        retained.status.unwrap().tokens_observed_at,
        team.status.as_ref().unwrap().tokens_observed_at
    );
    assert!(server.received_requests().await.unwrap().is_empty());
    let mut missing_uid = team.clone();
    missing_uid.metadata.uid = None;
    assert!(
        write_status(&api, &missing_uid, desired.clone())
            .await
            .is_err()
    );
    let mut missing_version = team.clone();
    missing_version.metadata.resource_version = None;
    assert!(
        write_status(&api, &missing_version, desired.clone())
            .await
            .is_err()
    );
    assert!(server.received_requests().await.unwrap().is_empty());

    team.status.as_mut().unwrap().tokens_observed_at =
        Some((now - chrono::Duration::seconds(60)).to_rfc3339());
    desired.tokens_observed_at = Some(Utc::now().to_rfc3339());
    let mut refreshed = team.clone();
    refreshed.status = Some(desired.clone());
    refreshed.metadata.resource_version = Some("11".into());
    Mock::given(method("PATCH"))
        .and(path(
            "/apis/kars.azure.com/v1alpha1/namespaces/tenant-a/karsteams/eng/status",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(&refreshed))
        .expect(1)
        .mount(&server)
        .await;
    let captured = write_status(&api, &team, desired.clone()).await.unwrap();
    assert_eq!(captured.metadata.resource_version.as_deref(), Some("11"));
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let patch: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(patch["metadata"]["uid"], "team-uid");
    assert_eq!(patch["metadata"]["resourceVersion"], "10");
    assert_eq!(patch["status"], serde_json::to_value(desired).unwrap());
}

#[tokio::test]
async fn same_name_customer_task_is_never_adopted() {
    let server = MockServer::start().await;
    let team = team();
    let mut existing = principal(&team);
    existing.metadata.owner_references = None;
    Mock::given(method("GET"))
        .and(path(task_path(&existing.name_any())))
        .respond_with(ResponseTemplate::new(200).set_body_json(&existing))
        .expect(1)
        .mount(&server)
        .await;
    let tasks_api = Api::namespaced(client(&server).await, "tenant-a");
    assert!(
        tasks::apply_task(
            &tasks_api,
            &team,
            &existing.name_any(),
            specs::principal_spec(&team),
            "principal"
        )
        .await
        .is_err()
    );
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|request| request.method == "GET")
    );
}

#[tokio::test]
async fn task_replace_preserves_execution_metadata_and_clears_removed_fields() {
    let server = MockServer::start().await;
    let mut team = team();
    team.spec.envelope.budget = None;
    let role = TeamRole {
        name: "reader".into(),
        ..Default::default()
    };
    let mut existing = owned_task(&team, &role);
    existing.spec.execution = Some(TaskExecution {
        launch: true,
        runtime: Some("Hermes".into()),
    });
    // Removing an optional non-authority blueprint is a whole-spec replace.
    existing.spec.blueprint = Some(TaskBlueprint {
        instructions: Some("old prompt".into()),
        ..Default::default()
    });
    Mock::given(method("GET"))
        .and(path(task_path(&existing.name_any())))
        .respond_with(ResponseTemplate::new(200).set_body_json(&existing))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path(task_path(&existing.name_any())))
        .respond_with(ResponseTemplate::new(200).set_body_json(&existing))
        .expect(1)
        .mount(&server)
        .await;
    let api = Api::namespaced(client(&server).await, "tenant-a");
    tasks::apply_task(
        &api,
        &team,
        &existing.name_any(),
        specs::member_spec(&team, &role),
        "member",
    )
    .await
    .unwrap();
    let requests = server.received_requests().await.unwrap();
    let replacement: serde_json::Value = serde_json::from_slice(
        &requests
            .iter()
            .find(|request| request.method == "PUT")
            .unwrap()
            .body,
    )
    .unwrap();
    assert_eq!(replacement["metadata"]["resourceVersion"], "40");
    assert_eq!(replacement["metadata"]["uid"], "member-uid");
    assert_eq!(replacement["metadata"]["annotations"]["unrelated"], "keep");
    assert_eq!(
        replacement["metadata"]["finalizers"][0],
        "customer.example/finalizer"
    );
    assert_eq!(replacement["spec"]["execution"]["launch"], true);
    assert_eq!(replacement["spec"]["execution"]["runtime"], "Hermes");
    assert!(replacement["spec"].get("blueprint").is_none());
}

#[tokio::test]
async fn paused_team_preserves_running_owned_tasks_and_spoofed_customer() {
    let server = MockServer::start().await;
    let mut team = team();
    team.spec.paused = true;
    team.spec.envelope.budget = None;
    let role = TeamRole {
        name: "reader".into(),
        ..Default::default()
    };
    team.spec.roster.push(role.clone());
    let mut member = owned_task(&team, &role);
    member.spec.execution = Some(TaskExecution {
        launch: true,
        runtime: None,
    });
    let mut foreign = member.clone();
    foreign.metadata.name = Some("customer".into());
    foreign.metadata.owner_references = None;
    foreign.metadata.labels = Some([(ANNOT_TEAM.into(), team.name_any())].into());
    Mock::given(method("GET"))
        .and(path(
            "/apis/kars.azure.com/v1alpha1/namespaces/tenant-a/karstasks",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(task_list(&[member.clone(), foreign])),
        )
        .mount(&server)
        .await;
    let api = Api::namespaced(client(&server).await, "tenant-a");
    for paused in [true, false] {
        team.spec.paused = paused;
        tasks::reconcile_revocations(&api, &team).await.unwrap();
    }
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|request| request.method == "GET"));
}

#[tokio::test]
async fn removed_role_retires_with_uid_and_resource_version_preconditions() {
    let server = MockServer::start().await;
    let team = team();
    let member = owned_task(
        &team,
        &TeamRole {
            name: "removed".into(),
            ..Default::default()
        },
    );
    Mock::given(method("GET"))
        .and(path(
            "/apis/kars.azure.com/v1alpha1/namespaces/tenant-a/karstasks",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(task_list(std::slice::from_ref(&member))),
        )
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(task_path(&member.name_any())))
        .respond_with(ResponseTemplate::new(200).set_body_json(&member))
        .expect(1)
        .mount(&server)
        .await;
    tasks::reconcile_revocations(&Api::namespaced(client(&server).await, "tenant-a"), &team)
        .await
        .unwrap();
    let requests = server.received_requests().await.unwrap();
    let deleted: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
    assert_eq!(deleted["preconditions"]["uid"], "member-uid");
    assert_eq!(deleted["preconditions"]["resourceVersion"], "40");
}

#[tokio::test]
async fn revocation_delete_failure_is_retryable_not_success() {
    let server = MockServer::start().await;
    let team = team();
    let member = owned_task(
        &team,
        &TeamRole {
            name: "removed".into(),
            ..Default::default()
        },
    );
    Mock::given(method("GET"))
        .and(path(
            "/apis/kars.azure.com/v1alpha1/namespaces/tenant-a/karstasks",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(task_list(std::slice::from_ref(&member))),
        )
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(task_path(&member.name_any())))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    assert!(
        tasks::revoke_all(&Api::namespaced(client(&server).await, "tenant-a"), &team)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn existing_cadence_slot_survives_pause_and_resume_without_replay() {
    let server = MockServer::start().await;
    let mut team = team();
    team.spec.envelope.budget = None;
    let name = runs::cadence_name(&team).unwrap();
    let mut run = KarsTask::new(&name, specs::run_spec(&team, "previous knowledge").unwrap());
    run.metadata.namespace = team.metadata.namespace.clone();
    run.metadata.uid = Some("run-uid".into());
    run.metadata.resource_version = Some("50".into());
    run.metadata.owner_references = Some(vec![tasks::owner_ref(&team).unwrap()]);
    run.metadata.annotations = Some(
        [
            (ANNOT_TEAM_ROLE.into(), "taskforce".into()),
            (ANNOT_RUN_REQUESTED.into(), name.clone()),
        ]
        .into(),
    );
    let api = Api::namespaced(client(&server).await, "tenant-a");
    for launched in [true, false] {
        server.reset().await;
        run.spec.execution.as_mut().unwrap().launch = launched;
        Mock::given(method("GET"))
            .and(path(task_path(&name)))
            .respond_with(ResponseTemplate::new(200).set_body_json(&run))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(
                "/apis/kars.azure.com/v1alpha1/namespaces/tenant-a/karstasks",
            ))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(task_list(std::slice::from_ref(&run))),
            )
            .mount(&server)
            .await;
        for paused in [true, false] {
            team.spec.paused = paused;
            tasks::reconcile_revocations(&api, &team).await.unwrap();
            let saved = tasks::apply_task(
                &api,
                &team,
                &name,
                specs::run_spec(&team, "new knowledge").unwrap(),
                "taskforce",
            )
            .await
            .unwrap();
            assert_eq!(
                serde_json::to_value(&saved).unwrap(),
                serde_json::to_value(&run).unwrap()
            );
        }
        let requests = server.received_requests().await.unwrap();
        assert!(requests.iter().all(|request| request.method == "GET"));
    }
}

#[tokio::test]
async fn skill_merge_inherits_runtime_model_isolation_and_checks_current_digest() {
    let server = MockServer::start().await;
    let mut team = team();
    team.spec.blueprint = Some(TaskBlueprint {
        runtime: Some("Hermes".into()),
        isolation: Some("confidential".into()),
        model: Some(TaskModel {
            provider: "azure-openai".into(),
            deployment: "model-a".into(),
        }),
        ..Default::default()
    });
    team.spec.roster.push(TeamRole {
        name: "reader".into(),
        skills: vec!["read".into()],
        ..Default::default()
    });
    let mut skill = crate::kars_skill::KarsSkill::new(
        "read",
        crate::kars_skill::KarsSkillSpec {
            summary: "read repository".into(),
            version: "1".into(),
            bounding_policy: "read-only".into(),
            ..Default::default()
        },
    );
    skill.metadata.generation = Some(2);
    skill.metadata.uid = Some("skill-uid".into());
    skill.metadata.resource_version = Some("20".into());
    skill.metadata.annotations = Some(std::collections::BTreeMap::from([
        (crate::skill_package::REVIEW.into(), "approved".into()),
        (crate::skill_package::LOCKED_UID.into(), "skill-uid".into()),
        (crate::skill_package::LOCKED_GENERATION.into(), "2".into()),
        (
            crate::skill_package::LOCKED_DIGEST.into(),
            skill.version_digest(),
        ),
        (
            crate::skill_package::UPLOADED_BY_SUB.into(),
            "author".into(),
        ),
        (
            crate::skill_package::APPROVED_BY_SUB.into(),
            "reviewer".into(),
        ),
    ]));
    skill.status = Some(crate::kars_skill::KarsSkillStatus {
        phase: Some("Ready".into()),
        observed_generation: Some(2),
        version_digest: Some(skill.version_digest()),
        ..Default::default()
    });
    Mock::given(method("GET"))
        .and(path(
            "/apis/kars.azure.com/v1alpha1/namespaces/tenant-a/karsskills/read",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(&skill))
        .mount(&server)
        .await;
    let effective = capabilities::effective_team(&client(&server).await, &team)
        .await
        .unwrap();
    let blueprint = effective.spec.roster[0].blueprint.as_ref().unwrap();
    assert_eq!(blueprint.runtime.as_deref(), Some("Hermes"));
    assert_eq!(blueprint.isolation.as_deref(), Some("confidential"));
    assert_eq!(blueprint.model.as_ref().unwrap().deployment, "model-a");
    assert_eq!(blueprint.tool_policy.as_deref(), Some("read-only"));
    assert_eq!(blueprint.skills, ["read"]);
    server.reset().await;
    skill.metadata.generation = Some(3);
    Mock::given(method("GET"))
        .and(path(
            "/apis/kars.azure.com/v1alpha1/namespaces/tenant-a/karsskills/read",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(&skill))
        .mount(&server)
        .await;
    assert!(
        capabilities::effective_team(&client(&server).await, &team)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn missing_profile_skill_and_mcp_api_errors_fail_closed() {
    let server = MockServer::start().await;
    let mut team = team();
    team.spec.profile_ref = Some(LocalObjectRef {
        name: "required".into(),
    });
    Mock::given(method("GET"))
        .respond_with(not_found())
        .mount(&server)
        .await;
    assert!(
        capabilities::effective_team(&client(&server).await, &team)
            .await
            .is_err()
    );
    team.spec.profile_ref = None;
    team.spec.roster.push(TeamRole {
        name: "reader".into(),
        skills: vec!["required".into()],
        ..Default::default()
    });
    assert!(
        capabilities::effective_team(&client(&server).await, &team)
            .await
            .is_err()
    );
    server.reset().await;
    team.spec.roster.clear();
    team.spec.blueprint = Some(TaskBlueprint {
        mcp_servers: vec!["required".into()],
        ..Default::default()
    });
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    assert!(
        capabilities::capability_readiness(&client(&server).await, &team)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn approved_promotion_is_consumed_and_requested_tier_cleared_in_one_cas() {
    let server = MockServer::start().await;
    let mut team = team();
    team.spec.requested_tier = Some(5);
    let principal = principal(&team);
    let approval = approved(&team, &principal);
    Mock::given(method("GET"))
        .and(path(task_path(&principal.name_any())))
        .respond_with(ResponseTemplate::new(200).set_body_json(&principal))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/apis/kars.azure.com/v1alpha1/namespaces/tenant-a/karsapprovals/{}",
            approval.name_any()
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(&approval))
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path(
            "/apis/kars.azure.com/v1alpha1/namespaces/tenant-a/karsteams/eng",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(&team))
        .expect(1)
        .mount(&server)
        .await;
    assert!(
        promotion::process_promotion(&client(&server).await, &team, &principal.name_any())
            .await
            .unwrap()
    );
    let requests = server.received_requests().await.unwrap();
    let patch: serde_json::Value = serde_json::from_slice(
        &requests
            .iter()
            .find(|request| request.method == "PATCH")
            .unwrap()
            .body,
    )
    .unwrap();
    assert_eq!(patch["metadata"]["resourceVersion"], "10");
    assert_eq!(patch["metadata"]["uid"], "team-uid");
    assert_eq!(
        patch["metadata"]["annotations"]["kars.azure.com/consumed-promotion"],
        "approval-uid"
    );
    assert_eq!(patch["spec"]["envelope"]["tier"], 5);
    assert!(patch["spec"]["requestedTier"].is_null());
    assert!(
        patch["spec"]
            .as_object()
            .unwrap()
            .contains_key("requestedTier")
    );
}

#[tokio::test]
async fn harvest_list_failure_is_not_zero_active_runs() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let client = client(&server).await;
    assert!(
        runs::harvest_and_retire_runs(
            &client,
            &Api::namespaced(client.clone(), "tenant-a"),
            &team()
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn malformed_or_foreign_digest_is_never_overwritten() {
    let server = MockServer::start().await;
    let team = team();
    Mock::given(method("GET")).and(path("/api/v1/namespaces/tenant-a/configmaps/kars-team-digest-eng"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "apiVersion": "v1", "kind": "ConfigMap",
            "metadata": { "name": "kars-team-digest-eng", "namespace": "tenant-a", "uid": "foreign", "resourceVersion": "8" },
            "data": { "log.json": "[]" },
        }))).mount(&server).await;
    assert!(
        crate::team_digest::publish(
            &client(&server).await,
            &team,
            None,
            "Healthy",
            "report",
            1,
            1,
            Some(1),
            crate::inference_budget::team::SYNTHESIS_SCOPE,
            1
        )
        .await
        .is_err()
    );
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|request| request.method == "GET")
    );
}
