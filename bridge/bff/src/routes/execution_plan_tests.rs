// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use axum::{
    Extension,
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

use crate::{auth::Principal, state::AppState};

const API: &str = "/apis/kars.azure.com/v1alpha1/namespaces/work";
const TASK: &str = "/apis/kars.azure.com/v1alpha1/namespaces/work/karstasks/task";
const TEAM: &str = "/apis/kars.azure.com/v1alpha1/namespaces/work/karsteams/team";
const GRANT: &str = "/apis/kars.azure.com/v1alpha1/namespaces/work/karscredentialgrants/workspace";
const MARKER: &str = "kars.azure.com/mission-decomposition";

#[derive(Default)]
struct Store {
    objects: BTreeMap<String, Value>,
    calls: Vec<(Method, String, Value)>,
    before_patch: Option<Value>,
    prune_create: bool,
    prune_patch: bool,
}

fn task() -> Value {
    json!({
        "apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsTask",
        "metadata":{"name":"task", "namespace":"work", "uid":"task-uid", "resourceVersion":"10",
            "annotations":{"kars.azure.com/owner-sub":"owner"}},
        "spec":{"objective":"Write a useful volunteer handbook.", "envelope":{"tier":1,"authorityCeiling":1},
            "execution":{"launch":false}}
    })
}

fn team() -> Value {
    json!({
        "apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsTeam",
        "metadata":{"name":"team", "namespace":"work", "uid":"team-uid", "resourceVersion":"10",
            "annotations":{"kars.azure.com/owner-sub":"owner"}},
        "spec":{"charter":"Write useful volunteer handbooks.", "envelope":{"tier":1,"authorityCeiling":1},
            "paused":true, "roster":[{"name":"writer"}]}
    })
}

fn plan() -> Value {
    json!(crate::kars::task::execution_plan::tests::plan())
}

fn reviewed(mut object: Value) -> Value {
    object["spec"]["blueprint"] = json!({"executionPlan":plan()});
    object["metadata"]["annotations"][MARKER] = "execution-plan/v1".into();
    object
}

fn blocked_tasks() -> Vec<Value> {
    let mut missing = task();
    missing["metadata"]["annotations"][MARKER] = "execution-plan/v1".into();
    let mut unsupported = task();
    unsupported["metadata"]["annotations"][MARKER] = "execution-plan/v2".into();
    vec![reviewed(task()), missing, unsupported]
}

fn blocked_teams() -> Vec<Value> {
    let mut role = team();
    role["spec"]["roster"][0]["blueprint"] = json!({"executionPlan":plan()});
    let mut missing = team();
    missing["metadata"]["annotations"][MARKER] = "execution-plan/v1".into();
    let mut unsupported = team();
    unsupported["metadata"]["annotations"][MARKER] = "execution-plan/v2".into();
    vec![reviewed(team()), role, missing, unsupported]
}

fn prune(object: &mut Value) {
    if let Some(blueprint) = object["spec"]["blueprint"].as_object_mut() {
        blueprint.remove("executionPlan");
    }
}

