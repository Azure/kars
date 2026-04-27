//! AzureClaw Inference Router
#![allow(
    clippy::collapsible_if,
    clippy::redundant_guards,
    clippy::needless_borrows_for_generic_args,
    clippy::match_like_matches_macro,
    clippy::await_holding_lock,
    clippy::unnecessary_unwrap
)]
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

use azureclaw_inference_router::{config, forward_proxy, governance, handoff, routes};

use anyhow::Result;
use axum::{
    Router,
    extract::Request,
    http::{HeaderName, HeaderValue, StatusCode},
    middleware::Next,
    response::IntoResponse,
};
use std::sync::Arc;
use tracing::Instrument;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// Header used for end-to-end correlation across CLI, router, and upstream
/// logs. Accepted on inbound (propagated when present), otherwise generated.
/// Also emitted on every response so clients can correlate their side.
const TRACE_ID_HEADER: &str = "x-azureclaw-trace-id";

/// Maximum accepted length for an incoming trace id. Longer values are
/// rejected — we don't want a caller burying 2MB into every log line.
const MAX_TRACE_ID_LEN: usize = 128;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "azureclaw_inference_router=info,tower_http=info".into()),
        )
        .with(tracing_subscriber::fmt::layer().json())
        .init();

    tracing::info!("AzureClaw Inference Router starting");

    let config = config::Config::from_env()?;

    // Log registry mode — informs whether handoff is available.
    tracing::info!(
        registry_mode = %config.registry_mode,
        registry_url = config.registry_url.as_deref().unwrap_or("<unset>"),
        "Registry topology"
    );

    // In global mode, verify registry connectivity at startup.
    if config.registry_mode == config::RegistryMode::Global {
        let url = config.registry_url.as_deref().unwrap_or_default();
        if url.is_empty() {
            anyhow::bail!("AGT_REGISTRY_MODE=global requires AGT_REGISTRY_URL to be set");
        }
        let health_url = format!("{}/v1/health", url.trim_end_matches('/'));
        tracing::info!(url = %health_url, "Checking global registry connectivity...");
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()?;
        match client.get(&health_url).send().await {
            Ok(resp) if resp.status().is_success() => {
                tracing::info!("Global registry is reachable");
            }
            Ok(resp) => {
                tracing::warn!(
                    status = %resp.status(),
                    url = %health_url,
                    "Global registry health check returned non-200 — registry may not be ready"
                );
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    url = %health_url,
                    "Global registry health check failed — registry may not be reachable. \
                     The router will start anyway and retry on first use."
                );
            }
        }
    }

    let state = routes::AppState::new(&config).await?;

    // Start policy hot-reload watcher (polls AGT_POLICY_DIR for mtime changes).
    governance::Governance::spawn_policy_watcher(state.governance.clone());

    // Clone blocklist for the forward proxy before state is moved into the router.
    let proxy_blocklist = state.blocklist.clone();

    // Read admin token for protecting sensitive endpoints.
    // Priority: file mount (AKS Secret) > env var > unset.
    // If set, /admin/*, /egress/*, /sandbox/*, and sensitive /agt/* endpoints
    // require Authorization: Bearer <token>. If unset, these endpoints are unrestricted
    // (backwards-compatible for local dev).
    let admin_token: Option<Arc<String>> =
        std::fs::read_to_string("/etc/azureclaw/secrets/admin-token")
            .ok()
            .or_else(|| std::fs::read_to_string("/run/secrets/admin-token").ok())
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .or_else(|| std::env::var("ADMIN_TOKEN").ok().filter(|t| !t.is_empty()))
            .or_else(|| {
                // Auto-generate a random admin token when none is configured.
                // This ensures admin endpoints are always protected.
                let mut buf = [0u8; 32];
                if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
                    use std::io::Read;
                    if f.read_exact(&mut buf).is_ok() {
                        let token: String = buf.iter().map(|b| format!("{:02x}", b)).collect();
                        tracing::warn!(
                            "ADMIN_TOKEN not configured — auto-generated a random token. \
                             Set ADMIN_TOKEN explicitly in production."
                        );
                        return Some(token);
                    }
                }
                tracing::error!("Failed to generate random admin token from /dev/urandom");
                None
            })
            .map(Arc::new);

    // s3 — optional origin allowlist for the admin API. Comma-separated
    // IPv4/IPv6 literals. Empty/unset = token-only legacy behaviour.
    let admin_allow_ips: Arc<Vec<std::net::IpAddr>> = Arc::new(
        std::env::var("ROUTER_ADMIN_ALLOW_IPS")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|s| {
                let mut ips = Vec::new();
                for entry in s.split(',').map(|e| e.trim()).filter(|e| !e.is_empty()) {
                    match entry.parse::<std::net::IpAddr>() {
                        Ok(ip) => ips.push(ip),
                        Err(err) => {
                            tracing::error!(
                                entry,
                                %err,
                                "Invalid entry in ROUTER_ADMIN_ALLOW_IPS; skipping"
                            );
                        }
                    }
                }
                if !ips.is_empty() {
                    tracing::info!(count = ips.len(), "Admin-API origin allowlist active (s3)");
                }
                ips
            })
            .unwrap_or_default(),
    );

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
            let allow_ips = admin_allow_ips.clone();
            protected.layer(axum::middleware::from_fn(move |req, next| {
                let token = token.clone();
                let allow_ips = allow_ips.clone();
                admin_auth_middleware(token, allow_ips, req, next)
            }))
        } else {
            // Unreachable — auto-generated token above guarantees Some
            tracing::error!("ADMIN_TOKEN is None — this should be unreachable");
            protected
        };

        // Handoff routes — three auth tiers, all stricter than admin_auth_middleware:
        //
        // 1. handoff/init: admin token only, NO localhost bypass
        // 2. handoff/* mutations: admin token + handoff token, NO localhost bypass
        // 3. handoff/status: admin token, localhost allowed (read-only)
        let handoff_init =
            routes::handoff_init_routes().layer(axum::middleware::from_fn_with_state(
                state.clone(),
                handoff::handoff_init_auth_middleware,
            ));

        let handoff_mutations = routes::handoff_protected_routes().layer(
            axum::middleware::from_fn_with_state(state.clone(), handoff::handoff_auth_middleware),
        );

        let handoff_status =
            routes::handoff_status_routes().layer(axum::middleware::from_fn_with_state(
                state.clone(),
                handoff::handoff_status_auth_middleware,
            ));

        public
            .merge(protected)
            .merge(handoff_init)
            .merge(handoff_mutations)
            .merge(handoff_status)
            .with_state(state)
            .merge(build_mcp_router())
            .layer(axum::middleware::from_fn(connection_close_middleware))
            .layer(tower::limit::ConcurrencyLimitLayer::new(
                std::env::var("ROUTER_CONCURRENCY_LIMIT")
                    .ok()
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(256),
            ))
            // r6 — trace-id middleware is outermost so every request gets a
            // trace span before any other layer runs (concurrency limit,
            // connection_close, auth gates all log inside the span).
            .layer(axum::middleware::from_fn(trace_id_middleware))
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
    let proxy_shutdown = forward_proxy::start(&proxy_addr, proxy_blocklist).await;

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    let shutdown_timeout = resolve_shutdown_timeout();
    tracing::info!(
        shutdown_timeout_secs = shutdown_timeout.as_secs(),
        "graceful shutdown timeout configured"
    );

    // Observe when the shutdown signal actually fires — the drain-deadline
    // timer MUST start from that moment, not from process startup.
    //
    // History: an earlier implementation wrapped the entire `serve_fut` in
    // `tokio::time::timeout(shutdown_timeout, ...)`. Since serve_fut only
    // completes *after* the shutdown signal fires, the timer raced the whole
    // server lifetime and force-killed the router ~25s after startup — no
    // signal required. The `bounded_drain_stays_alive_*` regression tests
    // guard against re-introducing that pattern.
    let (signal_fired_tx, signal_fired_rx) = tokio::sync::oneshot::channel::<()>();
    let shutdown_fut = async move {
        shutdown_signal().await;
        let _ = signal_fired_tx.send(());
    };

    let serve_fut = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_fut)
    .into_future();
    tokio::pin!(serve_fut);

    // Race the server exiting on its own (fatal bind/IO error) vs. the
    // shutdown signal firing. Only AFTER the signal fires do we apply
    // `shutdown_timeout` to the drain phase.
    tokio::select! {
        res = &mut serve_fut => res?,
        _ = signal_fired_rx => {
            match tokio::time::timeout(shutdown_timeout, serve_fut).await {
                Ok(res) => res?,
                Err(_) => tracing::warn!(
                    shutdown_timeout_secs = shutdown_timeout.as_secs(),
                    "graceful shutdown timed out with in-flight requests; forcing shutdown"
                ),
            }
        }
    }

    // Signal the forward proxy to drain and shut down
    proxy_shutdown.cancel();
    // Give it a moment to drain active tunnels
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    tracing::info!("Inference router shut down gracefully");
    Ok(())
}

