// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::crd::KarsSandboxSpec;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Mutex;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

fn helm_metadata(name: &str, namespace: &str) -> ObjectMeta {
    serde_json::from_value(json!({"name": name, "namespace": namespace, "uid":format!("{name}-uid"),
        "resourceVersion":"1", "labels":{"kars.azure.com/witness-addon":"true", "app.kubernetes.io/managed-by":"Helm"},
        "annotations":{"meta.helm.sh/release-name":"kars", "meta.helm.sh/release-namespace": WORKSPACE}
    })).unwrap()
}

fn settings() -> ConfigMap {
    ConfigMap {
        metadata: helm_metadata(SETTINGS, WORKSPACE),
        data: Some(BTreeMap::from([(
            "settings.json".into(),
            json!({
                "schema_version":1,"enabled":true,"scope":"all","sandboxes":[],
                "release_revision":1,"config_digest":"a".repeat(64)
            })
            .to_string(),
        )])),
        ..Default::default()
    }
}

fn set_setting(cm: &mut ConfigMap, key: &str, value: Value) {
    let mut settings: Value =
        serde_json::from_str(&cm.data.as_ref().unwrap()["settings.json"]).unwrap();
    settings[key] = value;
    cm.data
        .as_mut()
        .unwrap()
        .insert("settings.json".into(), settings.to_string());
}

fn observer() -> ServiceAccount {
    ServiceAccount {
        metadata: helm_metadata(OBSERVER, OBSERVER_NAMESPACE),
        ..Default::default()
    }
}

fn target(name: &str) -> (KarsSandbox, Namespace) {
    let mut sandbox = KarsSandbox::new(name, KarsSandboxSpec::default());
    sandbox.metadata.namespace = Some(WORKSPACE.into());
    sandbox.metadata.uid = Some(format!("{name}-uid"));
    sandbox.metadata.resource_version = Some("1".into());
    sandbox.metadata.annotations = Some(BTreeMap::from([(
        ownership::NAMESPACE_UID.into(),
        format!("ns-{name}-uid"),
    )]));
    let namespace: Namespace = serde_json::from_value(json!({"metadata":{
        "name":format!("kars-{name}"), "uid":format!("ns-{name}-uid"), "resourceVersion":"1",
        "annotations":{ownership::VERSION:"v1", ownership::SOURCE_NAMESPACE:WORKSPACE,
            ownership::SOURCE_NAME:name, ownership::SOURCE_UID:sandbox.uid()}
    }}))
    .unwrap();
    (sandbox, namespace)
}

fn persisted(meta: &mut ObjectMeta, uid: &str) {
    meta.uid = Some(uid.into());
    meta.resource_version = Some("1".into());
    meta.managed_fields = Some(
        serde_json::from_value(json!([{
            "manager":MANAGER,"operation":"Update","apiVersion":"rbac.authorization.k8s.io/v1"
        }]))
        .unwrap(),
    );
}

#[test]
fn exact_named_get_only_and_namespace_owned_for_both_supported_helm_releases() {
    for release in ["kars", "kars-datapath-witness"] {
        let mut cm = settings();
        let mut sa = observer();
        for meta in [&mut cm.metadata, &mut sa.metadata] {
            meta.annotations
                .as_mut()
                .unwrap()
                .insert("meta.helm.sh/release-name".into(), release.into());
        }
        let intent = intent(Some(&cm), Some(&sa)).unwrap().unwrap();
        let (sandbox, namespace) = target("new-mission-worker");
        let mut role = desired_role(&sandbox, &namespace, &intent);
        assert_eq!(
            serde_json::to_value(&role.rules).unwrap(),
            json!([{
                "apiGroups":[""], "resources":["configmaps"],
                "resourceNames":["karssandbox-new-mission-worker-egress-allowlist"], "verbs":["get"]
            }])
        );
        persisted(&mut role.metadata, "role-uid");
        let binding = desired_binding(&sandbox, &namespace, &intent, &role).unwrap();
        assert_eq!(
            serde_json::to_value(binding.subjects).unwrap(),
            json!([{
                "kind":"ServiceAccount","name":"aggregator","namespace":"kars-witness-gadget"
            }])
        );
        assert_eq!(binding.role_ref.name, GRANT);
        assert_eq!(binding.role_ref.kind, "Role");
        assert_eq!(annotation(&binding.metadata, ROLE_UID), Some("role-uid"));
        assert!(owned(&role.metadata, &sandbox, &namespace));
        assert_eq!(role.metadata.owner_references.unwrap()[0].kind, "Namespace");
    }
}

