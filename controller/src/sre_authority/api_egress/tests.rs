// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::sre_authority::tests::{State, fixture as base_fixture, registration};
use std::sync::{Arc, Mutex};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

const DISCOVERY: &str = "/apis/cilium.io/v2";
const REG: &str = "/apis/kars.azure.com/v1alpha1/karssreregistrations/canonical";
const POLICIES: &str = "/apis/cilium.io/v2/namespaces/kars-sre/ciliumnetworkpolicies";
const OBJECT: &str = "/apis/cilium.io/v2/namespaces/kars-sre/ciliumnetworkpolicies/kars-sre-api";

async fn fixture() -> (
    MockServer,
    Client,
    Arc<Mutex<State>>,
    KarsSandbox,
    Namespace,
) {
    let (server, client, state) = base_fixture().await;
    let (sandbox, namespace) = {
        let mut state = state.lock().unwrap();
        let reg = registration();
        let mut value = serde_json::to_value(&reg).unwrap();
        value["status"] = json!({"phase":"Ready","observedGeneration":1,
            "legacySecretAccessDenied":true,"privacyRevision":crate::sre_privacy::REVISION,
            "privacyEpoch":reg.epoch()});
        state.objects.insert(REG.into(), value);
        state.objects.insert(DISCOVERY.into(), json!({"apiVersion":"v1","kind":"APIResourceList",
            "groupVersion":"cilium.io/v2","resources":[{"name":"ciliumnetworkpolicies","singularName":"",
            "namespaced":true,"kind":"CiliumNetworkPolicy","verbs":["get","list","create","update","delete"]}]}));
        (
            serde_json::from_value(state.sandbox.clone()).unwrap(),
            serde_json::from_value(state.namespace.clone()).unwrap(),
        )
    };
    (server, client, state, sandbox, namespace)
}

fn rules() -> Vec<Value> {
    vec![
        json!({"to":[{"ipBlock":{"cidr":"10.96.0.1/32"}}],"ports":[{"protocol":"TCP","port":443}]}),
        json!({"to":[{"ipBlock":{"cidr":"172.18.0.3/32"}}],"ports":[{"protocol":"TCP","port":6443}]}),
    ]
}

fn mutations(state: &Arc<Mutex<State>>) -> Vec<(String, String, Value)> {
    state
        .lock()
        .unwrap()
        .calls
        .iter()
        .filter(|(verb, _, _)| verb != "GET")
        .cloned()
        .collect()
}

#[tokio::test]
async fn registered_sre_policy_is_exact_owned_idempotent_and_does_not_touch_standard_policy() {
    let (_server, client, state, sandbox, namespace) = fixture().await;
    ensure(&client, &sandbox, &namespace, &rules())
        .await
        .unwrap();
    let object = state.lock().unwrap().objects[OBJECT].clone();
    assert_eq!(
        object["spec"],
        json!({
            "endpointSelector":{"matchLabels":{"k8s:kars.azure.com/sandbox":"sre",
                "k8s:io.kubernetes.pod.namespace":"kars-sre"}},
            "egress":[{"toEntities":["kube-apiserver"],"toPorts":[{"ports":[
                {"port":"443","protocol":"TCP"},{"port":"6443","protocol":"TCP"}]}]}]
        })
    );
    assert_eq!(
        object["metadata"]["ownerReferences"],
        owner(&registration())
    );
    assert_eq!(
        object["metadata"]["annotations"][claim::SOURCE_UID],
        "source"
    );
    assert_eq!(
        object["metadata"]["annotations"][claim::NAMESPACE_UID],
        "runtime-ns"
    );
    ensure(&client, &sandbox, &namespace, &rules())
        .await
        .unwrap();
    assert_eq!(mutations(&state).len(), 1);
    assert_eq!(mutations(&state)[0].1, POLICIES);
}

#[tokio::test]
async fn absent_cilium_does_not_write_optional_resources() {
    let (_server, client, state, sandbox, namespace) = fixture().await;
    state.lock().unwrap().objects.remove(DISCOVERY);
    ensure(&client, &sandbox, &namespace, &rules())
        .await
        .unwrap();
    retire(&client, &registration()).await.unwrap();
    assert!(mutations(&state).is_empty());
}

