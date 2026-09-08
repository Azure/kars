// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{
    credentials::{RETIRED, REVISION, VERSION},
    *,
};
use crate::sre_registration::{EPOCH, KarsSRERegistration};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use wiremock::{Mock, MockServer, ResponseTemplate};

const NS: &str = "kars-normal";
const SECRETS: &str = "/api/v1/namespaces/kars-normal/secrets";
const REG: &str = "/apis/kars.azure.com/v1alpha1/karssreregistrations/canonical";
const DEPLOY: &str = "/apis/apps/v1/namespaces/kars-normal/deployments/normal";

#[path = "private_purpose_tests.rs"]
mod private_purpose_tests;

fn source() -> KarsSandbox {
    serde_json::from_value(json!({
        "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSandbox",
        "metadata":{"name":"normal","namespace":"kars-system","uid":"source","resourceVersion":"1",
            "annotations":{NAMESPACE_UID:"namespace"}},
        "spec":{"runtime":{"kind":"BYO","byo":{"image":"pinned:test","contractVersion":"v1"}},
            "sandbox":{"isolation":"standard"},"inferenceRef":{"name":"inference"}}
    }))
    .unwrap()
}

fn namespace() -> Namespace {
    serde_json::from_value(json!({"apiVersion":"v1","kind":"Namespace",
        "metadata":{"name":NS,"uid":"namespace","resourceVersion":"1","annotations":{
            "kars.azure.com/namespace-claim-version":"v1","kars.azure.com/sandbox-namespace":"kars-system",
            "kars.azure.com/sandbox-name":"normal",SOURCE_UID:"source"}}})).unwrap()
}

fn secret(epoch: Option<&str>, stamped: bool) -> Value {
    let mut value = json!({"apiVersion":"v1","kind":"Secret","type":"Opaque",
        "metadata":{"name":SECRET,"namespace":NS,"uid":"secret","resourceVersion":"7",
            "labels":{"app.kubernetes.io/managed-by":"kars-controller","customer":"preserve"},
            "annotations":{SOURCE_UID:"source",NAMESPACE_UID:"namespace"}},
        "data":{"control-token":STANDARD.encode("x".repeat(64))}});
    if let Some(epoch) = epoch {
        value["metadata"]["annotations"][EPOCH] = epoch.into();
    }
    if stamped {
        value["metadata"]["annotations"][REVISION] = crate::sre_privacy::REVISION.into();
    }
    value
}

fn deployment() -> Value {
    json!({"apiVersion":"apps/v1","kind":"Deployment",
        "metadata":{"name":"normal","namespace":NS,"uid":"deployment","resourceVersion":"3",
            "labels":{"kars.azure.com/sandbox":"normal"},
            "managedFields":[{"manager":crate::field_managers::CLAWSANDBOX,"operation":"Apply",
                "apiVersion":"apps/v1","fieldsType":"FieldsV1","fieldsV1":{"f:spec":{}}}]},
        "spec":{"replicas":1,"selector":{"matchLabels":{"kars.azure.com/sandbox":"normal"}},
            "template":{"metadata":{"annotations":{"customer":"preserve"}},
                "spec":{"containers":[{"name":"agent","image":"customer:pinned"}]}}}})
}

fn registration() -> Value {
    let mut reg: KarsSRERegistration = serde_json::from_value(json!({
        "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSRERegistration",
        "metadata":{"name":"canonical","uid":"registration","resourceVersion":"1","generation":1},
        "spec":{"controller":{"namespace":{"name":"kars-system","uid":"system"},
            "deployment":{"name":"kars-controller","uid":"controller"},"release":"kars"},
            "sandbox":{"namespace":"kars-system","name":"sre","uid":"sre-source"},
            "runtimeNamespace":{"name":"kars-sre","uid":"sre-namespace"},"enabled":true}
    }))
    .unwrap();
    reg.status = Some(
        serde_json::from_value(json!({
            "phase":"Ready","observedGeneration":1,"legacySecretAccessDenied":true,
            "privacyRevision":crate::sre_privacy::REVISION,"privacyEpoch":reg.epoch(),
            "routerServiceAccountUid":"sre-router",
        }))
        .unwrap(),
    );
    serde_json::to_value(reg).unwrap()
}