#[test]
fn selected_disabled_absent_do_not_require_observer_and_never_grant() {
    assert_eq!(intent(None, None).unwrap(), None);
    let mut cm = settings();
    set_setting(&mut cm, "enabled", json!(false));
    assert_eq!(intent(Some(&cm), None).unwrap(), None);
    set_setting(&mut cm, "enabled", json!(true));
    set_setting(&mut cm, "scope", json!("selected"));
    set_setting(&mut cm, "sandboxes", json!(["demo"]));
    assert_eq!(intent(Some(&cm), Some(&observer())).unwrap(), None);
    assert_eq!(intent(Some(&settings()), None).unwrap(), None);
}

#[test]
fn malformed_settings_and_foreign_observer_fail_closed() {
    for (key, value) in [
        ("schema_version", json!(2)),
        ("release_revision", json!(0)),
        ("scope", json!("everyone")),
        ("sandboxes", json!(["demo"])),
        ("config_digest", json!("z".repeat(64))),
        ("config_digest", json!("abc")),
        ("enabled", json!("true")),
        ("extra_authority", json!(true)),
    ] {
        let mut cm = settings();
        set_setting(&mut cm, key, value);
        assert!(intent(Some(&cm), Some(&observer())).is_err(), "{key}");
    }
    for is_settings in [true, false] {
        for field in [
            "name",
            "namespace",
            "uid",
            "resourceVersion",
            "labels",
            "annotations",
            "ownerReferences",
            "deletionTimestamp",
        ] {
            let mut cm = settings();
            let mut sa = observer();
            let meta = if is_settings {
                &mut cm.metadata
            } else {
                &mut sa.metadata
            };
            let mut value = serde_json::to_value(&*meta).unwrap();
            value[field] = match field {
                "ownerReferences" => {
                    json!([{"apiVersion":"v1","kind":"ConfigMap","name":"foreign","uid":"foreign"}])
                }
                "deletionTimestamp" => json!("2026-10-25T10:00:00Z"),
                _ => Value::Null,
            };
            *meta = serde_json::from_value(value).unwrap();
            assert!(
                intent(Some(&cm), Some(&sa)).is_err(),
                "{is_settings}/{field}"
            );
        }
    }
    let mut sa = observer();
    sa.metadata.annotations.as_mut().unwrap().insert(
        "meta.helm.sh/release-name".into(),
        "kars-datapath-witness".into(),
    );
    assert!(intent(Some(&settings()), Some(&sa)).is_err());
}

