// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! HTTP client and response → [`ActualDecision`] mapping.
//!
//! The runner talks to a single inference router base URL — the same
//! URL the sandbox agent uses, but **without** the loopback shortcut
//! (the runner Pod is sidecar-less; it routes through the in-cluster
//! Service the controller stamps for every sandbox).
//!
//! ### Status → [`Decision`] mapping
//!
//! Until the router gains explicit `X-Azureclaw-Decision*` headers
//! (deferred to a later slice — see `slice-6-claw-eval-conformance.md
//! §6 "Router delta: none mandatory"`), the runner infers the
//! decision from the HTTP status code and reads `reason` opportunistically
//! from response headers + body:
//!
//! | Status | [`Decision`]      |
//! |--------|-------------------|
//! | 2xx    | `Allowed`         |
//! | 402    | `BudgetExceeded`  |
//! | 403    | `Blocked`         |
//! | 429    | `RateLimited`     |
//! | other  | `Blocked` (with HTTP body as reason) |
//!
//! The scenario kind unambiguously determines [`PolicyKindRef`] for
//! denials in the v1 starter corpora (every starter case is single-
//! kind by construction; verified by the `byPolicyKind` matcher in
//! [`crate::scenarios`]). If the router later surfaces
//! `X-Azureclaw-Decision-By`, the transport will prefer that header.

use anyhow::Context;
use azureclaw_eval_corpus::{Decision, PolicyKindRef};
use reqwest::{Response, StatusCode};
use serde_json::Value;
use std::time::Duration;

/// Header names the runner reads if present. None of these are required
/// on the v1 router today; they are read opportunistically so a future
/// router slice can supply ground truth without a runner image bump.
pub const DECISION_HEADER: &str = "x-azureclaw-decision";
pub const DECISION_BY_HEADER: &str = "x-azureclaw-decision-by";
pub const DECISION_REASON_HEADER: &str = "x-azureclaw-decision-reason";

/// Header echoed back by the router so its logs can be correlated with
/// runner cases. Set by the runner; the router does not need to read it.
pub const CASE_ID_HEADER: &str = "x-azureclaw-eval-case-id";

#[derive(Clone)]
pub struct Transport {
    client: reqwest::Client,
    base: String,
}

impl Transport {
    pub fn new(base: impl Into<String>, timeout: Duration) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .context("build reqwest client")?;
        let base = base.into();
        let base = base.trim_end_matches('/').to_string();
        Ok(Self { client, base })
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn base(&self) -> &str {
        &self.base
    }

    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    pub fn url(&self, path: &str) -> String {
        if path.starts_with('/') {
            format!("{}{}", self.base, path)
        } else {
            format!("{}/{}", self.base, path)
        }
    }
}

/// Decompose a [`Response`] into a [`Decision`] + reason + optional
/// `byPolicyKind` override. Consumes the response body lazily — returns
/// the body text iff the runner needs it for `reasonContains`.
///
/// `scenario_default_kind` is the [`PolicyKindRef`] the scenario type
/// implies (e.g. `EgressConnect` → `EgressAllowlist`). It is used iff
/// the response carries no `x-azureclaw-decision-by` header.
pub async fn response_to_decision(
    response: Response,
    scenario_default_kind: PolicyKindRef,
) -> ActualParts {
    let status = response.status();
    let headers = response.headers().clone();

    let header_decision = headers
        .get(DECISION_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_decision_header);

    let header_by_kind = headers
        .get(DECISION_BY_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_policy_kind_header);

    let header_reason = headers
        .get(DECISION_REASON_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    let body = response.text().await.unwrap_or_default();

    let decision = header_decision.unwrap_or_else(|| decision_from_status(status));
    let by_policy_kind = header_by_kind.or(if decision == Decision::Allowed {
        None
    } else {
        Some(scenario_default_kind)
    });

    let reason = header_reason.or_else(|| reason_from_body(&body));

    ActualParts {
        decision,
        by_policy_kind,
        reason,
    }
}

/// What the transport derives from a single HTTP response. The runner
/// aggregates these into `ActualDecision` (adding the burst
/// observations list when applicable).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActualParts {
    pub decision: Decision,
    pub by_policy_kind: Option<PolicyKindRef>,
    pub reason: Option<String>,
}

fn decision_from_status(s: StatusCode) -> Decision {
    if s.is_success() {
        return Decision::Allowed;
    }
    match s.as_u16() {
        402 => Decision::BudgetExceeded,
        403 => Decision::Blocked,
        429 => Decision::RateLimited,
        _ => Decision::Blocked,
    }
}

fn parse_decision_header(s: &str) -> Option<Decision> {
    match s {
        "Allowed" => Some(Decision::Allowed),
        "Blocked" => Some(Decision::Blocked),
        "RateLimited" => Some(Decision::RateLimited),
        "BudgetExceeded" => Some(Decision::BudgetExceeded),
        _ => None,
    }
}

fn parse_policy_kind_header(s: &str) -> Option<PolicyKindRef> {
    match s {
        "EgressAllowlist" => Some(PolicyKindRef::EgressAllowlist),
        "InferencePolicy" => Some(PolicyKindRef::InferencePolicy),
        "ToolPolicy" => Some(PolicyKindRef::ToolPolicy),
        "ClawMemory" => Some(PolicyKindRef::ClawMemory),
        "McpServer" => Some(PolicyKindRef::McpServer),
        _ => None,
    }
}

