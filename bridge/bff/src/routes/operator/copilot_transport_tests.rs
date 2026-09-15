// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::routes::operator::providers::{
    copilot_jwt_with_client, fetch_copilot_models_with_client,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use serde_json::json;
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Write},
    net::SocketAddr,
    process::Command,
    sync::{Arc, Mutex},
    time::Duration,
};

const DEVICE: &str = "synthetic-device-code";
const TOKEN: &str = "synthetic-github-token";
const JWT: &str = "synthetic-copilot-jwt";
const ENDPOINTS: [CopilotEndpoint; 4] = [
    CopilotEndpoint::DeviceCode,
    CopilotEndpoint::AccessToken,
    CopilotEndpoint::Seat,
    CopilotEndpoint::Models,
];

fn test_builder() -> ClientBuilder {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    CopilotClient::builder()
        .no_proxy()
        .timeout(Duration::from_secs(3))
}

struct WireRequest {
    method: String,
    target: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

fn read_request(stream: &mut impl Read) -> std::io::Result<WireRequest> {
    let mut reader = BufReader::new(stream);
    let mut first = String::new();
    reader.read_line(&mut first)?;
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let target = parts.next().unwrap_or_default().to_owned();
    let mut headers = BTreeMap::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line == "\r\n" {
            break;
        }
        let (key, value) = line.trim_end().split_once(':').unwrap();
        headers.insert(key.to_ascii_lowercase(), value.trim().to_owned());
    }
    let length: usize = headers
        .get("content-length")
        .map(|value| value.parse().unwrap())
        .unwrap_or(0);
    assert!(length <= 16384);
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    Ok(WireRequest {
        method,
        target,
        headers,
        body,
    })
}

struct TlsFixture {
    address: SocketAddr,
    certificate: Vec<u8>,
    requests: Arc<Mutex<Vec<WireRequest>>>,
    reply: Arc<Mutex<(u16, Option<String>)>>,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for TlsFixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl TlsFixture {
    async fn new() -> Self {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        // Generate a fresh, memory-only test identity. No checked-in private
        // key, expiring certificate fixture, extra crate or real GitHub call.
        let identity = Command::new("openssl")
            .args([
                "req",
                "-x509",
                "-newkey",
                "ed25519",
                "-nodes",
                "-keyout",
                "/dev/stdout",
                "-days",
                "1",
                "-subj",
                "/CN=github.com",
                "-addext",
                "subjectAltName=DNS:github.com,DNS:api.github.com,DNS:api.githubcopilot.com",
                "-addext",
                "basicConstraints=critical,CA:FALSE",
                "-addext",
                "keyUsage=critical,digitalSignature",
                "-addext",
                "extendedKeyUsage=serverAuth",
            ])
            .output()
            .expect("OpenSSL is required for the loopback TLS regression fixture");
        assert!(identity.status.success(), "test identity generation failed");
        let certificate = CertificateDer::from_pem_slice(&identity.stdout).unwrap();
        let key = PrivateKeyDer::from_pem_slice(&identity.stdout).unwrap();
        let config = Arc::new(
            rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(vec![certificate.clone()], key)
                .unwrap(),
        );
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let reply = Arc::new(Mutex::new((200, None::<String>)));
        let server_requests = requests.clone();
        let server_reply = reply.clone();
        let server = tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let config = config.clone();
                let requests = server_requests.clone();
                let reply = server_reply.clone();
                tokio::task::spawn_blocking(move || {
                    let socket = socket.into_std().unwrap();
                    socket.set_nonblocking(false).unwrap();
                    socket
                        .set_read_timeout(Some(Duration::from_secs(3)))
                        .unwrap();
                    socket
                        .set_write_timeout(Some(Duration::from_secs(3)))
                        .unwrap();
                    let connection = rustls::ServerConnection::new(config).unwrap();
                    let mut stream = rustls::StreamOwned::new(connection, socket);
                    // Untrusted certificates/hostnames intentionally fail the handshake.
                    let Ok(request) = read_request(&mut stream) else {
                        return;
                    };
                    let body = match request.target.as_str() {
                        "/login/device/code" => json!({
                            "device_code":DEVICE, "user_code":"ABCD-EFGH",
                            "verification_uri":"https://github.com/login/device",
                            "interval":5, "expires_in":900,
                        }),
                        "/login/oauth/access_token" => {
                            json!({"access_token":TOKEN, "token_type":"bearer"})
                        }
                        "/copilot_internal/v2/token" => {
                            json!({"token":JWT, "chat_enabled":true})
                        }
                        "/models" => json!({"data":[{
                            "id":"test-chat", "model_picker_enabled":true,
                            "capabilities":{"type":"chat"}
                        }]}),
                        _ => json!({}),
                    }
                    .to_string();
                    requests.lock().unwrap().push(request);
                    let (status, location) = reply.lock().unwrap().clone();
                    let location = location
                        .map(|url| format!("Location: {url}\r\n"))
                        .unwrap_or_default();
                    let response = format!(
                        "HTTP/1.1 {status} Test\r\n{location}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len(),
                    );
                    let _ = stream.write_all(response.as_bytes());
                    let _ = stream.flush();
                });
            }
        });
        Self {
            address,
            certificate: certificate.to_vec(),
            requests,
            reply,
            server,
        }
    }

    fn client(&self, trust_certificate: bool) -> CopilotClient {
        let mut builder = test_builder();
        for host in [
            "github.com",
            "api.github.com",
            "api.githubcopilot.com",
            "untrusted.invalid",
        ] {
            builder = builder.resolve(host, self.address);
        }
        if trust_certificate {
            builder = builder
                .add_root_certificate(reqwest::Certificate::from_der(&self.certificate).unwrap());
        }
        CopilotClient {
            client: builder.build().unwrap(),
            loopback: false,
        }
    }
}

