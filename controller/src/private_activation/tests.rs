// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::credential_grant::KarsCredentialGrant;
use k8s_openapi::api::core::v1::{Namespace, Pod};
use kube::Client;
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn private_activation_checks_exact_policy_binding_epoch_and_root_incarnations() {
    let server = MockServer::start().await;
    let mut objects = BTreeMap::new();
    let activation = test_support::install(
        &mut objects,
        "core",
        "core-uid",
        "controller",
        &[("work", "work-uid"), ("bridge", "bridge-uid")],
    );
    let grant: KarsCredentialGrant = serde_json::from_value(json!({
        "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsCredentialGrant",
        "metadata":{"name":"workspace","namespace":"work","uid":"grant","resourceVersion":"1"},
        "spec":{"workspaceUid":"work-uid","writers":[{"namespace":"bridge","name":"bff","uid":"writer"}],
            "privateActivation":activation}
    })).unwrap();
    let baseline = objects.clone();
    let objects = Arc::new(Mutex::new(objects));
    let captured = objects.clone();
    Mock::given(|_: &wiremock::Request| true)
        .respond_with(move |r: &wiremock::Request| {
            if r.method == "POST" && r.url.path().ends_with("/selfsubjectreviews") {
                return ResponseTemplate::new(201).set_body_json(json!({
                    "apiVersion":"authentication.k8s.io/v1","kind":"SelfSubjectReview",
                    "status":{"userInfo":{"username":"system:serviceaccount:core:kars-controller","uid":"controller"}}
                }));
            }
            assert_eq!(r.method, "GET");
            if r.url.path().ends_with("/pods") {
                let namespace = r.url.path().split('/').nth(4).unwrap();
                let items: Vec<_> = captured.lock().unwrap().values().filter(|value|
                    value["kind"] == "Pod" && value["metadata"]["namespace"] == namespace).cloned().collect();
                return ResponseTemplate::new(200).set_body_json(json!({
                    "apiVersion":"v1","kind":"PodList","metadata":{},"items":items
                }));
            }
            captured.lock().unwrap().get(r.url.path()).map_or_else(
                || ResponseTemplate::new(404),
                |value| ResponseTemplate::new(200).set_body_json(value),
            )
        })
        .mount(&server)
        .await;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
    verify(&client, &grant).await.unwrap();
    objects.lock().unwrap().insert("/api/v1/namespaces/work/pods/unexplained".into(), json!({
        "apiVersion":"v1","kind":"Pod","metadata":{"name":"unexplained","namespace":"work",
            "uid":"foreign-pod","resourceVersion":"1"},
        "spec":{"containers":[{"name":"reader","image":"fixture"}],
            "volumes":[{"name":"identity","secret":{"secretName":"router-services-observer-identity"}}]}
    }));
    assert!(verify(&client, &grant).await.is_err());
    *objects.lock().unwrap() = baseline.clone();
    for (path, pointer, value) in [
        (
            "/apis/admissionregistration.k8s.io/v1/validatingadmissionpolicies/kars-private-consumption",
            "/spec/failurePolicy",
            json!("Ignore"),
        ),
        (
            "/apis/admissionregistration.k8s.io/v1/validatingadmissionpolicies/kars-private-consumption",
            "/spec/validations/0/expression",
            json!("true"),
        ),
        (
            "/apis/admissionregistration.k8s.io/v1/validatingadmissionpolicies/kars-private-consumption",
            "/status/observedGeneration",
            json!(0),
        ),
        (
            "/apis/admissionregistration.k8s.io/v1/validatingadmissionpolicybindings/kars-private-consumption",
            "/spec/validationActions",
            json!(["Audit"]),
        ),
        (
            "/api/v1/namespaces/work",
            "/metadata/uid",
            json!("replacement"),
        ),
        (
            "/api/v1/namespaces/core/serviceaccounts/kars-controller",
            "/metadata/uid",
            json!("replacement"),
        ),
        (
            "/apis/apps/v1/namespaces/core/deployments/kars-controller",
            "/metadata/uid",
            json!("replacement"),
        ),
    ] {
        *objects.lock().unwrap() = baseline.clone();
        *objects
            .lock()
            .unwrap()
            .get_mut(path)
            .unwrap()
            .pointer_mut(pointer)
            .unwrap() = value;
        assert!(verify(&client, &grant).await.is_err(), "{path} {pointer}");
    }
    *objects.lock().unwrap() = baseline.clone();
    objects
        .lock()
        .unwrap()
        .get_mut("/api/v1/namespaces/work")
        .unwrap()["metadata"]["annotations"][EPOCH] = "unqualified".into();
    assert!(verify(&client, &grant).await.is_err());
    *objects.lock().unwrap() = baseline;
    objects.lock().unwrap().remove("/apis/admissionregistration.k8s.io/v1/validatingadmissionpolicybindings/kars-private-consumption");
    assert!(verify(&client, &grant).await.is_err());
    let mut retired = grant.clone();
    retired.spec.writers.clear();
    verify(&client, &retired).await.unwrap();
}

#[test]
fn private_activation_material_inventory_includes_unlabelled_and_terminating_consumers_not_legacy_agent_tokens()
 {
    for container in ["containers", "initContainers", "ephemeralContainers"] {
        let mut pod = json!({"metadata":{"deletionTimestamp":"2026-01-01T00:00:00Z"},
            "spec":{"containers":[{"name":"agent","image":"fixture"}]}});
        pod["spec"][container] = json!([{"name":"reader","image":"fixture",
            "envFrom":[{"secretRef":{"name":"router-services-observer-identity"}}]}]);
        let pod: Pod = serde_json::from_value(pod).unwrap();
        assert!(private_material(&pod));
    }
    for secret in bundle()["secrets"].as_array().unwrap() {
        let pod: Pod = serde_json::from_value(json!({"metadata":{},"spec":{
            "containers":[{"name":"agent","image":"fixture"}],
            "volumes":[{"name":"private","projected":{"sources":[{"secret":{"name":secret}}]}}]
        }}))
        .unwrap();
        assert!(private_material(&pod));
    }
    let legacy: Pod = serde_json::from_value(json!({"metadata":{},"spec":{
        "containers":[{"name":"agent","image":"fixture"}],
        "volumes":[{"name":"agent","secret":{"secretName":"router-admin-token"}}]
    }}))
    .unwrap();
    assert!(!private_material(&legacy));
}

#[test]
fn private_activation_rsa_rotation_compares_keys_not_pem_encoding() {
    use rsa::{RsaPrivateKey, pkcs1::EncodeRsaPrivateKey, pkcs8::EncodePrivateKey};
    let first = RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 1024).unwrap();
    let second = RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 1024).unwrap();
    let one = first.to_pkcs1_pem(Default::default()).unwrap();
    let same = first.to_pkcs8_pem(Default::default()).unwrap();
    let other = second.to_pkcs8_pem(Default::default()).unwrap();
    assert!(!different_rsa_keys(&one, &same).unwrap());
    assert!(different_rsa_keys(&one, &other).unwrap());
}

#[tokio::test]
async fn private_activation_absence_does_not_require_a_bundle_for_ordinary_namespaces() {
    let server = MockServer::start().await;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
    let namespace: Namespace = serde_json::from_value(json!({
        "metadata":{"name":"ordinary","uid":"ordinary-uid","resourceVersion":"1"}
    }))
    .unwrap();
    assert!(
        namespace_epoch(&client, &namespace)
            .await
            .unwrap()
            .is_none()
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}
