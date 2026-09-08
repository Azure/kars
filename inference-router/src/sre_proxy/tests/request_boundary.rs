// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::super::*;
use chrono::{Duration, Utc};
use serde_json::{Value, json};
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

const TOKEN: &str = "/api/v1/namespaces/kars-sre/serviceaccounts/sre-api-router/token";
const REGISTRATION: &str = "/apis/kars.azure.com/v1alpha1/karssreregistrations/canonical";
const NAMESPACE: &str = "/api/v1/namespaces/kars-sre";
const SOURCE: &str = "/apis/kars.azure.com/v1alpha1/namespaces/kars-system/karssandboxes/sre";
const ACCOUNT: &str = "/api/v1/namespaces/kars-sre/serviceaccounts/sre-api-router";
const REVIEWS: &str = "/apis/authorization.k8s.io/v1/subjectaccessreviews";
const INVENTORY: &str = "/api/v1/namespaces/kars-sre/secrets";
const PROJECTED_TOKEN: &str = "selected-projected-token";
const RENEWED_TOKEN: &str = "selected-renewed-token";

#[derive(Clone)]
struct RecordedRequest {
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Value,
}

type Requests = Arc<Mutex<Vec<RecordedRequest>>>;

async fn kubernetes(
    State(requests): State<Requests>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    requests.lock().unwrap().push(RecordedRequest {
        method: method.clone(),
        uri: uri.clone(),
        headers: headers.clone(),
        body: body.clone(),
    });
    let expected = if uri.path() == TOKEN {
        PROJECTED_TOKEN
    } else {
        RENEWED_TOKEN
    };
    if headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        != Some(format!("Bearer {expected}").as_str())
    {
        return error(StatusCode::UNAUTHORIZED, "Unexpected test credential");
    }
    let value = match (method, uri.path()) {
        (Method::POST, TOKEN) => json!({"status":{
            "token":RENEWED_TOKEN,
            "expirationTimestamp":(Utc::now()+Duration::hours(1)).to_rfc3339(),
        }}),
        (Method::GET, REGISTRATION) => json!({
            "metadata":{"uid":"registration","generation":1},
            "spec":{"enabled":true,"sandbox":{"namespace":"kars-system","uid":"source"},
                "runtimeNamespace":{"uid":"namespace"}},
            "status":{"phase":"Ready","observedGeneration":1,"privacyEpoch":"epoch",
                "legacySecretAccessDenied":true,"privacyRevision":crate::sre_privacy::REVISION},
        }),
        (Method::GET, NAMESPACE) => json!({"metadata":{"uid":"namespace","annotations":{
            "kars.azure.com/namespace-claim-version":"v1",
            "kars.azure.com/sandbox-namespace":"kars-system",
            "kars.azure.com/sandbox-name":"sre",
            "kars.azure.com/sandbox-uid":"source",
        }}}),
        (Method::GET, SOURCE) => json!({"metadata":{"uid":"source",
            "annotations":{"kars.azure.com/namespace-uid":"namespace"}}}),
        (Method::GET, ACCOUNT) => json!({"metadata":{"uid":"router-sa"}}),
        (Method::POST, REVIEWS) => json!({"status":{"allowed":false}}),
        (Method::GET, INVENTORY) => json!({
            "apiVersion":"meta.k8s.io/v1","kind":"PartialObjectMetadataList",
            "metadata":{},"items":[],
        }),
        _ => return error(StatusCode::NOT_FOUND, "Unexpected test Kubernetes path"),
    };
    axum::Json(value).into_response()
}

async fn attacker(State(hits): State<Arc<AtomicUsize>>) -> &'static str {
    hits.fetch_add(1, Ordering::SeqCst);
    "attacker server reached"
}

struct Server {
    origin: String,
    authority: String,
    task: tokio::task::JoinHandle<()>,
}

