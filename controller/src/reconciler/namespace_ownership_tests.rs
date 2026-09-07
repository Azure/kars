// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use std::sync::{Arc, Mutex as StdMutex};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const NS_PATH: &str = "/api/v1/namespaces/kars-demo";
const CRS_PATH: &str = "/apis/kars.azure.com/v1alpha1/karssandboxes";
const CR_PATH: &str = "/apis/kars.azure.com/v1alpha1/namespaces/workspace-a/karssandboxes/demo";

fn fixture() -> Value {
    serde_json::from_str(include_str!(
        "../../../tests/compat/fixtures/namespace-legacy.json"
    ))
    .unwrap()
}

fn sandbox(value: &Value) -> KarsSandbox {
    serde_json::from_value(value.clone()).unwrap()
}

fn namespace(value: &Value) -> Namespace {
    serde_json::from_value(value.clone()).unwrap()
}

#[test]
fn legacy_evidence_accepts_cli_prestaged_namespace_without_changing_data() {
    let f = fixture();
    assert!(legacy_proof(
        &namespace(&f["namespace"]),
        &serde_json::from_value(f["deployment"].clone()).unwrap(),
        &sandbox(&f["sandbox"])
    ));
}

#[test]
fn legacy_evidence_rejects_labels_alone_foreign_parents_and_recreated_uids() {
    for pointer in [
        "/namespace/metadata/managedFields",
        "/deployment/metadata/managedFields",
        "/sandbox/status/namespace",
        "/sandbox/metadata/finalizers",
        "/deployment/spec/template/metadata/labels",
    ] {
        let mut f = fixture();
        *f.pointer_mut(pointer).unwrap() = Value::Null;
        assert!(
            !legacy_proof(
                &namespace(&f["namespace"]),
                &serde_json::from_value(f["deployment"].clone()).unwrap(),
                &sandbox(&f["sandbox"])
            ),
            "{pointer}"
        );
    }
    let mut f = fixture();
    f["deployment"]["metadata"]["labels"]["kars.azure.com/parent-namespace"] = json!("workspace-b");
    assert!(!legacy_proof(
        &namespace(&f["namespace"]),
        &serde_json::from_value(f["deployment"].clone()).unwrap(),
        &sandbox(&f["sandbox"])
    ));
    let mut f = fixture();
    f["sandbox"]["metadata"]["uid"] = json!("recreated");
    f["sandbox"]["metadata"]["creationTimestamp"] = json!("2026-09-01T11:00:00Z");
    assert!(!legacy_proof(
        &namespace(&f["namespace"]),
        &serde_json::from_value(f["deployment"].clone()).unwrap(),
        &sandbox(&f["sandbox"])
    ));
}

#[test]
fn claims_reject_foreign_workspace_uid_ownerrefs_and_namespace_replacement() {
    let f = fixture();
    let sb = sandbox(&f["sandbox"]);
    let mut ns = namespace(&f["namespace"]);
    ns.metadata.annotations = Some(serde_json::from_value(owner_annotations(&sb)).unwrap());
    assert!(claimed(&ns, &sb).unwrap());
    for key in [SOURCE_NAMESPACE, SOURCE_NAME, SOURCE_UID, VERSION] {
        let mut foreign = ns.clone();
        foreign
            .metadata
            .annotations
            .as_mut()
            .unwrap()
            .insert(key.into(), "foreign".into());
        assert!(claimed(&foreign, &sb).is_err(), "{key}");
    }
    let mut other = sb.clone();
    other
        .metadata
        .annotations
        .get_or_insert_default()
        .insert(NAMESPACE_UID.into(), "replaced".into());
    assert!(claimed(&ns, &other).is_err());
    let mut value = serde_json::to_value(&ns).unwrap();
    value["metadata"]["ownerReferences"] = json!([{
        "apiVersion": "v1", "kind": "Namespace", "name": "foreign", "uid": "foreign",
    }]);
    assert!(claimed(&namespace(&value), &sb).is_err());
}

