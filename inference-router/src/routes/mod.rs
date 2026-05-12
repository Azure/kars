// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Axum route handlers for the inference router.

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::get,
};
use bytes::Bytes;
use std::sync::Arc;

use crate::auth::WorkloadIdentityAuth;
use crate::blocklist::Blocklist;
use crate::budget::TokenBudgetTracker;
use crate::config::Config;
use crate::copilot_auth::CopilotTokenCache;
use crate::egress_blocked::BlockedBuffer;
use crate::governance::Governance;
use crate::handoff::{DrainState, HandoffSession, HandoffTokenStore, PendingHandoffStore};
use crate::mesh::{MeshInbox, MeshMetrics};
use crate::policy_status::PolicyStatusRegistry;
use crate::providers::{AuditSink, PolicyDecisionProvider, SigningProvider};
use crate::proxy::UpstreamConfig;

mod handoff;
pub use handoff::handoff_init_routes;
pub use handoff::handoff_protected_routes;
pub use handoff::handoff_status_routes;
pub use handoff::spawn_routes;

pub(crate) mod audit_events;
pub(crate) mod inference_policy;
pub(crate) mod inference_translate;
pub(crate) mod signing_ops;
pub(crate) mod spawn_policy;

mod governance;
pub use governance::sensitive_agt_routes;

mod mesh;
pub use mesh::mesh_routes;

mod egress;
pub use egress::egress_routes;

mod internal;
pub use internal::internal_routes;

mod inference;
pub use inference::{foundry_agent_routes, foundry_standalone_routes, inference_routes};

mod anthropic_messages;
mod chat_completions;

mod mcp;
pub use mcp::{
    MCP_SESSION_HEADER, McpRouteState, mcp_route, platform_mcp_route, protected_mcp_route,
};

mod a2a;
pub use a2a::{A2aRouteState, a2a_routes};

/// Shared application state.
#[derive(Clone)]
pub struct AppState {
    pub auth: Arc<WorkloadIdentityAuth>,
    /// GitHub Copilot token cache. Constructed unconditionally (cheap, lazy)
    /// — `proxy::forward()` only consults it when the upstream endpoint is
    /// `api.githubcopilot.com`. When no GH token is configured and the
    /// Copilot path is hit, the proxy returns a clean 502 instead of panicking.
    pub copilot: Arc<CopilotTokenCache>,
    pub client: reqwest::Client,
    pub config: Arc<Config>,
    pub budget: TokenBudgetTracker,
    pub governance: Arc<Governance>,
    /// Four-seam policy contract view of `governance`. Today it's the same
    /// `Arc<Governance>` coerced to `Arc<dyn PolicyDecisionProvider>` — the
    /// trait is implemented directly on `Governance`. Per-tenant swap to
    /// `Arc<AgtPolicyDecisionProvider>` is the next Phase 1 branch.
    pub policy_provider: Arc<dyn PolicyDecisionProvider>,
    /// Four-seam audit contract view of `governance`. Same `Arc<Governance>`
    /// coerced to `Arc<dyn AuditSink>`; the trait impl lives in
    /// `providers/audit_impl.rs` and adds an in-process dedup cache on top
    /// of the non-idempotent upstream `AuditLogger::log`.
    pub audit_sink: Arc<dyn AuditSink>,
    /// Four-seam signing contract view of `governance`. Same `Arc<Governance>`
    /// coerced to `Arc<dyn SigningProvider>`; the trait impl lives in
    /// `providers/signing_impl.rs` and delegates to the agent's Ed25519
    /// keypair owned by `Governance.identity`.
    pub signing_provider: Arc<dyn SigningProvider>,
    pub blocklist: Blocklist,
    /// S12.f — bounded ring buffer of blocked egress attempts surfaced via
    /// `GET /egress/learned/blocked`. Hostname-only, deduped, rate-limited
    /// per source. Populated by the forward proxy's deny branches.
    pub blocked_egress: Arc<BlockedBuffer>,
    pub sandbox_name: Arc<String>,
    pub inbox: Arc<MeshInbox>,
    pub mesh_metrics: Arc<MeshMetrics>,
    /// Live model override (set via /admin/model). Takes priority over config.default_model.
    pub model_override: Arc<std::sync::RwLock<Option<String>>>,
    /// Admin token for sensitive mutations (trust updates). None = no auth required.
    pub admin_token: Option<Arc<String>>,
    /// Models that don't support chat/completions (need Responses API).
    /// Populated on first 400 "unsupported" — avoids redundant round-trips.
    pub responses_only_models: Arc<std::sync::RwLock<std::collections::HashSet<String>>>,
    /// Handoff token store (in-memory, TTL-based, one-at-a-time).
    pub handoff_tokens: HandoffTokenStore,
    /// Handoff session tracker (phase, direction, progress).
    pub handoff_session: HandoffSession,
    /// Drain state (stops new work during handoff).
    pub drain_state: DrainState,
    /// Pending handoff confirmation store (§9.9.9 two-stage gate).
    pub pending_handoff: PendingHandoffStore,
    /// Per-CRD policy load status — populated by every consumer that
    /// materializes a controller-published artifact into the router's
    /// memory. Read by `GET /internal/policy-status`, which the
    /// controller polls to confirm `Compiled → Ready` transitions
    /// (Slice 1 of `crd-well-oiled-machine`).
    pub policy_status: Arc<PolicyStatusRegistry>,
}