/// Build the `/mcp` axum sub-router.
///
/// Selection rule (Phase 2 §8 entry 1, S1):
///
/// - When all three env vars are set:
///   - `MCP_PRODUCTION_MODE=true`
///   - `MCP_JWKS_PATH` (path to a file containing a raw RFC 7517 JWKSet)
///   - `MCP_OAUTH_AUDIENCE` (expected `aud` claim)
///
///   then mount [`routes::protected_mcp_route`] gated by OAuth 2.1.
///
///   Optional: `MCP_OAUTH_ISSUER` (defaults to a placeholder if unset
///   — controllers always set it) and `MCP_OAUTH_REQUIRED_SCOPES`
///   (space-separated).
///
/// - Otherwise mount the bare [`routes::mcp_route`] for dev/test.
///   Admission CEL on the `McpServer` CRD plus the
///   `admission-dev-only-label-immutable` policy block any tenant from
///   pointing a `productionMode=false` `McpServer` at this dev mount in
///   a non-dev namespace.
///
/// On a malformed production-mode configuration (e.g. JWKS path is
/// unreadable) the router refuses to mount `/mcp` rather than silently
/// falling back to the unauthenticated dev route. Operators see a clear
/// startup-time error instead of a route that quietly serves
/// unauthenticated MCP traffic.
fn build_mcp_router() -> Router {
    use azureclaw_inference_router::mcp::oauth::OAuthVerifierConfig;

    let production = std::env::var("MCP_PRODUCTION_MODE")
        .map(|v| matches!(v.as_str(), "true" | "1" | "yes"))
        .unwrap_or(false);
    let jwks_path = std::env::var("MCP_JWKS_PATH").ok();
    let audience = std::env::var("MCP_OAUTH_AUDIENCE").ok();

    let state = routes::McpRouteState::standard();

    if production && jwks_path.is_some() && audience.is_some() {
        let path = std::path::PathBuf::from(jwks_path.unwrap());
        let aud = audience.unwrap();
        let issuer = std::env::var("MCP_OAUTH_ISSUER").unwrap_or_default();
        let required_scopes = std::env::var("MCP_OAUTH_REQUIRED_SCOPES")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.split_whitespace().map(|t| t.to_string()).collect())
            .unwrap_or_default();
        match OAuthVerifierConfig::from_jwks_file(&path, &issuer, &aud, required_scopes) {
            Ok(cfg) => {
                tracing::info!(
                    jwks = %path.display(),
                    issuer = %issuer,
                    audience = %aud,
                    "Mounting /mcp with OAuth 2.1 verification (productionMode)"
                );
                return routes::protected_mcp_route(state, Arc::new(cfg));
            }
            Err(e) => {
                tracing::error!(error = %e, "MCP_PRODUCTION_MODE=true but JWKS load failed; refusing to mount /mcp");
                return Router::new();
            }
        }
    }

    if production {
        tracing::warn!(
            "MCP_PRODUCTION_MODE=true but MCP_JWKS_PATH or MCP_OAUTH_AUDIENCE missing; \
             refusing to mount /mcp (would otherwise be unauthenticated)"
        );
        return Router::new();
    }

    tracing::info!("Mounting /mcp in dev mode (no OAuth — productionMode=false)");
    routes::mcp_route().with_state(state)
}