#[test]
fn prestage_requires_explicit_two_way_intent_and_binds_only_once() {
    let f = fixture();
    let mut sb = sandbox(&f["sandbox"]);
    let mut ns = namespace(&f["namespace"]);
    let annotations = ns.metadata.annotations.get_or_insert_default();
    annotations.insert(VERSION.into(), "v1".into());
    annotations.insert(SOURCE_NAMESPACE.into(), "workspace-a".into());
    annotations.insert(SOURCE_NAME.into(), "demo".into());
    annotations.insert(PRESTAGE.into(), "bind-next-sandbox".into());
    assert!(!prestaged(&ns, &sb));
    sb.metadata
        .annotations
        .get_or_insert_default()
        .insert(NAMESPACE_UID.into(), "namespace-a".into());
    assert!(prestaged(&ns, &sb));
    ns.metadata
        .annotations
        .as_mut()
        .unwrap()
        .insert(SOURCE_UID.into(), "previous-incarnation".into());
    assert!(!prestaged(&ns, &sb));
    assert!(claimed(&ns, &sb).is_err());
}

#[test]
fn same_second_legacy_creation_is_ambiguous() {
    let mut f = fixture();
    f["sandbox"]["metadata"]["creationTimestamp"] =
        f["deployment"]["metadata"]["creationTimestamp"].clone();
    assert!(!legacy_proof(
        &namespace(&f["namespace"]),
        &serde_json::from_value(f["deployment"].clone()).unwrap(),
        &sandbox(&f["sandbox"]),
    ));
}

#[test]
fn namespace_watch_maps_only_the_exact_reserved_target_and_source() {
    let f = fixture();
    let mut ns = namespace(&f["namespace"]);
    assert!(to_sandbox_ref(ns.clone()).is_none());
    ns.metadata.annotations =
        Some(serde_json::from_value(owner_annotations(&sandbox(&f["sandbox"]))).unwrap());
    let reference = to_sandbox_ref(ns.clone()).unwrap();
    assert_eq!(reference.name, "demo");
    assert_eq!(reference.namespace.as_deref(), Some("workspace-a"));
    ns.metadata.name = Some("unrelated".into());
    assert!(to_sandbox_ref(ns).is_none());
}

#[derive(Clone)]
struct Store {
    sandboxes: Vec<Value>,
    namespace: Option<Value>,
    deployment: Option<Value>,
    version: u64,
    race_create: Option<Value>,
    replace_on_patch: bool,
    fail_namespace_get: u16,
    fail_delete: u16,
    fail_cr_patch_once: bool,
}

impl Default for Store {
    fn default() -> Self {
        let f = fixture();
        Self {
            sandboxes: vec![f["sandbox"].clone()],
            namespace: Some(f["namespace"].clone()),
            deployment: Some(f["deployment"].clone()),
            version: 100,
            race_create: None,
            replace_on_patch: false,
            fail_namespace_get: 0,
            fail_delete: 0,
            fail_cr_patch_once: false,
        }
    }
}

fn response(code: u16, value: Value) -> ResponseTemplate {
    ResponseTemplate::new(code).set_body_json(value)
}

fn failure(code: u16) -> ResponseTemplate {
    response(
        code,
        json!({
            "apiVersion": "v1", "kind": "Status", "status": "Failure",
            "reason": if code == 404 { "NotFound" } else { "Conflict" }, "code": code,
        }),
    )
}

fn merge(target: &mut Value, patch: Value) {
    if let Value::Object(patch) = patch {
        if !target.is_object() {
            *target = json!({});
        }
        for (key, value) in patch {
            if value.is_null() {
                target.as_object_mut().unwrap().remove(&key);
            } else {
                merge(
                    target
                        .as_object_mut()
                        .unwrap()
                        .entry(key)
                        .or_insert(Value::Null),
                    value,
                );
            }
        }
    } else {
        *target = patch;
    }
}

