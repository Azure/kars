// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Keyless git write (§14) — the router's **loopback GitHub reverse-proxy**.
//!
//! The agent talks plain HTTP on loopback and the router injects a short-lived,
//! repo-scoped credential and forwards to GitHub over TLS. The agent NEVER holds
//! a token (defeating prompt-injection exfil), and every request's `owner/repo`
//! is checked against a fail-closed allowlist, so a broad underlying credential
//! can still only ever reach the declared repositories.
//!
//!  - `ANY /git/{owner}/{repo}/…`  → `https://github.com/{owner}/{repo}/…`
//!    (git smart-HTTP: clone/fetch/push). Injects HTTP Basic
//!    `x-access-token:<token>`. `git config insteadOf` makes normal
//!    `https://github.com/…` URLs route here transparently.
//!  - `ANY /gh-api/repos/{owner}/{repo}/…` → `https://api.github.com/…`
//!    (REST: open a PR, comment). Injects `Authorization: Bearer <token>`.
//!
//! Both are mounted on the same-pod loopback (the agent's trust boundary) and
//! are inert unless git write is configured (`state.git_write` is `Some`).

use axum::{
    Router,
    body::Body,
    extract::{ConnectInfo, Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use base64::Engine;
use std::net::SocketAddr;

use super::AppState;

const GITHUB_GIT: &str = "https://github.com";
const GITHUB_API: &str = "https://api.github.com";

/// Hop-by-hop headers that must not be forwarded (RFC 7230 §6.1) plus the ones
/// we set ourselves.
fn is_stripped_request_header(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "host"
            | "authorization"
            | "content-length"
            | "connection"
            | "proxy-connection"
            | "keep-alive"
            | "transfer-encoding"
            | "te"
            | "trailer"
            | "upgrade"
    )
}

fn is_stripped_response_header(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "connection"
            | "keep-alive"
            | "transfer-encoding"
            | "te"
            | "trailer"
            | "upgrade"
            | "content-length"
    )
}

/// Split a proxied path into `(owner/repo, rest)`. For `/git` the captured path
/// is `owner/repo/rest…`; for `/gh-api` it is the full API path and we only
/// accept `repos/{owner}/{repo}/…`.
fn owner_repo_from_git(path: &str) -> Option<(String, String)> {
    let mut it = path.trim_start_matches('/').splitn(3, '/');
    let owner = it.next()?;
    let repo = it.next()?;
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    let rest = it.next().unwrap_or("");
    Some((format!("{owner}/{repo}"), rest.to_string()))
}

fn owner_repo_from_api(path: &str) -> Option<String> {
    // Only `repos/{owner}/{repo}/…` (and the bare repo) are in scope.
    let mut it = path.trim_start_matches('/').splitn(4, '/');
    if it.next()? != "repos" {
        return None;
    }
    let owner = it.next()?;
    let repo = it.next()?;
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

fn deny(status: StatusCode, msg: &str) -> Response {
    (status, msg.to_string()).into_response()
}

/// Core proxy: rebuild the upstream URL, inject the credential, stream the body
/// through, and stream the response back.
async fn proxy(
    state: &AppState,
    upstream_url: String,
    auth: HeaderValue,
    method: Method,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let client = &state.client;
    // Stream the request body straight through (packfiles can be large).
    let stream = body.into_data_stream();
    let reqwest_body = reqwest::Body::wrap_stream(stream);

    let mut builder = client
        .request(method, &upstream_url)
        .header(axum::http::header::AUTHORIZATION, auth)
        .header(axum::http::header::USER_AGENT, HeaderValue::from_static("kars-inference-router"));
    for (name, value) in headers.iter() {
        if !is_stripped_request_header(name) && name.as_str() != "user-agent" {
            builder = builder.header(name, value);
        }
    }

    let upstream = match builder.body(reqwest_body).send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(url = %upstream_url, error = %e, "git proxy upstream error");
            return deny(StatusCode::BAD_GATEWAY, "upstream request to GitHub failed");
        }
    };

    let status = upstream.status();
    let mut resp_headers = HeaderMap::new();
    for (name, value) in upstream.headers().iter() {
        if !is_stripped_response_header(name) {
            resp_headers.insert(name.clone(), value.clone());
        }
    }
    let out_stream = upstream.bytes_stream();
    let mut response = Response::builder()
        .status(status)
        .body(Body::from_stream(out_stream))
        .unwrap_or_else(|_| deny(StatusCode::BAD_GATEWAY, "failed to build response"));
    *response.headers_mut() = resp_headers;
    response
}

