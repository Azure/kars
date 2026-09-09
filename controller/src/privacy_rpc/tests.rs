// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use serde_json::Value;
use std::sync::Mutex;

mod boundaries;
mod fixture;
mod lifecycle;
use fixture::*;

struct Rig {
    _kube: wiremock::MockServer,
    task: tokio::task::JoinHandle<()>,
    client: reqwest::Client,
    origin: String,
    state: Arc<ServerState>,
    data: Arc<Mutex<Data>>,
    request: wire::Request,
}
impl Drop for Rig {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Rig {
    async fn new(active: bool) -> Self {
        let (kube, state, data, mut request) = fixture().await;
        let issued =
            crate::providers::sre_tls::issue_for(vec![request.verifier.server_name.clone()])
                .unwrap();
        request.verifier.ca_pem = issued.ca.clone();
        {
            let mut d = data.lock().unwrap();
            if active {
                request.epoch = Some(enroll(&mut d));
            }
            bind(&mut d, &request);
            d.objects
                .get_mut(&format!(
                    "/api/v1/namespaces/kars-system/configmaps/{}",
                    wire::DESCRIPTOR
                ))
                .unwrap()["data"]["config.json"] =
                serde_json::to_string(&request.verifier).unwrap().into();
            d.objects
                .get_mut(&format!(
                    "/api/v1/namespaces/kars-system/services/{}",
                    wire::SERVICE
                ))
                .unwrap()["spec"]["selector"][wire::REVISION_LABEL] =
                request.verifier.revision().into();
        }
        *state.endpoint.write().await = Some(request.verifier.clone());
        let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = tcp.local_addr().unwrap();
        let listener = crate::private_tls::Listener {
            tcp,
            tls: crate::private_tls::tls_from_pem(
                issued.certificate.as_bytes(),
                issued.private_key.as_bytes(),
            )
            .unwrap(),
        };
        let router = app(state.clone());
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let client = reqwest::Client::builder()
            .no_proxy()
            .https_only(true)
            .tls_built_in_root_certs(false)
            .add_root_certificate(reqwest::Certificate::from_pem(issued.ca.as_bytes()).unwrap())
            .resolve(&request.verifier.server_name, address)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(12))
            .build()
            .unwrap();
        let origin = format!(
            "https://{}:{}",
            request.verifier.server_name,
            address.port()
        );
        Self {
            _kube: kube,
            task,
            client,
            origin,
            state,
            data,
            request,
        }
    }
    async fn call(&self, request: &wire::Request, token: &str) -> (reqwest::StatusCode, Value) {
        let response = self
            .client
            .post(format!("{}{}", self.origin, wire::PATH))
            .bearer_auth(token)
            .json(request)
            .send()
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.bytes().await.unwrap();
        let text = String::from_utf8_lossy(&bytes);
        for value in [
            TOKEN,
            "PRIVATE_ALIAS",
            "PRIVATE_ERROR",
            "PRIVATE KEY",
            "certificatePem",
            "config.json",
        ] {
            assert!(!text.contains(value), "{value}");
        }
        (status, serde_json::from_slice(&bytes).unwrap())
    }
}

#[tokio::test]
async fn privacy_rpc_active_sre_uses_full_live_proof_without_mutation_or_secret_response() {
    let rig = Rig::new(true).await;
    let (status, value) = rig.call(&rig.request, TOKEN).await;
    assert_eq!(status, reqwest::StatusCode::OK, "{value}");
    let proof: wire::Proof = serde_json::from_value(value).unwrap();
    assert!(proof.matches(&rig.request));
    let data = rig.data.lock().unwrap();
    assert!(data.calls.iter().any(|(_, path, _)| path == ALIASES));
    assert!(
        data.calls
            .iter()
            .any(|(_, path, _)| path.contains("/validatingadmissionpolicies/"))
    );
    assert!(
        data.calls
            .iter()
            .any(|(_, path, _)| path.ends_with("/serviceaccounts/sre-api-router"))
    );
    for verb in ["get", "list", "watch"] {
        assert!(
            data.calls
                .iter()
                .any(|(_, _, body)| body["spec"]["resourceAttributes"]["verb"] == verb)
        );
    }
    assert!(data.calls.iter().all(|(method, path, _)| method == "GET"
        || path.ends_with("/subjectaccessreviews")
        || path.ends_with("/selfsubjectreviews")));
    assert!(
        data.calls
            .iter()
            .filter(|(_, path, _)| path.contains("/secrets/"))
            .all(|(_, path, _)| path == SOURCE || path.ends_with(wire::SECRET))
    );
}