#[test]
fn github_endpoints_use_fixed_https_urls_and_methods() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = CopilotClient::github().unwrap();
    for (endpoint, method, url) in [
        (
            CopilotEndpoint::DeviceCode,
            Method::POST,
            "https://github.com/login/device/code",
        ),
        (
            CopilotEndpoint::AccessToken,
            Method::POST,
            "https://github.com/login/oauth/access_token",
        ),
        (
            CopilotEndpoint::Seat,
            Method::GET,
            "https://api.github.com/copilot_internal/v2/token",
        ),
        (
            CopilotEndpoint::Models,
            Method::GET,
            "https://api.githubcopilot.com/models",
        ),
    ] {
        let request = client.request(endpoint).build().unwrap();
        assert_eq!(request.method(), method);
        assert_eq!(request.url().as_str(), url);
        assert!(request.url().query().is_none());
        assert!(request.url().username().is_empty());
        assert!(request.url().password().is_none());
    }
}

#[tokio::test]
async fn github_transport_rejects_http_before_connecting() {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let client = test_builder().build().unwrap();
    for method in [Method::POST, Method::GET] {
        let result = client
            .request(method, format!("http://{address}/must-not-receive"))
            .bearer_auth(TOKEN)
            .json(&json!({"device_code":DEVICE}))
            .send()
            .await;
        assert!(result.unwrap_err().is_builder());
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err()
    );
}

#[test]
fn loopback_fixture_rejects_non_loopback_addresses() {
    for address in [
        "192.0.2.1:443",
        "[2001:db8::1]:443",
        "0.0.0.0:80",
        "127.0.0.1:0",
    ] {
        assert!(
            std::panic::catch_unwind(|| CopilotClient::loopback(address.parse().unwrap())).is_err()
        );
    }
}

#[tokio::test]
async fn loopback_fixture_uses_fixed_urls_and_pinned_socket_routing() {
    async fn handle(
        method: Method,
        uri: axum::http::Uri,
        headers: axum::http::HeaderMap,
    ) -> axum::Json<serde_json::Value> {
        assert!(headers.contains_key("authorization"));
        axum::Json(json!({
            "method":method.as_str(),
            "path":uri.path(),
            "host":headers["host"].to_str().unwrap(),
        }))
    }

    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let client = CopilotClient::loopback(address);
    let mut servers = tokio::task::JoinSet::new();
    servers.spawn(async move {
        axum::serve(listener, axum::Router::new().fallback(handle))
            .await
            .unwrap();
    });
    for endpoint in ENDPOINTS {
        let request = client.request(endpoint).bearer_auth(TOKEN).build().unwrap();
        let expected_path = reqwest::Url::parse(endpoint.url())
            .unwrap()
            .path()
            .to_owned();
        let url = request.url();
        assert_eq!(url.scheme(), "http");
        assert_eq!(url.host_str(), Some("localhost"));
        assert_eq!(url.path(), expected_path);
        assert!(url.port().is_none());
        assert!(url.query().is_none() && url.fragment().is_none());
        assert!(url.username().is_empty() && url.password().is_none());
        let response = client.client.execute(request).await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = response.json().await.unwrap();
        assert_eq!(body["method"], endpoint.method().as_str());
        assert_eq!(body["path"], expected_path);
        assert_eq!(body["host"], "localhost");
    }
}