#[test]
fn ownership_and_delete_preconditions_bind_exact_sandbox_and_namespace_incarnations() {
    let (sandbox, namespace) = target("demo");
    let intent = intent(Some(&settings()), Some(&observer()))
        .unwrap()
        .unwrap();
    let mut role = desired_role(&sandbox, &namespace, &intent);
    persisted(&mut role.metadata, "role-uid");
    assert!(owned(&role.metadata, &sandbox, &namespace));
    let params = delete_params(&role.metadata).unwrap();
    assert_eq!(
        params.preconditions.as_ref().unwrap().uid.as_deref(),
        Some("role-uid")
    );
    assert_eq!(
        params
            .preconditions
            .as_ref()
            .unwrap()
            .resource_version
            .as_deref(),
        Some("1")
    );
    for pointer in [
        "/name",
        "/namespace",
        "/uid",
        "/resourceVersion",
        "/labels",
        "/annotations",
        "/ownerReferences",
        "/managedFields",
    ] {
        let mut value = serde_json::to_value(&role.metadata).unwrap();
        *value.pointer_mut(pointer).unwrap() = Value::Null;
        assert!(
            !owned(
                &serde_json::from_value(value).unwrap(),
                &sandbox,
                &namespace
            ),
            "{pointer}"
        );
    }
    let mut replaced = sandbox.clone();
    replaced.metadata.uid = Some("replaced".into());
    assert!(!owned(&role.metadata, &replaced, &namespace));
    let mut replaced = namespace.clone();
    replaced.metadata.uid = Some("replaced".into());
    assert!(!owned(&role.metadata, &sandbox, &replaced));
}

#[test]
fn watch_routes_corruption_and_deletion_without_cross_workspace_authority() {
    let (sandbox, _) = target("demo");
    let mut foreign = sandbox.clone();
    foreign.metadata.namespace = Some("another-workspace".into());
    let sandboxes = vec![Arc::new(sandbox), Arc::new(foreign)];
    let metadata = ObjectMeta {
        name: Some(SETTINGS.into()),
        namespace: Some(WORKSPACE.into()),
        ..Default::default()
    };
    assert_eq!(
        affected_sandboxes(&metadata, SETTINGS, WORKSPACE, &sandboxes),
        vec![ObjectRef::from_obj(sandboxes[0].as_ref())]
    );
    assert!(affected_sandboxes(&metadata, OBSERVER, OBSERVER_NAMESPACE, &sandboxes).is_empty());
    let sa = ObjectMeta {
        name: Some(OBSERVER.into()),
        namespace: Some(OBSERVER_NAMESPACE.into()),
        ..Default::default()
    };
    assert_eq!(
        affected_sandboxes(&sa, OBSERVER, OBSERVER_NAMESPACE, &sandboxes).len(),
        1
    );
}

const SETTINGS_PATH: &str =
    "/api/v1/namespaces/kars-system/configmaps/kars-datapath-witness-settings";
const SA_PATH: &str = "/api/v1/namespaces/kars-witness-gadget/serviceaccounts/aggregator";

fn role_path(namespace: &Namespace) -> String {
    format!(
        "/apis/rbac.authorization.k8s.io/v1/namespaces/{}/roles/{}",
        namespace.name_any(),
        GRANT
    )
}
fn binding_path(namespace: &Namespace) -> String {
    format!(
        "/apis/rbac.authorization.k8s.io/v1/namespaces/{}/rolebindings/{}",
        namespace.name_any(),
        GRANT
    )
}

#[derive(Default)]
struct Store {
    objects: BTreeMap<String, Value>,
    requests: Vec<(String, String, Value)>,
    sequence: usize,
    denied_reads: Vec<String>,
    deny_delete: Option<String>,
}

#[derive(Clone)]
struct Server(Arc<Mutex<Store>>);

fn status(code: u16) -> ResponseTemplate {
    ResponseTemplate::new(code).set_body_json(json!({"kind":"Status","apiVersion":"v1","status":"Failure",
        "message":"test API failure","reason":if code == 404 {"NotFound"} else {"Conflict"},"code":code}))
}