impl AppState {
    pub async fn new(_config: &Config) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .pool_max_idle_per_host(32)
            .redirect(reqwest::redirect::Policy::none()) // Never follow redirects — return 3xx as-is
            .build()?;

        let config = Config::from_env()?;
        let budget =
            TokenBudgetTracker::new(config.token_budget_daily, config.token_budget_per_request);

        let sandbox_name = std::env::var("SANDBOX_NAME").unwrap_or_else(|_| "unknown".into());

        // Shared per-CRD policy load status registry. Constructed once
        // and handed to both the AGT engine (which writes
        // `PolicyKind::AgtProfile` on every successful reload) and
        // `AppState` (read by `GET /internal/policy-status`).
        let policy_status = Arc::new(PolicyStatusRegistry::new());

        // Initialize native AGT governance
        let governance = Arc::new(Governance::new_with_status(
            &sandbox_name,
            policy_status.clone(),
        ));

        // Load policy YAML from AGT_POLICY_DIR if set
        let policy_dir = std::env::var("AGT_POLICY_DIR").ok();
        if let Some(ref dir) = policy_dir {
            match governance.load_policies_from_dir(dir) {
                Ok(count) => tracing::info!(dir, count, "AGT governance: loaded policy rules"),
                Err(e) => {
                    tracing::warn!(dir, error = %e, "AGT governance: failed to load policies")
                }
            }
        }

        // Initialize blocklist — enabled via BLOCKLIST_ENABLED=true
        let blocklist_enabled = std::env::var("BLOCKLIST_ENABLED")
            .unwrap_or_else(|_| "true".into())
            .parse::<bool>()
            .unwrap_or(true);

        let blocklist = if blocklist_enabled {
            let seed_path = std::env::var("BLOCKLIST_SEED_PATH")
                .unwrap_or_else(|_| "/etc/azureclaw/blocklist/domains.txt".into());
            let bl = Blocklist::new(Some(&seed_path)).await;

            let refresh_secs = std::env::var("BLOCKLIST_REFRESH_SECS")
                .ok()
                .and_then(|s| s.parse().ok());

            bl.start_refresh_task(client.clone(), refresh_secs, Some(seed_path));
            tracing::info!("Blocklist enabled — auto-refresh active");
            bl
        } else {
            tracing::info!("Blocklist disabled");
            Blocklist::disabled()
        };

        // Learn mode: observe all egress domains (blocklist still enforced)
        let learn_mode = std::env::var("EGRESS_LEARN_MODE")
            .unwrap_or_else(|_| "false".into())
            .parse::<bool>()
            .unwrap_or(false);
        if learn_mode {
            blocklist.set_learn_mode(true);
        }

        Ok(Self {
            auth: Arc::new(WorkloadIdentityAuth::new()),
            copilot: Arc::new(CopilotTokenCache::from_env()),
            client: client.clone(),
            config: Arc::new(config),
            budget,
            policy_provider: Arc::clone(&governance) as Arc<dyn PolicyDecisionProvider>,
            audit_sink: Arc::clone(&governance) as Arc<dyn AuditSink>,
            signing_provider: Arc::clone(&governance) as Arc<dyn SigningProvider>,
            governance,
            blocklist,
            blocked_egress: Arc::new(BlockedBuffer::with_defaults()),
            sandbox_name: Arc::new(sandbox_name),
            inbox: Arc::new(MeshInbox::new()),
            mesh_metrics: Arc::new(MeshMetrics::new()),
            model_override: Arc::new(std::sync::RwLock::new(None)),
            responses_only_models: Arc::new(std::sync::RwLock::new(
                std::collections::HashSet::new(),
            )),
            admin_token: std::fs::read_to_string("/etc/azureclaw/secrets/admin-token")
                .or_else(|_| std::fs::read_to_string("/run/secrets/admin-token"))
                .or_else(|_| std::env::var("ADMIN_TOKEN"))
                .ok()
                .filter(|s| !s.is_empty())
                .map(|s| Arc::new(s.trim().to_string())),
            handoff_tokens: HandoffTokenStore::new(),
            handoff_session: HandoffSession::new(),
            drain_state: DrainState::new(),
            pending_handoff: PendingHandoffStore::new(),
            policy_status,
        })
    }

    fn upstream_config(&self, sandbox_name: &str) -> UpstreamConfig {
        // For inference (chat completions, embeddings): prefer the dedicated OpenAI endpoint
        // (openai.azure.com) over the Foundry project endpoint (services.ai.azure.com).
        // Foundry project endpoint is used for agent/memory/knowledge APIs, not inference.
        let endpoint = self
            .config
            .azure_openai_endpoint
            .clone()
            .or_else(|| self.config.foundry_endpoint.clone())
            .unwrap_or_default();

        // Live model override takes priority over config default
        let deployment = self
            .model_override
            .read()
            .ok()
            .and_then(|g| g.clone())
            .unwrap_or_else(|| self.config.default_model.clone());

        UpstreamConfig {
            endpoint,
            deployment,
            sandbox_name: sandbox_name.to_string(),
        }
    }
}

