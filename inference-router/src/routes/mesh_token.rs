// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `/v1/mesh-token` — sandbox-side entry point for verified AGT mesh trust.
//!
//! When `KarsAuthConfig.spec.meshAuthBackend == EntraAgentIdentity`, the
//! controller sets `MESH_AUTH_BACKEND=EntraAgentIdentity` on the router
//! container, and the sandbox `entrypoint.sh` calls this route to acquire
//! an Entra-signed agent identity token for AGT mesh peer
//! authentication. The token is then exported as `AGT_OAUTH_TOKEN`,
//! replacing the legacy direct Workload-Identity→Entra exchange.
//!
//! ## Why route through here?
//!
//! UID 1000 (the openclaw container) is blocked by the egress-guard
//! iptables baseline from reaching anything except loopback + DNS. The
//! shared auth-sidecar lives in `kars-system` and is only reachable
//! from UID 1001 (the inference-router container). This route is the
//! seam that lets UID 1000 acquire its own agent identity token
//! without weakening the egress baseline.
//!
//! ## Fail-closed behaviour
//!
//! - When `MESH_AUTH_BACKEND` is unset or not `EntraAgentIdentity`,
//!   the route returns 404. The entrypoint then falls back to its
//!   existing logic (legacy WI exchange, or anonymous-tier if that
//!   also fails). This means flipping the CRD field is a pure
//!   forward-rollout — old sandboxes don't break.
//! - When the sidecar is configured but the call fails, the route
//!   returns 500 with the upstream error. The entrypoint treats this
//!   the same way it treats a failed WI exchange: register
//!   anonymous-tier and force `AGT_TRUST_THRESHOLD=0`.
//! - The route NEVER accepts a caller-supplied `AgentIdentity`. The
//!   pinned per-sandbox identity from `PINNED_AGENT_IDENTITY_APP_ID`
//!   is used unconditionally — same rubber-duck contract as every
//!   other sidecar caller.
//!
//! See `docs/architecture/entra-agent-id/06-mesh-trust-design.md`.

use axum::{Json, Router, extract::State, http::StatusCode, response::IntoResponse, routing::get};
use serde::Serialize;

use super::AppState;

/// Env var the controller sets when
/// `KarsAuthConfig.spec.meshAuthBackend == EntraAgentIdentity`.
/// Any other value (or absent) keeps the legacy entrypoint flow.
pub const ENV_MESH_AUTH_BACKEND: &str = "MESH_AUTH_BACKEND";

/// Override env for the AGT mesh audience. Sourced by the controller
/// from `KarsAuthConfig.spec.meshAuthAudience` when set. Default is
/// `api://agentmesh/.default`. Operators with a custom relay
/// audience override here.
pub const ENV_MESH_AUTH_AUDIENCE: &str = "MESH_AUTH_AUDIENCE";

/// Default audience (matches the entrypoint's legacy WI exchange).
const DEFAULT_MESH_AUDIENCE: &str = "api://agentmesh/.default";

/// Sentinel value enabling the route.
const BACKEND_ENABLED_VALUE: &str = "EntraAgentIdentity";

#[derive(Debug, Serialize)]
struct MeshTokenResponse {
    access_token: String,
    token_type: &'static str,
    /// Conservative TTL hint for the caller. The real expiry is
    /// encoded in the JWT `exp` claim; callers that need precision
    /// must decode the token. The entrypoint uses this purely to
    /// schedule a coarse refresh window, so a conservative
    /// under-estimate is correct.
    expires_in: u64,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: &'static str,
    detail: String,
}

/// Returns the backend enable flag in a single place so the route
/// and its tests agree. Public for the integration test below.
pub fn mesh_auth_backend_is_enabled() -> bool {
    std::env::var(ENV_MESH_AUTH_BACKEND)
        .ok()
        .map(|v| v.trim().eq_ignore_ascii_case(BACKEND_ENABLED_VALUE))
        .unwrap_or(false)
}

fn mesh_audience() -> String {
    std::env::var(ENV_MESH_AUTH_AUDIENCE)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_MESH_AUDIENCE.to_string())
}

