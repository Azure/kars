// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use backend::{Config, Source};
use chrono::Utc;
use serde_json::json;
use std::sync::Mutex;
use wiremock::{Mock, MockServer, ResponseTemplate};

mod request_boundary;

const PRIVATE_VALUE: &str = "PRIVATE_OPERATOR_CONTROL_VALUE";

struct Fixture {
    _upstream: MockServer,
    directory: tempfile::TempDir,
    task: tokio::task::JoinHandle<()>,
    backend: Arc<Backend>,
    client: reqwest::Client,
    url: String,
    token: String,
    privacy: Arc<Mutex<PrivacyState>>,
}

#[derive(Default)]
struct PrivacyState {
    aliases: Vec<serde_json::Value>,
    watch_allowed: bool,
    prior_revision: bool,
    metadata_error: Option<u16>,
    calls: Vec<(String, String, serde_json::Value)>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn fixture() -> Fixture {
    let upstream = MockServer::start().await;
    let privacy = Arc::new(Mutex::new(PrivacyState::default()));
    let observed = privacy.clone();
    Mock::given(|_:&wiremock::Request|true).respond_with(move |request:&wiremock::Request| {
        if request.headers.get("authorization").and_then(|v|v.to_str().ok())!=Some("Bearer private-kubernetes-token") {
            return ResponseTemplate::new(401).set_body_json(json!({"kind":"Status","code":401}));
        }
        let path=request.url.path();
        let mut state=observed.lock().unwrap();
        let body:serde_json::Value=request.body_json().unwrap_or_default();
        state.calls.push((request.method.to_string(),path.into(),body.clone()));
        let value=match path {
            "/apis/kars.azure.com/v1alpha1/karssreregistrations/canonical" => json!({
                "metadata":{"uid":"registration","generation":1},
                "spec":{"enabled":true,"sandbox":{"namespace":"kars-system","uid":"source"},"runtimeNamespace":{"uid":"namespace"}},
                "status":{"phase":"Ready","observedGeneration":1,"privacyEpoch":"epoch","legacySecretAccessDenied":true,
                    "privacyRevision":if state.prior_revision {None} else {Some(crate::sre_privacy::REVISION)}},
            }),
            "/api/v1/namespaces/kars-sre" => json!({"metadata":{"uid":"namespace","annotations":{
                "kars.azure.com/namespace-claim-version":"v1","kars.azure.com/sandbox-namespace":"kars-system",
                "kars.azure.com/sandbox-name":"sre","kars.azure.com/sandbox-uid":"source"}}}),
            "/apis/kars.azure.com/v1alpha1/namespaces/kars-system/karssandboxes/sre" =>
                json!({"metadata":{"uid":"source","annotations":{"kars.azure.com/namespace-uid":"namespace"}}}),
            "/api/v1/namespaces/kars-sre/serviceaccounts/sre-api-router" => json!({"metadata":{"uid":"router-sa"}}),
            "/apis/authorization.k8s.io/v1/subjectaccessreviews" if request.method=="POST" =>
                json!({"status":{"allowed":state.watch_allowed && body["spec"]["resourceAttributes"]["verb"]=="watch"}}),
            "/api/v1/namespaces/kars-sre/secrets" => {
                assert!(request.headers["accept"].to_str().unwrap().contains("PartialObjectMetadataList"));
                if let Some(code)=state.metadata_error {
                    return ResponseTemplate::new(code).set_body_json(json!({"message":PRIVATE_VALUE}));
                }
                json!({"apiVersion":"meta.k8s.io/v1","kind":"PartialObjectMetadataList","metadata":{},"items":state.aliases})
            }
            "/api/v1/namespaces/kars-demo/secrets/router-services-admin" => secret(),
            "/api/v1/secrets" => json!({"apiVersion":"v1","kind":"SecretList","metadata":{},"items":[secret()]}),
            "/api/v1/namespaces/kars-demo/pods/app/log" => return ResponseTemplate::new(200).set_body_raw("legitimate pod log\n","text/plain"),
            "/apis/metrics.k8s.io/v1beta1/nodes" => json!({"kind":"NodeMetricsList","items":[]}),
            "/apis/kars.azure.com/v1alpha1/namespaces/kars-sre/karssreactions" if request.method=="POST" => {
                let body:serde_json::Value=request.body_json().unwrap();
                assert_eq!(body["spec"]["approval"],json!({"state":"Pending"}));
                return ResponseTemplate::new(201).set_body_json(body);
            }
            "/api/v1/namespaces/kars-sre/serviceaccounts/sre-api-router/token" if request.method=="POST" =>
                json!({"status":{"token":"private-kubernetes-token","expirationTimestamp":(Utc::now()+chrono::Duration::hours(1)).to_rfc3339()}}),
            _ => return ResponseTemplate::new(404).set_body_json(json!({"kind":"Status","code":404})),
        };
        ResponseTemplate::new(200).set_body_json(value)
    }).mount(&upstream).await;
    let directory = tempfile::tempdir_in(".").unwrap();
    let key = rcgen::KeyPair::generate().unwrap();
    let mut params =
        rcgen::CertificateParams::new(vec!["localhost".into(), "127.0.0.1".into()]).unwrap();
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "SRE compatibility test");
    let cert = params.self_signed(&key).unwrap();
    std::fs::write(directory.path().join("server-cert.pem"), cert.pem()).unwrap();
    std::fs::write(directory.path().join("server-key.pem"), key.serialize_pem()).unwrap();
    std::fs::write(directory.path().join("ca.crt"), cert.pem()).unwrap();
    let token = "a".repeat(64);
    std::fs::write(directory.path().join("token"), &token).unwrap();
    std::fs::write(directory.path().join("namespace"), "kars-sre").unwrap();
    let backend = Backend::for_test(
        Config {
            schema: "kars.azure.com/sre-api/v1".into(),
            kube_url: upstream.uri(),
            registration_uid: "registration".into(),
            privacy_epoch: "epoch".into(),
            source: Source {
                namespace: "kars-system".into(),
                name: "sre".into(),
                uid: "source".into(),
            },
            runtime_namespace: "kars-sre".into(),
            namespace_uid: "namespace".into(),
            service_account_uid: "router-sa".into(),
            secret_uid: "secret".into(),
        },
        directory.path().into(),
        Utc::now() + chrono::Duration::hours(1),
    );
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let tcp = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let url = format!("https://{}", tcp.local_addr().unwrap());
    let listener = Listener {
        tcp,
        tls: tls(directory.path()).unwrap(),
    };
    let proxy = Proxy {
        backend: backend.clone(),
        token: Arc::from(token.as_str()),
        capacity: Arc::new(Semaphore::new(16)),
    };
    let task = tokio::spawn(async move { axum::serve(listener, app(proxy)).await.unwrap() });
    let client = reqwest::Client::builder()
        .no_proxy()
        .add_root_certificate(reqwest::Certificate::from_pem(cert.pem().as_bytes()).unwrap())
        .build()
        .unwrap();
    Fixture {
        _upstream: upstream,
        directory,
        task,
        backend,
        client,
        url,
        token,
        privacy,
    }
}