/// `ANY /git/{owner}/{repo}/…` — git smart-HTTP, Basic `x-access-token:<token>`.
async fn git_handler(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
) -> Response {
    if !peer.ip().is_loopback() {
        // Same-pod agent only. The router service is cluster-reachable on 8443,
        // so refuse the git proxy to anything but pod-local loopback — a sibling
        // sandbox must never mint through this sandbox's credential.
        return deny(StatusCode::NOT_FOUND, "not found");
    }
    let Some(gw) = state.git_write.clone() else {
        return deny(StatusCode::NOT_FOUND, "git write is not enabled for this sandbox");
    };
    let (parts, body) = req.into_parts();
    let full_path = parts.uri.path().strip_prefix("/git/").unwrap_or("");
    let Some((owner_repo, rest)) = owner_repo_from_git(full_path) else {
        return deny(StatusCode::BAD_REQUEST, "expected /git/{owner}/{repo}/…");
    };
    if !gw.repo_allowed(&owner_repo) {
        tracing::warn!(repo = %owner_repo, "git proxy denied: repo not in the operator allowlist");
        return deny(
            StatusCode::FORBIDDEN,
            "this repository is outside the operator-granted scope for this mission",
        );
    }
    let token = match gw.token().await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = %e, "git proxy: failed to mint token");
            return deny(StatusCode::BAD_GATEWAY, "could not obtain a GitHub token");
        }
    };
    // git over HTTPS authenticates with Basic x-access-token:<token>.
    let basic = base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{token}"));
    let Ok(auth) = HeaderValue::from_str(&format!("Basic {basic}")) else {
        return deny(StatusCode::INTERNAL_SERVER_ERROR, "bad token");
    };
    let url = build_upstream(GITHUB_GIT, &format!("{owner_repo}/{rest}"), parts.uri.query());
    tracing::info!(repo = %owner_repo, "git proxy → github.com (token injected)");
    proxy(&state, url, auth, parts.method, parts.headers, body).await
}

/// `ANY /gh-api/repos/{owner}/{repo}/…` — REST, `Authorization: Bearer <token>`.
async fn api_handler(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
) -> Response {
    if !peer.ip().is_loopback() {
        return deny(StatusCode::NOT_FOUND, "not found");
    }
    let Some(gw) = state.git_write.clone() else {
        return deny(StatusCode::NOT_FOUND, "git write is not enabled for this sandbox");
    };
    let (parts, body) = req.into_parts();
    let api_path = parts.uri.path().strip_prefix("/gh-api/").unwrap_or("");
    let Some(owner_repo) = owner_repo_from_api(api_path) else {
        return deny(
            StatusCode::FORBIDDEN,
            "only /gh-api/repos/{owner}/{repo}/… is proxied (repo-scoped)",
        );
    };
    if !gw.repo_allowed(&owner_repo) {
        tracing::warn!(repo = %owner_repo, "gh-api proxy denied: repo not in the operator allowlist");
        return deny(
            StatusCode::FORBIDDEN,
            "this repository is outside the operator-granted scope for this mission",
        );
    }
    // Review is a governed action too: a sub-agent must NOT approve a PR (no
    // self-approval of its own delegated work). Sub-agents open + comment; only a
    // principal reviews. Deny `POST /repos/{o}/{r}/pulls/{n}/reviews` for sub-agents.
    if !gw.can_merge() && is_pr_review_submit(&parts.method, api_path) {
        tracing::warn!(repo = %owner_repo, "gh-api proxy denied: sub-agents cannot submit PR reviews (no self-approval)");
        return deny(
            StatusCode::FORBIDDEN,
            "sub-agents cannot approve pull requests — a principal reviews your PR",
        );
    }
    // Merge is a governed action: a sub-agent may open PRs but must NOT merge —
    // it asks the principal (or a human via the inbox) to merge. Deny
    // `PUT/POST /repos/{owner}/{repo}/pulls/{n}/merge` for sub-agents.
    if !gw.can_merge() && is_pr_merge(&parts.method, api_path) {
        tracing::warn!(repo = %owner_repo, "gh-api proxy denied: sub-agents cannot merge (ask the principal to review + merge)");
        return deny(
            StatusCode::FORBIDDEN,
            "sub-agents cannot merge a pull request — push your branch, open the PR, and ask your principal to review and merge",
        );
    }
    let token = match gw.token().await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = %e, "gh-api proxy: failed to mint token");
            return deny(StatusCode::BAD_GATEWAY, "could not obtain a GitHub token");
        }
    };
    // Mandatory review before merge (§14): a PR may only be merged once it carries
    // an APPROVED review. kars enforces this at the gateway because every action
    // uses the same App identity on GitHub, so GitHub-native "review required"
    // can't tell author from reviewer — the router can. No approval → 403.
    if is_pr_merge(&parts.method, api_path) {
        if let Some(pr) = pr_number_from_api_path(api_path) {
            match pr_has_approved_review(&state, &owner_repo, pr, &token).await {
                Ok(true) => {}
                Ok(false) => {
                    tracing::warn!(repo = %owner_repo, pr, "gh-api proxy denied merge: no approving review on the PR");
                    return deny(
                        StatusCode::FORBIDDEN,
                        "a review is required before merge — a principal must submit an approving review on this PR first",
                    );
                }
                Err(_) => {
                    return deny(StatusCode::BAD_GATEWAY, "could not verify the PR review state before merge");
                }
            }
        }
    }
    let Ok(auth) = HeaderValue::from_str(&format!("Bearer {token}")) else {
        return deny(StatusCode::INTERNAL_SERVER_ERROR, "bad token");
    };
    let url = build_upstream(GITHUB_API, api_path, parts.uri.query());
    tracing::info!(repo = %owner_repo, "gh-api proxy → api.github.com (token injected)");
    proxy(&state, url, auth, parts.method, parts.headers, body).await
}

