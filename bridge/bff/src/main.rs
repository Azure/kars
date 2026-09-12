// Copyright (c) Pal Lakatos-Toth.
// kars Bridge BFF — secure backend-for-frontend for the kars Bridge web app.
//
// This process is the *only* server-side path between the browser and the
// kars cluster. The browser never holds kube credentials, signing keys, or
// the Entra confidential-client secret — all privileged access terminates
// here. (Inc 0: platform shell. Auth + cluster access land in later slices,
// each fully wired, never stubbed.)

use std::time::Duration;

use anyhow::Context;
use axum::http::{HeaderValue, Method};
use kars_bridge_bff::config::Config;
use kars_bridge_bff::routes;
use kars_bridge_bff::state::AppState;
use tokio::net::TcpListener;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // kube-rs (rustls 0.23) requires a process-level CryptoProvider to be
    // installed before any TLS client is built. Install the aws-lc-rs provider
    // explicitly so cluster connections don't panic on first use.
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .map_err(|_| anyhow::anyhow!("failed to install default rustls CryptoProvider"))?;

    let config = Config::from_env().context("loading configuration")?;
    init_tracing(&config);

    tracing::info!(
        bind = %config.bind_addr,
        web_origin = %config.web_origin,
        "starting kars-bridge-bff"
    );

    let cors = build_cors(&config)?;

    let state = AppState::new(config.default_namespace.clone())
        .await
        .with_api_token(config.api_token.clone())
        .with_principal_secret(config.principal_secret.clone())
        .with_teams_internal_secret(config.teams_internal_secret.clone())
        .with_teams_entra_role_map(config.teams_entra_role_map.clone());
    if config.api_token.is_none() {
        tracing::warn!(
            "BRIDGE_API_TOKEN not set — mutating endpoints are UNAUTHENTICATED. \
             Set BRIDGE_API_TOKEN in production to require a bearer token on writes."
        );
    }
    if config.principal_secret.is_none() {
        tracing::warn!(
            "BRIDGE_PRINCIPAL_SECRET not set — per-user API authentication and \
             persona authorization are disabled (local dev mode only)."
        );
    }

    let poller_interval = Duration::from_secs(config.engineering_poller_interval_seconds);
    routes::engineering::spawn_poller(state.clone(), poller_interval);

    // Orchestrator inference path. Two options, in priority order:
    //   1. Direct endpoint — set BRIDGE_ORCHESTRATOR_{ENDPOINT,TOKEN,MODEL} to
    //      route compose straight at a managed model (Azure AI Foundry / Azure
    //      OpenAI). Scales with the managed service — preferred for many teams /
    //      high query volume — and stands up NO pod.
    //   2. Self-contained — otherwise, stand up a single standing
    //      `bridge-orchestrator` sandbox the composer always routes through, so
    //      "intent → recommendations" works out of the box with no external
    //      dependency (one idle pod).
    let has_direct_endpoint = std::env::var("BRIDGE_ORCHESTRATOR_ENDPOINT")
        .map(|e| !e.trim().is_empty())
        .unwrap_or(false);
    if !has_direct_endpoint && let Some(cluster) = state.cluster() {
        match cluster.ensure_orchestrator_sandbox().await {
            Ok(()) => tracing::info!(
                "orchestrator: standing `bridge-orchestrator` sandbox ensured (compose inference path)"
            ),
            Err(e) => tracing::warn!(
                "orchestrator: could not ensure standing sandbox (compose falls back to any running sandbox): {e}"
            ),
        }
    } else if has_direct_endpoint {
        tracing::info!(
            "orchestrator: using direct endpoint (BRIDGE_ORCHESTRATOR_*) — no standing pod"
        );
    }

    let app = routes::router(state.clone())
        .layer(axum::middleware::from_fn_with_state(
            state,
            kars_bridge_bff::auth::require_token,
        ))
        .layer(axum::middleware::from_fn(ensure_utf8_charset))
        .layer(cors)
        .layer(TraceLayer::new_for_http());

    let listener = TcpListener::bind(config.bind_addr)
        .await
        .with_context(|| format!("binding {}", config.bind_addr))?;

    tracing::info!(addr = %config.bind_addr, "listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server error")?;

    tracing::info!("shutdown complete");
    Ok(())
}

/// Ensure `application/json` responses declare `charset=utf-8`. JSON is always
/// UTF-8 per RFC 8259, but naive/Latin-1 viewers mis-decode multi-byte
/// characters (en-dashes, curly quotes) into mojibake (`1â€"5`) when the charset
/// is absent. Stamping it makes every response render correctly everywhere. SSE
/// and other content types are left untouched.
async fn ensure_utf8_charset(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let mut res = next.run(req).await;
    let headers = res.headers_mut();
    if let Some(ct) = headers.get(axum::http::header::CONTENT_TYPE)
        && ct
            .to_str()
            .map(|v| v.trim() == "application/json")
            .unwrap_or(false)
    {
        headers.insert(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/json; charset=utf-8"),
        );
    }
    res
}

/// Initialize structured logging. JSON in production, pretty locally.
fn init_tracing(config: &Config) {
    let filter = EnvFilter::try_new(&config.log_filter).unwrap_or_else(|_| EnvFilter::new("info"));
    let registry = tracing_subscriber::registry().with(filter);
    if config.log_json {
        registry
            .with(tracing_subscriber::fmt::layer().json())
            .init();
    } else {
        registry.with(tracing_subscriber::fmt::layer()).init();
    }
}

/// Build a strict CORS layer: only the configured web origin, only the
/// methods and headers the app uses, credentials allowed for cookie auth.
fn build_cors(config: &Config) -> anyhow::Result<CorsLayer> {
    let origin: HeaderValue = config
        .web_origin
        .parse()
        .with_context(|| format!("invalid BRIDGE_WEB_ORIGIN `{}`", config.web_origin))?;
    Ok(CorsLayer::new()
        .allow_origin(origin)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
        ])
        .allow_headers([
            axum::http::header::CONTENT_TYPE,
            axum::http::header::AUTHORIZATION,
        ])
        .allow_credentials(true)
        .max_age(Duration::from_secs(600)))
}

/// Resolve when the process receives SIGINT or SIGTERM, enabling graceful
/// shutdown of in-flight requests.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutdown signal received");
}
