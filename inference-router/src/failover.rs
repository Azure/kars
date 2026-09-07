// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Health-aware provider and deployment failover.
//!
//! Wraps [`crate::proxy::forward`] with a candidate-walk that honours
//! `InferencePolicy.spec.modelPreference.{primary,fallback[]}.deployment`.
//! Every candidate is resolved from the original default endpoint, never the
//! preceding candidate, so fallback cannot retain another provider's credentials.
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
use crate::config::Config;
use crate::copilot_auth::CopilotTokenCache;
use crate::deployment_health::DeploymentHealthRegistry;
use crate::inference_policy_loader::{InferencePolicySnapshot, ModelRef};
use crate::proxy::failure::{ForwardFailure, retryable_failure};
use crate::proxy::{AuthenticationProvenance, UpstreamConfig, forward};

mod stream;
pub use stream::forward_stream_with_failover;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub provider: Option<String>,
    pub deployment: String,
    pub routing_intent: RoutingIntent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutingIntent {
    /// Historical primary-model metadata routes only when a named endpoint
    /// was separately registered. Native key presence is not routing intent.
    Metadata,
    /// An authoritative policy provider or a newly declared fallback route.
    Explicit,
}

impl RoutingIntent {
    pub(crate) fn primary(policy: &InferencePolicySnapshot) -> Self {
        if policy
            .provider
            .as_ref()
            .is_some_and(|provider| !provider.trim().is_empty())
        {
            Self::Explicit
        } else {
            Self::Metadata
        }
    }
}

// Native families and implicit Copilot selection can change destination with
// intent alone; their metadata route must not poison a native route's health.
fn intent_can_change_backend(provider: Option<&str>) -> bool {
    let Some(provider) = provider else {
        return false;
    };
    match crate::provider::parse_tag(provider) {
        Ok(Some(kind)) => kind != crate::provider::ProviderKind::AzureOpenAI,
        Err(_) => true,
        Ok(None) => provider.trim().eq_ignore_ascii_case("github-copilot"),
    }
}

fn health_key(candidate: &Candidate) -> String {
    match candidate.provider.as_deref().filter(|tag| !tag.is_empty()) {
        Some(provider)
            if candidate.routing_intent == RoutingIntent::Metadata
                && intent_can_change_backend(Some(provider)) =>
        {
            format!("metadata:{provider}::{}", candidate.deployment)
        }
        Some(provider) => format!("{provider}::{}", candidate.deployment),
        None => candidate.deployment.clone(),
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
    crate::proxy::failure::retryable_rejection(status)
}

/// Build the ordered candidate list the failover walk will try.
///
/// Returned vector is **non-empty by construction** — when no policy
/// is loaded (or the policy has no usable deployments), the original
/// `upstream.deployment` is returned as a single-element list, so the
/// caller always has at least one attempt to make.
///
/// Deduplicates provider/deployment pairs while preserving order, except when
/// metadata and explicit intent can select different native/default backends.
#[must_use]
pub fn build_candidates(
    upstream: &UpstreamConfig,
    snapshot: &InferencePolicySnapshot,
) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    let mut push = |dep: &str, provider: Option<String>, routing_intent: RoutingIntent| {
        if dep.is_empty() {
            return;
        }
        if !out.iter().any(|d| {
            d.deployment == dep
                && d.provider == provider
                && (d.routing_intent == routing_intent
                    || !intent_can_change_backend(provider.as_deref()))
        }) {
            out.push(Candidate {
                deployment: dep.to_string(),
                provider,
                routing_intent,
            });
        }
    };

    if let Some(ref pref) = snapshot.model_preference {
        push(
            &pref.primary.deployment,
            snapshot
                .provider
                .clone()
                .filter(|p| !p.trim().is_empty())
                .or_else(|| Some(pref.primary.provider.clone()).filter(|p| !p.trim().is_empty())),
            RoutingIntent::primary(snapshot),
        );
        for ModelRef {
            deployment,
            provider,
        } in &pref.fallback
        {
            push(
                deployment,
                Some(provider.clone()).filter(|p| !p.is_empty()),
                RoutingIntent::Explicit,
            );
        }
    } else if let Some(provider) = snapshot.provider.as_ref().filter(|p| !p.trim().is_empty()) {
        push(
            &upstream.deployment,
            Some(provider.clone()),
            RoutingIntent::Explicit,
        );
    }

    // Always keep the env-driven default as a final safety net so a
    // mid-flight policy unload (or a policy with only an empty
    // primary) never produces a zero-candidate list.
    push(&upstream.deployment, None, RoutingIntent::Explicit);

    if out.is_empty() {
        // Theoretically unreachable (`upstream.deployment` is set
        // from `Config::default_model` which has its own default),
        // but defence-in-depth: empty list ⇒ one attempt at the
        // caller-supplied upstream as-is.
        out.push(Candidate {
            provider: None,
            deployment: upstream.deployment.clone(),
            routing_intent: RoutingIntent::Explicit,
        });
    }
    out
}

