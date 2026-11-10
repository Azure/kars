// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::work;
use crate::{auth::Principal, state::AppState};
use axum::{
    Extension,
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode},
    response::Response,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tower::{ServiceExt, service_fn};

#[derive(Default)]
struct Store {
    team: Value,
    backlog: Value,
    approval: Value,
    calls: Vec<(Method, String, Value)>,
    before_put: Option<Value>,
    team_reads: usize,
    resume_before_guard: bool,
}

fn items(store: &Store) -> Vec<work::TeamTask> {
    serde_json::from_str(store.backlog["data"]["tasks.json"].as_str().unwrap()).unwrap()
}

fn put_item(store: &mut Store, item: &work::TeamTask) {
    store.backlog["data"]["tasks.json"] = json!(serde_json::to_string(&vec![item]).unwrap());
}

fn fixture(review: bool) -> (AppState, Arc<Mutex<Store>>, Value) {
    let item: work::TeamTask = serde_json::from_value(json!({
        "id":"checklist","title":"Checklist","description":"d".repeat(600),
        "depends_on":[],"acceptance_criteria":["Complete the checklist"],"review_required":true,
        "status":"pending","feedback":"x".repeat(3200),
        "history":[{"run":"old-run","run_uid":"old-uid","assignment_nonce":"old-nonce",
            "outcome":"changes_requested","finished_at":"2026-10-15T00:00:00Z",
            "output_sha256":"a".repeat(64),"feedback":"Original rejection evidence"}]
    }))
    .unwrap();
    let mut store = Store {
        team: json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTeam",
            "metadata":{"name":"eng","namespace":"work","uid":"team-uid","resourceVersion":"1",
                "annotations":{"kars.azure.com/owner-sub":"owner"}},
            "spec":{"charter":"c".repeat(1200),"paused":true,"envelope":{"tier":2,"authorityCeiling":2}}}),
        backlog: json!({"apiVersion":"v1","kind":"ConfigMap",
            "metadata":{"name":"kars-team-tasks-eng","namespace":"work","uid":"backlog-uid","resourceVersion":"1",
                "ownerReferences":[{"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTeam","name":"eng","uid":"team-uid","controller":true}]},
            "data":{"retained":"unchanged"}}),
        ..Default::default()
    };
    let mut item = item;
    if review {
        item.status = "awaiting_review".into();
        item.feedback = None;
        item.run = Some("run".into());
        item.run_uid = Some("run-uid".into());
        item.assignment_nonce = Some("nonce".into());
        let binding = work::Binding {
            backlog_uid: "backlog-uid".into(),
            item_id: item.id.clone(),
            request_digest: work::request_digest(&item).unwrap(),
        };
        let owner =
            super::work_owner(&serde_json::from_value(store.team.clone()).unwrap()).unwrap();
        let name = work::approval_name(&owner, &binding, &item).unwrap();
        let spec = json!({"taskRef":{"name":"run"},"action":{"kind":"checkpoint","summary":"Review result"},"ttl":"P7D"});
        store.approval = json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsApproval",
            "metadata":{"name":name,"namespace":"work","uid":"approval-uid","resourceVersion":"1",
                "ownerReferences":store.backlog["metadata"]["ownerReferences"],
                "annotations":{(work::ANNOTATION):serde_json::to_string(&binding).unwrap(),(work::TASK_UID):"run-uid",(work::RUN_NONCE):"nonce"}},
            "spec":spec,"status":{"phase":"Pending","boundTaskUid":"run-uid","boundRequest":spec.to_string(),
                "expiresAt":(chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339()}});
    }
    put_item(&mut store, &item);
    let store = Arc::new(Mutex::new(store));
    let shared = store.clone();
    let service = service_fn(move |request: Request<kube::client::Body>| {
        let shared = shared.clone();
        async move {
            let method = request.method().clone();
            let path = request.uri().path().to_owned();
            let bytes = to_bytes(Body::new(request.into_body()), 1_048_576)
                .await
                .unwrap();
            let body: Value = if bytes.is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(&bytes).unwrap()
            };
            let mut state = shared.lock().unwrap();
            state
                .calls
                .push((method.clone(), path.clone(), body.clone()));
            let mut code = 200;
            let response = match method {
                Method::GET if path.ends_with("/karsteams/eng") => {
                    state.team_reads += 1;
                    if state.resume_before_guard && state.team_reads > 1 {
                        state.team["spec"]["paused"] = json!(false);
                        state.team["metadata"]["resourceVersion"] = json!("2");
                    }
                    state.team.clone()
                }
                Method::GET if path.ends_with("/configmaps/kars-team-tasks-eng") => {
                    state.backlog.clone()
                }
                Method::GET if path.contains("/karsapprovals/") => state.approval.clone(),
                Method::PUT if path.ends_with("/configmaps/kars-team-tasks-eng") => {
                    if let Some(replacement) = state.before_put.take() {
                        state.backlog = replacement;
                        code = 409;
                    } else if body["metadata"]["uid"] != state.backlog["metadata"]["uid"]
                        || body["metadata"]["resourceVersion"]
                            != state.backlog["metadata"]["resourceVersion"]
                    {
                        code = 409;
                    }
                    if code == 409 {
                        json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"Conflict","code":409,"message":"concurrent writer"})
                    } else {
                        state.backlog = body.clone();
                        state.backlog["metadata"]["resourceVersion"] = json!("2");
                        state.backlog.clone()
                    }
                }
                Method::PATCH if path.contains("/karsapprovals/") => {
                    if body.is_array() {
                        let patch: json_patch::Patch = serde_json::from_value(body).unwrap();
                        json_patch::patch(&mut state.approval, &patch).unwrap();
                    } else {
                        assert_eq!(
                            body["metadata"]["resourceVersion"],
                            state.approval["metadata"]["resourceVersion"]
                        );
                        state.approval["spec"]["decision"] = body["spec"]["decision"].clone();
                    }
                    state.approval.clone()
                }
                _ => panic!("unexpected {method} {path}"),
            };
            Ok::<_, std::io::Error>(
                Response::builder()
                    .status(code)
                    .header("content-type", "application/json")
                    .body(Body::from(response.to_string()))
                    .unwrap(),
            )
        }
    });
    (
        AppState::for_test_client(kube::Client::new(service, "work"), "work"),
        store,
        json!(item),
    )
}