#[derive(Clone)]
struct Server(Arc<StdMutex<Store>>);

impl Respond for Server {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let mut store = self.0.lock().unwrap();
        let path = request.url.path();
        let method = request.method.as_str();
        if method == "GET" {
            if path == CRS_PATH {
                assert!(request.url.query().unwrap().contains("fieldSelector"));
                return response(
                    200,
                    json!({
                        "apiVersion": "kars.azure.com/v1alpha1", "kind": "KarsSandboxList",
                        "metadata": {}, "items": store.sandboxes,
                    }),
                );
            }
            if path.starts_with("/apis/kars.azure.com/v1alpha1/namespaces/")
                && path.ends_with("/karssandboxes/demo")
            {
                return store
                    .sandboxes
                    .iter()
                    .find(|s| s["metadata"]["namespace"].as_str() == path.split('/').nth(5))
                    .cloned()
                    .map_or_else(|| failure(404), |s| response(200, s));
            }
            if path == NS_PATH {
                if store.fail_namespace_get != 0 {
                    return failure(store.fail_namespace_get);
                }
                return store
                    .namespace
                    .clone()
                    .map_or_else(|| failure(404), |ns| response(200, ns));
            }
            if path == "/apis/apps/v1/namespaces/kars-demo/deployments/demo" {
                return store
                    .deployment
                    .clone()
                    .map_or_else(|| failure(404), |d| response(200, d));
            }
        }
        if method == "POST" && path == "/api/v1/namespaces" {
            if let Some(winner) = store.race_create.take() {
                store.namespace = Some(winner);
                return failure(409);
            }
            if store.namespace.is_some() {
                return failure(409);
            }
            let mut body: Value = serde_json::from_slice(&request.body).unwrap();
            body["metadata"]["uid"] = json!("namespace-new");
            store.version += 1;
            body["metadata"]["resourceVersion"] = json!(store.version.to_string());
            body["metadata"]["creationTimestamp"] = json!("2026-09-01T12:00:00Z");
            store.namespace = Some(body.clone());
            return response(201, body);
        }
        if method == "PATCH"
            && (path == NS_PATH || path == CR_PATH || path == format!("{CR_PATH}/status"))
        {
            assert_eq!(
                request.headers.get("content-type").unwrap(),
                "application/merge-patch+json"
            );
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            if path == NS_PATH && store.replace_on_patch {
                store.replace_on_patch = false;
                let ns = store.namespace.as_mut().unwrap();
                ns["metadata"]["uid"] = json!("replacement");
                ns["metadata"]["resourceVersion"] = json!("999");
            }
            if path == CR_PATH && store.fail_cr_patch_once {
                store.fail_cr_patch_once = false;
                return failure(409);
            }
            store.version += 1;
            let rv = store.version.to_string();
            let existing = if path == NS_PATH {
                store.namespace.as_mut().unwrap()
            } else {
                &mut store.sandboxes[0]
            };
            if body["metadata"]["uid"] != existing["metadata"]["uid"]
                || body["metadata"]["resourceVersion"] != existing["metadata"]["resourceVersion"]
            {
                return failure(409);
            }
            merge(existing, body);
            existing["metadata"]["resourceVersion"] = json!(rv);
            return response(200, existing.clone());
        }
        if method == "DELETE" && path == NS_PATH {
            if store.fail_delete != 0 {
                return failure(store.fail_delete);
            }
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            let Some(existing) = store.namespace.as_ref() else {
                return failure(404);
            };
            if body["preconditions"]["uid"] != existing["metadata"]["uid"]
                || body["preconditions"]["resourceVersion"]
                    != existing["metadata"]["resourceVersion"]
            {
                return failure(409);
            }
            return response(200, store.namespace.take().unwrap());
        }
        panic!("unexpected request (must not read/write Secrets or workloads): {method} {path}");
    }
}

