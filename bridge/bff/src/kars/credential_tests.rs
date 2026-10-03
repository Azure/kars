// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::cluster::Cluster;
use super::credentials::*;
use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, Method, Uri},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

#[path = "credential_discovery_tests.rs"]
mod discovery;

#[path = "credential_binding_tests.rs"]
mod binding_repairs;
#[path = "credential_handler_tests.rs"]
mod handler_conflicts;
#[path = "observation_credential_tests.rs"]
mod observations;

#[test]
fn budget_scope_survives_the_private_consumer_projection_without_defaulting_legacy_budgets() {
    let legacy = serde_json::json!({"tokens":100,"usdMicros":null});
    let budget: super::task::TaskBudget = serde_json::from_value(legacy.clone()).unwrap();
    assert_eq!(
        serde_json::to_value(budget).unwrap(),
        serde_json::json!({"tokens":100})
    );
    let governed: super::task::TaskBudget = serde_json::from_value(serde_json::json!({
        "scope":"GovernedInference","tokens":100
    }))
    .unwrap();
    assert_eq!(
        serde_json::to_value(governed).unwrap()["scope"],
        "GovernedInference"
    );
}

struct TestApi {
    calls: Vec<(String, String, Value)>,
    secret: Value,
    forbidden: bool,
    objects: std::collections::BTreeMap<String, Value>,
    pending_source_ack: Option<Value>,
    fault: Option<handler_conflicts::Fault>,
    source_metadata_reads: usize,
    source_value_reads: usize,
    bind_source_owner: bool,
    publish_ownership_receipt: bool,
    ownership_from_override: Option<String>,
    gate_patch_conflict: bool,
    target_read_mutation: Option<(String, usize, Value)>,
}

