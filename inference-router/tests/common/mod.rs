//! Shared integration test infrastructure.
//!
//! Three tiny axum servers simulate the Azure surface so the router can be
//! exercised end-to-end without touching live infrastructure:
//!   * `fake_imds`        — Azure IMDS token endpoint.
//!   * `fake_ad`          — Azure AD v2.0 `/oauth2/v2.0/token` endpoint.
//!   * `fake_azure`       — Azure OpenAI / Foundry upstream, serves JSON fixtures.
//!
//! Each helper binds `127.0.0.1:0`, returns its `SocketAddr`, and runs the
//! server on a detached tokio task that exits when the parent test drops.

#![allow(dead_code)]

use axum::{
    Json, Router,
    extract::{Path, Request, State},
    http::{HeaderMap, StatusCode, Uri},
    response::Response,
    routing::{any, post},
};
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::path::{Path as StdPath, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;

/// Records every request the fake servers received. Assertions in tests read
/// this to verify headers, bodies, and call counts.
#[derive(Clone, Default)]
pub struct RequestLog {
    inner: Arc<Mutex<Vec<RecordedRequest>>>,
}

#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub method: String,
    pub path: String,
    pub query: Option<String>,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl RequestLog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&self, rec: RecordedRequest) {
        self.inner.lock().unwrap().push(rec);
    }

    pub fn entries(&self) -> Vec<RecordedRequest> {
        self.inner.lock().unwrap().clone()
    }

    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

async fn record_and_extract(mut req: Request, log: &RequestLog) -> (Request, Vec<u8>) {
    let method = req.method().to_string();
    let uri: &Uri = req.uri();
    let path = uri.path().to_string();
    let query = uri.query().map(|s| s.to_string());
    let headers: Vec<(String, String)> = req
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("<binary>").to_string()))
        .collect();
    let body_bytes = axum::body::to_bytes(
        std::mem::replace(req.body_mut(), axum::body::Body::empty()),
        usize::MAX,
    )
    .await
    .unwrap_or_default()
    .to_vec();
    log.push(RecordedRequest {
        method,
        path,
        query,
        headers,
        body: body_bytes.clone(),
    });
    *req.body_mut() = axum::body::Body::from(body_bytes.clone());
    (req, body_bytes)
}

// ---------------------------------------------------------------------------
// Fake IMDS
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct FakeImds {
    pub addr: SocketAddr,
    pub log: RequestLog,
}

impl FakeImds {
    /// Start a fake IMDS endpoint that returns a canned JWT-shaped token.
    /// Use `base_url()` as `AZURE_IMDS_ENDPOINT`.
    pub async fn start() -> Self {
        Self::start_with_token("fake-imds-token-abc123", 3599).await
    }

    pub async fn start_with_token(token: &'static str, expires_in: u64) -> Self {
        let log = RequestLog::new();
        let state = (log.clone(), token.to_string(), expires_in);
        let app = Router::new()
            .route(
                "/metadata/identity/oauth2/token",
                any(
                    |State((log, token, expires)): State<(RequestLog, String, u64)>,
                     req: Request| async move {
                        let (_req, _body) = record_and_extract(req, &log).await;
                        Json(json!({
                            "access_token": token,
                            "expires_in": expires.to_string(),
                            "expires_on": "0",
                            "resource": "https://cognitiveservices.azure.com",
                            "token_type": "Bearer",
                        }))
                    },
                ),
            )
            .with_state(state);
        let (addr, handle) = spawn_server(app).await;
        // Keep the task alive for the lifetime of this struct via a detached handle.
        tokio::spawn(async move {
            let _ = handle.await;
        });
        Self { addr, log }
    }

    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

// ---------------------------------------------------------------------------
// Fake Azure AD
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct FakeAd {
    pub addr: SocketAddr,
    pub log: RequestLog,
}

impl FakeAd {
    pub async fn start() -> Self {
        let log = RequestLog::new();
        let state = log.clone();
        let app =
            Router::new()
                .route(
                    "/{tenant}/oauth2/v2.0/token",
                    post(
                        |Path(_tenant): Path<String>,
                         State(log): State<RequestLog>,
                         req: Request| async move {
                            let (_req, _body) = record_and_extract(req, &log).await;
                            Json(json!({
                                "access_token": "fake-ad-bearer-token",
                                "expires_in": 3599,
                                "token_type": "Bearer",
                            }))
                        },
                    ),
                )
                .with_state(state);
        let (addr, handle) = spawn_server(app).await;
        tokio::spawn(async move {
            let _ = handle.await;
        });
        Self { addr, log }
    }

