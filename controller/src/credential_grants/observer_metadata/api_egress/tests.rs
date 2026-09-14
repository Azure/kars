// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use wiremock::{Mock, MockServer, ResponseTemplate};

const NS: &str = "/api/v1/namespaces/kars-agent";
const GRANT: &str = "/apis/kars.azure.com/v1alpha1/namespaces/work/karscredentialgrants/workspace";
const API: &str = "/apis/cilium.io/v2";
const POLICIES: &str = "/apis/cilium.io/v2/namespaces/kars-agent/ciliumnetworkpolicies";
const POLICY: &str =
    "/apis/cilium.io/v2/namespaces/kars-agent/ciliumnetworkpolicies/observer-g1-api";
const SERVICE: &str = "/api/v1/namespaces/default/services/kubernetes";
const ENDPOINTS: &str = "/api/v1/namespaces/default/endpoints/kubernetes";

#[derive(Default)]
struct State {
    objects: BTreeMap<String, Value>,
    calls: Vec<(String, String, Value)>,
    errors: BTreeMap<String, u16>,
    delete_conflict: bool,
    replace_conflict: bool,
    retain_deleted: bool,
    namespace_replacement_on_policy_read: Option<String>,
}

fn failure(code: u16) -> ResponseTemplate {
    ResponseTemplate::new(code).set_body_json(json!({
        "apiVersion":"v1","kind":"Status","status":"Failure","code":code,
        "reason":if code == 404 {"NotFound"} else {"Forbidden"},
        "message":"private-api-error-canary"
    }))
}

