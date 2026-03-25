//! AzureClaw Inference Router
//!
//! High-performance reverse proxy that sits between sandboxed OpenClaw agents
//! and Azure AI backends. Every inference call from a sandbox flows through
//! this router, which handles:
//!
//! - **Authentication:** Workload Identity → Managed Identity token exchange.
//!   No API keys in the sandbox. Ever.
//! - **Model routing:** Declarative model selection per sandbox via ClawSandbox CRD.
//!   Instant switching, no restart.
//! - **Content safety:** Azure AI Content Safety + Prompt Shields enforcement
//!   (on by default, configurable per sandbox).
//! - **Token budgets:** Per-sandbox daily and per-request token limits with alerts.
//! - **Audit logging:** Every inference call logged with sandbox ID, model,
//!   token counts, latency, and content safety results.

mod auth;
mod blocklist;
mod budget;
mod config;
mod forward_proxy;
mod governance;
mod proxy;
mod routes;
mod safety;
mod metrics;
mod spawn;

use anyhow::Result;
use axum::{Router, extract::Request, http::StatusCode, middleware::Next, response::IntoResponse};
use std::sync::Arc;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "azureclaw_inference_router=info,tower_http=info".into()))
        .with(tracing_subscriber::fmt::layer().json())
        .init();

    tracing::info!("AzureClaw Inference Router starting");

    let config = config::Config::from_env()?;
    let state = routes::AppState::new(&config).await?;

    // Clone blocklist for the forward proxy before state is moved into the router.
    let proxy_blocklist = state.blocklist.clone();

    // Read admin token for protecting sensitive endpoints.
    // If ADMIN_TOKEN is set, /admin/*, /egress/*, /sandbox/*, and sensitive /agt/* endpoints
    // require Authorization: Bearer <token>. If unset, these endpoints are unrestricted
    // (backwards-compatible for local dev).
    let admin_token: Option<Arc<String>> = std::env::var("ADMIN_TOKEN")
        .ok()
        .filter(|t| !t.is_empty())
        .map(Arc::new);

    let app = {
        // Public routes — no admin token required (health, metrics, inference, Foundry proxies, mesh)
        let public = Router::new()
            .merge(routes::inference_routes())
            .merge(routes::foundry_agent_routes())
            .merge(routes::foundry_standalone_routes())
            .merge(routes::health_routes())
            .merge(routes::metrics_routes())
            .merge(routes::mesh_routes());

        // Protected routes — require admin token when configured
        let protected = Router::new()
            .merge(routes::admin_routes())
            .merge(routes::egress_routes())
            .merge(routes::spawn_routes())
            .merge(routes::sensitive_agt_routes());

        let protected = if let Some(ref token) = admin_token {
            let token = token.clone();
            protected.layer(axum::middleware::from_fn(move |req, next| {
                let token = token.clone();
                admin_auth_middleware(token, req, next)
            }))
        } else {
            tracing::warn!("ADMIN_TOKEN not set — sensitive endpoints are unauthenticated");
            protected
        };

        public
            .merge(protected)
            .with_state(state)
            .layer(tower::limit::ConcurrencyLimitLayer::new(64))
    };

    let addr = format!("0.0.0.0:{}", config.port);
    tracing::info!("Listening on {addr}");

    // Start the transparent forward proxy on a separate port.
    // iptables REDIRECT sends TCP 80/443 from UID 1000 here for blocklist enforcement.
    let proxy_port = std::env::var("FORWARD_PROXY_PORT")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(8444);
    let proxy_addr = format!("127.0.0.1:{proxy_port}");
    tokio::spawn(async move {
        forward_proxy::start(&proxy_addr, proxy_blocklist).await;
    });

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    tracing::info!("Inference router shut down gracefully");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.expect("failed to install Ctrl+C handler");
    };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => tracing::info!("Received SIGINT, shutting down"),
        _ = terminate => tracing::info!("Received SIGTERM, shutting down"),
    }
}

/// Middleware that gates protected endpoints for non-localhost callers.
///
/// - Localhost (127.0.0.1 / ::1) → always allowed (agent + kubectl exec are same-pod)
/// - Non-localhost → requires `Authorization: Bearer <ADMIN_TOKEN>` (cross-pod mesh traffic)
///
/// This ensures the agent process can call /egress/fetch, /sandbox/spawn, etc. without a token,
/// while preventing other pods in the cluster from calling sensitive endpoints without auth.
async fn admin_auth_middleware(
    expected_token: Arc<String>,
    req: Request,
    next: Next,
) -> impl IntoResponse {
    // Allow all localhost connections without auth — same pod is trusted.
    if let Some(connect_info) = req.extensions().get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        && connect_info.0.ip().is_loopback()
    {
        return next.run(req).await.into_response();
    }

    // Non-localhost: require bearer token
    let auth_header = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok());

    match auth_header {
        Some(value) if value.starts_with("Bearer ") => {
            let provided = &value[7..];
            if provided == expected_token.as_str() {
                next.run(req).await.into_response()
            } else {
                tracing::warn!(
                    path = %req.uri().path(),
                    "Admin auth: invalid token from non-localhost"
                );
                (StatusCode::UNAUTHORIZED, "Invalid admin token").into_response()
            }
        }
        _ => {
            tracing::warn!(
                path = %req.uri().path(),
                "Admin auth: non-localhost request without token"
            );
            (StatusCode::UNAUTHORIZED, "Admin token required for non-localhost access").into_response()
        }
    }
}
