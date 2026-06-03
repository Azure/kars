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
    /// Return the `digest` from the entry whose `kind` matches
    /// `kind`, if present. Returns `None` when:
    /// * the response has no entry of that kind (router has not
    ///   loaded a policy of this kind yet);
    /// * the entry has `digest: None` (load failed mid-flight, error
    ///   in `last_error`).
    ///
    /// Each `PolicyKind` variant on the router side has a fixed
    /// string name (`"AgtProfile"`, `"InferencePolicy"`, `"Memory"`,
    /// …); see `inference-router/src/policy_status.rs` for the
    /// canonical list. Reconcilers should pass that exact string.
    pub fn find_digest(&self, kind: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|e| e.kind == kind)
            .and_then(|e| e.digest.as_deref())
    }

    /// Return the `last_error` from the entry whose `kind` matches
    /// `kind`, if any. Used by reconcilers to surface a router-side
    /// parse failure as the Ready=False message instead of a generic
    /// timeout.
    pub fn find_last_error(&self, kind: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|e| e.kind == kind)
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
/// trailing slash, e.g. `http://my-sandbox.kars-my-sandbox.svc.cluster.local:8443`.
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

/// Outcome of attempting to confirm router-side enforcement for one
/// `PolicyKind` across every referencing sandbox. Drives both
/// `status.phase` and the `Ready` condition's reason+message on the
/// reconciler side.
///
/// This is the principles.md §3 binding shared by every CRD that
/// participates in the "Ready ⇔ router echo" loop (ToolPolicy
/// in Slice 1c, InferencePolicy in Slice 2a, KarsMemory in Slice 3,
/// fleet-of-McpServer in Slice 4). Each reconciler's only job after
/// computing the published digest is to pick one of these states;
/// everything downstream (phase, conditions, requeue cadence) is a
/// pure function of this enum.
#[derive(Debug, PartialEq, Eq)]
pub enum RouterEnforcementState {
    /// The CR has no router-observable enforcement surface — there
    /// is no data-plane observation the controller can make. For
    /// example, a ToolPolicy with `spec.agtProfile` unset still
    /// drives the legacy AGT runtime plugin (in-process consumer);
    /// the router has nothing to echo. Reconcilers stamp `Ready`
    /// here for back-compat.
    NotApplicable,
    /// The CR is ready on the controller side but no sandbox in the
    /// CR's namespace references it. There is no router that could
    /// confirm enforcement. Reconcilers stamp `Compiled`.
    NoSandboxesReferencing,
    /// At least one referencing sandbox's router failed to respond,
    /// returned a different digest, or had not yet loaded the new
    /// policy. `matched`/`total` is surfaced in the Ready message
    /// so operators can see partial confirmation. Reconcilers stamp
    /// `Compiled` with reason `AwaitingRouterEnforcement`.
    Awaiting {
        total: usize,
        matched: usize,
        message: String,
    },
    /// Every referencing sandbox's router echoed the exact digest
    /// the controller published. Promote to `Ready=True /
    /// reason=RouterEnforcing`. Closes the principles.md §3 loop.
    Confirmed { total: usize },
}

/// Should the reconciler emit a `PolicyNotEnforced` Warning Event for
/// this state? Returns `true` only when the controller is genuinely
/// waiting for a router-side echo from an already-bound sandbox.
///
/// The previous shape also returned `true` for
/// [`RouterEnforcementState::NoSandboxesReferencing`], which spammed
/// the event log every reconcile for any chart-installed default CR
/// nobody happens to reference (e.g. `kars-default` ToolPolicy on
/// every cluster). Orphan state is still surfaced in the CR's
/// `Ready=False / reason=NoSandboxesReferencing` condition so
/// `kubectl get` shows it — just without periodic Warning Event
/// noise that drowns out genuine `Awaiting` signals.
///
/// Shared by `tool_policy_reconciler`, `inference_policy_reconciler`,
/// and `kars_memory_reconciler` so the three CRD kinds stay in lock-
/// step on this decision.
pub fn should_publish_warning(state: &RouterEnforcementState, degraded: bool) -> bool {
    !degraded && matches!(state, RouterEnforcementState::Awaiting { .. })
}