async fn fixture() -> (
    MockServer,
    Client,
    Arc<Mutex<State>>,
    KarsCredentialGrant,
    KarsSandbox,
    Namespace,
) {
    let server = MockServer::start().await;
    let grant: KarsCredentialGrant = serde_json::from_value(json!({
        "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsCredentialGrant",
        "metadata":{"name":"workspace","namespace":"work","uid":"grant-uid","resourceVersion":"1","generation":1},
        "spec":{"enabled":true,"workspaceUid":"workspace-uid","writers":[],
            "observationTargets":[{"kind":"KarsSandbox","namespace":"work","name":"agent","uid":"sandbox-uid"}]}
    })).unwrap();
    let legacy: Value = serde_json::from_str(include_str!(
        "../../../../../tests/compat/fixtures/namespace-legacy.json"
    ))
    .unwrap();
    let mut sandbox = legacy["sandbox"].clone();
    sandbox["metadata"] = json!({"name":"agent","namespace":"work","uid":"sandbox-uid","resourceVersion":"1",
        "annotations":{claim::NAMESPACE_UID:"runtime-uid"}});
    let sandbox: KarsSandbox = serde_json::from_value(sandbox).unwrap();
    let namespace: Namespace = serde_json::from_value(json!({
        "apiVersion":"v1","kind":"Namespace","metadata":{"name":"kars-agent","uid":"runtime-uid",
            "resourceVersion":"1","annotations":{claim::VERSION:"v1",claim::SOURCE_NAMESPACE:"work",
                claim::SOURCE_NAME:"agent",claim::SOURCE_UID:"sandbox-uid"}}}))
    .unwrap();
    let mut data = State::default();
    data.objects
        .insert(GRANT.into(), serde_json::to_value(&grant).unwrap());
    data.objects
        .insert(NS.into(), serde_json::to_value(&namespace).unwrap());
    data.objects.insert(
        "/api/v1/namespaces/work".into(),
        json!({"apiVersion":"v1","kind":"Namespace",
        "metadata":{"name":"work","uid":"workspace-uid","resourceVersion":"1"}}),
    );
    data.objects.insert(SERVICE.into(), json!({"apiVersion":"v1","kind":"Service",
        "metadata":{"name":"kubernetes","namespace":"default","uid":"service-uid","resourceVersion":"1"},
        "spec":{"clusterIP":"10.96.0.1","ports":[{"name":"https","port":443,"protocol":"TCP"}]}}));
    data.objects.insert(ENDPOINTS.into(), json!({"apiVersion":"v1","kind":"Endpoints",
        "metadata":{"name":"kubernetes","namespace":"default","uid":"endpoint-uid","resourceVersion":"1"},
        "subsets":[{"addresses":[{"ip":"172.18.0.3"}],"notReadyAddresses":[{"ip":"172.18.0.99"}],
            "ports":[{"name":"https","port":6443,"protocol":"TCP"}]}]}));
    data.objects.insert(API.into(), json!({"apiVersion":"v1","kind":"APIResourceList",
        "groupVersion":"cilium.io/v2","resources":[{"name":"ciliumnetworkpolicies","singularName":"",
            "namespaced":true,"kind":"CiliumNetworkPolicy","verbs":["get","list","create","update","delete"]}]}));
    let state = Arc::new(Mutex::new(data));
    let captured = state.clone();
    Mock::given(|_: &wiremock::Request| true).respond_with(move |request: &wiremock::Request| {
        let mut state = captured.lock().unwrap();
        let path = request.url.path();
        let body: Value = request.body_json().unwrap_or(Value::Null);
        state.calls.push((request.method.to_string(), path.into(), body.clone()));
        if let Some(code) = state.errors.get(path) {
            return failure(*code);
        }
        if request.method == "GET" {
            if state.namespace_replacement_on_policy_read.as_deref() == Some(path) {
                state.objects.get_mut(NS).unwrap()["metadata"]["uid"] = "replacement".into();
            }
            if let Some(value) = state.objects.get(path) {
                return ResponseTemplate::new(200).set_body_json(value);
            }
            for (suffix, kind, version) in [
                ("/namespaces", "Namespace", "v1"),
                ("/ciliumnetworkpolicies", "CiliumNetworkPolicy", "cilium.io/v2"),
                ("/networkpolicies", "NetworkPolicy", "networking.k8s.io/v1"),
                ("/roles", "Role", "rbac.authorization.k8s.io/v1"),
                ("/rolebindings", "RoleBinding", "rbac.authorization.k8s.io/v1"),
                ("/clusterroles", "ClusterRole", "rbac.authorization.k8s.io/v1"),
                ("/clusterrolebindings", "ClusterRoleBinding", "rbac.authorization.k8s.io/v1"),
            ] {
                if path.ends_with(suffix) {
                    let selector = request.url.query_pairs().find(|(key, _)| key == "labelSelector")
                        .map(|(_, value)| value.into_owned());
                    let items: Vec<_> = state.objects.iter().filter(|(key, object)| {
                        object["kind"] == kind
                            && (!path.contains("/namespaces/") || key.starts_with(&format!("{path}/")))
                            && selector.as_ref().is_none_or(|selector| {
                                let (key, value) = selector.split_once('=').unwrap();
                                object["metadata"]["labels"][key] == value
                            })
                    }).map(|(_, object)|object.clone()).collect();
                    return ResponseTemplate::new(200).set_body_json(json!({
                        "apiVersion":version,"kind":format!("{kind}List"),"metadata":{},"items":items
                    }));
                }
            }
        }
        if request.method == "PATCH" && path == NS {
            let current = state.objects.get_mut(path).unwrap();
            assert_eq!(current["metadata"]["uid"], body["metadata"]["uid"]);
            assert_eq!(current["metadata"]["resourceVersion"], body["metadata"]["resourceVersion"]);
            let labels = current["metadata"].as_object_mut().unwrap()
                .entry("labels").or_insert_with(||json!({})).as_object_mut().unwrap();
            for (key, value) in body["metadata"]["labels"].as_object().unwrap() {
                labels.insert(key.clone(), value.clone());
            }
            current["metadata"]["resourceVersion"] = "2".into();
            return ResponseTemplate::new(200).set_body_json(current.clone());
        }
        if request.method == "POST" {
            let name = body["metadata"]["name"].as_str().unwrap();
            let key = format!("{path}/{name}");
            if state.objects.contains_key(&key) { return failure(409); }
            let mut created = body;
            created["metadata"]["uid"] = "policy-uid".into();
            created["metadata"]["resourceVersion"] = "10".into();
            state.objects.insert(key, created.clone());
            return ResponseTemplate::new(201).set_body_json(created);
        }
        if request.method == "PUT" && state.objects.contains_key(path) {
            let current = &state.objects[path];
            assert_eq!(current["metadata"]["uid"], body["metadata"]["uid"]);
            assert_eq!(current["metadata"]["resourceVersion"], body["metadata"]["resourceVersion"]);
            if state.replace_conflict { return failure(409); }
            let mut replaced = body;
            replaced["metadata"]["resourceVersion"] = "11".into();
            state.objects.insert(path.into(), replaced.clone());
            return ResponseTemplate::new(200).set_body_json(replaced);
        }
        if request.method == "DELETE" && state.objects.contains_key(path) {
            let current = &state.objects[path];
            assert_eq!(current["metadata"]["uid"], body["preconditions"]["uid"]);
            assert_eq!(current["metadata"]["resourceVersion"], body["preconditions"]["resourceVersion"]);
            if state.delete_conflict { return failure(409); }
            if !state.retain_deleted { state.objects.remove(path); }
            return ResponseTemplate::new(200).set_body_json(json!({
                "apiVersion":"v1","kind":"Status","status":"Success","code":200
            }));
        }
        failure(404)
    }).mount(&server).await;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
    (server, client, state, grant, sandbox, namespace)
}

