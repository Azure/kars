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
        if request.method=="POST" && path=="/apis/authentication.k8s.io/v1/selfsubjectreviews" {
            return ResponseTemplate::new(201).set_body_json(json!({
                "apiVersion":"authentication.k8s.io/v1","kind":"SelfSubjectReview",
                "status":{"userInfo":{"username":"system:serviceaccount:work:kars-controller","uid":"root-account"}}
            }));
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

fn qualify_root(state: &mut State, grant: &mut KarsCredentialGrant, profile: &str) {
    crate::private_activation::test_support::install(
        &mut state.objects,
        "work",
        "workspace",
        "root-account",
        &[],
    );
    grant.spec.controller.as_mut().unwrap().uid = "controller-deploy".into();
    state
        .objects
        .insert(GRANT.into(), serde_json::to_value(grant).unwrap());
    state.objects.get_mut("/api/v1/namespaces/work").unwrap()["metadata"]["annotations"]["kars.azure.com/private-profile"] =
        profile.into();
    if profile == "service-accounts" {
        for name in crate::private_activation::bundle()["controllers"]
            .as_array()
            .unwrap()
        {
            let name = name.as_str().unwrap();
            let uid = format!("{name}-uid");
            state.objects.insert(format!("/api/v1/namespaces/kube-system/serviceaccounts/{name}"), json!({
                "apiVersion":"v1","kind":"ServiceAccount",
                "metadata":{"name":name,"namespace":"kube-system","uid":uid,"resourceVersion":"1"}
            }));
            state.objects.get_mut("/api/v1/namespaces/work").unwrap()["metadata"]["annotations"]
                [format!("kars.azure.com/private-{name}-uid")] = uid.into();
        }
    }
}

#[tokio::test]
async fn credential_controller_settings_preserve_private_root_qualification() {
    for profile in ["kcm-certificate", "service-accounts"] {
        let (_server, client, state, mut grant) = fixture().await;
        qualify_root(&mut state.lock().unwrap(), &mut grant, profile);
        let namespace = Api::<Namespace>::all(client.clone())
            .get("work")
            .await
            .unwrap();
        let epoch = crate::private_activation::namespace_epoch(&client, &namespace)
            .await
            .unwrap()
            .unwrap();
        let before = state.lock().unwrap().objects.clone();
        let error = reconcile(&client, &grant).await.unwrap_err();
        assert!(
            error.contains("sealed private template"),
            "{profile}: {error}"
        );
        assert!(state.lock().unwrap().patches.is_empty());
        assert_eq!(state.lock().unwrap().objects, before);
        assert_eq!(
            crate::private_activation::namespace_epoch(&client, &namespace).await,
            Ok(Some(epoch)),
            "settings must not strand a previously qualified private controller"
        );
    }
}

#[tokio::test]
async fn credential_controller_qualified_settings_noop_rechecks_privacy_and_rotation() {
    for fault in ["none", "rotation", "template", "pending", "workspace"] {
        let (_server, client, state, mut grant) = fixture().await;
        let first = reconcile(&client, &grant).await.unwrap();
        {
            let mut state = state.lock().unwrap();
            let mut template = state.objects[DEPLOYMENT]["spec"]["template"].clone();
            template["spec"]["serviceAccountName"] = "kars-controller".into();
            qualify_root(&mut state, &mut grant, "service-accounts");
            state.objects.get_mut(DEPLOYMENT).unwrap()["spec"]["template"] = template.clone();
            state.objects.get_mut("/api/v1/namespaces/work").unwrap()["metadata"]["annotations"]
                ["kars.azure.com/private-root-template-digest"] =
                crate::private_activation::test_support::pod_spec_digest(&template).into();
        }
        assert_eq!(reconcile(&client, &grant).await.unwrap(), first);
        {
            let mut state = state.lock().unwrap();
            match fault {
                "rotation" => {
                    state.objects.get_mut(PROVIDER).unwrap()["metadata"]["resourceVersion"] =
                        "2".into()
                }
                "template" => {
                    state.objects.get_mut(DEPLOYMENT).unwrap()["spec"]["template"]["spec"]["containers"]
                        [0]["image"] = "changed".into()
                }
                "pending" => {
                    state.objects.get_mut("/api/v1/namespaces/work").unwrap()["metadata"]["annotations"]
                        ["kars.azure.com/private-state"] = "Pending".into()
                }
                "workspace" => {
                    state.objects.get_mut("/api/v1/namespaces/work").unwrap()["metadata"]["uid"] =
                        "replacement".into()
                }
                _ => {}
            }
        }
        let before = state.lock().unwrap().objects.clone();
        let result = reconcile(&client, &grant).await;
        assert_eq!(result.is_ok(), fault == "none", "{fault}: {result:?}");
        assert_eq!(state.lock().unwrap().objects, before);
        assert_eq!(state.lock().unwrap().patches.len(), 1);
    }
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
