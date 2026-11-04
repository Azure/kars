// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::kars::task::TaskModel;

#[path = "component_model_tests.rs"]
mod component_models;

#[path = "provider_lifecycle_tests.rs"]
mod provider_lifecycle;
#[path = "foundry_catalog_tests.rs"]
mod foundry_catalog;

async fn model_fixture() -> (
    Cluster,
    Arc<Mutex<TestApi>>,
    tokio::task::JoinHandle<()>,
    String,
) {
    let (cluster, state, server) = fixture().await;
    let ns = cluster.core_namespace();
    let path = format!("/api/v1/namespaces/{ns}/secrets/kars-inference-providers");
    {
        let mut data = state.lock().unwrap();
        for resource in [
            "toolpolicies",
            "mcpservers",
            "karsmemories",
            "inferencepolicies",
        ] {
            data.objects.insert(
                format!("/apis/kars.azure.com/v1alpha1/{resource}"),
                json!({
                    "apiVersion":"kars.azure.com/v1alpha1", "kind":"List", "metadata":{}, "items":[]
                }),
            );
        }
        data.objects.insert(
            "/apis/kars.azure.com/v1alpha1/namespaces/kars-system/karsskills".into(),
            json!({
                "apiVersion":"kars.azure.com/v1alpha1", "kind":"List", "metadata":{}, "items":[]
            }),
        );
        data.objects.insert(
            format!("/api/v1/namespaces/{ns}"),
            json!({
                "metadata":{"name":ns,"uid":"workspace","resourceVersion":"1",
                    "annotations":{"kars.azure.com/private-enabled":"true"}}
            }),
        );
        data.objects.insert(format!("/apis/kars.azure.com/v1alpha1/namespaces/{ns}/karscredentialgrants/workspace"), json!({
            "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsCredentialGrant",
            "metadata":{"name":"workspace","namespace":ns,"uid":"grant","resourceVersion":"1","generation":1},
            "spec":{"enabled":true,"workspaceUid":"workspace","integrationStores":[
                {"purpose":"providers","secret":{"name":"kars-inference-providers","uid":"registry"}}
            ]},
            "status":{"phase":"Ready","observedGeneration":1,"sources":[],"legacySources":[]}
        }));
        data.objects.insert(path.clone(), json!({
            "apiVersion":"v1","kind":"Secret","type":"Opaque",
            "metadata":{"name":"kars-inference-providers","namespace":ns,"uid":"registry","resourceVersion":"1",
                "annotations":{"kars.azure.com/credential-store-grant-uid":"grant","preserved":"yes"}},
            "data":{"KARS_PROVIDER_AZURE_OPENAI_ENDPOINT":"aHR0cHM6Ly9henVyZS5leGFtcGxl",
                "KARS_PROVIDER_AZURE_OPENAI_API_KEY":"cHJlc2VydmVk",
                "KARS_PROVIDER_AZURE_OPENAI_MODELS":"YXN0cmEsbWluaQ==",
                "KARS_PROVIDER_GITHUB_COPILOT_MODELS":"YXN0cmEsZmxhc2g=",
                "COPILOT_GITHUB_TOKEN":"cHJlc2VydmVk"}
        }));
    }
    (cluster, state, server, path)
}

#[tokio::test]
async fn model_default_switches_on_private_workspace_without_controller_or_credential_mutation() {
    let (cluster, state, server, path) = model_fixture().await;
    let before = state.lock().unwrap().objects[&path].clone();
    assert!(cluster.model_default().await.unwrap().is_none());
    for (provider, deployment) in [
        ("github-copilot", "astra"),
        ("azure-openai", "mini"),
        ("github-copilot", "flash"),
        ("github-copilot", "astra"),
    ] {
        let (_, expected) = cluster
            .integration_store(&cluster.core_namespace(), "kars-inference-providers")
            .await
            .unwrap();
        let selected = TaskModel {
            provider: provider.into(),
            deployment: deployment.into(),
        };
        cluster
            .write_model_default(&selected, &expected)
            .await
            .unwrap();
        let fresh = Cluster::for_test_client(cluster.client.clone());
        let actual = fresh.model_default().await.unwrap().unwrap();
        assert_eq!(actual.provider, provider);
        assert_eq!(actual.deployment, deployment);
    }
    let data = state.lock().unwrap();
    assert_eq!(data.objects[&path]["data"], before["data"]);
    assert_eq!(
        data.objects[&path]["metadata"]["annotations"]["preserved"],
        "yes"
    );
    assert_eq!(
        data.objects[&path]["metadata"]["annotations"]["kars.azure.com/credential-store-grant-uid"],
        "grant"
    );
    for (method, target, body) in &data.calls {
        assert!(!target.contains("/deployments/"));
        if method != "GET" {
            assert_eq!(method, "PATCH");
            assert_eq!(target, &path);
            assert_eq!(body[0]["op"], "test");
            assert_eq!(body[1]["path"], "/metadata/resourceVersion");
            assert_eq!(body[2]["path"], "/metadata/annotations");
            assert_eq!(body.as_array().unwrap().len(), 3);
        }
    }
    server.abort();
}

