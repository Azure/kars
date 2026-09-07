// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::AppState;
use crate::{
    failover,
    inference_policy_loader::InferencePolicySnapshot,
    proxy::{self, UpstreamConfig},
};
use anyhow::Result;
use axum::http::{HeaderMap, Method, StatusCode};
use bytes::Bytes;
use futures::{StreamExt, TryStreamExt, stream::BoxStream};

type BufferedResult = (StatusCode, HeaderMap, Bytes, UpstreamConfig);
type StreamingResult = (
    StatusCode,
    HeaderMap,
    BoxStream<'static, Result<Bytes, reqwest::Error>>,
    UpstreamConfig,
);

pub(super) fn model_capability_key(upstream: &UpstreamConfig) -> String {
    // Config is immutable for this AppState. Its explicit provider ID separates
    // accounts even when endpoint/model match; credentials never enter keys.
    serde_json::to_string(&(
        &upstream.authentication,
        upstream.endpoint.trim_end_matches('/'),
        &upstream.deployment,
    ))
    .expect("provider capability key always serializes")
}

pub(super) fn is_responses_only_error(body: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return false;
    };
    let error = value.get("error").unwrap_or(&value);
    let code = error
        .get("code")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let message = error
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    code.eq_ignore_ascii_case("unsupported_api_for_model")
        || message.contains("not accessible via the /chat/completions endpoint")
        || (message.contains("unsupported")
            && (message.contains("chat") || message.contains("api")))
}

fn is_model_unavailable_error(status: StatusCode, body: &[u8]) -> bool {
    if !matches!(status, StatusCode::BAD_REQUEST | StatusCode::NOT_FOUND)
        || is_responses_only_error(body)
    {
        return false;
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return false;
    };
    let error = value.get("error").unwrap_or(&value);
    let code = error
        .get("code")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let message = error
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    matches!(
        code.as_str(),
        "deploymentnotfound" | "model_not_found" | "model_not_supported" | "invalid_model"
    ) || ((message.contains("model") || message.contains("deployment"))
        && [
            "does not exist",
            "not found",
            "unknown model",
            "invalid model",
            "not supported",
            "is not a valid model",
            "no deployment",
        ]
        .iter()
        .any(|phrase| message.contains(phrase)))
}

pub(super) fn override_model_in_body(body: &[u8], model: &str) -> Bytes {
    match serde_json::from_slice::<serde_json::Value>(body) {
        Ok(mut value) if value.is_object() => {
            value["model"] = model.into();
            serde_json::to_vec(&value)
                .map(Bytes::from)
                .unwrap_or_else(|_| Bytes::copy_from_slice(body))
        }
        _ => Bytes::copy_from_slice(body),
    }
}

fn default_target(state: &AppState, base: &UpstreamConfig) -> UpstreamConfig {
    let mut target = base.clone();
    target.deployment = state.config.default_model.clone();
    target
}

fn primary_target(
    state: &AppState,
    base: &UpstreamConfig,
    policy: &InferencePolicySnapshot,
    body: &[u8],
) -> Result<UpstreamConfig> {
    let candidates = failover::candidates_for_request(base, policy, body);
    failover::resolve_candidate(base, &state.config, &candidates[0])
        .map_err(proxy::failure::ForwardFailure::configuration)
}

fn cached_unavailable(
    state: &AppState,
    selected: &UpstreamConfig,
    fallback: &UpstreamConfig,
) -> bool {
    model_capability_key(selected) != model_capability_key(fallback)
        && state
            .unavailable_models
            .read()
            .is_ok_and(|cache| cache.contains(&model_capability_key(selected)))
}

fn remember_unavailable(state: &AppState, selected: &UpstreamConfig) {
    if let Ok(mut cache) = state.unavailable_models.write() {
        cache.insert(model_capability_key(selected));
    }
}

pub(super) fn effective_primary(
    state: &AppState,
    base: &UpstreamConfig,
    policy: &InferencePolicySnapshot,
    body: &[u8],
) -> Result<UpstreamConfig> {
    let primary = primary_target(state, base, policy, body)?;
    let fallback = default_target(state, base);
    if cached_unavailable(state, &primary, &fallback) {
        Ok(fallback)
    } else {
        Ok(primary)
    }
}