#[derive(Default)]
struct State {
    objects: BTreeMap<String, Value>,
    calls: Vec<(String, String, Value)>,
    pods: Vec<Value>,
    allow_verb: Option<String>,
    sar_error: bool,
    revoke_on_consumer_read: bool,
    conflict: bool,
    fail_policy: bool,
    alias: bool,
    wrong_write_stamp: bool,
}

fn failure(code: u16) -> ResponseTemplate {
    ResponseTemplate::new(code).set_body_json(json!({"apiVersion":"v1","kind":"Status","code":code,
        "reason":if code==404 {"NotFound"} else if code==409 {"Conflict"} else {"Forbidden"},
        "status":"Failure","message":"PRIVATE_RESPONSE_SENTINEL"}))
}

fn merge(value: &mut Value, patch: &Value) {
    if let Some(entries) = patch.as_object() {
        if !value.is_object() {
            *value = json!({});
        }
        for (key, item) in entries {
            if item.is_null() {
                value.as_object_mut().unwrap().remove(key);
            } else {
                merge(&mut value[key], item);
            }
        }
    } else {
        *value = patch.clone();
    }
}

async fn fixture() -> (MockServer, Client, Arc<Mutex<State>>) {
    let server = MockServer::start().await;
    let state = Arc::new(Mutex::new(State::default()));
    let handler = state.clone();
    Mock::given(|_: &wiremock::Request| true).respond_with(move |request: &wiremock::Request| {
        let mut state = handler.lock().unwrap();
        let path = request.url.path();
        let body: Value = request.body_json().unwrap_or(Value::Null);
        let method = request.method.as_str();
        state.calls.push((method.into(), path.into(), body.clone()));
        if method == "POST" && path == "/apis/authorization.k8s.io/v1/subjectaccessreviews" {
            return ResponseTemplate::new(201).set_body_json(json!({
                "apiVersion":"authorization.k8s.io/v1","kind":"SubjectAccessReview",
                "spec":body["spec"],"status":{
                    "allowed": state.allow_verb.as_deref() == body["spec"]["resourceAttributes"]["verb"].as_str(),
                    "evaluationError":if state.sar_error {"PRIVATE_RESPONSE_SENTINEL"} else {""},
                }}));
        }
        if method == "GET" {
            if path == DEPLOY && state.revoke_on_consumer_read {
                state.allow_verb = Some("watch".into());
            }
            if let Some(value) = state.objects.get(path) {
                return ResponseTemplate::new(200).set_body_json(value);
            }
            if path == "/api/v1/namespaces/kars-normal/pods" {
                assert!(request.url.query_pairs().any(|(key, value)|
                    key == "labelSelector" && value == "kars.azure.com/sandbox=normal") || !state.objects.contains_key(DEPLOY));
                return ResponseTemplate::new(200).set_body_json(json!({
                    "apiVersion":"v1","kind":"PodList","metadata":{},"items":state.pods}));
            }
            if path.starts_with("/apis/admissionregistration.k8s.io/v1/validatingadmissionpolicies/") {
                return ResponseTemplate::new(200).set_body_json(json!({
                    "metadata":{"name":path.rsplit('/').next().unwrap(),"generation":1},
                    "spec":{"failurePolicy":if state.fail_policy {"Ignore"} else {"Fail"},"validations":[]},
                    "status":{"observedGeneration":1,"typeChecking":{}}}));
            }
            if path.starts_with("/apis/admissionregistration.k8s.io/v1/validatingadmissionpolicybindings/") {
                let name = path.rsplit('/').next().unwrap();
                return ResponseTemplate::new(200).set_body_json(json!({
                    "metadata":{"name":name},"spec":{"policyName":name,"validationActions":["Deny"]}}));
            }
            if path == "/api/v1/namespaces/kars-sre/secrets" {
                assert!(request.headers.get("accept").unwrap().to_str().unwrap().contains("PartialObjectMetadataList"));
                return ResponseTemplate::new(200).set_body_json(json!({
                    "apiVersion":"meta.k8s.io/v1","kind":"PartialObjectMetadataList","metadata":{},
                    "items":if state.alias { vec![json!({"metadata":{"name":"alias","uid":"alias","resourceVersion":"1",
                        "annotations":{"kubernetes.io/service-account.name":"sre-api-router"}}})] } else {vec![]}}));
            }
        }
        if (method == "POST" && path == SECRETS)
            || (method == "PATCH" && path.starts_with(&format!("{SECRETS}/")))
        {
            if state.conflict {
                return failure(409);
            }
            let key = if method == "POST" {
                format!("{SECRETS}/{}",body["metadata"]["name"].as_str().unwrap())
            } else { path.to_string() };
            let mut value = if method == "PATCH" {
                let existing = state.objects.get(&key).unwrap().clone();
                assert_eq!(body["metadata"]["uid"], existing["metadata"]["uid"]);
                assert_eq!(body["metadata"]["resourceVersion"], existing["metadata"]["resourceVersion"]);
                existing
            } else {
                json!({"apiVersion":"v1","kind":"Secret","metadata":{"uid":"new-secret","resourceVersion":"0"}})
            };
            let version = if method == "POST" {8} else {
                value["metadata"]["resourceVersion"].as_str().unwrap().parse::<u64>().unwrap()+1
            };
            merge(&mut value, &body);
            value["metadata"]["resourceVersion"] = version.to_string().into();
            for (key,material) in body["stringData"].as_object().into_iter().flatten() {
                value["data"][key] = STANDARD.encode(material.as_str().unwrap()).into();
            }
            value.as_object_mut().unwrap().remove("stringData");
            if state.wrong_write_stamp {
                value["metadata"]["annotations"][REVISION] = "wrong".into();
                value["metadata"]["annotations"].as_object_mut().unwrap().remove(EPOCH);
            }
            state.objects.insert(key, value.clone());
            return ResponseTemplate::new(if method == "POST" {201} else {200}).set_body_json(value);
        }
        if method == "PATCH" && path == DEPLOY {
            let value = state.objects.get_mut(path).unwrap();
            assert_eq!(body["metadata"]["uid"], value["metadata"]["uid"]);
            assert_eq!(body["metadata"]["resourceVersion"], value["metadata"]["resourceVersion"]);
            merge(value, &body);
            value["metadata"]["resourceVersion"] = "4".into();
            return ResponseTemplate::new(200).set_body_json(value.clone());
        }
        failure(404)
    }).mount(&server).await;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
    (server, client, state)
}

