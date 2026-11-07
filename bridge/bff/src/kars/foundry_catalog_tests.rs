// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::kars::credentials::{FoundryProject, saved_foundry_project};

fn project() -> FoundryProject {
    FoundryProject {
        project_endpoint: "https://resource.services.ai.azure.com/api/projects/project".into(),
        inference_endpoint: "https://resource.services.ai.azure.com/".into(),
        api_key_auth: true,
        ..Default::default()
    }
}

#[tokio::test]
async fn foundry_catalog_uses_only_enrolled_provider_store_in_private_workspace() {
    let (cluster, state, server, path) = model_fixture().await;
    let before = state.lock().unwrap().objects[&path].clone();
    let (_, expected) = cluster
        .integration_store(&cluster.core_namespace(), "kars-inference-providers")
        .await
        .unwrap();
    cluster
        .write_foundry_catalog(
            &expected,
            Some(&project()),
            &["model".into()],
            Some("test-key"),
        )
        .await
        .unwrap();
    let fresh = Cluster::for_test_client(cluster.client.clone());
    let (_, saved) = fresh
        .integration_store(&fresh.core_namespace(), "kars-inference-providers")
        .await
        .unwrap();
    assert_eq!(
        saved_foundry_project(&saved)
            .unwrap()
            .unwrap()
            .unwrap()
            .project_endpoint,
        project().project_endpoint
    );
    {
        let data = state.lock().unwrap();
        for (key, value) in before["data"].as_object().unwrap() {
            assert_eq!(&data.objects[&path]["data"][key], value);
        }
        for (key, value) in before["metadata"]["annotations"].as_object().unwrap() {
            assert_eq!(&data.objects[&path]["metadata"]["annotations"][key], value);
        }
        for (method, target, _) in &data.calls {
            assert!(!target.contains("/deployments"));
            assert!(!target.contains("kars-foundry-credentials"));
            if method != "GET" {
                assert_eq!(method, "PATCH");
                assert_eq!(target, &path);
            }
        }
    }
    cluster
        .write_foundry_catalog(&saved, None, &[], None)
        .await
        .unwrap();
    let (_, disconnected) = fresh
        .integration_store(&fresh.core_namespace(), "kars-inference-providers")
        .await
        .unwrap();
    assert!(
        saved_foundry_project(&disconnected)
            .unwrap()
            .unwrap()
            .is_none()
    );
    assert_eq!(state.lock().unwrap().objects[&path]["data"], before["data"]);
    server.abort();
}

#[tokio::test]
async fn foundry_catalog_refuses_stale_authority_and_in_use_removal() {
    for scenario in ["version", "uid", "default"] {
        let (cluster, state, server, path) = model_fixture().await;
        let (_, expected) = cluster
            .integration_store(&cluster.core_namespace(), "kars-inference-providers")
            .await
            .unwrap();
        cluster
            .write_foundry_catalog(
                &expected,
                Some(&project()),
                &["model".into()],
                Some("test-key"),
            )
            .await
            .unwrap();
        let (_, expected) = cluster
            .integration_store(&cluster.core_namespace(), "kars-inference-providers")
            .await
            .unwrap();
        {
            let mut data = state.lock().unwrap();
            let secret = data.objects.get_mut(&path).unwrap();
            match scenario {
                "version" => secret["metadata"]["resourceVersion"] = json!("new-version"),
                "uid" => secret["metadata"]["uid"] = json!("replacement"),
                "default" => {
                    secret["metadata"]["annotations"]["kars.azure.com/default-model"] =
                        json!(r#"{"provider":"foundry","deployment":"model"}"#)
                }
                _ => unreachable!(),
            }
            data.calls.clear();
        }
        let before = state.lock().unwrap().objects[&path].clone();
        assert!(
            cluster
                .write_foundry_catalog(&expected, None, &[], None)
                .await
                .is_err()
        );
        let data = state.lock().unwrap();
        assert_eq!(data.objects[&path], before);
        assert!(data.calls.iter().all(|(method, _, _)| method == "GET"));
        server.abort();
    }
}