async fn request(state: AppState, method: Method, route: &str, body: Value) -> (StatusCode, Value) {
    let response = crate::routes::router(state)
        .layer(Extension(Principal {
            sub: "owner".into(),
            name: "Owner".into(),
            roles: vec!["operator".into()],
        }))
        .oneshot(
            Request::builder()
                .method(method)
                .uri(format!("/api/namespaces/work/{route}"))
                .header("content-type", "application/json")
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
async fn unsupported_plan_tool_allowlists_reject_http_patch_without_writes_or_run_mint() {
    for runtime in ["OpenClaw", "Hermes"] {
        for field in ["tool_allowlist", "toolAllowlist"] {
            for tools in [
                json!([]),
                json!(["file_read"]),
                json!(["file_write"]),
                json!(["*"]),
                json!(["file_*"]),
            ] {
                for location in ["", "/roles/0", "/roles/0/phases/0", "/synthesis"] {
                    let (state, store, _) = fixture(false);
                    store.lock().unwrap().team["spec"]["blueprint"] =
                        json!({"runtime":runtime, "toolPolicy":"administrator-policy"});
                    let team_before = store.lock().unwrap().team.clone();
                    let backlog_before = store.lock().unwrap().backlog.clone();
                    let plan = crate::kars::task::execution_plan::tests::filesystem_plan();
                    crate::kars::task::execution_plan::validate_retained_input_execution(
                        Some(&plan),
                        1,
                    )
                    .unwrap();
                    let mut proposed =
                        json!(crate::routes::tasks::ExecutionPlanDto::from_crd(&plan));
                    proposed.pointer_mut(location).unwrap()[field] = tools.clone();
                    let body = json!({"execution_plan":proposed});
                    let response = crate::routes::router(state)
                        .layer(Extension(Principal {
                            sub: "owner".into(),
                            name: "Owner".into(),
                            roles: vec!["operator".into()],
                        }))
                        .oneshot(
                            Request::builder()
                                .method(Method::PATCH)
                                .uri("/api/namespaces/work/teams/eng")
                                .header("content-type", "application/json")
                                .body(Body::from(body.to_string()))
                                .unwrap(),
                        )
                        .await
                        .unwrap();
                    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
                    let bytes = to_bytes(response.into_body(), 1_048_576).await.unwrap();
                    assert!(String::from_utf8_lossy(&bytes).contains("unknown field"));
                    let stored = store.lock().unwrap();
                    assert!(
                        stored.calls.is_empty(),
                        "reject before any Kubernetes read, mutation or run request"
                    );
                    assert_eq!(stored.team, team_before);
                    assert_eq!(stored.backlog, backlog_before);
                }
            }
        }
    }
}

const FEEDBACK: &str = "teams/eng/tasks/checklist/feedback";

#[test]
fn amendment_requires_an_explicit_feedback_field_and_rejects_unknown_fields() {
    let expected = json!(work::TeamTask::default());
    for body in [
        json!({"team_uid":"team-uid","expected":expected}),
        json!({"team_uid":"team-uid","expected":expected,"feedback":"short","status":"done"}),
    ] {
        assert!(serde_json::from_value::<super::AmendFeedbackRequest>(body).is_err());
    }
}

#[tokio::test]
async fn teams_gateway_denials_preflight_the_same_serialized_feedback_budget() {
    use crate::routes::teams_internal::{TeamsDecisionRequest, teams_decision};
    for verdict in ["deny", "request-changes"] {
        for oversized in [false, true] {
            let (state, store, _) = fixture(true);
            let state = state
                .with_teams_internal_secret(Some("test-only-secret".into()))
                .with_teams_entra_role_map(vec![(
                    "entra-owner".into(),
                    "owner".into(),
                    vec!["operator".into()],
                    "Owner".into(),
                )]);
            let mut headers = axum::http::HeaderMap::new();
            headers.insert(
                "x-teams-internal-secret",
                "test-only-secret".parse().unwrap(),
            );
            let name = store.lock().unwrap().approval["metadata"]["name"]
                .as_str()
                .unwrap()
                .to_owned();
            let reason = if oversized {
                "\"🦀\\\n".repeat(700)
            } else {
                "  Short feedback 🦀\n".into()
            };
            let result = teams_decision(
                axum::extract::State(state),
                headers,
                axum::Json(TeamsDecisionRequest {
                    approval_name: name,
                    approval_namespace: "work".into(),
                    verdict: verdict.into(),
                    reason: Some(reason.clone()),
                    resource_version: "1".into(),
                    bound_envelope_digest: None,
                    entra_subject: "entra-owner".into(),
                    entra_name: "Owner".into(),
                }),
            )
            .await;
            let store = store.lock().unwrap();
            if oversized {
                let error = match result {
                    Err(crate::error::AppError::BadRequest(message)) => message,
                    _ => panic!("oversized Teams gateway denial was not rejected"),
                };
                assert!(
                    error.contains("allowed") && error.contains("actual") && error.contains("4096"),
                    "{error}"
                );
                assert!(
                    store
                        .calls
                        .iter()
                        .all(|(method, _, _)| *method == Method::GET)
                );
                assert!(store.approval["spec"]["decision"].is_null());
            } else {
                assert!(result.is_ok());
                assert_eq!(store.approval["spec"]["decision"]["verdict"], "deny");
                assert_eq!(store.approval["spec"]["decision"]["reason"], reason);
            }
        }
    }
}

#[tokio::test]
async fn paused_pending_feedback_repair_preserves_identity_history_and_other_data() {
    for feedback in [json!("  Shorter feedback 🦀\n"), Value::Null, json!("")] {
        let (state, store, expected) = fixture(false);
        let (status, result) = request(
            state,
            Method::PATCH,
            FEEDBACK,
            json!({"team_uid":"team-uid","expected":expected,"feedback":feedback}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{result}");
        let mut wanted: work::TeamTask = serde_json::from_value(expected).unwrap();
        wanted.feedback = serde_json::from_value(feedback).unwrap();
        assert_eq!(
            serde_json::from_value::<work::TeamTask>(result).unwrap(),
            wanted
        );
        let store = store.lock().unwrap();
        assert_eq!(items(&store), vec![wanted]);
        assert_eq!(store.backlog["metadata"]["uid"], "backlog-uid");
        assert_eq!(store.backlog["data"]["retained"], "unchanged");
        assert_eq!(store.team["spec"]["paused"], true);
        assert_eq!(
            store
                .calls
                .iter()
                .filter(|(m, _, _)| *m != Method::GET)
                .count(),
            1
        );
        assert!(store.calls.iter().any(|(m, _, _)| *m == Method::PUT));
    }
}

#[tokio::test]
async fn feedback_repair_refuses_nonpaused_foreign_claimed_stale_and_oversized_intents() {
    for case in [
        "unpaused",
        "foreign",
        "team-uid",
        "stale",
        "history",
        "active",
        "oversize",
        "wrong-id",
        "resume-race",
    ] {
        let (state, store, mut expected) = fixture(false);
        let mut uid = "team-uid";
        let mut feedback = "Short".to_owned();
        {
            let mut store = store.lock().unwrap();
            match case {
                "unpaused" => store.team["spec"]["paused"] = json!(false),
                "foreign" => {
                    store.team["metadata"]["annotations"]["kars.azure.com/owner-sub"] =
                        json!("other")
                }
                "team-uid" => uid = "recreated",
                "stale" => expected["feedback"] = json!("older"),
                "history" => expected["history"][0]["feedback"] = json!("changed evidence"),
                "wrong-id" => expected["id"] = json!("other"),
                "oversize" => feedback = "\"\n🦀".repeat(1000),
                "active" => {
                    let mut item = items(&store).remove(0);
                    item.status = "active".into();
                    item.run = Some("run".into());
                    item.run_uid = Some("uid".into());
                    item.assignment_nonce = Some("nonce".into());
                    put_item(&mut store, &item);
                    expected = json!(item);
                }
                "resume-race" => store.resume_before_guard = true,
                _ => unreachable!(),
            }
        }
        let original = store.lock().unwrap().backlog.clone();
        let (status, body) = request(
            state,
            Method::PATCH,
            FEEDBACK,
            json!({"team_uid":uid,"expected":expected,"feedback":feedback}),
        )
        .await;
        assert!(status.is_client_error(), "{case}: {status} {body}");
        if case == "oversize" {
            assert!(body.to_string().contains("allowed: 4096"), "{body}");
        }
        let store = store.lock().unwrap();
        assert_eq!(store.backlog, original, "{case}");
        assert!(
            store
                .calls
                .iter()
                .all(|(method, _, _)| *method == Method::GET),
            "{case}"
        );
    }
}

#[tokio::test]
async fn concurrent_claim_and_store_replacement_cannot_be_overwritten() {
    for replace_store in [false, true] {
        let (state, store, expected) = fixture(false);
        let replacement = {
            let mut store = store.lock().unwrap();
            let original = store.backlog.clone();
            let mut item = items(&store).remove(0);
            item.status = "active".into();
            item.run = Some("new-run".into());
            item.run_uid = Some("new-uid".into());
            item.assignment_nonce = Some("new-nonce".into());
            put_item(&mut store, &item);
            store.backlog["metadata"]["resourceVersion"] = json!("2");
            if replace_store {
                store.backlog["metadata"]["uid"] = json!("replacement");
            }
            let replacement = store.backlog.clone();
            store.backlog = original;
            store.before_put = Some(replacement.clone());
            replacement
        };
        let (status, body) = request(
            state,
            Method::PATCH,
            FEEDBACK,
            json!({"team_uid":"team-uid","expected":expected,"feedback":"Short"}),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        let store = store.lock().unwrap();
        assert_eq!(store.backlog, replacement);
        assert_eq!(
            store
                .calls
                .iter()
                .filter(|(m, _, _)| *m == Method::PUT)
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn create_rejects_excess_criteria_instead_of_silently_dropping_them() {
    let (state, store, _) = fixture(false);
    let (status, body) = request(
        state,
        Method::POST,
        "teams/eng/tasks",
        json!({"id":"new","title":"New work","acceptance_criteria":vec!["Check"; 21]}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.to_string().contains("21") && body.to_string().contains("20"));
    assert!(
        store
            .lock()
            .unwrap()
            .calls
            .iter()
            .all(|(method, _, _)| *method == Method::GET)
    );
}

#[tokio::test]
async fn create_and_both_review_routes_reject_oversize_before_writes() {
    for route in ["create", "team-review", "inbox"] {
        for oversized in [false, true] {
            let (state, store, _) = fixture(true);
            let text = if oversized {
                "\"\n🦀".repeat(1000)
            } else {
                "  Fix checklist 🦀\n".into()
            };
            let (path, body) = match route {
                "create" => (
                    "teams/eng/tasks".to_owned(),
                    json!({"id":"new","title":"New work","description":text}),
                ),
                "team-review" => (
                    "teams/eng/tasks/checklist/review".into(),
                    json!({"decision":"request_changes","assignment_nonce":"nonce","run_uid":"run-uid","feedback":text}),
                ),
                _ => (
                    format!(
                        "approvals/{}/decision",
                        store.lock().unwrap().approval["metadata"]["name"]
                            .as_str()
                            .unwrap()
                    ),
                    json!({"verdict":"deny","resource_version":"1","reason":text}),
                ),
            };
            let before = store.lock().unwrap().backlog.clone();
            let (status, body) = request(state, Method::POST, &path, body).await;
            let store = store.lock().unwrap();
            if oversized {
                assert_eq!(status, StatusCode::BAD_REQUEST, "{route}: {body}");
                assert!(body.to_string().contains("allowed: 4096"), "{body}");
                assert!(store.calls.iter().all(|(m, _, _)| *m == Method::GET));
                assert_eq!(store.backlog, before);
                assert!(store.approval["spec"]["decision"].is_null());
            } else {
                assert_eq!(status, StatusCode::OK, "{route}: {body}");
                if route != "create" {
                    assert_eq!(store.approval["spec"]["decision"]["reason"], text);
                }
            }
        }
    }
}

#[tokio::test]
async fn inbox_denial_fails_closed_for_stale_or_missing_work_binding() {
    for missing in [false, true] {
        let (state, store, _) = fixture(true);
        let name = {
            let mut store = store.lock().unwrap();
            if missing {
                store.approval["metadata"]["annotations"]
                    .as_object_mut()
                    .unwrap()
                    .remove(work::ANNOTATION);
                store.approval["metadata"]["labels"] =
                    json!({"kars.azure.com/milestone":"checklist"});
            } else {
                store.approval["metadata"]["ownerReferences"][0]["uid"] = json!("foreign-team");
            }
            store.approval["metadata"]["name"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        let (status, body) = request(
            state,
            Method::POST,
            &format!("approvals/{name}/decision"),
            json!({"verdict":"deny","resource_version":"1","reason":"Short feedback"}),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(
            store
                .lock()
                .unwrap()
                .calls
                .iter()
                .all(|(m, _, _)| *m == Method::GET)
        );
    }
}