#[tokio::test]
async fn privacy_rpc_has_no_positive_cache_after_alias_admission_or_legacy_denial_loss() {
    for fault in ["alias", "policy", "allowed"] {
        let rig = Rig::new(true).await;
        assert_eq!(
            rig.call(&rig.request, TOKEN).await.0,
            reqwest::StatusCode::OK
        );
        {
            let mut d = rig.data.lock().unwrap();
            match fault {
                "alias" => d.alias = true,
                "policy" => d.policy = true,
                _ => d.allowed = true,
            }
        }
        assert_eq!(
            rig.call(&rig.request, TOKEN).await.0,
            reqwest::StatusCode::FORBIDDEN,
            "{fault}"
        );
    }
}

#[tokio::test]
async fn privacy_rpc_current_uid_generation_epoch_version_and_recipient_loss_deny() {
    for (path, pointer, value) in [
        (SANDBOX, "/metadata/uid", json!("replacement")),
        (GRANT, "/metadata/uid", json!("replacement")),
        (GRANT, "/metadata/generation", json!(2)),
        (GRANT, "/status/conditions/0/status", json!("False")),
        (
            "/api/v1/namespaces/workspace",
            "/metadata/uid",
            json!("replacement"),
        ),
        (
            "/api/v1/namespaces/kars-agent",
            "/metadata/uid",
            json!("replacement"),
        ),
        (
            "/api/v1/namespaces/bridge",
            "/metadata/uid",
            json!("replacement"),
        ),
        (
            "/api/v1/namespaces/bridge/serviceaccounts/bff",
            "/metadata/uid",
            json!("replacement"),
        ),
        (
            "/api/v1/namespaces/kars-sre/serviceaccounts/sre-api-router",
            "/metadata/uid",
            json!("replacement"),
        ),
        (
            "/api/v1/namespaces/kars-system/serviceaccounts/kars-controller",
            "/metadata/uid",
            json!("replacement"),
        ),
        (
            "/api/v1/namespaces/kars-system/secrets/kars-observation-privacy-tls",
            "/metadata/uid",
            json!("replacement"),
        ),
        (
            "/api/v1/namespaces/kars-system/configmaps/kars-observation-privacy",
            "/metadata/uid",
            json!("replacement"),
        ),
        (
            "/api/v1/namespaces/kars-system/services/kars-observation-privacy",
            "/metadata/uid",
            json!("replacement"),
        ),
        (SOURCE, "/metadata/uid", json!("replacement")),
        (SOURCE, "/metadata/resourceVersion", json!("2")),
        (REG, "/status/phase", json!("Migrating")),
        (REG, "/status/privacyEpoch", json!("replacement")),
    ] {
        let rig = Rig::new(true).await;
        *rig.data
            .lock()
            .unwrap()
            .objects
            .get_mut(path)
            .unwrap()
            .pointer_mut(pointer)
            .unwrap() = value;
        assert_eq!(
            rig.call(&rig.request, TOKEN).await.0,
            reqwest::StatusCode::FORBIDDEN,
            "{path}{pointer}"
        );
    }
}