    /// Use this as `AZURE_AD_ENDPOINT`.
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

// ---------------------------------------------------------------------------
// Fake Azure upstream (AOAI + Foundry)
// ---------------------------------------------------------------------------

/// Canned response for a (method, path-prefix) pair.
#[derive(Clone)]
pub struct FixtureRoute {
    pub method: String,
    pub path_prefix: String,
    pub status: u16,
    pub body: Value,
}

impl FixtureRoute {
    pub fn from_file(method: &str, path_prefix: &str, fixture_rel: &str) -> Self {
        let fixtures_root = fixtures_dir();
        let full = fixtures_root.join(fixture_rel);
        let raw =
            std::fs::read(&full).unwrap_or_else(|e| panic!("fixture {}: {}", full.display(), e));
        let body: Value = serde_json::from_slice(&raw)
            .unwrap_or_else(|e| panic!("fixture {} parse: {}", full.display(), e));
        Self {
            method: method.to_string(),
            path_prefix: path_prefix.to_string(),
            status: 200,
            body,
        }
    }

    pub fn with_status(mut self, status: u16) -> Self {
        self.status = status;
        self
    }
}

pub fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/foundry")
}

#[derive(Clone)]
pub struct FakeAzure {
    pub addr: SocketAddr,
    pub log: RequestLog,
}

impl FakeAzure {
    pub async fn start(routes: Vec<FixtureRoute>) -> Self {
        let log = RequestLog::new();
        let state = (log.clone(), Arc::new(routes));
        let app: Router = Router::new().fallback(any(dispatch)).with_state(state);
        let (addr, handle) = spawn_server(app).await;
        tokio::spawn(async move {
            let _ = handle.await;
        });
        Self { addr, log }
    }

    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

async fn dispatch(
    State((log, routes)): State<(RequestLog, Arc<Vec<FixtureRoute>>)>,
    req: Request,
) -> Response {
    let method = req.method().to_string();
    let path = req.uri().path().to_string();
    let (_req, _body) = record_and_extract(req, &log).await;

    for r in routes.iter() {
        if r.method.eq_ignore_ascii_case(&method) && path.starts_with(&r.path_prefix) {
            return Response::builder()
                .status(r.status)
                .header("content-type", "application/json")
                .body(axum::body::Body::from(serde_json::to_vec(&r.body).unwrap()))
                .unwrap();
        }
    }

    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            serde_json::to_vec(&json!({
                "error": {
                    "code": "fake_azure_no_fixture",
                    "message": format!("no fixture for {method} {path}")
                }
            }))
            .unwrap(),
        ))
        .unwrap()
}

// ---------------------------------------------------------------------------
// Server plumbing
// ---------------------------------------------------------------------------

async fn spawn_server(app: Router) -> (SocketAddr, tokio::task::JoinHandle<std::io::Result<()>>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move { axum::serve(listener, app).await });
    (addr, handle)
}

// ---------------------------------------------------------------------------
// Convenience — load a fixture JSON for inline assertions.
// ---------------------------------------------------------------------------

pub fn load_fixture(rel: &str) -> Value {
    let full = fixtures_dir().join(rel);
    let raw = std::fs::read(&full).unwrap_or_else(|e| panic!("fixture {}: {}", full.display(), e));
    serde_json::from_slice(&raw).unwrap()
}

/// Helper: convert headers slice → map for assertion ergonomics.
pub fn headers_to_map(headers: &HeaderMap) -> std::collections::HashMap<String, String> {
    headers
        .iter()
        .map(|(k, v)| {
            (
                k.to_string().to_lowercase(),
                v.to_str().unwrap_or("").to_string(),
            )
        })
        .collect()
}

/// Helper: absolute path to a fixture file (for tests that want raw bytes).
pub fn fixture_path(rel: &str) -> PathBuf {
    fixtures_dir().join(rel)
}

pub fn fixtures_exist() -> bool {
    StdPath::new(&fixtures_dir()).exists()
}