fn mutations(state: &Arc<Mutex<State>>) -> Vec<(String, String, Value)> {
    state
        .lock()
        .unwrap()
        .calls
        .iter()
        .filter(|(method, _, _)| method != "GET")
        .cloned()
        .collect()
}

#[tokio::test]
async fn observer_api_canonical_wire_targets_and_cilium_policy_are_exact() {
    let (_server, client, state, grant, sandbox, namespace) = fixture().await;
    let plan = plan(&client, "10.96.0.1", "443").await.unwrap();
    assert_eq!(
        plan.rules,
        vec![
            json!({"to":[{"ipBlock":{"cidr":"10.96.0.1/32"}}],"ports":[{"protocol":"TCP","port":443}]}),
            json!({"to":[{"ipBlock":{"cidr":"172.18.0.3/32"}}],"ports":[{"protocol":"TCP","port":6443}]})
        ]
    );
    ensure(&client, &grant, &sandbox, &namespace, "observer-g1", &plan)
        .await
        .unwrap();
    let object = state.lock().unwrap().objects[POLICY].clone();
    assert_eq!(
        object["spec"],
        json!({
            "endpointSelector":{"matchLabels":{"k8s:kars.azure.com/sandbox":"agent",
                "k8s:io.kubernetes.pod.namespace":"kars-agent"}},
            "egress":[{"toEntities":["kube-apiserver"],"toPorts":[{"ports":[
                {"port":"443","protocol":"TCP"},{"port":"6443","protocol":"TCP"}]}]}]
        })
    );
    assert_eq!(
        object["metadata"]["annotations"][NAMESPACE_UID],
        "runtime-uid"
    );
    assert_eq!(object["metadata"]["annotations"][GRANT_OWNER], "grant-uid");
    assert_eq!(
        object["metadata"]["annotations"][claim::SOURCE_UID],
        "sandbox-uid"
    );
    assert!(object.get("specs").is_none());
    assert!(object["spec"].get("ingress").is_none());
    assert!(!object.to_string().contains("pod-template-hash"));
    assert!(
        state
            .lock()
            .unwrap()
            .calls
            .iter()
            .all(|(_, path, _)| !path.contains("ciliumclusterwide")
                && path != "/apis/cilium.io/v2/ciliumnetworkpolicies")
    );
}

#[tokio::test]
async fn observer_api_absent_cilium_keeps_portable_rules_without_optional_writes() {
    let (_server, client, state, grant, sandbox, namespace) = fixture().await;
    state.lock().unwrap().errors.insert(API.into(), 404);
    let plan = plan(&client, "10.96.0.1", "443").await.unwrap();
    assert!(!plan.cilium);
    assert_eq!(plan.rules.len(), 2);
    ensure(&client, &grant, &sandbox, &namespace, "observer-g1", &plan)
        .await
        .unwrap();
    assert!(mutations(&state).is_empty());
    let rules = json!({"spec":{"podSelector":{"matchLabels":{"kars.azure.com/sandbox":"agent"}},
        "policyTypes":["Egress"],"egress":plan.rules}});
    apply_runtime(
        &client,
        &grant,
        &namespace,
        "NetworkPolicy",
        "portable",
        rules.clone(),
    )
    .await
    .unwrap();
    let object = state.lock().unwrap().objects[
        "/apis/networking.k8s.io/v1/namespaces/kars-agent/networkpolicies/portable"].clone();
    assert_eq!(object["spec"], rules["spec"]);
    assert!(
        state.lock().unwrap().objects[NS]["metadata"]["labels"]
            .get(INDEX)
            .is_none()
    );
}

#[tokio::test]
async fn observer_api_discovery_errors_and_malformed_contracts_never_mean_absent() {
    for code in [401, 403, 429, 500, 503] {
        let (_server, client, state, _, _, _) = fixture().await;
        state.lock().unwrap().errors.insert(API.into(), code);
        let error = plan(&client, "10.96.0.1", "443").await.unwrap_err();
        assert!(error.contains(&code.to_string()));
        assert!(!error.contains("canary"));
        assert!(mutations(&state).is_empty());
    }
    for (pointer, value) in [
        ("/groupVersion", json!("foreign/v2")),
        ("/resources", json!([])),
        ("/resources/0/namespaced", json!(false)),
        ("/resources/0/kind", json!("CiliumClusterwideNetworkPolicy")),
        ("/resources/0/verbs", json!(["get", "list"])),
        ("/resources/0/verbs", json!("private-malformed-canary")),
    ] {
        let (_server, client, state, _, _, _) = fixture().await;
        *state
            .lock()
            .unwrap()
            .objects
            .get_mut(API)
            .unwrap()
            .pointer_mut(pointer)
            .unwrap() = value;
        assert!(
            plan(&client, "10.96.0.1", "443").await.is_err(),
            "{pointer}"
        );
        assert!(mutations(&state).is_empty());
    }
}

