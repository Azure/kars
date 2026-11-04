// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use wiremock::{Mock, MockServer, ResponseTemplate};

const NS: &str = "/api/v1/namespaces/kars-system";
const TLS: &str = "/api/v1/namespaces/kars-system/secrets/budget-tls";
const ROOT: &str = "/apis/apps/v1/namespaces/kars-system/deployments/kars-controller";
const ADOPTION: &str = "/api/v1/namespaces/kars-system/configmaps/kars-sre-helm-adoption";

fn fixture() -> (
    BTreeMap<String, Value>,
    Settings,
    KarsSRERegistration,
    KarsSandbox,
    String,
) {
    let (_, mut reg) = super::super::tests::fixture();
    reg.spec.controller.deployment.uid = "controller-deploy".into();
    // Recovery must not depend on SRE already being Ready.
    reg.status =
        Some(serde_json::from_value(json!({"phase":"Blocked","observedGeneration":1})).unwrap());
    let settings = Settings {
        accounting_namespace: "kars-system".into(),
        catalog_name: "contracts".into(),
        address: "0.0.0.0:9447".into(),
        tls_secret: "budget-tls".into(),
        router_image_digest: format!("sha256:{}", "a".repeat(64)),
        first_install_router_digest: None,
    };
    let mut source: KarsSandbox = serde_json::from_value(json!({
        "apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsSandbox",
        "metadata":{"name":"sre","namespace":"kars-system","uid":"source-uid"},
        "spec":{"inferenceRef":{"name":"sre-inference"}}
    }))
    .unwrap();
    source.spec.inference_budget_ref = Some(
        crate::inference_budget::builtin::binding(
            &reg.spec.first_install_funding.as_ref().unwrap().grant,
            serde_json::from_value(
                json!({"name":"account","namespace":"kars-system","uid":"account-uid"}),
            )
            .unwrap(),
        )
        .unwrap(),
    );
    let mut objects = BTreeMap::new();
    crate::private_activation::test_support::install(
        &mut objects,
        "kars-system",
        "workspace-uid",
        "root-account-uid",
        &[],
    );
    let root = objects.get_mut(ROOT).unwrap();
    root["spec"]["template"]["spec"]["containers"][0]["env"] = json!([
        {"name":"KARS_INFERENCE_BUDGET_ENABLED","value":"true"},
        {"name":"KARS_INFERENCE_BUDGET_TLS_SECRET","value":"budget-tls"}
    ]);
    let template_digest =
        crate::private_activation::test_support::pod_spec_digest(&root["spec"]["template"]);
    let identity =
        crate::providers::sre_tls::issue_for(vec!["kars-inference-budget.kars-system.svc".into()])
            .unwrap();
    let key = rcgen::KeyPair::from_pem(&identity.private_key).unwrap();
    let key_digest = crate::providers::signing::sha256_hex(&key.public_key_der());
    let namespace = objects.get_mut(NS).unwrap();
    let revision = namespace["metadata"]["annotations"]["kars.azure.com/private-bundle-revision"]
        .as_str()
        .unwrap()
        .to_owned();
    for (field, value) in [
        ("root-template-digest", template_digest.as_str()),
        ("budget-namespace", "kars-system"),
        ("budget-namespace-uid", "workspace-uid"),
        ("budget-tls-name", "budget-tls"),
        ("budget-tls-uid", "tls-uid"),
        ("budget-tls-version", "2"),
        ("budget-key", key_digest.as_str()),
        ("budget-qualified-key", key_digest.as_str()),
        ("budget-qualified-secret", "tls-uid"),
        ("budget-qualified-bundle", revision.as_str()),
    ] {
        namespace["metadata"]["annotations"][format!("kars.azure.com/private-{field}")] =
            json!(value);
    }
    let secret = Secret {
        metadata: serde_json::from_value(json!({"name":"budget-tls","namespace":"kars-system", "uid":"tls-uid", "resourceVersion":"2",
            "annotations":{"kars.azure.com/inference-budget-tls":"v1"}})).unwrap(),
        type_: Some("kubernetes.io/tls".into()),
        data: Some(BTreeMap::from([
            ("tls.crt".into(), k8s_openapi::ByteString(identity.certificate.into_bytes())),
            ("tls.key".into(), k8s_openapi::ByteString(identity.private_key.into_bytes())),
        ])), ..Default::default()
    };
    objects.insert(TLS.into(), serde_json::to_value(secret).unwrap());
    objects.insert(ADOPTION.into(), json!({"apiVersion":"v1","kind":"ConfigMap","immutable":true,
        "metadata":{"name":"kars-sre-helm-adoption","namespace":"kars-system","uid":"adoption-uid","resourceVersion":"1",
            "annotations":{"meta.helm.sh/release-name":"kars","meta.helm.sh/release-namespace":"kars-system"}},
        "data":{"intentUID":"intent-uid","valuesDigest":"a".repeat(64)}}));
    (objects, settings, reg, source, identity.ca)
}

