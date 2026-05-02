// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `azureclaw-a2a-gateway` binary entry-point.
//!
//! Phase 3 S5: real proxy wiring. Replaces the v1 binary skeleton
//! that bound only the admin port and let `_replay`/`_limiter` drop
//! immediately. The audit (files/phase-0-1-2-DEEP-CODE-AUDIT.md §5)
//! called this out as the highest-value gap.
//!
//! ## Two listener modes
//!
//! - **Production** (default): rustls server on `:8443` using
//!   [`tls::spawn_reloader`] for hot-reload, mTLS reqwest client to
//!   the inference-router using [`mtls::load`] PEM files. The
//!   user-facing TLS terminates here; downstream is a separate mTLS
//!   trust domain.
//!
//! - **Test mode**: `A2A_GATEWAY_TEST_MODE=1` flips both legs to
//!   plain HTTP. Same gates run (replay-cache, rate-limit, subject
//!   propagation), so e2e and CI can exercise the full chain without
//!   a PKI infrastructure. The production path is one env-var away.
//!
//! ## Env vars
//!
//! | name                            | default                                        | meaning                                                |
//! |---------------------------------|------------------------------------------------|--------------------------------------------------------|
//! | `A2A_GATEWAY_TEST_MODE`         | `0`                                            | when `1`, plain-HTTP listener + plain-HTTP upstream    |
//! | `A2A_GATEWAY_PUBLIC_PORT`       | `8443`                                         | inbound listener port                                  |
//! | `A2A_GATEWAY_ADMIN_PORT`        | `9090`                                         | admin (`/healthz` `/readyz` `/metrics`) port           |
//! | `A2A_GATEWAY_TLS_CERT`          | `/etc/azureclaw/a2a-gateway-tls/tls.crt`       | server PEM cert                                        |
//! | `A2A_GATEWAY_TLS_KEY`           | `/etc/azureclaw/a2a-gateway-tls/tls.key`       | server PEM key                                         |
//! | `A2A_GATEWAY_MTLS_CERT`         | `/etc/azureclaw/a2a-gateway-mtls/tls.crt`      | client cert presented to router                        |
//! | `A2A_GATEWAY_MTLS_KEY`          | `/etc/azureclaw/a2a-gateway-mtls/tls.key`      | client key                                             |
//! | `A2A_GATEWAY_ROUTER_CA`         | `/etc/azureclaw/router-ca/ca.crt`              | CA bundle to validate the router's cert                |
//! | `A2A_GATEWAY_UPSTREAM_URL`      | `https://azureclaw-inference-router...:8444`   | upstream base URL (overridable for tests)              |
//! | `A2A_GATEWAY_ANONYMOUS_OK`      | `0`                                            | when `1`, allows traffic without subject header        |
//! | `A2A_GATEWAY_RATE_CAPACITY`     | `120`                                          | per-subject token bucket size                          |
//! | `A2A_GATEWAY_RATE_REFILL`       | `2.0`                                          | per-subject refill (tokens/sec)                        |
//! | `A2A_GATEWAY_REPLAY_TTL_SECS`   | `300`                                          | replay-cache window                                    |
//! | `A2A_GATEWAY_REPLAY_CAPACITY`   | `100000`                                       | replay-cache size                                      |

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use tokio::net::TcpListener;
use tracing::{info, warn};

use azureclaw_a2a_gateway::health::ReadyState;
use azureclaw_a2a_gateway::metrics::Metrics;
use azureclaw_a2a_gateway::proxy_app::{ProxyState, UpstreamClient, router as proxy_router};
use azureclaw_a2a_gateway::rate_limit::{BucketSpec, SubjectLimiter};
use azureclaw_a2a_gateway::tls::spawn_reloader;
use azureclaw_a2a_gateway::verify::ReplayCache;

#[derive(Clone)]
struct AdminState {
    metrics: Arc<Metrics>,
    ready: ReadyState,
}

fn env_bool(key: &str) -> bool {
    matches!(
        std::env::var(key).as_deref(),
        Ok("1" | "true" | "True" | "TRUE" | "yes")
    )
}

