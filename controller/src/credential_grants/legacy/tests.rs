// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use serde_json::Value;
use std::sync::{Arc, Mutex};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[derive(Default)]
struct State {
    objects: BTreeMap<String, Value>,
    calls: Vec<String>,
    forbidden: Option<String>,
}

async fn fixture() -> (MockServer, Client, Arc<Mutex<State>>, KarsCredentialGrant) {
    let server = MockServer::start().await;
    let state = Arc::new(Mutex::new(State::default()));
    let grant:KarsCredentialGrant=serde_json::from_value(json!({
        "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsCredentialGrant",
        "metadata":{"name":"workspace","namespace":"work","uid":"grant","resourceVersion":"1","generation":1},
        "spec":{"workspaceUid":"work","writers":[],"enabled":true}
    })).unwrap();
    {
        let mut s = state.lock().unwrap();
        s.objects.insert(
            "/api/v1/namespaces/work".into(),
            json!({"metadata":{"name":"work","uid":"work","resourceVersion":"1"}}),
        );
        s.objects.insert("/api/v1/namespaces/work/secrets/kars-workspace-channels".into(),json!({
            "apiVersion":"v1","kind":"Secret","type":"Opaque","metadata":{"name":"kars-workspace-channels","namespace":"work",
                "uid":"legacy-work","resourceVersion":"1"},"data":{"TELEGRAM_BOT_TOKEN":k8s_openapi::ByteString(b"keep".to_vec())}}));
        for (kind, resource) in [
            ("KarsTask", "karstasks"),
            ("KarsTeam", "karsteams"),
            ("KarsSandbox", "karssandboxes"),
        ] {
            s.objects.insert(format!("/apis/kars.azure.com/v1alpha1/namespaces/work/{resource}"),json!({
                "apiVersion":"kars.azure.com/v1alpha1","kind":format!("{kind}List"),"metadata":{},"items":[{
                    "apiVersion":"kars.azure.com/v1alpha1","kind":kind,"metadata":{"name":"retiring","namespace":"work",
                        "uid":format!("{kind}-retiring"),"resourceVersion":"1","deletionTimestamp":"2026-01-01T00:00:00Z"}}]}));
        }
    }
    let captured = state.clone();
    Mock::given(|_:&wiremock::Request|true).respond_with(move |request:&wiremock::Request| {
        assert_eq!(request.method,"GET");
        let mut s=captured.lock().unwrap();let path=request.url.path();s.calls.push(path.into());
        if s.forbidden.as_deref()==Some(path) {return ResponseTemplate::new(403).set_body_json(json!({
            "apiVersion":"v1","kind":"Status","code":403,"reason":"Forbidden","message":"PRIVATE_ERROR"}));}
        if let Some(value)=s.objects.get(path) {return ResponseTemplate::new(200).set_body_json(value);}
        ResponseTemplate::new(404).set_body_json(json!({"apiVersion":"v1","kind":"Status","code":404,"reason":"NotFound"}))
    }).mount(&server).await;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
    (server, client, state, grant)
}

#[tokio::test]
async fn credential_legacy_inventory_ignores_unrelated_terminating_targets_with_or_without_stores()
{
    for has_store in [false, true] {
        let (_server, client, state, grant) = fixture().await;
        if has_store {
            state.lock().unwrap().objects.insert("/api/v1/namespaces/kars-retiring/secrets/retiring-credentials".into(),
                json!({"metadata":{"name":"retiring-credentials","uid":"unrelated","resourceVersion":"1"},"type":"Opaque"}));
        }
        let inventory = inventory(&client, &grant).await.unwrap();
        assert_eq!(inventory.len(), 1);
        assert_eq!(inventory[0].secret.uid, "legacy-work");
        assert!(
            !state
                .lock()
                .unwrap()
                .calls
                .iter()
                .any(|path| path.contains("kars-retiring"))
        );
    }
}

#[tokio::test]
async fn credential_legacy_discovery_checks_secret_before_unrelated_runtime_lifecycle() {
    let (_server, client, state, grant) = fixture().await;
    let list = "/apis/kars.azure.com/v1alpha1/namespaces/work/karstasks";
    {
        let mut s = state.lock().unwrap();
        s.objects.get_mut(list).unwrap()["items"][0]["metadata"]
            .as_object_mut()
            .unwrap()
            .remove("deletionTimestamp");
        s.objects.insert(
            "/api/v1/namespaces/kars-retiring".into(),
            json!({"metadata":{"name":"kars-retiring","uid":"runtime",
            "resourceVersion":"1","deletionTimestamp":"2026-01-01T00:00:00Z"}}),
        );
    }
    assert_eq!(inventory(&client, &grant).await.unwrap().len(), 1);
    assert!(
        !state
            .lock()
            .unwrap()
            .calls
            .iter()
            .any(|path| path == "/api/v1/namespaces/kars-retiring")
    );
    state.lock().unwrap().objects.insert("/api/v1/namespaces/kars-retiring/secrets/retiring-credentials".into(),
        json!({"metadata":{"name":"retiring-credentials","uid":"unrelated","resourceVersion":"1"},"type":"Opaque"}));
    assert_eq!(inventory(&client, &grant).await.unwrap().len(), 1);
    state.lock().unwrap().forbidden =
        Some("/api/v1/namespaces/kars-retiring/secrets/retiring-credentials".into());
    let error = inventory(&client, &grant).await.unwrap_err();
    assert!(!error.contains("PRIVATE_ERROR"));
}

#[tokio::test]
async fn credential_selected_legacy_owner_and_namespace_lifecycle_still_fail_closed() {
    for terminating_owner in [true, false] {
        let (_server, client, state, grant) = fixture().await;
        let target = CredentialTarget {
            kind: "KarsTask".into(),
            namespace: "work".into(),
            name: "retiring".into(),
            uid: "task".into(),
        };
        let mut owner = json!({"metadata":{"name":"retiring","namespace":"work","uid":"task","resourceVersion":"1"}});
        if terminating_owner {
            owner["metadata"]["deletionTimestamp"] = "2026-01-01T00:00:00Z".into();
        }
        {
            let mut s = state.lock().unwrap();
            s.objects.insert(
                "/apis/kars.azure.com/v1alpha1/namespaces/work/karstasks/retiring".into(),
                owner,
            );
            s.objects.insert("/api/v1/namespaces/kars-retiring/secrets/retiring-credentials".into(),json!({
                "metadata":{"name":"retiring-credentials","uid":"selected","resourceVersion":"1"},"type":"Opaque"}));
            s.objects.insert(
                "/api/v1/namespaces/kars-retiring".into(),
                json!({"metadata":{"name":"kars-retiring",
                "uid":"runtime","resourceVersion":"1","deletionTimestamp":"2026-01-01T00:00:00Z"}}),
            );
        }
        assert!(
            import_values(
                &client,
                &grant,
                "kars-credential-input-task-retiring",
                Some(&target)
            )
            .await
            .is_err()
        );
    }
}