async fn setup(store: Store) -> (MockServer, Client, Arc<StdMutex<Store>>) {
    let server = MockServer::start().await;
    let store = Arc::new(StdMutex::new(store));
    Mock::given(wiremock::matchers::any())
        .respond_with(Server(store.clone()))
        .mount(&server)
        .await;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
    (server, client, store)
}

#[tokio::test]
async fn adopts_unambiguous_legacy_metadata_only_and_is_idempotent() {
    let initial = Store::default();
    let original_namespace = initial.namespace.clone().unwrap();
    let original_deployment = initial.deployment.clone();
    let sb = sandbox(&initial.sandboxes[0]);
    let (server, client, store) = setup(initial).await;
    let (bound, ns) = ensure(&client, &sb).await.unwrap();
    assert!(claimed(&ns.unwrap(), &bound).unwrap());
    let after_first = server.received_requests().await.unwrap().len();
    ensure(&client, &bound).await.unwrap();
    assert!(
        server.received_requests().await.unwrap()[after_first..]
            .iter()
            .all(|r| r.method == "GET")
    );
    let state = store.lock().unwrap();
    assert_eq!(state.deployment, original_deployment);
    let ns = state.namespace.as_ref().unwrap();
    assert_eq!(
        ns["metadata"]["labels"],
        original_namespace["metadata"]["labels"]
    );
    assert_eq!(ns["spec"], original_namespace["spec"]);
    assert_eq!(ns["metadata"]["annotations"]["customer-annotation"], "keep");
}

#[tokio::test]
async fn rejects_arbitrary_namespace_without_any_writes() {
    let mut initial = Store::default();
    initial.namespace.as_mut().unwrap()["metadata"]["managedFields"] = Value::Null;
    let sb = sandbox(&initial.sandboxes[0]);
    let (server, client, _) = setup(initial).await;
    assert!(ensure(&client, &sb).await.is_err());
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|r| r.method == "GET")
    );
}

#[tokio::test]
async fn duplicate_name_in_another_workspace_cannot_adopt_or_create() {
    for existing_namespace in [true, false] {
        let mut initial = Store::default();
        let mut other = initial.sandboxes[0].clone();
        other["metadata"]["uid"] = json!("sandbox-b");
        other["metadata"]["namespace"] = json!("workspace-b");
        initial.sandboxes.push(other);
        if !existing_namespace {
            initial.namespace = None;
        }
        let sb = sandbox(&initial.sandboxes[0]);
        let (server, client, _) = setup(initial).await;
        assert!(ensure(&client, &sb).await.is_err());
        assert!(
            server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .all(|r| r.method == "GET")
        );
    }
}

#[tokio::test]
async fn new_namespace_creation_is_atomic_and_new_uid_never_adopts_old_claim() {
    let mut initial = Store {
        namespace: None,
        ..Store::default()
    };
    initial.sandboxes[0]["status"] = Value::Null;
    initial.sandboxes[0]["metadata"]["finalizers"] = Value::Null;
    let sb = sandbox(&initial.sandboxes[0]);
    let (server, client, store) = setup(initial).await;
    let (bound, ns) = ensure(&client, &sb).await.unwrap();
    assert_eq!(ns.unwrap().metadata.uid.as_deref(), Some("namespace-new"));
    assert_eq!(
        annotation(&bound.metadata, NAMESPACE_UID),
        Some("namespace-new")
    );
    let requests = server.received_requests().await.unwrap();
    assert!(
        requests
            .iter()
            .position(|r| r.method == "PATCH" && r.url.path() == CR_PATH)
            .unwrap()
            < requests.iter().position(|r| r.method == "POST").unwrap()
    );
    let recreated = {
        let mut store = store.lock().unwrap();
        store.sandboxes[0]["metadata"]["uid"] = json!("recreated");
        sandbox(&store.sandboxes[0])
    };
    assert!(ensure(&client, &recreated).await.is_err());
}

