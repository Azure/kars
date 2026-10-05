// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

fn enroll(state: &mut TestApi, namespace: &str) -> String {
    let path = format!(
        "/apis/kars.azure.com/v1alpha1/namespaces/{namespace}/karscredentialgrants/workspace"
    );
    state.objects.insert(path.clone(), json!({
        "apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsCredentialGrant",
        "metadata":{"name":"workspace","namespace":namespace,"uid":"grant","generation":1},
        "spec":{"enabled":true,"workspaceUid":"namespace","agentKeys":[],
            "integrationStores":[{"secret":{"name":"kars-inference-providers","uid":"secret"},"purpose":"inference"}]},
        "status":{"phase":"Ready","observedGeneration":1,"sources":[],"legacySources":[]}
    }));
    state.objects.insert(
        format!("/api/v1/namespaces/{namespace}"),
        json!({
            "apiVersion":"v1","kind":"Namespace","metadata":{"name":namespace,"uid":"namespace"}
        }),
    );
    let mut secret = state.secret.clone();
    secret["metadata"]["name"] = "kars-inference-providers".into();
    secret["metadata"]["namespace"] = namespace.into();
    state.objects.insert(
        format!("/api/v1/namespaces/{namespace}/secrets/kars-inference-providers"),
        secret,
    );
    path
}

#[tokio::test]
async fn optional_provider_discovery_does_not_read_unenrolled_secrets() {
    let (cluster, state, server) = fixture().await;
    let namespace = cluster.core_namespace();
    assert!(cluster.additional_provider_keys().await.unwrap().is_empty());
    assert_eq!(state.lock().unwrap().calls.len(), 1);
    let path = enroll(&mut state.lock().unwrap(), &namespace);
    state.lock().unwrap().objects.get_mut(&path).unwrap()["spec"]["integrationStores"] = json!([]);
    assert!(cluster.additional_provider_keys().await.unwrap().is_empty());
    assert!(
        cluster
            .read_secret_all(&namespace, "kars-inference-providers")
            .await
            .is_err()
    );
    assert!(
        cluster
            .mutate_secret_keys(&namespace, "kars-inference-providers", |keys| {
                keys.insert("new".into(), "value".into());
            })
            .await
            .is_err()
    );
    assert!(
        state
            .lock()
            .unwrap()
            .calls
            .iter()
            .all(|(method, path, _)| method == "GET" && !path.contains("/secrets/"))
    );
    server.abort();
}

#[tokio::test]
async fn optional_provider_discovery_reads_only_valid_enrolled_store() {
    let (cluster, state, server) = fixture().await;
    enroll(&mut state.lock().unwrap(), &cluster.core_namespace());
    let keys = cluster.additional_provider_keys().await.unwrap();
    assert_eq!(keys.get("client-id").map(String::as_str), Some("old"));
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
async fn optional_provider_discovery_preserves_authority_and_transport_errors() {
    for scenario in [
        "forbidden",
        "disabled",
        "stale",
        "malformed",
        "terminating-grant",
        "missing-namespace",
        "replaced-namespace",
        "terminating-namespace",
        "missing-secret",
        "replaced-secret",
        "wrong-type",
        "terminating-secret",
    ] {
        let (cluster, state, server) = fixture().await;
        let namespace = cluster.core_namespace();
        {
            let mut state = state.lock().unwrap();
            let grant = enroll(&mut state, &namespace);
            let ns = format!("/api/v1/namespaces/{namespace}");
            let secret = format!("{ns}/secrets/kars-inference-providers");
            match scenario {
                "forbidden" => state.forbidden = true,
                "disabled" => {
                    state.objects.get_mut(&grant).unwrap()["spec"]["enabled"] = false.into()
                }
                "stale" => {
                    state.objects.get_mut(&grant).unwrap()["status"]["observedGeneration"] =
                        0.into()
                }
                "malformed" => {
                    state.objects.get_mut(&grant).unwrap()["spec"]["integrationStores"] = json!({})
                }
                "terminating-grant" => {
                    state.objects.get_mut(&grant).unwrap()["metadata"]["deletionTimestamp"] =
                        "2026-01-01T00:00:00Z".into()
                }
                "missing-namespace" => {
                    state.objects.remove(&ns);
                }
                "replaced-namespace" => {
                    state.objects.get_mut(&ns).unwrap()["metadata"]["uid"] = "replacement".into()
                }
                "terminating-namespace" => {
                    state.objects.get_mut(&ns).unwrap()["metadata"]["deletionTimestamp"] =
                        "2026-01-01T00:00:00Z".into()
                }
                "missing-secret" => {
                    state.objects.remove(&secret);
                }
                "replaced-secret" => {
                    state.objects.get_mut(&secret).unwrap()["metadata"]["uid"] =
                        "replacement".into()
                }
                "wrong-type" => {
                    state.objects.get_mut(&secret).unwrap()["type"] = "kubernetes.io/tls".into()
                }
                "terminating-secret" => {
                    state.objects.get_mut(&secret).unwrap()["metadata"]["deletionTimestamp"] =
                        "2026-01-01T00:00:00Z".into()
                }
                _ => unreachable!(),
            }
        }
        let error = cluster
            .additional_provider_keys()
            .await
            .expect_err(scenario);
        assert!(!error.to_string().contains("PRIVATE_VALUE_SENTINEL"));
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
}
