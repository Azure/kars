// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::plan::*;
use super::*;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn source(uid: &str) -> McpServer {
    serde_json::from_value(json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"McpServer",
        "metadata":{"name":"browser","namespace":"workspace","uid":uid,"resourceVersion":"1","generation":1},
        "spec":{"managed":{"preset":"playwright"},"allowedTools":["browser_navigate"]}})).unwrap()
}

fn config() -> Config {
    Config {
        controller_namespace: "kars-system".into(),
        namespace: "kars-mcp".into(),
        playwright_image: "mcr.microsoft.com/playwright/mcp:latest".into(),
        everything_image: "ghcr.io/azure/kars/mcp-everything:latest".into(),
        pull_secret: None,
    }
}

fn namespace() -> Value {
    json!({"apiVersion":"v1","kind":"Namespace","metadata":{"name":"kars-mcp","uid":"managed-ns",
        "resourceVersion":"1","annotations":{CLAIM:"v1",CONTROLLER_NS:"kars-system",CONTROLLER_UID:"controller-ns"},
        "labels":{"pod-security.kubernetes.io/enforce":"restricted"}}})
}

#[derive(Default)]
struct ApiState {
    objects: BTreeMap<String, Value>,
    calls: Vec<(String, String, Value)>,
    error: Option<u16>,
    conflict: bool,
}

fn failure(code: u16) -> ResponseTemplate {
    ResponseTemplate::new(code).set_body_json(json!({"apiVersion":"v1","kind":"Status",
        "code":code,"status":"Failure","reason":if code==404 {"NotFound"} else if code==409 {"Conflict"} else {"Forbidden"},
        "message":"PRIVATE_API_RESPONSE"}))
}

fn merge(value: &mut Value, patch: &Value) {
    if let Some(object) = patch.as_object() {
        if !value.is_object() {
            *value = json!({});
        }
        for (key, item) in object {
            if item.is_null() {
                value.as_object_mut().unwrap().remove(key);
            } else {
                merge(&mut value[key], item);
            }
        }
    } else {
        *value = patch.clone();
    }
}

async fn fixture() -> (MockServer, Client, Arc<Mutex<ApiState>>) {
    let server = MockServer::start().await;
    let state = Arc::new(Mutex::new(ApiState::default()));
    {
        let mut state = state.lock().unwrap();
        state.objects.insert(
            "/api/v1/namespaces/kars-system".into(),
            json!({"metadata":{"name":"kars-system","uid":"controller-ns","resourceVersion":"1"}}),
        );
        state.objects.insert(
            "/apis/kars.azure.com/v1alpha1/namespaces/workspace/mcpservers/browser".into(),
            serde_json::to_value(source("source")).unwrap(),
        );
    }
    let handler = state.clone();
    Mock::given(|_: &wiremock::Request| true)
        .respond_with(move |request: &wiremock::Request| {
            let mut state = handler.lock().unwrap();
            let method = request.method.as_str();
            let path = request.url.path();
            let body: Value = request.body_json().unwrap_or(Value::Null);
            state.calls.push((method.into(), path.into(), body.clone()));
            if let Some(code) = state.error {
                return failure(code);
            }
            match method {
                "GET" => state
                    .objects
                    .get(path)
                    .map(|value| ResponseTemplate::new(200).set_body_json(value))
                    .unwrap_or_else(|| failure(404)),
                "POST" => {
                    let key = format!("{path}/{}", body["metadata"]["name"].as_str().unwrap());
                    if state.objects.contains_key(&key) || state.conflict {
                        return failure(409);
                    }
                    let mut body = body;
                    body["metadata"]["uid"] = "created-resource".into();
                    body["metadata"]["resourceVersion"] = "1".into();
                    state.objects.insert(key, body.clone());
                    ResponseTemplate::new(201).set_body_json(body)
                }
                "PATCH" => {
                    if state.conflict {
                        return failure(409);
                    }
                    let Some(value) = state.objects.get_mut(path) else {
                        return failure(404);
                    };
                    assert_eq!(body["metadata"]["uid"], value["metadata"]["uid"]);
                    assert_eq!(
                        body["metadata"]["resourceVersion"],
                        value["metadata"]["resourceVersion"]
                    );
                    merge(value, &body);
                    value["metadata"]["resourceVersion"] = "2".into();
                    ResponseTemplate::new(200).set_body_json(value.clone())
                }
                "DELETE" => {
                    let Some(value) = state.objects.get(path) else {
                        return failure(404);
                    };
                    assert_eq!(body["preconditions"]["uid"], value["metadata"]["uid"]);
                    assert_eq!(
                        body["preconditions"]["resourceVersion"],
                        value["metadata"]["resourceVersion"]
                    );
                    if state.conflict {
                        return failure(409);
                    }
                    state.objects.remove(path);
                    ResponseTemplate::new(200).set_body_json(
                        json!({"apiVersion":"v1","kind":"Status","status":"Success"}),
                    )
                }
                _ => failure(405),
            }
        })
        .mount(&server)
        .await;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
    (server, client, state)
}