async fn handle(
    State(state): State<Arc<Mutex<TestApi>>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let mut state = state.lock().unwrap();
    state
        .calls
        .push((method.to_string(), uri.path().into(), body.clone()));
    if method == Method::GET
        && let Some((path, read_number, _)) = &state.target_read_mutation
        && path == uri.path()
        && state
            .calls
            .iter()
            .filter(|(method, called, _)| method == "GET" && called == path)
            .count()
            == *read_number
    {
        let (path, _, patch) = state.target_read_mutation.take().unwrap();
        let target = state.objects.get_mut(&path).unwrap();
        json_patch::merge(target, &patch);
        let version = target["metadata"]["resourceVersion"]
            .as_str()
            .unwrap()
            .parse::<u64>()
            .unwrap()
            + 1;
        target["metadata"]["resourceVersion"] = version.to_string().into();
    }
    if let Some(response) = handler_conflicts::before_request(&mut state, &method, uri.path()) {
        return response;
    }
    if state.forbidden {
        return (axum::http::StatusCode::FORBIDDEN,axum::Json(json!({
        "kind":"Status","apiVersion":"v1","status":"Failure","reason":"Forbidden","code":403,"message":"PRIVATE_VALUE_SENTINEL"}))).into_response();
    }
    const WORKSPACE_GRANT: &str =
        "/apis/kars.azure.com/v1alpha1/namespaces/work/karscredentialgrants/workspace";
    if method == Method::GET
        && uri.path() == WORKSPACE_GRANT
        && let Some(mut source) = state.pending_source_ack.take()
    {
        let previous_version = source["metadata"]["resourceVersion"].clone();
        let annotations = source["metadata"]["annotations"].clone();
        let target_uid = annotations["kars.azure.com/credential-target-uid"].clone();
        if state.bind_source_owner && target_uid.is_string() {
            source["metadata"]["ownerReferences"] = json!([{
                "apiVersion":"kars.azure.com/v1alpha1",
                "kind":annotations["kars.azure.com/credential-target-kind"],
                "name":annotations["kars.azure.com/credential-target"],
                "uid":target_uid, "controller":true, "blockOwnerDeletion":false
            }]);
            source["metadata"]["resourceVersion"] =
                (previous_version.as_str().unwrap().parse::<u64>().unwrap() + 1)
                    .to_string()
                    .into();
            state.objects.insert(
                format!(
                    "/api/v1/namespaces/work/secrets/{}",
                    source["metadata"]["name"].as_str().unwrap()
                ),
                source.clone(),
            );
        }
        binding_repairs::acknowledge(&mut state, &source);
        if state.bind_source_owner && target_uid.is_string() {
            let publish = state.publish_ownership_receipt;
            let from = state.ownership_from_override.clone();
            let entry =
                &mut state.objects.get_mut(WORKSPACE_GRANT).unwrap()["status"]["sources"][0];
            entry["phase"] = "Ready".into();
            entry["target"] = json!({
                "kind":annotations["kars.azure.com/credential-target-kind"],
                "namespace":"work", "name":annotations["kars.azure.com/credential-target"],
                "uid":target_uid
            });
            if publish {
                entry["ownershipFromResourceVersion"] =
                    from.map(Value::String).unwrap_or(previous_version);
            }
        }
    }
    if method == Method::GET
        && uri
            .path()
            .starts_with("/api/v1/namespaces/work/secrets/kars-credential-input-")
    {
        let name = uri.path().rsplit('/').next().unwrap();
        let enrolled = state
            .objects
            .get(WORKSPACE_GRANT)
            .and_then(|grant| grant["status"]["sources"].as_array())
            .is_some_and(|sources| sources.iter().any(|source| source["name"] == name));
        if !enrolled {
            return (axum::http::StatusCode::FORBIDDEN,axum::Json(json!({
                "apiVersion":"v1","kind":"Status","status":"Failure","reason":"Forbidden","code":403,
                "message":"Source GET is not enrolled"
            }))).into_response();
        }
    }
    if (method == Method::GET
        || (method == Method::POST && uri.path().ends_with("/selfsubjectreviews")))
        && let Some(value) = state.objects.get(uri.path())
    {
        let value = value.clone();
        if method == Method::GET && uri.path().contains("/secrets/kars-credential-input-") {
            if headers
                .get("accept")
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.contains("as=PartialObjectMetadata"))
            {
                let metadata = value["metadata"].clone();
                state.source_metadata_reads += 1;
                return axum::Json(json!({"apiVersion":"meta.k8s.io/v1","kind":"PartialObjectMetadata","metadata":metadata})).into_response();
            }
            state.source_value_reads += 1;
        }
        return axum::Json(value).into_response();
    }
    if method == Method::GET {
        for (resource, kind) in [
            ("karsteams", "KarsTeam"),
            ("karstasks", "KarsTask"),
            ("karssandboxes", "KarsSandbox"),
        ] {
            if uri.path().ends_with(&format!("/{resource}")) {
                let items = state
                    .objects
                    .iter()
                    .filter(|(path, _)| path.starts_with(&format!("{}/", uri.path())))
                    .map(|(_, value)| value.clone())
                    .collect::<Vec<_>>();
                return axum::Json(
                    json!({"apiVersion":"kars.azure.com/v1alpha1","kind":format!("{kind}List"),
                    "metadata":{},"items":items}),
                )
                .into_response();
            }
        }
    }
    if method == Method::POST && uri.path() == "/api/v1/namespaces/work/secrets" {
        let name = body["metadata"]["name"].as_str().unwrap();
        let path = format!("{}/{name}", uri.path());
        if state.objects.contains_key(&path) {
            return (axum::http::StatusCode::CONFLICT,axum::Json(json!({
                "apiVersion":"v1","kind":"Status","status":"Failure","reason":"AlreadyExists","code":409
            }))).into_response();
        }
        let mut value = body.clone();
        value["metadata"]["uid"] = "created-source".into();
        value["metadata"]["resourceVersion"] = "1".into();
        for (key, text) in body["stringData"].as_object().into_iter().flatten() {
            value["data"][key] = json!(k8s_openapi::ByteString(
                text.as_str().unwrap().as_bytes().to_vec()
            ));
        }
        value.as_object_mut().unwrap().remove("stringData");
        let name = value["metadata"]["name"].as_str().unwrap().to_string();
        state
            .objects
            .insert(format!("{}/{name}", uri.path()), value.clone());
        state.pending_source_ack = Some(value.clone());
        if let Some(response) = handler_conflicts::after_write(&mut state, &method, uri.path()) {
            return response;
        }
        return (axum::http::StatusCode::CREATED, axum::Json(value)).into_response();
    }
    if method == Method::PATCH && state.objects.contains_key(uri.path()) {
        if state.gate_patch_conflict {
            state.gate_patch_conflict = false;
            let object = state.objects.get_mut(uri.path()).unwrap();
            object["metadata"]["resourceVersion"] = "3".into();
            object["status"]["phase"] = "Ready".into();
        }
        let mut value = state.objects[uri.path()].clone();
        let old_spec = value.get("spec").cloned();
        if body.is_array() {
            let patch: json_patch::Patch = serde_json::from_value(body).unwrap();
            if json_patch::patch(&mut value, &patch).is_err() {
                return (
                    axum::http::StatusCode::CONFLICT,
                    axum::Json(
                        json!({"kind":"Status","apiVersion":"v1","code":409,"reason":"Conflict"}),
                    ),
                )
                    .into_response();
            }
        } else {
            if body["metadata"]["uid"] != value["metadata"]["uid"]
                || body["metadata"]["resourceVersion"] != value["metadata"]["resourceVersion"]
            {
                return handler_conflicts::api_failure(409);
            }
            binding_repairs::merge(&mut value, &body);
        }
        if value.get("spec") != old_spec.as_ref()
            && let Some(generation) = value["metadata"]["generation"].as_i64()
        {
            value["metadata"]["generation"] = (generation + 1).into();
        }
        let version = value["metadata"]["resourceVersion"]
            .as_str()
            .unwrap()
            .parse::<u64>()
            .unwrap()
            + 1;
        value["metadata"]["resourceVersion"] = version.to_string().into();
        state.objects.insert(uri.path().into(), value.clone());
        if uri.path().contains("/secrets/kars-credential-input-") {
            state.pending_source_ack = Some(value.clone());
        }
        if let Some(response) = handler_conflicts::after_write(&mut state, &method, uri.path()) {
            return response;
        }
        return axum::Json(value).into_response();
    }
    let value=match (method,uri.path()) {
        (Method::GET,"/apis/kars.azure.com/v1alpha1/namespaces/work/karscredentialgrants/workspace")=>json!({
            "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsCredentialGrant",
            "metadata":{"name":"workspace","namespace":"work","uid":"grant","resourceVersion":"1","generation":1},
            "spec":{"enabled":true,"workspaceUid":"namespace","agentKeys":["GITHUB_TOKEN"],
                "integrationStores":[{"secret":{"name":"test-teams","uid":"secret"},"purpose":"teams"}],"legacyImports":[]},
            "status":{"phase":"Ready","observedGeneration":1,"sources":[],"legacySources":[]}}),
        (Method::GET,"/api/v1/namespaces/work")=>json!({"metadata":{"name":"work","uid":"namespace","resourceVersion":"1"}}),
        (Method::GET,"/api/v1/namespaces/work/secrets/test-teams")=>state.secret.clone(),
        (Method::PATCH,"/api/v1/namespaces/work/secrets/test-teams")=>{
            let mut current=state.secret.clone();
            let patch:json_patch::Patch=serde_json::from_value(body).unwrap();
            if json_patch::patch(&mut current,&patch).is_err() {
                return (axum::http::StatusCode::UNPROCESSABLE_ENTITY,axum::Json(json!({
                    "kind":"Status","apiVersion":"v1","status":"Failure","reason":"Invalid","code":422,"message":"CAS rejected"}))).into_response();
            }
            state.secret=current;state.secret.clone()
        }
        _=>return (axum::http::StatusCode::NOT_FOUND,axum::Json(json!({
            "kind":"Status","apiVersion":"v1","status":"Failure","reason":"NotFound","code":404,"message":"missing"}))).into_response(),
    };
    axum::Json(value).into_response()
}

