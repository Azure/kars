// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Slice 2d.2 — health-aware, cross-provider deployment failover.
//!
//! Wraps [`crate::proxy::forward`] with a candidate-walk that honours
//! `InferencePolicy.spec.modelPreference.{primary,fallback[]}.{provider,
//! deployment}`. Each candidate carries its OWN provider tag; when a
//! candidate's provider differs from the sandbox's default (and is
//! configured — see `Config::resolve_provider`), the attempt is sent to
//! that provider's real endpoint/auth, not just a different deployment name
//! on the same endpoint. This is what makes "GitHub Copilot is down, retry
//! on the Foundry route configured for this sandbox" an actual failover,
//! not just a deployment-name swap within one provider.
//!
//! A candidate with no resolvable provider (tag absent from the policy, or
//! not configured on this sandbox) falls back to `upstream_base` — the
//! sandbox's own default endpoint/auth — so a policy that only ever names
//! deployments (no provider tags) behaves exactly as before.
//!
//! Per-attempt outcome feeds [`DeploymentHealthRegistry`], keyed by
//! `"<provider>::<deployment>"` when a provider is known (so the same
//! deployment name on two different providers is tracked independently),
//! or bare `<deployment>` for the provider-less/default case (unchanged
//! key shape — no behavior change for existing single-provider policies):
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
use crate::config::Config;
use crate::copilot_auth::CopilotTokenCache;
use crate::deployment_health::DeploymentHealthRegistry;
use crate::inference_policy_loader::{InferencePolicySnapshot, ModelRef};
use crate::proxy::{UpstreamConfig, forward};

/// One candidate in a failover walk: a deployment name plus the provider
/// tag it should be resolved against (`None` ⇒ use `upstream_base` as-is,
/// the pre-existing single-provider behavior).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub provider: Option<String>,
    pub deployment: String,
}

/// Health-registry key for a candidate — `"<provider>::<deployment>"` when
/// a provider tag is present, else the bare deployment name (unchanged
/// shape for the common single-provider case).
fn health_key(c: &Candidate) -> String {
    match &c.provider {
        Some(p) if !p.is_empty() => format!("{p}::{}", c.deployment),
        _ => c.deployment.clone(),
    }
}

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
/// Deduplicates by deployment name while preserving order (first
/// occurrence — and its provider tag — wins): if `primary.deployment` and
/// `fallback[0].deployment` happen to be the same, we only try it once.
/// Empty strings are skipped.
#[must_use]
pub fn build_candidates(
    upstream: &UpstreamConfig,
    snapshot: &InferencePolicySnapshot,
) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    let mut push = |dep: &str, provider: Option<String>| {
        if dep.is_empty() {
            return;
        }
        if !out.iter().any(|c| c.deployment == dep) {
            out.push(Candidate {
                provider,
                deployment: dep.to_string(),
            });
        }
    };

    if let Some(ref pref) = snapshot.model_preference {
        push(
            &pref.primary.deployment,
            Some(pref.primary.provider.clone()).filter(|p| !p.is_empty()),
        );
        for ModelRef { deployment, provider } in &pref.fallback {
            push(
                deployment,
                Some(provider.clone()).filter(|p| !p.is_empty()),
            );
        }
    }

    // Always keep the env-driven default as a final safety net so a
    // mid-flight policy unload (or a policy with only an empty
    // primary) never produces a zero-candidate list. No provider tag —
    // it rides on `upstream_base` exactly as before.
    push(&upstream.deployment, None);

    if out.is_empty() {
        // Theoretically unreachable (`upstream.deployment` is set
        // from `Config::default_model` which has its own default),
        // but defence-in-depth: empty list ⇒ one attempt at the
        // caller-supplied upstream as-is.
        out.push(Candidate {
            provider: None,
            deployment: upstream.deployment.clone(),
        });
    }
    out
}