#[tokio::test]
async fn absent_discovery_without_a_status_reason_does_not_block_retirement() {
    for response in [
        ResponseTemplate::new(404).set_body_string("404 page not found\n"),
        ResponseTemplate::new(404).set_body_json(json!({
            "kind":"Status","apiVersion":"v1","status":"Failure",
            "code":404,"reason":"","message":"PRIVATE_SENTINEL"})),
    ] {
        let (server, client, state, sandbox, namespace) = fixture().await;
        Mock::given(method("GET"))
            .and(path(DISCOVERY))
            .respond_with(response)
            .with_priority(1)
            .mount(&server)
            .await;
        ensure(&client, &sandbox, &namespace, &rules())
            .await
            .unwrap();
        retire(&client, &registration()).await.unwrap();
        assert!(mutations(&state).is_empty());
    }
}

#[tokio::test]
async fn discovery_http_failures_never_authorize_retirement_or_leak_response() {
    for code in [401, 403, 429, 500, 503] {
        let (server, client, state, sandbox, namespace) = fixture().await;
        Mock::given(method("GET"))
            .and(path(DISCOVERY))
            .respond_with(ResponseTemplate::new(code).set_body_json(json!({
                "kind":"Status","apiVersion":"v1","status":"Failure",
                "code":code,"reason":"NotFound","message":"PRIVATE_SENTINEL"})))
            .with_priority(1)
            .mount(&server)
            .await;
        for result in [
            ensure(&client, &sandbox, &namespace, &rules()).await,
            retire(&client, &registration()).await,
        ] {
            let error = result.unwrap_err();
            assert!(error.contains(&code.to_string()));
            assert!(!error.contains("PRIVATE_SENTINEL"));
        }
        assert!(mutations(&state).is_empty());
    }
}

#[tokio::test]
async fn discovery_failure_or_incomplete_contract_never_falls_back_or_leaks_response() {
    let (server, client, state, sandbox, namespace) = fixture().await;
    state.lock().unwrap().objects.get_mut(DISCOVERY).unwrap()["resources"][0]["namespaced"] =
        false.into();
    assert!(
        ensure(&client, &sandbox, &namespace, &rules())
            .await
            .is_err()
    );
    Mock::given(method("GET"))
        .and(path(DISCOVERY))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "kind":"Status","apiVersion":"v1","status":"Failure","reason":"Forbidden",
            "code":403,"message":"PRIVATE_SENTINEL"})))
        .with_priority(1)
        .mount(&server)
        .await;
    let error = ensure(&client, &sandbox, &namespace, &rules())
        .await
        .unwrap_err();
    assert!(error.contains("403"));
    assert!(!error.contains("PRIVATE_SENTINEL"));
    assert!(mutations(&state).is_empty());
}

#[tokio::test]
async fn unqualified_replaced_or_noncanonical_occupants_never_receive_a_policy() {
    for change in ["disabled", "stale", "epoch", "source", "namespace", "label"] {
        let (_server, client, state, sandbox, namespace) = fixture().await;
        {
            let mut state = state.lock().unwrap();
            match change {
                "disabled" => state.objects.get_mut(REG).unwrap()["spec"]["enabled"] = false.into(),
                "stale" => state.objects.get_mut(REG).unwrap()["metadata"]["generation"] = 2.into(),
                "epoch" => {
                    state.objects.get_mut(REG).unwrap()["status"]["privacyEpoch"] = "old".into()
                }
                "source" => state.sandbox["metadata"]["uid"] = "replacement".into(),
                "namespace" => state.namespace["metadata"]["uid"] = "replacement".into(),
                _ => state.sandbox["metadata"]["labels"]["kars.azure.com/role"] = "ordinary".into(),
            };
        }
        assert!(
            ensure(&client, &sandbox, &namespace, &rules())
                .await
                .is_err(),
            "{change}"
        );
        assert!(mutations(&state).is_empty(), "{change}");
    }
}