impl Respond for Server {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let mut store = self.0.lock().unwrap();
        let path = request.url.path().to_owned();
        let method = request.method.as_str();
        let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
        store
            .requests
            .push((method.into(), path.clone(), body.clone()));
        match method {
            "GET" => {
                if store.denied_reads.contains(&path) {
                    return status(403);
                }
                store
                    .objects
                    .get(&path)
                    .map(|value| ResponseTemplate::new(200).set_body_json(value))
                    .unwrap_or_else(|| status(404))
            }
            "POST" => {
                assert!(
                    path.ends_with("/roles") || path.ends_with("/rolebindings"),
                    "unexpected write: {path}"
                );
                assert!(
                    request
                        .url
                        .query_pairs()
                        .any(|(k, v)| k == "fieldManager" && v == MANAGER)
                );
                let key = format!("{path}/{}", body["metadata"]["name"].as_str().unwrap());
                if store.objects.contains_key(&key) {
                    return status(409);
                }
                store.sequence += 1;
                let mut value = body;
                let mut meta: ObjectMeta =
                    serde_json::from_value(value["metadata"].clone()).unwrap();
                persisted(&mut meta, &format!("created-{}", store.sequence));
                value["metadata"] = serde_json::to_value(meta).unwrap();
                store.objects.insert(key, value.clone());
                ResponseTemplate::new(201).set_body_json(value)
            }
            "DELETE" => {
                if store.deny_delete.as_deref() == Some(&path) {
                    return status(409);
                }
                let Some(existing) = store.objects.get(&path) else {
                    return status(404);
                };
                assert_eq!(body["preconditions"]["uid"], existing["metadata"]["uid"]);
                assert_eq!(
                    body["preconditions"]["resourceVersion"],
                    existing["metadata"]["resourceVersion"]
                );
                store.objects.remove(&path);
                ResponseTemplate::new(200)
                    .set_body_json(json!({"kind":"Status","apiVersion":"v1","status":"Success"}))
            }
            _ => panic!("unexpected API operation: {method} {path}"),
        }
    }
}

async fn setup(
    name: &str,
) -> (
    MockServer,
    Client,
    Arc<Mutex<Store>>,
    KarsSandbox,
    Namespace,
) {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let (sandbox, namespace) = target(name);
    let mut store = Store::default();
    for (path, value) in [
        (
            SETTINGS_PATH.into(),
            serde_json::to_value(settings()).unwrap(),
        ),
        (SA_PATH.into(), serde_json::to_value(observer()).unwrap()),
        (
            format!("/api/v1/namespaces/{}", namespace.name_any()),
            serde_json::to_value(&namespace).unwrap(),
        ),
        (
            format!("/apis/kars.azure.com/v1alpha1/namespaces/{WORKSPACE}/karssandboxes/{name}"),
            serde_json::to_value(&sandbox).unwrap(),
        ),
    ] {
        store.objects.insert(path, value);
    }
    let store = Arc::new(Mutex::new(store));
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(Server(store.clone()))
        .mount(&server)
        .await;
    let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
    (server, client, store, sandbox, namespace)
}

#[tokio::test]
async fn new_team_worker_granted_once_then_revoke_on_disable_without_observer_dependency() {
    let (_server, client, store, sandbox, ns) = setup("new-team-worker").await;
    reconcile(&client, &sandbox, &ns).await.unwrap();
    reconcile(&client, &sandbox, &ns).await.unwrap();
    {
        let mut store = store.lock().unwrap();
        assert_eq!(store.requests.iter().filter(|r| r.0 == "POST").count(), 2);
        let mut cm = settings();
        set_setting(&mut cm, "enabled", json!(false));
        store
            .objects
            .insert(SETTINGS_PATH.into(), serde_json::to_value(cm).unwrap());
        store.objects.remove(SA_PATH);
        store.requests.clear();
    }
    reconcile(&client, &sandbox, &ns).await.unwrap();
    let store = store.lock().unwrap();
    assert!(!store.objects.contains_key(&role_path(&ns)));
    assert!(!store.objects.contains_key(&binding_path(&ns)));
    assert_eq!(store.requests.iter().filter(|r| r.0 == "DELETE").count(), 2);
    assert!(!store.requests.iter().any(|r| r.1 == SA_PATH));
}