fn enroll(state: &mut State) -> String {
    let reg = registration();
    let epoch = reg["status"]["privacyEpoch"].as_str().unwrap().to_string();
    state.objects.insert(REG.into(), reg);
    state.objects.insert(
        "/api/v1/namespaces/kars-system".into(),
        json!({"metadata":{"name":"kars-system","uid":"system","resourceVersion":"1"}}),
    );
    state.objects.insert("/apis/apps/v1/namespaces/kars-system/deployments/kars-controller".into(),
        json!({"metadata":{"name":"kars-controller","namespace":"kars-system","uid":"controller","resourceVersion":"1"}}));
    state.objects.insert(
        "/apis/kars.azure.com/v1alpha1/namespaces/kars-system/karssandboxes/sre".into(),
        json!({
        "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSandbox",
        "metadata":{"name":"sre","namespace":"kars-system","uid":"sre-source","resourceVersion":"1",
            "labels":{"kars.azure.com/role":"sre"},"annotations":{NAMESPACE_UID:"sre-namespace"}},
        "spec":{"runtime":{"kind":"Hermes","hermes":{}},"inferenceRef":{"name":"sre-inference"}}}),
    );
    state.objects.insert("/api/v1/namespaces/kars-sre".into(), json!({
        "metadata":{"name":"kars-sre","uid":"sre-namespace","resourceVersion":"1","annotations":{
            "kars.azure.com/namespace-claim-version":"v1","kars.azure.com/sandbox-namespace":"kars-system",
            "kars.azure.com/sandbox-name":"sre",SOURCE_UID:"sre-source"}}}));
    epoch
}

fn secret_writes(state: &State) -> usize {
    state
        .calls
        .iter()
        .filter(|(method, path, _)| method != "GET" && path.starts_with(SECRETS))
        .count()
}

fn token_issuances(state: &State) -> usize {
    state
        .calls
        .iter()
        .filter(|(method, path, body)| {
            method != "GET"
                && path.starts_with(SECRETS)
                && body["stringData"]["control-token"].is_string()
        })
        .count()
}

