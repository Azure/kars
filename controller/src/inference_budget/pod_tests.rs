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
                {"name":"inference-router","image":"router:latest","env": [], "volumeMounts":[],
                 "livenessProbe":{"httpGet":{"path":"/healthz","port":"inference"},"timeoutSeconds":1}}
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
        assert_eq!(router["readinessProbe"]["timeoutSeconds"], 10, "{runtime}");
        assert_eq!(router["readinessProbe"]["periodSeconds"], 15, "{runtime}");
        assert_eq!(router["livenessProbe"]["httpGet"]["path"], "/healthz");
        assert_eq!(router["livenessProbe"]["timeoutSeconds"], 1, "{runtime}");
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
async fn no_task_owner_no_reference_preserves_legacy_pod_after_calendar_boundary_check() {
    let server = wiremock::MockServer::start().await;
    let config = kube::Config::new(server.uri().parse().unwrap());
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = Client::try_from(config).unwrap();
    let sandbox: KarsSandbox = serde_json::from_value(json!({
        "apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsSandbox",
        "metadata":{"name":"legacy","namespace":"workspace","uid":"legacy-uid","resourceVersion":"1"},
        "spec":{"inferenceRef":{"name":"legacy-inference"}}
    }))
    .unwrap();
    let namespace: Namespace = serde_json::from_value(json!({
        "apiVersion":"v1","kind":"Namespace",
        "metadata":{
            "name":"kars-legacy","uid":"namespace-uid","resourceVersion":"1",
            "annotations":{
                "kars.azure.com/namespace-claim-version":"v1",
                "kars.azure.com/sandbox-namespace":"workspace",
                "kars.azure.com/sandbox-name":"legacy",
                "kars.azure.com/sandbox-uid":"legacy-uid"
            }
        }
    }))
    .unwrap();
    assert!(
        prepare(&client, &sandbox, &namespace)
            .await
            .unwrap()
            .is_none()
    );
    assert!(server.received_requests().await.unwrap().is_empty());
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/api/v1/namespaces/kars-legacy"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(&namespace))
        .expect(1)
        .mount(&server)
        .await;
    let mut pod = json!({"containers":[{"name":"agent","envFrom":[{"secretRef":{"name":"legacy-credentials"}}]}]});
    let before = pod.clone();
    assert!(
        decorate(&client, &sandbox, &namespace, &mut pod)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(pod, before);
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method.as_str(), "GET");
}

#[test]
fn collision_never_adds_a_second_private_volume() {
    let mut pod = json!({"volumes":[{"name":TOKEN_VOLUME}], "containers":[]});
    let before = pod.clone();
    assert!(plan().apply(&mut pod, &mut Default::default()).is_err());
    assert_eq!(before, pod);
}