/// Recovery starts with the provider that actually answered, not the original
/// primary that may already have failed. Remaining policy fallbacks still apply.
pub(super) async fn forward_responses(
    state: &AppState,
    base: &UpstreamConfig,
    policy: &InferencePolicySnapshot,
    selected: &UpstreamConfig,
    headers: &HeaderMap,
    body: Bytes,
) -> Result<BufferedResult> {
    use crate::inference_policy_loader::{ModelPreference, ModelRef};
    let candidates = failover::build_candidates(base, policy);
    let mut start = None;
    for (index, candidate) in candidates.iter().enumerate() {
        let target = failover::resolve_candidate(base, &state.config, candidate)
            .map_err(proxy::failure::ForwardFailure::configuration)?;
        if model_capability_key(&target) == model_capability_key(selected) {
            start = Some(index);
            break;
        }
    }
    if let Some(start) = start {
        let models: Vec<_> = candidates[start..]
            .iter()
            .map(|candidate| ModelRef {
                provider: candidate.provider.clone().unwrap_or_default(),
                deployment: candidate.deployment.clone(),
            })
            .collect();
        let mut response_policy = policy.clone();
        response_policy.provider = candidates[start].provider.clone();
        response_policy.model_preference = Some(ModelPreference {
            primary: models[0].clone(),
            fallback: models[1..].to_vec(),
        });
        return failover::forward_with_failover(
            &state.auth,
            Some(&state.copilot),
            &state.client,
            &state.deployment_health,
            base,
            &state.config,
            &response_policy,
            Method::POST,
            "responses",
            headers,
            body,
        )
        .await;
    }
    proxy::forward(
        &state.auth,
        Some(&state.copilot),
        &state.client,
        selected,
        Method::POST,
        "responses",
        headers,
        override_model_in_body(&body, &selected.deployment),
    )
    .await
    .map(|(status, headers, bytes)| (status, headers, bytes, selected.clone()))
}

pub(super) async fn forward_chat(
    state: &AppState,
    base: &UpstreamConfig,
    policy: &InferencePolicySnapshot,
    headers: &HeaderMap,
    body: Bytes,
) -> Result<BufferedResult> {
    let fallback = default_target(state, base);
    let primary = primary_target(state, base, policy, &body)?;
    let result = if cached_unavailable(state, &primary, &fallback) {
        proxy::forward(
            &state.auth,
            Some(&state.copilot),
            &state.client,
            &fallback,
            Method::POST,
            "chat/completions",
            headers,
            override_model_in_body(&body, &fallback.deployment),
        )
        .await
        .map(|(status, headers, bytes)| (status, headers, bytes, fallback.clone()))
    } else {
        failover::forward_with_failover(
            &state.auth,
            Some(&state.copilot),
            &state.client,
            &state.deployment_health,
            base,
            &state.config,
            policy,
            Method::POST,
            "chat/completions",
            headers,
            body.clone(),
        )
        .await
    };
    if let Ok((status, _, response, selected)) = &result
        && is_model_unavailable_error(*status, response)
        && !fallback.deployment.is_empty()
        && model_capability_key(selected) != model_capability_key(&fallback)
    {
        remember_unavailable(state, selected);
        return proxy::forward(
            &state.auth,
            Some(&state.copilot),
            &state.client,
            &fallback,
            Method::POST,
            "chat/completions",
            headers,
            override_model_in_body(&body, &fallback.deployment),
        )
        .await
        .map(|(status, headers, bytes)| (status, headers, bytes, fallback));
    }
    result
}

