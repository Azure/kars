// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use futures::FutureExt;
use kube::core::DynamicObject;
use serde_json::json;
use std::{io::Write, path::PathBuf, sync::Mutex, time::Duration};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path},
};

const TEST_DEADLINE: Duration = Duration::from_secs(15);

async fn cancel_after_receipt(
    future: impl std::future::Future<Output = Result<DynamicObject, kube::Error>>,
    received: tokio::sync::oneshot::Receiver<()>,
) {
    tokio::pin!(future);
    tokio::time::timeout(TEST_DEADLINE, async {
        tokio::select! {
            biased;
            _ = &mut future => panic!("Held request completed before fixture receipt"),
            receipt = received => receipt.expect("Fixture receipt sender closed"),
        }
        assert!(
            future.as_mut().now_or_never().is_none(),
            "Held request must remain pending after fixture receipt"
        );
    })
    .await
    .expect("Real request did not reach the fixture within the test deadline");
}

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

#[test]
fn endpoint_comparison_uses_effective_https_ports_and_canonical_ip_literals() {
    for (uri, host, port, expected) in [
        ("https://10.96.0.1/", Some("10.96.0.1"), Some("443"), true),
        (
            "https://10.96.0.1:443/",
            Some("10.96.0.1"),
            Some("443"),
            true,
        ),
        (
            "https://10.96.0.1:6443/",
            Some("10.96.0.1"),
            Some("6443"),
            true,
        ),
        (
            "https://[2001:db8::1]/",
            Some("2001:0db8:0:0:0:0:0:1"),
            Some("443"),
            true,
        ),
        (
            "https://[2001:db8::1]:6443/",
            Some("2001:db8::1"),
            Some("6443"),
            true,
        ),
        (
            "https://api.internal/",
            Some("API.INTERNAL"),
            Some("443"),
            true,
        ),
        ("https://10.96.0.1/", Some("10.96.0.2"), Some("443"), false),
        ("https://10.96.0.1/", Some("10.96.0.1"), Some("6443"), false),
        (
            "https://10.96.0.1:6443/",
            Some("10.96.0.1"),
            Some("443"),
            false,
        ),
        ("https://10.96.0.1/", None, Some("443"), false),
        ("https://10.96.0.1/", Some("10.96.0.1"), None, false),
        (
            "https://10.96.0.1/",
            Some("10.96.0.1"),
            Some("invalid"),
            false,
        ),
        (
            "https://10.96.0.1/",
            Some("10.96.0.1"),
            Some("65536"),
            false,
        ),
        ("https://10.96.0.1/", Some("10.96.0.1"), Some(""), false),
        (
            "https://[2001:db8::1]/",
            Some("2001:db8::2"),
            Some("443"),
            false,
        ),
        (
            "https://api.internal/",
            Some("10.96.0.1"),
            Some("443"),
            false,
        ),
        (
            "http://10.96.0.1:443/",
            Some("10.96.0.1"),
            Some("443"),
            false,
        ),
        ("/", None, None, false),
    ] {
        assert_eq!(
            endpoint_environment_matches(&uri.parse().unwrap(), host, port),
            expected,
        );
    }
}

#[test]
fn transport_observations_never_format_private_suffixes_or_unrelated_fields() {
    struct Private;
    impl fmt::Debug for Private {
        fn fmt(&self, _: &mut fmt::Formatter<'_>) -> fmt::Result {
            panic!("private fields must not be formatted");
        }
    }
    assert!(message_starts_with(
        &format_args!("connecting to {:?}", Private),
        "connecting to ",
    ));
    assert!(!message_starts_with(
        &format_args!("other {:?}", Private),
        "connecting to ",
    ));
    assert!(!message_starts_with(
        &format_args!("connect"),
        "connecting to "
    ));
    let progress = Progress::new("kars-runtime".into());
    tracing::dispatcher::with_default(
        &tracing::Dispatch::new(HttpBoundary(progress.clone())),
        || {
            tracing::debug!(target: "unrelated", "connected to {:?}", Private);
            tracing::debug!(target: "hyper_util::client::legacy::connect::http",
                unrelated = ?Private, "connecting to {:?}", Private);
            tracing::debug!(target: "hyper_util::client::legacy::connect::http",
                "connected to {:?}", Private);
            tracing::trace!(target: "hyper_util::client::legacy::client",
                "http2 handshake complete, spawning background dispatcher task");
        },
    );
    assert_eq!(
        progress.bits.load(Ordering::Relaxed),
        TCP_STARTED | TCP_CONNECTED | HTTP_HANDSHAKE,
    );
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
async fn pooled_http_success_does_not_inherit_previous_connection_progress() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSandbox",
            "metadata":{"name":"runtime"}
        })))
        .expect(2..=65)
        .mount(&server)
        .await;
    let client = client(configured(&server)).unwrap();
    let connection = TCP_STARTED | TCP_CONNECTED | HTTP_HANDSHAKE;
    tokio::time::timeout(TEST_DEADLINE, async {
        let first = Progress::new("kars-runtime".into());
        read_target(&client, request(&first, "runtime"), &first)
            .await
            .unwrap();
        assert_eq!(
            first.bits.load(Ordering::Relaxed) & (connection | HEADERS),
            connection | HEADERS
        );
        assert_eq!(first.status.load(Ordering::Relaxed), 200);
        // hyper-util may return an HTTP/1 connection to its idle pool in a
        // spawned future. Require the same strict observation without assuming
        // that future has run before the immediately following request.
        for _ in 0..64 {
            tokio::task::yield_now().await;
            let progress = Progress::new("kars-runtime".into());
            read_target(&client, request(&progress, "runtime"), &progress)
                .await
                .unwrap();
            assert_eq!(progress.status.load(Ordering::Relaxed), 200);
            assert_ne!(progress.bits.load(Ordering::Relaxed) & HEADERS, 0);
            if progress.bits.load(Ordering::Relaxed) & connection == 0 {
                return;
            }
        }
        panic!("No successful request without fresh connection observations");
    })
    .await
    .expect("Pooled request regression exceeded its test deadline");
}