#[tokio::test]
async fn standalone_control_issuance_requires_real_get_list_watch_denial_without_registration() {
    let (_server, client, state) = fixture().await;
    let projection = credentials::ensure(&client, &source(), &namespace())
        .await
        .unwrap();
    let state = state.lock().unwrap();
    let secret = &state.objects[&format!("{SECRETS}/{SECRET}")];
    assert_eq!(
        secret["metadata"]["annotations"][REVISION],
        crate::sre_privacy::REVISION
    );
    assert!(secret["metadata"]["annotations"].get(EPOCH).is_none());
    assert_eq!(projection.version, "new-secret:8");
    assert_eq!(secret_writes(&state), 1);
    for verb in ["get", "list", "watch"] {
        assert!(
            state
                .calls
                .iter()
                .any(|(_, _, body)| body["spec"]["resourceAttributes"]["verb"] == verb)
        );
    }
}

#[tokio::test]
async fn legacy_secret_authority_or_indeterminate_reviews_prevent_any_control_issuance() {
    for verb in ["get", "list", "watch", "evaluation-error"] {
        let (_server, client, state) = fixture().await;
        if verb == "evaluation-error" {
            state.lock().unwrap().sar_error = true;
        } else {
            state.lock().unwrap().allow_verb = Some(verb.into());
        }
        let error = credentials::ensure(&client, &source(), &namespace())
            .await
            .err()
            .unwrap();
        assert!(!error.contains("PRIVATE_RESPONSE_SENTINEL"));
        assert_eq!(secret_writes(&state.lock().unwrap()), 0);
    }
}

#[tokio::test]
async fn current_ready_epoch_is_required_and_recorded_before_control_token_creation() {
    let (_server, client, state) = fixture().await;
    let epoch = enroll(&mut state.lock().unwrap());
    credentials::ensure(&client, &source(), &namespace())
        .await
        .unwrap();
    let state = state.lock().unwrap();
    assert_eq!(
        state.objects[&format!("{SECRETS}/{SECRET}")]["metadata"]["annotations"][EPOCH],
        epoch
    );
    assert_eq!(
        state
            .calls
            .iter()
            .filter(|(_, path, _)| path.contains("/validatingadmissionpolicies/"))
            .count(),
        14
    );
}

#[tokio::test]
async fn stale_migrating_alias_or_unenforced_sre_authority_cannot_issue_or_reuse_controls() {
    for failure in [
        "Migrating",
        "revision",
        "source-uid",
        "generation",
        "alias",
        "policy",
    ] {
        let (_server, client, state) = fixture().await;
        {
            let mut state = state.lock().unwrap();
            let epoch = enroll(&mut state);
            state
                .objects
                .insert(format!("{SECRETS}/{SECRET}"), secret(Some(&epoch), true));
            match failure {
                "Migrating" => {
                    state.objects.get_mut(REG).unwrap()["status"]["phase"] = "Migrating".into()
                }
                "revision" => {
                    state.objects.get_mut(REG).unwrap()["status"]["privacyRevision"] = "old".into()
                }
                "source-uid" => {
                    state.objects.get_mut(REG).unwrap()["spec"]["sandbox"]["uid"] =
                        "replaced".into()
                }
                "generation" => {
                    state.objects.get_mut(REG).unwrap()["status"]["observedGeneration"] = 0.into()
                }
                "alias" => state.alias = true,
                _ => state.fail_policy = true,
            }
        }
        assert!(
            credentials::ensure(&client, &source(), &namespace())
                .await
                .is_err(),
            "{failure}"
        );
        let state = state.lock().unwrap();
        assert_eq!(token_issuances(&state), 0);
        if failure == "Migrating" {
            assert_eq!(secret_writes(&state), 0);
            assert!(
                state.objects[&format!("{SECRETS}/{SECRET}")]["metadata"]["annotations"]
                    .get(RETIRED)
                    .is_none()
            );
        } else {
            assert_eq!(
                state.objects[&format!("{SECRETS}/{SECRET}")]["metadata"]["annotations"][RETIRED],
                "true"
            );
        }
    }
}

