// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use kube::core::DynamicObject;
use serde_json::json;
use std::{io::Write, path::PathBuf, sync::Mutex, time::Duration};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path},
};

struct TokenFile(PathBuf);

impl TokenFile {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            ".observer-client-token-{}.fixture",
            rand::random::<u64>()
        ));
        std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .unwrap()
            .write_all(b"observer-test-token")
            .unwrap();
        Self(path)
    }
}

impl Drop for TokenFile {
    fn drop(&mut self) {
        std::fs::remove_file(&self.0).unwrap();
    }
}

fn request(progress: &Progress, name: &str) -> Request<Vec<u8>> {
    let mut request =
        kube::core::Request::new("/apis/kars.azure.com/v1alpha1/namespaces/work/karssandboxes")
            .get(name, &Default::default())
            .unwrap();
    request.extensions_mut().insert("get");
    request.extensions_mut().insert(progress.clone());
    progress.built();
    request
}

fn configured(server: &MockServer) -> Config {
    let mut config = Config::new(server.uri().parse().unwrap());
    config.default_namespace = "kars-runtime".into();
    config
}

#[tokio::test]
async fn actual_http_token_file_dispatch_keeps_auth_and_per_request_progress() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(
            "/apis/kars.azure.com/v1alpha1/namespaces/work/karssandboxes/runtime",
        ))
        .and(header("authorization", "Bearer observer-test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSandbox",
            "metadata":{"name":"runtime","uid":"runtime-uid","resourceVersion":"1"}
        })))
        .expect(4)
        .mount(&server)
        .await;
    let token = TokenFile::new();
    let mut config = configured(&server);
    config.auth_info.token_file = Some(token.0.to_str().unwrap().into());
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = client(config).unwrap();
    let calls = (0..4).map(|_| {
        let client = client.clone();
        async move {
            let progress = Progress::new("kars-runtime".into());
            progress.initialized();
            let value = read_target(&client, request(&progress, "runtime"), &progress)
                .await
                .unwrap();
            assert_eq!(value.metadata.uid.as_deref(), Some("runtime-uid"));
            progress.decoded();
            assert_eq!(progress.status.load(Ordering::Relaxed), 200);
            let bits = progress.bits.load(Ordering::Relaxed);
            assert_eq!(
                bits & (INITIALIZED | BUILT | ENTERED | DISPATCH | HEADERS | DECODED | NAMESPACE),
                INITIALIZED | BUILT | ENTERED | DISPATCH | HEADERS | DECODED | NAMESPACE
            );
        }
    });
    futures::future::join_all(calls).await;
}

#[tokio::test]
async fn cancelled_http_wait_is_distinct_from_predispatch_and_wrong_identity_is_not_hidden() {
    let server = MockServer::start().await;
    Mock::given(path(
        "/apis/kars.azure.com/v1alpha1/namespaces/work/karssandboxes/slow",
    ))
    .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(1)))
    .mount(&server)
    .await;
    Mock::given(path("/apis/kars.azure.com/v1alpha1/namespaces/work/karssandboxes/denied"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "apiVersion":"v1","kind":"Status","status":"Failure","code":403,"reason":"Forbidden","message":"fixture denial"
        }))).mount(&server).await;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = client(configured(&server)).unwrap();
    let slow = Progress::new("kars-runtime".into());
    let result = tokio::time::timeout(
        Duration::from_millis(200),
        read_target(&client, request(&slow, "slow"), &slow),
    )
    .await;
    assert!(result.is_err());
    assert_eq!(
        slow.bits.load(Ordering::Relaxed) & (ENTERED | DISPATCH | HEADERS),
        ENTERED | DISPATCH
    );
    let denied = Progress::new("different-runtime".into());
    let error = read_target(&client, request(&denied, "denied"), &denied)
        .await
        .unwrap_err();
    assert!(matches!(error, kube::Error::Api(status) if status.code == 403));
    assert_eq!(denied.status.load(Ordering::Relaxed), 403);
    assert_eq!(
        denied.bits.load(Ordering::Relaxed) & (HEADERS | NAMESPACE),
        HEADERS
    );
    assert_eq!(slow.status.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn default_stack_still_rejects_missing_token_file_and_unsupported_proxy() {
    let server = MockServer::start().await;
    let mut config = configured(&server);
    config.auth_info.token_file = Some(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!(
                ".absent-observer-token-{}.fixture",
                rand::random::<u64>()
            ))
            .to_str()
            .unwrap()
            .into(),
    );
    assert!(client(config).is_err());
    let mut config = configured(&server);
    config.proxy_url = Some("unsupported://127.0.0.1:1".parse().unwrap());
    assert!(client(config).is_err());
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn response_headers_are_distinguished_from_response_decoding() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not-json"))
        .mount(&server)
        .await;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = client(configured(&server)).unwrap();
    let progress = Progress::new("kars-runtime".into());
    assert!(
        read_target(&client, request(&progress, "runtime"), &progress)
            .await
            .is_err()
    );
    assert_eq!(
        progress.bits.load(Ordering::Relaxed) & (DISPATCH | HEADERS | DECODED),
        DISPATCH | HEADERS
    );
    assert_eq!(progress.status.load(Ordering::Relaxed), 200);
}