#[tokio::test]
async fn observer_api_reuses_canonical_refusals_before_cilium_discovery() {
    for (path, pointer, value) in [
        (SERVICE, "/spec/clusterIP", json!("10.96.0.2")),
        (SERVICE, "/metadata/uid", Value::Null),
        (SERVICE, "/metadata/namespace", json!("foreign")),
        (ENDPOINTS, "/subsets/0/addresses", json!([])),
        (
            ENDPOINTS,
            "/subsets/0/addresses/0/ip",
            json!("169.254.169.254"),
        ),
        (ENDPOINTS, "/subsets/0/ports/0/port", json!(0)),
        (ENDPOINTS, "/subsets/0/ports/0/protocol", json!("UDP")),
    ] {
        let (_server, client, state, _, _, _) = fixture().await;
        *state
            .lock()
            .unwrap()
            .objects
            .get_mut(path)
            .unwrap()
            .pointer_mut(pointer)
            .unwrap() = value;
        assert!(
            plan(&client, "10.96.0.1", "443").await.is_err(),
            "{pointer}"
        );
        assert!(
            !state
                .lock()
                .unwrap()
                .calls
                .iter()
                .any(|(_, path, _)| path == API)
        );
        assert!(mutations(&state).is_empty());
    }
}

#[tokio::test]
async fn observer_api_ipv6_wire_targets_are_host_routes_not_subnets() {
    let (_server, client, state, _, _, _) = fixture().await;
    {
        let mut state = state.lock().unwrap();
        state.objects.get_mut(SERVICE).unwrap()["spec"]["clusterIP"] = "fd00::1".into();
        state.objects.get_mut(ENDPOINTS).unwrap()["subsets"][0]["addresses"] =
            json!([{"ip":"fd01::3"}]);
    }
    let plan = plan(&client, "fd00::1", "443").await.unwrap();
    assert_eq!(plan.rules[0]["to"][0]["ipBlock"]["cidr"], "fd00::1/128");
    assert_eq!(plan.rules[1]["to"][0]["ipBlock"]["cidr"], "fd01::3/128");
}

#[tokio::test]
async fn observer_api_updates_replace_extensions_under_uid_rv_and_then_are_idempotent() {
    let (_server, client, state, grant, sandbox, namespace) = fixture().await;
    let plan = plan(&client, "10.96.0.1", "443").await.unwrap();
    ensure(&client, &grant, &sandbox, &namespace, "observer-g1", &plan)
        .await
        .unwrap();
    {
        let mut state = state.lock().unwrap();
        let object = state.objects.get_mut(POLICY).unwrap();
        object["specs"] = json!([{"endpointSelector":{},"egress":[{}]}]);
        object["spec"]["endpointSelector"] = json!({});
        object["spec"]["ingress"] = json!([{}]);
        object["metadata"]["annotations"]["private-extension"] = "private-canary".into();
        state.calls.clear();
    }
    ensure(&client, &grant, &sandbox, &namespace, "observer-g1", &plan)
        .await
        .unwrap();
    assert_eq!(mutations(&state).len(), 1);
    assert_eq!(mutations(&state)[0].0, "PUT");
    assert_eq!(
        state.lock().unwrap().objects[POLICY]["metadata"]["uid"],
        "policy-uid"
    );
    assert!(state.lock().unwrap().objects[POLICY].get("specs").is_none());
    assert!(
        state.lock().unwrap().objects[POLICY]["spec"]
            .get("ingress")
            .is_none()
    );
    state.lock().unwrap().calls.clear();
    ensure(&client, &grant, &sandbox, &namespace, "observer-g1", &plan)
        .await
        .unwrap();
    assert!(mutations(&state).is_empty());
}