#[tokio::test]
async fn cancelled_http_wait_is_distinct_from_predispatch_and_wrong_identity_is_not_hidden() {
    use axum::{Json, Router, http::StatusCode, routing::get};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (sent, received) = tokio::sync::oneshot::channel();
    let sent = Arc::new(Mutex::new(Some(sent)));
    let router = Router::new()
        .route(
            "/apis/kars.azure.com/v1alpha1/namespaces/work/karssandboxes/slow",
            get(move || {
                let sent = sent.clone();
                async move {
                    sent.lock().unwrap().take().unwrap().send(()).unwrap();
                    std::future::pending::<StatusCode>().await
                }
            }),
        )
        .route(
            "/apis/kars.azure.com/v1alpha1/namespaces/work/karssandboxes/denied",
            get(|| async {
                (
                    StatusCode::FORBIDDEN,
                    Json(json!({
                        "apiVersion":"v1","kind":"Status","status":"Failure","code":403,
                        "reason":"Forbidden","message":"fixture denial"
                    })),
                )
            }),
        );
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let mut config = Config::new(format!("http://{address}").parse().unwrap());
    config.default_namespace = "kars-runtime".into();
    let client = client(config).unwrap();
    let slow = Progress::new("kars-runtime".into());
    cancel_after_receipt(
        read_target(&client, request(&slow, "slow"), &slow),
        received,
    )
    .await;
    assert_eq!(
        slow.bits.load(Ordering::Relaxed)
            & (ENTERED | DISPATCH | TCP_STARTED | TCP_CONNECTED | HTTP_HANDSHAKE | HEADERS),
        ENTERED | DISPATCH | TCP_STARTED | TCP_CONNECTED | HTTP_HANDSHAKE
    );
    let denied = Progress::new("different-runtime".into());
    let error = tokio::time::timeout(
        TEST_DEADLINE,
        read_target(&client, request(&denied, "denied"), &denied),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(matches!(error, kube::Error::Api(status) if status.code == 403));
    assert_eq!(denied.status.load(Ordering::Relaxed), 403);
    assert_eq!(
        denied.bits.load(Ordering::Relaxed) & (HEADERS | NAMESPACE),
        HEADERS
    );
    assert_eq!(slow.status.load(Ordering::Relaxed), 0);
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn stalled_tls_is_distinct_from_completed_tcp_and_http_setup() {
    use tokio::{io::AsyncReadExt, net::TcpListener};

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (sent, received) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut hello = [0u8; 6];
        stream.read_exact(&mut hello).await.unwrap();
        // TLS handshake record header followed by ClientHello's message type.
        assert_eq!(hello[0], 22);
        assert_eq!(hello[5], 1);
        sent.send(()).unwrap();
        std::future::pending::<()>().await;
    });
    let client = client(Config::new(format!("https://{address}").parse().unwrap())).unwrap();
    let progress = Progress::new("default".into());
    cancel_after_receipt(
        read_target(&client, request(&progress, "runtime"), &progress),
        received,
    )
    .await;
    assert_eq!(
        progress.bits.load(Ordering::Relaxed)
            & (TCP_STARTED | TCP_CONNECTED | HTTP_HANDSHAKE | HEADERS),
        TCP_STARTED | TCP_CONNECTED
    );
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn actual_tls_progress_preserves_ca_and_server_identity_verification() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let key = rcgen::KeyPair::generate().unwrap();
    let certificate = rcgen::CertificateParams::new(vec!["127.0.0.1".into()])
        .unwrap()
        .self_signed(&key)
        .unwrap();
    let listener = crate::sre_proxy::Listener {
        tcp: tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap(),
        tls: crate::sre_proxy::tls_from_pem(
            certificate.pem().as_bytes(),
            key.serialize_pem().as_bytes(),
        )
        .unwrap(),
    };
    let address = listener.tcp.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let router = axum::Router::new().fallback(|| async {
            axum::Json(json!({
                "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSandbox",
                "metadata":{"name":"runtime","uid":"runtime-uid"}
            }))
        });
        axum::serve(listener, router).await.unwrap();
    });
    for mode in ["trusted", "untrusted", "wrong-name"] {
        let mut config = Config::new(format!("https://{address}").parse().unwrap());
        if mode != "untrusted" {
            config.root_cert = Some(vec![certificate.der().to_vec()]);
        }
        if mode == "wrong-name" {
            config.tls_server_name = Some("different.invalid".into());
        }
        let client = client(config).unwrap();
        let progress = Progress::new("default".into());
        let result = tokio::time::timeout(
            TEST_DEADLINE,
            read_target(&client, request(&progress, "runtime"), &progress),
        )
        .await
        .unwrap();
        assert_eq!(result.is_ok(), mode == "trusted");
        let bits = progress.bits.load(Ordering::Relaxed);
        assert_eq!(
            bits & (TCP_STARTED | TCP_CONNECTED),
            TCP_STARTED | TCP_CONNECTED
        );
        assert_eq!(
            bits & (HTTP_HANDSHAKE | HEADERS),
            if mode == "trusted" {
                HTTP_HANDSHAKE | HEADERS
            } else {
                0
            }
        );
        assert_eq!(
            progress.status.load(Ordering::Relaxed),
            if mode == "trusted" { 200 } else { 0 }
        );
    }
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
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
