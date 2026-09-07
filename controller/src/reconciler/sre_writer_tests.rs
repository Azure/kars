// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::reconciler::namespace_ownership::VERSION;
use serde_json::Value;
use std::sync::{Arc, Mutex};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const NS_PATH: &str = "/api/v1/namespaces/kars-sre";
const ACCOUNTS_PATH: &str = "/api/v1/namespaces/kars-sre/serviceaccounts";
const WRITER_PATH: &str = "/api/v1/namespaces/kars-sre/serviceaccounts/sre-writer";

fn sandbox() -> KarsSandbox {
    serde_json::from_value(json!({
        "apiVersion": "kars.azure.com/v1alpha1", "kind": "KarsSandbox",
        "metadata": {
            "name": "sre", "namespace": "workspace-a", "uid": "sandbox-sre",
            "resourceVersion": "10",
            "labels": {"kars.azure.com/role": "sre"},
            "annotations": {
                HELM_NAME: "customer-release", HELM_NAMESPACE: "workspace-a",
                NAMESPACE_UID: "namespace-sre"
            }
        },
        "spec": {"inferenceRef": {"name": "sre-inference"}}
    }))
    .unwrap()
}

fn namespace(sandbox: &KarsSandbox) -> Namespace {
    serde_json::from_value(json!({
        "apiVersion": "v1", "kind": "Namespace",
        "metadata": {
            "name": NAMESPACE, "uid": "namespace-sre", "resourceVersion": "20",
            "annotations": {
                VERSION: "v1", SOURCE_NAME: "sre",
                SOURCE_NAMESPACE: sandbox.namespace(), SOURCE_UID: sandbox.metadata.uid
            }
        }
    }))
    .unwrap()
}

fn helm_account() -> ServiceAccount {
    serde_json::from_value(json!({
        "apiVersion": "v1", "kind": "ServiceAccount",
        "metadata": {
            "name": NAME, "namespace": NAMESPACE, "uid": "legacy-writer", "resourceVersion": "30",
            "labels": {
                "app.kubernetes.io/name": "kars", "app.kubernetes.io/component": "sre",
                MANAGED_BY: "Helm", "kars.azure.com/role": NAME, "customer-label": "keep"
            },
            "annotations": {
                HELM_NAME: "customer-release", HELM_NAMESPACE: "workspace-a",
                "kars.azure.com/no-automount": "true", "customer.example/note": "keep"
            }
        },
        "automountServiceAccountToken": false
    }))
    .unwrap()
}

struct State {
    namespace: Namespace,
    account: Option<ServiceAccount>,
    get_error: u16,
    create_error: u16,
    race_account: Option<ServiceAccount>,
}

fn response(code: u16, body: Value) -> ResponseTemplate {
    ResponseTemplate::new(code).set_body_json(body)
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

#[derive(Clone)]
struct Server(Arc<Mutex<State>>);

impl Respond for Server {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let mut state = self.0.lock().unwrap();
        match (request.method.as_str(), request.url.path()) {
            ("GET", NS_PATH) => response(200, serde_json::to_value(&state.namespace).unwrap()),
            ("GET", WRITER_PATH) => {
                if state.get_error != 0 {
                    return failure(state.get_error);
                }
                state.account.as_ref().map_or_else(
                    || failure(404),
                    |account| response(200, serde_json::to_value(account).unwrap()),
                )
            }
            ("POST", ACCOUNTS_PATH) => {
                if let Some(winner) = state.race_account.take() {
                    state.account = Some(winner);
                    return failure(409);
                }
                if state.create_error != 0 {
                    return failure(state.create_error);
                }
                if state.account.is_some() {
                    return failure(409);
                }
                let mut account: ServiceAccount = serde_json::from_slice(&request.body).unwrap();
                account.metadata.uid = Some("created-writer".into());
                account.metadata.resource_version = Some("31".into());
                let body = serde_json::to_value(&account).unwrap();
                state.account = Some(account);
                response(201, body)
            }
            _ => failure(500),
        }
    }
}

async fn setup(
    sandbox: &KarsSandbox,
    account: Option<ServiceAccount>,
) -> (MockServer, Client, Api<ServiceAccount>, Arc<Mutex<State>>) {
    let server = MockServer::start().await;
    let state = Arc::new(Mutex::new(State {
        namespace: namespace(sandbox),
        account,
        get_error: 0,
        create_error: 0,
        race_account: None,
    }));
    Mock::given(wiremock::matchers::any())
        .respond_with(Server(state.clone()))
        .mount(&server)
        .await;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
    let accounts = Api::namespaced(client.clone(), NAMESPACE);
    (server, client, accounts, state)
}