#[tokio::test]
async fn absent_selected_or_foreign_addon_never_creates_and_revokes_previous_grants() {
    for mode in [
        "absent",
        "selected",
        "missing-observer",
        "foreign-observer",
        "malformed",
        "read-failure",
    ] {
        let (_server, client, store, sandbox, ns) = setup("demo").await;
        reconcile(&client, &sandbox, &ns).await.unwrap();
        {
            let mut store = store.lock().unwrap();
            match mode {
                "absent" => {
                    store.objects.remove(SETTINGS_PATH);
                }
                "selected" => {
                    let mut cm = settings();
                    set_setting(&mut cm, "scope", json!("selected"));
                    store
                        .objects
                        .insert(SETTINGS_PATH.into(), serde_json::to_value(cm).unwrap());
                }
                "missing-observer" => {
                    store.objects.remove(SA_PATH);
                }
                "foreign-observer" => {
                    store.objects.get_mut(SA_PATH).unwrap()["metadata"]["labels"] = Value::Null;
                }
                "malformed" => {
                    store.objects.get_mut(SETTINGS_PATH).unwrap()["data"]["settings.json"] =
                        json!("invalid JSON");
                }
                "read-failure" => store.denied_reads.push(SETTINGS_PATH.into()),
                _ => unreachable!(),
            }
            store.requests.clear();
        }
        let result = reconcile(&client, &sandbox, &ns).await;
        assert_eq!(
            result.is_err(),
            matches!(mode, "foreign-observer" | "malformed" | "read-failure"),
            "{mode}"
        );
        let store = store.lock().unwrap();
        assert!(!store.objects.contains_key(&role_path(&ns)), "{mode}");
        assert!(!store.objects.contains_key(&binding_path(&ns)), "{mode}");
        assert!(!store.requests.iter().any(|r| r.0 == "POST"), "{mode}");
    }
}

#[tokio::test]
async fn replacement_observer_or_settings_revokes_before_fresh_authorized_creation() {
    for path in [SA_PATH, SETTINGS_PATH] {
        let (_server, client, store, sandbox, ns) = setup("demo").await;
        reconcile(&client, &sandbox, &ns).await.unwrap();
        store.lock().unwrap().objects.get_mut(path).unwrap()["metadata"]["uid"] =
            json!("replaced-uid");
        assert!(reconcile(&client, &sandbox, &ns).await.is_err());
        {
            let store = store.lock().unwrap();
            assert!(!store.objects.contains_key(&role_path(&ns)));
            assert!(!store.objects.contains_key(&binding_path(&ns)));
        }
        reconcile(&client, &sandbox, &ns).await.unwrap();
        let store = store.lock().unwrap();
        let key = if path == SA_PATH {
            OBSERVER_UID
        } else {
            SETTINGS_UID
        };
        assert_eq!(
            store.objects[&binding_path(&ns)]["metadata"]["annotations"][key],
            "replaced-uid"
        );
    }
}

#[tokio::test]
async fn foreign_half_is_never_adopted_or_deleted_and_owned_half_is_revoked() {
    for foreign_role in [true, false] {
        let (_server, client, store, sandbox, ns) = setup("demo").await;
        reconcile(&client, &sandbox, &ns).await.unwrap();
        let foreign = if foreign_role {
            role_path(&ns)
        } else {
            binding_path(&ns)
        };
        let counterpart = if foreign_role {
            binding_path(&ns)
        } else {
            role_path(&ns)
        };
        let original = {
            let mut store = store.lock().unwrap();
            let object = store.objects.get_mut(&foreign).unwrap();
            object["metadata"]["uid"] = json!("foreign-uid");
            object["metadata"]["managedFields"] = Value::Null;
            let original = object.clone();
            store.requests.clear();
            original
        };
        assert!(reconcile(&client, &sandbox, &ns).await.is_err());
        let store = store.lock().unwrap();
        assert_eq!(store.objects[&foreign], original);
        assert!(!store.objects.contains_key(&counterpart));
        assert!(
            !store
                .requests
                .iter()
                .any(|r| r.0 == "POST" || r.0 == "PATCH" || (r.0 == "DELETE" && r.1 == foreign))
        );
    }
}

