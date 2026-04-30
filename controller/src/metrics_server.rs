//! Minimal axum HTTP server exposing controller metrics + health.
//!
//! S7.E: `:9091/metrics` (Prometheus text format) and
//! `:9091/healthz` (always 200 once the task is running). Bind
//! address overridable via `CONTROLLER_METRICS_ADDR` env (default
//! `0.0.0.0:9091` so the K8s scrape pulls work without manual
//! tuning). Set to empty string or `disabled` to opt out.

use anyhow::{Result, anyhow};
use axum::{
    Router,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use std::net::SocketAddr;
use tokio::net::TcpListener;

const DEFAULT_ADDR: &str = "0.0.0.0:9091";

/// Read the bind address from the environment. `None` means the
/// caller should skip starting the server (operator opt-out).
pub fn bind_addr_from_env() -> Option<String> {
    match std::env::var("CONTROLLER_METRICS_ADDR") {
        Ok(s) if s.is_empty() || s.eq_ignore_ascii_case("disabled") => None,
        Ok(s) => Some(s),
        Err(_) => Some(DEFAULT_ADDR.to_string()),
    }
}

pub async fn run(addr: String) -> Result<()> {
    let socket: SocketAddr = addr
        .parse()
        .map_err(|e| anyhow!("invalid CONTROLLER_METRICS_ADDR `{addr}`: {e}"))?;
    let listener = TcpListener::bind(socket).await?;
    let app = Router::new()
        .route("/metrics", get(metrics_handler))
        .route("/healthz", get(|| async { "ok" }));
    tracing::info!(%socket, "controller metrics server listening");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn metrics_handler() -> Response {
    use prometheus::Encoder;
    let encoder = prometheus::TextEncoder::new();
    let families = prometheus::gather();
    let mut buf = Vec::new();
    if let Err(e) = encoder.encode(&families, &mut buf) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("encode error: {e}"),
        )
            .into_response();
    }
    match String::from_utf8(buf) {
        Ok(body) => (
            StatusCode::OK,
            [(
                axum::http::header::CONTENT_TYPE,
                "text/plain; version=0.0.4",
            )],
            body,
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("utf8 error: {e}"),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_addr_from_env_default() {
        // Calling outside the env-mutation safety zone — just verify
        // the function returns a parseable thing in the absence of
        // the env var. Using a unique-name var to avoid races.
        let _ = std::env::var("CONTROLLER_METRICS_ADDR"); // probe only.
        let v = bind_addr_from_env();
        assert!(v.is_some());
        let s = v.unwrap();
        assert!(s.parse::<SocketAddr>().is_ok() || s == DEFAULT_ADDR);
    }
}