async fn fixture() -> (Cluster, Arc<Mutex<TestApi>>, tokio::task::JoinHandle<()>) {
    let state = Arc::new(Mutex::new(TestApi {
        calls: Vec::new(),
        forbidden: false,
        objects: std::collections::BTreeMap::new(),
        pending_source_ack: None,
        fault: None,
        source_metadata_reads: 0,
        source_value_reads: 0,
        bind_source_owner: false,
        publish_ownership_receipt: false,
        ownership_from_override: None,
        gate_patch_conflict: false,
        target_read_mutation: None,
        secret: json!({
        "apiVersion":"v1","kind":"Secret","type":"Opaque","metadata":{"name":"test-teams","namespace":"work","uid":"secret","resourceVersion":"2"},
        "data":{"client-id":"b2xk","bff-internal-secret":"cHJlc2VydmVk"}}),
    }));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let app = Router::new().fallback(handle).with_state(state.clone());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client =
        kube::Client::try_from(kube::Config::new(format!("http://{addr}").parse().unwrap()))
            .unwrap();
    (Cluster::for_test_client(client), state, task)
}

fn created_target(kind: &str, active: bool) -> (Target, String, Value) {
    let resource = if kind == "KarsTask" {
        "karstasks"
    } else {
        "karsteams"
    };
    let target = Target {
        kind: kind.into(),
        namespace: "work".into(),
        name: "draft".into(),
        uid: "draft-uid".into(),
    };
    let path = format!("/apis/kars.azure.com/v1alpha1/namespaces/work/{resource}/draft");
    let spec = if kind == "KarsTask" {
        json!({"execution":{"launch":active},"blueprint":{}})
    } else {
        json!({"paused":!active,"blueprint":{}})
    };
    let object = json!({"apiVersion":"kars.azure.com/v1alpha1","kind":kind,
        "metadata":{"name":"draft","namespace":"work","uid":"draft-uid","resourceVersion":"2"},
        "spec":spec,"status":{"phase":"Ready"}});
    (target, path, object)
}