#[tokio::test]
async fn retired_authority_requires_current_denial_revision_before_standalone_issuance() {
    for current in [false, true] {
        let (_server, client, state) = fixture().await;
        {
            let mut state = state.lock().unwrap();
            enroll(&mut state);
            let reg = state.objects.get_mut(REG).unwrap();
            reg["spec"]["enabled"] = false.into();
            reg["status"]["phase"] = "Retired".into();
            if !current {
                reg["status"]["privacyRevision"] = "old".into();
            }
        }
        assert_eq!(
            credentials::ensure(&client, &source(), &namespace())
                .await
                .is_ok(),
            current
        );
        assert_eq!(secret_writes(&state.lock().unwrap()), usize::from(current));
    }
}

#[tokio::test]
async fn unqualified_owned_control_token_rotates_with_cas_and_preserves_customer_metadata() {
    for enrolled in [false, true] {
        let (_server, client, state) = fixture().await;
        let epoch = {
            let mut state = state.lock().unwrap();
            state
                .objects
                .insert(format!("{SECRETS}/{SECRET}"), secret(None, false));
            state.objects.insert(DEPLOY.into(), deployment());
            enrolled.then(|| enroll(&mut state))
        };
        let projection = credentials::ensure(&client, &source(), &namespace())
            .await
            .unwrap();
        let state = state.lock().unwrap();
        let secret = &state.objects[&format!("{SECRETS}/{SECRET}")];
        assert_eq!(secret["metadata"]["uid"], "secret");
        assert_eq!(secret["metadata"]["labels"]["customer"], "preserve");
        assert_ne!(
            secret["data"]["control-token"],
            STANDARD.encode("x".repeat(64))
        );
        assert_eq!(
            secret["metadata"]["annotations"]
                .get(EPOCH)
                .and_then(Value::as_str),
            epoch.as_deref()
        );
        assert_eq!(projection.version, "secret:8");
        assert_eq!(secret_writes(&state), 1);
    }
}

#[tokio::test]
async fn already_qualified_control_is_reused_without_reissuing_or_touching_foreign_workloads() {
    let (_server, client, state) = fixture().await;
    state
        .lock()
        .unwrap()
        .objects
        .insert(format!("{SECRETS}/{SECRET}"), secret(None, true));
    assert_eq!(
        credentials::ensure(&client, &source(), &namespace())
            .await
            .unwrap()
            .version,
        "secret:7"
    );
    assert_eq!(secret_writes(&state.lock().unwrap()), 0);
}

#[tokio::test]
async fn foreign_secret_identity_or_unowned_consumers_never_receive_rotation() {
    for changed in [
        "label",
        "source",
        "namespace",
        "owner",
        "deployment",
        "orphan-pod",
    ] {
        let (_server, client, state) = fixture().await;
        {
            let mut state = state.lock().unwrap();
            let mut secret = secret(None, false);
            match changed {
                "label" => {
                    secret["metadata"]["labels"]["app.kubernetes.io/managed-by"] = "Helm".into()
                }
                "source" => secret["metadata"]["annotations"][SOURCE_UID] = "replacement".into(),
                "namespace" => {
                    secret["metadata"]["annotations"][NAMESPACE_UID] = "replacement".into()
                }
                "owner" => {
                    secret["metadata"]["ownerReferences"] =
                        json!([{"apiVersion":"v1","kind":"Pod","name":"foreign","uid":"foreign"}])
                }
                "deployment" => {
                    let mut deployment = deployment();
                    deployment["metadata"]["managedFields"] = json!([]);
                    state.objects.insert(DEPLOY.into(), deployment);
                }
                _ => state.pods.push(json!({"metadata":{"name":"unreviewed"}})),
            }
            state.objects.insert(format!("{SECRETS}/{SECRET}"), secret);
        }
        assert!(
            credentials::ensure(&client, &source(), &namespace())
                .await
                .is_err(),
            "{changed}"
        );
        assert_eq!(secret_writes(&state.lock().unwrap()), 0);
    }
}

