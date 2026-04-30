//! `azureclaw-a2a-gateway` binary entry-point.
//!
//! Wires:
//! - `tls::spawn_reloader` — server TLS for :8443.
//! - `mtls::load` — upstream TLS for inference-router :8444.
//! - `verify::ReplayCache` — replay protection.
//! - `rate_limit::SubjectLimiter` — per-subject token-bucket.
//! - `metrics::Metrics` — Prometheus on :9090.
//! - `health::ReadyState` — `/healthz` + `/readyz` on :9090.
//!
//! Configuration is by env var — no CRD in S3.5 (Helm value →
//! Deployment env var → here).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use tracing::info;

use azureclaw_a2a_gateway::health::ReadyState;
use azureclaw_a2a_gateway::metrics::Metrics;
use azureclaw_a2a_gateway::rate_limit::{BucketSpec, SubjectLimiter};
use azureclaw_a2a_gateway::verify::ReplayCache;

#[derive(Clone)]
struct AppState {
    metrics: Arc<Metrics>,
    ready: ReadyState,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .json()
        .init();

    let _tls_cert: PathBuf = std::env::var("A2A_GATEWAY_TLS_CERT")
        .unwrap_or_else(|_| "/etc/azureclaw/a2a-gateway-tls/tls.crt".into())
        .into();
    let _tls_key: PathBuf = std::env::var("A2A_GATEWAY_TLS_KEY")
        .unwrap_or_else(|_| "/etc/azureclaw/a2a-gateway-tls/tls.key".into())
        .into();
    let _mtls_cert: PathBuf = std::env::var("A2A_GATEWAY_MTLS_CERT")
        .unwrap_or_else(|_| "/etc/azureclaw/a2a-gateway-mtls/tls.crt".into())
        .into();
    let _mtls_key: PathBuf = std::env::var("A2A_GATEWAY_MTLS_KEY")
        .unwrap_or_else(|_| "/etc/azureclaw/a2a-gateway-mtls/tls.key".into())
        .into();
    let _router_ca: PathBuf = std::env::var("A2A_GATEWAY_ROUTER_CA")
        .unwrap_or_else(|_| "/etc/azureclaw/router-ca/ca.crt".into())
        .into();
    let _router_host = std::env::var("A2A_GATEWAY_ROUTER_HOST").unwrap_or_else(|_| {
        "azureclaw-inference-router.azureclaw-system.svc.cluster.local:8444".into()
    });

    let metrics = Arc::new(Metrics::new());
    let ready = ReadyState::new();
    let _replay = Arc::new(ReplayCache::new(Duration::from_secs(300), 100_000));
    let _limiter = Arc::new(SubjectLimiter::new(BucketSpec::default(), 50_000));

    let state = AppState {
        metrics: metrics.clone(),
        ready: ready.clone(),
    };

    let admin = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/metrics", get(metrics_handler))
        .with_state(state);

    let admin_addr: SocketAddr = "0.0.0.0:9090".parse()?;
    info!(%admin_addr, "a2a-gateway admin server starting");
    let admin_listener = tokio::net::TcpListener::bind(admin_addr).await?;

    // Mark ready once admin is up. The full TLS listener wiring is
    // intentionally out of scope for the v1 binary skeleton — the
    // gateway is opt-in via Helm and the production wiring lands in
    // a follow-up slice that pairs with AGC (Application Gateway for
    // Containers). The library API + tests cover the security-critical
    // paths.
    ready.mark_ready();

    axum::serve(admin_listener, admin).await?;
    Ok(())
}

async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

async fn readyz(State(s): State<AppState>) -> impl IntoResponse {
    if s.ready.is_ready() {
        (StatusCode::OK, "ready")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "not ready")
    }
}

async fn metrics_handler(State(s): State<AppState>) -> impl IntoResponse {
    (
        StatusCode::OK,
        [("content-type", "text/plain; version=0.0.4")],
        s.metrics.render(),
    )
}