fn writes(state: &ApiState) -> usize {
    state
        .calls
        .iter()
        .filter(|(method, _, _)| method != "GET")
        .count()
}

#[test]
fn managed_plans_have_uid_unique_selectors_rootless_posture_and_no_credentials() {
    let first = Plan::new(&source("first"), &config()).unwrap();
    let second = Plan::new(&source("second"), &config()).unwrap();
    assert_ne!(first.owner.name, second.owner.name);
    let pod = first.deployment("namespace-uid", None);
    assert_eq!(
        pod["spec"]["selector"]["matchLabels"],
        json!({SOURCE_UID:"first"})
    );
    assert_eq!(
        first.service("namespace-uid")["spec"]["selector"],
        json!({SOURCE_UID:"first"})
    );
    assert_eq!(
        pod["spec"]["template"]["spec"]["automountServiceAccountToken"],
        false
    );
    assert_eq!(
        pod["spec"]["template"]["spec"]["containers"][0]["securityContext"]["readOnlyRootFilesystem"],
        true
    );
    assert_eq!(
        pod["spec"]["template"]["spec"]["containers"][0]["imagePullPolicy"],
        "Always"
    );
    assert!(first.args.iter().any(|argument| argument == "--isolated"));
    let policy = first.network_policy("namespace-uid", vec![]);
    assert!(
        policy["spec"]["egress"][1]["to"][0]["ipBlock"]["except"]
            .as_array()
            .unwrap()
            .contains(&json!("169.254.0.0/16"))
    );
    let mut invalid = source("valid");
    invalid.metadata.uid = None;
    assert!(Plan::new(&invalid, &config()).is_err());
    invalid.metadata.uid = Some("valid".into());
    invalid.metadata.name = Some("browser.team".into());
    assert!(Plan::new(&invalid, &config()).is_err());
}

#[test]
fn managed_schema_is_closed_and_endpoint_auth_paths_cannot_be_combined() {
    for extra in [
        json!({"url":"https://example/mcp"}),
        json!({"productionMode":false}),
        json!({"bearerFromEnv":"TOKEN"}),
        json!({"oauth":{"issuer":"https://example"}}),
    ] {
        let mut value = serde_json::to_value(source("source")).unwrap();
        merge(&mut value["spec"], &extra);
        let value: McpServer = serde_json::from_value(value).unwrap();
        assert!(validate_source(&value).is_err());
    }
    assert!(
        serde_json::from_value::<crate::mcp_server::ManagedMcpConfig>(
            json!({"preset":"playwright","image":"unreviewed:latest"})
        )
        .is_err()
    );
    assert!(
        serde_json::from_value::<crate::mcp_server::ManagedMcpConfig>(
            json!({"preset":"unreviewed"})
        )
        .is_err()
    );
}

#[tokio::test]
async fn namespace_creation_is_claimed_without_adopting_existing_occupants() {
    let (_server, client, state) = fixture().await;
    let namespace = ownership::namespace(&client, &config(), &source("source"), true)
        .await
        .unwrap()
        .unwrap();
    assert!(ownership::namespace_owned(
        &namespace,
        &config(),
        "controller-ns"
    ));
    {
        let mut guard = state.lock().unwrap();
        let ns = guard
            .objects
            .get_mut("/api/v1/namespaces/kars-mcp")
            .unwrap();
        ns["metadata"]["annotations"][CONTROLLER_UID] = "foreign".into();
        guard.calls.clear();
    }
    assert!(
        ownership::namespace(&client, &config(), &source("source"), true)
            .await
            .is_err()
    );
    assert_eq!(writes(&state.lock().unwrap()), 0);
}

#[tokio::test]
async fn namespace_replacement_api_errors_and_stale_sources_never_create_workloads() {
    for kind in ["replacement", "api-error", "source"] {
        let (_server, client, state) = fixture().await;
        let mut source = source("source");
        match kind {
            "replacement" => {
                source.status =
                    Some(serde_json::from_value(json!({"managedNamespaceUid":"old"})).unwrap());
                state
                    .lock()
                    .unwrap()
                    .objects
                    .insert("/api/v1/namespaces/kars-mcp".into(), namespace());
            }
            "api-error" => state.lock().unwrap().error = Some(403),
            _ => source.metadata.uid = Some("stale".into()),
        }
        let result = ownership::namespace(&client, &config(), &source, true).await;
        assert!(result.is_err(), "{kind}");
        assert_eq!(writes(&state.lock().unwrap()), 0);
        assert!(!result.err().unwrap().contains("PRIVATE_API_RESPONSE"));
    }
}