#[tokio::test]
async fn github_tls_transport_preserves_oauth_and_copilot_requests() {
    let fixture = TlsFixture::new().await;
    let client = fixture.client(true);
    let start: serde_json::Value = client
        .request(CopilotEndpoint::DeviceCode)
        .json(&json!({"client_id":"Iv1.b507a08c87ecfe98","scope":"read:user"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(start["device_code"], DEVICE);
    let poll: serde_json::Value = client
        .request(CopilotEndpoint::AccessToken)
        .json(&json!({
            "client_id":"Iv1.b507a08c87ecfe98", "device_code":DEVICE,
            "grant_type":"urn:ietf:params:oauth:grant-type:device_code",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(poll["access_token"], TOKEN);
    let jwt = copilot_jwt_with_client(TOKEN, &client).await.unwrap();
    assert_eq!(jwt, JWT);
    let models = fetch_copilot_models_with_client(&jwt, &client)
        .await
        .unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "test-chat");
    let requests = fixture.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    for (request, endpoint) in requests.iter().zip(ENDPOINTS) {
        let url = reqwest::Url::parse(endpoint.url()).unwrap();
        assert_eq!(request.method, endpoint.method().as_str());
        assert_eq!(request.target, url.path());
        assert_eq!(request.headers["host"], url.host_str().unwrap());
    }
    let body: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
    assert_eq!(body["device_code"], DEVICE);
    assert_eq!(
        requests[2].headers["authorization"],
        format!("token {TOKEN}")
    );
    assert_eq!(
        requests[3].headers["authorization"],
        format!("Bearer {JWT}")
    );
    assert_eq!(requests[3].headers["editor-version"], "vscode/1.107.0");
    assert_eq!(requests[3].headers["copilot-integration-id"], "vscode-chat");
}

#[tokio::test]
async fn github_tls_transport_does_not_follow_redirects() {
    let fixture = TlsFixture::new().await;
    let sink = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let client = fixture.client(true);
    let mut expected_requests = 0;
    for status in [301, 302, 303, 307, 308] {
        for location in [
            format!("http://{}/capture", sink.local_addr().unwrap()),
            format!(
                "https://untrusted.invalid:{}/capture",
                fixture.address.port()
            ),
            "/same-origin-capture".into(),
        ] {
            *fixture.reply.lock().unwrap() = (status, Some(location));
            for endpoint in ENDPOINTS {
                match endpoint {
                    CopilotEndpoint::DeviceCode | CopilotEndpoint::AccessToken => {
                        let response = client
                            .request(endpoint)
                            .json(&json!({"device_code":DEVICE}))
                            .send()
                            .await
                            .unwrap();
                        assert_eq!(response.status().as_u16(), status);
                        assert!(!response.status().is_success());
                    }
                    CopilotEndpoint::Seat => {
                        assert!(copilot_jwt_with_client(TOKEN, &client).await.is_err());
                    }
                    CopilotEndpoint::Models => {
                        assert!(
                            fetch_copilot_models_with_client(JWT, &client)
                                .await
                                .is_err()
                        );
                    }
                }
                expected_requests += 1;
                let requests = fixture.requests.lock().unwrap();
                assert_eq!(requests.len(), expected_requests);
                assert_eq!(
                    requests.last().unwrap().target,
                    reqwest::Url::parse(endpoint.url()).unwrap().path()
                );
            }
        }
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(100), sink.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn github_transport_rejects_untrusted_certificates_and_hostnames() {
    let fixture = TlsFixture::new().await;
    assert!(
        fixture
            .client(false)
            .request(CopilotEndpoint::Seat)
            .send()
            .await
            .unwrap_err()
            .is_connect()
    );
    assert!(
        fixture
            .client(true)
            .client
            .get("https://untrusted.invalid/models")
            .send()
            .await
            .unwrap_err()
            .is_connect()
    );
    assert!(fixture.requests.lock().unwrap().is_empty());
}
