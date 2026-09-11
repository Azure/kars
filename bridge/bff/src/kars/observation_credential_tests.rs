use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};

fn inventory(workspace: &str) -> std::collections::BTreeMap<String, Value> {
    let identity = json!({"sandbox":{"namespace":workspace,"name":"agent","uid":"sandbox"},
        "namespace_uid":"runtime","task":null,"task_authorization":null,"task_generation":null,"managed":true});
    let binding = json!({"capability":"kars.azure.com/egress-observation/v1","identity":identity,
        "grant":{"namespace":workspace,"name":"workspace","uid":"grant","generation":1},
        "recipients":[{"namespace":"bridge","namespaceUid":"bridge","name":"bff","uid":"bff"}],
        "privacyRevision":"kars.azure.com/sre-privacy/v2","privacyEpoch":null,
        "workspaceUid":"workspace","expiresAt":chrono::Utc::now().timestamp()+600,
        "verifier":{"capability":"kars.azure.com/observation-privacy/v1"},
        "serverName":"observer-sandbox.kars.internal","caPem":"not-used-by-rejection-cases"});
    std::collections::BTreeMap::from([
        (
            format!(
                "/apis/kars.azure.com/v1alpha1/namespaces/{workspace}/karscredentialgrants/workspace"
            ),
            json!({
                "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsCredentialGrant",
                "metadata":{"name":"workspace","namespace":workspace,"uid":"grant","generation":1,"resourceVersion":"1"},
                "spec":{"enabled":true,"workspaceUid":"workspace","agentKeys":[],"integrationStores":[],
                    "observationTargets":[{"kind":"KarsSandbox","namespace":workspace,"name":"agent","uid":"sandbox"}]},
                "status":{"phase":"Ready","observedGeneration":1,"sources":[],"legacySources":[]}
            }),
        ),
        (
            format!("/api/v1/namespaces/{workspace}"),
            json!({"apiVersion":"v1","kind":"Namespace",
            "metadata":{"name":workspace,"uid":"workspace","resourceVersion":"1"}}),
        ),
        (
            format!("/apis/kars.azure.com/v1alpha1/namespaces/{workspace}/karssandboxes/agent"),
            json!({
                "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSandbox",
                "metadata":{"name":"agent","namespace":workspace,"uid":"sandbox","resourceVersion":"1",
                    "annotations":{"kars.azure.com/namespace-uid":"runtime"}},
                "status":{"serviceObservation":{"capability":"kars.azure.com/egress-observation/v1",
                    "phase":"Ready","grant":{"uid":"grant"},"namespaceUid":"runtime","secret":{"name":"router-services-observer","uid":"secret"},
                    "version":"secret:1","deploymentUid":"deployment","privacyRevision":"kars.azure.com/sre-privacy/v2","privacyEpoch":null}}
            }),
        ),
        (
            "/api/v1/namespaces/kars-agent".into(),
            json!({"apiVersion":"v1","kind":"Namespace",
            "metadata":{"name":"kars-agent","uid":"runtime","resourceVersion":"1","annotations":{
                "kars.azure.com/namespace-claim-version":"v1","kars.azure.com/sandbox-namespace":workspace,
                "kars.azure.com/sandbox-name":"agent","kars.azure.com/sandbox-uid":"sandbox"}}}),
        ),
        (
            "/api/v1/namespaces/kars-agent/secrets/router-services-observer".into(),
            json!({
                "apiVersion":"v1","kind":"Secret","type":"Opaque",
                "metadata":{"name":"router-services-observer","namespace":"kars-agent","uid":"secret","resourceVersion":"1",
                    "annotations":{"kars.azure.com/sandbox-uid":"sandbox","kars.azure.com/namespace-uid":"runtime"}},
                "data":{"observation-token":STANDARD.encode("o".repeat(64)),"config.json":STANDARD.encode(binding.to_string())}
            }),
        ),
        (
            "/apis/authentication.k8s.io/v1/selfsubjectreviews".into(),
            json!({
            "apiVersion":"authentication.k8s.io/v1","kind":"SelfSubjectReview","status":{"userInfo":{
                "uid":"bff","username":"system:serviceaccount:bridge:bff"}}}),
        ),
        (
            "/api/v1/namespaces/bridge".into(),
            json!({"apiVersion":"v1","kind":"Namespace",
            "metadata":{"name":"bridge","uid":"bridge","resourceVersion":"1"}}),
        ),
        (
            "/api/v1/namespaces/bridge/serviceaccounts/bff".into(),
            json!({"apiVersion":"v1","kind":"ServiceAccount",
            "metadata":{"name":"bff","namespace":"bridge","uid":"bff","resourceVersion":"1"}}),
        ),
        (
            "/apis/apps/v1/namespaces/kars-agent/deployments/agent".into(),
            json!({
            "apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":"agent","namespace":"kars-agent","uid":"deployment","resourceVersion":"1"}}),
        ),
        (
            "/api/v1/namespaces/kars-agent/pods".into(),
            json!({
            "apiVersion":"v1","kind":"PodList","metadata":{},"items":[{
                "metadata":{"name":"agent-pod","namespace":"kars-agent","uid":"pod","resourceVersion":"1",
                    "annotations":{"kars.azure.com/services-observer-version":"secret:1"},
                    "ownerReferences":[{"apiVersion":"apps/v1","kind":"ReplicaSet","name":"agent-rs","uid":"rs","controller":true}]},
                "status":{"phase":"Running","podIP":"127.0.0.1"}
            }]}),
        ),
        (
            "/apis/apps/v1/namespaces/kars-agent/replicasets/agent-rs".into(),
            json!({
                "apiVersion":"apps/v1","kind":"ReplicaSet","metadata":{"name":"agent-rs","namespace":"kars-agent","uid":"rs",
                    "ownerReferences":[{"apiVersion":"apps/v1","kind":"Deployment","name":"agent","uid":"deployment","controller":true}]}
            }),
        ),
    ])
}

