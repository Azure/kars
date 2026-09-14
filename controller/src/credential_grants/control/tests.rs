// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use k8s_openapi::ByteString;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use wiremock::{Mock, MockServer, ResponseTemplate};

const GRANT: &str = "/apis/kars.azure.com/v1alpha1/namespaces/work/karscredentialgrants/workspace";
const SETTINGS: &str = "/api/v1/namespaces/work/secrets/kars-credential-controller-settings";
const PROVIDER: &str = "/api/v1/namespaces/work/secrets/kars-inference-providers";
const DEPLOYMENT: &str = "/apis/apps/v1/namespaces/work/deployments/kars-controller";

#[derive(Default)]
struct State {
    objects: BTreeMap<String, Value>,
    patches: Vec<Value>,
}

async fn fixture() -> (MockServer, Client, Arc<Mutex<State>>, KarsCredentialGrant) {
    let server = MockServer::start().await;
    let grant:KarsCredentialGrant=serde_json::from_value(json!({
        "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsCredentialGrant",
        "metadata":{"name":"workspace","namespace":"work","uid":"grant","generation":1,"resourceVersion":"1"},
        "spec":{"enabled":true,"workspaceUid":"workspace","writers":[],"controller":{"name":"kars-controller","uid":"controller"},
            "integrationStores":[{"secret":{"name":"kars-credential-controller-settings","uid":"settings"},"purpose":"controller-settings"},
                {"secret":{"name":"kars-inference-providers","uid":"provider"},"purpose":"providers"}]}
    })).unwrap();
    let state = Arc::new(Mutex::new(State::default()));
    {
        let mut s = state.lock().unwrap();
        s.objects
            .insert(GRANT.into(), serde_json::to_value(&grant).unwrap());
        s.objects.insert(
            "/api/v1/namespaces/work".into(),
            json!({"metadata":{"name":"work","uid":"workspace","resourceVersion":"1"}}),
        );
        s.objects.insert(SETTINGS.into(),json!({"apiVersion":"v1","kind":"Secret","type":"Opaque",
            "metadata":{"name":"kars-credential-controller-settings","namespace":"work","uid":"settings","resourceVersion":"1"},
            "data":{"configuration":ByteString(serde_json::to_vec(&json!([{"name":"COPILOT_GITHUB_TOKEN","secret":{
                "name":"kars-inference-providers","uid":"provider","key":"COPILOT_GITHUB_TOKEN"}}])).unwrap())}}));
        s.objects.insert(PROVIDER.into(),json!({"apiVersion":"v1","kind":"Secret","type":"Opaque",
            "metadata":{"name":"kars-inference-providers","namespace":"work","uid":"provider","resourceVersion":"1"},
            "data":{"COPILOT_GITHUB_TOKEN":ByteString(b"PRIVATE_OLD".to_vec())}}));
        s.objects.insert(DEPLOYMENT.into(),json!({"apiVersion":"apps/v1","kind":"Deployment",
            "metadata":{"name":"kars-controller","namespace":"work","uid":"controller","resourceVersion":"1"},
            "spec":{"selector":{"matchLabels":{"app":"controller"}},"template":{"metadata":{},"spec":{"containers":[{
                "name":"controller","image":"test:latest"}]}}}}));
    }
    let captured = state.clone();
    Mock::given(|_:&wiremock::Request|true).respond_with(move |request:&wiremock::Request| {
        let mut s=captured.lock().unwrap();let path=request.url.path();
        if request.method=="GET" && let Some(value)=s.objects.get(path) {
            return ResponseTemplate::new(200).set_body_json(value);
        }
        if request.method=="PATCH" && path==DEPLOYMENT {
            let body:Value=request.body_json().unwrap();
            let object=s.objects.get_mut(path).unwrap();
            assert_eq!(object["metadata"]["uid"],body["metadata"]["uid"]);
            assert_eq!(object["metadata"]["resourceVersion"],body["metadata"]["resourceVersion"]);
            object["spec"]["template"]["metadata"]["annotations"]=body["spec"]["template"]["metadata"]["annotations"].clone();
            let result=object.clone();s.patches.push(body);
            return ResponseTemplate::new(200).set_body_json(result);
        }
        ResponseTemplate::new(404).set_body_json(json!({"apiVersion":"v1","kind":"Status","status":"Failure","code":404,"reason":"NotFound"}))
    }).mount(&server).await;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
    (server, client, state, grant)
}

#[tokio::test]
async fn credential_controller_rollout_tracks_referenced_token_rotation_not_grant_status() {
    let (_server, client, state, grant) = fixture().await;
    let first = reconcile(&client, &grant).await.unwrap();
    assert_eq!(state.lock().unwrap().patches.len(), 1);
    assert_eq!(reconcile(&client, &grant).await.unwrap(), first);
    assert_eq!(state.lock().unwrap().patches.len(), 1);
    {
        let mut s = state.lock().unwrap();
        s.objects.get_mut(PROVIDER).unwrap()["metadata"]["resourceVersion"] = "2".into();
        s.objects.get_mut(PROVIDER).unwrap()["data"]["COPILOT_GITHUB_TOKEN"] =
            json!(ByteString(b"PRIVATE_NEW".to_vec()));
    }
    let rotated = reconcile(&client, &grant).await.unwrap();
    assert_ne!(rotated, first);
    assert_eq!(state.lock().unwrap().patches.len(), 2);
    state.lock().unwrap().objects.get_mut(GRANT).unwrap()["metadata"]["resourceVersion"] =
        "status-only".into();
    assert_eq!(reconcile(&client, &grant).await.unwrap(), rotated);
    let s = state.lock().unwrap();
    assert_eq!(s.patches.len(), 2);
    for patch in &s.patches {
        assert!(!patch.to_string().contains("PRIVATE_"));
        assert_eq!(
            patch["spec"]["template"]["spec"]["containers"][0]["env"][0]["valueFrom"]["secretKeyRef"]
                ["name"],
            "kars-inference-providers"
        );
    }
}

#[tokio::test]
async fn credential_controller_validates_references_before_unchanged_revision_fast_path() {
    for fault in ["uid", "missing-key", "type", "purpose"] {
        let (_server, client, state, mut grant) = fixture().await;
        reconcile(&client, &grant).await.unwrap();
        {
            let mut s = state.lock().unwrap();
            let secret = s.objects.get_mut(PROVIDER).unwrap();
            match fault {
                "uid" => secret["metadata"]["uid"] = "replacement".into(),
                "missing-key" => secret["data"] = json!({}),
                "type" => secret["type"] = "kubernetes.io/service-account-token".into(),
                _ => grant.spec.integration_stores[1].purpose = "teams".into(),
            }
        }
        assert!(reconcile(&client, &grant).await.is_err(), "{fault}");
        assert_eq!(state.lock().unwrap().patches.len(), 1);
    }
}
