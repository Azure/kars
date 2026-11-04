// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use serde_json::{Value, json};

#[test]
fn model_capacity_rejects_missing_ambiguous_and_invalid_provider_metadata() {
    let upstream = UpstreamConfig::azure(
        "https://example.com".into(),
        "model".into(),
        "sandbox".into(),
    );
    for body in [
        json!({}),
        json!({"data": []}),
        json!({"data": [{"id":"other", "context_window":128000, "max_output_tokens":16000}]}),
        json!({"data": [{"id":"model", "context_window":128000}]}),
        json!({"data": [{"id":"model", "context_window":0, "max_output_tokens":16000}]}),
        json!({"data": [{"id":"model", "context_window":128000, "max_output_tokens":-1}]}),
        json!({"data": [{"id":"model", "context_window":"128000", "max_output_tokens":16000}]}),
        json!({"data": [{"id":"model", "context_window":9007199254740992u64, "max_output_tokens":16000}]}),
        json!({"data": [
            {"id":"model", "context_window":128000, "max_output_tokens":16000},
            {"id":"model", "context_window":128000, "max_output_tokens":16000}
        ]}),
    ] {
        assert!(selected_model_capacity(&body, &upstream).is_err());
    }
    assert_eq!(
        selected_model_capacity(
            &json!({"data":[{"id":"model", "context_window":128000, "max_output_tokens":16000}]}),
            &upstream
        )
        .unwrap(),
        (128000, 16000)
    );
}

#[test]
fn azure_model_capacity_uses_exact_documented_model_only_when_limits_are_absent() {
    let entry = json!({
        "id": "gpt-6.1-sol", "status": "succeeded",
        "capabilities": {"inference": true, "chat_completion": true},
        "lifecycle_status": "generally-available", "object": "model"
    });
    let body = json!({"data": [entry.clone()]});
    let mut upstream = UpstreamConfig::azure(
        "https://example.services.ai.azure.com".into(),
        "gpt-6.1-sol".into(),
        "sandbox".into(),
    );
    for host in [
        "example.services.ai.azure.com",
        "example.openai.azure.com",
        "example.cognitiveservices.azure.com",
    ] {
        upstream.endpoint = format!("https://{host}");
        assert_eq!(
            selected_model_capacity(&body, &upstream).unwrap(),
            (1_050_000, 128_000)
        );
    }
    for host in [
        "api.openai.com",
        "api.githubcopilot.com",
        "example.services.ai.azure.com.evil.example",
        "example.com",
    ] {
        upstream.endpoint = format!("https://{host}");
        assert!(selected_model_capacity(&body, &upstream).is_err());
    }
    upstream.endpoint = "https://example.services.ai.azure.com".into();
    for provider in [ProviderKind::Anthropic, ProviderKind::Ollama] {
        upstream.provider = provider;
        assert!(selected_model_capacity(&body, &upstream).is_err());
    }
    upstream.provider = ProviderKind::AzureOpenAI;
    for model in ["gpt-6-sol", "gpt-6.1-sol-alias", "gpt-6.1-sol-2026-09-29"] {
        upstream.deployment = model.into();
        let mut other = entry.clone();
        other["id"] = json!(model);
        assert!(selected_model_capacity(&json!({"data": [other]}), &upstream).is_err());
    }
    upstream.deployment = "gpt-6.1-sol".into();
    for bad in [
        json!({"data": []}),
        json!({"data": [entry.clone(), entry.clone()]}),
    ] {
        assert!(selected_model_capacity(&bad, &upstream).is_err());
    }
    for (field, value) in [
        ("status", json!("failed")),
        ("capabilities", json!({"inference": false})),
    ] {
        let mut unavailable = entry.clone();
        unavailable[field] = value;
        assert!(selected_model_capacity(&json!({"data": [unavailable]}), &upstream).is_err());
    }
    for field in ["context_window", "max_output_tokens"] {
        for value in [
            json!(null),
            json!(0),
            json!(-1),
            json!("128000"),
            json!(9007199254740992u64),
            json!(128000),
        ] {
            let mut partial_or_invalid = entry.clone();
            partial_or_invalid[field] = value;
            assert!(
                selected_model_capacity(&json!({"data": [partial_or_invalid]}), &upstream).is_err()
            );
        }
    }
    for capacities in [
        json!({"context_window": 256000, "max_output_tokens": 32000}),
        json!({"capabilities": {"limits": {"max_context_window_tokens": 256000, "max_output_tokens": 32000}}}),
    ] {
        let mut reported = entry.clone();
        reported
            .as_object_mut()
            .unwrap()
            .extend(capacities.as_object().unwrap().clone());
        assert_eq!(
            selected_model_capacity(&json!({"data": [reported]}), &upstream).unwrap(),
            (256000, 32000)
        );
    }
    let mut invalid = entry;
    invalid["capabilities"]["limits"] =
        json!({"max_context_window_tokens": 0, "max_output_tokens": 128000});
    assert!(selected_model_capacity(&json!({"data": [invalid]}), &upstream).is_err());
}