/// Resolve one candidate into the `UpstreamConfig` an attempt should
/// actually use: when the candidate names a provider that's configured on
/// this sandbox (`Config::resolve_provider`), route to that provider's real
/// endpoint/key; otherwise fall back to `upstream_base` (the sandbox's
/// default), only swapping the deployment name — the original behavior.
fn resolve_candidate(base: &UpstreamConfig, config: &Config, c: &Candidate) -> UpstreamConfig {
    let mut upstream = base.clone();
    upstream.deployment = c.deployment.clone();
    if let Some(tag) = c.provider.as_deref()
        && let Some(target) = config.resolve_provider(tag)
        && target.endpoint != base.endpoint
    {
        tracing::info!(
            sandbox = %base.sandbox_name,
            provider = %tag,
            endpoint = %target.endpoint,
            "InferencePolicy failover: routing candidate to a different provider"
        );
        upstream.endpoint = target.endpoint;
        upstream.provider_api_key = target.api_key;
    }
    upstream
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
    config: &Config,
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
    let first_candidate = candidates.first().cloned().unwrap_or(Candidate {
        provider: None,
        deployment: upstream_base.deployment.clone(),
    });

    for (idx, candidate) in candidates.iter().enumerate() {
        let key = health_key(candidate);
        // Skip unhealthy candidates *unless* this is the only one
        // we have left to try (i.e. we've exhausted the list).
        if !health.is_healthy(&key) {
            tracing::info!(
                sandbox = %upstream_base.sandbox_name,
                deployment = %key,
                "InferencePolicy failover: skipping unhealthy deployment"
            );
            continue;
        }

        let upstream = resolve_candidate(upstream_base, config, candidate);

        if idx > 0 {
            tracing::warn!(
                sandbox = %upstream_base.sandbox_name,
                from = %health_key(&first_candidate),
                to = %key,
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
                health.record_failure(&key);
                tracing::warn!(
                    sandbox = %upstream_base.sandbox_name,
                    deployment = %key,
                    status = %status.as_u16(),
                    digest = %snapshot.digest,
                    "InferencePolicy failover: upstream returned retry-worthy status"
                );
                last_result = Some(attempt);
                continue;
            }
            Ok((status, _, _)) => {
                if status.is_success() {
                    health.record_success(&key);
                }
                return attempt;
            }
            Err(e) => {
                health.record_failure(&key);
                tracing::warn!(
                    sandbox = %upstream_base.sandbox_name,
                    deployment = %key,
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
    let first_key = health_key(&first_candidate);
    tracing::warn!(
        sandbox = %upstream_base.sandbox_name,
        deployment = %first_key,
        digest = %snapshot.digest,
        "InferencePolicy failover: all candidates unhealthy, retrying primary anyway"
    );
    let upstream = resolve_candidate(upstream_base, config, &first_candidate);
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
        Ok((status, _, _)) if status.is_success() => health.record_success(&first_key),
        Ok((status, _, _)) if is_failover_trigger(*status) => {
            health.record_failure(&first_key);
        }
        Err(_) => health.record_failure(&first_key),
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
            provider_api_key: None,
        }
    }

    /// Helper: extract just the deployment names, in order, for assertions
    /// that predate provider-tagged candidates.
    fn deployments(candidates: &[Candidate]) -> Vec<&str> {
        candidates.iter().map(|c| c.deployment.as_str()).collect()
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
        assert_eq!(deployments(&c), vec!["primary", "fb-a", "fb-b", "default"]);
        // Every policy-sourced candidate carries its provider tag.
        assert_eq!(c[0].provider.as_deref(), Some("Foundry"));
        assert_eq!(c[1].provider.as_deref(), Some("Foundry"));
        // The env-driven default has no provider tag — rides on upstream_base.
        assert_eq!(c[3].provider, None);
    }

    #[test]
    fn build_candidates_dedups_overlap() {
        let snap = snapshot_with("primary", &["primary", "fb-a"]);
        let c = build_candidates(&upstream("primary"), &snap);
        assert_eq!(deployments(&c), vec!["primary", "fb-a"]);
    }

    #[test]
    fn build_candidates_skips_empty_deployment_strings() {
        let snap = snapshot_with("", &["", "fb-a"]);
        let c = build_candidates(&upstream("default"), &snap);
        assert_eq!(deployments(&c), vec!["fb-a", "default"]);
    }

    #[test]
    fn build_candidates_no_policy_yields_just_default() {
        let snap = InferencePolicySnapshot::default();
        let c = build_candidates(&upstream("env-default"), &snap);
        assert_eq!(deployments(&c), vec!["env-default"]);
    }

    #[test]
    fn build_candidates_never_empty() {
        // Even with everything blank, we get a one-element list.
        let snap = snapshot_with("", &[]);
        let c = build_candidates(&upstream(""), &snap);
        assert_eq!(deployments(&c), vec![""]);
    }

    // ── Cross-provider resolution ────────────────────────────────────────

    fn provider_config(tag: &str, endpoint: &str) -> Config {
        let mut providers = std::collections::HashMap::new();
        providers.insert(
            tag.to_string(),
            crate::config::ProviderEndpoint {
                tag: tag.to_string(),
                endpoint: endpoint.to_string(),
                api_key: Some("provider-key".to_string()),
            },
        );
        Config {
            port: 8443,
            foundry_endpoint: None,
            foundry_project_endpoint: None,
            azure_openai_endpoint: Some("https://example.openai.azure.com".into()),
            default_model: "gpt-4o-mini".into(),
            content_safety_enabled: false,
            prompt_shields_enabled: false,
            content_safety_endpoint: None,
            token_budget_daily: 0,
            token_budget_per_request: 0,
            registry_mode: crate::config::RegistryMode::Local,
            registry_url: None,
            provider_override: None,
            providers,
        }
    }

    #[test]
    fn resolve_candidate_switches_endpoint_for_configured_provider() {
        let base = upstream("default");
        let config = provider_config("foundry", "https://contoso.services.ai.azure.com");
        let candidate = Candidate {
            provider: Some("foundry".to_string()),
            deployment: "gpt-4.1".to_string(),
        };
        let resolved = resolve_candidate(&base, &config, &candidate);
        assert_eq!(resolved.endpoint, "https://contoso.services.ai.azure.com");
        assert_eq!(resolved.deployment, "gpt-4.1");
        assert_eq!(resolved.provider_api_key.as_deref(), Some("provider-key"));
    }

    #[test]
    fn resolve_candidate_falls_back_to_base_when_provider_not_configured() {
        let base = upstream("default");
        let config = provider_config("foundry", "https://contoso.services.ai.azure.com");
        let candidate = Candidate {
            // Not configured on this sandbox — must not error, just ride
            // on the base endpoint (fail-open, matching the pre-cross-
            // provider behavior for policies with an unresolvable tag).
            provider: Some("github-copilot".to_string()),
            deployment: "opus-4.8".to_string(),
        };
        let resolved = resolve_candidate(&base, &config, &candidate);
        assert_eq!(resolved.endpoint, base.endpoint);
        assert_eq!(resolved.deployment, "opus-4.8");
    }

    #[test]
    fn resolve_candidate_with_no_provider_tag_only_swaps_deployment() {
        let base = upstream("default");
        let config = provider_config("foundry", "https://contoso.services.ai.azure.com");
        let candidate = Candidate {
            provider: None,
            deployment: "gpt-4o".to_string(),
        };
        let resolved = resolve_candidate(&base, &config, &candidate);
        assert_eq!(resolved.endpoint, base.endpoint);
        assert_eq!(resolved.deployment, "gpt-4o");
    }

    #[test]
    fn health_key_includes_provider_when_present() {
        let c = Candidate {
            provider: Some("foundry".to_string()),
            deployment: "gpt-4.1".to_string(),
        };
        assert_eq!(health_key(&c), "foundry::gpt-4.1");
    }

    #[test]
    fn health_key_is_bare_deployment_when_no_provider() {
        let c = Candidate {
            provider: None,
            deployment: "gpt-4.1".to_string(),
        };
        assert_eq!(health_key(&c), "gpt-4.1");
    }
}

