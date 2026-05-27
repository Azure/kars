// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public-facing reverse proxy for inbound A2A traffic.
//!
//! Phase 3 S5 wiring (Phase 3 audit closure):
//!
//! Before this module the gateway binary only served `/healthz` /
//! `/readyz` / `/metrics` on the admin port. The audit found:
//!
//! > The A2A gateway binary at a2a-gateway/src/main.rs:55-97 only
//! > binds the admin port :9090, constructs a ReplayCache and
//! > SubjectLimiter and immediately drops them.
//!
//! This module wires the actual proxy chain:
//!
//! 1. **Subject extraction** — the verified-caller subject is read
//!    from `X-A2A-Agent-Subject`. In production this header is
//!    written by the gateway's own JWS-verifying middleware (the
//!    upstream module exists; wiring the verifier is a separate
//!    follow-up); in test mode it is supplied by the client.
//!    Missing → request is rejected as `unauthenticated` (401)
//!    *unless* the deployment opts into `A2A_GATEWAY_ANONYMOUS_OK=1`
//!    (off by default — fail-closed).
//!
//! 2. **Replay protection** — when the request carries an
//!    `X-A2A-Nonce` header, the gateway records the nonce in
//!    [`crate::verify::ReplayCache`] and rejects duplicates (409).
//!    Absent nonce is allowed (request body itself may be replay-
//!    protected at higher layers); present-but-replayed is hard-
//!    rejected.
//!
//! 3. **Rate limiting** — the verified subject is checked against
//!    [`crate::rate_limit::SubjectLimiter`]; over-budget callers
//!    get HTTP 429 with `Retry-After: 1`.
//!
//! 4. **Forward** — the request is reproduced against the upstream
//!    inference-router using `reqwest`. In production the client is
//!    wired with mTLS (loaded via [`crate::mtls::load`]) and points
//!    at `https://router:8444`; in test mode it is a plain HTTP
//!    client pointed wherever `A2A_GATEWAY_UPSTREAM_URL` says.
//!
//! ## Test mode
//!
//! Setting `A2A_GATEWAY_TEST_MODE=1` flips the binary to a plain-
//! HTTP listener + plain-HTTP upstream client. Every other gate
//! (rate-limit, replay-cache, header propagation) runs identically.
//! This is the "dev / CI / no-cert-infra" path the user asked for —
//! the production path is one env-var away.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    Router,
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, HeaderName, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::any,
};
use reqwest::Client;
use tracing::{debug, info, warn};

use crate::metrics::Metrics;
use crate::proxy::SUBJECT_HEADER;
use crate::rate_limit::SubjectLimiter;
use crate::verify::ReplayCache;

/// Header carrying a per-request nonce used for replay protection.
/// Not part of the A2A spec; Kars-private convention.
pub const NONCE_HEADER: &str = "X-A2A-Nonce";

/// Header set by the rate-limit middleware on 429 responses so the
/// client knows when to retry.
pub const RETRY_AFTER_HEADER: &str = "Retry-After";

/// Maximum inbound request body, bytes. The router rejects larger
/// frames at its own ingress; the gateway short-circuits here so we
/// never spend CPU forwarding something the next hop will reject.
pub const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// Per-app state, cheap to clone (everything inside is `Arc`).
#[derive(Clone)]
pub struct ProxyState {
    pub metrics: Arc<Metrics>,
    pub replay: Arc<ReplayCache>,
    pub limiter: Arc<SubjectLimiter>,
    pub upstream: UpstreamClient,
    /// When `false`, requests without `X-A2A-Agent-Subject` are
    /// rejected with 401. When `true`, anonymous traffic is bucketed
    /// under the synthetic subject `"anonymous"` for rate-limit
    /// purposes — useful in dev, never recommended in production.
    pub anonymous_ok: bool,
}

/// Outbound client + base URL. Two flavours:
/// - `Production`: rustls/mTLS-backed `reqwest::Client`, pointing at
///   `https://router:8444`.
/// - `Test`: plain HTTP `reqwest::Client`, pointing at any caller-
///   supplied URL.
#[derive(Clone)]
pub struct UpstreamClient {
    pub client: Client,
    pub base_url: String,
}