fn reviewed_plan_changes(kind: &str) -> Vec<Value> {
    let plan = super::task::execution_plan::tests::plan();
    let mut changes = vec![
        json!({"spec":{"blueprint":{"executionPlan":plan}}}),
        json!({"metadata":{"annotations":{"kars.azure.com/mission-decomposition":"execution-plan/v1"}}}),
    ];
    if kind == "KarsTeam" {
        changes.push(
            json!({"spec":{"roster":[{"name":"writer","blueprint":{"executionPlan":plan}}]}}),
        );
    }
    changes
}

#[tokio::test]
async fn credential_finish_rejects_unsupported_or_pruned_plans_before_credentials() {
    for kind in ["KarsTask", "KarsTeam"] {
        for change in reviewed_plan_changes(kind) {
            let (cluster, state, server) = fixture().await;
            let (target, path, mut object) = created_target(kind, false);
            json_patch::merge(&mut object, &change);
            state
                .lock()
                .unwrap()
                .objects
                .insert(path.clone(), object.clone());
            let error = cluster
                .finish_created_credentials(&target, true)
                .await
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("TypedPlanExecutionUnavailable")
                    || error.contains("ReviewedExecutionPlanMissing"),
                "{error}"
            );
            {
                let s = state.lock().unwrap();
                assert_eq!(s.objects[&path], object);
                assert_eq!(s.calls.len(), 1);
                assert_eq!((&s.calls[0].0, &s.calls[0].1), (&"GET".to_string(), &path));
            }
            server.abort();
        }
    }
}

#[tokio::test]
async fn credential_finish_rechecks_plan_and_identity_after_credential_validation() {
    for kind in ["KarsTask", "KarsTeam"] {
        let mut changes = reviewed_plan_changes(kind);
        changes.extend([
            json!({"metadata":{"uid":"replacement"}}),
            json!({"metadata":{"deletionTimestamp":"2026-10-03T00:00:00Z"}}),
        ]);
        for change in changes {
            let (cluster, state, server) = fixture().await;
            let (target, path, object) = created_target(kind, false);
            {
                let mut s = state.lock().unwrap();
                s.objects.insert(path.clone(), object);
                s.target_read_mutation = Some((path.clone(), 2, change));
            }
            assert!(
                cluster
                    .finish_created_credentials(&target, true)
                    .await
                    .is_err()
            );
            {
                let s = state.lock().unwrap();
                assert!(s.target_read_mutation.is_none());
                assert!(s.calls.iter().all(|(method, _, _)| method == "GET"));
                assert!(
                    s.calls
                        .iter()
                        .any(|(_, path, _)| path.ends_with("/karscredentialgrants/workspace"))
                );
                if kind == "KarsTask" {
                    assert_eq!(s.objects[&path]["spec"]["execution"]["launch"], false);
                } else {
                    assert_eq!(s.objects[&path]["spec"]["paused"], true);
                }
            }
            server.abort();
        }
    }
}

#[tokio::test]
async fn credential_finish_can_stop_unsupported_or_pruned_plan_targets() {
    for kind in ["KarsTask", "KarsTeam"] {
        for change in reviewed_plan_changes(kind) {
            let (cluster, state, server) = fixture().await;
            let (target, path, mut object) = created_target(kind, true);
            json_patch::merge(&mut object, &change);
            state.lock().unwrap().objects.insert(path.clone(), object);
            cluster
                .finish_created_credentials(&target, false)
                .await
                .unwrap();
            {
                let s = state.lock().unwrap();
                let writes: Vec<_> = s
                    .calls
                    .iter()
                    .filter(|(method, _, _)| method != "GET")
                    .collect();
                assert_eq!(writes.len(), 1);
                assert_eq!(writes[0].0, "PATCH");
                assert_eq!(
                    writes[0].2["metadata"],
                    json!({"uid":"draft-uid","resourceVersion":"2"})
                );
                if kind == "KarsTask" {
                    assert_eq!(s.objects[&path]["spec"]["execution"]["launch"], false);
                } else {
                    assert_eq!(s.objects[&path]["spec"]["paused"], true);
                }
            }
            server.abort();
        }
    }
}