#[tokio::test]
async fn foreign_policies_are_never_updated_or_deleted() {
    let (_server, client, state, sandbox, namespace) = fixture().await;
    ensure(&client, &sandbox, &namespace, &rules())
        .await
        .unwrap();
    let original = state.lock().unwrap().objects[OBJECT].clone();
    for key in [OWNER, claim::SOURCE_UID, claim::NAMESPACE_UID, "owner"] {
        let mut foreign = original.clone();
        if key == "owner" {
            foreign["metadata"]["ownerReferences"][0]["uid"] = "foreign".into();
        } else {
            foreign["metadata"]["annotations"][key] = "foreign".into();
        }
        {
            let mut state = state.lock().unwrap();
            state.objects.insert(OBJECT.into(), foreign.clone());
            state.calls.clear();
        }
        assert!(
            ensure(&client, &sandbox, &namespace, &rules())
                .await
                .is_err()
        );
        assert!(retire(&client, &registration()).await.is_err());
        assert!(mutations(&state).is_empty());
        assert_eq!(state.lock().unwrap().objects[OBJECT], foreign);
    }
}

#[tokio::test]
async fn owned_updates_remove_union_drift_preserve_status_and_use_compare_and_swap() {
    let (_server, client, state, sandbox, namespace) = fixture().await;
    ensure(&client, &sandbox, &namespace, &rules())
        .await
        .unwrap();
    {
        let mut state = state.lock().unwrap();
        let object = state.objects.get_mut(OBJECT).unwrap();
        object["specs"] = json!([{"egress":[{"toEntities":["world"]}]}]);
        object["status"] = json!({"canary":"preserved"});
        state.calls.clear();
    }
    ensure(&client, &sandbox, &namespace, &rules())
        .await
        .unwrap();
    let writes = mutations(&state);
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].0, "PUT");
    assert_eq!(writes[0].2["metadata"]["resourceVersion"], "1");
    assert!(writes[0].2["metadata"]["uid"].as_str().is_some());
    let object = state.lock().unwrap().objects[OBJECT].clone();
    assert!(object.get("specs").is_none());
    assert_eq!(object["status"]["canary"], "preserved");
    retire(&client, &registration()).await.unwrap();
    retire(&client, &registration()).await.unwrap();
    assert!(!state.lock().unwrap().objects.contains_key(OBJECT));
    assert_eq!(mutations(&state).len(), 2);
    assert_eq!(
        mutations(&state)[1].2["preconditions"]["resourceVersion"],
        "2"
    );
}

#[tokio::test]
async fn replaced_namespace_or_cas_conflict_preserves_the_policy() {
    let (server, client, state, sandbox, namespace) = fixture().await;
    ensure(&client, &sandbox, &namespace, &rules())
        .await
        .unwrap();
    state.lock().unwrap().calls.clear();
    state.lock().unwrap().namespace["metadata"]["uid"] = "replacement".into();
    assert!(retire(&client, &registration()).await.is_err());
    assert!(mutations(&state).is_empty());
    state.lock().unwrap().namespace["metadata"]["uid"] = "runtime-ns".into();
    Mock::given(method("DELETE"))
        .and(path(OBJECT))
        .respond_with(ResponseTemplate::new(409).set_body_json(json!({
            "kind":"Status","apiVersion":"v1","status":"Failure","reason":"Conflict",
            "code":409,"message":"PRIVATE_SENTINEL"})))
        .with_priority(1)
        .mount(&server)
        .await;
    let error = retire(&client, &registration()).await.unwrap_err();
    assert!(error.contains("409"));
    assert!(!error.contains("PRIVATE_SENTINEL"));
    assert!(state.lock().unwrap().objects.contains_key(OBJECT));
}

#[tokio::test]
async fn registration_revoked_during_issuance_retires_the_just_created_policy() {
    let (server, client, state, sandbox, namespace) = fixture().await;
    let captured = state.clone();
    Mock::given(method("POST"))
        .and(path(POLICIES))
        .respond_with(move |request: &wiremock::Request| {
            let mut object: Value = request.body_json().unwrap();
            object["metadata"]["uid"] = "issued-policy".into();
            object["metadata"]["resourceVersion"] = "1".into();
            let mut state = captured.lock().unwrap();
            state.objects.insert(OBJECT.into(), object.clone());
            state.objects.get_mut(REG).unwrap()["spec"]["enabled"] = false.into();
            ResponseTemplate::new(201).set_body_json(object)
        })
        .with_priority(1)
        .mount(&server)
        .await;
    assert!(
        ensure(&client, &sandbox, &namespace, &rules())
            .await
            .is_err()
    );
    assert!(!state.lock().unwrap().objects.contains_key(OBJECT));
    assert_eq!(mutations(&state)[0].0, "DELETE");
}