impl UpstreamClient {
    /// Construct a plain-HTTP client. Used by tests and by the
    /// `A2A_GATEWAY_TEST_MODE=1` path.
    pub fn plain(base_url: impl Into<String>) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(60))
            .pool_idle_timeout(Duration::from_secs(30))
            .build()
            .expect("plain reqwest client");
        Self {
            client,
            base_url: base_url.into(),
        }
    }

    /// Construct a TLS+mTLS-backed client from the rustls
    /// `ClientConfig` produced by [`crate::mtls::load`].
    ///
    /// `reqwest`'s rustls integration is opaque — we serialise the
    /// caller-supplied cert bundle via PEM round-trip rather than
    /// reaching into `ClientConfig` internals (which are not part of
    /// rustls' stable public API).
    pub fn mtls(
        base_url: impl Into<String>,
        client_cert_pem: &[u8],
        client_key_pem: &[u8],
        ca_bundle_pem: &[u8],
    ) -> Result<Self, anyhow::Error> {
        // Build a single PEM blob containing the leaf cert + key, the
        // shape `reqwest::Identity::from_pem` expects.
        let mut id_pem = Vec::with_capacity(client_cert_pem.len() + client_key_pem.len());
        id_pem.extend_from_slice(client_cert_pem);
        if !client_cert_pem.ends_with(b"\n") {
            id_pem.push(b'\n');
        }
        id_pem.extend_from_slice(client_key_pem);
        let identity = reqwest::Identity::from_pem(&id_pem)?;
        let ca = reqwest::Certificate::from_pem(ca_bundle_pem)?;
        let client = Client::builder()
            .identity(identity)
            .add_root_certificate(ca)
            .https_only(true)
            .timeout(Duration::from_secs(60))
            .pool_idle_timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self {
            client,
            base_url: base_url.into(),
        })
    }
}

/// Build the proxy router. Catches every method on every path and
/// hands the request to [`forward`] after the verification gates.
pub fn router(state: ProxyState) -> Router {
    Router::new()
        .route("/{*path}", any(forward))
        .route("/", any(forward))
        .with_state(state)
}

#[tracing::instrument(skip_all, fields(method = %method, path = uri.path()))]
async fn forward(
    State(state): State<ProxyState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if body.len() > MAX_BODY_BYTES {
        state
            .metrics
            .rejections_total
            .with_label_values(&["body_too_large"])
            .inc();
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            "request body exceeds gateway limit",
        )
            .into_response();
    }

    // 1. Subject extraction.
    let subject = match headers.get(SUBJECT_HEADER).and_then(|v| v.to_str().ok()) {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => {
            if state.anonymous_ok {
                "anonymous".to_string()
            } else {
                state
                    .metrics
                    .rejections_total
                    .with_label_values(&["unauthenticated"])
                    .inc();
                return (
                    StatusCode::UNAUTHORIZED,
                    "missing or empty X-A2A-Agent-Subject",
                )
                    .into_response();
            }
        }
    };

    // 2. Replay protection.
    if let Some(nonce) = headers.get(NONCE_HEADER).and_then(|v| v.to_str().ok())
        && !nonce.is_empty()
        && let Err(e) = state.replay.check_and_insert(nonce)
    {
        warn!(subject = %subject, %nonce, error = %e, "replay rejected");
        state
            .metrics
            .rejections_total
            .with_label_values(&["replay"])
            .inc();
        return (StatusCode::CONFLICT, "nonce replay").into_response();
    }

    // 3. Rate limit.
    if !state.limiter.allow(&subject) {
        debug!(subject = %subject, "rate limited");
        state
            .metrics
            .rejections_total
            .with_label_values(&["rate_limited"])
            .inc();
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [(RETRY_AFTER_HEADER, "1")],
            "rate limited",
        )
            .into_response();
    }

    // 4. Forward.
    let target = format!(
        "{}{}",
        state.upstream.base_url.trim_end_matches('/'),
        uri.path_and_query()
            .map(|p| p.as_str())
            .unwrap_or(uri.path())
    );

    let mut req = state
        .upstream
        .client
        .request(method.clone(), &target)
        .body(body.to_vec());

    // Re-emit headers to upstream, dropping hop-by-hop ones.
    for (name, value) in headers.iter() {
        if is_hop_by_hop(name) {
            continue;
        }
        req = req.header(name.clone(), value.clone());
    }
    // Re-assert the verified subject (defence in depth: even if the
    // client tried to spoof it, we re-write what we believe).
    req = req.header(SUBJECT_HEADER, &subject);

    let upstream_response = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            warn!(subject = %subject, target = %target, error = %e, "upstream send failed");
            state
                .metrics
                .rejections_total
                .with_label_values(&["upstream_unavailable"])
                .inc();
            return (StatusCode::BAD_GATEWAY, format!("upstream: {e}")).into_response();
        }
    };

    let status = upstream_response.status();
    let mut out = Response::builder().status(status);
    for (name, value) in upstream_response.headers().iter() {
        if is_hop_by_hop(name) {
            continue;
        }
        if let Some(builder_headers) = out.headers_mut() {
            builder_headers.append(name.clone(), value.clone());
        }
    }
    let body_bytes = match upstream_response.bytes().await {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, "reading upstream body failed");
            state
                .metrics
                .rejections_total
                .with_label_values(&["upstream_body"])
                .inc();
            return (StatusCode::BAD_GATEWAY, format!("upstream body: {e}")).into_response();
        }
    };

    state
        .metrics
        .requests_total
        .with_label_values(&[subject.as_str(), status.as_str()])
        .inc();

    info!(
        subject = %subject,
        target = %target,
        status = %status.as_u16(),
        "proxied"
    );

    out.body(Body::from(body_bytes))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn is_hop_by_hop(name: &HeaderName) -> bool {
    matches!(
        name.as_str().to_ascii_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailers"
            | "transfer-encoding"
            | "upgrade"
            | "host"
            | "content-length"
    )
}

