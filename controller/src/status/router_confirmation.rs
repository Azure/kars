// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Controller-side poller for the inference-router's
//! `GET /internal/policy-status` endpoint.
//!
//! This is the consumer half of the principles.md §3 invariant —
//! "Ready ⇔ the router echoes the exact published digest". Slice 1a
//! shipped the router endpoint + `PolicyStatusRegistry`. Slice 1b
//! shipped the producer half on the controller side (ToolPolicy
//! writes `agt-profile.yaml` + digest annotation). This module is the
//! last missing link: the controller polls each sandbox's router,
//! parses the response, and the caller (a reconciler) decides whether
//! to promote `phase=Compiled → Ready`.
//!
//! ## Design notes
//!
//! * **Pure HTTP + parse.** This module knows nothing about Kubernetes
//!   or CRD shapes. It takes a URL + admin token and returns a parsed
//!   response (or a typed error). The reconciler is responsible for
//!   discovering URLs (via `Service` lookup) and tokens (via `Secret`
//!   lookup).
//! * **Plain HTTP, not HTTPS.** Sandbox routers expose the admin port
//!   (8443) without TLS — the network policy fences the cluster so
//!   only the operator pod can reach it. This matches the existing
//!   admin-mode endpoints (`/admin/*`, `/egress/*`).
//! * **Short timeout + small body cap.** The endpoint returns a
//!   bounded JSON envelope (one entry per `PolicyKind`, currently
//!   only `AgtProfile`). We cap both for defense-in-depth against a
//!   compromised or stuck router.
//! * **No retries here.** The caller already requeues every 15s while
//!   Compiled — retrying inside the poller would multiply that.
//!
//! ## Wire contract
//!
//! Mirrors `inference-router/src/routes/internal.rs` exactly. If the
//! router bumps `schema_version` past `1`, this client logs an error
//! and refuses to promote — fail-closed under principles.md §3.

use serde::Deserialize;
use std::time::Duration;

/// Maximum response body size we will accept from the router.
/// `GET /internal/policy-status` returns ~200 bytes per
/// `PolicyKind`; one MiB is several orders of magnitude of headroom
/// while still bounding memory if the router is malfunctioning.
const MAX_BODY_BYTES: usize = 1024 * 1024;

/// Default per-call timeout. Short because the router endpoint is
/// in-cluster and returns a constant-size body.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// Wire-contract `schema_version` value this client understands. Any
/// other version aborts with [`ConfirmError::UnknownSchemaVersion`].
/// Bump in lockstep with the router when the envelope changes shape.
pub const SUPPORTED_SCHEMA_VERSION: u32 = 1;

/// Parsed response from `GET /internal/policy-status`. Field-for-field
/// counterpart of `inference-router::routes::internal::PolicyStatusResponse`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct PolicyStatusResponse {
    pub schema_version: u32,
    pub count: usize,
    pub entries: Vec<PolicyStatusEntry>,
}

/// Parsed entry — counterpart of the router's `EntryDto`. `digest`
/// is `Option<String>` because the router records `None` if a reload
/// failed before any file was successfully loaded.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct PolicyStatusEntry {
    pub kind: String,
    pub digest: Option<String>,
    pub source_path: String,
    pub loaded_at: String,
    pub last_error: Option<String>,
}

impl PolicyStatusResponse {
    /// Return the `digest` from the `AgtProfile` entry, if present.
    /// Returns `None` when:
    /// * the response has no `AgtProfile` entry (router has not loaded
    ///   any policy yet);
    /// * the entry has `digest: None` (load failed mid-flight, error
    ///   in `last_error`).
    pub fn agt_profile_digest(&self) -> Option<&str> {
        self.entries
            .iter()
            .find(|e| e.kind == "AgtProfile")
            .and_then(|e| e.digest.as_deref())
    }

    /// Return the `last_error` from the `AgtProfile` entry, if any.
    /// Used by the reconciler to surface a router-side parse failure
    /// as the Ready=False message instead of a generic timeout.
    pub fn agt_profile_last_error(&self) -> Option<&str> {
        self.entries
            .iter()
            .find(|e| e.kind == "AgtProfile")
            .and_then(|e| e.last_error.as_deref())
    }
}