/// Pure decision: aggregate per-sandbox poll outcomes into a
/// [`RouterEnforcementState`]. Factored out of any specific
/// reconciler so the promotion logic is unit-testable without K8s
/// or HTTP I/O, and so every CRD-kind shares one implementation.
///
/// * `expected_digest` is the controller-side digest published for
///   the policy (e.g. `find_digest("AgtProfile")` for ToolPolicy,
///   `find_digest("InferencePolicy")` for InferencePolicy).
/// * `kind` is the `PolicyKind` string the router uses on the wire
///   — `"AgtProfile"`, `"InferencePolicy"`, `"KarsMemory"`, … — and
///   must match `inference-router/src/policy_status.rs` exactly.
/// * `results` is one entry per referencing sandbox: either the
///   parsed router response or the network/parse error encountered
///   while polling.
pub fn decide_enforcement_state(
    expected_digest: &str,
    kind: &str,
    results: &[(String, Result<PolicyStatusResponse, ConfirmError>)],
) -> RouterEnforcementState {
    let total = results.len();
    if total == 0 {
        return RouterEnforcementState::NoSandboxesReferencing;
    }
    let mut matched = 0usize;
    let mut messages: Vec<String> = Vec::with_capacity(total);
    for (sandbox, outcome) in results {
        match outcome {
            Ok(resp) => match resp.find_digest(kind) {
                Some(d) if d == expected_digest => {
                    matched += 1;
                }
                Some(other) => messages.push(format!(
                    "{sandbox}: router echoed digest mismatch ({other} != {expected_digest})"
                )),
                None => {
                    let err = resp
                        .find_last_error(kind)
                        .map(|e| format!(" (last_error: {e})"))
                        .unwrap_or_default();
                    messages.push(format!("{sandbox}: router has not yet loaded {kind}{err}"));
                }
            },
            Err(e) => {
                messages.push(format!("{sandbox}: router unreachable ({e})"));
            }
        }
    }
    if matched == total {
        RouterEnforcementState::Confirmed { total }
    } else {
        let mut message = format!("{matched}/{total} sandbox routers confirmed digest");
        if !messages.is_empty() {
            let detail = messages
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join("; ");
            message.push_str("; ");
            message.push_str(&detail);
            if messages.len() > 3 {
                message.push_str(&format!("; (+{} more)", messages.len() - 3));
            }
        }
        RouterEnforcementState::Awaiting {
            total,
            matched,
            message,
        }
    }
}

