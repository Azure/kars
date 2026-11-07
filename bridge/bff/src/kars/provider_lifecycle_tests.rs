// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

const POLICIES: &str = "/apis/kars.azure.com/v1alpha1/inferencepolicies";

#[tokio::test]
async fn provider_lifecycle_bootstrap_rejection_preserves_shared_credentials() {
    for sealed in [true, false] {
        let (cluster, state, server, path) = model_fixture().await;
        let ns = cluster.core_namespace();
        {
            let mut data = state.lock().unwrap();
            data.objects
                .get_mut(&format!("/api/v1/namespaces/{ns}"))
                .unwrap()["metadata"]["annotations"]["kars.azure.com/private-enabled"] =
                sealed.to_string().into();
            data.objects
                .get_mut(&format!(
                    "/apis/kars.azure.com/v1alpha1/namespaces/{ns}/karscredentialgrants/workspace"
                ))
                .unwrap()["spec"]["controller"] =
                json!({"name":"kars-controller","uid":"controller"});
        }
        let before = state.lock().unwrap().objects[&path].clone();
        let error = crate::routes::operator::put_provider(
            axum::extract::State(crate::state::AppState::for_test_client(
                cluster.client.clone(),
                &ns,
            )),
            axum::Json(crate::routes::operator::ProviderRequest {
                kind: "azure-openai".into(),
                auth: "api".into(),
                endpoint: Some("https://azure.example".into()),
                models: "astra,mini".into(),
                key: Some("must-not-save".into()),
            }),
        )
        .await
        .unwrap_err();
        assert!(matches!(
            error,
            crate::error::AppError::CredentialAuthorityUnavailable
        ));
        let data = state.lock().unwrap();
        assert_eq!(data.objects[&path], before);
        assert!(data.calls.iter().all(|(method, _, _)| method == "GET"));
        server.abort();
    }
}

#[tokio::test]
async fn provider_lifecycle_bootstrap_api_key_reuses_enrolled_shared_store() {
    let (cluster, state, server, path) = model_fixture().await;
    let ns = cluster.core_namespace();
    let settings_path =
        format!("/api/v1/namespaces/{ns}/secrets/kars-credential-controller-settings");
    {
        let mut data = state.lock().unwrap();
        data.objects
            .get_mut(&format!("/api/v1/namespaces/{ns}"))
            .unwrap()["metadata"]["annotations"]["kars.azure.com/private-enabled"] = "false".into();
        let grant = data
            .objects
            .get_mut(&format!(
                "/apis/kars.azure.com/v1alpha1/namespaces/{ns}/karscredentialgrants/workspace"
            ))
            .unwrap();
        grant["spec"]["controller"] = json!({"name":"kars-controller","uid":"controller"});
        grant["spec"]["integrationStores"].as_array_mut().unwrap().push(json!({
            "purpose":"controller-settings","secret":{"name":"kars-credential-controller-settings","uid":"settings"}
        }));
        data.objects.insert(settings_path.clone(), json!({
            "apiVersion":"v1","kind":"Secret","type":"Opaque",
            "metadata":{"name":"kars-credential-controller-settings","namespace":ns,"uid":"settings","resourceVersion":"1"},"data":{}
        }));
        data.objects.insert(format!("/apis/apps/v1/namespaces/{ns}/deployments/kars-controller"), json!({
            "apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":"kars-controller","namespace":ns,"uid":"controller"},
            "spec":{"selector":{"matchLabels":{"app":"controller"}},"template":{"spec":{"containers":[{"name":"controller","env":[
                {"name":"FOUNDRY_ENDPOINT","value":"https://azure.example"},
                {"name":"KARS_MODEL_CATALOG","value":"astra,mini"}
            ]}]}}}
        }));
    }
    let _ = crate::routes::operator::put_provider(
        axum::extract::State(crate::state::AppState::for_test_client(
            cluster.client.clone(),
            &ns,
        )),
        axum::Json(crate::routes::operator::ProviderRequest {
            kind: "azure-openai".into(),
            auth: "api".into(),
            endpoint: Some("https://azure.example".into()),
            models: "astra,mini".into(),
            key: Some("rotated-test-key".into()),
        }),
    )
    .await
    .unwrap();
    let keys = cluster.additional_provider_keys().await.unwrap();
    assert_eq!(
        keys["KARS_PROVIDER_AZURE_OPENAI_API_KEY"],
        "rotated-test-key"
    );
    assert_eq!(keys["COPILOT_GITHUB_TOKEN"], "preserved");
    let settings = cluster
        .read_secret_all(&ns, "kars-credential-controller-settings")
        .await
        .unwrap();
    let configuration: Vec<Value> = serde_json::from_str(&settings["configuration"]).unwrap();
    let key = configuration
        .iter()
        .find(|entry| entry["name"] == "AZURE_OPENAI_API_KEY")
        .unwrap();
    assert_eq!(
        key["secret"],
        json!({"name":"kars-inference-providers","uid":"registry","key":"KARS_PROVIDER_AZURE_OPENAI_API_KEY"})
    );
    {
        let data = state.lock().unwrap();
        assert!(
            data.calls
                .iter()
                .all(|(_, path, _)| !path.contains("/secrets/kars-provider-"))
        );
        assert!(
            data.calls
                .iter()
                .filter(|(method, _, _)| method != "GET")
                .all(|(method, target, _)| method == "PATCH"
                    && (target == &path || target == &settings_path))
        );
    }
    state.lock().unwrap().calls.clear();
    let error = cluster
        .mutate_secret_keys(&ns, "kars-inference-providers", |keys| {
            keys.retain(|key, _| !key.starts_with("KARS_PROVIDER_AZURE_OPENAI_"));
        })
        .await
        .unwrap_err();
    assert!(matches!(error, kube::Error::Api(response)
        if response.reason == "ProviderInUse" && response.message.contains("controller bootstrap")));
    assert!(
        state
            .lock()
            .unwrap()
            .calls
            .iter()
            .all(|(method, _, _)| method == "GET")
    );
    assert_eq!(
        cluster.additional_provider_keys().await.unwrap()["KARS_PROVIDER_AZURE_OPENAI_API_KEY"],
        "rotated-test-key"
    );
    server.abort();
}