#[tokio::test]
async fn credential_finish_unchanged_gate_validates_grant_and_target_without_writing() {
    for kind in ["KarsTask", "KarsTeam"] {
        for active in [false, true] {
            let (cluster, state, server) = fixture().await;
            let (target, path, object) = created_target(kind, active);
            state
                .lock()
                .unwrap()
                .objects
                .insert(path.clone(), object.clone());
            cluster
                .finish_created_credentials(&target, active)
                .await
                .unwrap();
            {
                let s = state.lock().unwrap();
                assert_eq!(s.objects[&path], object);
                assert!(s.calls.iter().all(|(method, _, _)| method == "GET"));
                assert!(
                    s.calls
                        .iter()
                        .any(|(_, path, _)| path.ends_with("/karscredentialgrants/workspace"))
                );
                assert!(s.calls.iter().any(|(_, p, _)| p == &path));
            }
            server.abort();
        }
    }
}

#[tokio::test]
async fn credential_finish_changed_gate_retains_exact_uid_resource_version_preconditions() {
    for kind in ["KarsTask", "KarsTeam"] {
        for active in [false, true] {
            let (cluster, state, server) = fixture().await;
            let (target, path, object) = created_target(kind, !active);
            state.lock().unwrap().objects.insert(path.clone(), object);
            cluster
                .finish_created_credentials(&target, active)
                .await
                .unwrap();
            {
                let s = state.lock().unwrap();
                let writes: Vec<_> = s
                    .calls
                    .iter()
                    .filter(|(method, _, _)| method != "GET")
                    .collect();
                assert_eq!(writes.len(), 1);
                assert_eq!((&writes[0].0, &writes[0].1), (&"PATCH".to_string(), &path));
                assert_eq!(
                    writes[0].2["metadata"],
                    json!({"uid":"draft-uid","resourceVersion":"2"})
                );
                if kind == "KarsTask" {
                    assert_eq!(s.objects[&path]["spec"]["execution"]["launch"], active);
                } else {
                    assert_eq!(s.objects[&path]["spec"]["paused"], !active);
                }
            }
            server.abort();
        }
    }
}

#[tokio::test]
async fn credential_finish_noop_rejects_replacement_deletion_and_unavailable_grant() {
    for kind in ["KarsTask", "KarsTeam"] {
        for fault in ["replacement", "deletion", "grant"] {
            let (cluster, state, server) = fixture().await;
            let (target, path, mut object) = created_target(kind, false);
            match fault {
                "replacement" => object["metadata"]["uid"] = "replacement".into(),
                "deletion" => {
                    object["metadata"]["deletionTimestamp"] = "2026-10-02T17:00:00Z".into()
                }
                _ => state.lock().unwrap().forbidden = true,
            }
            state.lock().unwrap().objects.insert(path, object);
            assert!(
                cluster
                    .finish_created_credentials(&target, false)
                    .await
                    .is_err()
            );
            assert!(
                state
                    .lock()
                    .unwrap()
                    .calls
                    .iter()
                    .all(|(method, _, _)| method == "GET")
            );
            server.abort();
        }
    }
}

#[tokio::test]
async fn credential_finish_task_without_execution_uses_inactive_default() {
    for execution in [
        None,
        Some(Value::Null),
        Some(json!({})),
        Some(json!({"runtime":"OpenClaw"})),
    ] {
        for active in [false, true] {
            let (cluster, state, server) = fixture().await;
            let (target, path, mut object) = created_target("KarsTask", false);
            object["spec"].as_object_mut().unwrap().remove("execution");
            if let Some(execution) = &execution {
                object["spec"]["execution"] = execution.clone();
            }
            state
                .lock()
                .unwrap()
                .objects
                .insert(path.clone(), object.clone());
            cluster
                .finish_created_credentials(&target, active)
                .await
                .unwrap();
            {
                let s = state.lock().unwrap();
                assert_eq!(
                    s.calls
                        .iter()
                        .filter(|(method, _, _)| method == "PATCH")
                        .count(),
                    usize::from(active)
                );
                if active {
                    assert_eq!(s.objects[&path]["spec"]["execution"]["launch"], true);
                } else {
                    assert_eq!(s.objects[&path], object);
                }
            }
            server.abort();
        }
    }
}