#[tokio::test]
async fn creates_writer_only_after_verified_namespace_claim_with_automount_disabled() {
    let sandbox = sandbox();
    let ns = namespace(&sandbox);
    let (server, client, accounts, state) = setup(&sandbox, None).await;
    ensure(&client, &sandbox, &accounts, Some(&ns))
        .await
        .unwrap();
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        (requests[0].method.as_str(), requests[0].url.path()),
        ("GET", NS_PATH)
    );
    assert_eq!(
        (requests[1].method.as_str(), requests[1].url.path()),
        ("GET", WRITER_PATH)
    );
    assert_eq!(
        (requests[2].method.as_str(), requests[2].url.path()),
        ("POST", ACCOUNTS_PATH)
    );
    let body: Value = serde_json::from_slice(&requests[2].body).unwrap();
    assert_eq!(body["automountServiceAccountToken"], false);
    assert_eq!(body["metadata"]["name"], "sre-writer");
    assert_eq!(body["metadata"]["namespace"], "kars-sre");
    assert_eq!(body["metadata"]["annotations"][SOURCE_UID], "sandbox-sre");
    assert_eq!(
        body["metadata"]["ownerReferences"][0]["uid"],
        "namespace-sre"
    );
    let original = state.lock().unwrap().account.clone();
    ensure(&client, &sandbox, &accounts, Some(&ns))
        .await
        .unwrap();
    assert_eq!(state.lock().unwrap().account, original);
    assert!(
        server.received_requests().await.unwrap()[3..]
            .iter()
            .all(|r| r.method == "GET")
    );
}

#[tokio::test]
async fn ordinary_sandboxes_and_noncanonical_sre_names_are_unchanged() {
    for (name, role) in [("demo", "agent"), ("demo", "sre"), ("sre", "agent")] {
        let mut sandbox = sandbox();
        sandbox.metadata.name = Some(name.into());
        sandbox
            .metadata
            .labels
            .as_mut()
            .unwrap()
            .insert("kars.azure.com/role".into(), role.into());
        let (server, client, accounts, state) = setup(&sandbox, None).await;
        ensure(&client, &sandbox, &accounts, None).await.unwrap();
        assert!(server.received_requests().await.unwrap().is_empty());
        assert!(state.lock().unwrap().account.is_none());
    }
}

#[tokio::test]
async fn unverified_foreign_and_replaced_namespaces_never_read_or_create_writer_accounts() {
    for variant in [
        "missing-proof",
        "unclaimed",
        "foreign",
        "replacement",
        "wrong-api",
    ] {
        let sandbox = sandbox();
        let ns = namespace(&sandbox);
        let (server, client, accounts, state) = setup(&sandbox, None).await;
        match variant {
            "unclaimed" => state.lock().unwrap().namespace.metadata.annotations = None,
            "foreign" => {
                state
                    .lock()
                    .unwrap()
                    .namespace
                    .metadata
                    .annotations
                    .as_mut()
                    .unwrap()
                    .insert(SOURCE_UID.into(), "foreign-sandbox".into());
            }
            "replacement" => {
                state.lock().unwrap().namespace.metadata.uid = Some("replacement".into())
            }
            _ => {}
        }
        let accounts = if variant == "wrong-api" {
            Api::namespaced(client.clone(), "customer-namespace")
        } else {
            accounts
        };
        assert!(
            ensure(
                &client,
                &sandbox,
                &accounts,
                if variant == "missing-proof" {
                    None
                } else {
                    Some(&ns)
                },
            )
            .await
            .is_err(),
            "{variant}"
        );
        assert!(
            server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .all(|r| r.method == "GET" && r.url.path() == NS_PATH)
        );
        assert!(state.lock().unwrap().account.is_none());
    }
}

#[tokio::test]
async fn compatible_legacy_helm_account_keeps_its_uid_fields_and_annotations_without_writes() {
    let sandbox = sandbox();
    let ns = namespace(&sandbox);
    let original = helm_account();
    let (server, client, accounts, state) = setup(&sandbox, Some(original.clone())).await;
    ensure(&client, &sandbox, &accounts, Some(&ns))
        .await
        .unwrap();
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|r| r.method == "GET")
    );
    assert_eq!(state.lock().unwrap().account, Some(original));
}

#[test]
fn cli_source_can_reuse_retained_namespace_release_but_not_unproven_helm_ownership() {
    let mut sandbox = sandbox();
    sandbox
        .metadata
        .annotations
        .as_mut()
        .unwrap()
        .remove(HELM_NAME);
    sandbox
        .metadata
        .annotations
        .as_mut()
        .unwrap()
        .remove(HELM_NAMESPACE);
    let mut ns = namespace(&sandbox);
    let account = helm_account();
    assert!(compatible(&account, &sandbox, &ns).is_err());
    ns.metadata
        .annotations
        .as_mut()
        .unwrap()
        .insert(HELM_NAME.into(), "customer-release".into());
    ns.metadata
        .annotations
        .as_mut()
        .unwrap()
        .insert(HELM_NAMESPACE.into(), "workspace-a".into());
    compatible(&account, &sandbox, &ns).unwrap();
    sandbox
        .metadata
        .annotations
        .as_mut()
        .unwrap()
        .insert(HELM_NAME.into(), "other-release".into());
    assert!(compatible(&account, &sandbox, &ns).is_err());
}

