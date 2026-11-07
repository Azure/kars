// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::routes::teams::{CreateTeamRequest, UpdateTeamRequest};

fn body(extra: serde_json::Value) -> CreateTeamRequest {
    let mut value = json!({"name":"qualified-attempt","charter":"Use the reviewed MCP server"});
    value
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    serde_json::from_value(value).unwrap()
}

#[test]
fn team_qualification_defaults_off_and_requires_explicit_finite_routes() {
    assert!(!body(json!({})).qualification_run);
    assert!(validate_designation(&body(json!({}))).is_ok());
    assert!(
        serde_json::from_value::<CreateTeamRequest>(
            json!({"name":"t","charter":"c","qualification_run":"true"})
        )
        .is_err()
    );
    let valid = json!({"qualification_run":true,"runtime":"OpenClaw","model":"github-copilot::principal","budget":{"tokens":10000},"roles":[{"name":"worker","runtime":"Hermes","model":"foundry::worker"}]});
    assert!(validate_designation(&body(valid.clone())).is_ok());
    for (key, replacement) in [
        ("runtime", json!("")),
        ("model", json!("missing-provider")),
        ("budget", json!({"tokens":0})),
        ("launch", json!(true)),
    ] {
        let mut invalid = valid.clone();
        invalid[key] = replacement;
        assert!(validate_designation(&body(invalid)).is_err(), "{key}");
    }
    let roles = body(valid).roles;
    assert_eq!(roles[0].runtime.as_deref(), Some("Hermes"));
    assert_eq!(roles[0].model.as_deref(), Some("foundry::worker"));
}

#[tokio::test]
async fn team_qualification_designation_and_review_reject_nonoperators_before_cluster_access() {
    let principal = || Principal {
        sub: "reader".into(),
        name: "Reader".into(),
        roles: vec!["viewer".into()],
    };
    let state = || crate::state::AppState::web_only("work".into());
    let create = crate::routes::teams::create_team(
        State(state()),
        Extension(principal()),
        Path("work".into()),
        Json(body(json!({"qualification_run":true}))),
    )
    .await;
    assert!(matches!(create, Err(AppError::Forbidden(_))));
    let compose = crate::routes::compose::compose_team(
        State(state()),
        Extension(principal()),
        Path("work".into()),
        Json(
            serde_json::from_value(json!({
                "charter":"review","qualification_run":true,
                "spending":{"budget_tokens":null,"role_tokens":{}}
            }))
            .unwrap(),
        ),
    )
    .await;
    assert!(matches!(compose, Err(AppError::Forbidden(_))));
    let attempt = QualificationAttemptRequest {
        team_uid: "uid".into(),
        expected: serde_json::from_value(
            json!({"id":"first","title":"Use MCP","description":"Fetch docs","status":"pending"}),
        )
        .unwrap(),
    };
    assert!(matches!(
        qualify_team(
            State(state()),
            Extension(principal()),
            Path(("work".into(), "t".into())),
            Json(attempt)
        )
        .await,
        Err(AppError::Forbidden(_))
    ));
    let review: UpdateTeamRequest = serde_json::from_value(
        json!({"qualification_reviewed":true,"qualification_team_uid":"uid"}),
    )
    .unwrap();
    assert!(matches!(
        crate::routes::teams::update_team(
            State(state()),
            Extension(principal()),
            Path(("work".into(), "t".into())),
            Json(review)
        )
        .await,
        Err(AppError::Forbidden(_))
    ));
}