/// Build the in-cluster router admin URL for a sandbox whose
/// `metadata.name` is `sandbox_name`. The controller creates per-
/// sandbox `Service`s in namespace `kars-<sandbox_name>` whose
/// admin port is `8443` (see `reconciler/mod.rs::ensure_router_service`).
pub fn router_admin_url(sandbox_name: &str) -> String {
    format!(
        "http://{name}.kars-{name}.svc.cluster.local:8443",
        name = sandbox_name
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn should_publish_warning_only_when_awaiting() {
        // Truly waiting on a bound router → emit (the legit one-shot
        // diagnostic operators want to see).
        assert!(should_publish_warning(
            &RouterEnforcementState::Awaiting {
                total: 1,
                matched: 0,
                message: "router silent".into(),
            },
            false,
        ));
        // No referrers → suppress (orphan default policies otherwise
        // re-fire every reconcile and flood the event log).
        assert!(!should_publish_warning(
            &RouterEnforcementState::NoSandboxesReferencing,
            false,
        ));
        // Healthy → suppress (loop is closed; nothing to shout about).
        assert!(!should_publish_warning(
            &RouterEnforcementState::Confirmed { total: 3 },
            false,
        ));
        // Back-compat NotApplicable → suppress.
        assert!(!should_publish_warning(
            &RouterEnforcementState::NotApplicable,
            false,
        ));
        // Degraded short-circuits: the Degraded handler emits its own
        // (different-reason) event; we must not double-fire.
        assert!(!should_publish_warning(
            &RouterEnforcementState::Awaiting {
                total: 1,
                matched: 0,
                message: "router silent".into(),
            },
            true,
        ));
    }

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
            "http://dev01.kars-dev01.svc.cluster.local:8443"
        );
    }

    #[test]
    fn parsed_response_finds_agt_profile_digest() {
        let resp: PolicyStatusResponse =
            serde_json::from_value(populated_response()).expect("valid wire contract");
        assert_eq!(resp.find_digest("AgtProfile"), Some("sha256:abcdef0123"));
        assert_eq!(resp.find_last_error("AgtProfile"), None);
    }

    #[test]
    fn agt_profile_digest_returns_none_when_entry_missing() {
        let resp: PolicyStatusResponse = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "count": 0,
            "entries": []
        }))
        .unwrap();
        assert_eq!(resp.find_digest("AgtProfile"), None);
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
        assert_eq!(resp.find_digest("AgtProfile"), None);
        assert_eq!(
            resp.find_last_error("AgtProfile"),
            Some("parse error: unexpected mapping at line 4")
        );
    }

    #[test]
    fn find_digest_generalizes_over_kind() {
        let resp: PolicyStatusResponse = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "count": 2,
            "entries": [
                {
                    "kind": "AgtProfile",
                    "digest": "sha256:aaa",
                    "source_path": "/etc/agt/policies",
                    "loaded_at": "2026-05-13T09:00:00.000Z",
                    "last_error": null
                },
                {
                    "kind": "InferencePolicy",
                    "digest": "sha256:bbb",
                    "source_path": "/etc/inference/policies",
                    "loaded_at": "2026-05-13T09:00:01.000Z",
                    "last_error": null
                }
            ]
        }))
        .unwrap();
        assert_eq!(resp.find_digest("AgtProfile"), Some("sha256:aaa"));
        assert_eq!(resp.find_digest("InferencePolicy"), Some("sha256:bbb"));
        assert_eq!(resp.find_digest("Nonexistent"), None);
    }

    #[test]
    fn find_last_error_isolates_per_kind() {
        let resp: PolicyStatusResponse = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "count": 2,
            "entries": [
                {
                    "kind": "AgtProfile",
                    "digest": "sha256:aaa",
                    "source_path": "/etc/agt/policies",
                    "loaded_at": "2026-05-13T09:00:00.000Z",
                    "last_error": null
                },
                {
                    "kind": "InferencePolicy",
                    "digest": null,
                    "source_path": "/etc/inference/policies",
                    "loaded_at": "2026-05-13T09:00:01.000Z",
                    "last_error": "schema mismatch on v2"
                }
            ]
        }))
        .unwrap();
        assert_eq!(resp.find_last_error("AgtProfile"), None);
        assert_eq!(
            resp.find_last_error("InferencePolicy"),
            Some("schema mismatch on v2")
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
        assert_eq!(resp.find_digest("AgtProfile"), Some("sha256:abcdef0123"));
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

    // ── decide_enforcement_state: generic-kind coverage ──────────────
    //
    // Per-CRD reconcilers test their own calling conventions (e.g.
    // `tool_policy_reconciler::tests` covers `"AgtProfile"`). These
    // tests pin the generic-over-kind contract so a future reconciler
    // (InferencePolicy, KarsMemory, McpServer) can pass any kind
    // string and get the same pure-function behaviour.

    fn mk_resp(kind: &str, digest: Option<&str>, last_error: Option<&str>) -> PolicyStatusResponse {
        PolicyStatusResponse {
            schema_version: 1,
            count: 1,
            entries: vec![PolicyStatusEntry {
                kind: kind.into(),
                digest: digest.map(String::from),
                source_path: "/etc/kars/policies".into(),
                loaded_at: "2026-05-13T09:00:00.000Z".into(),
                last_error: last_error.map(String::from),
            }],
        }
    }

    #[test]
    fn decide_confirms_per_kind_independently() {
        // Same digest published, but the router only echoes it under
        // `InferencePolicy` — a reconciler asking for `"AgtProfile"`
        // must NOT confirm.
        let results = vec![(
            "a".into(),
            Ok(mk_resp("InferencePolicy", Some("sha256:abc"), None)),
        )];
        let agt = decide_enforcement_state("sha256:abc", "AgtProfile", &results);
        assert!(
            matches!(agt, RouterEnforcementState::Awaiting { matched: 0, .. }),
            "AgtProfile must not be confirmed by an InferencePolicy entry: {agt:?}"
        );
        let inf = decide_enforcement_state("sha256:abc", "InferencePolicy", &results);
        assert_eq!(inf, RouterEnforcementState::Confirmed { total: 1 });
    }

    #[test]
    fn decide_carries_kind_into_not_yet_loaded_message() {
        // Router responded but has no entry for the requested kind.
        // The Awaiting message must mention the kind so operators can
        // tell which policy bundle the router is still missing.
        let results = vec![(
            "a".into(),
            Ok(mk_resp("AgtProfile", Some("sha256:other"), None)),
        )];
        let s = decide_enforcement_state("sha256:abc", "InferencePolicy", &results);
        let msg = match s {
            RouterEnforcementState::Awaiting { message, .. } => message,
            other => panic!("expected Awaiting, got {other:?}"),
        };
        assert!(
            msg.contains("InferencePolicy"),
            "kind must appear in the message: {msg}"
        );
    }

    #[test]
    fn decide_no_results_yields_no_sandboxes_referencing_regardless_of_kind() {
        // The empty-results branch is kind-agnostic — verify it.
        for kind in ["AgtProfile", "InferencePolicy", "KarsMemory"] {
            let s = decide_enforcement_state("sha256:abc", kind, &[]);
            assert_eq!(
                s,
                RouterEnforcementState::NoSandboxesReferencing,
                "kind={kind}"
            );
        }
    }
}
