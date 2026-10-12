// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use axum::{
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode},
    response::Response,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tower::{ServiceExt, service_fn};

const SKILL: &str = "/apis/kars.azure.com/v1alpha1/namespaces/kars-system/karsskills/checklist";
const PACKAGE: &str = "/api/v1/namespaces/kars-system/configmaps/karsskill-checklist";
const TEST_SECRET: &str = "test-only-skills-principal-key";

#[test]
fn skill_summary_explains_rejected_controller_validation() {
    let mut raw = json!({
        "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSkill",
        "metadata":{"name":"checklist","namespace":"kars-system","uid":"skill-uid",
            "resourceVersion":"10","generation":1},
        "spec":{"summary":"Check local inputs.","version":"1.0.0","boundingPolicy":"kars-default"},
        "status":{"phase":"Ready","observedGeneration":1,
            "versionDigest":"sha256:98698de66277886a1dc35e2074b9b9d2",
            "detail":"Content validated"}
    });
    let summary = to_skill(&serde_json::from_value(raw.clone()).unwrap());
    assert!(summary.validated);
    assert_eq!(
        summary.validation_detail.as_deref(),
        Some("Content validated")
    );
    raw["spec"]["summary"] = json!("Changed after validation");
    let summary = to_skill(&serde_json::from_value(raw).unwrap());
    assert!(!summary.validated);
    assert_eq!(
        summary.validation_detail.as_deref(),
        Some("Skill content changed since controller validation")
    );
}

struct Store {
    skill: Value,
    package: Value,
    writes: Vec<Value>,
    race: bool,
    requests: usize,
}

fn fixture() -> (AppState, Arc<Mutex<Store>>) {
    let files: BTreeMap<String, String> = serde_json::from_str(include_str!(
        "../../../../../tests/fixtures/skill-package.json"
    ))
    .unwrap();
    let encoded: BTreeMap<_, _> = files
        .iter()
        .map(|(p, c)| (p.replace('/', "__"), c.clone()))
        .collect();
    let mut skill = json!({
        "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSkill",
        "metadata":{"name":"checklist","namespace":"kars-system","uid":"skill-uid",
            "resourceVersion":"10","generation":2,
            "annotations":{(package::UPLOADED_BY_SUB):"author",(package::REVIEW):"pending"}},
        "spec":{"summary":"Review evidence","version":"1","boundingPolicy":"read-only",
            "package":true,"files":files.keys().collect::<Vec<_>>(),
            "packageDigest":package::package_digest(&encoded).unwrap()},
        "status":{"phase":"Ready","observedGeneration":2}
    });
    skill["status"]["versionDigest"] = json!(package::version_digest(&skill["spec"]));
    let package = json!({
        "apiVersion":"v1","kind":"ConfigMap",
        "metadata":{"name":"karsskill-checklist","namespace":"kars-system","uid":"package-uid","resourceVersion":"20",
            "ownerReferences":[{"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSkill",
                "name":"checklist","uid":"skill-uid","controller":true}]},
        "data":encoded
    });
    let store = Arc::new(Mutex::new(Store {
        skill,
        package,
        writes: vec![],
        race: false,
        requests: 0,
    }));
    let state = store.clone();
    let service = service_fn(move |request: Request<kube::client::Body>| {
        let state = state.clone();
        async move {
            let method = request.method().clone();
            let path = request.uri().path().to_owned();
            if method == Method::PATCH && path == SKILL {
                assert_eq!(
                    request.headers()["content-type"],
                    "application/merge-patch+json"
                );
            }
            assert!(
                !request
                    .uri()
                    .query()
                    .unwrap_or_default()
                    .contains("force=true")
            );
            let bytes = to_bytes(Body::new(request.into_body()), 1_048_576)
                .await
                .unwrap();
            let body = if bytes.is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(&bytes).unwrap()
            };
            let mut store = state.lock().unwrap();
            store.requests += 1;
            let (status, response) = match (method, path.as_str()) {
                (Method::GET, SKILL) => (200, store.skill.clone()),
                (Method::GET, PACKAGE) => (200, store.package.clone()),
                (Method::PATCH, SKILL) => {
                    store.writes.push(body.clone());
                    if store.race {
                        store.skill["metadata"]["resourceVersion"] = "11".into();
                    }
                    if body["metadata"]["uid"] != store.skill["metadata"]["uid"]
                        || body["metadata"]["resourceVersion"]
                            != store.skill["metadata"]["resourceVersion"]
                    {
                        (
                            409,
                            json!({"apiVersion":"v1","kind":"Status","status":"Failure",
                            "reason":"Conflict","code":409,"message":"Concurrent review"}),
                        )
                    } else {
                        json_patch::merge(&mut store.skill, &body);
                        store.skill["metadata"]["resourceVersion"] = "12".into();
                        (200, store.skill.clone())
                    }
                }
                (method, path) => panic!("Unexpected {method} {path}"),
            };
            Ok::<_, std::io::Error>(
                Response::builder()
                    .status(status)
                    .header("content-type", "application/json")
                    .body(Body::from(response.to_string()))
                    .unwrap(),
            )
        }
    });
    (
        AppState::for_test_client(kube::Client::new(service, "kars-system"), "kars-system")
            .with_principal_secret(Some(TEST_SECRET.into())),
        store,
    )
}

