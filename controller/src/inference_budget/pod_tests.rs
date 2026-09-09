// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::inference_budget_contract::{
    AccountReference, BudgetScope, RootIdentity, RootKind, TaskBudgetBinding,
};

fn identity(name: &str) -> ResourceIdentity {
    ResourceIdentity {
        namespace: "workspace".into(),
        name: name.into(),
        uid: format!("{name}-uid"),
    }
}

fn plan() -> Plan {
    Plan {
        binding: RouterBinding {
            task: TaskBudgetBinding {
                scope: BudgetScope::GovernedInference,
                account: AccountReference {
                    namespace: "accounting".into(),
                    name: "root".into(),
                    uid: "account-uid".into(),
                },
                root: RootIdentity {
                    kind: RootKind::KarsTask,
                    resource: identity("task"),
                    workspace_uid: "workspace-uid".into(),
                    cluster_uid: "cluster-uid".into(),
                },
                task_uid: "task-uid".into(),
                parent_task_uid: None,
                root_task_uid: "task-uid".into(),
                authorization_digest: format!("sha256:{}", "a".repeat(64)),
            },
            sandbox: identity("task"),
            runtime_namespace: "kars-task".into(),
            runtime_namespace_uid: "namespace-uid".into(),
            privacy_epoch: None,
        },
        ca_version: "public-ca-version".into(),
        endpoint: "https://kars-inference-budget.accounting.svc:9447".into(),
        router_image_digest: format!("sha256:{}", "7".repeat(64)),
    }
}

#[test]
fn every_runtime_receives_only_a_router_private_token_mount() {
    for runtime in [
        "openclaw",
        "hermes",
        "openai-agents",
        "maf-python",
        "anthropic",
        "pydantic-ai",
        "langgraph",
        "byo",
    ] {
        let agent = if runtime == "openclaw" {
            "openclaw"
        } else {
            "agent"
        };
        let mut pod = json!({
            "volumes": [], "serviceAccountName": "sandbox",
            "containers": [
                {"name": agent, "env": [{"name":"KARS_RUNTIME_KIND","value":runtime}], "volumeMounts": []},
                {"name":"inference-router","image":"router:latest","env": [], "volumeMounts":[]}
            ]
        });
        let original_agent = pod["containers"][0].clone();
        let mut annotations = std::collections::BTreeMap::new();
        plan().apply(&mut pod, &mut annotations).unwrap();
        assert_eq!(pod["containers"][0], original_agent, "{runtime}");
        let router = &pod["containers"][1];
        assert_eq!(router["volumeMounts"][0]["mountPath"], PRIVATE_MOUNT);
        assert_eq!(router["volumeMounts"][0]["readOnly"], true);
        assert_eq!(
            pod["volumes"][0]["projected"]["sources"][0]["serviceAccountToken"]["audience"],
            AUDIENCE
        );
        assert_eq!(router["readinessProbe"]["httpGet"]["path"], "/readyz");
        assert_eq!(
            router["image"],
            format!("router:latest@sha256:{}", "7".repeat(64))
        );
        assert_eq!(
            annotations["kars.azure.com/inference-budget-ca-version"],
            "public-ca-version"
        );
    }
}

#[tokio::test]
async fn no_task_owner_no_reference_is_a_byte_safe_legacy_noop_without_api_access() {
    let server = wiremock::MockServer::start().await;
    let config = kube::Config::new(server.uri().parse().unwrap());
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = Client::try_from(config).unwrap();
    let sandbox: KarsSandbox = serde_json::from_value(json!({
        "apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsSandbox",
        "metadata":{"name":"legacy","namespace":"workspace","uid":"legacy-uid"},
        "spec":{"inferenceRef":{"name":"legacy-inference"}}
    }))
    .unwrap();
    let namespace: Namespace = serde_json::from_value(json!({
        "apiVersion":"v1","kind":"Namespace","metadata":{"name":"kars-legacy","uid":"namespace-uid"}
    }))
    .unwrap();
    let mut pod = json!({"containers":[{"name":"agent","envFrom":[{"secretRef":{"name":"legacy-credentials"}}]}]});
    let before = pod.clone();
    assert!(
        decorate(&client, &sandbox, &namespace, &mut pod)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(pod, before);
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[test]
fn collision_never_adds_a_second_private_volume() {
    let mut pod = json!({"volumes":[{"name":TOKEN_VOLUME}], "containers":[]});
    let before = pod.clone();
    assert!(plan().apply(&mut pod, &mut Default::default()).is_err());
    assert_eq!(before, pod);
}