#[tokio::test]
async fn privacy_revoked_during_consumer_inventory_retires_cache_without_issuing_a_token() {
    let (_server, client, state) = fixture().await;
    {
        let mut state = state.lock().unwrap();
        state
            .objects
            .insert(format!("{SECRETS}/{SECRET}"), secret(None, false));
        state.objects.insert(DEPLOY.into(), deployment());
        state.revoke_on_consumer_read = true;
    }
    assert!(
        credentials::ensure(&client, &source(), &namespace())
            .await
            .is_err()
    );
    let state = state.lock().unwrap();
    assert_eq!(token_issuances(&state), 0);
    assert_eq!(state.objects[DEPLOY]["spec"]["replicas"], 0);
    assert_eq!(
        state.objects[&format!("{SECRETS}/{SECRET}")]["metadata"]["annotations"][RETIRED],
        "true"
    );
}

#[tokio::test]
async fn standalone_privacy_loss_quarantines_then_rotates_instead_of_reusing_exposed_cache() {
    let (_server, client, state) = fixture().await;
    {
        let mut state = state.lock().unwrap();
        state
            .objects
            .insert(format!("{SECRETS}/{SECRET}"), secret(None, true));
        state.objects.insert(DEPLOY.into(), deployment());
        state.allow_verb = Some("watch".into());
    }
    assert!(
        credentials::ensure(&client, &source(), &namespace())
            .await
            .is_err()
    );
    {
        let mut state = state.lock().unwrap();
        assert_eq!(state.objects[DEPLOY]["spec"]["replicas"], 0);
        assert_eq!(
            state.objects[&format!("{SECRETS}/{SECRET}")]["data"]["control-token"],
            STANDARD.encode("x".repeat(64))
        );
        assert_eq!(token_issuances(&state), 0);
        state.allow_verb = None;
    }
    let projection = credentials::ensure(&client, &source(), &namespace())
        .await
        .unwrap();
    let state = state.lock().unwrap();
    let secret = &state.objects[&format!("{SECRETS}/{SECRET}")];
    assert!(secret["metadata"]["annotations"].get(RETIRED).is_none());
    assert_ne!(
        secret["data"]["control-token"],
        STANDARD.encode("x".repeat(64))
    );
    assert_eq!(projection.version, "secret:9");
    assert_eq!(token_issuances(&state), 1);
}

#[tokio::test]
async fn control_cas_conflicts_and_mismatched_write_results_are_not_reported_as_applied() {
    for conflict in [true, false] {
        let (_server, client, state) = fixture().await;
        {
            let mut state = state.lock().unwrap();
            state
                .objects
                .insert(format!("{SECRETS}/{SECRET}"), secret(None, false));
            state.objects.insert(DEPLOY.into(), deployment());
            state.conflict = conflict;
            state.wrong_write_stamp = !conflict;
        }
        let error = credentials::ensure(&client, &source(), &namespace())
            .await
            .err()
            .unwrap();
        assert!(!error.contains("PRIVATE_RESPONSE_SENTINEL"));
        assert_eq!(secret_writes(&state.lock().unwrap()), 1);
    }
}

#[tokio::test]
async fn cached_old_and_terminating_consumers_block_completion_until_they_are_gone() {
    let (_server, client, state) = fixture().await;
    let projection = credentials::ensure(&client, &source(), &namespace())
        .await
        .unwrap();
    let mut deployment: Deployment = serde_json::from_value(deployment()).unwrap();
    projection.decorate(&mut deployment);
    assert_eq!(
        deployment
            .spec
            .as_ref()
            .unwrap()
            .template
            .metadata
            .as_ref()
            .unwrap()
            .annotations
            .as_ref()
            .unwrap()["customer"],
        "preserve"
    );
    assert_eq!(
        deployment
            .spec
            .as_ref()
            .unwrap()
            .template
            .spec
            .as_ref()
            .unwrap()
            .containers[0]
            .image
            .as_deref(),
        Some("customer:pinned")
    );
    let old = json!({"metadata":{"name":"old","deletionTimestamp":"2026-09-08T00:00:00Z",
        "annotations":{VERSION:"old"}}});
    let current = json!({"metadata":{"name":"current","annotations":{VERSION:projection.version}}});
    state.lock().unwrap().pods = vec![old, current.clone()];
    assert!(
        !projection
            .consumers_current(&client, NS, "normal")
            .await
            .unwrap()
    );
    state.lock().unwrap().pods = vec![current];
    assert!(
        projection
            .consumers_current(&client, NS, "normal")
            .await
            .unwrap()
    );
}