#[tokio::test]
async fn observer_api_foreign_grant_namespace_owner_target_and_finalizers_are_preserved() {
    for (pointer, value) in [
        (
            format!("/metadata/annotations/{GRANT_OWNER}")
                .replace("kars.azure.com/", "kars.azure.com~1"),
            json!("foreign"),
        ),
        (
            format!("/metadata/annotations/{NAMESPACE_UID}")
                .replace("kars.azure.com/", "kars.azure.com~1"),
            json!("foreign"),
        ),
        (
            format!("/metadata/annotations/{}", claim::SOURCE_UID)
                .replace("kars.azure.com/", "kars.azure.com~1"),
            json!("foreign"),
        ),
        ("/metadata/ownerReferences/0/uid".into(), json!("foreign")),
        (
            "/metadata/labels/kars.azure.com~1observer-metadata-grant".into(),
            json!("foreign"),
        ),
        ("/metadata/uid".into(), Value::Null),
        ("/metadata/finalizers".into(), json!(["foreign"])),
    ] {
        let (_server, client, state, grant, sandbox, namespace) = fixture().await;
        let plan = plan(&client, "10.96.0.1", "443").await.unwrap();
        ensure(&client, &grant, &sandbox, &namespace, "observer-g1", &plan)
            .await
            .unwrap();
        {
            let mut state = state.lock().unwrap();
            state.objects.get_mut(POLICY).unwrap()["metadata"]["finalizers"] = json!([]);
            *state
                .objects
                .get_mut(POLICY)
                .unwrap()
                .pointer_mut(&pointer)
                .unwrap() = value;
            state.calls.clear();
        }
        assert!(
            ensure(&client, &grant, &sandbox, &namespace, "observer-g1", &plan)
                .await
                .is_err(),
            "{pointer}"
        );
        assert!(mutations(&state).is_empty());
    }
}

#[tokio::test]
async fn observer_api_approval_and_namespace_fences_precede_all_writes() {
    let (_server, client, state, grant, sandbox, namespace) = fixture().await;
    let plan = plan(&client, "10.96.0.1", "443").await.unwrap();
    for mode in [
        "disabled",
        "removed",
        "target-uid",
        "workspace",
        "generation",
    ] {
        let mut candidate = grant.clone();
        match mode {
            "disabled" => candidate.spec.enabled = false,
            "removed" => candidate.spec.observation_targets.clear(),
            "target-uid" => candidate.spec.observation_targets[0].uid = "foreign".into(),
            "workspace" => candidate.spec.observation_targets[0].namespace = "foreign".into(),
            _ => candidate.metadata.generation = Some(2),
        }
        assert!(
            ensure(
                &client,
                &candidate,
                &sandbox,
                &namespace,
                "observer-g1",
                &plan
            )
            .await
            .is_err(),
            "{mode}"
        );
        assert!(mutations(&state).is_empty());
    }
    state.lock().unwrap().objects.get_mut(NS).unwrap()["metadata"]["uid"] = "replacement".into();
    assert!(
        ensure(&client, &grant, &sandbox, &namespace, "observer-g1", &plan)
            .await
            .is_err()
    );
    assert!(mutations(&state).is_empty());
}

#[tokio::test]
async fn observer_api_missing_isolation_preflight_never_reads_targets_or_writes_policy() {
    let (_server, client, state, grant, sandbox, namespace) = fixture().await;
    state.lock().unwrap().objects.insert("/api/v1/namespaces/controller".into(), json!({
        "apiVersion":"v1","kind":"Namespace","metadata":{"name":"controller","uid":"controller-ns","resourceVersion":"1"}
    }));
    let binding: Binding = serde_json::from_value(json!({
        "capability":crate::service_observer::CAPABILITY,"identity":{"managed":true},
        "grant":{"name":"workspace","namespace":"work","uid":"grant-uid","generation":1},
        "recipients":[],"privacyRevision":"fixture","privacyEpoch":null,
        "serverName":"observer-sandbox-uid.kars.internal","caPem":"fixture","workspaceUid":"workspace-uid",
        "verifier":{"capability":crate::observation_privacy::CAPABILITY,"namespace":"controller",
            "namespaceUid":"controller-ns","controllerUid":"controller-uid","serviceUid":"service-uid",
            "port":9448,"descriptorUid":"descriptor","tlsUid":"tls","tlsVersion":"1",
            "serverName":"privacy-controller-ns.kars.internal","caPem":"fixture","expiresAt":100}
    })).unwrap();
    assert!(
        super::super::ensure(&client, &grant, &sandbox, &namespace, &binding)
            .await
            .unwrap_err()
            .contains("existing controller/runtime network isolation")
    );
    assert!(mutations(&state).is_empty());
    assert!(
        !state
            .lock()
            .unwrap()
            .calls
            .iter()
            .any(|(_, path, _)| path == SERVICE || path == API)
    );
}