#[test]
fn conflicting_account_data_and_foreign_ownership_are_rejected() {
    let sandbox = sandbox();
    let ns = namespace(&sandbox);
    for variant in [
        "automount-true",
        "automount-default",
        "foreign-release",
        "foreign-release-namespace",
        "foreign-owner",
        "unknown-owner",
        "foreign-binding",
        "token-secret",
        "pull-secret",
    ] {
        let mut account = serde_json::to_value(helm_account()).unwrap();
        match variant {
            "automount-true" => account["automountServiceAccountToken"] = json!(true),
            "automount-default" => account["automountServiceAccountToken"] = Value::Null,
            "foreign-release" => {
                account["metadata"]["annotations"][HELM_NAME] = json!("other-release")
            }
            "foreign-release-namespace" => {
                account["metadata"]["annotations"][HELM_NAMESPACE] = json!("other-workspace")
            }
            "foreign-owner" => {
                account["metadata"]["ownerReferences"] = json!([{
                    "apiVersion": "v1", "kind": "Namespace", "name": NAMESPACE, "uid": "foreign",
                }])
            }
            "unknown-owner" => {
                account["metadata"]["annotations"] = json!({});
                account["metadata"]["labels"][MANAGED_BY] = json!("customer");
            }
            "foreign-binding" => {
                account["metadata"]["annotations"][SOURCE_UID] = json!("previous-sandbox")
            }
            "token-secret" => account["secrets"] = json!([{"name": "old-token"}]),
            _ => account["imagePullSecrets"] = json!([{"name": "customer-registry"}]),
        }
        let account: ServiceAccount = serde_json::from_value(account).unwrap();
        assert!(compatible(&account, &sandbox, &ns).is_err(), "{variant}");
    }
}

#[tokio::test]
async fn incompatible_account_is_preserved_instead_of_force_overwritten() {
    let sandbox = sandbox();
    let ns = namespace(&sandbox);
    let mut original = helm_account();
    original.automount_service_account_token = Some(true);
    let (server, client, accounts, state) = setup(&sandbox, Some(original.clone())).await;
    assert!(
        ensure(&client, &sandbox, &accounts, Some(&ns))
            .await
            .is_err()
    );
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|r| r.method == "GET")
    );
    assert_eq!(state.lock().unwrap().account, Some(original));
}

#[tokio::test]
async fn api_failures_and_create_races_never_force_take_over_an_account() {
    let sandbox = sandbox();
    let ns = namespace(&sandbox);
    for (get_error, create_error) in [(403, 0), (500, 0), (0, 403), (0, 409), (0, 500)] {
        let (server, client, accounts, state) = setup(&sandbox, None).await;
        {
            let mut state = state.lock().unwrap();
            state.get_error = get_error;
            state.create_error = create_error;
        }
        assert!(
            ensure(&client, &sandbox, &accounts, Some(&ns))
                .await
                .is_err()
        );
        assert!(state.lock().unwrap().account.is_none());
        assert!(
            server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .all(|r| r.method != "PATCH" && r.method != "PUT" && r.method != "DELETE")
        );
    }
    let (server, client, accounts, state) = setup(&sandbox, None).await;
    let mut foreign = helm_account();
    foreign
        .metadata
        .annotations
        .as_mut()
        .unwrap()
        .insert(HELM_NAME.into(), "other-release".into());
    state.lock().unwrap().race_account = Some(foreign.clone());
    assert!(
        ensure(&client, &sandbox, &accounts, Some(&ns))
            .await
            .is_err()
    );
    assert!(
        ensure(&client, &sandbox, &accounts, Some(&ns))
            .await
            .is_err()
    );
    assert_eq!(state.lock().unwrap().account, Some(foreign));
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|r| r.method != "PATCH" && r.method != "PUT" && r.method != "DELETE")
    );
}

#[test]
fn reconciler_wires_writer_after_namespace_ensure_and_primary_service_account() {
    let source = include_str!("mod.rs");
    let writer = source.find("sre_writer::ensure(").unwrap();
    assert!(source.find("namespace_ownership::ensure(").unwrap() < writer);
    assert!(
        source[..writer]
            .rfind(".patch(\n            \"sandbox\",")
            .is_some()
    );
    assert!(source[..writer].ends_with("if is_sre_sandbox && name == \"sre\" {\n        "));
}
