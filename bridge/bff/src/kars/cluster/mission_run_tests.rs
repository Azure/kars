// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{Cluster, MeshRunOutcome, MissionRunError};
use crate::{auth::Principal, kars::task::KarsTask, state::AppState};
use axum::{
    Extension, Json, Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use k8s_openapi::api::core::v1::ConfigMap;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tower::ServiceExt;

const TASK: &str = "/apis/kars.azure.com/v1alpha1/namespaces/kars-system/karstasks/briefing";
const MAPS: &str = "/api/v1/namespaces/kars-system/configmaps";
#[path = "mission_revision_read_tests.rs"]
mod revision_reads;

const REQUESTED: &str = "kars.azure.com/run-requested";
const COMPLETED: &str = "kars.azure.com/run-completed";

fn task() -> Value {
    json!({"apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsTask",
        "metadata":{"name":"briefing", "namespace":"kars-system", "uid":"task-uid", "resourceVersion":"1",
            "annotations":{"kars.azure.com/owner-sub":"owner", "kars.azure.com/run-requested":"run-1", "kars.azure.com/run-completed":"run-1"}},
        "spec":{"objective":"Write a briefing", "execution":{"launch":true}, "envelope":{"tier":1,"authorityCeiling":1}},
        "status":{"sandboxRef":{"name":"briefing"}}})
}

fn output(task: &Value) -> Value {
    let uid = &task["metadata"]["uid"];
    let nonce = &task["metadata"]["annotations"][COMPLETED];
    json!({"apiVersion":"v1", "kind":"ConfigMap",
        "metadata":{"name":"kars-mission-output-briefing","namespace":"kars-system",
            "annotations":{"kars.azure.com/mission-task-uid":uid,"kars.azure.com/mission-run-nonce":nonce,"kars.azure.com/mission-principal-name":"briefing"},
            "ownerReferences":[{"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTask","name":"briefing","uid":uid,"controller":true}]},
        "data":{"taskName":"briefing","taskUid":uid,"assignmentNonce":nonce,"status":"ok","output":"Useful briefing","model":"recorded-model","totalTokens":"123","finishedAt":"2026-10-03T00:00:00Z"}})
}

fn review() -> Value {
    json!({"apiVersion":"v1","kind":"ConfigMap",
        "metadata":{"namespace":"kars-system","name":"kars-mission-review-briefing","uid":"review-uid","resourceVersion":"1"},
        "data":{"taskUid":"task-uid","assignmentNonce":"run-1","status":"approved","revision":"0","history":"[]","redrivePending":"false"}})
}

struct Store {
    task: Value,
    output: Value,
    review: Option<Value>,
    requests: Vec<(Method, String)>,
    patches: usize,
    lose_patch_response: bool,
    deny_review_write: bool,
    rotate_on_output: Option<Value>,
    slow_task_get: bool,
    trace: Option<Value>,
    rotate_on_trace: Option<Value>,
    rotate_on_second_list: Option<Value>,
    task_lists: usize,
}

fn status(code: StatusCode) -> Response {
    (code, Json(json!({"apiVersion":"v1","kind":"Status","status":"Failure",
        "reason":match code { StatusCode::NOT_FOUND => "NotFound", StatusCode::CONFLICT => "Conflict", _ => "Forbidden" },"message":"test API response","code":code.as_u16()}))).into_response()
}

async fn handle(State(shared): State<Arc<Mutex<Store>>>, request: Request) -> Response {
    let path = request.uri().path().to_owned();
    let method = request.method().clone();
    let body = to_bytes(request.into_body(), 1024 * 1024).await.unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let slow = shared.lock().unwrap().slow_task_get;
    if slow && path == TASK && method == Method::GET {
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let mut store = shared.lock().unwrap();
    store.requests.push((method.clone(), path.clone()));
    if path == TASK {
        if method == Method::GET {
            return if store.task.is_null() {
                status(StatusCode::NOT_FOUND)
            } else {
                Json(store.task.clone()).into_response()
            };
        }
        if method == Method::PATCH {
            if body["metadata"]["uid"] != store.task["metadata"]["uid"]
                || body["metadata"]["resourceVersion"] != store.task["metadata"]["resourceVersion"]
            {
                return status(StatusCode::CONFLICT);
            }
            let version = store.task["metadata"]["resourceVersion"]
                .as_str()
                .unwrap()
                .parse::<u64>()
                .unwrap()
                + 1;
            for (key, value) in body["metadata"]["annotations"].as_object().unwrap() {
                store.task["metadata"]["annotations"][key] = value.clone();
            }
            store.task["metadata"]["resourceVersion"] = json!(version.to_string());
            store.patches += 1;
            if store.lose_patch_response {
                // The API committed, but its success body was truncated in transit.
                return (StatusCode::OK, "{").into_response();
            }
            return Json(store.task.clone()).into_response();
        }
    }
    if path == TASK.trim_end_matches("/briefing") && method == Method::GET {
        store.task_lists += 1;
        if store.task_lists == 2
            && let Some(next) = store.rotate_on_second_list.take()
        {
            store.task = next;
        }
        let items = if store.task.is_null() {
            vec![]
        } else {
            vec![store.task.clone()]
        };
        return Json(json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTaskList","metadata":{},"items":items})).into_response();
    }
    if path == MAPS && method == Method::GET {
        let mut listed = store.output.clone();
        listed["metadata"]["labels"]["kars.azure.com/mission-output"] = json!("briefing");
        return Json(
            json!({"apiVersion":"v1","kind":"ConfigMapList","metadata":{},"items":[listed]}),
        )
        .into_response();
    }
    if path == format!("{MAPS}/kars-mission-trace-briefing") && method == Method::GET {
        if let Some(next) = store.rotate_on_trace.take() {
            store.task = next;
        }
        return store
            .trace
            .clone()
            .map(|value| Json(value).into_response())
            .unwrap_or_else(|| status(StatusCode::NOT_FOUND));
    }
    if path == format!("{MAPS}/kars-mission-output-briefing") && method == Method::GET {
        if let Some(next) = store.rotate_on_output.take() {
            store.task = next;
            store.output = output(&store.task);
        }
        return Json(store.output.clone()).into_response();
    }
    if path == format!("{MAPS}/kars-mission-review-briefing") && method == Method::GET {
        return store
            .review
            .clone()
            .map(|value| Json(value).into_response())
            .unwrap_or_else(|| status(StatusCode::NOT_FOUND));
    }
    if (path == MAPS && method == Method::POST)
        || (path == format!("{MAPS}/kars-mission-review-briefing") && method == Method::PUT)
    {
        if store.deny_review_write {
            return status(StatusCode::FORBIDDEN);
        }
        if method == Method::POST && store.review.is_some() {
            return status(StatusCode::CONFLICT);
        }
        if method == Method::PUT
            && store.review.as_ref().is_none_or(|prior| {
                body["metadata"]["uid"] != prior["metadata"]["uid"]
                    || body["metadata"]["resourceVersion"] != prior["metadata"]["resourceVersion"]
            })
        {
            return status(StatusCode::CONFLICT);
        }
        let mut next = body;
        let version = store
            .review
            .as_ref()
            .and_then(|cm| cm["metadata"]["resourceVersion"].as_str())
            .unwrap_or("0")
            .parse::<u64>()
            .unwrap()
            + 1;
        next["metadata"]["resourceVersion"] = json!(version.to_string());
        next["metadata"]["uid"] = json!("review-uid");
        store.review = Some(next.clone());
        return Json(next).into_response();
    }
    if path == "/api/v1/namespaces/kars-briefing/pods" && method == Method::GET {
        return Json(json!({"apiVersion":"v1","kind":"PodList","metadata":{},"items":[{"metadata":{"name":"agent-pod"},"status":{"phase":"Running"}}]})).into_response();
    }
    status(StatusCode::NOT_FOUND)
}

#[tokio::test]
async fn review_rejects_non_deliverables_without_any_write() {
    for (status, text, evidence) in [
        (Some("failed"), "Partial report", None),
        (Some("rejected"), "Denied", None),
        (Some("error"), "Failed", None),
        (None, "Useful report", None),
        (Some("ok"), "  ", None),
        (Some("ok"), "Useful report", Some("{}")),
    ] {
        let fixture = Fixture::new().await;
        let before = review();
        {
            let mut store = fixture.store.lock().unwrap();
            store.review = Some(before.clone());
            let data = store.output["data"].as_object_mut().unwrap();
            data.remove("status");
            if let Some(status) = status {
                data.insert("status".into(), json!(status));
            }
            data.insert("output".into(), json!(text));
            if let Some(evidence) = evidence {
                data.insert("evidence.json".into(), json!(evidence));
            }
        }
        let (code, _) = fixture
            .route(
                Method::POST,
                "review",
                json!({"decision":"approve","assignment_nonce":"run-1"}),
            )
            .await;
        assert_eq!(code, StatusCode::CONFLICT, "{status:?}/{text}/{evidence:?}");
        let store = fixture.store.lock().unwrap();
        assert_eq!(store.patches, 0);
        assert_eq!(store.review.as_ref(), Some(&before));
        assert!(
            store
                .requests
                .iter()
                .all(|(method, _)| method == Method::GET)
        );
    }
}

#[tokio::test]
async fn review_allows_feedback_on_failed_and_empty_results() {
    for (status, text) in [("failed", "Partial report"), ("ok", "")] {
        let fixture = Fixture::new().await;
        {
            let mut store = fixture.store.lock().unwrap();
            store.output["data"]["status"] = json!(status);
            store.output["data"]["output"] = json!(text);
        }
        let (code, _) = fixture.route(Method::POST, "review", json!({"decision":"request_changes","comment":"Produce the complete briefing","assignment_nonce":"run-1"})).await;
        assert_eq!(code, StatusCode::OK);
        let store = fixture.store.lock().unwrap();
        assert_eq!(store.patches, 1);
        assert_ne!(
            store.task["metadata"]["annotations"][REQUESTED],
            json!("run-1")
        );
    }
}

#[tokio::test]
async fn review_accepts_explicit_legacy_success() {
    let fixture = Fixture::new().await;
    let (code, _) = fixture
        .route(
            Method::POST,
            "review",
            json!({"decision":"approve","assignment_nonce":"run-1"}),
        )
        .await;
    assert_eq!(code, StatusCode::OK);
    let store = fixture.store.lock().unwrap();
    assert_eq!(store.patches, 0);
    assert_eq!(store.review.as_ref().unwrap()["data"]["status"], "approved");
}

struct Fixture {
    store: Arc<Mutex<Store>>,
    client: kube::Client,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Fixture {
    async fn new() -> Self {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let store = Arc::new(Mutex::new(Store {
            output: output(&task()),
            task: task(),
            review: None,
            requests: Vec::new(),
            patches: 0,
            lose_patch_response: false,
            deny_review_write: false,
            rotate_on_output: None,
            slow_task_get: false,
            trace: None,
            rotate_on_trace: None,
            rotate_on_second_list: None,
            task_lists: 0,
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = kube::Client::try_from(kube::Config::new(
            format!("http://{}", listener.local_addr().unwrap())
                .parse()
                .unwrap(),
        ))
        .unwrap();
        let app = Router::new().fallback(handle).with_state(store.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            store,
            client,
            server,
        }
    }
    fn cluster(&self) -> Cluster {
        Cluster::for_test_client(self.client.clone())
    }
    fn task(&self) -> KarsTask {
        serde_json::from_value(self.store.lock().unwrap().task.clone()).unwrap()
    }
    async fn read_route(&self, uri: &str) -> (StatusCode, String) {
        let app = Router::new()
            .route(
                "/api/namespaces/{ns}/tasks",
                get(crate::routes::tasks::list_tasks),
            )
            .route(
                "/api/namespaces/{ns}/tasks/{name}",
                get(crate::routes::tasks::get_task),
            )
            .route(
                "/api/namespaces/{ns}/tasks/{name}/stream",
                get(crate::routes::telemetry::stream_mission),
            )
            .layer(Extension(Principal {
                sub: "owner".into(),
                name: "Owner".into(),
                roles: vec![],
            }))
            .with_state(AppState::for_test_client(
                self.client.clone(),
                "kars-system",
            ));
        let response = app
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let code = response.status();
        let bytes = tokio::time::timeout(
            Duration::from_secs(5),
            to_bytes(response.into_body(), 1024 * 1024),
        )
        .await
        .expect("read must terminate without polling an unrelated revision")
        .unwrap();
        (code, String::from_utf8(bytes.to_vec()).unwrap())
    }
    async fn route(&self, method: Method, suffix: &str, body: Value) -> (StatusCode, Value) {
        let app = Router::new()
            .route(
                "/api/namespaces/{ns}/tasks/{name}/run",
                post(crate::routes::run::run_mission),
            )
            .route(
                "/api/namespaces/{ns}/tasks/{name}/review",
                get(crate::routes::review::get_review).post(crate::routes::review::post_review),
            )
            .layer(Extension(Principal {
                sub: "owner".into(),
                name: "Verified reviewer".into(),
                roles: vec!["user".into()],
            }))
            .with_state(AppState::for_test_client(
                self.client.clone(),
                "kars-system",
            ));
        let response = app
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(format!(
                        "/api/namespaces/kars-system/tasks/briefing/{suffix}"
                    ))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        (
            response.status(),
            serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
                .unwrap(),
        )
    }
}

#[tokio::test]
async fn concurrent_initial_requests_have_one_cas_winner() {
    let fixture = Fixture::new().await;
    let snapshot = fixture.task();
    let cluster = fixture.cluster();
    let (a, b) = tokio::join!(
        cluster.request_mesh_run(&snapshot),
        cluster.request_mesh_run(&snapshot)
    );
    assert_ne!(a.is_ok(), b.is_ok());
    let error = a.err().or_else(|| b.err()).unwrap();
    assert_eq!(
        crate::error::AppError::from(error).into_response().status(),
        StatusCode::CONFLICT
    );
    assert_eq!(fixture.store.lock().unwrap().patches, 1);
}

#[tokio::test]
async fn pending_request_is_reused_without_write() {
    let fixture = Fixture::new().await;
    fixture.store.lock().unwrap().task["metadata"]["annotations"][REQUESTED] = json!("pending-run");
    assert_eq!(
        fixture
            .cluster()
            .request_mesh_run(&fixture.task())
            .await
            .unwrap(),
        "pending-run"
    );
    assert!(fixture.store.lock().unwrap().requests.is_empty());
}

#[tokio::test]
async fn invalid_launch_identity_never_mutates() {
    let fixture = Fixture::new().await;
    for (pointer, value) in [
        ("/metadata/uid", json!("")),
        ("/metadata/resourceVersion", Value::Null),
        ("/metadata/name", json!("")),
        ("/metadata/namespace", Value::Null),
        ("/spec/execution", Value::Null),
        ("/spec/execution/launch", json!(false)),
    ] {
        let mut value_task = task();
        *value_task.pointer_mut(pointer).unwrap() = value;
        let snapshot = serde_json::from_value(value_task).unwrap();
        assert!(matches!(
            fixture.cluster().request_mesh_run(&snapshot).await,
            Err(MissionRunError::Conflict(_))
        ));
    }
    let mut deleting = task();
    deleting["metadata"]["deletionTimestamp"] = json!("2026-10-03T00:00:00Z");
    assert!(
        fixture
            .cluster()
            .request_mesh_run(&serde_json::from_value(deleting).unwrap())
            .await
            .is_err()
    );
    assert!(fixture.store.lock().unwrap().requests.is_empty());
}

#[tokio::test]
async fn stale_uid_and_revision_cannot_request_runs() {
    let fixture = Fixture::new().await;
    let snapshot = fixture.task();
    for (field, value) in [("uid", "recreated-uid"), ("resourceVersion", "2")] {
        fixture.store.lock().unwrap().task = task();
        fixture.store.lock().unwrap().task["metadata"][field] = json!(value);
        assert!(fixture.cluster().request_mesh_run(&snapshot).await.is_err());
    }
    assert_eq!(fixture.store.lock().unwrap().patches, 0);
}

#[tokio::test]
async fn revision_binds_distinct_nonce_exact_objective_and_digest() {
    let fixture = Fixture::new().await;
    let before = fixture.task();
    let objective = "Revise the café briefing — keep the safety checklist.";
    let nonce = fixture
        .cluster()
        .redrive_with_revision(&before, "run-1", objective)
        .await
        .unwrap();
    assert!(nonce.starts_with("rev-"));
    assert_ne!(nonce, "run-1");
    let after = fixture.task();
    let annotations = after.metadata.annotations.unwrap();
    assert_eq!(annotations[REQUESTED], nonce);
    assert_eq!(annotations["kars.azure.com/run-objective-nonce"], nonce);
    assert_eq!(
        STANDARD
            .decode(&annotations["kars.azure.com/run-objective-b64"])
            .unwrap(),
        objective.as_bytes()
    );
    assert_eq!(
        annotations["kars.azure.com/run-objective-digest"],
        format!(
            "sha256:{}",
            crate::providers::signing::sha256_hex(objective.as_bytes())
        )
    );
    assert_eq!(after.spec.objective, before.spec.objective);
    assert!(
        fixture
            .cluster()
            .redrive_with_revision(&fixture.task(), "run-1", "Overwrite pending")
            .await
            .is_err()
    );
    assert!(
        fixture
            .cluster()
            .redrive_with_revision(&before, "run-1", "Stale writer")
            .await
            .is_err()
    );
    assert_eq!(fixture.store.lock().unwrap().patches, 1);
}

#[tokio::test]
async fn invalid_revision_does_not_patch() {
    let fixture = Fixture::new().await;
    for (nonce, objective) in [("", "revision"), ("old-run", "revision"), ("run-1", " \n")] {
        assert!(
            fixture
                .cluster()
                .redrive_with_revision(&fixture.task(), nonce, objective)
                .await
                .is_err()
        );
    }
    assert!(fixture.store.lock().unwrap().requests.is_empty());
}

#[tokio::test]
async fn lost_committed_request_response_is_not_model_fallback_or_second_request() {
    let fixture = Fixture::new().await;
    fixture.store.lock().unwrap().lose_patch_response = true;
    let (status, body) = fixture.route(Method::POST, "run", json!({})).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_eq!(body["error"]["message"], "upstream dependency failed");
    let fresh = fixture
        .cluster()
        .tasks("kars-system")
        .get("briefing")
        .await
        .unwrap();
    let nonce = fixture.cluster().request_mesh_run(&fresh).await.unwrap();
    let store = fixture.store.lock().unwrap();
    assert_eq!(store.patches, 1);
    assert_eq!(store.task["metadata"]["annotations"][REQUESTED], nonce);
    assert!(
        !store
            .requests
            .iter()
            .any(|(_, path)| path.contains("proxy") || path.contains("trace"))
    );
    assert!(store.review.is_none());
}

#[tokio::test]
async fn await_returns_only_exact_committed_output_and_bounds_hung_reads() {
    let fixture = Fixture::new().await;
    assert!(matches!(
        fixture
            .cluster()
            .await_mesh_run(&fixture.task(), "run-1", Duration::from_secs(1))
            .await,
        MeshRunOutcome::Completed(_)
    ));
    fixture.store.lock().unwrap().slow_task_get = true;
    let result = tokio::time::timeout(
        Duration::from_millis(200),
        fixture
            .cluster()
            .await_mesh_run(&fixture.task(), "run-1", Duration::from_millis(20)),
    )
    .await
    .unwrap();
    assert!(matches!(result, MeshRunOutcome::NeverProcessed));
}

#[tokio::test]
async fn await_rejects_superseded_or_recreated_output_during_read() {
    for recreated in [false, true] {
        let fixture = Fixture::new().await;
        let expected = fixture.task();
        let mut next = task();
        if recreated {
            next["metadata"]["uid"] = json!("new-task-uid");
        } else {
            next["metadata"]["annotations"][REQUESTED] = json!("run-2");
            next["metadata"]["annotations"][COMPLETED] = json!("run-2");
        }
        fixture.store.lock().unwrap().rotate_on_output = Some(next);
        assert!(matches!(
            fixture
                .cluster()
                .await_mesh_run(&expected, "run-1", Duration::from_secs(1))
                .await,
            MeshRunOutcome::InProgress
        ));
    }
}

#[tokio::test]
async fn await_missing_ack_never_uses_trace_liveness() {
    let fixture = Fixture::new().await;
    fixture.store.lock().unwrap().task["metadata"]["annotations"]
        .as_object_mut()
        .unwrap()
        .remove(COMPLETED);
    let result = fixture
        .cluster()
        .await_mesh_run(&fixture.task(), "run-1", Duration::from_millis(20))
        .await;
    assert!(matches!(result, MeshRunOutcome::NeverProcessed));
    assert!(
        fixture
            .store
            .lock()
            .unwrap()
            .requests
            .iter()
            .all(|(method, path)| *method == Method::GET && path == TASK)
    );
}

#[tokio::test]
async fn concurrent_review_writers_cannot_overwrite_or_adopt_history() {
    let fixture = Fixture::new().await;
    fixture.store.lock().unwrap().review = Some(review());
    let cluster = fixture.cluster();
    let snapshot = cluster
        .review_snapshot("kars-system", "briefing")
        .await
        .unwrap()
        .unwrap();
    let data = BTreeMap::from([
        ("taskUid".into(), "task-uid".into()),
        ("history".into(), "[\"first\"]".into()),
    ]);
    cluster
        .write_review(&fixture.task(), Some(&snapshot), data.clone())
        .await
        .unwrap();
    assert!(
        cluster
            .write_review(&fixture.task(), Some(&snapshot), data)
            .await
            .is_err()
    );
    assert_eq!(
        fixture.store.lock().unwrap().review.as_ref().unwrap()["data"]["history"],
        "[\"first\"]"
    );
}

#[tokio::test]
async fn concurrent_review_creates_cannot_adopt_a_record() {
    let fixture = Fixture::new().await;
    let data = BTreeMap::from([("taskUid".into(), "task-uid".into())]);
    fixture
        .cluster()
        .write_review(&fixture.task(), None, data.clone())
        .await
        .unwrap();
    assert!(
        fixture
            .cluster()
            .write_review(&fixture.task(), None, data)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn stale_unbound_or_deleting_review_snapshots_fail_closed() {
    let fixture = Fixture::new().await;
    for (field, value) in [
        ("uid", Value::Null),
        ("resourceVersion", json!("")),
        ("name", json!("other")),
        ("namespace", json!("other")),
        ("deletionTimestamp", json!("2026-10-03T00:00:00Z")),
    ] {
        let mut prior = review();
        prior["metadata"][field] = value;
        fixture.store.lock().unwrap().review = Some(prior);
        assert!(
            fixture
                .cluster()
                .review_snapshot("kars-system", "briefing")
                .await
                .is_err()
        );
    }
    let mut prior = review();
    prior["data"]["taskUid"] = json!("old-uid");
    let prior: ConfigMap = serde_json::from_value(prior).unwrap();
    assert!(
        fixture
            .cluster()
            .write_review(
                &fixture.task(),
                Some(&prior),
                BTreeMap::from([("taskUid".into(), "task-uid".into())])
            )
            .await
            .is_err()
    );
    assert!(
        fixture
            .store
            .lock()
            .unwrap()
            .requests
            .iter()
            .all(|(method, _)| *method == Method::GET)
    );
}

#[tokio::test]
async fn review_get_is_read_only_and_does_not_approve_a_later_run_or_recreated_task() {
    let fixture = Fixture::new().await;
    fixture.store.lock().unwrap().review = Some(review());
    let (status, body) = fixture.route(Method::GET, "review", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "approved");
    for recreated in [false, true] {
        let mut next = task();
        if recreated {
            next["metadata"]["uid"] = json!("recreated-uid");
        } else {
            next["metadata"]["annotations"][REQUESTED] = json!("run-2");
            next["metadata"]["annotations"][COMPLETED] = json!("run-2");
        }
        {
            let mut store = fixture.store.lock().unwrap();
            store.task = next;
            store.output = output(&store.task);
        }
        let (status, body) = fixture.route(Method::GET, "review", Value::Null).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], "none");
    }
    assert!(
        fixture
            .store
            .lock()
            .unwrap()
            .requests
            .iter()
            .all(|(method, _)| *method == Method::GET)
    );
}

#[tokio::test]
async fn feedback_records_distinct_revision_and_only_exact_completion_clears_pending() {
    let fixture = Fixture::new().await;
    let (status, body) = fixture.route(Method::POST, "review", json!({"decision":"request_changes","comment":"Add privacy risks","assignment_nonce":"run-1"})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["assignment_nonce"], "run-1");
    assert_eq!(body["redrive_pending"], true);
    assert_eq!(body["history"][0]["reviewer"], "Verified reviewer");
    assert_eq!(body["history"][0]["attested"], true);
    let nonce = body["requested_revision_nonce"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(nonce, "run-1");
    for completed in ["unrelated-run", nonce.as_str()] {
        {
            let mut store = fixture.store.lock().unwrap();
            store.task["metadata"]["annotations"][REQUESTED] = json!(completed);
            store.task["metadata"]["annotations"][COMPLETED] = json!(completed);
            store.output = output(&store.task);
        }
        let (status, body) = fixture.route(Method::GET, "review", Value::Null).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["redrive_pending"], completed != nonce);
        assert_eq!(
            body["status"],
            if completed == nonce {
                "none"
            } else {
                "changes_requested"
            }
        );
    }
}

#[tokio::test]
async fn review_rejects_stale_input_and_malformed_or_foreign_journal_before_redrive() {
    let fixture = Fixture::new().await;
    let request =
        json!({"decision":"request_changes","comment":"Add risks","assignment_nonce":"old-run"});
    assert_eq!(
        fixture.route(Method::POST, "review", request).await.0,
        StatusCode::CONFLICT
    );
    for (field, value, expected) in [
        ("history", "{", StatusCode::BAD_GATEWAY),
        ("revision", "invalid", StatusCode::BAD_GATEWAY),
        ("revision", "-1", StatusCode::BAD_GATEWAY),
        ("revision", "9223372036854775807", StatusCode::CONFLICT),
        ("taskUid", "other-uid", StatusCode::CONFLICT),
    ] {
        let mut prior = review();
        prior["data"][field] = json!(value);
        fixture.store.lock().unwrap().review = Some(prior);
        let request =
            json!({"decision":"request_changes","comment":"Add risks","assignment_nonce":"run-1"});
        assert_eq!(
            fixture.route(Method::POST, "review", request).await.0,
            expected
        );
    }
    for (field, value) in [
        ("uid", ""),
        ("resourceVersion", ""),
        ("name", "another-review"),
        ("deletionTimestamp", "2026-10-03T00:00:00Z"),
    ] {
        let mut prior = review();
        prior["metadata"][field] = json!(value);
        fixture.store.lock().unwrap().review = Some(prior);
        let request =
            json!({"decision":"request_changes","comment":"Add risks","assignment_nonce":"run-1"});
        assert_eq!(
            fixture.route(Method::POST, "review", request).await.0,
            StatusCode::CONFLICT
        );
    }
    assert_eq!(fixture.store.lock().unwrap().patches, 0);
}

#[tokio::test]
async fn failed_journal_write_preserves_revision_and_retry_cannot_redrive_again() {
    let fixture = Fixture::new().await;
    fixture.store.lock().unwrap().deny_review_write = true;
    let request = json!({"decision":"request_changes","comment":"Add privacy risks","assignment_nonce":"run-1"});
    let (status, body) = fixture.route(Method::POST, "review", request.clone()).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    let nonce = fixture.task().metadata.annotations.unwrap()[REQUESTED].clone();
    assert!(nonce.starts_with("rev-"));
    assert_eq!(
        fixture.route(Method::POST, "review", request).await.0,
        StatusCode::CONFLICT
    );
    let store = fixture.store.lock().unwrap();
    assert_eq!(store.patches, 1);
    assert_eq!(store.task["metadata"]["annotations"][REQUESTED], nonce);
    assert!(store.review.is_none());
}