#[tokio::test]
async fn observation_private_adapter_rejects_old_core_and_foreign_uid_before_contacting_router() {
    let (cluster, state, server) = fixture().await;
    let workspace = cluster.core_namespace();
    let sandbox =
        format!("/apis/kars.azure.com/v1alpha1/namespaces/{workspace}/karssandboxes/agent");
    for (path, pointer, value, expected_last) in [
        (
            sandbox.as_str(),
            "/status/serviceObservation/capability",
            json!("old-core"),
            sandbox.as_str(),
        ),
        (
            sandbox.as_str(),
            "/metadata/uid",
            json!("replacement"),
            sandbox.as_str(),
        ),
        (
            "/api/v1/namespaces/kars-agent",
            "/metadata/uid",
            json!("replacement"),
            "/api/v1/namespaces/kars-agent",
        ),
        (
            "/api/v1/namespaces/kars-agent/secrets/router-services-observer",
            "/metadata/uid",
            json!("replacement"),
            "/api/v1/namespaces/kars-agent/secrets/router-services-observer",
        ),
        (
            "/api/v1/namespaces/bridge/serviceaccounts/bff",
            "/metadata/uid",
            json!("replacement"),
            "/api/v1/namespaces/bridge/serviceaccounts/bff",
        ),
        (
            "/api/v1/namespaces/bridge",
            "/metadata/uid",
            json!("replacement"),
            "/api/v1/namespaces/bridge/serviceaccounts/bff",
        ),
        (
            "/apis/apps/v1/namespaces/kars-agent/deployments/agent",
            "/metadata/uid",
            json!("replacement"),
            "/apis/apps/v1/namespaces/kars-agent/deployments/agent",
        ),
        (
            "/apis/apps/v1/namespaces/kars-agent/replicasets/agent-rs",
            "/metadata/uid",
            json!("replacement"),
            "/apis/apps/v1/namespaces/kars-agent/replicasets/agent-rs",
        ),
    ] {
        {
            let mut data = state.lock().unwrap();
            data.calls.clear();
            data.objects = inventory(&workspace);
            *data
                .objects
                .get_mut(path)
                .unwrap()
                .pointer_mut(pointer)
                .unwrap() = value;
        }
        assert!(
            cluster.private_learned_domains("agent").await.is_err(),
            "{path}{pointer}"
        );
        let data = state.lock().unwrap();
        assert_eq!(
            data.calls.last().unwrap().1,
            expected_last,
            "{path}{pointer}"
        );
        assert!(data.calls.iter().all(|(method, path, _)| method == "GET"
            || path == "/apis/authentication.k8s.io/v1/selfsubjectreviews"));
        assert!(
            data.calls
                .iter()
                .filter(|(_, path, _)| path.contains("/secrets/"))
                .all(|(_, path, _)| path.ends_with("/router-services-observer"))
        );
    }
    server.abort();
}

#[tokio::test]
async fn observation_private_adapter_surfaces_forbidden_without_legacy_fallback() {
    let (cluster, state, server) = fixture().await;
    state.lock().unwrap().forbidden = true;
    let error = cluster
        .private_learned_domains("agent")
        .await
        .unwrap_err()
        .to_string();
    assert!(!error.contains("PRIVATE_VALUE_SENTINEL"));
    assert_eq!(state.lock().unwrap().calls.len(), 1);
    server.abort();
}

#[tokio::test]
async fn observation_private_adapter_requires_current_rpc_capability_and_unexpired_token() {
    let (cluster, state, server) = fixture().await;
    let workspace = cluster.core_namespace();
    let path = "/api/v1/namespaces/kars-agent/secrets/router-services-observer";
    for (field, value) in [
        ("verifier", Value::Null),
        ("expiresAt", json!(chrono::Utc::now().timestamp() - 1)),
        ("workspaceUid", json!("replaced")),
    ] {
        {
            let mut data = state.lock().unwrap();
            data.calls.clear();
            data.objects = inventory(&workspace);
            let secret = data.objects.get_mut(path).unwrap();
            let raw = STANDARD
                .decode(secret["data"]["config.json"].as_str().unwrap())
                .unwrap();
            let mut config: Value = serde_json::from_slice(&raw).unwrap();
            config[field] = value;
            secret["data"]["config.json"] = STANDARD.encode(config.to_string()).into();
        }
        assert!(
            cluster.private_learned_domains("agent").await.is_err(),
            "{field}"
        );
        assert_eq!(state.lock().unwrap().calls.last().unwrap().1, path);
    }
    server.abort();
}
