// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Slice 2d.2 — health-aware deployment failover.
//!
//! Wraps [`crate::proxy::forward`] with a candidate-walk that honours
//! `InferencePolicy.spec.modelPreference.{primary,fallback[]}.deployment`.
//! Same-provider only — the router still holds a single Foundry/AOAI
//! client at process start (`UpstreamConfig.endpoint`); we only swap
//! the `deployment` field per attempt.
//!
//! Per-attempt outcome feeds [`DeploymentHealthRegistry`]:
//! * 2xx ⇒ `record_success` (clears any streak)
//! * 5xx (502/503/504 + generic 500) or 429 ⇒ `record_failure`
//!   (increments streak, may flip to unhealthy after 3 in 60s)
//! * 4xx (other than 429) ⇒ no record, returned to caller immediately
//!   (client error — failover wouldn't help)
//! * Transport error (no HTTP status) ⇒ `record_failure` + try next
//!
//! When every candidate has been exhausted, the **last attempt's**
//! result is surfaced to the caller. This keeps the agent-facing
//! contract authentic — operators see the real upstream error, not a
//! synthetic 503 hiding the actual failure mode. Audit logging in
//! between every attempt captures the full failover chain.

use anyhow::Result;
use axum::http::{HeaderMap, Method, StatusCode};
use bytes::Bytes;
use reqwest::Client;
use std::sync::Arc;

use crate::auth::WorkloadIdentityAuth;
use crate::copilot_auth::CopilotTokenCache;
use crate::deployment_health::DeploymentHealthRegistry;
use crate::inference_policy_loader::{InferencePolicySnapshot, ModelRef};
use crate::proxy::{UpstreamConfig, forward};

/// Decide whether an upstream response status is a *retry-worthy*
/// failure that should mark the deployment unhealthy and trigger a
/// failover walk.
///
/// Pulled out of the loop so the unit tests can pin the classifier
/// independently from the I/O — adding a new "retry on this status"
/// must be a deliberate change, not an accident.
#[must_use]
pub fn is_failover_trigger(status: StatusCode) -> bool {
    let code = status.as_u16();
    code == 429 || (500..=599).contains(&code)
}

/// Build the ordered candidate list the failover walk will try.
///
/// Returned vector is **non-empty by construction** — when no policy
/// is loaded (or the policy has no usable deployments), the original
/// `upstream.deployment` is returned as a single-element list, so the
/// caller always has at least one attempt to make.
///
/// Deduplicates while preserving order: if `primary.deployment` and
/// `fallback[0].deployment` happen to be the same, we only try it
/// once. Empty strings are skipped.
#[must_use]
pub fn build_candidates(
    upstream: &UpstreamConfig,
    snapshot: &InferencePolicySnapshot,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |dep: &str| {
        if dep.is_empty() {
            return;
        }
        if !out.iter().any(|d| d == dep) {
            out.push(dep.to_string());
        }
    };

    if let Some(ref pref) = snapshot.model_preference {
        push(&pref.primary.deployment);
        for ModelRef { deployment, .. } in &pref.fallback {
            push(deployment);
        }
    }

    // Always keep the env-driven default as a final safety net so a
    // mid-flight policy unload (or a policy with only an empty
    // primary) never produces a zero-candidate list.
    push(&upstream.deployment);

    if out.is_empty() {
        // Theoretically unreachable (`upstream.deployment` is set
        // from `Config::default_model` which has its own default),
        // but defence-in-depth: empty list ⇒ one attempt at the
        // caller-supplied upstream as-is.
        out.push(upstream.deployment.clone());
    }
    out
}