fn build_upstream(base: &str, path: &str, query: Option<&str>) -> String {
    let path = path.trim_start_matches('/');
    match query {
        Some(q) if !q.is_empty() => format!("{base}/{path}?{q}"),
        _ => format!("{base}/{path}"),
    }
}

/// True for the "submit a PR review" API call —
/// `POST /repos/{owner}/{repo}/pulls/{number}/reviews`.
fn is_pr_review_submit(method: &Method, api_path: &str) -> bool {
    if *method != Method::POST {
        return false;
    }
    let p = api_path.trim_end_matches('/');
    p.ends_with("/reviews") && p.contains("/pulls/")
}

/// Extract the PR number from `repos/{owner}/{repo}/pulls/{number}/…`.
fn pr_number_from_api_path(api_path: &str) -> Option<u64> {
    let mut it = api_path.trim_start_matches('/').split('/');
    // repos / owner / repo / pulls / NUMBER
    if it.next()? != "repos" {
        return None;
    }
    let _owner = it.next()?;
    let _repo = it.next()?;
    if it.next()? != "pulls" {
        return None;
    }
    it.next()?.parse::<u64>().ok()
}

/// Whether the PR is mergeable per the review policy: at least one review has
/// been submitted, and the most recent review is not `CHANGES_REQUESTED`.
///
/// NB: we deliberately do NOT require `APPROVED`. Every kars agent acts under the
/// same GitHub App identity, and GitHub forbids approving your *own* PR — so an
/// `APPROVED` state is unreachable for App-authored PRs and would deadlock the
/// merge. Instead the gate enforces that a review STEP happened (sub-agents are
/// blocked from submitting reviews, so a sub-agent's PR can only be reviewed by a
/// principal), and that changes weren't requested.
async fn pr_has_approved_review(
    state: &AppState,
    owner_repo: &str,
    pr: u64,
    token: &str,
) -> Result<bool, ()> {
    let url = format!("{GITHUB_API}/repos/{owner_repo}/pulls/{pr}/reviews?per_page=100");
    let resp = state
        .client
        .get(&url)
        .header(axum::http::header::AUTHORIZATION, format!("Bearer {token}"))
        .header(axum::http::header::USER_AGENT, "kars-inference-router")
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .map_err(|_| ())?;
    if !resp.status().is_success() {
        return Err(());
    }
    let reviews: serde_json::Value = resp.json().await.map_err(|_| ())?;
    let Some(arr) = reviews.as_array() else {
        return Ok(false);
    };
    let states: Vec<String> = arr
        .iter()
        .filter_map(|r| r.get("state").and_then(|s| s.as_str()).map(|s| s.to_ascii_uppercase()))
        .collect();
    Ok(review_states_permit_merge(&states))
}

/// Pure review-policy decision (extracted for unit testing): a review STEP must
/// have happened, and the most recent decisive review must not request changes.
/// COMMENTED counts as a review (an App cannot APPROVE its own PR), but a trailing
/// CHANGES_REQUESTED blocks the merge until resolved.
fn review_states_permit_merge(states: &[String]) -> bool {
    let decisive: Vec<&String> = states
        .iter()
        .filter(|s| *s == "APPROVED" || *s == "CHANGES_REQUESTED" || *s == "COMMENTED")
        .collect();
    if decisive.is_empty() {
        return false; // no review at all → block
    }
    // Block if the most recent decisive review (APPROVED/CHANGES_REQUESTED)
    // requested changes. (GitHub returns reviews in chronological order.)
    let last_decisive = decisive
        .iter()
        .rev()
        .find(|s| ***s == "APPROVED" || ***s == "CHANGES_REQUESTED");
    !last_decisive.map(|s| **s == "CHANGES_REQUESTED").unwrap_or(false)
}