fn env_or<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
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

    let test_mode = env_bool("A2A_GATEWAY_TEST_MODE");
    let public_port: u16 = env_or("A2A_GATEWAY_PUBLIC_PORT", 8443);
    let admin_port: u16 = env_or("A2A_GATEWAY_ADMIN_PORT", 9090);
    let anonymous_ok = env_bool("A2A_GATEWAY_ANONYMOUS_OK");

    let metrics = Arc::new(Metrics::new());
    let ready = ReadyState::new();

    let replay_ttl = Duration::from_secs(env_or("A2A_GATEWAY_REPLAY_TTL_SECS", 300_u64));
    let replay_cap: usize = env_or("A2A_GATEWAY_REPLAY_CAPACITY", 100_000);
    let replay = Arc::new(ReplayCache::new(replay_ttl, replay_cap));

    let bucket = BucketSpec {
        capacity: env_or("A2A_GATEWAY_RATE_LIMIT_BURST", 120),
        refill_per_sec: env_or("A2A_GATEWAY_RATE_LIMIT_REFILL", 2.0_f64),
    };
    let max_subjects: usize = env_or("A2A_GATEWAY_MAX_SUBJECTS", 50_000);
    let limiter = Arc::new(SubjectLimiter::new(bucket, max_subjects));

    // Upstream client.
    let upstream = if test_mode {
        let url = std::env::var("A2A_GATEWAY_UPSTREAM_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:8443".into());
        info!(%url, "test mode: plain-HTTP upstream client");
        UpstreamClient::plain(url)
    } else {
        let url = std::env::var("A2A_GATEWAY_UPSTREAM_URL")
            .or_else(|_| {
                std::env::var("A2A_GATEWAY_ROUTER_HOST").map(|h| {
                    if h.starts_with("http") {
                        h
                    } else {
                        format!("https://{h}")
                    }
                })
            })
            .unwrap_or_else(|_| {
                "https://azureclaw-inference-router.azureclaw-system.svc.cluster.local:8444".into()
            });
        let cert: PathBuf = std::env::var("A2A_GATEWAY_MTLS_CERT")
            .unwrap_or_else(|_| "/etc/azureclaw/a2a-gateway-mtls/tls.crt".into())
            .into();
        let key: PathBuf = std::env::var("A2A_GATEWAY_MTLS_KEY")
            .unwrap_or_else(|_| "/etc/azureclaw/a2a-gateway-mtls/tls.key".into())
            .into();
        let ca: PathBuf = std::env::var("A2A_GATEWAY_ROUTER_CA")
            .unwrap_or_else(|_| "/etc/azureclaw/router-ca/ca.crt".into())
            .into();
        info!(%url, ?cert, ?key, ?ca, "production: mTLS upstream client");
        let cert_pem = std::fs::read(&cert)?;
        let key_pem = std::fs::read(&key)?;
        let ca_pem = std::fs::read(&ca)?;
        UpstreamClient::mtls(url, &cert_pem, &key_pem, &ca_pem)?
    };

    let proxy_state = ProxyState {
        metrics: metrics.clone(),
        replay,
        limiter,
        upstream,
        anonymous_ok,
    };

    // Admin server (always plain HTTP, ClusterIP-internal).
    let admin = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/metrics", get(metrics_handler))
        .with_state(AdminState {
            metrics: metrics.clone(),
            ready: ready.clone(),
        });
    let admin_addr: SocketAddr = format!("0.0.0.0:{admin_port}").parse()?;
    info!(%admin_addr, "admin listener starting");
    let admin_listener = TcpListener::bind(admin_addr).await?;
    let admin_task = tokio::spawn(async move {
        if let Err(e) = axum::serve(admin_listener, admin).await {
            warn!(error = %e, "admin server exited");
        }
    });

    // Public listener.
    let public_addr: SocketAddr = format!("0.0.0.0:{public_port}").parse()?;
    let proxy = proxy_router(proxy_state);

    if test_mode {
        info!(%public_addr, "test mode: plain-HTTP public listener starting");
        let listener = TcpListener::bind(public_addr).await?;
        ready.mark_ready();
        if let Err(e) = axum::serve(listener, proxy).await {
            warn!(error = %e, "public server exited");
        }
    } else {
        let cert: PathBuf = std::env::var("A2A_GATEWAY_TLS_CERT")
            .unwrap_or_else(|_| "/etc/azureclaw/a2a-gateway-tls/tls.crt".into())
            .into();
        let key: PathBuf = std::env::var("A2A_GATEWAY_TLS_KEY")
            .unwrap_or_else(|_| "/etc/azureclaw/a2a-gateway-tls/tls.key".into())
            .into();
        info!(%public_addr, ?cert, ?key, "production: rustls public listener starting");
        let mut tls_rx = spawn_reloader(cert, key)?;
        let listener = TcpListener::bind(public_addr).await?;
        ready.mark_ready();
        run_tls_loop(listener, &mut tls_rx, proxy, metrics.clone()).await?;
    }

    admin_task.abort();
    Ok(())
}

/// Per-connection TLS accept loop. Reads the latest [`Arc<ServerConfig>`]
/// from the watch channel for each connection so cert rotations take
/// effect on the next accept.
async fn run_tls_loop(
    listener: TcpListener,
    tls_rx: &mut tokio::sync::watch::Receiver<Arc<rustls::ServerConfig>>,
    app: Router,
    metrics: Arc<Metrics>,
) -> anyhow::Result<()> {
    use tokio_rustls::TlsAcceptor;
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "accept error");
                continue;
            }
        };
        let cfg = tls_rx.borrow().clone();
        let acceptor = TlsAcceptor::from(cfg);
        let app = app.clone();
        let m = metrics.clone();
        tokio::spawn(async move {
            m.active_connections.inc();
            let outcome = async {
                let tls_stream = acceptor.accept(stream).await?;
                let io = hyper_util::rt::TokioIo::new(tls_stream);
                let svc = hyper_util::service::TowerToHyperService::new(app);
                hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, svc)
                    .await
                    .map_err(|e| anyhow::anyhow!("hyper conn: {e}"))
            }
            .await;
            if let Err(e) = outcome {
                warn!(%peer, error = %e, "tls connection error");
            }
            m.active_connections.dec();
        });
    }
}

async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

async fn readyz(State(s): State<AdminState>) -> impl IntoResponse {
    if s.ready.is_ready() {
        (StatusCode::OK, "ready")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "not ready")
    }
}

async fn metrics_handler(State(s): State<AdminState>) -> impl IntoResponse {
    (
        StatusCode::OK,
        [("content-type", "text/plain; version=0.0.4")],
        s.metrics.render(),
    )
}