#[tokio::test]
async fn creation_409_never_force_adopts_a_winner() {
    let f = fixture();
    let initial = Store {
        namespace: None,
        race_create: Some(f["namespace"].clone()),
        ..Store::default()
    };
    let sb = sandbox(&initial.sandboxes[0]);
    let (server, client, store) = setup(initial).await;
    assert!(ensure(&client, &sb).await.is_err());
    assert_eq!(
        store.lock().unwrap().namespace.as_ref().unwrap(),
        &f["namespace"]
    );
    assert!(
        !server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .any(|r| r.method == "PATCH" && r.url.path() == NS_PATH)
    );
}

#[tokio::test]
async fn cas_retry_completes_a_partially_persisted_claim_without_overwrites() {
    let initial = Store {
        fail_cr_patch_once: true,
        ..Store::default()
    };
    let sb = sandbox(&initial.sandboxes[0]);
    let (_server, client, store) = setup(initial).await;
    assert!(ensure(&client, &sb).await.is_err());
    assert_eq!(
        store.lock().unwrap().namespace.as_ref().unwrap()["metadata"]["annotations"][SOURCE_UID],
        "sandbox-a"
    );
    let (bound, ns) = ensure(&client, &sb).await.unwrap();
    assert!(claimed(&ns.unwrap(), &bound).unwrap());
}

#[tokio::test]
async fn namespace_replacement_between_read_and_claim_fails_cas() {
    let initial = Store {
        replace_on_patch: true,
        ..Store::default()
    };
    let sb = sandbox(&initial.sandboxes[0]);
    let (_server, client, store) = setup(initial).await;
    assert!(ensure(&client, &sb).await.is_err());
    assert!(
        store.lock().unwrap().namespace.as_ref().unwrap()["metadata"]["annotations"]
            .get(SOURCE_UID)
            .is_none()
    );
}

#[tokio::test]
async fn cleanup_accepts_deletion_and_non404_errors_preserve_the_finalizer() {
    for error in [0, 403, 409, 500] {
        let mut initial = Store {
            fail_delete: error,
            ..Store::default()
        };
        initial.sandboxes[0]["metadata"]["deletionTimestamp"] = json!("2026-09-01T12:00:00Z");
        let sb = sandbox(&initial.sandboxes[0]);
        let (_server, client, store) = setup(initial).await;
        let (bound, ns) = ensure(&client, &sb).await.unwrap();
        let result = delete(&client, &bound, ns.as_ref()).await;
        if error == 0 {
            result.unwrap();
            let (live, gone) = ensure(&client, &bound).await.unwrap();
            delete(&client, &live, gone.as_ref()).await.unwrap();
        } else {
            assert!(result.is_err());
        }
        assert_eq!(
            store.lock().unwrap().sandboxes[0]["metadata"]["finalizers"],
            json!([FINALIZER])
        );
    }
}

#[tokio::test]
async fn read_errors_do_not_become_absence_or_trigger_creation() {
    for error in [403, 500] {
        let initial = Store {
            fail_namespace_get: error,
            ..Store::default()
        };
        let sb = sandbox(&initial.sandboxes[0]);
        let (server, client, _) = setup(initial).await;
        assert!(ensure(&client, &sb).await.is_err());
        assert!(
            server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .all(|r| r.method == "GET")
        );
    }
}

#[tokio::test]
async fn full_prestage_flow_binds_uid_atomically_and_preserves_existing_data() {
    let mut initial = Store::default();
    initial.deployment = None;
    initial.sandboxes[0]["status"] = Value::Null;
    initial.sandboxes[0]["metadata"]["annotations"] = json!({NAMESPACE_UID: "namespace-a"});
    let annotations = &mut initial.namespace.as_mut().unwrap()["metadata"]["annotations"];
    annotations[VERSION] = json!("v1");
    annotations[SOURCE_NAME] = json!("demo");
    annotations[SOURCE_NAMESPACE] = json!("workspace-a");
    annotations[PRESTAGE] = json!("bind-next-sandbox");
    let sb = sandbox(&initial.sandboxes[0]);
    let (_server, client, store) = setup(initial).await;
    let (live, ns) = ensure(&client, &sb).await.unwrap();
    assert!(claimed(&ns.unwrap(), &live).unwrap());
    let state = store.lock().unwrap();
    let annotations = &state.namespace.as_ref().unwrap()["metadata"]["annotations"];
    assert!(annotations.get(PRESTAGE).is_none());
    assert_eq!(annotations[SOURCE_UID], "sandbox-a");
    assert_eq!(annotations["customer-annotation"], "keep");
}