#[tokio::test]
async fn credential_finish_real_transition_conflict_is_not_retried_or_adopted() {
    for kind in ["KarsTask", "KarsTeam"] {
        let (cluster, state, server) = fixture().await;
        let (target, path, object) = created_target(kind, false);
        {
            let mut s = state.lock().unwrap();
            s.objects.insert(path.clone(), object.clone());
            s.gate_patch_conflict = true;
        }
        let error = cluster
            .finish_created_credentials(&target, true)
            .await
            .unwrap_err();
        assert!(matches!(error, kube::Error::Api(ref response) if response.code == 409));
        {
            let s = state.lock().unwrap();
            assert_eq!(s.objects[&path]["spec"], object["spec"]);
            assert_eq!(
                s.calls
                    .iter()
                    .filter(|(method, _, _)| method == "PATCH")
                    .count(),
                1
            );
        }
        server.abort();
    }
}

#[tokio::test]
async fn credential_finish_missing_or_malformed_gate_is_not_treated_as_satisfied() {
    for kind in ["KarsTask", "KarsTeam"] {
        for malformed in [Value::Null, json!("false"), json!(0)] {
            let (cluster, state, server) = fixture().await;
            let (target, path, mut object) = created_target(kind, false);
            if kind == "KarsTask" {
                object["spec"]["execution"]["launch"] = malformed;
            } else {
                object["spec"]["paused"] = malformed;
            }
            state.lock().unwrap().objects.insert(path, object);
            cluster
                .finish_created_credentials(&target, false)
                .await
                .unwrap();
            assert_eq!(
                state
                    .lock()
                    .unwrap()
                    .calls
                    .iter()
                    .filter(|(method, _, _)| method == "PATCH")
                    .count(),
                1
            );
            server.abort();
        }
    }
}

#[tokio::test]
async fn credential_finish_mutation_rejects_missing_resource_version() {
    for kind in ["KarsTask", "KarsTeam"] {
        let (cluster, state, server) = fixture().await;
        let (target, path, mut object) = created_target(kind, false);
        object["metadata"]
            .as_object_mut()
            .unwrap()
            .remove("resourceVersion");
        state.lock().unwrap().objects.insert(path, object);
        assert!(
            cluster
                .finish_created_credentials(&target, true)
                .await
                .is_err()
        );
        assert!(
            state
                .lock()
                .unwrap()
                .calls
                .iter()
                .all(|(method, _, _)| method == "GET")
        );
        server.abort();
    }
}

#[tokio::test]
async fn credential_store_updates_keep_uid_and_unrelated_keys_with_real_cas() {
    let (cluster, state, server) = fixture().await;
    cluster
        .mutate_integration("work", "test-teams", |keys| {
            keys.insert("client-id".into(), "updated".into());
        })
        .await
        .unwrap();
    {
        let state = state.lock().unwrap();
        assert_eq!(state.secret["metadata"]["uid"], "secret");
        assert_eq!(state.secret["data"]["bff-internal-secret"], "cHJlc2VydmVk");
        let patch = &state
            .calls
            .iter()
            .find(|(method, _, _)| method == "PATCH")
            .unwrap()
            .2;
        assert_eq!(
            patch[0],
            json!({"op":"test","path":"/metadata/uid","value":"secret"})
        );
        assert_eq!(
            patch[1],
            json!({"op":"test","path":"/metadata/resourceVersion","value":"2"})
        );
        assert!(
            !state
                .calls
                .iter()
                .any(|(method, _, _)| method == "DELETE" || method == "POST")
        );
    }
    server.abort();
}

#[tokio::test]
async fn credential_reads_do_not_fallback_on_forbidden_or_recreated_store() {
    let (cluster, state, server) = fixture().await;
    state.lock().unwrap().forbidden = true;
    let error = cluster
        .integration_store("work", "test-teams")
        .await
        .unwrap_err()
        .to_string();
    assert!(!error.contains("PRIVATE_VALUE_SENTINEL"));
    state.lock().unwrap().forbidden = false;
    state.lock().unwrap().secret["metadata"]["uid"] = "replacement".into();
    assert!(
        cluster
            .integration_store("work", "test-teams")
            .await
            .is_err()
    );
    assert!(
        state
            .lock()
            .unwrap()
            .calls
            .iter()
            .all(|(method, _, _)| method == "GET")
    );
    server.abort();
}