fn fixture(object: Value) -> (AppState, Arc<Mutex<Store>>) {
    let resource = if object["kind"] == "KarsTask" {
        TASK
    } else {
        TEAM
    };
    let store = Arc::new(Mutex::new(Store {
        objects: BTreeMap::from([
            (resource.into(), object),
            (
                GRANT.into(),
                json!({
                    "apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsCredentialGrant",
                    "metadata":{"name":"workspace","namespace":"work","uid":"grant-uid","generation":1},
                    "spec":{"enabled":true,"workspaceUid":"workspace-uid","integrationStores":[]},
                    "status":{"phase":"Ready","observedGeneration":1,"sources":[]}
                }),
            ),
            (
                "/api/v1/namespaces/work".into(),
                json!({
                    "apiVersion":"v1","kind":"Namespace",
                    "metadata":{"name":"work","uid":"workspace-uid"}
                }),
            ),
        ]),
        ..Default::default()
    }));
    let state = store.clone();
    let service = service_fn(move |request: Request<kube::client::Body>| {
        let state = state.clone();
        async move {
            let method = request.method().clone();
            let path = request.uri().path().to_owned();
            let bytes = to_bytes(Body::new(request.into_body()), 1_048_576)
                .await
                .unwrap();
            let body = if bytes.is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(&bytes).unwrap()
            };
            let mut store = state.lock().unwrap();
            store
                .calls
                .push((method.clone(), path.clone(), body.clone()));
            let (status, response) = match method {
                Method::GET => (
                    200,
                    store
                        .objects
                        .get(&path)
                        .unwrap_or_else(|| panic!("unexpected GET {path}"))
                        .clone(),
                ),
                Method::POST
                    if path == format!("{API}/karstasks") || path == format!("{API}/karsteams") =>
                {
                    let mut created = body;
                    let name = created["metadata"]["name"].as_str().unwrap().to_owned();
                    let target = format!("{path}/{name}");
                    assert!(!store.objects.contains_key(&target));
                    created["metadata"]["namespace"] = "work".into();
                    created["metadata"]["uid"] = format!("{name}-uid").into();
                    created["metadata"]["resourceVersion"] = "1".into();
                    if store.prune_create {
                        prune(&mut created);
                    }
                    store.objects.insert(target, created.clone());
                    (201, created)
                }
                Method::PATCH if path == format!("{API}/karstasks/team-principal") => {
                    let current = store.objects.get_mut(&path).expect("principal task");
                    json_patch::merge(current, &body);
                    (200, current.clone())
                }
                Method::PATCH if path == TASK || path == TEAM => {
                    if let Some(change) = store.before_patch.take() {
                        json_patch::merge(store.objects.get_mut(&path).unwrap(), &change);
                    }
                    let prune_patch = store.prune_patch;
                    let current = store.objects.get_mut(&path).unwrap();
                    if body["metadata"]["uid"] != current["metadata"]["uid"]
                        || body["metadata"]["resourceVersion"]
                            != current["metadata"]["resourceVersion"]
                    {
                        (
                            409,
                            json!({"apiVersion":"v1","kind":"Status","status":"Failure",
                            "reason":"Conflict","code":409,"message":"captured identity changed"}),
                        )
                    } else {
                        json_patch::merge(current, &body);
                        current["metadata"]["resourceVersion"] = "11".into();
                        if prune_patch {
                            prune(current);
                        }
                        (200, current.clone())
                    }
                }
                _ => panic!("unexpected {method} {path}"),
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
        AppState::for_test_client(kube::Client::new(service, "work"), "work"),
        store,
    )
}

async fn request(state: AppState, method: Method, route: &str, body: Value) -> (StatusCode, Value) {
    let response = super::router(state)
        .layer(Extension(Principal {
            sub: "owner".into(),
            name: "owner".into(),
            roles: vec!["user".into()],
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

fn with_options(store: &mut Store) {
    let core = "/apis/kars.azure.com/v1alpha1/namespaces/kars-system";
    let mut grant = store.objects[GRANT].clone();
    grant["metadata"]["namespace"] = "kars-system".into();
    grant["spec"]["workspaceUid"] = "core-uid".into();
    store
        .objects
        .insert(format!("{core}/karscredentialgrants/workspace"), grant);
    store.objects.insert("/api/v1/namespaces/kars-system".into(), json!({
        "apiVersion":"v1", "kind":"Namespace", "metadata":{"name":"kars-system","uid":"core-uid"}
    }));
    store.objects.insert(
        "/apis/apps/v1/namespaces/kars-system/deployments/kars-controller".into(),
        json!({
            "apiVersion":"apps/v1", "kind":"Deployment", "metadata":{"name":"kars-controller"},
            "spec":{"selector":{"matchLabels":{"app":"controller"}},"template":{
                "metadata":{"labels":{"app":"controller"}},"spec":{"containers":[{
                    "name":"controller","image":"ghcr.io/example/controller:latest","env":[
                        {"name":"KARS_PROVIDER","value":"foundry"},
                        {"name":"KARS_TASK_DEFAULT_MODEL","value":"primary"},
                        {"name":"SANDBOX_IMAGE","value":"ghcr.io/example/runtime:latest"}
                    ]
                }]}
            }}
        }),
    );
    for (plural, kind) in [
        ("inferencepolicies", "InferencePolicy"),
        ("toolpolicies", "ToolPolicy"),
        ("mcpservers", "McpServer"),
        ("karsmemories", "KarsMemory"),
        ("karsskills", "KarsSkill"),
    ] {
        store.objects.insert(format!("/apis/kars.azure.com/v1alpha1/{plural}"), json!({
            "apiVersion":"kars.azure.com/v1alpha1", "kind":format!("{kind}List"), "metadata":{}, "items":[]
        }));
    }
    store.objects.insert(
        format!("{API}/toolpolicies/kars-default"),
        json!({
            "apiVersion":"kars.azure.com/v1alpha1", "kind":"ToolPolicy",
            "metadata":{"name":"kars-default","namespace":"work"},
            "spec":{"appliesTo":{"tool":"*"},"rules":[]}
        }),
    );
    store.objects.insert(
        "/api/v1/namespaces/kars-system/configmaps/kars-mcp-profiles".into(),
        json!({
            "apiVersion":"v1", "kind":"ConfigMap", "metadata":{"name":"kars-mcp-profiles"},
            "data":{"profiles.json":"[]"}
        }),
    );
}

fn qualified_child(test: &str) -> bool {
    const CHILD: &str = "KARS_EXECUTION_PLAN_ROUTE_CHILD";
    if std::env::var(CHILD).ok().as_deref() == Some(test) {
        return true;
    }
    let records = json!([{
        "runtime":"OpenClaw", "provider":"azure-foundry", "deployment":"primary",
        "capabilities":["team","telemetry","delegation","filesystem-write","mcp","artifacts"],
        "max_parallel":1, "min_total_tokens":1000,
        "evidence":{"task":"test-fixture","run_id":"test-run","digest":"sha256:test-fixture"}
    }]);
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("routes::execution_plan_tests::{test}"),
            "--nocapture",
        ])
        .env(CHILD, test)
        .env("BRIDGE_CORE_NAMESPACE", "kars-system")
        .env("BRIDGE_ROUTE_QUALIFICATION_MODE", "required")
        .env("BRIDGE_QUALIFICATION_RECORDS_JSON", records.to_string())
        .env_remove("BRIDGE_ADDITIONAL_QUALIFICATION_RECORDS_JSON")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    false
}

fn create_team_request(launch: bool) -> Value {
    json!({
        "name":"created-team", "charter":"Write useful volunteer handbooks.", "tier":1, "launch":launch,
        "runtime":"OpenClaw", "model":"azure-foundry::primary",
        "budget":{"tokens":2_000_000,"scope":"GovernedInference"},
        "roles":[{"name":"writer","system_prompt":"Write the requested useful handbook."}],
        "execution_plan":crate::routes::tasks::ExecutionPlanDto::from_crd(&crate::kars::task::execution_plan::tests::plan())
    })
}

#[tokio::test]
async fn execution_plan_team_create_active_rejects_before_discovery_or_writes() {
    let (state, store) = fixture(task());
    let (status, body) = request(state, Method::POST, "teams", create_team_request(true)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body.to_string().contains("TypedPlanExecutionUnavailable"),
        "{body}"
    );
    let store = store.lock().unwrap();
    assert_eq!(
        store
            .calls
            .iter()
            .map(|(_, path, _)| path.as_str())
            .collect::<Vec<_>>(),
        [GRANT, "/api/v1/namespaces/work"]
    );
    assert!(
        store
            .calls
            .iter()
            .all(|(method, _, _)| *method == Method::GET)
    );
}

#[tokio::test]
async fn execution_plan_team_create_retains_reviewed_draft_or_rejects_pruning() {
    if !qualified_child("execution_plan_team_create_retains_reviewed_draft_or_rejects_pruning") {
        return;
    }
    for pruning in [false, true] {
        let (state, store) = fixture(task());
        {
            let mut store = store.lock().unwrap();
            with_options(&mut store);
            store.prune_create = pruning;
        }
        let (status, body) =
            request(state, Method::POST, "teams", create_team_request(false)).await;
        assert_eq!(
            status,
            if pruning {
                StatusCode::BAD_GATEWAY
            } else {
                StatusCode::OK
            },
            "{body}"
        );
        let store = store.lock().unwrap();
        let created = &store.objects[&format!("{API}/karsteams/created-team")];
        assert_eq!(created["spec"]["paused"], true);
        assert_eq!(
            created["metadata"]["annotations"][MARKER],
            "execution-plan/v1"
        );
        assert_eq!(
            created["metadata"]["annotations"]["kars.azure.com/owner-sub"],
            "owner"
        );
        let writes = store
            .calls
            .iter()
            .filter(|(method, _, _)| *method != Method::GET)
            .collect::<Vec<_>>();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].0, Method::POST);
        assert_eq!(writes[0].2["spec"]["blueprint"]["executionPlan"], plan());
        let create_index = store
            .calls
            .iter()
            .position(|(method, _, _)| *method == Method::POST)
            .unwrap();
        if pruning {
            assert!(created["spec"]["blueprint"]["executionPlan"].is_null());
            assert_eq!(
                create_index + 1,
                store.calls.len(),
                "no credential attachment after pruned CREATE"
            );
        } else {
            assert_eq!(created["spec"]["blueprint"]["executionPlan"], plan());
            assert!(
                store.calls[create_index + 1..]
                    .iter()
                    .any(|(_, path, _)| path == GRANT)
            );
        }
    }
}

#[tokio::test]
async fn execution_plan_team_plan_edit_clears_principal_park_only_after_preservation() {
    if !qualified_child(
        "execution_plan_team_plan_edit_clears_principal_park_only_after_preservation",
    ) {
        return;
    }
    for pruning in [false, true] {
        let mut object = reviewed(team());
        object["spec"]["blueprint"]["runtime"] = "OpenClaw".into();
        object["spec"]["blueprint"]["model"] =
            json!({"provider":"azure-foundry","deployment":"primary"});
        let (state, store) = fixture(object);
        let principal_path = format!("{API}/karstasks/team-principal");
        let mut principal = task();
        principal["metadata"]["name"] = "team-principal".into();
        principal["metadata"]["annotations"]["kars.azure.com/retry-not-before"] =
            "2099-01-01T00:00:00Z".into();
        {
            let mut store = store.lock().unwrap();
            with_options(&mut store);
            store
                .objects
                .insert(principal_path.clone(), principal.clone());
            store.prune_patch = pruning;
        }
        let mut changed = crate::kars::task::execution_plan::tests::plan();
        changed.synthesis.objective = "Assemble the complete revised volunteer handbook.".into();
        let (status, body) = request(
            state,
            Method::PATCH,
            "teams/team",
            json!({
                "execution_plan":crate::routes::tasks::ExecutionPlanDto::from_crd(&changed)
            }),
        )
        .await;
        assert_eq!(
            status,
            if pruning {
                StatusCode::BAD_GATEWAY
            } else {
                StatusCode::OK
            },
            "{body}"
        );
        let store = store.lock().unwrap();
        let writes = store
            .calls
            .iter()
            .filter(|(method, _, _)| *method != Method::GET)
            .collect::<Vec<_>>();
        assert_eq!(writes.len(), if pruning { 1 } else { 2 });
        assert_eq!(writes[0].1, TEAM);
        assert_fence(&writes[0].2, "team-uid");
        assert_eq!(store.objects[TEAM]["spec"]["paused"], true);
        if pruning {
            assert_eq!(
                store.objects[&principal_path], principal,
                "pruned update must not clear the principal park"
            );
        } else {
            assert_eq!(
                store.objects[TEAM]["spec"]["blueprint"]["executionPlan"],
                json!(changed)
            );
            assert_eq!(writes[1].1, principal_path);
            assert!(store.objects[&principal_path]["metadata"]["annotations"]["kars.azure.com/retry-not-before"].is_null());
        }
    }
}

fn assert_only_read(store: &Store, path: &str) {
    assert_eq!(store.calls, vec![(Method::GET, path.into(), Value::Null)]);
}

fn assert_fence(patch: &Value, uid: &str) {
    assert_eq!(patch["metadata"]["uid"], uid);
    assert_eq!(patch["metadata"]["resourceVersion"], "10");
}

#[tokio::test]
async fn execution_plan_manual_team_run_rejects_before_queue_or_task_access() {
    for mut object in blocked_teams() {
        object["spec"]["paused"] = false.into();
        let (state, store) = fixture(object.clone());
        let (status, body) = request(state, Method::POST, "teams/team/run", json!({})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        let store = store.lock().unwrap();
        assert_only_read(&store, TEAM);
        assert_eq!(store.objects[TEAM], object);
    }
}

#[tokio::test]
async fn execution_plan_task_launch_and_active_replica_reject_before_writes() {
    for object in blocked_tasks() {
        for (route, body) in [
            ("tasks/task/launch", json!({"launch":true})),
            ("tasks/task/replicate", json!({"launch":true,"count":2})),
        ] {
            let (state, store) = fixture(object.clone());
            let (status, body) = request(state, Method::POST, route, body).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
            let store = store.lock().unwrap();
            assert_only_read(&store, TASK);
            assert_eq!(store.objects[TASK], object);
        }
    }
}

#[tokio::test]
async fn execution_plan_legacy_task_launch_and_safe_stop_are_identity_fenced() {
    let mut cases = vec![(task(), true)];
    cases.extend(blocked_tasks().into_iter().map(|mut object| {
        object["spec"]["execution"]["launch"] = true.into();
        (object, false)
    }));
    for (object, active) in cases {
        let (state, store) = fixture(object.clone());
        let (status, body) = request(
            state,
            Method::POST,
            "tasks/task/launch",
            json!({"launch":active}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let store = store.lock().unwrap();
        assert_eq!(store.calls.len(), 2);
        assert_eq!(store.calls[1].0, Method::PATCH);
        assert_fence(&store.calls[1].2, "task-uid");
        assert_eq!(store.objects[TASK]["spec"]["execution"]["launch"], active);
        assert_eq!(
            store.objects[TASK]["spec"]["blueprint"],
            object["spec"]["blueprint"]
        );
    }
}

#[tokio::test]
async fn execution_plan_task_launch_cannot_activate_a_replaced_or_changed_task() {
    for change in [
        json!({"metadata":{"uid":"replacement-uid"}}),
        json!({"metadata":{"resourceVersion":"12"},"spec":{"blueprint":{"executionPlan":plan()}}}),
    ] {
        let (state, store) = fixture(task());
        store.lock().unwrap().before_patch = Some(change);
        let (status, body) = request(
            state,
            Method::POST,
            "tasks/task/launch",
            json!({"launch":true}),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        let store = store.lock().unwrap();
        assert_eq!(store.calls.len(), 2);
        assert_fence(&store.calls[1].2, "task-uid");
        assert_eq!(store.objects[TASK]["spec"]["execution"]["launch"], false);
    }
}

#[tokio::test]
async fn execution_plan_paused_replica_preserves_plan_marker_and_owner() {
    let source = reviewed(task());
    let (state, store) = fixture(source.clone());
    let (status, body) = request(
        state,
        Method::POST,
        "tasks/task/replicate",
        json!({"launch":false,"count":1}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["count"], 1);
    let store = store.lock().unwrap();
    let replica_path = format!("{API}/karstasks/{}", body["runs"][0].as_str().unwrap());
    let replica = &store.objects[&replica_path];
    assert_eq!(
        replica["spec"]["blueprint"]["executionPlan"],
        source["spec"]["blueprint"]["executionPlan"]
    );
    assert_eq!(
        replica["metadata"]["annotations"][MARKER],
        "execution-plan/v1"
    );
    assert_eq!(
        replica["metadata"]["annotations"]["kars.azure.com/owner-sub"],
        "owner"
    );
    assert_eq!(replica["spec"]["execution"]["launch"], false);
    assert!(replica["spec"]["parentRef"].is_null());
    assert_eq!(store.objects[TASK], source);
    assert_eq!(
        store
            .calls
            .iter()
            .map(|(method, path, _)| (method.clone(), path.clone()))
            .collect::<Vec<_>>(),
        vec![
            (Method::GET, TASK.into()),
            (Method::POST, format!("{API}/karstasks")),
            (Method::GET, GRANT.into()),
            (Method::GET, "/api/v1/namespaces/work".into()),
            (Method::GET, replica_path),
        ]
    );
}

#[tokio::test]
async fn execution_plan_pruned_replica_create_stops_before_credential_attachment() {
    let (state, store) = fixture(reviewed(task()));
    store.lock().unwrap().prune_create = true;
    let (status, body) = request(
        state,
        Method::POST,
        "tasks/task/replicate",
        json!({"launch":false,"count":2}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    let store = store.lock().unwrap();
    assert_eq!(store.calls.len(), 2);
    assert_eq!(store.calls[1].0, Method::POST);
    let replicas = store
        .objects
        .iter()
        .filter(|(path, _)| path.starts_with(&format!("{TASK}-rep-")))
        .collect::<Vec<_>>();
    assert_eq!(replicas.len(), 1);
    assert_eq!(replicas[0].1["spec"]["execution"]["launch"], false);
    assert!(replicas[0].1["spec"]["blueprint"]["executionPlan"].is_null());
    assert_eq!(
        replicas[0].1["metadata"]["annotations"][MARKER],
        "execution-plan/v1"
    );
}

#[tokio::test]
async fn execution_plan_paused_replica_rejects_missing_or_unsupported_source_plan() {
    for object in blocked_tasks().into_iter().skip(1) {
        let (state, store) = fixture(object);
        let (status, body) = request(
            state,
            Method::POST,
            "tasks/task/replicate",
            json!({"launch":false,"count":1}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_only_read(&store.lock().unwrap(), TASK);
    }
}

#[tokio::test]
async fn execution_plan_team_edits_validate_effective_active_state() {
    for mut object in blocked_teams() {
        object["spec"]["paused"] = false.into();
        let (state, store) = fixture(object.clone());
        let (status, body) = request(
            state,
            Method::PATCH,
            "teams/team",
            json!({"charter":"A revised useful handbook charter."}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        let store = store.lock().unwrap();
        assert_only_read(&store, TEAM);
        assert_eq!(store.objects[TEAM], object);
    }
}

#[tokio::test]
async fn execution_plan_paused_team_edits_preserve_root_and_role_plans() {
    for object in blocked_teams().into_iter().take(2) {
        let (state, store) = fixture(object.clone());
        let (status, body) = request(
            state,
            Method::PATCH,
            "teams/team",
            json!({"charter":"A revised useful handbook charter."}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let store = store.lock().unwrap();
        assert_eq!(store.calls.len(), 2);
        assert_fence(&store.calls[1].2, "team-uid");
        assert_eq!(store.objects[TEAM]["spec"]["paused"], true);
        assert_eq!(
            store.objects[TEAM]["spec"]["charter"],
            "A revised useful handbook charter."
        );
        assert_eq!(
            store.objects[TEAM]["spec"]["blueprint"],
            object["spec"]["blueprint"]
        );
        assert_eq!(
            store.objects[TEAM]["spec"]["roster"],
            object["spec"]["roster"]
        );
    }
}

#[tokio::test]
async fn execution_plan_team_edit_rejects_pruned_readback_without_followup_writes() {
    let (state, store) = fixture(reviewed(team()));
    store.lock().unwrap().prune_patch = true;
    let (status, body) = request(
        state,
        Method::PATCH,
        "teams/team",
        json!({"charter":"A revised useful handbook charter."}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    let store = store.lock().unwrap();
    assert_eq!(store.calls.len(), 2);
    assert_fence(&store.calls[1].2, "team-uid");
    assert_eq!(store.objects[TEAM]["spec"]["paused"], true);
    assert!(store.objects[TEAM]["spec"]["blueprint"]["executionPlan"].is_null());
}

#[tokio::test]
async fn execution_plan_team_edits_do_not_retry_concurrent_identity_changes() {
    for change in [
        json!({"metadata":{"uid":"replacement-uid"}}),
        json!({"metadata":{"resourceVersion":"12"}}),
    ] {
        let object = reviewed(team());
        let (state, store) = fixture(object.clone());
        store.lock().unwrap().before_patch = Some(change);
        let (status, body) = request(
            state,
            Method::PATCH,
            "teams/team",
            json!({"charter":"A revised useful handbook charter."}),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        let store = store.lock().unwrap();
        assert_eq!(store.calls.len(), 2);
        assert_fence(&store.calls[1].2, "team-uid");
        assert_eq!(store.objects[TEAM]["spec"], object["spec"]);
    }
}

#[tokio::test]
async fn execution_plan_team_pure_pause_is_allowed_but_combined_invalid_edits_are_not() {
    for mut object in blocked_teams() {
        object["spec"]["paused"] = false.into();
        let (state, store) = fixture(object.clone());
        let (status, body) =
            request(state, Method::PATCH, "teams/team", json!({"paused":true})).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let store = store.lock().unwrap();
        assert_eq!(store.calls.len(), 2);
        assert_fence(&store.calls[1].2, "team-uid");
        assert_eq!(store.objects[TEAM]["spec"]["paused"], true);
        assert_eq!(
            store.objects[TEAM]["spec"]["blueprint"],
            object["spec"]["blueprint"]
        );
    }
    for object in blocked_teams().into_iter().skip(2) {
        let (state, store) = fixture(object);
        let (status, body) = request(
            state,
            Method::PATCH,
            "teams/team",
            json!({"paused":true,"charter":"A revised useful handbook charter."}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_only_read(&store.lock().unwrap(), TEAM);
    }
}