#[tokio::test]
async fn observer_api_retirement_finds_orphans_without_other_metadata_and_keeps_only_approved_generation()
 {
    let (_server, client, state, grant, sandbox, namespace) = fixture().await;
    let plan = plan(&client, "10.96.0.1", "443").await.unwrap();
    ensure(&client, &grant, &sandbox, &namespace, "observer-g1", &plan)
        .await
        .unwrap();
    retire(&client, &grant, true).await.unwrap();
    assert!(state.lock().unwrap().objects.contains_key(POLICY));
    let mut next = grant.clone();
    next.metadata.generation = Some(2);
    retire(&client, &next, true).await.unwrap();
    assert!(!state.lock().unwrap().objects.contains_key(POLICY));
    assert_eq!(
        state.lock().unwrap().objects[NS]["metadata"]["labels"][INDEX],
        "v1"
    );
    retire(&client, &next, true).await.unwrap();
    let deletes: Vec<_> = mutations(&state)
        .into_iter()
        .filter(|(method, _, _)| method == "DELETE")
        .collect();
    assert_eq!(deletes.len(), 1);
    assert_eq!(
        deletes[0].2["preconditions"],
        json!({"uid":"policy-uid","resourceVersion":"10"})
    );
}

#[tokio::test]
async fn observer_api_removed_disabled_and_full_revoke_remove_even_same_generation_policies() {
    for mode in ["removed", "disabled", "revoke"] {
        let (_server, client, state, mut grant, sandbox, namespace) = fixture().await;
        let plan = plan(&client, "10.96.0.1", "443").await.unwrap();
        ensure(&client, &grant, &sandbox, &namespace, "observer-g1", &plan)
            .await
            .unwrap();
        if mode == "removed" {
            grant.spec.observation_targets.clear();
        }
        if mode == "disabled" {
            grant.spec.enabled = false;
        }
        retire(&client, &grant, mode != "revoke").await.unwrap();
        assert!(
            !state.lock().unwrap().objects.contains_key(POLICY),
            "{mode}"
        );
    }
}

#[tokio::test]
async fn observer_api_cleanup_conflicts_pending_deletes_and_api_errors_remain_retryable() {
    for mode in ["conflict", "pending", "forbidden"] {
        let (_server, client, state, grant, sandbox, namespace) = fixture().await;
        let plan = plan(&client, "10.96.0.1", "443").await.unwrap();
        ensure(&client, &grant, &sandbox, &namespace, "observer-g1", &plan)
            .await
            .unwrap();
        {
            let mut state = state.lock().unwrap();
            state.delete_conflict = mode == "conflict";
            state.retain_deleted = mode == "pending";
            if mode == "forbidden" {
                state.errors.insert(POLICIES.into(), 403);
            }
        }
        assert!(retire(&client, &grant, false).await.is_err());
        assert!(state.lock().unwrap().objects.contains_key(POLICY));
        assert_eq!(
            state.lock().unwrap().objects[NS]["metadata"]["labels"][INDEX],
            "v1"
        );
        {
            let mut state = state.lock().unwrap();
            state.delete_conflict = false;
            state.retain_deleted = false;
            state.errors.clear();
        }
        retire(&client, &grant, false).await.unwrap();
        assert!(!state.lock().unwrap().objects.contains_key(POLICY));
    }
}

#[tokio::test]
async fn observer_api_partial_other_metadata_failure_cannot_skip_cnp_revocation() {
    let (_server, client, state, grant, sandbox, namespace) = fixture().await;
    let plan = plan(&client, "10.96.0.1", "443").await.unwrap();
    ensure(&client, &grant, &sandbox, &namespace, "observer-g1", &plan)
        .await
        .unwrap();
    state.lock().unwrap().errors.insert(
        "/apis/rbac.authorization.k8s.io/v1/rolebindings".into(),
        403,
    );
    assert!(super::super::revoke(&client, &grant).await.is_err());
    assert!(!state.lock().unwrap().objects.contains_key(POLICY));
    assert_eq!(
        state.lock().unwrap().objects[NS]["metadata"]["labels"][INDEX],
        "v1"
    );
    state.lock().unwrap().errors.clear();
    super::super::revoke(&client, &grant).await.unwrap();
}

#[tokio::test]
async fn observer_api_namespace_race_before_create_never_writes_into_the_replacement() {
    let (_server, client, state, grant, sandbox, namespace) = fixture().await;
    let plan = plan(&client, "10.96.0.1", "443").await.unwrap();
    state.lock().unwrap().namespace_replacement_on_policy_read = Some(POLICY.into());
    assert!(
        ensure(&client, &grant, &sandbox, &namespace, "observer-g1", &plan)
            .await
            .is_err()
    );
    assert!(!state.lock().unwrap().objects.contains_key(POLICY));
    assert!(
        !mutations(&state)
            .iter()
            .any(|(_, path, _)| path.contains("ciliumnetworkpolicies"))
    );
}