#[derive(Clone, Default)]
struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

impl Write for CapturedLogs {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLogs {
    type Writer = Self;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

impl CapturedLogs {
    fn dispatch(&self) -> tracing::Dispatch {
        tracing::Dispatch::new(
            tracing_subscriber::fmt()
                .json()
                .with_ansi(false)
                .with_max_level(tracing::Level::TRACE)
                .with_writer(self.clone())
                .finish(),
        )
    }
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

fn error_kind(result: Result<DynamicObject, kube::Error>) -> (&'static str, u16) {
    match result {
        Err(kube::Error::SerdeError(_)) => ("json", 0),
        Err(kube::Error::Api(response)) => ("api", response.code),
        _ => panic!("unexpected controlled response class"),
    }
}

#[tokio::test]
async fn malformed_success_and_error_bodies_are_suppressed_through_complete_target_request() {
    let server = MockServer::start().await;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = client(configured(&server)).unwrap();
    for (name, status, canary) in [
        ("bad-success", 200, "MALFORMED_SUCCESS_PRIVATE_BODY_CANARY"),
        ("bad-error", 503, "MALFORMED_ERROR_PRIVATE_BODY_CANARY"),
    ] {
        Mock::given(path(format!(
            "/apis/kars.azure.com/v1alpha1/namespaces/work/karssandboxes/{name}"
        )))
        .respond_with(ResponseTemplate::new(status).set_body_string(canary))
        .expect(2)
        .mount(&server)
        .await;
        let baseline_logs = CapturedLogs::default();
        let baseline = Progress::new("kars-runtime".into());
        let original = client
            .request::<DynamicObject>(request(&baseline, name))
            .with_subscriber(baseline_logs.dispatch())
            .await;
        let original_kind = error_kind(original);
        // A positive control proves the upstream post-header warning is
        // observable: the service-only shield does not cover body decoding.
        assert!(baseline_logs.text().contains(canary));

        let safe_logs = CapturedLogs::default();
        let safe = Progress::new("kars-runtime".into());
        let result = async {
            safe.initialized();
            let pending = Pending(safe.clone());
            let result = read_target(&client, request(&safe, name), &safe).await;
            drop(pending);
            tracing::warn!("PUBLIC_AFTER_TARGET_REQUEST");
            result
        }
        .with_subscriber(safe_logs.dispatch())
        .await;
        assert_eq!(error_kind(result), original_kind);
        let logs = safe_logs.text();
        assert!(!logs.contains(canary));
        assert!(!logs.contains(&server.uri()));
        assert!(logs.contains("PUBLIC_AFTER_TARGET_REQUEST"));
        assert!(logs.contains("Private observation target client pending"));
        let record: serde_json::Value = logs
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .find(|value: &serde_json::Value| {
                value["fields"]["message"] == "Private observation target client pending"
            })
            .unwrap();
        assert_eq!(record["fields"]["http_status"], status);
        assert_eq!(record["fields"]["response_headers"], true);
        assert_eq!(record["fields"]["after_auth_dispatch"], true);
        assert_eq!(safe.bits.load(Ordering::Relaxed) & DECODED, 0);
    }
}

#[tokio::test]
async fn concurrent_target_body_shields_leave_sibling_logs_and_progress_request_local() {
    let server = MockServer::start().await;
    for (name, status, canary) in [
        ("one", 200, "CONCURRENT_ONE_PRIVATE_BODY_CANARY"),
        ("two", 502, "CONCURRENT_TWO_PRIVATE_BODY_CANARY"),
    ] {
        Mock::given(path(format!(
            "/apis/kars.azure.com/v1alpha1/namespaces/work/karssandboxes/{name}"
        )))
        .respond_with(
            ResponseTemplate::new(status)
                .set_body_string(canary)
                .set_delay(Duration::from_millis(30)),
        )
        .expect(1)
        .mount(&server)
        .await;
    }
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = client(configured(&server)).unwrap();
    let logs = CapturedLogs::default();
    let run = |name: &'static str| {
        let client = client.clone();
        async move {
            let progress = Progress::new("kars-runtime".into());
            progress.initialized();
            let pending = Pending(progress.clone());
            let result = read_target(&client, request(&progress, name), &progress).await;
            drop(pending);
            (error_kind(result), progress.status.load(Ordering::Relaxed))
        }
    };
    let (one, two, ()) = async {
        futures::join!(run("one"), run("two"), async {
            tokio::task::yield_now().await;
            tracing::warn!("PUBLIC_CONCURRENT_SIBLING");
        })
    }
    .with_subscriber(logs.dispatch())
    .await;
    assert_eq!(one, (("json", 0), 200));
    assert_eq!(two, (("api", 502), 502));
    let text = logs.text();
    assert!(!text.contains("PRIVATE_BODY_CANARY"));
    assert!(!text.contains(&server.uri()));
    assert!(text.contains("PUBLIC_CONCURRENT_SIBLING"));
    let diagnostics: Vec<serde_json::Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .filter(|value: &serde_json::Value| {
            value["fields"]["message"] == "Private observation target client pending"
        })
        .collect();
    assert_eq!(diagnostics.len(), 2);
}
