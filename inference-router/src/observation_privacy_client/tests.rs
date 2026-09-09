// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub(crate) struct Control {
    pub(crate) fault: String,
    pub(crate) calls: Vec<wire::Request>,
}

pub(crate) struct Verifier {
    pub(crate) endpoint: wire::Endpoint,
    pub(crate) control: Arc<Mutex<Control>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Verifier {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn respond(
    State(control): State<Arc<Mutex<Control>>>,
    headers: HeaderMap,
    Json(request): Json<wire::Request>,
) -> Response {
    assert_eq!(
        headers.get("authorization").unwrap(),
        &format!("Bearer {}", "o".repeat(64))
    );
    let fault = {
        let mut control = control.lock().unwrap();
        control.calls.push(request.clone());
        control.fault.clone()
    };
    if fault == "delay" {
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    }
    if fault == "deny" {
        return (StatusCode::FORBIDDEN, Json(json!({"allowed":false}))).into_response();
    }
    if fault == "redirect" {
        return (
            StatusCode::TEMPORARY_REDIRECT,
            [("location", "http://untrusted.invalid/secret")],
        )
            .into_response();
    }
    let mut proof = wire::Proof::allow(&request, request.epoch.clone());
    match fault.as_str() {
        "nonce" => proof.nonce = "f".repeat(64),
        "digest" => proof.request_digest = "forged".into(),
        "epoch" => proof.epoch = Some("wrong".into()),
        "purpose" => proof.purpose = "admin".into(),
        _ => {}
    }
    Json(proof).into_response()
}

impl Verifier {
    pub(crate) async fn start() -> Arc<Self> {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = rcgen::CertificateParams::new(vec!["privacy-core-uid.kars.internal".into()])
            .unwrap()
            .self_signed(&key)
            .unwrap();
        let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = tcp.local_addr().unwrap();
        let endpoint = wire::Endpoint {
            capability: wire::CAPABILITY.into(),
            namespace: "core".into(),
            namespace_uid: "core-uid".into(),
            controller_uid: "core-sa".into(),
            service_uid: "service".into(),
            port: address.port(),
            descriptor_uid: "descriptor".into(),
            tls_uid: "tls-uid".into(),
            tls_version: "1".into(),
            server_name: "privacy-core-uid.kars.internal".into(),
            ca_pem: cert.pem(),
            expires_at: chrono::Utc::now().timestamp() + 3600,
        };
        let listener = crate::private_tls::Listener {
            tcp,
            tls: crate::private_tls::tls_from_pem(
                cert.pem().as_bytes(),
                key.serialize_pem().as_bytes(),
            )
            .unwrap(),
        };
        let control = Arc::new(Mutex::new(Control::default()));
        let router = Router::new()
            .route(wire::PATH, post(respond))
            .with_state(control.clone());
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Arc::new(Self {
            endpoint,
            control,
            task,
        })
    }
    pub(crate) fn objects(&self) -> Vec<(String, Value)> {
        let ep = &self.endpoint;
        vec![
            (
                "/api/v1/namespaces/core".into(),
                json!({"metadata":{"name":"core","uid":"core-uid","resourceVersion":"1"}}),
            ),
            (
                "/api/v1/namespaces/core/serviceaccounts/kars-controller".into(),
                json!({
                "metadata":{"name":"kars-controller","namespace":"core","uid":"core-sa","resourceVersion":"1"}}),
            ),
            (
                format!("/api/v1/namespaces/core/configmaps/{}", wire::DESCRIPTOR),
                json!({
                "apiVersion":"v1","kind":"ConfigMap","metadata":{"name":wire::DESCRIPTOR,"namespace":"core","uid":"descriptor","resourceVersion":"1",
                    "annotations":{wire::CONTROLLER_UID:"core-sa",wire::NAMESPACE_UID:"core-uid"}},
                "data":{"config.json":serde_json::to_string(ep).unwrap()}}),
            ),
            (
                format!("/api/v1/namespaces/core/services/{}", wire::SERVICE),
                json!({"apiVersion":"v1","kind":"Service",
                "metadata":{"name":wire::SERVICE,"namespace":"core","uid":"service","resourceVersion":"1"},
                "spec":{"type":"ClusterIP","clusterIP":"127.0.0.1","ports":[{"port":ep.port,"protocol":"TCP","targetPort":ep.port}],
                    "selector":{"app.kubernetes.io/name":"kars","app.kubernetes.io/component":"controller",wire::REVISION_LABEL:ep.revision()}}}),
            ),
        ]
    }
}

fn request(endpoint: wire::Endpoint) -> wire::Request {
    wire::Request {
        capability: wire::CAPABILITY.into(),
        purpose: wire::PURPOSE.into(),
        target: wire::Target {
            workspace: "work".into(),
            workspace_uid: "work-uid".into(),
            name: "agent".into(),
            uid: "target".into(),
            namespace_uid: "runtime".into(),
        },
        grant_uid: "grant".into(),
        grant_generation: 1,
        recipients: vec![crate::service_observer::Recipient {
            namespace: "bridge".into(),
            namespace_uid: "bridge".into(),
            name: "bff".into(),
            uid: "writer".into(),
        }],
        credential_version: "secret:1".into(),
        identity: json!({"managed":true}),
        scope_id: "scope".into(),
        operation: Operation::Learned,
        epoch: None,
        nonce: "a".repeat(64),
        verifier: endpoint,
    }
}

#[tokio::test]
async fn observation_privacy_client_pins_tls_and_rejects_replayed_wrong_purpose_epoch_and_redirect_proofs()
 {
    let verifier = Verifier::start().await;
    let request = request(verifier.endpoint.clone());
    let address = SocketAddr::new("127.0.0.1".parse().unwrap(), verifier.endpoint.port);
    let token = "o".repeat(64);
    exchange(&verifier.endpoint, address, &token, &request)
        .await
        .unwrap();
    for fault in ["nonce", "digest", "epoch", "purpose", "deny", "redirect"] {
        verifier.control.lock().unwrap().fault = fault.into();
        assert!(
            exchange(&verifier.endpoint, address, &token, &request)
                .await
                .is_err(),
            "{fault}"
        );
    }
    verifier.control.lock().unwrap().fault.clear();
    let mut wrong = verifier.endpoint.clone();
    wrong.server_name = "privacy-other-uid.kars.internal".into();
    assert!(exchange(&wrong, address, &token, &request).await.is_err());
}
