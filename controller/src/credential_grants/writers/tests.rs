// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use wiremock::{Mock, MockServer, ResponseTemplate};

const WORKSPACE: &str = "/api/v1/namespaces/work";
const NAMESPACE: &str = "/api/v1/namespaces/bridge";
const ACCOUNT: &str = "/api/v1/namespaces/bridge/serviceaccounts/bff";
const GRANT: &str = "/apis/kars.azure.com/v1alpha1/namespaces/work/karscredentialgrants/workspace";
const ROLE: &str =
    "/apis/rbac.authorization.k8s.io/v1/namespaces/work/roles/kars-credential-writer-grant";

#[derive(Default)]
struct State {
    objects: BTreeMap<String, Value>,
    calls: Vec<(String, String, Value)>,
    allow: bool,
}

async fn fixture() -> (MockServer, Client, Arc<Mutex<State>>, KarsCredentialGrant) {
    let server = MockServer::start().await;
    let grant: KarsCredentialGrant = serde_json::from_value(json!({
        "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsCredentialGrant",
        "metadata":{"name":"workspace","namespace":"work","uid":"grant","resourceVersion":"1","generation":1},
        "spec":{"workspaceUid":"workspace","writers":[{"namespace":"bridge","name":"bff","uid":"writer"}]},
        "status":{"phase":"Ready","observedGeneration":1,"reason":"fixture"}
    })).unwrap();
    let state = Arc::new(Mutex::new(State::default()));
    {
        let mut state = state.lock().unwrap();
        state
            .objects
            .insert(GRANT.into(), serde_json::to_value(&grant).unwrap());
        for (path, name, uid, kind) in [
            (WORKSPACE, "work", "workspace", "Namespace"),
            (NAMESPACE, "bridge", "bridge-uid", "Namespace"),
            (ACCOUNT, "bff", "writer", "ServiceAccount"),
        ] {
            let mut object = json!({"apiVersion":"v1","kind":kind,
                "metadata":{"name":name,"uid":uid,"resourceVersion":"1"}});
            if kind == "ServiceAccount" {
                object["metadata"]["namespace"] = "bridge".into();
            } else {
                object["spec"] = json!({"finalizers":["kubernetes"]});
            }
            state.objects.insert(path.into(), object);
        }
    }
    let captured = state.clone();
    Mock::given(|_: &wiremock::Request| true).respond_with(move |request: &wiremock::Request| {
        let mut state = captured.lock().unwrap();
        let path = request.url.path();
        let body: Value = request.body_json().unwrap_or(Value::Null);
        state.calls.push((request.method.to_string(), path.into(), body.clone()));
        if request.method == "POST" && path.ends_with("/subjectaccessreviews") {
            return ResponseTemplate::new(201).set_body_json(json!({
                "apiVersion":"authorization.k8s.io/v1","kind":"SubjectAccessReview",
                "spec":body["spec"],"status":{"allowed":state.allow}
            }));
        }
        if request.method == "POST" && path.ends_with("/selfsubjectreviews") {
            return ResponseTemplate::new(201).set_body_json(json!({
                "apiVersion":"authentication.k8s.io/v1","kind":"SelfSubjectReview",
                "status":{"userInfo":{"username":"system:serviceaccount:work:kars-controller","uid":"controller"}}
            }));
        }
        if request.method == "GET" {
            if let Some(value) = state.objects.get(path) {
                return ResponseTemplate::new(200).set_body_json(value);
            }
            for (suffix, kind) in [
                ("/serviceaccounts", "ServiceAccount"), ("/namespaces", "Namespace"),
                ("/rolebindings", "RoleBinding"), ("/roles", "Role"),
            ] {
                if path.ends_with(suffix) {
                    let selector = request.url.query_pairs().find(|(k, _)| k == "labelSelector").map(|(_, v)| v.into_owned());
                    let items: Vec<_> = state.objects.values().filter(|value| {
                        value["kind"] == kind && selector.as_ref().is_none_or(|selector| {
                            let (key, expected) = selector.split_once('=').map_or((selector.as_str(), None), |(key, value)| (key, Some(value)));
                            value["metadata"]["labels"][key].as_str().is_some_and(|value| expected.is_none_or(|expected| value == expected))
                        })
                    }).cloned().collect();
                    return ResponseTemplate::new(200).set_body_json(json!({
                        "apiVersion":if kind.contains("Role") {"rbac.authorization.k8s.io/v1"} else {"v1"},
                        "kind":format!("{kind}List"),"metadata":{},"items":items
                    }));
                }
            }
        }
        if request.method == "PATCH" && let Some(value) = state.objects.get_mut(path) {
            assert_eq!(value["metadata"]["uid"], body["metadata"]["uid"]);
            assert_eq!(value["metadata"]["resourceVersion"], body["metadata"]["resourceVersion"]);
            value["metadata"]["finalizers"] = body["metadata"]["finalizers"].clone();
            for key in ["annotations", "labels"] {
                let fields = value["metadata"].as_object_mut().unwrap()
                    .entry(key).or_insert_with(|| json!({})).as_object_mut().unwrap();
                for (name, entry) in body["metadata"][key].as_object().unwrap() {
                    if entry.is_null() {
                        fields.remove(name);
                    } else {
                        fields.insert(name.clone(), entry.clone());
                    }
                }
            }
            return ResponseTemplate::new(200).set_body_json(value.clone());
        }
        ResponseTemplate::new(404).set_body_json(json!({
            "apiVersion":"v1","kind":"Status","status":"Failure","reason":"NotFound","code":404
        }))
    }).mount(&server).await;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
    (server, client, state, grant)
}