fn approval(store: &Store) -> Value {
    json!({"uid":"skill-uid","generation":2,"version_digest":store.skill["status"]["versionDigest"]})
}

#[tokio::test]
async fn skill_replacement_uses_cas_clears_review_and_removes_omitted_spec_fields() {
    let (state, store) = fixture();
    let body = approval(&store.lock().unwrap());
    assert_eq!(
        request(state.clone(), Method::POST, "approve", "reviewer", body)
            .await
            .0,
        StatusCode::OK
    );
    let spec = json!({"version":"2","summary":"Review only local evidence","boundingPolicy":"read-only","recipe":"Report limitations."});
    let result = save_skill(
        require_cluster(&state).unwrap(),
        "checklist",
        "kars-system",
        spec.clone(),
        &Principal {
            sub: "author".into(),
            name: "Author".into(),
            roles: vec!["user".into()],
        },
        true,
    )
    .await
    .unwrap();
    assert_eq!(result.data["spec"], spec);
    let metadata = serde_json::to_value(result.metadata).unwrap();
    assert_eq!(metadata["uid"], "skill-uid");
    assert_eq!(metadata["annotations"][package::REVIEW], "pending");
    for key in [
        package::LOCKED_DIGEST,
        package::LOCKED_UID,
        package::LOCKED_GENERATION,
        package::APPROVED_BY_SUB,
        ANN_APPROVED_BY,
        ANN_APPROVED_AT,
    ] {
        assert!(metadata["annotations"].get(key).is_none(), "{key}");
    }
    assert_eq!(store.lock().unwrap().writes.len(), 2);
}

#[tokio::test]
async fn skill_replacement_rejects_foreign_submitters_and_concurrent_changes() {
    for (subject, race) in [("other", false), ("author", true)] {
        let (state, store) = fixture();
        store.lock().unwrap().race = race;
        let original = store.lock().unwrap().skill["spec"].clone();
        let result = save_skill(
            require_cluster(&state).unwrap(), "checklist", "kars-system",
            json!({"version":"2","summary":"Review only local evidence","boundingPolicy":"read-only"}),
            &Principal { sub:subject.into(),name:subject.into(),roles:vec!["user".into()] }, true,
        ).await;
        assert!(
            matches!(result, Err(AppError::Conflict(_))) && race
                || matches!(result, Err(AppError::Forbidden(_))) && !race
        );
        let store = store.lock().unwrap();
        assert_eq!(store.skill["spec"], original);
        assert_eq!(store.writes.len(), usize::from(race));
    }
}

async fn request(
    state: AppState,
    method: Method,
    route: &str,
    subject: &str,
    body: Value,
) -> (StatusCode, Value) {
    request_as(state, method, route, subject, &["operator"], body).await
}