#[tokio::test]
async fn prior_ready_watch_only_access_aliases_and_inventory_errors_fail_closed() {
    for case in [
        "prior-ready",
        "watch-only",
        "alias-name",
        "alias-uid",
        "inventory-error",
    ] {
        let f = fixture().await;
        {
            let mut state = f.privacy.lock().unwrap();
            match case {
                    "prior-ready"=>state.prior_revision=true,
                    "watch-only"=>state.watch_allowed=true,
                    "inventory-error"=>state.metadata_error=Some(403),
                    _=>state.aliases.push(json!({"metadata":{"name":"arbitrary-token-alias","uid":"alias","resourceVersion":"1",
                        "annotations":if case=="alias-name" {
                            json!({"kubernetes.io/service-account.name":"sre-api-router"})
                        } else {json!({"kubernetes.io/service-account.name":"renamed","kubernetes.io/service-account.uid":"router-sa"})}}})),
                }
        }
        let response = f
            .client
            .get(format!(
                "{}/api/v1/namespaces/kars-demo/secrets/router-services-admin",
                f.url
            ))
            .bearer_auth(&f.token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE, "{case}");
        assert!(!response.text().await.unwrap().contains(PRIVATE_VALUE));
        assert!(
            f.privacy
                .lock()
                .unwrap()
                .calls
                .iter()
                .all(|(_, path, _)| path
                    != "/api/v1/namespaces/kars-demo/secrets/router-services-admin"),
            "{case}"
        );
    }
}

fn secret() -> serde_json::Value {
    json!({"apiVersion":"v1","kind":"Secret","type":"Opaque",
        "metadata":{"name":"router-services-admin","namespace":"kars-demo",
            "annotations":{"kubectl.kubernetes.io/last-applied-configuration":PRIVATE_VALUE},
            "labels":{"copy":PRIVATE_VALUE},"managedFields":[{"copy":PRIVATE_VALUE}]},
        "data":{"control-token":PRIVATE_VALUE},"stringData":{"copy":PRIVATE_VALUE}})
}