#[tokio::test]
async fn a_conflicting_new_cr_does_not_disable_the_already_claimed_owner() {
    let mut initial = Store::default();
    let sb = sandbox(&initial.sandboxes[0]);
    merge(
        initial.namespace.as_mut().unwrap(),
        json!({"metadata": {"annotations": owner_annotations(&sb)}}),
    );
    let mut other = initial.sandboxes[0].clone();
    other["metadata"]["uid"] = json!("sandbox-b");
    other["metadata"]["namespace"] = json!("workspace-b");
    initial.sandboxes.push(other);
    let (_server, client, _) = setup(initial).await;
    assert!(ensure(&client, &sb).await.is_ok());
}

#[tokio::test]
async fn an_existing_foreign_claim_is_rejected_before_mutation() {
    let mut initial = Store::default();
    let sb = sandbox(&initial.sandboxes[0]);
    let mut annotations = owner_annotations(&sb);
    annotations[SOURCE_NAMESPACE] = json!("workspace-b");
    annotations[SOURCE_UID] = json!("sandbox-b");
    merge(
        initial.namespace.as_mut().unwrap(),
        json!({"metadata": {"annotations": annotations}}),
    );
    let (server, client, _) = setup(initial).await;
    assert!(ensure(&client, &sb).await.is_err());
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|r| r.method == "GET")
    );
}

#[tokio::test]
async fn namespace_replacement_before_cleanup_is_never_deleted() {
    let mut initial = Store::default();
    initial.sandboxes[0]["metadata"]["deletionTimestamp"] = json!("2026-09-01T12:00:00Z");
    let sb = sandbox(&initial.sandboxes[0]);
    let (server, client, store) = setup(initial).await;
    let (bound, ns) = ensure(&client, &sb).await.unwrap();
    store.lock().unwrap().namespace.as_mut().unwrap()["metadata"]["uid"] = json!("replacement");
    assert!(delete(&client, &bound, ns.as_ref()).await.is_err());
    assert!(
        !server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .any(|r| r.method == "DELETE")
    );
}

#[tokio::test]
async fn stale_cr_uid_and_missing_bound_namespace_are_rejected() {
    let mut initial = Store::default();
    let old = sandbox(&initial.sandboxes[0]);
    initial.sandboxes[0]["metadata"]["uid"] = json!("new-incarnation");
    let (server, client, store) = setup(initial).await;
    assert!(ensure(&client, &old).await.is_err());
    let current = {
        let mut state = store.lock().unwrap();
        state.namespace = None;
        state.sandboxes[0]["metadata"]["annotations"] = json!({NAMESPACE_UID: "missing"});
        sandbox(&state.sandboxes[0])
    };
    assert!(ensure(&client, &current).await.is_err());
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|r| r.method == "GET")
    );
}

#[tokio::test]
async fn finalizer_removal_retries_cas_and_preserves_unrelated_finalizers() {
    let mut initial = Store {
        namespace: None,
        ..Store::default()
    };
    initial.sandboxes[0]["metadata"]["deletionTimestamp"] = json!("2026-09-01T12:00:00Z");
    initial.sandboxes[0]["metadata"]["finalizers"] = json!([FINALIZER, "customer.example/cleanup"]);
    let sb = sandbox(&initial.sandboxes[0]);
    let (_server, client, store) = setup(initial).await;
    store.lock().unwrap().fail_cr_patch_once = true;
    assert!(remove_finalizer(&client, &sb).await.is_err());
    assert_eq!(
        store.lock().unwrap().sandboxes[0]["metadata"]["finalizers"],
        json!([FINALIZER, "customer.example/cleanup"])
    );
    remove_finalizer(&client, &sb).await.unwrap();
    assert_eq!(
        store.lock().unwrap().sandboxes[0]["metadata"]["finalizers"],
        json!(["customer.example/cleanup"])
    );
}