#[tokio::test]
async fn privacy_rpc_request_purpose_target_identity_recipient_nonce_and_version_are_bound() {
    let rig = Rig::new(true).await;
    for (pointer, value) in [
        ("/purpose", json!("admin")),
        ("/target/name", json!("another")),
        ("/target/name", json!("..")),
        ("/target/workspaceUid", json!("foreign")),
        ("/target/uid", json!("foreign")),
        ("/grantUid", json!("foreign")),
        ("/recipients/0/uid", json!("foreign")),
        ("/identity/namespace_uid", json!("foreign")),
        ("/credentialVersion", json!("observer-secret:2")),
        ("/nonce", json!("not-a-valid-nonce")),
        ("/epoch", Value::Null),
        ("/verifier/tlsUid", json!("foreign")),
    ] {
        let mut value_request = serde_json::to_value(&rig.request).unwrap();
        *value_request.pointer_mut(pointer).unwrap() = value;
        let request = serde_json::from_value(value_request).unwrap();
        assert_eq!(
            rig.call(&request, TOKEN).await.0,
            reqwest::StatusCode::FORBIDDEN,
            "{pointer}"
        );
    }
    assert_eq!(
        rig.call(&rig.request, &"x".repeat(64)).await.0,
        reqwest::StatusCode::FORBIDDEN
    );
    let (_, value) = rig.call(&rig.request, TOKEN).await;
    let proof: wire::Proof = serde_json::from_value(value).unwrap();
    let mut replay = rig.request.clone();
    replay.nonce = "b".repeat(64);
    assert!(!proof.matches(&replay));
    replay = rig.request.clone();
    replay.target.uid = "foreign".into();
    assert!(!proof.matches(&replay));
    replay = rig.request.clone();
    replay.scope_id = "reset-scope".into();
    assert!(!proof.matches(&replay));
    replay = rig.request.clone();
    replay.operation = wire::Operation::Scope;
    assert!(!proof.matches(&replay));
}

#[tokio::test]
async fn privacy_rpc_absent_and_retired_registration_require_real_denial_and_current_epoch() {
    let mut rig = Rig::new(false).await;
    assert_eq!(
        rig.call(&rig.request, TOKEN).await.0,
        reqwest::StatusCode::OK
    );
    rig.data.lock().unwrap().allowed = true;
    assert_eq!(
        rig.call(&rig.request, TOKEN).await.0,
        reqwest::StatusCode::FORBIDDEN
    );
    {
        let mut d = rig.data.lock().unwrap();
        d.allowed = false;
        rig.request.epoch = Some("fabricated".into());
        bind(&mut d, &rig.request);
    }
    assert_eq!(
        rig.call(&rig.request, TOKEN).await.0,
        reqwest::StatusCode::FORBIDDEN
    );
    {
        let mut d = rig.data.lock().unwrap();
        enroll(&mut d);
        d.objects.get_mut(REG).unwrap()["spec"]["enabled"] = false.into();
        d.objects.get_mut(REG).unwrap()["status"]["phase"] = "Retired".into();
        rig.request.epoch = None;
        bind(&mut d, &rig.request);
    }
    assert_eq!(
        rig.call(&rig.request, TOKEN).await.0,
        reqwest::StatusCode::OK
    );
}

#[tokio::test]
async fn privacy_rpc_expired_and_prepared_credentials_do_not_authorize_learned_data() {
    let mut rig = Rig::new(true).await;
    rig.data.lock().unwrap().objects.get_mut(SANDBOX).unwrap()["status"]["serviceObservation"]["phase"] =
        "Prepared".into();
    assert_eq!(
        rig.call(&rig.request, TOKEN).await.0,
        reqwest::StatusCode::FORBIDDEN
    );
    rig.request.operation = wire::Operation::Scope;
    assert_eq!(
        rig.call(&rig.request, TOKEN).await.0,
        reqwest::StatusCode::OK
    );
    {
        use base64::Engine;
        let mut d = rig.data.lock().unwrap();
        let secret = d.objects.get_mut(SOURCE).unwrap();
        let raw = base64::engine::general_purpose::STANDARD
            .decode(secret["data"]["config.json"].as_str().unwrap())
            .unwrap();
        let mut value: Value = serde_json::from_slice(&raw).unwrap();
        value["expiresAt"] = (chrono::Utc::now().timestamp() - 1).into();
        secret["data"]["config.json"] =
            json!(k8s_openapi::ByteString(serde_json::to_vec(&value).unwrap()));
    }
    assert_eq!(
        rig.call(&rig.request, TOKEN).await.0,
        reqwest::StatusCode::FORBIDDEN
    );
}