impl Server {
    async fn start(router: Router, identity: Option<&Path>) -> Self {
        let tcp = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = tcp.local_addr().unwrap();
        let task = if let Some(directory) = identity {
            let listener = Listener {
                tcp,
                tls: tls(directory).unwrap(),
            };
            tokio::spawn(async move { axum::serve(listener, router).await.unwrap() })
        } else {
            tokio::spawn(async move { axum::serve(tcp, router).await.unwrap() })
        };
        Self {
            origin: format!(
                "{}://{address}",
                if identity.is_some() { "https" } else { "http" }
            ),
            authority: address.to_string(),
            task,
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn write_identity(directory: &Path) -> String {
    let key = rcgen::KeyPair::generate().unwrap();
    let certificate = rcgen::CertificateParams::new(vec!["127.0.0.1".into(), "localhost".into()])
        .unwrap()
        .self_signed(&key)
        .unwrap();
    let pem = certificate.pem();
    std::fs::write(directory.join("server-cert.pem"), &pem).unwrap();
    std::fs::write(directory.join("server-key.pem"), key.serialize_pem()).unwrap();
    std::fs::write(directory.join("kube-ca.crt"), &pem).unwrap();
    pem
}

async fn exercise_readiness_inputs(channel: &str, use_https_attacker: bool) {
    let directory = tempfile::tempdir_in(".").unwrap();
    let selected = directory.path().join("selected");
    let alternate = directory.path().join("alternate");
    std::fs::create_dir(&selected).unwrap();
    std::fs::create_dir(&alternate).unwrap();
    let ca = write_identity(&selected);
    let client = reqwest::Client::builder()
        .no_proxy()
        .add_root_certificate(reqwest::Certificate::from_pem(ca.as_bytes()).unwrap())
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let attacker_app = Router::new().fallback(attacker).with_state(hits.clone());
    let http_attacker = Server::start(attacker_app.clone(), None).await;
    let https_attacker = Server::start(attacker_app, Some(&selected)).await;

    // Both attacker origins are reachable; the HTTPS attacker is trusted by
    // this test CA too, so a bad destination cannot be masked by TLS rejection.
    for origin in [&http_attacker.origin, &https_attacker.origin] {
        assert_eq!(
            client
                .get(origin)
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap(),
            "attacker server reached"
        );
    }
    assert_eq!(hits.swap(0, Ordering::SeqCst), 2);
    let foreign = if use_https_attacker {
        &https_attacker
    } else {
        &http_attacker
    };
    let requests: Requests = Arc::new(Mutex::new(Vec::new()));
    let kube = Server::start(
        Router::new()
            .fallback(kubernetes)
            .with_state(requests.clone()),
        Some(&selected),
    )
    .await;
    let configuration = json!({
        "schema":"kars.azure.com/sre-api/v1","kubeUrl":kube.origin,
        "registrationUid":"registration","privacyEpoch":"epoch",
        "source":{"namespace":"kars-system","name":"sre","uid":"source"},
        "runtimeNamespace":"kars-sre","namespaceUid":"namespace",
        "serviceAccountUid":"router-sa","secretUid":"selected-secret",
    });
    std::fs::write(
        selected.join("config.json"),
        serde_json::to_vec(&configuration).unwrap(),
    )
    .unwrap();
    std::fs::write(selected.join("kube-token"), "initial-selected-token").unwrap();
    std::fs::write(
        selected.join("kube-expires-at"),
        (Utc::now() + Duration::minutes(2)).to_rfc3339(),
    )
    .unwrap();

    // Use the production loader and its HTTPS/CA/redirect configuration, not
    // Backend::for_test. Only fixture setup selects this project-local volume.
    let backend = Backend::load(&selected).unwrap();
    assert!(requests.lock().unwrap().is_empty());
    std::fs::write(selected.join("kube-token"), PROJECTED_TOKEN).unwrap();
    std::fs::write(
        selected.join("kube-expires-at"),
        (Utc::now() + Duration::minutes(4)).to_rfc3339(),
    )
    .unwrap();
    std::fs::write(alternate.join("kube-token"), "attacker-alternate-token").unwrap();
    std::fs::write(alternate.join("kube-expires-at"), "invalid-attacker-expiry").unwrap();
    let mut hostile_configuration = configuration.clone();
    hostile_configuration["kubeUrl"] = foreign.origin.clone().into();
    hostile_configuration["runtimeNamespace"] = "kars-attacker".into();
    std::fs::write(
        alternate.join("config.json"),
        serde_json::to_vec(&hostile_configuration).unwrap(),
    )
    .unwrap();

    let proxy = Proxy {
        backend,
        token: Arc::from("a".repeat(64)),
        capacity: Arc::new(Semaphore::new(16)),
    };
    let server = Server::start(app(proxy), Some(&selected)).await;
    let mut request = client.get(format!("{}/readyz", server.origin));
    let alternate_name = alternate.canonicalize().unwrap().display().to_string();
    if matches!(channel, "query" | "all") {
        request = request.query(&[
            ("directory", alternate_name.as_str()),
            ("credentialDirectory", alternate_name.as_str()),
            ("kubeUrl", foreign.origin.as_str()),
            ("runtimeNamespace", "kars-attacker"),
        ]);
    }
    if matches!(channel, "headers" | "all") {
        request = request
            .header("host", &foreign.authority)
            .header("x-forwarded-host", &foreign.authority)
            .header(
                "forwarded",
                format!("host={};proto=https", foreign.authority),
            )
            .header("origin", &foreign.origin)
            .header("x-credential-directory", &alternate_name)
            .header("x-kube-url", &foreign.origin)
            .header("x-runtime-namespace", "kars-attacker");
    }
    if matches!(channel, "body" | "all") {
        request = request.json(&json!({
            "directory":alternate_name,"kubeUrl":foreign.origin,"runtimeNamespace":"kars-attacker",
            "proxy":{"backend":{"directory":alternate_name,"config":hostile_configuration}},
        }));
    }
    let response = request.send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{channel}");
    assert_eq!(
        response.json::<Value>().await.unwrap(),
        json!({"ready":true})
    );
    assert_eq!(hits.load(Ordering::SeqCst), 0, "{channel}");

    let calls = requests.lock().unwrap().clone();
    assert_eq!(
        calls.len(),
        24,
        "{channel}: renewal + four live identities + 18 reviews + inventory"
    );
    assert_eq!(calls[0].method, Method::POST);
    assert_eq!(calls[0].uri.path(), TOKEN);
    assert_eq!(
        calls[0].headers["authorization"],
        format!("Bearer {PROJECTED_TOKEN}")
    );
    assert_eq!(
        calls[0].body["spec"]["boundObjectRef"],
        json!({"apiVersion":"v1","kind":"Secret","name":"sre-api-router-identity","uid":"selected-secret"})
    );
    for call in &calls {
        assert_eq!(call.headers["host"], kube.authority);
        assert!(call.uri.query().is_none());
        assert!(
            [
                TOKEN,
                REGISTRATION,
                NAMESPACE,
                SOURCE,
                ACCOUNT,
                REVIEWS,
                INVENTORY
            ]
            .contains(&call.uri.path())
        );
        assert!(!call.headers.contains_key("x-credential-directory"));
        assert!(!call.headers.contains_key("x-kube-url"));
        assert!(!call.headers.contains_key("x-forwarded-host"));
        assert!(
            !serde_json::to_string(&call.body)
                .unwrap()
                .contains("kars-attacker")
        );
    }
    for call in &calls[1..] {
        assert_eq!(
            call.headers["authorization"],
            format!("Bearer {RENEWED_TOKEN}")
        );
    }
    assert_eq!(
        calls.iter().filter(|call| call.uri.path() == TOKEN).count(),
        1
    );
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.uri.path() == REVIEWS)
            .count(),
        18
    );
    let inventory = calls
        .iter()
        .find(|call| call.uri.path() == INVENTORY)
        .unwrap();
    assert_eq!(inventory.method, Method::GET);
    assert!(
        inventory.headers["accept"]
            .to_str()
            .unwrap()
            .contains("PartialObjectMetadataList")
    );
    assert_eq!(
        std::fs::read_to_string(alternate.join("kube-token")).unwrap(),
        "attacker-alternate-token"
    );
    assert_eq!(
        std::fs::read_to_string(alternate.join("kube-expires-at")).unwrap(),
        "invalid-attacker-expiry"
    );
}

#[tokio::test]
async fn ready_hostile_inputs_cannot_select_files_origin_or_namespace() {
    for channel in ["query", "headers", "body", "all"] {
        for https in [false, true] {
            exercise_readiness_inputs(channel, https).await;
        }
    }
}

#[test]
fn production_factory_remains_startup_only_with_a_fixed_credential_directory() {
    assert_eq!(DIRECTORY, "/etc/kars/sre-api");
    let production = include_str!("../mod.rs");
    assert_eq!(production.matches("Backend::load(").count(), 1);
    let start = production.find("pub async fn start()").unwrap();
    let probe = production.find("pub async fn readiness_probe()").unwrap();
    let startup = &production[start..probe];
    assert!(startup.contains("let directory = PathBuf::from(DIRECTORY);"));
    assert!(startup.contains("Backend::load(&directory)"));
    let ready_start = production.find("async fn ready(").unwrap();
    let forward_start = production.find("async fn forward(").unwrap();
    let ready = &production[ready_start..forward_start];
    assert!(ready.contains("async fn ready(State(proxy): State<Proxy>)"));
    assert!(!ready.contains("Backend::load("));
    assert!(include_str!("../../main.rs").contains("kars_inference_router::sre_proxy::start()"));
}