#[tokio::test]
async fn model_default_actions_validate_catalogue_and_recover_stale_or_malformed_selection() {
    use axum::{
        body::{Body, to_bytes},
        http::Request,
        routing::{get, post},
    };
    use tower::ServiceExt;
    let (cluster, state, server, path) = model_fixture().await;
    let app = Router::new()
        .route("/options", get(crate::routes::options::get_options))
        .route("/default", post(crate::routes::operator::set_default_model))
        .with_state(crate::state::AppState::for_test_client(
            cluster.client.clone(),
            &cluster.core_namespace(),
        ));
    for raw in [
        r#"{"provider":"removed","deployment":"missing"}"#,
        "not-json",
    ] {
        state.lock().unwrap().objects.get_mut(&path).unwrap()["metadata"]["annotations"]["kars.azure.com/default-model"] =
            raw.into();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/options")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(!response.status().is_success());
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/options?catalogue_only=true")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            response.status().is_success(),
            "catalogue recovery {}",
            response.status()
        );
        let options: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert!(options["default_model"].is_null());
        assert!(options["provider"].is_null());
        assert!(
            options["models"]
                .as_array()
                .unwrap()
                .iter()
                .all(|m| m["is_default"] == false)
        );
        assert!(
            options["models"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["deployment"] == "astra")
        );
        for (deployment, succeeds) in [("absent", false), ("", false), ("astra", true)] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/default")
                        .header("content-type", "application/json")
                        .body(Body::from(
                            json!({"provider":"github-copilot","deployment":deployment})
                                .to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = response.status();
            let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            assert_eq!(
                status.is_success(),
                succeeds,
                "{status}: {}",
                String::from_utf8_lossy(&body)
            );
            if !succeeds {
                assert_eq!(
                    state.lock().unwrap().objects[&path]["metadata"]["annotations"]["kars.azure.com/default-model"],
                    raw
                );
            }
        }
        let options = crate::routes::options::build_options(&Cluster::for_test_client(
            cluster.client.clone(),
        ))
        .await
        .unwrap();
        assert_eq!(options.default_model.as_deref(), Some("astra"));
        assert_eq!(options.provider.unwrap().id, "github-copilot");
        let defaults: Vec<_> = options.models.iter().filter(|m| m.is_default).collect();
        assert_eq!(defaults.len(), 1);
        assert_eq!(defaults[0].provider, "github-copilot");
        assert_eq!(defaults[0].deployment, "astra");
    }
    server.abort();
}

#[tokio::test]
async fn model_default_preserves_concurrent_configuration_and_rejects_replacement_or_denied_write()
{
    for fault in ["version", "replacement", "patch-race", "forbidden"] {
        let (cluster, state, server, path) = model_fixture().await;
        let (_, expected) = cluster
            .integration_store(&cluster.core_namespace(), "kars-inference-providers")
            .await
            .unwrap();
        {
            let mut data = state.lock().unwrap();
            match fault {
                "version" => {
                    data.objects.get_mut(&path).unwrap()["metadata"]["resourceVersion"] = "2".into()
                }
                "replacement" => {
                    data.objects.get_mut(&path).unwrap()["metadata"]["uid"] = "replacement".into()
                }
                "patch-race" => data.gate_patch_conflicts = 1,
                "forbidden" => data.forbidden = true,
                _ => unreachable!(),
            }
        }
        let selected = TaskModel {
            provider: "github-copilot".into(),
            deployment: "astra".into(),
        };
        assert!(
            cluster
                .write_model_default(&selected, &expected)
                .await
                .is_err(),
            "{fault}"
        );
        let data = state.lock().unwrap();
        assert!(
            data.objects[&path]["metadata"]["annotations"]
                .get("kars.azure.com/default-model")
                .is_none()
        );
        assert_eq!(
            data.objects[&path]["data"],
            serde_json::to_value(expected.data).unwrap()
        );
        server.abort();
    }
}