#[tokio::test]
async fn exact_owned_resource_updates_use_cas_and_preserve_unrelated_metadata() {
    let (_server, client, state) = fixture().await;
    state
        .lock()
        .unwrap()
        .objects
        .insert("/api/v1/namespaces/kars-mcp".into(), namespace());
    let source = source("source");
    let plan = Plan::new(&source, &config()).unwrap();
    let api: Api<Service> = Api::namespaced(client.clone(), "kars-mcp");
    let service = ownership::upsert(
        &client,
        &source,
        &api,
        &plan.owner,
        "managed-ns",
        plan.service("managed-ns"),
    )
    .await
    .unwrap();
    assert!(ownership::owned(
        &service.metadata,
        &plan.owner,
        "managed-ns"
    ));
    let path = format!("/api/v1/namespaces/kars-mcp/services/{}", plan.owner.name);
    {
        let mut state = state.lock().unwrap();
        state.objects.get_mut(&path).unwrap()["metadata"]["annotations"]["customer"] =
            "preserve".into();
        state.calls.clear();
    }
    ownership::upsert(
        &client,
        &source,
        &api,
        &plan.owner,
        "managed-ns",
        plan.service("managed-ns"),
    )
    .await
    .unwrap();
    assert_eq!(
        writes(&state.lock().unwrap()),
        0,
        "unchanged specs must not cause rollout loops"
    );
    let mut changed = plan.service("managed-ns");
    changed["spec"]["ports"][0]["port"] = 3001.into();
    ownership::upsert(&client, &source, &api, &plan.owner, "managed-ns", changed)
        .await
        .unwrap();
    assert_eq!(
        state.lock().unwrap().objects[&path]["metadata"]["annotations"]["customer"],
        "preserve"
    );
}

#[tokio::test]
async fn selector_or_uid_conflicts_are_preserved_and_cleanup_is_cas_fenced() {
    let (_server, client, state) = fixture().await;
    state
        .lock()
        .unwrap()
        .objects
        .insert("/api/v1/namespaces/kars-mcp".into(), namespace());
    let source = source("source");
    let plan = Plan::new(&source, &config()).unwrap();
    let api: Api<Service> = Api::namespaced(client.clone(), "kars-mcp");
    ownership::upsert(
        &client,
        &source,
        &api,
        &plan.owner,
        "managed-ns",
        plan.service("managed-ns"),
    )
    .await
    .unwrap();
    let path = format!("/api/v1/namespaces/kars-mcp/services/{}", plan.owner.name);
    {
        let mut state = state.lock().unwrap();
        state.objects.get_mut(&path).unwrap()["spec"]["selector"][SOURCE_UID] = "foreign".into();
        state.calls.clear();
    }
    assert!(
        ownership::delete(&api, &plan.owner, "managed-ns")
            .await
            .is_err()
    );
    assert_eq!(writes(&state.lock().unwrap()), 0);
    {
        let mut state = state.lock().unwrap();
        state.objects.get_mut(&path).unwrap()["spec"]["selector"][SOURCE_UID] = "source".into();
        state.conflict = true;
    }
    assert!(
        ownership::delete(&api, &plan.owner, "managed-ns")
            .await
            .is_err()
    );
    assert!(state.lock().unwrap().objects.contains_key(&path));
    state.lock().unwrap().conflict = false;
    assert!(
        ownership::delete(&api, &plan.owner, "managed-ns")
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn cleanup_never_uses_an_arbitrary_status_reference_as_delete_authority() {
    let (_server, client, state) = fixture().await;
    let mut source = source("source");
    source.status =
        Some(serde_json::from_value(json!({"workloadRef":"kars-mcp/foreign"})).unwrap());
    assert!(cleanup(&client, &source).await.is_err());
    assert_eq!(writes(&state.lock().unwrap()), 0);
}

#[test]
fn pending_conditions_never_claim_readiness() {
    let conditions = pending_conditions(&[], Some(2), "Waiting for rollout");
    assert_eq!(
        conditions
            .iter()
            .find(|condition| condition.type_ == "Ready")
            .unwrap()
            .status,
        "False"
    );
    assert_eq!(
        conditions
            .iter()
            .find(|condition| condition.type_ == "Progressing")
            .unwrap()
            .status,
        "True"
    );
}