#[test]
fn team_qualification_release_requires_exact_paused_consumption_not_expiry_renewal() {
    let mut team: KarsTeam = serde_json::from_value(json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTeam","metadata":{"name":"t","namespace":"work","uid":"uid"},"spec":{"charter":"c","paused":true,"envelope":{"tier":2,"authorityCeiling":1}}})).unwrap();
    let now = Utc::now() - chrono::Duration::hours(1);
    let record = contract::Record {
        approved_by: "Operator".into(),
        approved_by_sub: "operator".into(),
        approved_at: now,
        expires_at: now + chrono::Duration::minutes(45),
        team_uid: "uid".into(),
        package_sha256: contract::package(&json!(team)),
        resources: Default::default(),
        backlog_uid: "backlog".into(),
        item_id: "item".into(),
        work_sha256: "a".repeat(64),
        request: format!("manual-1-{}", "b".repeat(64)),
    };
    team.annotations_mut().insert(
        contract::RECORD.into(),
        serde_json::to_string(&record).unwrap(),
    );
    team.annotations_mut()
        .insert(contract::CONSUMED.into(), "task-uid".into());
    team.status
        .get_or_insert_with(Default::default)
        .run_admission = Some(crate::kars::team::TeamRunAdmission {
        request: record.request.clone(),
        task_uid: "task-uid".into(),
        task_name: "attempt".into(),
        authority_digest: "authority".into(),
        task_spec_digest: "spec".into(),
    });
    let released = release_record(&team, Some("uid")).unwrap();
    assert_eq!(released.expires_at, record.expires_at);
    assert!(contract::validate(&released, &json!(team), released.approved_at).is_ok());
    assert!(contract::validate(&released, &json!(team), Utc::now()).is_err());
    assert!(team.spec.paused);
    assert!(release_record(&team, Some("replacement")).is_err());
    assert!(release_record(&team, None).is_err());
    for case in [
        "unpaused",
        "pending",
        "unconsumed",
        "wrong-task",
        "no-reservation",
        "no-approval",
    ] {
        let mut changed = team.clone();
        match case {
            "unpaused" => changed.spec.paused = false,
            "pending" => {
                changed
                    .annotations_mut()
                    .insert("kars.azure.com/run-now".into(), record.request.clone());
            }
            "unconsumed" => {
                changed.annotations_mut().remove(contract::CONSUMED);
            }
            "wrong-task" => {
                changed
                    .annotations_mut()
                    .insert(contract::CONSUMED.into(), "replacement".into());
            }
            "no-reservation" => changed.status.as_mut().unwrap().run_admission = None,
            "no-approval" => {
                changed.annotations_mut().remove(contract::RECORD);
            }
            _ => unreachable!(),
        }
        assert!(release_record(&changed, Some("uid")).is_err(), "{case}");
    }
}

#[test]
fn team_qualification_release_waits_for_active_descendants_even_after_root_retirement() {
    let mut team: KarsTeam = serde_json::from_value(json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTeam","metadata":{"name":"t","namespace":"work","uid":"uid"},"spec":{"charter":"c","paused":true,"envelope":{"tier":2,"authorityCeiling":1}}})).unwrap();
    team.annotations_mut()
        .insert(contract::CONSUMED.into(), "root-uid".into());
    let child: crate::kars::task::KarsTask = serde_json::from_value(json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTask","metadata":{"name":"child","namespace":"work","uid":"child-uid","ownerReferences":[{"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTask","name":"root","uid":"root-uid","controller":true}]},"spec":{"objective":"work","envelope":{"tier":1,"authorityCeiling":1},"execution":{"launch":true}}})).unwrap();
    assert!(has_active_attempt(&team, std::slice::from_ref(&child)));
    let mut stopped = child.clone();
    stopped.spec.execution.as_mut().unwrap().launch = false;
    assert!(!has_active_attempt(&team, &[stopped]));
    let mut foreign = child;
    foreign.metadata.owner_references.as_mut().unwrap()[0].uid = "other-root".into();
    assert!(!has_active_attempt(&team, &[foreign]));
}

fn binding_team() -> serde_json::Value {
    json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTeam",
        "metadata":{"name":"t","namespace":"work","uid":"original","resourceVersion":"1","generation":1,
            "annotations":{(contract::PENDING):"binding","kars.azure.com/created-by":"Operator",
                "kars.azure.com/owner-sub":"operator","kars.azure.com/owner-name":"Operator",
                "kars.azure.com/mission-decomposition":"execution-plan/v1"}},
        "spec":{"charter":"c","paused":true,"envelope":{"tier":2,"authorityCeiling":1}}})
}

#[test]
fn team_qualification_creation_recovery_requires_identical_owner_spec_and_no_attempt() {
    let desired = binding_team();
    let mut existing = desired.clone();
    existing["status"] = json!({"phase":"Hibernating","generatedTaskCount":0,"tokensSpentTotal":0});
    assert!(validate_creation_recovery(&desired, &existing).is_ok());
    let mut strict_request = desired.clone();
    strict_request["spec"]["blueprint"] =
        json!({"egressMode":"Strict","egress":[{"host":"docs.rs","port":443}]});
    let mut pruned = existing.clone();
    pruned["spec"]["blueprint"] = json!({"egress":[{"host":"docs.rs","port":443}]});
    assert!(validate_creation_recovery(&strict_request, &pruned).is_ok());
    for mode in [json!("Learn"), json!("learning"), json!(null)] {
        let mut changed = strict_request.clone();
        changed["spec"]["blueprint"]["egressMode"] = mode;
        assert!(validate_creation_recovery(&changed, &pruned).is_err());
    }
    for (key, value) in [
        ("egressMode", json!("Learn")),
        ("egress", json!([])),
        ("unknownAuthority", json!(true)),
    ] {
        let mut changed = pruned.clone();
        changed["spec"]["blueprint"][key] = value;
        assert!(validate_creation_recovery(&strict_request, &changed).is_err());
    }
    for (field, value) in [
        ("/spec/charter", json!("changed")),
        ("/spec/paused", json!(false)),
        ("/metadata/name", json!("different")),
        ("/metadata/namespace", json!("different")),
        ("/metadata/uid", json!("")),
        (
            "/metadata/annotations/kars.azure.com~1owner-sub",
            json!("different"),
        ),
        (
            "/metadata/annotations/kars.azure.com~1owner-name",
            json!("different"),
        ),
        (
            "/metadata/annotations/kars.azure.com~1created-by",
            json!("different"),
        ),
        (
            "/metadata/annotations/kars.azure.com~1mission-decomposition",
            json!("changed"),
        ),
    ] {
        let mut changed = existing.clone();
        *changed.pointer_mut(field).unwrap() = value;
        assert!(
            validate_creation_recovery(&desired, &changed).is_err(),
            "{field}"
        );
    }
    for key in [
        contract::RECORD,
        contract::CONSUMED,
        "kars.azure.com/run-now",
    ] {
        let mut changed = existing.clone();
        changed["metadata"]["annotations"][key] = json!("existing-attempt");
        assert!(
            validate_creation_recovery(&desired, &changed).is_err(),
            "{key}"
        );
    }
    for key in [
        "runAdmission",
        "lastGeneratedTask",
        "lastRunAt",
        "generatedTaskCount",
        "tokensSpentTotal",
        "runsSucceeded",
    ] {
        let mut changed = existing.clone();
        changed["status"][key] = json!(1);
        assert!(
            validate_creation_recovery(&desired, &changed).is_err(),
            "{key}"
        );
    }
    let mut changed = existing.clone();
    changed["metadata"]["deletionTimestamp"] = json!("2026-10-18T00:00:00Z");
    assert!(validate_creation_recovery(&desired, &changed).is_err());
    for pending in [json!("expired-record"), json!("valid-record"), json!(null)] {
        changed = existing.clone();
        changed["metadata"]["annotations"][contract::PENDING] = pending;
        assert!(validate_creation_recovery(&desired, &changed).is_err());
    }
}

async fn qualification_patch_race(
    latest: serde_json::Value,
    failures: Vec<u16>,
    patch: serde_json::Value,
) -> (AppResult<()>, Vec<serde_json::Value>) {
    use axum::{
        body::{Body, to_bytes},
        http::{Request, Response},
    };
    use std::sync::{Arc, Mutex};
    let patches = Arc::new(Mutex::new(Vec::new()));
    let recorded = patches.clone();
    let service = tower::service_fn(move |request: Request<kube::client::Body>| {
        let latest = latest.clone();
        let failures = failures.clone();
        let recorded = recorded.clone();
        async move {
            assert_eq!(
                request.uri().path(),
                "/apis/kars.azure.com/v1alpha1/namespaces/work/karsteams/t"
            );
            let status = if request.method() == "PATCH" {
                let bytes = to_bytes(Body::new(request.into_body()), 65536)
                    .await
                    .unwrap();
                let mut recorded = recorded.lock().unwrap();
                let status = failures.get(recorded.len()).copied().unwrap_or(200);
                recorded.push(serde_json::from_slice::<serde_json::Value>(&bytes).unwrap());
                status
            } else {
                assert_eq!(request.method(), "GET");
                200
            };
            let body = if status == 200 {
                latest
            } else {
                json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"Conflict","code":status,"message":"test failure"})
            };
            Ok::<_, std::io::Error>(
                Response::builder()
                    .status(status)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
        }
    });
    let api = contract::api(kube::Client::new(service, "work"), "work", "KarsTeam");
    let result = patch_reviewed(
        &api,
        &serde_json::from_value(binding_team()).unwrap(),
        patch,
    )
    .await;
    let records = patches.lock().unwrap().clone();
    (result, records)
}