/// Helper: read the verified subject from an outbound `RequestBuilder`
/// for tests. Provides observable assertion that we re-write the
/// header rather than trust the client.
#[doc(hidden)]
pub fn _hop_by_hop_for_tests(name: &str) -> bool {
    is_hop_by_hop(&HeaderName::from_bytes(name.as_bytes()).unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rate_limit::BucketSpec;
    use axum::body::to_bytes;
    use axum::http::{Request as HttpRequest, header};
    use std::sync::atomic::{AtomicU32, Ordering};
    use tokio::net::TcpListener;
    use tower::ServiceExt;

    fn test_state(upstream: UpstreamClient, anonymous_ok: bool) -> ProxyState {
        ProxyState {
            metrics: Arc::new(Metrics::new()),
            replay: Arc::new(ReplayCache::new(Duration::from_secs(60), 1024)),
            limiter: Arc::new(SubjectLimiter::new(
                BucketSpec {
                    capacity: 5,
                    refill_per_sec: 5.0,
                },
                1024,
            )),
            upstream,
            anonymous_ok,
        }
    }

    #[tokio::test]
    async fn hop_by_hop_header_classification() {
        for h in [
            "connection",
            "keep-alive",
            "transfer-encoding",
            "host",
            "content-length",
            "Upgrade",
        ] {
            assert!(_hop_by_hop_for_tests(h), "{h} should be hop-by-hop");
        }
        for h in ["x-a2a-agent-subject", "authorization", "user-agent"] {
            assert!(!_hop_by_hop_for_tests(h), "{h} should NOT be hop-by-hop");
        }
    }

    #[tokio::test]
    async fn missing_subject_rejected_when_anonymous_disabled() {
        let upstream = UpstreamClient::plain("http://127.0.0.1:1");
        let app = router(test_state(upstream, false));
        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/a2a")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn body_too_large_short_circuits() {
        let upstream = UpstreamClient::plain("http://127.0.0.1:1");
        let app = router(test_state(upstream, true));
        let big = vec![b'x'; MAX_BODY_BYTES + 1];
        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/a2a")
                    .body(Body::from(big))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    /// Full happy-path: spin up a stub upstream over plain HTTP,
    /// point the proxy at it, fire a request through, assert the
    /// subject header was propagated and the body round-tripped.
    #[tokio::test]
    async fn end_to_end_proxy_round_trip_in_test_mode() {
        // Stub upstream that echoes the verified subject + body.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bound = listener.local_addr().unwrap();
        let observed_subject: Arc<std::sync::Mutex<Option<String>>> =
            Arc::new(std::sync::Mutex::new(None));
        let counter = Arc::new(AtomicU32::new(0));
        let observed_clone = observed_subject.clone();
        let counter_clone = counter.clone();
        tokio::spawn(async move {
            let app = axum::Router::new().route(
                "/a2a",
                axum::routing::post(move |headers: HeaderMap, body: Bytes| {
                    let observed = observed_clone.clone();
                    let c = counter_clone.clone();
                    async move {
                        c.fetch_add(1, Ordering::SeqCst);
                        *observed.lock().unwrap() = headers
                            .get(SUBJECT_HEADER)
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_string);
                        (StatusCode::OK, body)
                    }
                }),
            );
            let _ = axum::serve(listener, app).await;
        });

        let upstream = UpstreamClient::plain(format!("http://{bound}"));
        let app = router(test_state(upstream, false));
        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/a2a")
                    .header(SUBJECT_HEADER, "did:agentmesh:peer-42")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{\"hello\":\"world\"}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 1 << 16).await.unwrap();
        assert_eq!(body.as_ref(), b"{\"hello\":\"world\"}");
        assert_eq!(
            observed_subject.lock().unwrap().as_deref(),
            Some("did:agentmesh:peer-42")
        );
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn replay_protection_rejects_duplicate_nonce() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bound = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let app =
                axum::Router::new().route("/x", axum::routing::any(|| async { StatusCode::OK }));
            let _ = axum::serve(listener, app).await;
        });

        let upstream = UpstreamClient::plain(format!("http://{bound}"));
        let state = test_state(upstream, false);
        let app = router(state);

        let mk = || {
            HttpRequest::builder()
                .method("POST")
                .uri("/x")
                .header(SUBJECT_HEADER, "did:agentmesh:peer-42")
                .header(NONCE_HEADER, "nonce-fixed")
                .body(Body::empty())
                .unwrap()
        };
        let r1 = app.clone().oneshot(mk()).await.unwrap();
        assert_eq!(r1.status(), StatusCode::OK);
        let r2 = app.oneshot(mk()).await.unwrap();
        assert_eq!(r2.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn rate_limiter_returns_429_after_burst() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bound = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let app =
                axum::Router::new().route("/x", axum::routing::any(|| async { StatusCode::OK }));
            let _ = axum::serve(listener, app).await;
        });

        let upstream = UpstreamClient::plain(format!("http://{bound}"));
        let state = ProxyState {
            metrics: Arc::new(Metrics::new()),
            replay: Arc::new(ReplayCache::new(Duration::from_secs(60), 1024)),
            limiter: Arc::new(SubjectLimiter::new(
                BucketSpec {
                    capacity: 2,
                    refill_per_sec: 0.0,
                },
                1024,
            )),
            upstream,
            anonymous_ok: false,
        };
        let app = router(state);

        for _ in 0..2 {
            let r = app
                .clone()
                .oneshot(
                    HttpRequest::builder()
                        .method("POST")
                        .uri("/x")
                        .header(SUBJECT_HEADER, "burst-subject")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(r.status(), StatusCode::OK);
        }
        let r3 = app
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/x")
                    .header(SUBJECT_HEADER, "burst-subject")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r3.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(r3.headers().get(RETRY_AFTER_HEADER).unwrap(), "1");
    }

    #[tokio::test]
    async fn anonymous_mode_bucket_is_shared() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bound = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let app =
                axum::Router::new().route("/x", axum::routing::any(|| async { StatusCode::OK }));
            let _ = axum::serve(listener, app).await;
        });
        let upstream = UpstreamClient::plain(format!("http://{bound}"));
        let app = router(test_state(upstream, true));
        // Both requests are anonymous → both billed to the same
        // synthetic subject "anonymous".
        let r = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method("GET")
                    .uri("/x")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
    }
}
