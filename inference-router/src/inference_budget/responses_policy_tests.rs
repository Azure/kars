// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::{
    config::Config,
    inference_policy_loader::{LoadedInferencePolicy, ModelPreference, ModelRef},
    providers::{PolicyDecisionProvider, PolicyRequest, PolicyVerdict, policy::PolicyError},
    routes::AppState,
};
use axum::{Router, body::Body, http::StatusCode};
use serde_json::Value;
use tower::ServiceExt;
use wiremock::matchers::{body_partial_json, method, path};

const MODES: [&str; 3] = ["none", "finite", "record-only"];

async fn setup(
    mode: &str,
    default: &str,
    primary: Option<&str>,
    fallback: &[&str],
) -> (Fixture, AppState) {
    let fixture = if mode == "record-only" {
        Fixture::record_only(false, Operation::Responses).await
    } else {
        Fixture::for_operation(1000, false, Operation::Responses).await
    };
    let endpoint = format!("{}/openai/v1", fixture.provider.uri());
    let mut catalog = fixture.client.catalog().await.unwrap();
    if let Some(contract) = catalog.contracts.first().cloned() {
        catalog.contracts = ["allowed", "denied", "other"]
            .into_iter()
            .map(|model| ModelContract {
                id: model.into(),
                provider_id: "azure-openai".into(),
                endpoint: endpoint.clone(),
                model: model.into(),
                ..contract.clone()
            })
            .collect();
    }
    fixture._broker.reset().await;
    Mock::given(method("POST"))
        .respond_with(BrokerFixture {
            ledger: fixture.ledger.clone(),
            catalog,
            lose_begin_ack: false,
        })
        .mount(&fixture._broker)
        .await;
    let mut config = Config::from_env().unwrap();
    config.azure_openai_endpoint = Some(endpoint);
    config.default_model = default.into();
    config.provider_override = None;
    config.providers.clear();
    config.content_safety_enabled = false;
    config.prompt_shields_enabled = false;
    let mut state = crate::routes::test_state(config);
    state.auth = Arc::new(WorkloadIdentityAuth::for_test(
        Some("responses-test-key"),
        None,
    ));
    state.client = reqwest::Client::builder().no_proxy().build().unwrap();
    state.sandbox_name = Arc::new("task".into());
    state.inference_budget = (mode != "none").then(|| fixture.client.clone());
    let model_ref = |deployment: &str| ModelRef {
        provider: String::new(),
        deployment: deployment.into(),
    };
    *state.inference_policy.write().await = Some(LoadedInferencePolicy {
        digest: "responses-policy-test".into(),
        source_path: "responses-policy-test".into(),
        per_request_tokens: None,
        daily_tokens: None,
        monthly_tokens: None,
        content_safety: Default::default(),
        model_preference: primary.map(|deployment| ModelPreference {
            primary: model_ref(deployment),
            fallback: fallback
                .iter()
                .map(|deployment| model_ref(deployment))
                .collect(),
        }),
        provider: None,
        guardrails: vec![],
        raw: json!({}),
    });
    let policies = tempfile::tempdir().unwrap();
    std::fs::write(
        policies.path().join("policy.yaml"),
        r#"version: "1.0"
agent: task
policies:
  - name: deny-deployment
    type: capability
    denied_actions:
      - "inference:responses:denied"
    priority: 100
  - name: allow-inference
    type: capability
    allowed_actions:
      - "inference:*"
    priority: 1
"#,
    )
    .unwrap();
    assert!(
        state
            .governance
            .load_policies_from_dir(policies.path().to_str().unwrap())
            .unwrap()
            > 0
    );
    assert_eq!(
        state
            .governance
            .evaluate("task", "inference:responses:", None)["allowed"],
        true
    );
    assert_eq!(
        state
            .governance
            .evaluate("task", "inference:responses:denied", None)["allowed"],
        false
    );
    (fixture, state)
}