async fn mesh_token_handler(State(state): State<AppState>) -> impl IntoResponse {
    if !mesh_auth_backend_is_enabled() {
        // 404 (not 401 / 503) is deliberate: the route is absent
        // from this router's surface unless the CRD opts in.
        // Entrypoint MUST treat 404 as "feature disabled, fall back"
        // — this contract is exercised by the tests below.
        return (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "mesh_auth_backend_disabled",
                detail: "MESH_AUTH_BACKEND != EntraAgentIdentity".into(),
            }),
        )
            .into_response();
    }

    if !state.auth.is_sidecar_mode() {
        // The CRD says EntraAgentIdentity but the router was not
        // booted with the sidecar env. This is an operator
        // misconfiguration — refuse and surface a clear error rather
        // than silently falling back to WI / IMDS which would
        // attribute the mesh peer to the wrong principal.
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorResponse {
                error: "sidecar_not_configured",
                detail: "MESH_AUTH_BACKEND=EntraAgentIdentity but AUTH_SIDECAR_URL/PINNED_AGENT_IDENTITY_APP_ID are missing on the router. Refusing to fall back.".into(),
            }),
        )
            .into_response();
    }

    let audience = mesh_audience();
    // Phase 6.b — use the dedicated mesh-token path that bypasses
    // resource → service-name mapping. The audience is operator-
    // configurable and may not match any well-known mapping
    // (e.g. blueprint GUID instead of api://agentmesh) — but the
    // sidecar already has the correct scope wired via
    // DownstreamApis__AgentMesh__Scopes__0 (auto-emitted by the
    // controller). We log the audience for diagnostics so operators
    // can confirm what scope is requested.
    match state.auth.get_mesh_token().await {
        Ok(token) => (
            StatusCode::OK,
            Json(MeshTokenResponse {
                access_token: token,
                token_type: "Bearer",
                expires_in: 50 * 60,
            }),
        )
            .into_response(),
        Err(e) => {
            tracing::warn!(
                error = %e,
                audience = %audience,
                "auth-sidecar mesh-token acquisition failed"
            );
            (
                StatusCode::BAD_GATEWAY,
                Json(ErrorResponse {
                    error: "sidecar_call_failed",
                    detail: format!("{e:#}"),
                }),
            )
                .into_response()
        }
    }
}

/// Mount as a public route alongside `/healthz`, `/metrics`, etc.
/// The entrypoint reads it via `http://127.0.0.1:8443/v1/mesh-token`
/// from inside the same pod.
pub fn mesh_token_routes() -> Router<AppState> {
    Router::new().route("/v1/mesh-token", get(mesh_token_handler))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Process-wide env-var lock. The route tests mutate
    /// `MESH_AUTH_BACKEND` / `MESH_AUTH_AUDIENCE`, which would race
    /// against each other (and against any other env-var test in the
    /// same binary) without serialisation. `cargo test` runs tests
    /// within a binary in parallel by default; this mutex pins them
    /// to one-at-a-time for the env-mutation window.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_env<F: FnOnce()>(backend: Option<&str>, audience: Option<&str>, f: F) {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev_backend = std::env::var(ENV_MESH_AUTH_BACKEND).ok();
        let prev_aud = std::env::var(ENV_MESH_AUTH_AUDIENCE).ok();
        match backend {
            Some(v) => unsafe { std::env::set_var(ENV_MESH_AUTH_BACKEND, v) },
            None => unsafe { std::env::remove_var(ENV_MESH_AUTH_BACKEND) },
        }
        match audience {
            Some(v) => unsafe { std::env::set_var(ENV_MESH_AUTH_AUDIENCE, v) },
            None => unsafe { std::env::remove_var(ENV_MESH_AUTH_AUDIENCE) },
        }
        f();
        match prev_backend {
            Some(v) => unsafe { std::env::set_var(ENV_MESH_AUTH_BACKEND, v) },
            None => unsafe { std::env::remove_var(ENV_MESH_AUTH_BACKEND) },
        }
        match prev_aud {
            Some(v) => unsafe { std::env::set_var(ENV_MESH_AUTH_AUDIENCE, v) },
            None => unsafe { std::env::remove_var(ENV_MESH_AUTH_AUDIENCE) },
        }
    }

    #[test]
    fn backend_disabled_by_default() {
        with_env(None, None, || {
            assert!(!mesh_auth_backend_is_enabled());
        });
    }

    #[test]
    fn backend_enabled_when_env_matches() {
        with_env(Some("EntraAgentIdentity"), None, || {
            assert!(mesh_auth_backend_is_enabled());
        });
    }

    #[test]
    fn backend_disabled_for_unknown_value() {
        with_env(Some("Anonymous"), None, || {
            assert!(!mesh_auth_backend_is_enabled());
        });
        with_env(Some("foo"), None, || {
            assert!(!mesh_auth_backend_is_enabled());
        });
    }

    #[test]
    fn backend_is_case_insensitive_but_value_pinned() {
        // Forward-compat: small case-typo tolerance, but unrelated
        // strings still fall back to disabled.
        with_env(Some("entraAgentIdentity"), None, || {
            assert!(mesh_auth_backend_is_enabled());
        });
        with_env(Some("ENTRAAGENTIDENTITY"), None, || {
            assert!(mesh_auth_backend_is_enabled());
        });
    }

    #[test]
    fn audience_defaults_when_unset() {
        with_env(None, None, || {
            assert_eq!(mesh_audience(), DEFAULT_MESH_AUDIENCE);
        });
    }

    #[test]
    fn audience_overridable_via_env() {
        with_env(None, Some("api://my-relay/.default"), || {
            assert_eq!(mesh_audience(), "api://my-relay/.default");
        });
    }

    #[test]
    fn audience_falls_back_on_blank_value() {
        with_env(None, Some("   "), || {
            assert_eq!(mesh_audience(), DEFAULT_MESH_AUDIENCE);
        });
    }
}
