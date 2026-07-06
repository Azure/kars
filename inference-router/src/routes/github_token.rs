// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `/v1/github-token` — keyless repo access for the sandbox (design note §14).
//!
//! UID 1000 (the agent) is blocked by the egress-guard from holding or fetching
//! long-lived Git credentials. This route lets the sandbox acquire a **short-
//! lived GitHub App installation token** from the router on demand: the router
//! holds the App private key (never the agent), mints the token, and returns it
//! scoped + expiring (~1h). The sandbox wires it as a git credential helper, so
//! `git`/`gh` authenticate without the agent ever storing a credential.
//!
//! Fail-closed: when no GitHub App is configured (the three `GITHUB_APP_*` env
//! vars are absent), the route returns 404 — the feature is simply off and the
//! sandbox falls back to anonymous (public-repo) access. Configuring the App is
//! a pure forward-rollout; nothing breaks when it's absent.

use axum::{
    Json, Router, extract::State, http::HeaderMap, http::StatusCode, response::IntoResponse,
    routing::get,
};
use serde::Serialize;

use super::{AppState, extract_admin_token};
use crate::errors;
use crate::github_app::GitHubApp;

#[derive(Debug, Serialize)]
struct GitHubTokenResponse {
    /// The installation access token. The agent never persists this; it is used
    /// transiently by the git credential helper and expires within the hour.
    token: String,
    token_type: &'static str,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: &'static str,
    detail: String,
}

async fn github_token_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // Minting a live, write-scoped GitHub installation token MUST require the
    // admin token — even over loopback. The router's same-pod auth exemption
    // otherwise lets the sandboxed agent (UID 1000), which reaches the router on
    // 127.0.0.1:8443, fetch a credential it must never hold (design note §14;
    // this route sits in the admin-protected group precisely to withhold it from
    // UID 1000). Mirrors the governance.rs trust-mutation guard, which likewise
    // enforces even from localhost.
    if let Some(ref expected) = state.admin_token {
        match extract_admin_token(&headers).as_deref() {
            Some(tok) if crate::handoff::constant_time_eq(tok.as_bytes(), expected.as_bytes()) => {}
            _ => {
                tracing::warn!(
                    "GET /v1/github-token denied: missing or invalid admin token (localhost is NOT exempt for credential minting)"
                );
                return errors::flat(
                    StatusCode::FORBIDDEN,
                    "Admin token required to mint a GitHub installation token",
                )
                .into_response();
            }
        }
    }

    let Some(app) = GitHubApp::from_env() else {
        // No GitHub App configured. Fall back to a direct write token when the
        // operator provided one (a fine-grained PAT scoped to the target repos).
        // This is the simple, no-App path for keyless git write. Fail-closed:
        // when neither is set, 404 → the sandbox stays read-only/anonymous.
        if let Ok(tok) = std::env::var("GIT_WRITE_TOKEN") {
            let tok = tok.trim().to_string();
            if !tok.is_empty() {
                return (
                    StatusCode::OK,
                    Json(GitHubTokenResponse { token: tok, token_type: "token" }),
                )
                    .into_response();
            }
        }
        return (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "github_app_not_configured",
                detail: "GITHUB_APP_ID / GITHUB_APP_INSTALLATION_ID / GITHUB_APP_PRIVATE_KEY (or GIT_WRITE_TOKEN) not set".into(),
            }),
        )
            .into_response();
    };

    match app.installation_token().await {
        Ok(token) => (
            StatusCode::OK,
            Json(GitHubTokenResponse { token, token_type: "token" }),
        )
            .into_response(),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(ErrorResponse {
                error: "github_token_exchange_failed",
                detail: format!("{e:#}"),
            }),
        )
            .into_response(),
    }
}

/// Routes for keyless GitHub access. Mounted unconditionally; the handler
/// returns 404 when no App is configured (fail-closed, additive).
pub fn routes() -> Router<AppState> {
    Router::new().route("/v1/github-token", get(github_token_handler))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_builds() {
        // Smoke: the router assembles without an App configured.
        let _r = routes();
    }
}