#[tokio::test]
async fn team_qualification_status_conflict_rebases_binding_and_same_execution_request_only() {
    let mut latest = binding_team();
    latest["metadata"]["resourceVersion"] = json!("2");
    latest["metadata"]["managedFields"] = json!([]);
    latest["status"] = json!({"phase":"Hibernating","observedGeneration":1,"generatedTaskCount":0,
        "tokensSpentTotal":0,"runsSucceeded":0,"memberCount":2});
    for patch in [
        json!({"metadata":{"annotations":{(contract::PENDING):"original-bound-record"}}}),
        json!({"metadata":{"annotations":{(contract::PENDING):null,(contract::RECORD):"original-approved-record",
            "kars.azure.com/run-now":"manual-original-nonce"}},"spec":{"paused":false}}),
    ] {
        let (result, patches) =
            qualification_patch_race(latest.clone(), vec![409], patch.clone()).await;
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(patches.len(), 2);
        for (i, actual) in patches.iter().enumerate() {
            let mut expected = patch.clone();
            expected["metadata"]["uid"] = json!("original");
            expected["metadata"]["resourceVersion"] = json!((i + 1).to_string());
            assert_eq!(actual, &expected);
        }
    }
}

#[tokio::test]
async fn team_qualification_conflict_never_rebases_changed_authority_or_consumption() {
    let original = binding_team();
    let mut changes = Vec::new();
    for (key, value) in [
        ("uid", json!("replacement")),
        ("generation", json!(2)),
        ("deletionTimestamp", json!("2026-10-18T00:00:00Z")),
        ("labels", json!({"owner":"other"})),
    ] {
        let mut changed = original.clone();
        changed["metadata"][key] = value;
        changes.push(changed);
    }
    for key in [
        contract::PENDING,
        contract::RECORD,
        contract::CONSUMED,
        "kars.azure.com/run-now",
        "kars.azure.com/owner-sub",
    ] {
        let mut changed = original.clone();
        changed["metadata"]["annotations"][key] = json!("changed");
        changes.push(changed);
    }
    let mut changed = original.clone();
    changed["spec"]["charter"] = json!("different");
    changes.push(changed);
    for key in [
        "runAdmission",
        "lastGeneratedTask",
        "lastRunAt",
        "generatedTaskCount",
        "tokensSpentTotal",
        "runsSucceeded",
    ] {
        let mut changed = original.clone();
        changed["status"] = json!({key:1});
        changes.push(changed);
    }
    for mut latest in changes {
        latest["metadata"]["resourceVersion"] = json!("2");
        let (result, patches) =
            qualification_patch_race(latest, vec![409], json!({"metadata":{}})).await;
        assert!(result.is_err());
        assert_eq!(patches.len(), 1);
    }
}