#[tokio::test]
async fn observer_api_retirement_preserves_foreign_owners_and_namespace_replacements() {
    for mode in ["grant", "owner", "target", "namespace", "workspace"] {
        let (_server, client, state, grant, sandbox, namespace) = fixture().await;
        let plan = plan(&client, "10.96.0.1", "443").await.unwrap();
        ensure(&client, &grant, &sandbox, &namespace, "observer-g1", &plan)
            .await
            .unwrap();
        {
            let mut state = state.lock().unwrap();
            match mode {
                "grant" => {
                    state.objects.get_mut(POLICY).unwrap()["metadata"]["annotations"][GRANT_OWNER] =
                        "foreign".into()
                }
                "owner" => {
                    state.objects.get_mut(POLICY).unwrap()["metadata"]["ownerReferences"][0]["uid"] =
                        "foreign".into()
                }
                "target" => {
                    state.objects.get_mut(POLICY).unwrap()["metadata"]["annotations"]
                        [claim::SOURCE_UID] = "foreign".into()
                }
                "namespace" => {
                    state.objects.get_mut(NS).unwrap()["metadata"]["uid"] = "replacement".into()
                }
                _ => {
                    state.objects.get_mut(NS).unwrap()["metadata"]["annotations"]
                        [claim::SOURCE_NAMESPACE] = "foreign".into()
                }
            }
            state.calls.clear();
        }
        let result = retire(&client, &grant, false).await;
        if mode != "workspace" {
            assert!(result.is_err(), "{mode}");
        }
        assert!(state.lock().unwrap().objects.contains_key(POLICY));
        assert!(mutations(&state).is_empty());
    }
}

#[tokio::test]
async fn observer_api_ordinary_namespaces_do_not_probe_cilium_or_gain_an_index() {
    let (_server, client, state, mut grant, _, _) = fixture().await;
    grant.spec.observation_targets.clear();
    state.lock().unwrap().errors.insert(API.into(), 403);
    retire(&client, &grant, true).await.unwrap();
    assert!(mutations(&state).is_empty());
    assert!(
        !state
            .lock()
            .unwrap()
            .calls
            .iter()
            .any(|(_, path, _)| path == API)
    );
}

#[tokio::test]
async fn observer_api_portable_policy_is_revoked_when_last_target_is_removed() {
    let (_server, client, state, mut grant, sandbox, namespace) = fixture().await;
    let name = format!(
        "{}-rpc",
        policy_prefix(&grant, &sandbox.uid().unwrap()).unwrap()
    );
    let path = format!("/apis/networking.k8s.io/v1/namespaces/kars-agent/networkpolicies/{name}");
    let plan = plan(&client, "10.96.0.1", "443").await.unwrap();
    apply_runtime(
        &client,
        &grant,
        &namespace,
        "NetworkPolicy",
        &name,
        json!({
            "spec":{"podSelector":{"matchLabels":{"kars.azure.com/sandbox":"agent"}},
                "policyTypes":["Egress"],"egress":plan.rules}
        }),
    )
    .await
    .unwrap();
    state.lock().unwrap().calls.clear();
    state.lock().unwrap().errors.insert(API.into(), 403);
    grant.spec.observation_targets.clear();
    super::super::revoke_stale(&client, &grant).await.unwrap();
    assert!(!state.lock().unwrap().objects.contains_key(&path));
    assert!(
        !state
            .lock()
            .unwrap()
            .calls
            .iter()
            .any(|(_, path, _)| path == API)
    );
    assert_eq!(
        mutations(&state)[0].2["preconditions"],
        json!({"uid":"policy-uid","resourceVersion":"10"})
    );
}

fn runtime_policy(
    kind: &str,
    sandbox: &KarsSandbox,
    namespace: &Namespace,
    plan: &Plan,
) -> (String, Value) {
    if kind == KIND {
        (
            format!("{POLICIES}/fenced"),
            json!({"spec":spec(sandbox, namespace, plan).unwrap()}),
        )
    } else {
        (
            "/apis/networking.k8s.io/v1/namespaces/kars-agent/networkpolicies/fenced".into(),
            json!({"spec":{"podSelector":{"matchLabels":{"kars.azure.com/sandbox":"agent"}},
                "policyTypes":["Egress"],"egress":plan.rules}}),
        )
    }
}