/// Extract the admin bearer token from either `Authorization: Bearer <token>`
/// (canonical) or the legacy `x-azureclaw-admin: <token>` header.
///
/// Q3: `Authorization: Bearer` is canonical; `x-azureclaw-admin` is accepted
/// for backward compatibility but logs a deprecation warning (once per
/// process — see `DEPRECATED_ADMIN_HEADER_WARNED`) so operators notice and
/// migrate callers. The legacy header will be removed in a future release.
fn extract_admin_token(headers: &HeaderMap) -> Option<String> {
    if let Some(value) = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    {
        return Some(value.to_string());
    }
    if let Some(value) = headers
        .get("x-azureclaw-admin")
        .and_then(|v| v.to_str().ok())
    {
        if !DEPRECATED_ADMIN_HEADER_WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            tracing::warn!(
                "Deprecated header 'x-azureclaw-admin' used for admin auth. \
                 Migrate to 'Authorization: Bearer <token>'. This header will \
                 be removed in a future release."
            );
        }
        return Some(value.to_string());
    }
    None
}

static DEPRECATED_ADMIN_HEADER_WARNED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Health and readiness routes.
pub fn health_routes() -> Router<AppState> {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
}

/// Prometheus metrics endpoint.
pub fn metrics_routes() -> Router<AppState> {
    Router::new().route("/metrics", get(metrics))
}

/// Admin routes — live configuration (localhost only, for dev mode model switching).
pub fn admin_routes() -> Router<AppState> {
    Router::new().route("/admin/model", get(admin_get_model).put(admin_set_model))
}

async fn healthz() -> &'static str {
    "ok"
}

async fn readyz(State(state): State<AppState>) -> impl IntoResponse {
    // Check that we can acquire a token (validates Workload Identity / IMDS setup)
    let audience = if state
        .config
        .foundry_endpoint
        .as_deref()
        .is_some_and(|ep| ep.contains("services.ai.azure.com") && ep.contains("/api/projects/"))
    {
        "https://ai.azure.com"
    } else {
        "https://cognitiveservices.azure.com"
    };
    match state.auth.get_token(audience).await {
        Ok(_) => {
            // Also check Content Safety endpoint if configured
            if let Some(ref cs_endpoint) = state.config.content_safety_endpoint
                && state.config.content_safety_enabled
            {
                let url = format!(
                    "{}/contentsafety/text:analyze?api-version=2024-09-01",
                    cs_endpoint.trim_end_matches('/')
                );
                let reachable = state
                    .client
                    .post(&url)
                    .timeout(std::time::Duration::from_secs(3))
                    .send()
                    .await
                    .is_ok();
                if !reachable {
                    tracing::warn!("Content Safety endpoint unreachable: {cs_endpoint}");
                    return (
                        StatusCode::OK,
                        "ok (content safety unreachable — failing open)",
                    )
                        .into_response();
                }
            }
            (StatusCode::OK, "ok").into_response()
        }
        Err(e) => {
            tracing::warn!("Readiness check failed: {e}");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "not ready — token acquisition failed",
            )
                .into_response()
        }
    }
}

async fn metrics() -> String {
    use prometheus::Encoder;
    let encoder = prometheus::TextEncoder::new();
    let metric_families = prometheus::gather();
    let mut buffer = Vec::new();
    encoder.encode(&metric_families, &mut buffer).unwrap();
    String::from_utf8(buffer).unwrap_or_default()
}

/// GET /admin/model — show current model
async fn admin_get_model(State(state): State<AppState>) -> impl IntoResponse {
    let current = state
        .model_override
        .read()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or_else(|| state.config.default_model.clone());
    Json(serde_json::json!({ "model": current, "default": state.config.default_model }))
}

/// PUT /admin/model — switch model live (body: {"model": "gpt-5-mini"})
async fn admin_set_model(State(state): State<AppState>, body: Bytes) -> impl IntoResponse {
    let model = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("model")?.as_str().map(String::from));

    match model {
        Some(m) => {
            let prev = state
                .model_override
                .read()
                .ok()
                .and_then(|g| g.clone())
                .unwrap_or_else(|| state.config.default_model.clone());
            if let Ok(mut guard) = state.model_override.write() {
                *guard = Some(m.clone());
            }
            tracing::info!(from = %prev, to = %m, "Model switched via /admin/model");
            Json(serde_json::json!({ "model": m, "previous": prev }))
        }
        None => Json(serde_json::json!({ "error": "body must contain {\"model\": \"<name>\"}" })),
    }
}