fn resolve_shutdown_timeout() -> std::time::Duration {
    const FLOOR_SECS: u64 = 10;
    const DEFAULT_SECS: u64 = 25;
    if let Some(v) = std::env::var("SHUTDOWN_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|s| *s > 0)
    {
        return std::time::Duration::from_secs(v);
    }
    if let Some(grace) = std::env::var("TERMINATION_GRACE_PERIOD_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
    {
        let derived = grace.saturating_sub(5).max(FLOOR_SECS);
        return std::time::Duration::from_secs(derived);
    }
    std::time::Duration::from_secs(DEFAULT_SECS)
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            tracing::error!(
                error = %err,
                "failed to install Ctrl+C handler; process will not shut down cleanly on SIGINT"
            );
            // Park this branch so the SIGTERM arm can still drive shutdown.
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(err) => {
                tracing::error!(
                    error = %err,
                    "failed to install SIGTERM handler; process will not shut down cleanly on SIGTERM"
                );
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => tracing::info!("Received SIGINT, shutting down"),
        _ = terminate => tracing::info!("Received SIGTERM, shutting down"),
    }
}

/// Prevent HTTP/1.1 keep-alive connection accumulation (Envoy-style
/// `max_requests_per_connection: 1`).  On a localhost router the overhead
/// of a fresh TCP handshake is ~100 µs — negligible vs. the risk of FD
/// exhaustion that stalls the Telegram CONNECT proxy sharing this process.
/// WebSocket upgrades are excluded so mesh relay connections work normally.
async fn connection_close_middleware(req: Request, next: Next) -> impl IntoResponse {
    let mut response = next.run(req).await;
    if response.status() != StatusCode::SWITCHING_PROTOCOLS {
        response.headers_mut().insert(
            axum::http::header::CONNECTION,
            axum::http::HeaderValue::from_static("close"),
        );
    }
    response
}

/// Middleware that gates protected endpoints for non-localhost callers.
///
/// Three-layer gate:
/// 1. Localhost (127.0.0.1 / ::1) → always allowed (agent + kubectl exec are same-pod).
/// 2. Non-localhost → if `ROUTER_ADMIN_ALLOW_IPS` is set, the remote IP **must**
///    appear in the comma-separated list. Empty/unset = legacy behaviour (any
///    IP accepted with the right token).
/// 3. Non-localhost → requires `Authorization: Bearer <ADMIN_TOKEN>`.
///
/// Layer 2 is the s3 hardening: token leak alone is not enough to pwn the
/// admin API; attacker must also have a pod at an allowlisted address.
async fn admin_auth_middleware(
    expected_token: Arc<String>,
    allow_ips: Arc<Vec<std::net::IpAddr>>,
    req: Request,
    next: Next,
) -> impl IntoResponse {
    // Allow all localhost connections without auth — same pod is trusted.
    let remote_ip = req
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|c| c.0.ip());

    if let Some(ip) = remote_ip
        && ip.is_loopback()
    {
        return next.run(req).await.into_response();
    }

    // s3 — origin allowlist (defence in depth against admin-token theft).
    if !allow_ips.is_empty() {
        match remote_ip {
            Some(ip) if allow_ips.contains(&ip) => {}
            Some(ip) => {
                tracing::warn!(
                    path = %req.uri().path(),
                    remote = %ip,
                    "Admin auth: remote IP not in ROUTER_ADMIN_ALLOW_IPS"
                );
                return (StatusCode::FORBIDDEN, "Admin origin not allowed").into_response();
            }
            None => {
                tracing::warn!(
                    path = %req.uri().path(),
                    "Admin auth: missing ConnectInfo while allowlist is set"
                );
                return (StatusCode::FORBIDDEN, "Admin origin not allowed").into_response();
            }
        }
    }

    // Non-localhost: require bearer token
    let auth_header = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok());

    match auth_header {
        Some(value) if value.starts_with("Bearer ") => {
            let provided = &value[7..];
            if handoff::constant_time_eq(provided.as_bytes(), expected_token.as_bytes()) {
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
            (
                StatusCode::UNAUTHORIZED,
                "Admin token required for non-localhost access",
            )
                .into_response()
        }
    }
}

// ── Trace-id middleware (r6) ────────────────────────────────────────────────
//
// Attaches a stable correlation id to every request so logs emitted from
// the CLI, router, and upstream Azure services can be joined. Accepts an
// incoming `X-Azureclaw-Trace-Id` verbatim if the caller supplied one (so
// CLI→router correlation works), otherwise generates 64 random bits as
// 16 hex characters.
//
// The id is:
//   • placed in request extensions (for handlers that want it directly)
//   • inserted back into request headers (so proxy::forward's generic
//     header-copy picks it up and forwards to Azure upstream)
//   • used to open a tracing span so *every* log event from this request
//     inherits the `trace_id` field in the JSON logs
//   • echoed back in the response as `X-Azureclaw-Trace-Id`
async fn trace_id_middleware(mut req: Request, next: Next) -> impl IntoResponse {
    let trace_id = req
        .headers()
        .get(TRACE_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty() && s.len() <= MAX_TRACE_ID_LEN && is_safe_trace_id(s))
        .map(|s| s.to_owned())
        .unwrap_or_else(generate_trace_id);

    // Normalise back into the request so forward() will propagate it.
    if let Ok(hv) = HeaderValue::from_str(&trace_id) {
        req.headers_mut()
            .insert(HeaderName::from_static(TRACE_ID_HEADER), hv.clone());
    }
    req.extensions_mut().insert(TraceId(trace_id.clone()));

    let span = tracing::info_span!("request", trace_id = %trace_id);
    let mut response = next.run(req).instrument(span).await.into_response();

    if let Ok(hv) = HeaderValue::from_str(&trace_id) {
        response
            .headers_mut()
            .insert(HeaderName::from_static(TRACE_ID_HEADER), hv);
    }
    response
}

/// Request-extension wrapper for the trace id. Handlers can pull this out
/// with `.extensions().get::<TraceId>()` if they need to log with it
/// directly (most don't — the tracing span handles it automatically).
#[derive(Clone, Debug)]
pub struct TraceId(pub String);

/// Tight charset for trace ids to prevent log-injection (CRLF, tabs, ANSI
/// escapes) when a caller passes their own id. Allowed: ASCII alphanumeric,
/// `-`, `_`. Anything else → regenerate.
fn is_safe_trace_id(s: &str) -> bool {
    s.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Generate a 64-bit trace id as 16 hex characters. Collision-resistant
/// for the volumes we care about (log correlation within a short window),
/// without pulling in a full UUID dependency.
fn generate_trace_id() -> String {
    use rand::Rng;
    format!("{:016x}", rand::rng().random::<u64>())
}

// ── Tests (r6, s3) ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        body::Body,
        extract::ConnectInfo,
        http::{Request, StatusCode},
        routing::get,
    };
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use tower::ServiceExt;

    async fn ok() -> &'static str {
        "ok"
    }

    fn build_app(token: Arc<String>, allow_ips: Arc<Vec<IpAddr>>) -> Router {
        let token_for_closure = token.clone();
        Router::new()
            .route("/admin/ping", get(ok))
            .layer(axum::middleware::from_fn(move |req, next| {
                let token = token_for_closure.clone();
                let allow_ips = allow_ips.clone();
                admin_auth_middleware(token, allow_ips, req, next)
            }))
            .layer(axum::middleware::from_fn(trace_id_middleware))
    }

    fn req_from(ip: IpAddr, auth: Option<&str>, trace: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder().uri("/admin/ping");
        if let Some(t) = auth {
            builder = builder.header("authorization", format!("Bearer {t}"));
        }
        if let Some(t) = trace {
            builder = builder.header(TRACE_ID_HEADER, t);
        }
        let mut req = builder.body(Body::empty()).unwrap();
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::new(ip, 49_000)));
        req
    }

    // ── trace-id sanitizer (r6) ──────────────────────────────────────────────

    #[test]
    fn is_safe_trace_id_accepts_hex() {
        assert!(is_safe_trace_id("9f3a2b1883c04e70"));
    }

    #[test]
    fn is_safe_trace_id_accepts_caller_supplied() {
        assert!(is_safe_trace_id("my-debug-run_42"));
    }

    #[test]
    fn is_safe_trace_id_rejects_log_injection() {
        assert!(!is_safe_trace_id("abc\n\"evil\":\"true")); // CRLF + JSON
        assert!(!is_safe_trace_id("abc\r\nLog-Forge"));
        assert!(!is_safe_trace_id("abc\tinject"));
        assert!(!is_safe_trace_id("abc\x1b[31mred")); // ANSI escape
        assert!(!is_safe_trace_id("abc/../etc")); // path-ish
        assert!(!is_safe_trace_id("abc def")); // whitespace
    }

    #[test]
    fn generate_trace_id_is_16_hex_chars() {
        let id = generate_trace_id();
        assert_eq!(id.len(), 16);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn generate_trace_id_is_unique_across_calls() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1000 {
            seen.insert(generate_trace_id());
        }
        // 1000 64-bit ids: collision probability ≈ 2.7e-14
        assert_eq!(seen.len(), 1000);
    }

    // ── trace-id middleware behaviour (r6) ───────────────────────────────────

    #[tokio::test]
    async fn trace_id_generated_when_absent() {
        let app = build_app(Arc::new("ignored".into()), Arc::new(Vec::new()));
        let req = req_from(IpAddr::V4(Ipv4Addr::LOCALHOST), None, None);
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let hv = resp
            .headers()
            .get(TRACE_ID_HEADER)
            .expect("response carries trace-id");
        let id = hv.to_str().unwrap();
        assert_eq!(id.len(), 16);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[tokio::test]
    async fn trace_id_propagated_from_caller() {
        let app = build_app(Arc::new("ignored".into()), Arc::new(Vec::new()));
        let req = req_from(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            None,
            Some("my-debug-run-42"),
        );
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get(TRACE_ID_HEADER).unwrap(),
            "my-debug-run-42"
        );
    }

    #[tokio::test]
    async fn malicious_trace_id_is_regenerated() {
        let app = build_app(Arc::new("ignored".into()), Arc::new(Vec::new()));
        // The http crate already rejects CR/LF/NUL in header values at
        // request build time, so we exercise our sanitizer with a payload
        // that's a valid HTTP header *value* but still dangerous if echoed
        // verbatim into a log line or a file path: traversal + quote chars.
        let req = req_from(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            None,
            Some("../../../etc/passwd\"injected"),
        );
        let resp = app.oneshot(req).await.unwrap();
        // Request still succeeds (we silently regenerate) but response trace id
        // must NOT echo the attack payload — must be our 16-hex-char fresh id.
        assert_eq!(resp.status(), StatusCode::OK);
        let id = resp
            .headers()
            .get(TRACE_ID_HEADER)
            .and_then(|v| v.to_str().ok())
            .unwrap();
        assert!(!id.contains('/'));
        assert!(!id.contains('.'));
        assert!(!id.contains('"'));
        assert_eq!(id.len(), 16);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    // ── admin origin allowlist (s3) ──────────────────────────────────────────

    #[tokio::test]
    async fn localhost_bypasses_everything() {
        let app = build_app(
            Arc::new("secret".into()),
            Arc::new(vec![IpAddr::V4(Ipv4Addr::new(10, 200, 0, 5))]),
        );
        let req = req_from(IpAddr::V4(Ipv4Addr::LOCALHOST), None, None);
        let resp = app.oneshot(req).await.unwrap();
        // Localhost allowed without token, without allowlist match.
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn allowlist_off_token_gate_still_works() {
        let app = build_app(Arc::new("secret".into()), Arc::new(Vec::new()));
        let non_local = IpAddr::V4(Ipv4Addr::new(10, 200, 0, 99));

        // Good token → 200
        let resp = app
            .clone()
            .oneshot(req_from(non_local, Some("secret"), None))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // Bad token → 401
        let resp = app
            .clone()
            .oneshot(req_from(non_local, Some("wrong"), None))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn allowlist_on_wrong_ip_gets_403_even_with_token() {
        let allow = IpAddr::V4(Ipv4Addr::new(10, 200, 0, 5));
        let attacker = IpAddr::V4(Ipv4Addr::new(10, 200, 0, 99));
        let app = build_app(Arc::new("secret".into()), Arc::new(vec![allow]));

        // Attacker has a valid token but wrong origin → 403.
        let resp = app
            .oneshot(req_from(attacker, Some("secret"), None))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn allowlist_on_right_ip_wrong_token_still_rejected() {
        let allow = IpAddr::V4(Ipv4Addr::new(10, 200, 0, 5));
        let app = build_app(Arc::new("secret".into()), Arc::new(vec![allow]));

        let resp = app
            .oneshot(req_from(allow, Some("wrong"), None))
            .await
            .unwrap();
        // Origin passes but token wrong → 401 (not 403).
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn allowlist_on_right_ip_right_token_accepted() {
        let allow = IpAddr::V4(Ipv4Addr::new(10, 200, 0, 5));
        let app = build_app(Arc::new("secret".into()), Arc::new(vec![allow]));

        let resp = app
            .oneshot(req_from(allow, Some("secret"), None))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // ── bounded-drain regression (shutdown bug) ─────────────────────────────
    //
    // The bug: an earlier `tokio::time::timeout(drain, serve_fut).await`
    // applied the drain deadline to the entire server lifetime. The router
    // force-exited ~25s after startup regardless of traffic, with no
    // SIGINT/SIGTERM ever received.
    //
    // These tests replicate the exact pattern `main()` now uses, with a tiny
    // drain timeout (200ms). If the pattern regresses, the server-alive
    // assertions will fire within the test window.

    /// Drive a miniature axum server using the same bounded-drain pattern as
    /// `main()`. Returns (local_addr, serve_handle, signal_tx).
    /// Pushing `()` through `signal_tx` simulates SIGTERM.
    async fn spawn_mini_server_with_bounded_drain(
        drain_timeout: std::time::Duration,
    ) -> (
        std::net::SocketAddr,
        tokio::task::JoinHandle<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        use std::future::IntoFuture;
        let app: Router = Router::new().route("/healthz", get(ok));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Caller's "signal" — pushed manually by the test.
        let (signal_tx, signal_rx) = tokio::sync::oneshot::channel::<()>();

        // Internal "signal fired" observer wrapping the caller's signal.
        let (signal_fired_tx, signal_fired_rx) = tokio::sync::oneshot::channel::<()>();
        let shutdown_fut = async move {
            let _ = signal_rx.await;
            let _ = signal_fired_tx.send(());
        };

        let handle = tokio::spawn(async move {
            let serve_fut = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .with_graceful_shutdown(shutdown_fut)
            .into_future();
            tokio::pin!(serve_fut);

            tokio::select! {
                res = &mut serve_fut => { let _ = res; }
                _ = signal_fired_rx => {
                    let _ = tokio::time::timeout(drain_timeout, serve_fut).await;
                }
            }
        });

        (addr, handle, signal_tx)
    }

    /// Regression: the server MUST stay up for at least 3× the drain timeout
    /// when no shutdown signal is ever sent. The old buggy code would kill
    /// the server after exactly `drain_timeout`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bounded_drain_stays_alive_when_no_signal() {
        let drain = std::time::Duration::from_millis(200);
        let (addr, handle, _signal_tx) = spawn_mini_server_with_bounded_drain(drain).await;

        // Wait 3× the drain timeout — in the buggy version the server is dead
        // by now (at t=drain).
        tokio::time::sleep(drain * 3).await;

        // Server must still accept connections.
        let client = reqwest::Client::new();
        let resp = client
            .get(format!("http://{addr}/healthz"))
            .timeout(std::time::Duration::from_secs(1))
            .send()
            .await
            .expect("server should still accept connections after 3× drain_timeout");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);

        // Task must not have terminated.
        assert!(
            !handle.is_finished(),
            "serve task terminated without a signal"
        );
        handle.abort();
    }

    /// Regression: when the signal DOES fire, the server shuts down within
    /// roughly the drain window (here: 200ms + slack). This proves the drain
    /// timer is armed correctly after the signal.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bounded_drain_exits_after_signal() {
        let drain = std::time::Duration::from_millis(200);
        let (_addr, handle, signal_tx) = spawn_mini_server_with_bounded_drain(drain).await;

        // Let the server get into steady state.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!handle.is_finished(), "server exited before signal");

        // Fire the signal; task should complete within ~drain + slack.
        let _ = signal_tx.send(());
        let res = tokio::time::timeout(drain + std::time::Duration::from_secs(1), handle)
            .await
            .expect("server did not exit within drain window");
        res.expect("serve task panicked");
    }
}
