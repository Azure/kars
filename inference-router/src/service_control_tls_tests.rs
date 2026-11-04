// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::{
    access_request::{Identity, Request},
    governed_services::GovernedServices,
};
use serde_json::{Value, json};
use std::sync::Arc;

fn fixture() -> (AppState, TlsIdentity) {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let identity: Identity = serde_json::from_value(json!({
        "sandbox":{"namespace":"workspace","name":"agent","uid":"sandbox-uid"},
        "namespace_uid":"runtime-uid","managed":true,
        "task":{"namespace":"workspace","name":"mission","uid":"task-uid"},
        "task_generation":1,"task_authorization":format!("sha256:{}", "a".repeat(64))
    }))
    .unwrap();
    let value = serde_json::to_value(&identity).unwrap();
    let host = service_control::server_name(&value).unwrap();
    let key = rcgen::KeyPair::generate().unwrap();
    let certificate = rcgen::CertificateParams::new(vec![host.clone()])
        .unwrap()
        .self_signed(&key)
        .unwrap();
    let tls = TlsIdentity {
        identity: value,
        server_name: host,
        ca_pem: certificate.pem(),
        certificate_pem: certificate.pem(),
        private_key_pem: key.serialize_pem(),
        expires_at: chrono::Utc::now().timestamp() + 600,
    };
    let mut state = crate::routes::test_state(crate::config::Config::from_env().unwrap());
    state.services = Arc::new(GovernedServices::new(identity, Some("c".repeat(64))));
    (state, tls)
}