/// Walks `build_candidates(...)`, skipping deployments the health
/// cache currently flags as unhealthy, and returns the first
/// successful (or non-retryable) response. If every candidate either
/// fails with a retry-worthy status or is currently unhealthy, falls
/// back to the **last attempted** result (or to the **first unhealthy
/// candidate** when every candidate was skipped without an attempt).
///
/// Logs each failover transition with `tracing::warn!` carrying the
/// `from` / `to` deployment, observed status (if any), and the policy
/// digest — enough for an operator to correlate against the loaded
/// `InferencePolicy`.
#[allow(clippy::too_many_arguments)]
pub async fn forward_with_failover(
    auth: &WorkloadIdentityAuth,
    copilot: Option<&CopilotTokenCache>,
    client: &Client,
    health: &Arc<DeploymentHealthRegistry>,
    upstream_base: &UpstreamConfig,
    snapshot: &InferencePolicySnapshot,
    method: Method,
    path: &str,
    request_headers: &HeaderMap,
    request_body: Bytes,
) -> Result<(StatusCode, HeaderMap, Bytes)> {
    let candidates = build_candidates(upstream_base, snapshot);

    // Track the last *actually attempted* response so we can surface
    // a real upstream error if every candidate fails.
    let mut last_result: Option<Result<(StatusCode, HeaderMap, Bytes)>> = None;
    // The very first candidate (regardless of health) — used as a
    // fallback-of-last-resort when every candidate was skipped
    // because the cache flagged them all unhealthy.
    let first_candidate = candidates
        .first()
        .cloned()
        .unwrap_or_else(|| upstream_base.deployment.clone());

    for (idx, deployment) in candidates.iter().enumerate() {
        // Skip unhealthy candidates *unless* this is the only one
        // we have left to try (i.e. we've exhausted the list).
        if !health.is_healthy(deployment) {
            tracing::info!(
                sandbox = %upstream_base.sandbox_name,
                deployment = %deployment,
                "InferencePolicy failover: skipping unhealthy deployment"
            );
            continue;
        }

        let mut upstream = upstream_base.clone();
        upstream.deployment = deployment.clone();

        if idx > 0 {
            tracing::warn!(
                sandbox = %upstream_base.sandbox_name,
                from = %first_candidate,
                to = %deployment,
                attempt = idx + 1,
                digest = %snapshot.digest,
                "InferencePolicy failover: trying fallback deployment"
            );
        }

        let attempt = forward(
            auth,
            copilot,
            client,
            &upstream,
            method.clone(),
            path,
            request_headers,
            request_body.clone(),
        )
        .await;

        match &attempt {
            Ok((status, _, _)) if is_failover_trigger(*status) => {
                health.record_failure(deployment);
                tracing::warn!(
                    sandbox = %upstream_base.sandbox_name,
                    deployment = %deployment,
                    status = %status.as_u16(),
                    digest = %snapshot.digest,
                    "InferencePolicy failover: upstream returned retry-worthy status"
                );
                last_result = Some(attempt);
                continue;
            }
            Ok((status, _, _)) => {
                if status.is_success() {
                    health.record_success(deployment);
                }
                return attempt;
            }
            Err(e) => {
                health.record_failure(deployment);
                tracing::warn!(
                    sandbox = %upstream_base.sandbox_name,
                    deployment = %deployment,
                    error = %format!("{e:#}"),
                    digest = %snapshot.digest,
                    "InferencePolicy failover: transport error"
                );
                last_result = Some(attempt);
                continue;
            }
        }
    }

    if let Some(result) = last_result {
        return result;
    }

    // Every candidate was skipped without an attempt — the cache says
    // none are healthy. Punch through with the first candidate
    // anyway so the agent gets *some* response (even if it's the
    // same upstream failure that put us here). Better than a synthetic
    // error that hides the real cause.
    tracing::warn!(
        sandbox = %upstream_base.sandbox_name,
        deployment = %first_candidate,
        digest = %snapshot.digest,
        "InferencePolicy failover: all candidates unhealthy, retrying primary anyway"
    );
    let mut upstream = upstream_base.clone();
    upstream.deployment = first_candidate.clone();
    let attempt = forward(
        auth,
        copilot,
        client,
        &upstream,
        method,
        path,
        request_headers,
        request_body,
    )
    .await;
    match &attempt {
        Ok((status, _, _)) if status.is_success() => health.record_success(&first_candidate),
        Ok((status, _, _)) if is_failover_trigger(*status) => {
            health.record_failure(&first_candidate);
        }
        Err(_) => health.record_failure(&first_candidate),
        _ => {}
    }
    attempt
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inference_policy_loader::{ModelPreference, ModelRef};

    fn upstream(dep: &str) -> UpstreamConfig {
        UpstreamConfig {
            endpoint: "https://example.openai.azure.com".into(),
            deployment: dep.to_string(),
            sandbox_name: "sbx".into(),
        }
    }

    fn snapshot_with(primary: &str, fallback: &[&str]) -> InferencePolicySnapshot {
        InferencePolicySnapshot {
            digest: "sha256:test".into(),
            model_preference: Some(ModelPreference {
                primary: ModelRef {
                    provider: "Foundry".into(),
                    deployment: primary.into(),
                },
                fallback: fallback
                    .iter()
                    .map(|d| ModelRef {
                        provider: "Foundry".into(),
                        deployment: (*d).into(),
                    })
                    .collect(),
            }),
            ..InferencePolicySnapshot::default()
        }
    }

    #[test]
    fn classifier_treats_5xx_and_429_as_retry_worthy() {
        assert!(is_failover_trigger(StatusCode::INTERNAL_SERVER_ERROR));
        assert!(is_failover_trigger(StatusCode::BAD_GATEWAY));
        assert!(is_failover_trigger(StatusCode::SERVICE_UNAVAILABLE));
        assert!(is_failover_trigger(StatusCode::GATEWAY_TIMEOUT));
        assert!(is_failover_trigger(StatusCode::TOO_MANY_REQUESTS));
    }

    #[test]
    fn classifier_passes_4xx_through_without_failover() {
        assert!(!is_failover_trigger(StatusCode::BAD_REQUEST));
        assert!(!is_failover_trigger(StatusCode::UNAUTHORIZED));
        assert!(!is_failover_trigger(StatusCode::FORBIDDEN));
        assert!(!is_failover_trigger(StatusCode::NOT_FOUND));
    }

    #[test]
    fn classifier_passes_2xx_through() {
        assert!(!is_failover_trigger(StatusCode::OK));
        assert!(!is_failover_trigger(StatusCode::ACCEPTED));
    }

    #[test]
    fn build_candidates_includes_primary_then_fallback_chain() {
        let snap = snapshot_with("primary", &["fb-a", "fb-b"]);
        let c = build_candidates(&upstream("default"), &snap);
        assert_eq!(c, vec!["primary", "fb-a", "fb-b", "default"]);
    }

    #[test]
    fn build_candidates_dedups_overlap() {
        let snap = snapshot_with("primary", &["primary", "fb-a"]);
        let c = build_candidates(&upstream("primary"), &snap);
        assert_eq!(c, vec!["primary", "fb-a"]);
    }

    #[test]
    fn build_candidates_skips_empty_deployment_strings() {
        let snap = snapshot_with("", &["", "fb-a"]);
        let c = build_candidates(&upstream("default"), &snap);
        assert_eq!(c, vec!["fb-a", "default"]);
    }

    #[test]
    fn build_candidates_no_policy_yields_just_default() {
        let snap = InferencePolicySnapshot::default();
        let c = build_candidates(&upstream("env-default"), &snap);
        assert_eq!(c, vec!["env-default"]);
    }

    #[test]
    fn build_candidates_never_empty() {
        // Even with everything blank, we get a one-element list.
        let snap = snapshot_with("", &[]);
        let c = build_candidates(&upstream(""), &snap);
        assert_eq!(c, vec![""]);
    }
}
