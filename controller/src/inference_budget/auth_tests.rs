// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use serde_json::{Value, json};

fn pod() -> Value {
    json!({
        "apiVersion":"v1", "kind":"Pod",
        "metadata":{"name":"task-pod","namespace":"kars-task","uid":"pod-uid"},
        "spec":{
            "serviceAccountName":"sandbox",
            "volumes":[{"name":TOKEN_VOLUME,"projected":{"sources":[{"serviceAccountToken":{
                "audience":AUDIENCE,"path":"token","expirationSeconds":600
            }}]}}],
            "containers":[
                {"name":"agent","image":"agent:latest"},
                {"name":"inference-router","image":"router:latest",
                    "securityContext":{"runAsUser":1001,"allowPrivilegeEscalation":false,"readOnlyRootFilesystem":true},
                    "volumeMounts":[{"name":TOKEN_VOLUME,"mountPath":PRIVATE_MOUNT,"readOnly":true}]}
            ]
        }
    })
}

fn valid(value: Value) -> bool {
    has_router_only_projection(&serde_json::from_value::<Pod>(value).unwrap())
}

#[test]
fn only_the_secure_router_may_mount_the_budget_audience_token() {
    assert!(valid(pod()));
    let mut leaked = pod();
    leaked["spec"]["containers"][0]["volumeMounts"] =
        leaked["spec"]["containers"][1]["volumeMounts"].clone();
    assert!(!valid(leaked));
    let mut wrong_user = pod();
    wrong_user["spec"]["containers"][1]["securityContext"]["runAsUser"] = json!(1000);
    assert!(!valid(wrong_user));
}

#[test]
fn alternate_named_budget_projection_and_shared_process_namespaces_fail_closed() {
    let mut alternate = pod();
    let mut volume = alternate["spec"]["volumes"][0].clone();
    volume["name"] = json!("disguised");
    alternate["spec"]["volumes"]
        .as_array_mut()
        .unwrap()
        .push(volume);
    alternate["spec"]["containers"][0]["volumeMounts"] =
        json!([{"name":"disguised","mountPath":"/agent-token"}]);
    assert!(!valid(alternate));
    for field in ["hostPID", "hostNetwork", "hostIPC", "shareProcessNamespace"] {
        let mut shared = pod();
        shared["spec"][field] = json!(true);
        assert!(!valid(shared));
    }
}

#[test]
fn standard_admission_injections_are_not_mistaken_for_replaced_router_authority() {
    use k8s_openapi::api::core::v1::Container;
    let template: Container =
        serde_json::from_value(pod()["spec"]["containers"][1].clone()).unwrap();
    let mut actual = template.clone();
    actual.env = Some(serde_json::from_value(json!([
            {"name":"AZURE_CLIENT_ID","value":"operator-client"},
            {"name":"AZURE_TENANT_ID","value":"operator-tenant"},
            {"name":"AZURE_FEDERATED_TOKEN_FILE","value":"/var/run/secrets/azure/tokens/azure-identity-token"}
        ])).unwrap());
    actual.volume_mounts.as_mut().unwrap().extend(
            serde_json::from_value::<Vec<k8s_openapi::api::core::v1::VolumeMount>>(json!([
                {"name":"kube-api-access-abcde","mountPath":"/var/run/secrets/kubernetes.io/serviceaccount","readOnly":true},
                {"name":"azure-identity-token","mountPath":"/var/run/secrets/azure/tokens","readOnly":true}
            ])).unwrap()
        );
    assert!(router_env_matches(&actual, &template));
    assert!(router_mounts_match(&actual, &template));
    actual.env.as_mut().unwrap().push(
        serde_json::from_value(json!({
            "name":"KARS_INFERENCE_BUDGET_REQUIRED","value":"false"
        }))
        .unwrap(),
    );
    assert!(!router_env_matches(&actual, &template));
    actual.volume_mounts.as_mut().unwrap()[1].mount_path = PRIVATE_MOUNT.into();
    assert!(!router_mounts_match(&actual, &template));
}

#[test]
fn audience_or_expiry_substitution_does_not_authorize_a_router() {
    let mut wrong = pod();
    wrong["spec"]["volumes"][0]["projected"]["sources"][0]["serviceAccountToken"]["audience"] =
        json!("api");
    assert!(!valid(wrong));
    let mut persistent = pod();
    persistent["spec"]["volumes"][0]["projected"]["sources"][0]["serviceAccountToken"]["expirationSeconds"] =
        json!(86400);
    assert!(!valid(persistent));
}