#[tokio::test]
async fn control_tls_works_without_observer_and_preserves_auth_scope_and_origin_checks() {
    let (mut state, tls) = fixture();
    assert!(state.services.observer.is_none());
    let bytes = serde_json::to_vec(&tls).unwrap();
    assert!(configuration(&state, &bytes).is_ok());
    let scope = state.services.requests.scope().unwrap();
    let (entry, _) = state
        .services
        .requests
        .record(Request {
            scope_id: scope.id.clone(),
            kind: "egress".into(),
            target: "example.net".into(),
            reason: "Owner approval required".into(),
            tier: None,
            port: Some(443),
        })
        .unwrap();
    Arc::get_mut(&mut state.services).unwrap().allow_ips = Some(vec!["127.0.0.1".parse().unwrap()]);
    let listener = crate::sre_proxy::Listener {
        tcp: TcpListener::bind("127.0.0.1:0").await.unwrap(),
        tls: crate::sre_proxy::tls_from_pem(
            tls.certificate_pem.as_bytes(),
            tls.private_key_pem.as_bytes(),
        )
        .unwrap(),
    };
    let address = listener.tcp.local_addr().unwrap();
    let router =
        crate::routes::access_request_control_routes(state.clone()).with_state(state.clone());
    let server = crate::service_observation_tls::serve(listener, router, "service-control-test");
    let client = |host: &str, trusted: bool| {
        let mut builder = reqwest::Client::builder()
            .no_proxy()
            .https_only(true)
            .tls_built_in_root_certs(false)
            .redirect(reqwest::redirect::Policy::none())
            .resolve(host, address)
            .timeout(std::time::Duration::from_secs(3));
        if trusted {
            builder = builder.add_root_certificate(
                reqwest::Certificate::from_pem(tls.ca_pem.as_bytes()).unwrap(),
            );
        }
        builder.build().unwrap()
    };
    let endpoint = |host: &str, path: &str| format!("https://{host}:{}{path}", address.port());
    let http = client(&tls.server_name, true);
    let url = endpoint(&tls.server_name, "/internal/access-requests");
    for token in [None, Some("agent-admin-token"), Some("observation-token")] {
        let mut request = http.get(&url);
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        assert_eq!(request.send().await.unwrap().status(), 401);
    }
    let snapshot: Value = http
        .get(&url)
        .bearer_auth("c".repeat(64))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(snapshot["entries"][0]["request_id"], entry.request_id);
    assert_eq!(snapshot["scope"]["identity"], tls.identity);
    let decision = endpoint(&tls.server_name, "/internal/access-requests/decision");
    let body =
        |scope: &str| json!({"scope_id":scope,"request_id":entry.request_id,"verdict":"approved"});
    assert_eq!(
        http.post(&decision)
            .bearer_auth("c".repeat(64))
            .json(&body("stale:0"))
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    let approved: Value = http
        .post(&decision)
        .bearer_auth("c".repeat(64))
        .json(&body(&scope.id))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(approved["request"]["status"], "approved");
    assert_eq!(approved["enforcement_changed"], false);
    for path in [
        "/internal/observations/scope",
        "/internal/access-requests/reset",
        "/v1/access-request",
    ] {
        assert_eq!(
            http.get(endpoint(&tls.server_name, path))
                .bearer_auth("c".repeat(64))
                .send()
                .await
                .unwrap()
                .status(),
            404
        );
    }
    assert!(
        client(&tls.server_name, false)
            .get(&url)
            .send()
            .await
            .is_err()
    );
    let wrong = "control-replaced-uid.kars.internal";
    assert!(
        client(wrong, true)
            .get(endpoint(wrong, "/internal/access-requests"))
            .send()
            .await
            .is_err()
    );
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());

    let mut restricted = GovernedServices::new(scope.identity.clone(), Some("c".repeat(64)));
    restricted.allow_ips = Some(vec!["192.0.2.1".parse().unwrap()]);
    state.services = Arc::new(restricted);
    let listener = crate::sre_proxy::Listener {
        tcp: TcpListener::bind("127.0.0.1:0").await.unwrap(),
        tls: crate::sre_proxy::tls_from_pem(
            tls.certificate_pem.as_bytes(),
            tls.private_key_pem.as_bytes(),
        )
        .unwrap(),
    };
    let restricted_address = listener.tcp.local_addr().unwrap();
    let server = crate::service_observation_tls::serve(
        listener,
        crate::routes::access_request_control_routes(state.clone()).with_state(state),
        "restricted-control-test",
    );
    let restricted_client = reqwest::Client::builder()
        .no_proxy()
        .https_only(true)
        .tls_built_in_root_certs(false)
        .redirect(reqwest::redirect::Policy::none())
        .add_root_certificate(reqwest::Certificate::from_pem(tls.ca_pem.as_bytes()).unwrap())
        .resolve(&tls.server_name, restricted_address)
        .build()
        .unwrap();
    assert_eq!(
        restricted_client
            .get(format!(
                "https://{}:{}/internal/access-requests",
                tls.server_name,
                restricted_address.port()
            ))
            .bearer_auth("c".repeat(64))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn standalone_does_not_require_task_control_tls() {
    let state = crate::routes::test_state(crate::config::Config::from_env().unwrap());
    assert!(start(state).await.unwrap().is_none());
}

#[tokio::test]
async fn control_tls_rejects_wrong_identity_expiry_and_missing_private_control_authority() {
    let (mut state, tls) = fixture();
    for field in ["namespace_uid", "task_generation", "task_authorization"] {
        let mut stale = tls.clone();
        stale.identity[field] = json!("changed");
        assert!(configuration(&state, &serde_json::to_vec(&stale).unwrap()).is_err());
    }
    let mut expired = tls.clone();
    expired.expires_at = chrono::Utc::now().timestamp();
    assert!(configuration(&state, &serde_json::to_vec(&expired).unwrap()).is_err());
    assert!(configuration(&state, b"{}").is_err());
    Arc::get_mut(&mut state.services).unwrap().identity_valid = false;
    assert!(configuration(&state, &serde_json::to_vec(&tls).unwrap()).is_err());
    let identity = state.services.requests.scope().unwrap().identity;
    state.services = Arc::new(GovernedServices::new(identity, None));
    assert!(configuration(&state, &serde_json::to_vec(&tls).unwrap()).is_err());
}