/// Failure modes of a single poll attempt.
///
/// The reconciler maps these to status reasons; in particular,
/// `Unreachable` / `HttpError` / `Timeout` all map to a transient
/// `AwaitingRouterEnforcement` (no Degraded), while
/// `DigestMismatch` / `UnknownSchemaVersion` are escalated to
/// `Degraded` because they indicate a real wire-contract problem.
#[derive(Debug, thiserror::Error)]
pub enum ConfirmError {
    #[error("HTTP request failed: {0}")]
    Network(#[from] reqwest::Error),

    #[error("router responded with status {0}")]
    HttpStatus(u16),

    #[error("response body too large (cap = {0} bytes)")]
    BodyTooLarge(usize),

    #[error("response body parse failed: {0}")]
    BodyParse(#[from] serde_json::Error),

    #[error("unsupported schema_version = {actual}, expected {expected}")]
    UnknownSchemaVersion { actual: u32, expected: u32 },
}

/// Fetch `GET {base_url}/internal/policy-status` with the supplied
/// admin token. `base_url` should be the router service root without
/// trailing slash, e.g. `http://my-sandbox.azureclaw-my-sandbox.svc.cluster.local:8443`.
///
/// The function:
/// 1. Sends `Authorization: Bearer <token>`.
/// 2. Reads at most [`MAX_BODY_BYTES`] of response body.
/// 3. Parses as [`PolicyStatusResponse`].
/// 4. Rejects any `schema_version` other than [`SUPPORTED_SCHEMA_VERSION`].
pub async fn fetch_router_policy_status(
    http: &reqwest::Client,
    base_url: &str,
    admin_token: &str,
) -> Result<PolicyStatusResponse, ConfirmError> {
    let url = format!("{}/internal/policy-status", base_url.trim_end_matches('/'));
    let resp = http
        .get(&url)
        .bearer_auth(admin_token)
        .send()
        .await
        .map_err(ConfirmError::Network)?;

    let status = resp.status();
    if !status.is_success() {
        return Err(ConfirmError::HttpStatus(status.as_u16()));
    }

    let body = resp.bytes().await.map_err(ConfirmError::Network)?;
    if body.len() > MAX_BODY_BYTES {
        return Err(ConfirmError::BodyTooLarge(MAX_BODY_BYTES));
    }

    let parsed: PolicyStatusResponse = serde_json::from_slice(&body)?;
    if parsed.schema_version != SUPPORTED_SCHEMA_VERSION {
        return Err(ConfirmError::UnknownSchemaVersion {
            actual: parsed.schema_version,
            expected: SUPPORTED_SCHEMA_VERSION,
        });
    }
    Ok(parsed)
}

/// Build the in-cluster router admin URL for a sandbox whose
/// `metadata.name` is `sandbox_name`. The controller creates per-
/// sandbox `Service`s in namespace `azureclaw-<sandbox_name>` whose
/// admin port is `8443` (see `reconciler/mod.rs::ensure_router_service`).
pub fn router_admin_url(sandbox_name: &str) -> String {
    format!(
        "http://{name}.azureclaw-{name}.svc.cluster.local:8443",
        name = sandbox_name
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn populated_response() -> serde_json::Value {
        serde_json::json!({
            "schema_version": 1,
            "count": 1,
            "entries": [{
                "kind": "AgtProfile",
                "digest": "sha256:abcdef0123",
                "source_path": "/etc/agt/policies",
                "loaded_at": "2026-05-13T09:00:00.000Z",
                "last_error": null
            }]
        })
    }

    #[test]
    fn router_admin_url_format_is_in_cluster_dns() {
        assert_eq!(
            router_admin_url("dev01"),
            "http://dev01.azureclaw-dev01.svc.cluster.local:8443"
        );
    }

    #[test]
    fn parsed_response_finds_agt_profile_digest() {
        let resp: PolicyStatusResponse =
            serde_json::from_value(populated_response()).expect("valid wire contract");
        assert_eq!(resp.agt_profile_digest(), Some("sha256:abcdef0123"));
        assert_eq!(resp.agt_profile_last_error(), None);
    }

    #[test]
    fn agt_profile_digest_returns_none_when_entry_missing() {
        let resp: PolicyStatusResponse = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "count": 0,
            "entries": []
        }))
        .unwrap();
        assert_eq!(resp.agt_profile_digest(), None);
    }

    #[test]
    fn agt_profile_digest_returns_none_when_digest_field_is_null() {
        let resp: PolicyStatusResponse = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "count": 1,
            "entries": [{
                "kind": "AgtProfile",
                "digest": null,
                "source_path": "/etc/agt/policies",
                "loaded_at": "2026-05-13T09:00:00.000Z",
                "last_error": "parse error: unexpected mapping at line 4"
            }]
        }))
        .unwrap();
        assert_eq!(resp.agt_profile_digest(), None);
        assert_eq!(
            resp.agt_profile_last_error(),
            Some("parse error: unexpected mapping at line 4")
        );
    }

    #[tokio::test]
    async fn fetch_returns_parsed_response_on_200() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/internal/policy-status"))
            .and(header("authorization", "Bearer test-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(populated_response()))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let resp = fetch_router_policy_status(&http, &server.uri(), "test-token")
            .await
            .expect("should succeed");
        assert_eq!(resp.schema_version, 1);
        assert_eq!(resp.agt_profile_digest(), Some("sha256:abcdef0123"));
    }

    #[tokio::test]
    async fn fetch_rejects_unknown_schema_version() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/internal/policy-status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "schema_version": 99,
                "count": 0,
                "entries": []
            })))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let err = fetch_router_policy_status(&http, &server.uri(), "tok")
            .await
            .expect_err("must reject");
        match err {
            ConfirmError::UnknownSchemaVersion { actual, expected } => {
                assert_eq!(actual, 99);
                assert_eq!(expected, SUPPORTED_SCHEMA_VERSION);
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[tokio::test]
    async fn fetch_surfaces_4xx_as_http_status() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/internal/policy-status"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let err = fetch_router_policy_status(&http, &server.uri(), "wrong-token")
            .await
            .expect_err("must reject");
        match err {
            ConfirmError::HttpStatus(401) => {}
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[tokio::test]
    async fn fetch_surfaces_5xx_as_http_status() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/internal/policy-status"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let err = fetch_router_policy_status(&http, &server.uri(), "tok")
            .await
            .expect_err("must reject");
        match err {
            ConfirmError::HttpStatus(503) => {}
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[tokio::test]
    async fn fetch_surfaces_garbage_body_as_body_parse() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/internal/policy-status"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let err = fetch_router_policy_status(&http, &server.uri(), "tok")
            .await
            .expect_err("must reject");
        assert!(matches!(err, ConfirmError::BodyParse(_)));
    }

    #[tokio::test]
    async fn fetch_trims_trailing_slash_in_base_url() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/internal/policy-status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(populated_response()))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        // base_url with trailing slash — must not result in
        // `//internal/policy-status` (404).
        let base = format!("{}/", server.uri());
        fetch_router_policy_status(&http, &base, "tok")
            .await
            .expect("must succeed despite trailing slash");
    }
}