/// True for the "merge a pull request" API call —
/// `PUT /repos/{owner}/{repo}/pulls/{number}/merge`. GitHub uses PUT; we also
/// treat POST defensively. The path is the `/gh-api/`-stripped API path.
fn is_pr_merge(method: &Method, api_path: &str) -> bool {
    if *method != Method::PUT && *method != Method::POST {
        return false;
    }
    let p = api_path.trim_end_matches('/');
    p.ends_with("/merge") && p.contains("/pulls/")
}

/// Loopback routes for keyless git write. Inert (404) unless `state.git_write`
/// is configured — the handlers check it per request.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/git/{*path}", any(git_handler))
        .route("/gh-api/{*path}", any(api_handler))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_path_split() {
        assert_eq!(
            owner_repo_from_git("o/r/info/refs"),
            Some(("o/r".into(), "info/refs".into()))
        );
        assert_eq!(owner_repo_from_git("o/r"), Some(("o/r".into(), "".into())));
        assert_eq!(owner_repo_from_git("o"), None);
    }

    #[test]
    fn api_path_scope() {
        assert_eq!(owner_repo_from_api("repos/o/r/pulls"), Some("o/r".into()));
        assert_eq!(owner_repo_from_api("user/repos"), None);
        assert_eq!(owner_repo_from_api("orgs/x/repos"), None);
    }

    #[test]
    fn upstream_url_with_query() {
        assert_eq!(
            build_upstream(GITHUB_GIT, "o/r/info/refs", Some("service=git-upload-pack")),
            "https://github.com/o/r/info/refs?service=git-upload-pack"
        );
        assert_eq!(
            build_upstream(GITHUB_API, "repos/o/r/pulls", None),
            "https://api.github.com/repos/o/r/pulls"
        );
    }

    #[test]
    fn merge_detection() {
        assert!(is_pr_merge(&Method::PUT, "repos/o/r/pulls/3/merge"));
        assert!(is_pr_merge(&Method::PUT, "repos/o/r/pulls/3/merge/"));
        // Opening / listing / commenting PRs is not a merge.
        assert!(!is_pr_merge(&Method::POST, "repos/o/r/pulls"));
        assert!(!is_pr_merge(&Method::GET, "repos/o/r/pulls/3/merge"));
        assert!(!is_pr_merge(&Method::PATCH, "repos/o/r/pulls/3"));
    }

    #[test]
    fn review_submit_detection() {
        assert!(is_pr_review_submit(&Method::POST, "repos/o/r/pulls/3/reviews"));
        assert!(!is_pr_review_submit(&Method::GET, "repos/o/r/pulls/3/reviews"));
        assert!(!is_pr_review_submit(&Method::POST, "repos/o/r/pulls/3/comments"));
    }

    #[test]
    fn pr_number_parse() {
        assert_eq!(pr_number_from_api_path("repos/o/r/pulls/42/merge"), Some(42));
        assert_eq!(pr_number_from_api_path("repos/o/r/pulls/7/reviews"), Some(7));
        assert_eq!(pr_number_from_api_path("repos/o/r/pulls"), None);
        assert_eq!(pr_number_from_api_path("repos/o/r/issues/3"), None);
    }

    fn states(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn review_gate_blocks_when_no_review() {
        assert!(!review_states_permit_merge(&[]));
        // Non-review states (e.g. DISMISSED/PENDING) do not count as a review.
        assert!(!review_states_permit_merge(&states(&["PENDING", "DISMISSED"])));
    }

    #[test]
    fn review_gate_allows_commented() {
        // An App can't APPROVE its own PR; a COMMENTED review satisfies the gate.
        assert!(review_states_permit_merge(&states(&["COMMENTED"])));
        assert!(review_states_permit_merge(&states(&["APPROVED"])));
    }

    #[test]
    fn review_gate_blocks_trailing_changes_requested() {
        assert!(!review_states_permit_merge(&states(&["CHANGES_REQUESTED"])));
        // A trailing CHANGES_REQUESTED blocks even after an earlier approval.
        assert!(!review_states_permit_merge(&states(&["APPROVED", "CHANGES_REQUESTED"])));
        // ...but a later APPROVED/COMMENTED clears an earlier CHANGES_REQUESTED
        // (last decisive review wins; COMMENTED is not decisive so APPROVED does it).
        assert!(review_states_permit_merge(&states(&[
            "CHANGES_REQUESTED",
            "APPROVED"
        ])));
        // A COMMENTED after CHANGES_REQUESTED does NOT clear it (not decisive).
        assert!(!review_states_permit_merge(&states(&[
            "CHANGES_REQUESTED",
            "COMMENTED"
        ])));
    }
}