async fn post(state: AppState, body: impl Into<Body>) -> (StatusCode, Value) {
    let response = Router::new()
        .merge(crate::routes::inference_routes())
        .with_state(state)
        .oneshot(
            axum::http::Request::post("/v1/responses")
                .header("content-type", "application/json")
                .body(body.into())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 65_536)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

fn completed() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "id": "resp_test", "status": "completed", "output": [],
        "usage": {"input_tokens": 2, "output_tokens": 3, "total_tokens": 5}
    }))
}

fn unhealthy(state: &AppState, deployments: &[&str]) {
    for deployment in deployments {
        for _ in 0..3 {
            state.deployment_health.record_failure(deployment);
        }
        assert!(!state.deployment_health.is_healthy(deployment));
    }
}

#[tokio::test]
async fn responses_denies_default_override_and_explicit_models_before_admission() {
    for mode in MODES {
        for (default, primary, body) in [
            ("denied", None, json!({"input":"hello"})),
            (
                "allowed",
                Some("denied"),
                json!({"model":"allowed","input":"hello"}),
            ),
            (
                "allowed",
                Some("allowed"),
                json!({"model":"denied","input":"hello"}),
            ),
        ] {
            let (fixture, state) = setup(mode, default, primary, &[]).await;
            let (status, error) = post(state, body.to_string()).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{mode}: {error}");
            assert_eq!(error["error"]["code"], "policy_denied");
            assert!(
                fixture
                    .provider
                    .received_requests()
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert!(
                fixture
                    ._broker
                    .received_requests()
                    .await
                    .unwrap()
                    .is_empty()
            );
        }
    }
}

#[tokio::test]
async fn responses_rejects_invalid_json_objects_and_model_types_before_dispatch() {
    for mode in MODES {
        let (fixture, state) = setup(mode, "allowed", None, &[]).await;
        let mut cases = vec![
            ("not-json-private".to_owned(), "invalid_json"),
            ("[]".into(), "invalid_json"),
            ("null".into(), "invalid_json"),
            ("\"private-input\"".into(), "invalid_json"),
            (
                format!(
                    "{{\"input\":\"hello\",\"padding\":{}0{}}}",
                    "[".repeat(140),
                    "]".repeat(140)
                ),
                "invalid_json",
            ),
        ];
        for model in [
            json!(null),
            json!(42),
            json!({}),
            json!([]),
            json!(true),
            json!(""),
            json!(" \t"),
        ] {
            cases.push((
                json!({"model": model, "input":"hello"}).to_string(),
                "invalid_model",
            ));
        }
        for (body, code) in cases {
            let (status, error) = post(state.clone(), body).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{mode}: {error}");
            assert_eq!(error["error"]["code"], code);
            assert!(!error.to_string().contains("private"));
        }
        assert!(
            fixture
                .provider
                .received_requests()
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            fixture
                ._broker
                .received_requests()
                .await
                .unwrap()
                .is_empty()
        );
    }
}

#[tokio::test]
async fn responses_denied_fallback_is_never_sent_on_retry_health_skip_or_safety_net() {
    for mode in MODES {
        for case in [
            "retry",
            "health-skip",
            "all-unhealthy",
            "default-safety-net",
        ] {
            let default = if case == "default-safety-net" {
                "denied"
            } else {
                "other"
            };
            let primary = if case == "all-unhealthy" {
                "denied"
            } else {
                "allowed"
            };
            let fallback = if case == "default-safety-net" {
                vec![]
            } else {
                vec!["denied"]
            };
            let (fixture, state) = setup(mode, default, Some(primary), &fallback).await;
            if case == "health-skip" {
                unhealthy(&state, &["allowed"]);
            } else if case == "all-unhealthy" {
                unhealthy(&state, &["denied", "other"]);
            }
            let sends = usize::from(matches!(case, "retry" | "default-safety-net"));
            Mock::given(path("/openai/v1/responses"))
                .and(body_partial_json(json!({"model":"allowed"})))
                .respond_with(ResponseTemplate::new(503).set_body_json(json!({"error":"retry"})))
                .expect(sends as u64)
                .mount(&fixture.provider)
                .await;
            let (status, error) = post(state, json!({"input":"hello"}).to_string()).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{mode}/{case}: {error}");
            assert_eq!(error["error"]["code"], "policy_denied");
            let requests = fixture.provider.received_requests().await.unwrap();
            assert_eq!(requests.len(), sends, "{mode}/{case}");
            assert!(
                requests
                    .iter()
                    .all(|request| request.body_json::<Value>().unwrap()["model"] == "allowed")
            );
        }
    }
}

#[tokio::test]
async fn responses_allows_authorized_defaults_and_does_not_preemptively_deny_unused_fallbacks() {
    for mode in MODES {
        for body in [
            json!({"input":"hello"}),
            json!({"model":"allowed","input":"hello"}),
        ] {
            let (fixture, state) = setup(mode, "denied", Some("allowed"), &["denied"]).await;
            Mock::given(path("/openai/v1/responses"))
                .and(body_partial_json(json!({"model":"allowed"})))
                .respond_with(completed())
                .expect(1)
                .mount(&fixture.provider)
                .await;
            let (status, response) = post(state, body.to_string()).await;
            assert_eq!(status, StatusCode::OK, "{mode}: {response}");
            assert_eq!(response["status"], "completed");
            assert_eq!(fixture.provider.received_requests().await.unwrap().len(), 1);
            if mode != "none" {
                assert_eq!(fixture.ledger.lock().unwrap().meters.settled.tokens, 5);
            }
        }
        let (fixture, state) = setup(mode, "allowed", None, &[]).await;
        Mock::given(body_partial_json(json!({"model":"allowed"})))
            .respond_with(completed())
            .expect(1)
            .mount(&fixture.provider)
            .await;
        let (status, response) = post(state, json!({"input":"hello"}).to_string()).await;
        assert_eq!(status, StatusCode::OK, "{mode}: {response}");
    }
}

#[tokio::test]
async fn responses_still_fails_over_between_authorized_deployments() {
    for mode in MODES {
        let (fixture, state) = setup(mode, "other", Some("allowed"), &["other"]).await;
        Mock::given(body_partial_json(json!({"model":"allowed"})))
            .respond_with(ResponseTemplate::new(503).set_body_json(json!({"error":"retry"})))
            .expect(1)
            .mount(&fixture.provider)
            .await;
        Mock::given(body_partial_json(json!({"model":"other"})))
            .respond_with(completed())
            .expect(1)
            .mount(&fixture.provider)
            .await;
        let (status, response) = post(state, json!({"input":"hello"}).to_string()).await;
        assert_eq!(status, StatusCode::OK, "{mode}: {response}");
        let requests = fixture.provider.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[0].body_json::<Value>().unwrap()["model"],
            "allowed"
        );
        assert_eq!(requests[1].body_json::<Value>().unwrap()["model"], "other");
    }
}

struct UnavailablePolicy(bool);

#[async_trait::async_trait]
impl PolicyDecisionProvider for UnavailablePolicy {
    async fn decide(&self, _: PolicyRequest) -> std::result::Result<PolicyVerdict, PolicyError> {
        if self.0 {
            Ok(PolicyVerdict::NeedsApproval {
                approver: "reviewer".into(),
                ttl: std::time::Duration::from_secs(60),
            })
        } else {
            Err(PolicyError::Unreachable("private-backend-address".into()))
        }
    }
}

#[tokio::test]
async fn responses_candidate_approval_or_provider_failure_fails_closed() {
    for mode in MODES {
        for approval in [false, true] {
            let (fixture, mut state) = setup(mode, "allowed", None, &[]).await;
            state.policy_provider = Arc::new(UnavailablePolicy(approval));
            let (status, error) = post(state, json!({"input":"hello"}).to_string()).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{mode}: {error}");
            assert_eq!(error["error"]["code"], "policy_denied");
            assert!(!error.to_string().contains("private-backend-address"));
            assert!(
                fixture
                    .provider
                    .received_requests()
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert!(
                fixture
                    ._broker
                    .received_requests()
                    .await
                    .unwrap()
                    .is_empty()
            );
        }
    }
}
