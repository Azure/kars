// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use axum::{
    body::{Body, to_bytes},
    http::Request,
    routing::{get, put},
};
use tower::ServiceExt;

#[tokio::test]
async fn component_models_use_governed_selection_preserve_fallbacks_and_reject_stale_writes() {
    let (cluster, state, server, registry) = model_fixture().await;
    let path =
        "/apis/kars.azure.com/v1alpha1/namespaces/kars-system/inferencepolicies/sre-inference";
    {
        let mut data = state.lock().unwrap();
        data.objects.get_mut(&registry).unwrap()["metadata"]["annotations"]["kars.azure.com/default-model"] =
            r#"{"provider":"github-copilot","deployment":"astra"}"#.into();
        data.objects.insert(path.into(),json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"InferencePolicy","metadata":{"name":"sre-inference","namespace":"kars-system","uid":"policy","resourceVersion":"1","generation":1},"spec":{"appliesTo":{"sandboxName":"sre"},"tokenBudget":{"dailyTokens":42},"modelPreference":{"primary":{"provider":"azure-openai","deployment":"mini"},"fallback":[{"provider":"azure-openai","deployment":"astra"}]}}}));
    }
    let app = Router::new()
        .route(
            "/models",
            get(crate::routes::operator::get_component_models),
        )
        .route(
            "/models/{component}",
            put(crate::routes::operator::set_component_model),
        )
        .with_state(crate::state::AppState::for_test_client(
            cluster.client.clone(),
            &cluster.core_namespace(),
        ));
    for (mode, model, uid, succeeds) in [
        (
            "override",
            json!({"provider":"absent","deployment":"astra"}),
            "policy",
            false,
        ),
        ("inherit", Value::Null, "replaced", false),
        ("inherit", Value::Null, "policy", true),
        (
            "override",
            json!({"provider":"azure-openai","deployment":"mini"}),
            "policy",
            true,
        ),
    ] {
        let before = state.lock().unwrap().objects[path].clone();
        let response=app.clone().oneshot(Request::builder().method("PUT").uri("/models/sre")
            .header("content-type","application/json").body(Body::from(json!({"mode":mode,"model":model,"uid":uid,"resource_version":before["metadata"]["resourceVersion"]}).to_string())).unwrap()).await.unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            status.is_success(),
            succeeds,
            "{status}: {}",
            String::from_utf8_lossy(&body)
        );
        let data = state.lock().unwrap();
        let after = &data.objects[path];
        if succeeds {
            assert_eq!(after["spec"]["tokenBudget"], before["spec"]["tokenBudget"]);
            assert_eq!(
                after["spec"]["modelPreference"]["fallback"],
                before["spec"]["modelPreference"]["fallback"]
            );
            assert_eq!(
                after["spec"]["modelPreference"]["primary"]["deployment"],
                if mode == "inherit" { "astra" } else { "mini" }
            );
            assert_eq!(
                after["metadata"]["annotations"]["kars.azure.com/model-default-source"],
                if mode == "inherit" {
                    json!("cluster")
                } else {
                    Value::Null
                }
            );
        } else {
            assert_eq!(&before, after);
        }
    }
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/models")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(response.status().is_success());
    let dto: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(
        dto["cluster_default"],
        json!({"provider":"github-copilot","deployment":"astra"})
    );
    assert_eq!(dto["components"].as_array().unwrap().len(), 1);
    assert_eq!(dto["components"][0]["mode"], "override");
    assert_eq!(
        dto["components"][0]["desired"],
        json!({"provider":"azure-openai","deployment":"mini"})
    );
    assert_eq!(dto["components"][0]["applied"], false);
    let before = state.lock().unwrap().objects[path].clone();
    let response = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/models/sre")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"mode":"inherit","uid":"policy","resource_version":"stale"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(!response.status().is_success());
    assert_eq!(state.lock().unwrap().objects[path], before);
    server.abort();
}

#[tokio::test]
async fn component_models_refuse_managed_missing_default_and_denied_authority() {
    for scenario in [
        "owner",
        "bundle",
        "no-default",
        "denied",
        "override-on-inherit",
    ] {
        let (cluster, state, server, registry) = model_fixture().await;
        let path =
            "/apis/kars.azure.com/v1alpha1/namespaces/kars-system/inferencepolicies/sre-inference";
        let mut policy = json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"InferencePolicy",
            "metadata":{"name":"sre-inference","namespace":"kars-system","uid":"policy","resourceVersion":"1"},
            "spec":{"modelPreference":{"primary":{"provider":"azure-openai","deployment":"mini"}}}});
        if scenario == "owner" {
            policy["metadata"]["ownerReferences"] = json!([{"apiVersion":"v1","kind":"ConfigMap","name":"owner","uid":"owner","controller":true}]);
        }
        if scenario == "bundle" {
            policy["spec"]["bundleRef"] = json!({"name":"signed"});
        }
        {
            let mut data = state.lock().unwrap();
            data.objects.insert(path.into(), policy.clone());
            if scenario != "no-default" {
                data.objects.get_mut(&registry).unwrap()["metadata"]["annotations"]["kars.azure.com/default-model"] =
                    r#"{"provider":"github-copilot","deployment":"astra"}"#.into();
            }
            data.forbidden = scenario == "denied";
        }
        let app = Router::new()
            .route(
                "/models/{component}",
                put(crate::routes::operator::set_component_model),
            )
            .with_state(crate::state::AppState::for_test_client(
                cluster.client.clone(),
                &cluster.core_namespace(),
            ));
        let mut request = json!({"mode":"inherit","uid":"policy","resource_version":"1"});
        if scenario == "override-on-inherit" {
            request["model"] = json!({"provider":"azure-openai","deployment":"mini"});
        }
        let response = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/models/sre")
                    .header("content-type", "application/json")
                    .body(Body::from(request.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(!response.status().is_success(), "{scenario}");
        let data = state.lock().unwrap();
        assert_eq!(data.objects[path], policy, "{scenario}");
        assert!(
            data.calls.iter().all(|(method, _, _)| method == "GET"),
            "{scenario}"
        );
        server.abort();
    }
}

#[tokio::test]
async fn component_models_orchestrator_reads_override_without_changing_policy() {
    let (cluster, state, server, _) = model_fixture().await;
    let path = "/apis/kars.azure.com/v1alpha1/namespaces/kars-system/inferencepolicies/bridge-orchestrator-inference";
    state.lock().unwrap().objects.insert(path.into(),json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"InferencePolicy","metadata":{"name":"bridge-orchestrator-inference","namespace":"kars-system","generation":3},"spec":{"modelPreference":{"primary":{"provider":"azure-openai","deployment":"mini"},"fallback":[{"provider":"github-copilot","deployment":"astra"}]}},"status":{"observedGeneration":3,"compiledDigest":"sha256:test","loadedDigest":"sha256:test","conditions":[{"type":"Ready","status":"True"}]}}));
    let before = state.lock().unwrap().objects[path].clone();
    let model = cluster
        .bridge_orchestrator_model_route()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(model.provider, "azure-openai");
    assert_eq!(model.deployment, "mini");
    cluster
        .wait_for_bridge_orchestrator_model(&model.provider, &model.deployment)
        .await
        .unwrap();
    assert_eq!(state.lock().unwrap().objects[path], before);
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