pub(crate) fn resolve_candidate(
    base: &UpstreamConfig,
    config: &Config,
    candidate: &Candidate,
) -> std::result::Result<UpstreamConfig, crate::provider::ProviderError> {
    let mut upstream = base.clone();
    upstream.deployment = candidate.deployment.clone();
    let Some(tag) = candidate.provider.as_deref() else {
        return Ok(upstream);
    };
    let identity = AuthenticationProvenance::Named {
        provider_id: tag.trim().to_ascii_lowercase(),
    };
    if candidate.routing_intent == RoutingIntent::Metadata {
        if let Some(target) = config.providers.get(&tag.trim().to_ascii_lowercase()) {
            upstream.endpoint = target.endpoint.clone();
            upstream.provider = crate::provider::ProviderKind::AzureOpenAI;
            upstream.api_key = None;
            upstream.provider_api_key = target.api_key.clone();
            upstream.authentication = identity;
        }
        return Ok(upstream);
    }
    match crate::provider::parse_tag(tag)? {
        Some(crate::provider::ProviderKind::Anthropic) => {
            if let crate::provider::ProviderTarget::Anthropic { endpoint, api_key } =
                crate::provider::resolve(Some(tag), config)?
            {
                upstream.endpoint = endpoint;
                upstream.provider = crate::provider::ProviderKind::Anthropic;
                upstream.api_key = Some(api_key);
                upstream.provider_api_key = None;
                upstream.authentication = identity;
            }
        }
        Some(crate::provider::ProviderKind::Ollama) => {
            if let crate::provider::ProviderTarget::Ollama { endpoint } =
                crate::provider::resolve(Some(tag), config)?
            {
                upstream.endpoint = endpoint;
                upstream.provider = crate::provider::ProviderKind::Ollama;
                upstream.api_key = None;
                upstream.provider_api_key = None;
                upstream.authentication = identity;
            }
        }
        _ => {
            if let Some(target) = config.resolve_provider(tag.trim()) {
                // An informational tag on the unchanged legacy Copilot default
                // does not manufacture a second named account.
                if !config
                    .providers
                    .contains_key(&tag.trim().to_ascii_lowercase())
                    && crate::proxy::is_copilot_endpoint(&base.endpoint)
                    && crate::proxy::is_copilot_endpoint(&target.endpoint)
                {
                    return Ok(upstream);
                }
                upstream.endpoint = target.endpoint;
                upstream.provider = crate::provider::ProviderKind::AzureOpenAI;
                upstream.api_key = None;
                upstream.provider_api_key = target.api_key;
                upstream.authentication = identity;
            }
        }
    }
    Ok(upstream)
}

pub(crate) fn candidates_for_request(
    base: &UpstreamConfig,
    policy: &InferencePolicySnapshot,
    body: &[u8],
) -> Vec<Candidate> {
    let mut candidates = build_candidates(base, policy);
    if policy.model_preference.is_none()
        && let Ok(value) = serde_json::from_slice::<serde_json::Value>(body)
        && let Some(model) = value
            .get("model")
            .and_then(|model| model.as_str())
            .filter(|model| !model.trim().is_empty())
        && model != candidates[0].deployment
    {
        // No policy-selected model: preserve the pre-existing public API's
        // caller-selected model, then retain the configured safety net.
        let mut requested = candidates[0].clone();
        requested.deployment = model.to_string();
        candidates.insert(0, requested);
    }
    candidates
}