async fn client(objects: BTreeMap<String, Value>) -> (MockServer, Client) {
    let server = MockServer::start().await;
    Mock::given(|_: &wiremock::Request| true).respond_with(move |request: &wiremock::Request| {
        let path = request.url.path();
        let response = if let Some(value) = objects.get(path) {
            value.clone()
        } else if path.ends_with("/selfsubjectreviews") {
            assert_eq!(request.method, "POST");
            json!({"apiVersion":"authentication.k8s.io/v1","kind":"SelfSubjectReview",
                "status":{"userInfo":{"username":"system:serviceaccount:kars-system:kars-controller","uid":"root-account-uid"}}})
        } else if path.ends_with("/subjectaccessreviews") {
            assert_eq!(request.method, "POST");
            json!({"apiVersion":"authorization.k8s.io/v1","kind":"SubjectAccessReview","status":{"allowed":false,"denied":true}})
        } else if path.ends_with("/pods") {
            json!({"apiVersion":"v1","kind":"PodList","metadata":{},"items":[]})
        } else {
            return ResponseTemplate::new(404).set_body_json(json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"NotFound","message":"Not found","code":404}));
        };
        ResponseTemplate::new(200).set_body_json(response)
    }).mount(&server).await;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
    (server, client)
}

#[tokio::test]
async fn sealed_ca_needs_no_adoption_or_private_activation() {
    let (_, settings, reg, source, ca) = fixture();
    let (server, client) = client(BTreeMap::new()).await;
    validate_ca(
        &client,
        &settings,
        &reg,
        &source,
        Some(&format!(" \n{ca}")),
        Some(&ca),
    )
    .await
    .unwrap();
    assert!(server.received_requests().await.unwrap().is_empty());
    for (original, current) in [
        (None, Some(ca.as_str())),
        (Some(ca.as_str()), None),
        (Some(""), Some("")),
        (None, None),
    ] {
        assert!(
            validate_ca(&client, &settings, &reg, &source, original, current)
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn qualified_rotation_accepts_new_ca_without_changing_sealed_intent_or_funding() {
    let (objects, settings, reg, source, ca) = fixture();
    let original = crate::providers::sre_tls::issue().unwrap().ca;
    assert_ne!(original, ca);
    let before = serde_json::to_value((&reg, &source)).unwrap();
    let (server, client) = client(objects).await;
    validate_ca(
        &client,
        &settings,
        &reg,
        &source,
        Some(&original),
        Some(&ca),
    )
    .await
    .unwrap();
    assert_eq!(serde_json::to_value((&reg, &source)).unwrap(), before);
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|r| r.method == "GET"
                || r.url.path().ends_with("/selfsubjectreviews")
                || r.url.path().ends_with("/subjectaccessreviews"))
    );
}

#[tokio::test]
async fn rotation_requires_adoption_current_qualification_and_exact_live_identity() {
    let (baseline, settings, reg, source, ca) = fixture();
    for (path, pointer, value) in [
        (ADOPTION, "/data/intentUID", json!("wrong-intent")),
        (ADOPTION, "/immutable", json!(false)),
        (
            ADOPTION,
            "/metadata/annotations/meta.helm.sh~1release-name",
            json!("other"),
        ),
        (
            NS,
            "/metadata/annotations/kars.azure.com~1private-enabled",
            json!("false"),
        ),
        (
            NS,
            "/metadata/annotations/kars.azure.com~1private-state",
            json!("Pending"),
        ),
        (
            NS,
            "/metadata/annotations/kars.azure.com~1private-bundle-revision",
            json!("stale"),
        ),
        (
            NS,
            "/metadata/annotations/kars.azure.com~1private-budget-key",
            json!("b".repeat(64)),
        ),
        (
            NS,
            "/metadata/annotations/kars.azure.com~1private-budget-qualified-key",
            json!("b".repeat(64)),
        ),
        (
            NS,
            "/metadata/annotations/kars.azure.com~1private-budget-qualified-secret",
            json!("other"),
        ),
        (
            NS,
            "/metadata/annotations/kars.azure.com~1private-budget-qualified-bundle",
            json!("stale"),
        ),
        (TLS, "/metadata/uid", json!("replaced")),
        (TLS, "/metadata/resourceVersion", json!("3")),
        (TLS, "/type", json!("Opaque")),
        (ROOT, "/metadata/uid", json!("replaced")),
        (
            ROOT,
            "/spec/template/spec/containers/0/image",
            json!("replaced"),
        ),
    ] {
        let mut objects = baseline.clone();
        *objects.get_mut(path).unwrap().pointer_mut(pointer).unwrap() = value;
        let (_server, client) = client(objects).await;
        assert!(
            validate_ca(
                &client,
                &settings,
                &reg,
                &source,
                Some("sealed-ca"),
                Some(&ca)
            )
            .await
            .is_err(),
            "{path} {pointer}"
        );
    }
    for path in [ADOPTION, NS, TLS] {
        let mut objects = baseline.clone();
        objects.remove(path);
        let (_server, client) = client(objects).await;
        assert!(
            validate_ca(
                &client,
                &settings,
                &reg,
                &source,
                Some("sealed-ca"),
                Some(&ca)
            )
            .await
            .is_err()
        );
    }
    let (_server, client) = client(baseline).await;
    let mut unbound = source.clone();
    unbound.spec.inference_budget_ref = None;
    assert!(
        validate_ca(
            &client,
            &settings,
            &reg,
            &unbound,
            Some("sealed-ca"),
            Some(&ca)
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn rotated_ca_must_validate_the_pinned_key_and_budget_server_name() {
    let (objects, settings, reg, source, ca) = fixture();
    let unrelated = crate::providers::sre_tls::issue().unwrap();
    let (server, client) = client(objects.clone()).await;
    for invalid in [
        unrelated.ca.clone(),
        format!("{ca}{}", unrelated.ca),
        String::new(),
        "not a certificate".into(),
        format!("{ca}{}", unrelated.private_key),
    ] {
        assert!(
            validate_ca(
                &client,
                &settings,
                &reg,
                &source,
                Some("sealed-ca"),
                Some(&invalid)
            )
            .await
            .is_err()
        );
    }
    drop(server);
    let namespace: Namespace = serde_json::from_value(objects[NS].clone()).unwrap();
    let secret: Secret = serde_json::from_value(objects[TLS].clone()).unwrap();
    let mut replaced_key = secret.clone();
    replaced_key.data.as_mut().unwrap().insert(
        "tls.key".into(),
        k8s_openapi::ByteString(unrelated.private_key.into_bytes()),
    );
    assert!(verify_identity(&namespace, &replaced_key, &settings, &reg, &ca).is_err());
    // Even with matching reviewed key/metadata, a certificate for another service is invalid.
    let mut wrong_name = secret;
    wrong_name.data.as_mut().unwrap().insert(
        "tls.crt".into(),
        k8s_openapi::ByteString(unrelated.certificate.into_bytes()),
    );
    wrong_name.data.as_mut().unwrap().insert(
        "tls.key".into(),
        replaced_key.data.unwrap().remove("tls.key").unwrap(),
    );
    let key = rcgen::KeyPair::from_pem(
        std::str::from_utf8(&wrong_name.data.as_ref().unwrap()["tls.key"].0).unwrap(),
    )
    .unwrap();
    let digest = crate::providers::signing::sha256_hex(&key.public_key_der());
    let mut wrong_namespace = namespace;
    for name in ["budget-key", "budget-qualified-key"] {
        wrong_namespace
            .metadata
            .annotations
            .as_mut()
            .unwrap()
            .insert(format!("kars.azure.com/private-{name}"), digest.clone());
    }
    assert!(
        verify_identity(
            &wrong_namespace,
            &wrong_name,
            &settings,
            &reg,
            &unrelated.ca
        )
        .is_err()
    );
}