#[tokio::test]
async fn agent_credential_cannot_read_control_material_directly_or_through_tls_proxy() {
    let f = fixture().await;
    let direct = reqwest::Client::new()
        .get(format!(
            "{}/api/v1/namespaces/kars-demo/secrets/router-services-admin",
            f.backend.config.kube_url
        ))
        .bearer_auth(&f.token)
        .send()
        .await
        .unwrap();
    assert_eq!(direct.status(), StatusCode::UNAUTHORIZED);
    for path in [
        "/api/v1/namespaces/kars-demo/secrets/router-services-admin",
        "/api/v1/secrets",
    ] {
        let response = f
            .client
            .get(format!("{}{path}", f.url))
            .bearer_auth(&f.token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.text().await.unwrap();
        assert!(body.contains("control-token"));
        assert!(!body.contains(PRIVATE_VALUE));
        assert!(!body.contains("annotations"));
        assert!(!body.contains("stringData"));
    }
}

#[tokio::test]
async fn tls_proxy_preserves_logs_metrics_and_pending_proposals_but_rejects_escapes() {
    let f = fixture().await;
    let log = f
        .client
        .get(format!(
            "{}/api/v1/namespaces/kars-demo/pods/app/log?tailLines=100",
            f.url
        ))
        .bearer_auth(&f.token)
        .send()
        .await
        .unwrap();
    assert_eq!(log.text().await.unwrap(), "legitimate pod log\n");
    assert!(
        f.client
            .get(format!("{}/apis/metrics.k8s.io/v1beta1/nodes", f.url))
            .bearer_auth(&f.token)
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    let path = "/apis/kars.azure.com/v1alpha1/namespaces/kars-sre/karssreactions";
    let mut proposal = json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSREAction",
        "metadata":{"name":"proposal"},"spec":{"action":{"type":"RolloutRestart","params":{}}}});
    assert_eq!(
        f.client
            .post(format!("{}{path}", f.url))
            .bearer_auth(&f.token)
            .json(&proposal)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CREATED
    );
    proposal["spec"]["approval"] = json!({"state":"Approved"});
    assert_eq!(
        f.client
            .post(format!("{}{path}", f.url))
            .bearer_auth(&f.token)
            .json(&proposal)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    for path in [
        "/api/v1/%73ecrets",
        "/api/v1/secrets?watch=true",
        "/api/v1/namespaces/kars-sre/pods/sre/proxy",
        "/api/v1/namespaces/kars-sre/serviceaccounts/sre-api-router/token",
    ] {
        assert_eq!(
            f.client
                .get(format!("{}{path}", f.url))
                .bearer_auth(&f.token)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        f.client
            .get(format!("{}/api/v1/secrets", f.url))
            .bearer_auth(&f.token)
            .header("accept", "application/vnd.kubernetes.protobuf")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_ACCEPTABLE
    );
}

#[tokio::test]
async fn private_identity_renews_without_an_ambient_credential_fallback() {
    let f = fixture().await;
    let backend = Backend::for_test(
        f.backend.config.clone(),
        f.directory.path().into(),
        Utc::now() + chrono::Duration::minutes(2),
    );
    assert_eq!(backend.bearer().await.unwrap(), "private-kubernetes-token");
    assert!(backend.authorize().await.is_ok());
    let expired = Backend::for_test(
        f.backend.config.clone(),
        f.directory.path().into(),
        Utc::now() - chrono::Duration::seconds(1),
    );
    assert!(expired.bearer().await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn unchanged_legacy_hermes_client_uses_https_standard_files_logs_and_proposals() {
    let f = fixture().await;
    let python = std::env::var("SRE_TEST_PYTHON").unwrap_or_else(|_| "python3".into());
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../runtimes/hermes/src/kars_runtime_hermes/plugin/sre_kube.py");
    let port = reqwest::Url::parse(&f.url).unwrap().port().unwrap();
    let result=tokio::process::Command::new(python)
        .env("KUBERNETES_SERVICE_HOST","127.0.0.1").env("KUBERNETES_SERVICE_PORT",port.to_string())
        .env("NO_PROXY","127.0.0.1,localhost").env("no_proxy","127.0.0.1,localhost")
        .env("AGENT_FILES",f.directory.path()).env("LEGACY_SOURCE",source)
        .arg("-c").arg(r#"
import importlib.util, os, pathlib, sys, types
package = types.ModuleType("legacy"); package.__path__ = []
sys.modules["legacy"] = package
spec = importlib.util.spec_from_file_location("legacy.sre_kube", os.environ["LEGACY_SOURCE"])
module = importlib.util.module_from_spec(spec); spec.loader.exec_module(module)
sys.modules["legacy.sre_kube"] = module
module._SA_DIR = pathlib.Path(os.environ["AGENT_FILES"])
client = module.KubeClient()
secret = client.get("/api/v1/namespaces/kars-demo/secrets/router-services-admin")
assert secret["data"] == {"control-token": ""}
assert "annotations" not in secret["metadata"]
assert client._ensure_client().get("/api/v1/namespaces/kars-demo/pods/app/log").text == "legitimate pod log\n"
proposal = client.post("/apis/kars.azure.com/v1alpha1/namespaces/kars-sre/karssreactions", json={
 "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSREAction","metadata":{"name":"legacy"},
 "spec":{"action":{"type":"RolloutRestart","params":{}}}})
assert proposal["spec"]["approval"] == {"state":"Pending"}
source = pathlib.Path(os.environ["LEGACY_SOURCE"]).with_name("sre.py")
spec = importlib.util.spec_from_file_location("legacy.sre", source)
sre = importlib.util.module_from_spec(spec); spec.loader.exec_module(sre)
# Execute the unchanged plugin's real builder, including generateName and its
# diagnostic labels, rather than merely posting a hand-written minimal object.
sre._create_karssreaction_cr(action={"type":"RolloutRestart","namespace":"kars-demo","name":"app"},
 diagnosis="legitimate diagnosis", rationale="restart", ttl_minutes=5)
module.client().close()
client.close()
"#).output().await.unwrap();
    assert!(
        result.status.success(),
        "legacy HTTPS client failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}