fn remove_copilot(keys: &mut std::collections::BTreeMap<String, String>) {
    keys.retain(|key, _| {
        key != "COPILOT_GITHUB_TOKEN" && !key.starts_with("KARS_PROVIDER_GITHUB_COPILOT_")
    });
}

#[tokio::test]
async fn provider_lifecycle_rejects_default_primary_and_fallback_removal_without_writes() {
    use axum::{
        body::{Body, to_bytes},
        http::Request,
        routing::delete,
    };
    use tower::ServiceExt;
    for consumer in [
        "default",
        "bridge-orchestrator-inference",
        "sre-inference",
        "fallback",
    ] {
        let (cluster, state, server, path) = model_fixture().await;
        {
            let mut data = state.lock().unwrap();
            if consumer == "default" {
                data.objects.get_mut(&path).unwrap()["metadata"]["annotations"]["kars.azure.com/default-model"] =
                    json!({"provider":"github-copilot","deployment":"astra"})
                        .to_string()
                        .into();
            } else {
                let route = json!({"provider":"github-copilot","deployment":"astra"});
                let preference = if consumer == "fallback" {
                    json!({"primary":{"provider":"azure-openai","deployment":"mini"},"fallback":[route]})
                } else {
                    json!({"primary":route})
                };
                data.objects.get_mut(POLICIES).unwrap()["items"] = json!([{
                    "apiVersion":"kars.azure.com/v1alpha1","kind":"InferencePolicy",
                    "metadata":{"name":consumer,"namespace":"kars-system"},
                    "spec":{"modelPreference":preference}
                }]);
            }
        }
        let before = state.lock().unwrap().objects[&path].clone();
        let app = Router::new()
            .route(
                "/providers/{tag}",
                delete(crate::routes::operator::delete_additional_provider),
            )
            .with_state(crate::state::AppState::for_test_client(
                cluster.client.clone(),
                &cluster.core_namespace(),
            ));
        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/providers/github-copilot")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            axum::http::StatusCode::CONFLICT,
            "{consumer}"
        );
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        let message = body["error"]["message"].as_str().unwrap();
        assert!(message.contains(consumer), "{message}");
        assert!(message.contains("Policies"), "{message}");
        let data = state.lock().unwrap();
        assert_eq!(data.objects[&path], before);
        assert!(data.calls.iter().all(|(method, _, _)| method == "GET"));
        server.abort();
    }
}

