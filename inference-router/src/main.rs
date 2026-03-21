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
mod budget;
mod config;
mod governance;
mod proxy;
mod routes;
mod safety;
mod metrics;

use anyhow::Result;
use axum::Router;
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

    let app = Router::new()
        .merge(routes::inference_routes())
        .merge(routes::foundry_agent_routes())
        .merge(routes::foundry_standalone_routes())
        .merge(routes::agt_routes())
        .merge(routes::health_routes())
        .merge(routes::metrics_routes())
        .with_state(state)
        // Rate limit: max 64 concurrent requests (prevents DoS from compromised agent)
        .layer(tower::limit::ConcurrencyLimitLayer::new(64));

    let addr = format!("0.0.0.0:{}", config.port);
    tracing::info!("Listening on {addr}");

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app)
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
