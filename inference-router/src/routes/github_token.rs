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
    Router, extract::State, http::HeaderMap, http::StatusCode, response::IntoResponse, routing::get,
};

use super::AppState;
use crate::errors;

async fn github_token_handler(
    State(_state): State<AppState>,
    _headers: HeaderMap,
) -> impl IntoResponse {
    // DISABLED (§14): the keyless git write flow is now the loopback reverse-proxy
    // (`/git/*` + `/gh-api/*`), where the router injects a repo-scoped token that
    // the agent NEVER sees. This agent-facing mint endpoint is intentionally
    // retired — leaving it live would let UID 1000 (which can read the admin
    // token) obtain a raw installation token and defeat the "agent holds no
    // credential" guarantee. Fail closed.
    errors::flat(
        StatusCode::GONE,
        "This endpoint is retired. Git write is keyless via the router's git proxy (http://127.0.0.1:8443/git/); the agent never receives a token.",
    )
    .into_response()
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