fn wire(endpoint: &str, path: &str, body: &Value) -> Value {
    let upstream =
        UpstreamConfig::azure(endpoint.into(), "claude-opus-5.5".into(), "sandbox".into());
    let (_, body) = build_upstream_url(
        &WorkloadIdentityAuth::new(),
        &upstream,
        path,
        serde_json::to_vec(body).unwrap().into(),
    )
    .unwrap();
    serde_json::from_slice(&body).unwrap()
}

fn title_request() -> Value {
    json!({
        "model": "claude-opus-5.5",
        "messages": [{"role": "user", "content": "Name this SRE session."}],
        "max_tokens": 512,
        "reasoning_effort": "none",
        "response_format": {
            "type": "json_schema",
            "json_schema": {
                "name": "session_title", "strict": true,
                "schema": {
                    "type": "object", "properties": {"title": {"type": "string"}},
                    "required": ["title"], "additionalProperties": false
                }
            }
        }
    })
}

#[test]
fn copilot_claude_preserves_title_schema_and_explicit_output_bound() {
    for stream in [false, true] {
        for cap in ["max_tokens", "max_completion_tokens"] {
            let mut request = title_request();
            request.as_object_mut().unwrap().remove("max_tokens");
            request[cap] = json!(512);
            request["stream"] = json!(stream);
            let actual = wire(
                "https://api.githubcopilot.com",
                "/chat/completions",
                &request,
            );
            request.as_object_mut().unwrap().remove("reasoning_effort");
            assert_eq!(actual, request);
        }
    }
}

#[test]
fn copilot_claude_normalization_is_idempotent_and_uses_final_model() {
    let mut request = title_request();
    request.as_object_mut().unwrap().remove("model");
    let actual = wire(
        "https://api.githubcopilot.com",
        "chat/completions",
        &request,
    );
    assert_eq!(actual["model"], "claude-opus-5.5");
    assert!(actual.get("reasoning_effort").is_none());
    assert_eq!(
        wire("https://api.githubcopilot.com", "chat/completions", &actual),
        actual
    );
}

#[test]
fn other_hosts_models_and_endpoints_keep_their_reasoning_contract() {
    for endpoint in [
        "https://example.openai.azure.com",
        "https://api.openai.com/v1",
        "https://models.github.ai/inference",
        "https://api.githubcopilot.com.evil.example",
    ] {
        let request = title_request();
        assert_eq!(wire(endpoint, "chat/completions", &request), request);
    }
    for model in ["gpt-5.4-mini", "o3", "not-claude-opus-5.5"] {
        let mut request = title_request();
        request["model"] = json!(model);
        assert_eq!(
            wire(
                "https://api.githubcopilot.com",
                "chat/completions",
                &request
            ),
            request
        );
    }
    for path in ["responses", "messages", "embeddings"] {
        let request = title_request();
        assert_eq!(
            wire("https://api.githubcopilot.com", path, &request),
            request
        );
    }
}

#[test]
fn enabled_malformed_and_conflicting_controls_are_not_silently_removed() {
    for effort in [
        json!("low"),
        json!("medium"),
        json!("high"),
        json!("max"),
        json!(null),
        json!({"enabled": false}),
    ] {
        let mut request = title_request();
        request["reasoning_effort"] = effort;
        assert_eq!(
            wire(
                "https://api.githubcopilot.com",
                "chat/completions",
                &request
            ),
            request
        );
    }
    for field in ["thinking", "reasoning"] {
        let mut request = title_request();
        request[field] = json!({"type": "enabled"});
        assert_eq!(
            wire(
                "https://api.githubcopilot.com",
                "chat/completions",
                &request
            ),
            request
        );
    }
}