#[tokio::test]
async fn team_qualification_conflicts_are_bounded_and_other_failures_are_not_retried() {
    for failures in [vec![409, 409, 409], vec![403], vec![500], vec![422]] {
        let expected = failures.len();
        let (result, patches) =
            qualification_patch_race(binding_team(), failures, json!({"metadata":{}})).await;
        assert!(result.is_err());
        assert_eq!(patches.len(), expected);
    }
}

#[test]
fn team_qualification_status_never_claims_evidence_or_normal_admission() {
    let mut team: KarsTeam = serde_json::from_value(json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTeam","metadata":{"name":"t","namespace":"work","uid":"uid"},"spec":{"charter":"c","paused":true,"envelope":{"tier":2,"authorityCeiling":1}}})).unwrap();
    assert!(status(&team).is_none());
    team.annotations_mut()
        .insert(contract::PENDING.into(), "binding".into());
    assert_eq!(status(&team).unwrap().state, "blocked");
    team.annotations_mut().remove(contract::PENDING);
    team.annotations_mut()
        .insert(contract::RECORD.into(), "malformed".into());
    assert_eq!(status(&team).unwrap().state, "requested");
    team.annotations_mut()
        .insert(contract::CONSUMED.into(), "task-uid".into());
    assert_eq!(status(&team).unwrap().state, "awaiting_evidence");
    team.spec.paused = false;
    assert_eq!(status(&team).unwrap().state, "running");
    assert!(status(&team).unwrap().note.contains("Unqualified"));
}