#[tokio::test]
async fn conflicts_are_visible_without_a_status_hot_loop_or_target_mutations() {
    let initial = Store::default();
    let original_namespace = initial.namespace.clone();
    let sb = sandbox(&initial.sandboxes[0]);
    let (server, client, store) = setup(initial).await;
    report_conflict(&client, &sb, "ownership cannot be proven")
        .await
        .unwrap();
    report_conflict(&client, &sb, "ownership cannot be proven")
        .await
        .unwrap();
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.iter().filter(|r| r.method == "PATCH").count(), 1);
    let state = store.lock().unwrap();
    assert_eq!(state.namespace, original_namespace);
    assert_eq!(state.sandboxes[0]["status"]["phase"], "Degraded");
    assert!(
        state.sandboxes[0]["status"]["conditions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["type"] == "Ready" && c["status"] == "False")
    );
}

#[tokio::test]
async fn shared_target_gate_rejects_unclaimed_foreign_and_missing_source_owners() {
    let initial = Store::default();
    let sb = sandbox(&initial.sandboxes[0]);
    let (_server, client, store) = setup(initial).await;
    assert!(verify_target(&client, "workspace-a", "demo").await.is_err());
    ensure(&client, &sb).await.unwrap();
    assert!(verify_target(&client, "workspace-a", "demo").await.unwrap());
    store.lock().unwrap().sandboxes[0]["metadata"]["uid"] = json!("foreign-uid");
    assert!(verify_target(&client, "workspace-a", "demo").await.is_err());
    store.lock().unwrap().sandboxes.clear();
    assert!(verify_target(&client, "workspace-a", "demo").await.is_err());
    store.lock().unwrap().namespace = None;
    assert!(!verify_target(&client, "workspace-a", "demo").await.unwrap());
}

#[tokio::test]
async fn target_gate_rejects_path_shaped_names_before_api_access() {
    let (server, client, _) = setup(Store::default()).await;
    assert!(
        verify_target(&client, "workspace-a", "../other")
            .await
            .is_err()
    );
    assert!(
        verify_target(&client, "../workspace-b", "demo")
            .await
            .is_err()
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn token_reader_does_not_read_secrets_for_an_unclaimed_namespace() {
    let (server, client, _) = setup(Store::default()).await;
    let _lock = lock("demo").await;
    assert!(
        crate::status::router_confirmation_io::read_admin_token(&client, "workspace-a", "demo",)
            .await
            .is_err()
    );
    assert!(
        !server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .any(|request| request.url.path().contains("/secrets/"))
    );
}

#[tokio::test]
async fn self_hosted_cr_cleanup_does_not_wait_for_its_own_namespace_to_disappear() {
    let mut initial = Store::default();
    initial.sandboxes[0]["metadata"]["namespace"] = json!("kars-demo");
    initial.sandboxes[0]["metadata"]["deletionTimestamp"] = json!("2026-09-01T12:00:00Z");
    initial.namespace.as_mut().unwrap()["metadata"]["deletionTimestamp"] =
        json!("2026-09-01T12:00:01Z");
    let sb = sandbox(&initial.sandboxes[0]);
    merge(
        initial.namespace.as_mut().unwrap(),
        json!({"metadata": {"annotations": owner_annotations(&sb)}}),
    );
    let ns = namespace(initial.namespace.as_ref().unwrap());
    let (_server, client, store) = setup(initial).await;
    delete(&client, &sb, Some(&ns)).await.unwrap();
    assert!(store.lock().unwrap().namespace.is_some());
}
