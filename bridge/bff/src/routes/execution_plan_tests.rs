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
    get_failures: BTreeMap<String, u16>,
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

fn budget_catalogue(model: &str) -> Value {
    json!({"apiVersion":"v1","kind":"ConfigMap",
        "metadata":{"name":"kars-inference-budget-contracts"},
        "data":{"contracts.json":json!({
            "version":"foundry-only-v1",
            "contracts":[{
                "id":"foundry-chat","version":"v1","validUntil":"2030-01-01T00:00:00Z",
                "providerId":"azure-openai","endpoint":"https://models.example",
                "model":model,"operation":"ChatCompletions",
                "outputField":"MaxTokens","maximumInputTokens":100,
                "maximumOutputTokens":50,"maximumWireBytes":4096,
                "outputBoundIncludesReasoning":true
            }]
        }).to_string()}})
}

#[tokio::test]
async fn missing_budget_contract_blocks_launch_and_replication_before_writes() {
    for route in ["tasks/task/launch", "tasks/task/replicate"] {
        let mut object = task();
        object["spec"]["envelope"]["budget"] = json!({"scope":"GovernedInference","tokens":40000});
        object["spec"]["blueprint"] =
            json!({"model":{"provider":"github-copilot","deployment":"gpt-6.1-sol"}});
        let (state, store) = fixture(object);
        store.lock().unwrap().objects.insert(
            "/api/v1/namespaces/kars-system/configmaps/kars-inference-budget-contracts".into(),
            budget_catalogue("gpt-5.4-mini"),
        );
        let (status, body) =
            request(state, Method::POST, route, json!({"launch":true,"count":2})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.to_string().contains("gpt-6.1-sol"), "{body}");
        let store = store.lock().unwrap();
        assert!(
            store
                .calls
                .iter()
                .all(|(method, _, _)| *method == Method::GET)
        );
        assert_eq!(store.objects[TASK]["spec"]["execution"]["launch"], false);
    }
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

#[tokio::test]
async fn impossible_role_reservation_blocks_team_resume_and_run_without_writes() {
    for resume in [false, true] {
        let mut object = team();
        let mut plan = json!(crate::kars::task::execution_plan::tests::filesystem_plan());
        for role in plan["roles"].as_array_mut().unwrap() {
            role["budgetTokens"] = json!(150_000);
        }
        object["spec"]["roster"] = json!(
            plan["roles"]
                .as_array()
                .unwrap()
                .iter()
                .map(|role| json!({"name":role["name"]}))
                .collect::<Vec<_>>()
        );
        object["spec"]["envelope"]["delegationDepth"] = json!(3);
        object["spec"]["envelope"]["budget"] =
            json!({"scope":"GovernedInference","tokens":1_500_000});
        object["spec"]["paused"] = json!(resume);
        object["spec"]["blueprint"] = json!({
            "model":{"provider":"azure-foundry","deployment":"gpt-5.4-mini"},
            "executionPlan":plan
        });
        object["metadata"]["annotations"][MARKER] = json!("execution-plan/v1");
        let (state, store) = fixture(object);
        with_options(&mut store.lock().unwrap());
        let mut catalogue = budget_catalogue("gpt-5.4-mini");
        let mut contracts: Value =
            serde_json::from_str(catalogue["data"]["contracts.json"].as_str().unwrap()).unwrap();
        contracts["contracts"][0]["maximumInputTokens"] = json!(272_000);
        catalogue["data"]["contracts.json"] = json!(contracts.to_string());
        store.lock().unwrap().objects.insert(
            "/api/v1/namespaces/kars-system/configmaps/kars-inference-budget-contracts".into(),
            catalogue,
        );
        let (status, body) = if resume {
            request(state, Method::PATCH, "teams/team", json!({"paused":false})).await
        } else {
            request(state, Method::POST, "teams/team/run", json!({})).await
        };
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        let detail = body.to_string();
        assert!(
            detail.contains("Role") && detail.contains("150000") && detail.contains("272001"),
            "{body}"
        );
        let store = store.lock().unwrap();
        assert!(
            store
                .calls
                .iter()
                .all(|(method, _, _)| *method == Method::GET)
        );
        assert_eq!(store.objects[TEAM]["spec"]["paused"], resume);
        assert_eq!(
            store.objects[TEAM]["spec"]["envelope"]["budget"]["tokens"],
            1_500_000
        );
    }
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
                Method::GET if store.get_failures.contains_key(&path) => {
                    let code = store.get_failures[&path];
                    (
                        code,
                        json!({"apiVersion":"v1","kind":"Status","status":"Failure",
                        "code":code,"reason":if code == 404 { "NotFound" } else { "Forbidden" },
                        "message":"fixture policy unavailable"}),
                    )
                }
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
                Method::PATCH
                    if path == TASK
                        || path == TEAM
                        || path == format!("{API}/karstasks/created-task")
                        || path == format!("{API}/karsteams/created-team") =>
                {
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
    store.objects.insert(
        "/api/v1/namespaces/kars-system/configmaps/kars-inference-budget-contracts".into(),
        budget_catalogue("primary"),
    );
    store.objects.insert(
        "/api/v1/namespaces/kars-system/configmaps/kars-inference-budgets".into(),
        json!({"apiVersion":"v1", "kind":"ConfigMap",
            "metadata":{"name":"kars-inference-budgets"}, "data":{"budgets.json":"{}"}}),
    );
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
                        {"name":"SANDBOX_IMAGE","value":"ghcr.io/example/runtime:latest"},
                        {"name":"IMAGE_PULL_REGISTRIES","value":"ghcr.io"}
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
        format!("{core}/karsskills"),
        json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSkillList","metadata":{},"items":[]}),
    );
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
        "capabilities":["team","telemetry","delegation","filesystem-read","filesystem-write","mcp","artifacts"],
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
async fn profile_team_create_preserves_budget_and_pinned_policy_without_fallback() {
    if !qualified_child("profile_team_create_preserves_budget_and_pinned_policy_without_fallback") {
        return;
    }
    for failure in [None, Some(404), Some(403)] {
        let (state, store) = fixture(team());
        {
            let mut store = store.lock().unwrap();
            with_options(&mut store);
            let path = format!("{API}/toolpolicies/restricted");
            let mut policy = store.objects[&format!("{API}/toolpolicies/kars-default")].clone();
            policy["metadata"]["name"] = "restricted".into();
            store.objects.insert(path.clone(), policy);
            if let Some(code) = failure {
                store.get_failures.insert(path, code);
            }
        }
        let mut body = create_team_request(false);
        body["tool_policy"] = "restricted".into();
        body["pin_tool_policy"] = true.into();
        body["budget"]["usd_micros"] = 250000.into();
        body["delegation_depth"] = 0.into();
        body["roles"][0]["runtime"] = "OpenClaw".into();
        let (status, response) = request(state, Method::POST, "teams", body).await;
        let store = store.lock().unwrap();
        if failure.is_some() {
            assert_eq!(
                status,
                if failure == Some(404) {
                    StatusCode::BAD_REQUEST
                } else {
                    StatusCode::BAD_GATEWAY
                },
                "{response}"
            );
            assert!(
                store
                    .calls
                    .iter()
                    .all(|(method, _, _)| *method == Method::GET)
            );
            assert!(
                !store
                    .objects
                    .contains_key(&format!("{API}/karsteams/created-team"))
            );
        } else {
            assert_eq!(status, StatusCode::OK, "{response}");
            let spec = &store.objects[&format!("{API}/karsteams/created-team")]["spec"];
            assert_eq!(spec["paused"], true);
            assert_eq!(spec["envelope"]["delegationDepth"], 0);
            assert_eq!(spec["envelope"]["budget"]["tokens"], 2_000_000);
            assert_eq!(spec["envelope"]["budget"]["usdMicros"], 250000);
            assert_eq!(spec["envelope"]["toolPolicyRef"]["name"], "restricted");
            assert_eq!(spec["blueprint"]["toolPolicy"], "restricted");
            assert_eq!(spec["roster"][0]["blueprint"]["toolPolicy"], "restricted");
            assert_eq!(
                spec["roster"][0]["blueprint"]["model"],
                spec["blueprint"]["model"]
            );
        }
    }
}

#[tokio::test]
async fn team_partial_role_routes_use_the_effective_updated_parent() {
    if !qualified_child("team_partial_role_routes_use_the_effective_updated_parent") {
        return;
    }
    let mut object = reviewed(team());
    object["spec"]["blueprint"]["model"] =
        json!({"provider":"old-provider","deployment":"old-model"});
    object["spec"]["blueprint"]["runtime"] = "Hermes".into();
    object["spec"]["blueprint"]["toolPolicy"] = "kars-default".into();
    let (state, store) = fixture(object);
    with_options(&mut store.lock().unwrap());
    let (status, response) = request(
        state,
        Method::PATCH,
        "teams/team",
        json!({
            "runtime":"OpenClaw",
            "model":"azure-foundry::primary",
            "roles":[{"name":"writer","runtime":"OpenClaw","model":null}]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let store = store.lock().unwrap();
    let spec = &store.objects[TEAM]["spec"];
    assert_eq!(
        spec["roster"][0]["blueprint"]["model"],
        spec["blueprint"]["model"]
    );
    assert_eq!(spec["roster"][0]["blueprint"]["runtime"], "OpenClaw");
    assert_eq!(spec["roster"][0]["blueprint"]["toolPolicy"], "kars-default");
    assert_eq!(spec["paused"], true);
}

fn supported_plan_dto() -> Value {
    json!(crate::routes::tasks::ExecutionPlanDto::from_crd(
        &crate::kars::task::execution_plan::tests::filesystem_plan(),
    ))
}

fn create_task_request(depth: i32, launch: bool) -> Value {
    json!({
        "name":"created-task", "objective":"Write a useful volunteer handbook.", "launch":launch,
        "envelope":{"tier":1,"authority_ceiling":1,"delegation_depth":depth,
            "budget":{"tokens":2_000_000,"scope":"GovernedInference"}},
        "blueprint":{"runtime":"OpenClaw","model":{"provider":"azure-foundry","deployment":"primary"},
            "execution_plan":supported_plan_dto()}
    })
}

#[tokio::test]
async fn execution_plan_create_preserves_explicit_depth_and_team_default() {
    if !qualified_child("execution_plan_create_preserves_explicit_depth_and_team_default") {
        return;
    }
    for (depth, launch) in [
        (0, false),
        (1, false),
        (2, false),
        (4, false),
        (2, true),
        (4, true),
    ] {
        let (state, store) = fixture(task());
        with_options(&mut store.lock().unwrap());
        let (status, body) = request(
            state,
            Method::POST,
            "tasks",
            create_task_request(depth, launch),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "depth {depth}: {body}");
        let store = store.lock().unwrap();
        let created = &store.objects[&format!("{API}/karstasks/created-task")];
        assert_eq!(
            created["metadata"]["annotations"]["kars.azure.com/run-requested"]
                .as_str()
                .is_some_and(|nonce| nonce.starts_with("run-")),
            launch,
        );
        assert_eq!(created["spec"]["envelope"]["delegationDepth"], depth);
        assert_eq!(created["spec"]["envelope"]["budget"]["tokens"], 2_000_000);
        assert_eq!(
            created["spec"]["blueprint"]["executionPlan"],
            json!(crate::kars::task::execution_plan::tests::filesystem_plan())
        );
        assert_eq!(
            created["spec"]["execution"]["launch"]
                .as_bool()
                .unwrap_or(false),
            launch
        );
    }
    for (depth, launch) in [
        (None, false),
        (Some(1), false),
        (Some(3), false),
        (Some(4), false),
        (None, true),
        (Some(3), true),
        (Some(4), true),
    ] {
        let (state, store) = fixture(task());
        with_options(&mut store.lock().unwrap());
        let mut body = create_team_request(launch);
        body["execution_plan"] = supported_plan_dto();
        if let Some(depth) = depth {
            body["delegation_depth"] = json!(depth);
        }
        let (status, body) = request(state, Method::POST, "teams", body).await;
        assert_eq!(status, StatusCode::OK, "depth {depth:?}: {body}");
        let store = store.lock().unwrap();
        let created = &store.objects[&format!("{API}/karsteams/created-team")];
        assert_eq!(
            created["spec"]["envelope"]["delegationDepth"],
            depth.unwrap_or(3)
        );
        assert_eq!(created["spec"]["envelope"]["budget"]["tokens"], 2_000_000);
        assert_eq!(
            created["spec"]["blueprint"]["executionPlan"],
            json!(crate::kars::task::execution_plan::tests::filesystem_plan())
        );
        assert_eq!(created["spec"]["paused"], !launch);
    }
}

#[tokio::test]
async fn execution_plan_create_rejects_insufficient_active_depth_before_discovery() {
    for depth in [0, 1] {
        let (state, store) = fixture(task());
        let (status, body) = request(
            state,
            Method::POST,
            "tasks",
            create_task_request(depth, true),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(
            body.to_string()
                .contains("ReviewedDelegationDepthInsufficient"),
            "{body}"
        );
        assert!(
            store
                .lock()
                .unwrap()
                .calls
                .iter()
                .all(|(method, _, _)| *method == Method::GET)
        );
    }
    for depth in [0, 1, 2] {
        let (state, store) = fixture(task());
        let mut body = create_team_request(true);
        body["execution_plan"] = supported_plan_dto();
        body["delegation_depth"] = json!(depth);
        let (status, body) = request(state, Method::POST, "teams", body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(
            body.to_string()
                .contains("ReviewedDelegationDepthInsufficient"),
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
    }
}

#[tokio::test]
async fn execution_plan_task_launch_requires_two_levels_and_safe_stop_remains_available() {
    for depth in [1, 2] {
        for launch in [false, true] {
            let mut source = reviewed(task());
            source["spec"]["blueprint"]["executionPlan"] =
                json!(crate::kars::task::execution_plan::tests::filesystem_plan());
            source["spec"]["envelope"]["delegationDepth"] = json!(depth);
            let (state, store) = fixture(source);
            let (status, body) = request(
                state,
                Method::POST,
                "tasks/task/launch",
                json!({"launch":launch}),
            )
            .await;
            let rejected = launch && depth < 2;
            assert_eq!(
                status,
                if rejected {
                    StatusCode::BAD_REQUEST
                } else {
                    StatusCode::OK
                },
                "{body}"
            );
            let store = store.lock().unwrap();
            if rejected {
                assert!(
                    body.to_string()
                        .contains("ReviewedDelegationDepthInsufficient")
                );
                assert!(
                    store
                        .calls
                        .iter()
                        .all(|(method, _, _)| *method == Method::GET)
                );
            } else {
                assert_eq!(store.objects[TASK]["spec"]["execution"]["launch"], launch);
                assert_eq!(
                    store.objects[TASK]["metadata"]["annotations"]["kars.azure.com/run-requested"]
                        .as_str()
                        .is_some_and(|nonce| nonce.starts_with("run-")),
                    launch,
                );
                let writes: Vec<_> = store
                    .calls
                    .iter()
                    .filter(|(method, _, _)| *method == Method::PATCH)
                    .collect();
                assert_eq!(writes.len(), 1);
                assert_eq!(writes[0].2["spec"]["execution"]["launch"], launch);
                assert_eq!(
                    store.objects[TASK]["spec"]["envelope"]["delegationDepth"],
                    depth
                );
            }
        }
    }
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
        assert_eq!(created["spec"]["envelope"]["delegationDepth"], 3);
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
async fn execution_plan_manual_team_run_preserves_reviewed_depth() {
    for depth in [2, 3, 4] {
        let mut object = reviewed(team());
        object["spec"]["paused"] = false.into();
        object["spec"]["envelope"]["delegationDepth"] = depth.into();
        object["spec"]["blueprint"]["executionPlan"] =
            json!(crate::kars::task::execution_plan::tests::filesystem_plan());
        let (state, store) = fixture(object.clone());
        let backlog_path = "/api/v1/namespaces/work/configmaps/kars-team-tasks-team";
        {
            let mut store = store.lock().unwrap();
            store.objects.insert(format!("{API}/karstasks"), json!({
                "apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsTaskList", "metadata":{}, "items":[]
            }));
            store.objects.insert(backlog_path.into(), json!({
                "apiVersion":"v1", "kind":"ConfigMap",
                "metadata":{"name":"kars-team-tasks-team","namespace":"work","uid":"backlog-uid",
                    "resourceVersion":"10","ownerReferences":[{"apiVersion":"kars.azure.com/v1alpha1",
                        "kind":"KarsTeam","name":"team","uid":"team-uid","controller":true}]},
                "data":{"tasks.json":"[]"}
            }));
        }
        let (status, body) = request(state, Method::POST, "teams/team/run", json!({})).await;
        let store = store.lock().unwrap();
        assert_eq!(store.objects[TEAM]["spec"], object["spec"]);
        if depth < 3 {
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
            assert!(
                body.to_string()
                    .contains("ReviewedDelegationDepthInsufficient"),
                "{body}"
            );
            assert_only_read(&store, TEAM);
        } else {
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(store.calls.len(), 4);
            assert_eq!(
                store.calls[1],
                (Method::GET, backlog_path.into(), Value::Null)
            );
            assert_eq!(
                store.calls[2],
                (Method::GET, format!("{API}/karstasks"), Value::Null)
            );
            assert_eq!(store.calls[3].0, Method::PATCH);
            assert_fence(&store.calls[3].2, "team-uid");
            assert_eq!(
                store.objects[TEAM]["metadata"]["annotations"]["kars.azure.com/run-now"],
                body["request"]
            );
            assert!(
                body["request"]
                    .as_str()
                    .is_some_and(|request| !request.is_empty())
            );
        }
    }
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