#[tokio::test]
async fn provider_lifecycle_edits_preserve_used_models_keys_other_providers_and_defaults() {
    let (cluster, state, server, path) = model_fixture().await;
    state.lock().unwrap().objects.get_mut(&path).unwrap()["metadata"]["annotations"]["kars.azure.com/default-model"] =
        json!({"provider":"github-copilot","deployment":"astra"})
            .to_string()
            .into();
    let before = state.lock().unwrap().objects[&path].clone();
    // Removing an unused model and adding another is safe; leaving the key blank preserves it.
    cluster
        .mutate_secret_keys(
            &cluster.core_namespace(),
            "kars-inference-providers",
            |keys| {
                keys.insert(
                    "KARS_PROVIDER_GITHUB_COPILOT_MODELS".into(),
                    "astra,new-model".into(),
                );
            },
        )
        .await
        .unwrap();
    let error = cluster
        .mutate_secret_keys(
            &cluster.core_namespace(),
            "kars-inference-providers",
            |keys| {
                keys.insert(
                    "KARS_PROVIDER_GITHUB_COPILOT_MODELS".into(),
                    "new-model".into(),
                );
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(error, kube::Error::Api(response) if response.reason == "ProviderInUse"));
    let keys = cluster.additional_provider_keys().await.unwrap();
    assert_eq!(
        keys["KARS_PROVIDER_GITHUB_COPILOT_MODELS"],
        "astra,new-model"
    );
    assert_eq!(
        state.lock().unwrap().objects[&path]["data"]["COPILOT_GITHUB_TOKEN"],
        before["data"]["COPILOT_GITHUB_TOKEN"]
    );
    assert_eq!(
        state.lock().unwrap().objects[&path]["data"]["KARS_PROVIDER_AZURE_OPENAI_API_KEY"],
        before["data"]["KARS_PROVIDER_AZURE_OPENAI_API_KEY"]
    );
    assert_eq!(
        state.lock().unwrap().objects[&path]["metadata"]["annotations"],
        before["metadata"]["annotations"]
    );
    // An explicitly selected replacement permits removal; it never repoints consumers automatically.
    let (_, expected) = cluster
        .integration_store(&cluster.core_namespace(), "kars-inference-providers")
        .await
        .unwrap();
    cluster
        .write_model_default(
            &TaskModel {
                provider: "azure-openai".into(),
                deployment: "mini".into(),
            },
            &expected,
        )
        .await
        .unwrap();
    cluster
        .mutate_secret_keys(
            &cluster.core_namespace(),
            "kars-inference-providers",
            remove_copilot,
        )
        .await
        .unwrap();
    let keys = cluster.additional_provider_keys().await.unwrap();
    assert!(!keys.contains_key("COPILOT_GITHUB_TOKEN"));
    assert_eq!(keys["KARS_PROVIDER_AZURE_OPENAI_MODELS"], "astra,mini");
    assert_eq!(
        cluster.model_default().await.unwrap().unwrap().provider,
        "azure-openai"
    );
    server.abort();
}

#[tokio::test]
async fn provider_lifecycle_rechecks_default_after_secret_cas_conflict() {
    let (cluster, state, server, path) = model_fixture().await;
    state.lock().unwrap().target_read_mutation = Some((
        path.clone(),
        2,
        json!({
            "metadata":{"annotations":{"kars.azure.com/default-model":json!({"provider":"github-copilot","deployment":"astra"}).to_string()}}
        }),
    ));
    let before = state.lock().unwrap().objects[&path]["data"].clone();
    let error = cluster
        .mutate_secret_keys(
            &cluster.core_namespace(),
            "kars-inference-providers",
            remove_copilot,
        )
        .await
        .unwrap_err();
    assert!(matches!(error, kube::Error::Api(response) if response.reason == "ProviderInUse"));
    assert_eq!(state.lock().unwrap().objects[&path]["data"], before);
    assert!(
        state
            .lock()
            .unwrap()
            .calls
            .iter()
            .all(|(method, _, _)| method == "GET")
    );
    server.abort();
}

#[tokio::test]
async fn provider_lifecycle_fails_closed_when_policy_usage_cannot_be_read() {
    let (cluster, state, server, path) = model_fixture().await;
    state.lock().unwrap().objects.remove(POLICIES);
    let before = state.lock().unwrap().objects[&path].clone();
    let error = cluster
        .mutate_secret_keys(
            &cluster.core_namespace(),
            "kars-inference-providers",
            remove_copilot,
        )
        .await
        .unwrap_err();
    assert!(matches!(error, kube::Error::Api(response) if response.code == 404));
    let data = state.lock().unwrap();
    assert_eq!(data.objects[&path], before);
    assert!(data.calls.iter().all(|(method, _, _)| method == "GET"));
    server.abort();
}

#[tokio::test]
async fn provider_lifecycle_external_disconnect_preflights_before_destructive_work() {
    let (cluster, state, server, path) = model_fixture().await;
    for tag in ["foundry", "local-small"] {
        let prefix = format!(
            "KARS_PROVIDER_{}",
            tag.to_ascii_uppercase().replace('-', "_")
        );
        cluster
            .mutate_secret_keys(
                &cluster.core_namespace(),
                "kars-inference-providers",
                |keys| {
                    keys.insert(format!("{prefix}_ENDPOINT"), "https://example.test".into());
                    keys.insert(format!("{prefix}_MODELS"), "astra".into());
                },
            )
            .await
            .unwrap();
        state.lock().unwrap().objects.get_mut(&path).unwrap()["metadata"]["annotations"]["kars.azure.com/default-model"] =
            json!({"provider":tag,"deployment":"astra"})
                .to_string()
                .into();
        let before = state.lock().unwrap().objects[&path].clone();
        state.lock().unwrap().calls.clear();
        let error = cluster.require_provider_removable(tag).await.unwrap_err();
        assert!(matches!(error, kube::Error::Api(response) if response.reason == "ProviderInUse"));
        assert_eq!(state.lock().unwrap().objects[&path], before);
        assert!(
            state
                .lock()
                .unwrap()
                .calls
                .iter()
                .all(|(method, _, _)| method == "GET")
        );
    }
    server.abort();
}