fn request_body_for_candidate(body: &Bytes, deployment: &str) -> Bytes {
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return body.clone();
    };
    let Some(object) = value.as_object_mut() else {
        return body.clone();
    };
    object.insert("model".into(), deployment.into());
    serde_json::to_vec(&value)
        .map(Bytes::from)
        .unwrap_or_else(|_| body.clone())
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
) -> Result<(StatusCode, HeaderMap, Bytes, UpstreamConfig)> {
    let candidates = candidates_for_request(upstream_base, snapshot, &request_body);

    // Track the last *actually attempted* response so we can surface
    // a real upstream error if every candidate fails.
    let mut last_result = None;
    // The very first candidate (regardless of health) — used as a
    // fallback-of-last-resort when every candidate was skipped
    // because the cache flagged them all unhealthy.
    let first_candidate = candidates
        .first()
        .cloned()
        .expect("candidate list is non-empty");

    for (idx, candidate) in candidates.iter().enumerate() {
        let deployment = health_key(candidate);
        // Skip unhealthy candidates *unless* this is the only one
        // we have left to try (i.e. we've exhausted the list).
        if !health.is_healthy(&deployment) {
            tracing::info!(
                sandbox = %upstream_base.sandbox_name,
                deployment = %deployment,
                "InferencePolicy failover: skipping unhealthy deployment"
            );
            continue;
        }

        let upstream = resolve_candidate(upstream_base, config, candidate)
            .map_err(ForwardFailure::configuration)?;

        if idx > 0 {
            tracing::warn!(
                sandbox = %upstream_base.sandbox_name,
                from = %health_key(&first_candidate),
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
            request_body_for_candidate(&request_body, &candidate.deployment),
        )
        .await;

        match &attempt {
            Ok((status, _, _)) if is_failover_trigger(*status) => {
                health.record_failure(&deployment);
                tracing::warn!(
                    sandbox = %upstream_base.sandbox_name,
                    deployment = %deployment,
                    status = %status.as_u16(),
                    digest = %snapshot.digest,
                    "InferencePolicy failover: upstream returned retry-worthy status"
                );
                last_result =
                    Some(attempt.map(|(status, headers, body)| (status, headers, body, upstream)));
                continue;
            }
            Ok((status, _, _)) => {
                if status.is_success() {
                    health.record_success(&deployment);
                }
                return attempt.map(|(status, headers, body)| (status, headers, body, upstream));
            }
            Err(e) if retryable_failure(e) => {
                health.record_failure(&deployment);
                tracing::warn!(
                    sandbox = %upstream_base.sandbox_name,
                    deployment = %deployment,
                    error = %format!("{e:#}"),
                    digest = %snapshot.digest,
                    "InferencePolicy failover: retryable upstream failure"
                );
                last_result =
                    Some(attempt.map(|(status, headers, body)| (status, headers, body, upstream)));
                continue;
            }
            Err(_) => {
                return attempt.map(|(status, headers, body)| (status, headers, body, upstream));
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
        deployment = %health_key(&first_candidate),
        digest = %snapshot.digest,
        "InferencePolicy failover: all candidates unhealthy, retrying primary anyway"
    );
    let upstream = resolve_candidate(upstream_base, config, &first_candidate)
        .map_err(ForwardFailure::configuration)?;
    let attempt = forward(
        auth,
        copilot,
        client,
        &upstream,
        method,
        path,
        request_headers,
        request_body_for_candidate(&request_body, &first_candidate.deployment),
    )
    .await;
    match &attempt {
        Ok((status, _, _)) if status.is_success() => {
            health.record_success(&health_key(&first_candidate))
        }
        Ok((status, _, _)) if is_failover_trigger(*status) => {
            health.record_failure(&health_key(&first_candidate));
        }
        Err(error) if retryable_failure(error) => {
            health.record_failure(&health_key(&first_candidate))
        }
        _ => {}
    }
    attempt.map(|(status, headers, body)| (status, headers, body, upstream))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inference_policy_loader::{ModelPreference, ModelRef};

    fn deployments(candidates: &[Candidate]) -> Vec<&str> {
        candidates
            .iter()
            .map(|candidate| candidate.deployment.as_str())
            .collect()
    }

    fn upstream(dep: &str) -> UpstreamConfig {
        UpstreamConfig {
            endpoint: "https://example.openai.azure.com".into(),
            deployment: dep.to_string(),
            sandbox_name: "sbx".into(),
            provider: crate::provider::ProviderKind::AzureOpenAI,
            api_key: None,
            provider_api_key: None,
            authentication: AuthenticationProvenance::LegacyDefault,
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
        assert_eq!(deployments(&c), vec!["primary", "fb-a", "fb-b", "default"]);
    }

    #[test]
    fn build_candidates_dedups_overlap() {
        let snap = snapshot_with("primary", &["primary", "fb-a"]);
        let c = build_candidates(&upstream("primary"), &snap);
        assert_eq!(deployments(&c), vec!["primary", "fb-a", "primary"]);
    }

    #[test]
    fn legacy_metadata_and_explicit_native_fallback_are_distinct_routing_intents() {
        let snapshot = InferencePolicySnapshot {
            model_preference: Some(ModelPreference {
                primary: ModelRef {
                    provider: "anthropic".into(),
                    deployment: "claude-prod".into(),
                },
                fallback: vec![
                    ModelRef {
                        provider: "anthropic".into(),
                        deployment: "claude-prod".into(),
                    },
                    ModelRef {
                        provider: "anthropic".into(),
                        deployment: "claude-prod".into(),
                    },
                ],
            }),
            ..Default::default()
        };
        let candidates = build_candidates(&upstream("default"), &snapshot);
        assert_eq!(candidates.len(), 3);
        assert_eq!(candidates[0].routing_intent, RoutingIntent::Metadata);
        assert_eq!(candidates[1].routing_intent, RoutingIntent::Explicit);
        assert_eq!(candidates[2].provider, None);
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
}