#[tokio::test]
async fn observer_api_both_policy_kinds_keep_original_namespace_uid_across_create_update_and_noop()
{
    for kind in ["NetworkPolicy", KIND] {
        for operation in ["create", "update", "noop"] {
            for replacement in ["before-namespace-read", "after-policy-read"] {
                let (_server, client, state, grant, sandbox, namespace) = fixture().await;
                let plan = plan(&client, "10.96.0.1", "443").await.unwrap();
                let (path, desired) = runtime_policy(kind, &sandbox, &namespace, &plan);
                if operation != "create" {
                    let mut initial = desired.clone();
                    if operation == "update" {
                        initial["spec"]["egress"] = json!([]);
                    }
                    apply_runtime(&client, &grant, &namespace, kind, "fenced", initial)
                        .await
                        .unwrap();
                }
                let original = {
                    let mut state = state.lock().unwrap();
                    let original = state.objects.get(&path).cloned();
                    state.calls.clear();
                    if replacement == "before-namespace-read" {
                        state.objects.get_mut(NS).unwrap()["metadata"]["uid"] =
                            "replacement-uid".into();
                    } else {
                        state.namespace_replacement_on_policy_read = Some(path.clone());
                    }
                    original
                };
                let error = apply_runtime(&client, &grant, &namespace, kind, "fenced", desired)
                    .await
                    .unwrap_err();
                assert!(
                    error.contains("namespace"),
                    "{kind}/{operation}/{replacement}"
                );
                assert!(
                    mutations(&state).is_empty(),
                    "{kind}/{operation}/{replacement}"
                );
                assert_eq!(state.lock().unwrap().objects.get(&path), original.as_ref());
                assert_eq!(namespace.uid().as_deref(), Some("runtime-uid"));
            }
        }
    }
}

#[tokio::test]
async fn observer_api_both_policy_kinds_require_existing_uid_rv_and_preserve_conflicted_objects() {
    for kind in ["NetworkPolicy", KIND] {
        let (_server, client, state, grant, sandbox, namespace) = fixture().await;
        let plan = plan(&client, "10.96.0.1", "443").await.unwrap();
        let (path, desired) = runtime_policy(kind, &sandbox, &namespace, &plan);
        let mut initial = desired.clone();
        initial["spec"]["egress"] = json!([]);
        apply_runtime(&client, &grant, &namespace, kind, "fenced", initial)
            .await
            .unwrap();
        let original = state.lock().unwrap().objects[&path].clone();
        for missing in ["uid", "resourceVersion"] {
            {
                let mut state = state.lock().unwrap();
                state.objects.insert(path.clone(), original.clone());
                state.objects.get_mut(&path).unwrap()["metadata"][missing] = Value::Null;
                state.calls.clear();
            }
            assert!(
                apply_runtime(&client, &grant, &namespace, kind, "fenced", desired.clone())
                    .await
                    .is_err(),
                "{kind}/{missing}"
            );
            assert!(mutations(&state).is_empty());
        }
        {
            let mut state = state.lock().unwrap();
            state.objects.insert(path.clone(), original.clone());
            state.replace_conflict = true;
            state.calls.clear();
        }
        let error = apply_runtime(&client, &grant, &namespace, kind, "fenced", desired.clone())
            .await
            .unwrap_err();
        assert!(error.contains("409"));
        assert_eq!(state.lock().unwrap().objects[&path], original);
        let requests = mutations(&state);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].0, "PUT");
        assert_eq!(requests[0].2["metadata"]["uid"], "policy-uid");
        assert_eq!(requests[0].2["metadata"]["resourceVersion"], "10");
        assert_eq!(
            requests[0].2["metadata"]["annotations"][NAMESPACE_UID],
            "runtime-uid"
        );
        {
            let mut state = state.lock().unwrap();
            state.replace_conflict = false;
            state.calls.clear();
        }
        apply_runtime(&client, &grant, &namespace, kind, "fenced", desired.clone())
            .await
            .unwrap();
        assert_eq!(
            state.lock().unwrap().objects[&path]["spec"],
            desired["spec"]
        );
        assert_eq!(
            state.lock().unwrap().objects[&path]["metadata"]["uid"],
            "policy-uid"
        );
    }
}

#[test]
fn observer_api_policy_does_not_change_agent_uid_1000_guard() {
    let guard = crate::reconciler::build_egress_guard_command(false);
    assert_eq!(guard, crate::reconciler::build_egress_guard_command(true));
    assert!(guard.contains("-m owner --uid-owner 1000 -j DROP"));
    assert!(guard.contains("--dport 443 -j REDIRECT --to-port 8444"));
    assert!(!guard.contains("6443"));
    assert!(!guard.contains("KUBERNETES_SERVICE_HOST"));
}