async fn request_as(
    state: AppState,
    method: Method,
    route: &str,
    subject: &str,
    roles: &[&str],
    body: Value,
) -> (StatusCode, Value) {
    let token = jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &json!({"sub":subject,"name":subject,"roles":roles,"exp":chrono::Utc::now().timestamp()+3600}),
        &jsonwebtoken::EncodingKey::from_secret(TEST_SECRET.as_bytes()),
    )
    .unwrap();
    let response = crate::routes::router(state.clone())
        .layer(axum::middleware::from_fn_with_state(
            state,
            crate::auth::require_token,
        ))
        .oneshot(
            Request::builder()
                .method(method)
                .uri(format!("/api/operator/skills/checklist/{route}"))
                .header("content-type", "application/json")
                .header(crate::auth::PRINCIPAL_HEADER, token)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1_048_576).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn skill_routes_enforce_signed_operator_roles_before_reading_or_writing_packages() {
    for role in ["user", "auditor", "viewer"] {
        let (state, store) = fixture();
        for (method, route) in [
            (Method::GET, "package"),
            (Method::POST, "approve"),
            (Method::POST, "revoke"),
        ] {
            let (status, _) = request_as(
                state.clone(),
                method,
                route,
                "unprivileged",
                &[role],
                Value::Null,
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN);
        }
        assert_eq!(store.lock().unwrap().requests, 0);
    }
    let (state, store) = fixture();
    let (status, _) = request_as(state, Method::GET, "package", "empty", &[], Value::Null).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(store.lock().unwrap().requests, 0);
}

#[tokio::test]
async fn skill_review_reads_exact_files_then_approves_and_revokes_current_identity() {
    let (state, store) = fixture();
    let (status, inspected) = request(
        state.clone(),
        Method::GET,
        "package",
        "reviewer",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        inspected["files"]["references/check.md"],
        "Consent: caf\u{e9} \u{2713}\n"
    );
    assert_eq!(inspected["files"]["empty.txt"], "");
    assert_eq!(inspected["skill"]["validated"], true);
    let body = approval(&store.lock().unwrap());
    let (status, reviewed) =
        request(state.clone(), Method::POST, "approve", "reviewer", body).await;
    assert_eq!(status, StatusCode::OK, "{reviewed}");
    assert_eq!(reviewed["usable"], true);
    let (status, revoked) = request(
        state,
        Method::POST,
        "revoke",
        "reviewer",
        json!({"uid":"skill-uid"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(revoked["usable"], false);
    let store = store.lock().unwrap();
    assert_eq!(store.writes[0]["metadata"]["uid"], "skill-uid");
    assert_eq!(store.writes[0]["metadata"]["resourceVersion"], "10");
    assert_eq!(
        store.writes[0]["metadata"]["annotations"][package::APPROVED_BY_SUB],
        "reviewer"
    );
    assert_eq!(
        store.writes[0]["metadata"]["annotations"][package::LOCKED_GENERATION],
        "2"
    );
}

#[tokio::test]
async fn skill_review_refuses_self_review_stale_display_and_package_mutation() {
    for change in [
        "self",
        "uid",
        "generation",
        "digest",
        "stale",
        "bytes",
        "owner",
    ] {
        let (state, store) = fixture();
        let mut body = approval(&store.lock().unwrap());
        {
            let mut store = store.lock().unwrap();
            match change {
                "uid" => body["uid"] = "old-uid".into(),
                "generation" => body["generation"] = 1.into(),
                "digest" => body["version_digest"] = "old-digest".into(),
                "stale" => store.skill["status"]["observedGeneration"] = 1.into(),
                "bytes" => store.package["data"]["2"] = "changed".into(),
                "owner" => {
                    store.package["metadata"]["ownerReferences"][0]["uid"] = "foreign".into()
                }
                "self" => (),
                _ => unreachable!(),
            }
        }
        let (status, _) = request(
            state,
            Method::POST,
            "approve",
            if change == "self" {
                "author"
            } else {
                "reviewer"
            },
            body,
        )
        .await;
        assert_eq!(
            status,
            if change == "self" {
                StatusCode::FORBIDDEN
            } else {
                StatusCode::CONFLICT
            },
            "{change}"
        );
        assert!(store.lock().unwrap().writes.is_empty(), "{change}");
    }
}

#[tokio::test]
async fn skill_review_cas_race_is_not_forced_or_retried() {
    let (state, store) = fixture();
    let body = approval(&store.lock().unwrap());
    store.lock().unwrap().race = true;
    let (status, response) = request(state, Method::POST, "approve", "reviewer", body).await;
    assert_eq!(status, StatusCode::CONFLICT, "{response}");
    let store = store.lock().unwrap();
    assert_eq!(store.writes.len(), 1);
    assert_eq!(
        store.skill["metadata"]["annotations"][package::REVIEW],
        "pending"
    );
}