/// Best-effort: look for `"reason": "..."` or `"error": "..."` in a
/// JSON body. Returns `None` for non-JSON bodies so the verdict still
/// short-circuits if the caller did not require a reason.
fn reason_from_body(body: &str) -> Option<String> {
    if body.is_empty() {
        return None;
    }
    let v: Value = serde_json::from_str(body).ok()?;
    let obj = v.as_object()?;
    for key in ["reason", "error", "message", "detail"] {
        if let Some(s) = obj.get(key).and_then(|v| v.as_str())
            && !s.is_empty()
        {
            return Some(s.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn fetch(server: &MockServer, scenario_kind: PolicyKindRef) -> ActualParts {
        let client = reqwest::Client::new();
        let r = client
            .get(format!("{}/probe", server.uri()))
            .send()
            .await
            .unwrap();
        response_to_decision(r, scenario_kind).await
    }

    #[tokio::test]
    async fn status_200_maps_to_allowed_no_kind() {
        let s = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/probe"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&s)
            .await;
        let parts = fetch(&s, PolicyKindRef::InferencePolicy).await;
        assert_eq!(parts.decision, Decision::Allowed);
        assert_eq!(parts.by_policy_kind, None);
    }

    #[tokio::test]
    async fn status_403_maps_to_blocked_with_scenario_kind() {
        let s = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/probe"))
            .respond_with(
                ResponseTemplate::new(403).set_body_string(r#"{"reason":"host not in allowlist"}"#),
            )
            .mount(&s)
            .await;
        let parts = fetch(&s, PolicyKindRef::EgressAllowlist).await;
        assert_eq!(parts.decision, Decision::Blocked);
        assert_eq!(parts.by_policy_kind, Some(PolicyKindRef::EgressAllowlist));
        assert_eq!(parts.reason.as_deref(), Some("host not in allowlist"));
    }

    #[tokio::test]
    async fn status_429_maps_to_rate_limited() {
        let s = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/probe"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&s)
            .await;
        let parts = fetch(&s, PolicyKindRef::ToolPolicy).await;
        assert_eq!(parts.decision, Decision::RateLimited);
        assert_eq!(parts.by_policy_kind, Some(PolicyKindRef::ToolPolicy));
    }

    #[tokio::test]
    async fn status_402_maps_to_budget_exceeded() {
        let s = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/probe"))
            .respond_with(ResponseTemplate::new(402))
            .mount(&s)
            .await;
        let parts = fetch(&s, PolicyKindRef::InferencePolicy).await;
        assert_eq!(parts.decision, Decision::BudgetExceeded);
    }

    #[tokio::test]
    async fn header_overrides_status_mapping() {
        let s = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/probe"))
            .respond_with(
                ResponseTemplate::new(403)
                    .insert_header(DECISION_HEADER, "RateLimited")
                    .insert_header(DECISION_BY_HEADER, "ToolPolicy"),
            )
            .mount(&s)
            .await;
        let parts = fetch(&s, PolicyKindRef::InferencePolicy).await;
        assert_eq!(parts.decision, Decision::RateLimited);
        assert_eq!(parts.by_policy_kind, Some(PolicyKindRef::ToolPolicy));
    }

    #[tokio::test]
    async fn header_reason_wins_over_body_reason() {
        let s = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/probe"))
            .respond_with(
                ResponseTemplate::new(403)
                    .insert_header(DECISION_REASON_HEADER, "from-header")
                    .set_body_string(r#"{"reason":"from-body"}"#),
            )
            .mount(&s)
            .await;
        let parts = fetch(&s, PolicyKindRef::EgressAllowlist).await;
        assert_eq!(parts.reason.as_deref(), Some("from-header"));
    }

    #[tokio::test]
    async fn unknown_header_decision_falls_back_to_status() {
        let s = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/probe"))
            .respond_with(ResponseTemplate::new(403).insert_header(DECISION_HEADER, "Bogus"))
            .mount(&s)
            .await;
        let parts = fetch(&s, PolicyKindRef::ToolPolicy).await;
        assert_eq!(parts.decision, Decision::Blocked);
    }

    #[tokio::test]
    async fn empty_body_yields_no_reason() {
        let s = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/probe"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&s)
            .await;
        let parts = fetch(&s, PolicyKindRef::EgressAllowlist).await;
        assert_eq!(parts.reason, None);
    }

    #[tokio::test]
    async fn non_json_body_yields_no_reason() {
        let s = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/probe"))
            .respond_with(
                ResponseTemplate::new(403).set_body_string("Forbidden — host not in allowlist"),
            )
            .mount(&s)
            .await;
        let parts = fetch(&s, PolicyKindRef::EgressAllowlist).await;
        assert_eq!(parts.reason, None);
    }

    #[tokio::test]
    async fn body_error_field_used_when_reason_absent() {
        let s = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/probe"))
            .respond_with(
                ResponseTemplate::new(403)
                    .set_body_string(r#"{"error":"content-safety: jailbreak detected"}"#),
            )
            .mount(&s)
            .await;
        let parts = fetch(&s, PolicyKindRef::InferencePolicy).await;
        assert_eq!(
            parts.reason.as_deref(),
            Some("content-safety: jailbreak detected")
        );
    }

    #[test]
    fn transport_normalises_trailing_slash() {
        let t = Transport::new("http://router.local:8443/", Duration::from_secs(1)).unwrap();
        assert_eq!(t.base(), "http://router.local:8443");
    }

    #[test]
    fn transport_url_joins_paths() {
        let t = Transport::new("http://router.local:8443", Duration::from_secs(1)).unwrap();
        assert_eq!(
            t.url("/v1/chat/completions"),
            "http://router.local:8443/v1/chat/completions"
        );
    }
}
