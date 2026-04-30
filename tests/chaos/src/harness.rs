// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Shared chaos-test harness — small axum mock servers + counters.
//!
//! Tests inject arbitrary chaos (status code, delay, body) on the server
//! side, drive HTTP/WebSocket traffic against it, and assert reliability
//! invariants on the client side.

use axum::{
    Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::Mutex;

/// Per-request scripted behavior. The mock pops the front of `script` for
/// every request; if empty, returns 200 OK with an empty body.
#[derive(Clone, Default)]
pub struct ChaosScript {
    inner: Arc<Mutex<Vec<ChaosResponse>>>,
    pub call_count: Arc<AtomicU64>,
    pub last_retry_after_observed: Arc<Mutex<Option<u64>>>,
}

#[derive(Clone, Debug)]
pub struct ChaosResponse {
    pub status: u16,
    pub retry_after: Option<u64>,
    pub body: Vec<u8>,
    /// Optional pre-response delay in ms (real wall time). Use sparingly —
    /// most tests use `tokio::time::pause()` instead.
    pub delay_ms: u64,
}

impl ChaosResponse {
    pub fn status(code: u16) -> Self {
        Self {
            status: code,
            retry_after: None,
            body: Vec::new(),
            delay_ms: 0,
        }
    }

    pub fn ok() -> Self {
        Self::status(200).body(b"{}")
    }

    pub fn retry_after(mut self, secs: u64) -> Self {
        self.retry_after = Some(secs);
        self
    }

    pub fn body(mut self, body: &[u8]) -> Self {
        self.body = body.to_vec();
        self
    }

    pub fn slow(mut self, ms: u64) -> Self {
        self.delay_ms = ms;
        self
    }
}

impl ChaosScript {
    pub fn new(script: Vec<ChaosResponse>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(script)),
            call_count: Arc::new(AtomicU64::new(0)),
            last_retry_after_observed: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn next(&self) -> ChaosResponse {
        let mut g = self.inner.lock().await;
        if g.is_empty() {
            ChaosResponse::ok()
        } else {
            g.remove(0)
        }
    }

    pub fn calls(&self) -> u64 {
        self.call_count.load(Ordering::SeqCst)
    }
}

async fn serve(State(script): State<ChaosScript>, _headers: HeaderMap) -> Response {
    script.call_count.fetch_add(1, Ordering::SeqCst);
    let r = script.next().await;
    if r.delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(r.delay_ms)).await;
    }
    let mut hdrs = HeaderMap::new();
    if let Some(ra) = r.retry_after {
        hdrs.insert("retry-after", ra.to_string().parse().expect("ra header"));
    }
    let status = StatusCode::from_u16(r.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, hdrs, r.body).into_response()
}

/// Bind a chaos mock server to a random localhost port and return its base URL
/// + the script handle (for later mutation / assertions).
pub async fn start_chaos_server(script: ChaosScript) -> (String, ChaosScript) {
    let app = Router::new()
        .route("/", any(serve))
        .route("/{*rest}", any(serve))
        .with_state(script.clone());

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr: SocketAddr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (format!("http://{addr}"), script)
}

/// Reqwest client tuned for chaos testing — short timeouts, no connection
/// pool reuse across tests.
pub fn chaos_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .pool_max_idle_per_host(0)
        .build()
        .expect("client")
}

/// A retry helper that mirrors the controller/router pattern: respect
/// `Retry-After`, exponential backoff with cap, max attempts.
///
/// Returns the final response status. Uses `tokio::time::sleep`, but tests
/// drive it via `tokio::time::pause()` + `advance()` so wall time stays
/// sub-second.
pub async fn http_with_retry(
    client: &reqwest::Client,
    url: &str,
    max_attempts: u32,
) -> Result<(u16, Vec<u8>), String> {
    let mut backoff = Duration::from_millis(50);
    for attempt in 0..max_attempts {
        let resp = match client.get(url).send().await {
            Ok(r) => r,
            Err(e) => {
                if attempt + 1 == max_attempts {
                    return Err(format!("send error: {e}"));
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(30));
                continue;
            }
        };
        let status = resp.status().as_u16();
        // Retry-After: respect on 429 and 503.
        if (status == 429 || status == 503) && attempt + 1 < max_attempts {
            let wait = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .map(Duration::from_secs)
                .unwrap_or(backoff);
            tokio::time::sleep(wait).await;
            backoff = (backoff * 2).min(Duration::from_secs(30));
            continue;
        }
        if status >= 500 && attempt + 1 < max_attempts {
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(30));
            continue;
        }
        let bytes = resp.bytes().await.map_err(|e| e.to_string())?.to_vec();
        return Ok((status, bytes));
    }
    Err("max attempts exhausted".into())
}