#[tokio::test]
async fn credential_writer_inherited_permission_failure_revokes_writer_not_delivery_authority() {
    let (_server, client, state, grant) = fixture().await;
    state.lock().unwrap().allow = true;
    let (active, error) = authority(&client, &grant, &[]).await;
    assert!(active.spec.writers.is_empty());
    assert!(error.unwrap().contains("authentication groups"));
    super::super::verify(&client, &grant).await.unwrap();
    for path in [NAMESPACE, ACCOUNT] {
        assert_eq!(
            state.lock().unwrap().objects[path]["metadata"]["finalizers"],
            json!([])
        );
    }
}

#[tokio::test]
async fn credential_writer_replacement_never_inherits_or_adopts_an_old_uid_grant() {
    let (_server, client, state, grant) = fixture().await;
    state.lock().unwrap().objects.get_mut(ACCOUNT).unwrap()["metadata"]["uid"] =
        "replacement".into();
    let active = reconcile(&client, &grant).await.unwrap();
    assert!(active.spec.writers.is_empty());
    super::super::verify(&client, &grant).await.unwrap();
    assert!(
        state
            .lock()
            .unwrap()
            .calls
            .iter()
            .all(|(method, _, _)| method == "GET")
    );
}

#[tokio::test]
async fn credential_writer_absence_does_not_revoke_current_delivery_authority() {
    let (_server, client, state, grant) = fixture().await;
    state.lock().unwrap().objects.remove(ACCOUNT);
    super::super::verify(&client, &grant).await.unwrap();
    let active = reconcile(&client, &grant).await.unwrap();
    assert!(active.spec.writers.is_empty());
    assert!(
        state
            .lock()
            .unwrap()
            .calls
            .iter()
            .all(|(method, _, _)| method == "GET")
    );
}

#[tokio::test]
async fn credential_writer_names_are_held_before_native_read_roles_can_be_issued() {
    let (_server, client, state, grant) = fixture().await;
    reconcile(&client, &grant).await.unwrap();
    verify(&client, &grant).await.unwrap();
    let state = state.lock().unwrap();
    let mutations: Vec<_> = state
        .calls
        .iter()
        .filter(|(method, _, _)| method == "PATCH")
        .collect();
    assert_eq!(mutations.len(), 2);
    assert_eq!(mutations[0].1, NAMESPACE);
    assert_eq!(mutations[1].1, ACCOUNT);
    for path in [NAMESPACE, ACCOUNT] {
        let metadata = &state.objects[path]["metadata"];
        assert_eq!(metadata["finalizers"], json!([key(&grant).unwrap()]));
        assert_eq!(metadata["annotations"][key(&grant).unwrap()], "controller");
        assert_eq!(metadata["labels"][key(&grant).unwrap()], "bridge-uid");
    }
}

#[tokio::test]
async fn credential_writer_role_delete_ack_does_not_release_names_while_role_still_exists() {
    let (_server, client, state, grant) = fixture().await;
    reconcile(&client, &grant).await.unwrap();
    {
        let mut state = state.lock().unwrap();
        state.objects.insert(ROLE.into(), json!({
            "apiVersion":"rbac.authorization.k8s.io/v1","kind":"Role",
            "metadata":{"name":"kars-credential-writer-grant","namespace":"work","uid":"role",
                "resourceVersion":"1","deletionTimestamp":"2026-01-01T00:00:00Z","finalizers":["external"]},
            "rules":[]
        }));
        state.calls.clear();
    }
    assert!(release(&client, &grant).await.is_err());
    assert!(
        state
            .lock()
            .unwrap()
            .calls
            .iter()
            .all(|(method, _, _)| method == "GET")
    );
    state.lock().unwrap().objects.remove(ROLE);
    release(&client, &grant).await.unwrap();
    let state = state.lock().unwrap();
    for path in [ACCOUNT, NAMESPACE] {
        assert_eq!(state.objects[path]["metadata"]["finalizers"], json!([]));
    }
    assert!(state.calls.iter().all(|(method, _, _)| method != "DELETE"));
}