pub(super) async fn forward_stream_chat(
    state: &AppState,
    base: &UpstreamConfig,
    policy: &InferencePolicySnapshot,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StreamingResult> {
    let fallback = default_target(state, base);
    let primary = primary_target(state, base, policy, &body)?;
    let (status, response_headers, stream, selected) =
        if cached_unavailable(state, &primary, &fallback) {
            proxy::forward_stream(
                state.auth.clone(),
                Some(state.copilot.clone()),
                state.client.clone(),
                fallback.clone(),
                "chat/completions",
                headers.clone(),
                override_model_in_body(&body, &fallback.deployment),
            )
            .await
            .map(|(status, headers, stream)| (status, headers, stream, fallback.clone()))?
        } else {
            failover::forward_stream_with_failover(
                state.auth.clone(),
                Some(state.copilot.clone()),
                state.client.clone(),
                &state.deployment_health,
                base,
                &state.config,
                policy,
                "chat/completions",
                headers.clone(),
                body.clone(),
            )
            .await?
        };
    if !matches!(status, StatusCode::BAD_REQUEST | StatusCode::NOT_FOUND) {
        return Ok((status, response_headers, stream, selected));
    }
    let bytes = stream
        .try_fold(Vec::new(), |mut bytes, chunk| async move {
            bytes.extend_from_slice(&chunk);
            Ok(bytes)
        })
        .await?;
    if is_model_unavailable_error(status, &bytes)
        && !fallback.deployment.is_empty()
        && model_capability_key(&selected) != model_capability_key(&fallback)
    {
        remember_unavailable(state, &selected);
        return proxy::forward_stream(
            state.auth.clone(),
            Some(state.copilot.clone()),
            state.client.clone(),
            fallback.clone(),
            "chat/completions",
            headers,
            override_model_in_body(&body, &fallback.deployment),
        )
        .await
        .map(|(status, headers, stream)| (status, headers, stream, fallback));
    }
    Ok((
        status,
        response_headers,
        futures::stream::once(async move { Ok(Bytes::from(bytes)) }).boxed(),
        selected,
    ))
}

#[cfg(test)]
#[path = "model_routing_regressions.rs"]
mod regressions;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_partial_json, header, path},
    };

    pub(super) fn test_state(config: crate::config::Config) -> AppState {
        let policy_status = Arc::new(crate::policy_status::PolicyStatusRegistry::new());
        let governance = Arc::new(crate::governance::Governance::new_with_status(
            "test",
            policy_status.clone(),
        ));
        AppState {
            auth: Arc::new(crate::auth::WorkloadIdentityAuth::new()),
            copilot: Arc::new(crate::copilot_auth::CopilotTokenCache::from_env()),
            client: reqwest::Client::new(),
            config: Arc::new(config),
            budget: crate::budget::TokenBudgetTracker::new(0, 0),
            policy_provider: governance.clone(),
            audit_sink: governance.clone(),
            signing_provider: governance.clone(),
            governance,
            blocklist: crate::blocklist::Blocklist::disabled(),
            blocked_egress: Arc::new(crate::egress_blocked::BlockedBuffer::with_defaults()),
            sandbox_name: Arc::new("test".into()),
            inbox: Arc::new(crate::mesh::MeshInbox::new()),
            mesh_metrics: Arc::new(crate::mesh::MeshMetrics::new()),
            model_override: Default::default(),
            responses_only_models: Default::default(),
            unavailable_models: Default::default(),
            admin_token: None,
            handoff_tokens: crate::handoff::HandoffTokenStore::new(),
            handoff_session: crate::handoff::HandoffSession::new(),
            drain_state: crate::handoff::DrainState::new(),
            pending_handoff: crate::handoff::PendingHandoffStore::new(),
            policy_status,
            inference_policy: crate::inference_policy_loader::empty_handle(),
            memory_binding: crate::memory_binding_loader::empty_handle(),
            egress_allowlist: crate::egress_allowlist_loader::empty_handle(),
            deployment_health: Arc::new(crate::deployment_health::DeploymentHealthRegistry::new()),
        }
    }

    #[tokio::test]
    async fn unavailable_provider_model_recovers_once_then_uses_scoped_cache() {
        use crate::{
            config::{Config, ProviderEndpoint},
            inference_policy_loader::{ModelPreference, ModelRef},
        };
        let primary = MockServer::start().await;
        let default = MockServer::start().await;
        Mock::given(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(404)
                    .set_body_json(json!({"error":{"code":"model_not_found"}})),
            )
            .expect(1)
            .mount(&primary)
            .await;
        Mock::given(header("authorization", "Bearer default-key"))
            .and(body_partial_json(json!({"model":"default-model"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"choices":[]})))
            .expect(2)
            .mount(&default)
            .await;
        let mut config = Config::from_env().unwrap();
        config.default_model = "default-model".into();
        config.providers.insert(
            "primary".into(),
            ProviderEndpoint {
                tag: "primary".into(),
                endpoint: primary.uri(),
                api_key: Some("primary-key".into()),
            },
        );
        let state = test_state(config);
        let mut base = UpstreamConfig::azure(default.uri(), "default-model".into(), "test".into());
        base.provider_api_key = Some("default-key".into());
        let policy = InferencePolicySnapshot {
            model_preference: Some(ModelPreference {
                primary: ModelRef {
                    provider: "primary".into(),
                    deployment: "missing-model".into(),
                },
                fallback: vec![],
            }),
            ..Default::default()
        };
        let (_, _, _, selected) =
            forward_chat(&state, &base, &policy, &HeaderMap::new(), Bytes::from("{}"))
                .await
                .unwrap();
        assert_eq!(selected.endpoint, default.uri());
        let (status, _, stream, selected) =
            forward_stream_chat(&state, &base, &policy, HeaderMap::new(), Bytes::from("{}"))
                .await
                .unwrap();
        assert_eq!(status, StatusCode::OK);
        assert_eq!(selected.deployment, "default-model");
        let _: Vec<_> = stream.try_collect().await.unwrap();
        assert_eq!(state.unavailable_models.read().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn responses_recovery_starts_at_the_provider_that_answered() {
        use crate::{
            config::{Config, ProviderEndpoint},
            inference_policy_loader::{ModelPreference, ModelRef},
        };
        let first = MockServer::start().await;
        let second = MockServer::start().await;
        Mock::given(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(503))
            .expect(1)
            .mount(&first)
            .await;
        Mock::given(path("/responses"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&first)
            .await;
        Mock::given(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_json(json!({"error":{"code":"unsupported_api_for_model"}})),
            )
            .expect(1)
            .mount(&second)
            .await;
        Mock::given(path("/responses")).and(header("authorization", "Bearer second-key"))
            .and(body_partial_json(json!({"model":"second-model"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"actual deliverable"}]}]})))
            .expect(1).mount(&second).await;
        let mut config = Config::from_env().unwrap();
        for (tag, endpoint) in [("first", first.uri()), ("second", second.uri())] {
            config.providers.insert(
                tag.into(),
                ProviderEndpoint {
                    tag: tag.into(),
                    endpoint,
                    api_key: Some(format!("{tag}-key")),
                },
            );
        }
        let state = test_state(config);
        let mut base = UpstreamConfig::azure(first.uri(), "default".into(), "test".into());
        base.provider_api_key = Some("default-key".into());
        let policy = InferencePolicySnapshot {
            model_preference: Some(ModelPreference {
                primary: ModelRef {
                    provider: "first".into(),
                    deployment: "first-model".into(),
                },
                fallback: vec![ModelRef {
                    provider: "second".into(),
                    deployment: "second-model".into(),
                }],
            }),
            ..Default::default()
        };
        let (_, _, _, selected) =
            forward_chat(&state, &base, &policy, &HeaderMap::new(), Bytes::from("{}"))
                .await
                .unwrap();
        assert_eq!(selected.endpoint, second.uri());
        let (_, _, body, winner) = forward_responses(
            &state,
            &base,
            &policy,
            &selected,
            &HeaderMap::new(),
            Bytes::from("{}"),
        )
        .await
        .unwrap();
        assert_eq!(winner.endpoint, second.uri());
        assert!(String::from_utf8_lossy(&body).contains("actual deliverable"));
    }

    #[test]
    fn model_errors_do_not_confuse_auth_policy_or_protocol_failures() {
        for code in ["DeploymentNotFound", "model_not_found", "invalid_model"] {
            let body = serde_json::to_vec(&json!({"error": {"code": code}})).unwrap();
            assert!(is_model_unavailable_error(StatusCode::NOT_FOUND, &body));
            assert!(!is_model_unavailable_error(StatusCode::FORBIDDEN, &body));
            assert!(!is_model_unavailable_error(
                StatusCode::TOO_MANY_REQUESTS,
                &body
            ));
        }
        let body = br#"{"error":{"code":"unsupported_api_for_model","message":"model not supported via chat API"}}"#;
        assert!(is_responses_only_error(body));
        assert!(!is_model_unavailable_error(StatusCode::BAD_REQUEST, body));
        assert!(!is_responses_only_error(
            br#"{"error":{"message":"unsupported tool argument"}}"#
        ));
    }

    #[test]
    fn capability_cache_separates_endpoints_and_models() {
        let a = UpstreamConfig::azure("https://a.example/v1".into(), "model".into(), "test".into());
        let mut b = a.clone();
        b.endpoint = "https://b.example/v1".into();
        assert_ne!(model_capability_key(&a), model_capability_key(&b));
        b = a.clone();
        b.deployment = "Model".into();
        assert_ne!(model_capability_key(&a), model_capability_key(&b));
    }
}