#[tokio::test]
async fn replaced_role_uid_or_widened_rules_revokes_both_owned_halves() {
    for replacement in [true, false] {
        let (_server, client, store, sandbox, ns) = setup("demo").await;
        reconcile(&client, &sandbox, &ns).await.unwrap();
        {
            let mut store = store.lock().unwrap();
            let role = store.objects.get_mut(&role_path(&ns)).unwrap();
            if replacement {
                role["metadata"]["uid"] = json!("replaced-role");
            } else {
                role["rules"][0]["resources"] = json!(["secrets"]);
            }
        }
        assert!(reconcile(&client, &sandbox, &ns).await.is_err());
        let store = store.lock().unwrap();
        assert!(!store.objects.contains_key(&role_path(&ns)));
        assert!(!store.objects.contains_key(&binding_path(&ns)));
    }
}

#[tokio::test]
async fn grant_read_failure_revokes_the_readable_owned_counterpart() {
    for deny_role in [true, false] {
        let (_server, client, store, sandbox, ns) = setup("demo").await;
        reconcile(&client, &sandbox, &ns).await.unwrap();
        let denied = if deny_role {
            role_path(&ns)
        } else {
            binding_path(&ns)
        };
        let counterpart = if deny_role {
            binding_path(&ns)
        } else {
            role_path(&ns)
        };
        store.lock().unwrap().denied_reads.push(denied.clone());
        assert!(reconcile(&client, &sandbox, &ns).await.is_err());
        let store = store.lock().unwrap();
        assert!(store.objects.contains_key(&denied));
        assert!(!store.objects.contains_key(&counterpart));
    }
}

#[tokio::test]
async fn terminating_sandbox_revokes_and_foreign_workspace_never_receives_grants() {
    let (_server, client, store, mut sandbox, ns) = setup("demo").await;
    reconcile(&client, &sandbox, &ns).await.unwrap();
    sandbox.metadata.deletion_timestamp =
        Some(serde_json::from_value(json!("2026-10-25T10:00:00Z")).unwrap());
    reconcile(&client, &sandbox, &ns).await.unwrap();
    assert!(
        !store
            .lock()
            .unwrap()
            .objects
            .contains_key(&binding_path(&ns))
    );

    let (_server, client, store, mut sandbox, mut ns) = setup("other-workspace").await;
    sandbox.metadata.namespace = Some("tenant-b".into());
    ns.metadata
        .annotations
        .as_mut()
        .unwrap()
        .insert(ownership::SOURCE_NAMESPACE.into(), "tenant-b".into());
    store.lock().unwrap().objects.insert(
        format!("/api/v1/namespaces/{}", ns.name_any()),
        serde_json::to_value(&ns).unwrap(),
    );
    reconcile(&client, &sandbox, &ns).await.unwrap();
    let store = store.lock().unwrap();
    assert!(
        !store
            .requests
            .iter()
            .any(|r| r.0 == "POST" || r.1 == SETTINGS_PATH || r.1 == SA_PATH)
    );
}

#[tokio::test]
async fn binding_delete_conflict_still_revokes_owned_role_and_reports_failure() {
    let (_server, client, store, sandbox, ns) = setup("demo").await;
    reconcile(&client, &sandbox, &ns).await.unwrap();
    {
        let mut store = store.lock().unwrap();
        store.objects.remove(SETTINGS_PATH);
        store.deny_delete = Some(binding_path(&ns));
    }
    assert!(reconcile(&client, &sandbox, &ns).await.is_err());
    let store = store.lock().unwrap();
    assert!(!store.objects.contains_key(&role_path(&ns)));
    assert!(store.objects.contains_key(&binding_path(&ns)));
}
