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
