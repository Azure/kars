//! Axum route handlers for the inference router.

use axum::{Router, routing::{get, post}, Json};
use std::sync::Arc;
use crate::auth::WorkloadIdentityAuth;
use crate::config::Config;

/// Shared application state.
#[derive(Clone)]
pub struct AppState {
    pub auth: Arc<WorkloadIdentityAuth>,
    pub config: Arc<Config>,
}

impl AppState {
    pub async fn new(config: &Config) -> anyhow::Result<Self> {
        Ok(Self {
            auth: Arc::new(WorkloadIdentityAuth::new()),
            config: Arc::new(Config::from_env()?),
        })
    }
}

/// Inference API routes — proxied to Azure OpenAI / AI Foundry.
pub fn inference_routes() -> Router<AppState> {
    Router::new()
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/completions", post(completions))
        .route("/v1/embeddings", post(embeddings))
}

/// Health and readiness routes.
pub fn health_routes() -> Router<AppState> {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
}

/// Prometheus metrics endpoint.
pub fn metrics_routes() -> Router<AppState> {
    Router::new()
        .route("/metrics", get(metrics))
}

async fn chat_completions() -> Json<serde_json::Value> {
    // TODO: proxy to Azure OpenAI with auth + content safety + token tracking
    Json(serde_json::json!({
        "status": "not_implemented",
        "message": "Inference routing not yet wired — scaffold only"
    }))
}

async fn completions() -> Json<serde_json::Value> {
    Json(serde_json::json!({"status": "not_implemented"}))
}

async fn embeddings() -> Json<serde_json::Value> {
    Json(serde_json::json!({"status": "not_implemented"}))
}

async fn healthz() -> &'static str {
    "ok"
}

async fn readyz() -> &'static str {
    // TODO: check upstream Azure OpenAI connectivity
    "ok"
}

async fn metrics() -> String {
    // TODO: export Prometheus metrics (token usage, latency, etc.)
    String::new()
}
