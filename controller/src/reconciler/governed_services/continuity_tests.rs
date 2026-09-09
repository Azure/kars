// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::sre_registration::{
    AGENT_SECRET, CONTROL_VERSION, EPOCH, KarsSRERegistration, OWNER, PRIVATE_SECRET, ROUTER_SA,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use wiremock::{Mock, MockServer, ResponseTemplate};

const REG: &str = "/apis/kars.azure.com/v1alpha1/karssreregistrations/canonical";
const SRE: &str = "/apis/kars.azure.com/v1alpha1/namespaces/kars-system/karssandboxes/sre";
const NORMAL: &str = "/apis/kars.azure.com/v1alpha1/namespaces/kars-system/karssandboxes/normal";

struct State {
    objects: BTreeMap<String, Value>,
    calls: Vec<(String, String, Value)>,
    allowed: bool,
}

fn failure(code: u16) -> ResponseTemplate {
    ResponseTemplate::new(code).set_body_json(json!({"kind":"Status","apiVersion":"v1","status":"Failure",
        "reason":if code==404 {"NotFound"} else {"Conflict"},"code":code,"message":"PRIVATE_SENTINEL"}))
}

fn merge(value: &mut Value, patch: &Value) {
    if let Some(object) = patch.as_object() {
        if !value.is_object() {
            *value = json!({});
        }
        for (key, item) in object {
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

fn registration() -> KarsSRERegistration {
    let mut reg: KarsSRERegistration = serde_json::from_value(json!({
        "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSRERegistration",
        "metadata":{"name":"canonical","uid":"registration","resourceVersion":"1","generation":1},
        "spec":{"controller":{"namespace":{"name":"kars-system","uid":"system"},
            "deployment":{"name":"kars-controller","uid":"controller"},"release":"kars"},
            "sandbox":{"namespace":"kars-system","name":"sre","uid":"source-sre"},
            "runtimeNamespace":{"name":"kars-sre","uid":"namespace-sre"},"enabled":true}
    }))
    .unwrap();
    reg.status = Some(
        serde_json::from_value(json!({"phase":"Ready","observedGeneration":1,
        "privacyEpoch":reg.epoch(),"privacyRevision":crate::sre_privacy::REVISION,
        "legacySecretAccessDenied":true,"routerServiceAccountUid":"private-sa"}))
        .unwrap(),
    );
    reg
}

fn initial() -> State {
    let reg = registration();
    let epoch = reg.epoch();
    let mut objects = BTreeMap::new();
    objects.insert(REG.into(), serde_json::to_value(&reg).unwrap());
    objects.insert(
        "/api/v1/namespaces/kars-system".into(),
        json!({"metadata":{"name":"kars-system","uid":"system","resourceVersion":"1"}}),
    );
    objects.insert("/apis/apps/v1/namespaces/kars-system/deployments/kars-controller".into(),
        json!({"metadata":{"name":"kars-controller","namespace":"kars-system","uid":"controller","resourceVersion":"1"}}));
    for name in ["sre", "normal"] {
        let namespace = format!("kars-{name}");
        let source_uid = format!("source-{name}");
        let namespace_uid = format!("namespace-{name}");
        let source = json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSandbox",
            "metadata":{"name":name,"namespace":"kars-system","uid":source_uid,"resourceVersion":"1","generation":1,
                "finalizers":[super::super::namespace_ownership::FINALIZER],
                "labels":if name=="sre" {json!({"kars.azure.com/role":"sre"})} else {json!({})},
                "annotations":{NAMESPACE_UID:namespace_uid}},
            "spec":{"runtime":{"kind":"Hermes","hermes":{}},"sandbox":{"isolation":"standard"},"inferenceRef":{"name":"inference"}},
            "status":{"phase":"Running","namespace":namespace}});
        objects.insert(
            format!("/apis/kars.azure.com/v1alpha1/namespaces/kars-system/karssandboxes/{name}"),
            source,
        );
        objects.insert(format!("/api/v1/namespaces/{namespace}"), json!({"metadata":{
            "name":namespace,"uid":namespace_uid,"resourceVersion":"1","annotations":{
                "kars.azure.com/namespace-claim-version":"v1","kars.azure.com/sandbox-namespace":"kars-system",
                "kars.azure.com/sandbox-name":name,SOURCE_UID:source_uid}}}));
        let version = format!("control-{name}:1");
        objects.insert(format!("/api/v1/namespaces/{namespace}/secrets/router-services-admin"), json!({
            "apiVersion":"v1","kind":"Secret","metadata":{"name":"router-services-admin","namespace":namespace,
                "uid":format!("control-{name}"),"resourceVersion":"1","labels":{"app.kubernetes.io/managed-by":"kars-controller"},
                "annotations":{SOURCE_UID:source_uid,NAMESPACE_UID:namespace_uid,EPOCH:epoch}},
            "data":{"control-token":STANDARD.encode("x".repeat(64))}}));
        objects.insert(format!("/apis/apps/v1/namespaces/{namespace}/deployments/{name}"), json!({
            "apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":name,"namespace":namespace,
                "uid":format!("deployment-{name}"),"resourceVersion":"1","generation":1,
                "labels":{"kars.azure.com/sandbox":name},
                "managedFields":[{"manager":crate::field_managers::CLAWSANDBOX,"operation":"Apply","apiVersion":"apps/v1",
                    "fieldsType":"FieldsV1","fieldsV1":{"f:spec":{}}}]},
            "spec":{"replicas":1,"selector":{"matchLabels":{"kars.azure.com/sandbox":name}},
                "template":{"metadata":{"annotations":{OWNER:"registration",EPOCH:epoch,CONTROL_VERSION:version}},
                    "spec":{"containers":[{"name":"inference-router","image":"router:pinned"}]}}},
            "status":{"observedGeneration":1,"updatedReplicas":1,"availableReplicas":0}}));
        objects.insert(format!("/api/v1/namespaces/{namespace}/pods"), json!({
            "apiVersion":"v1","kind":"PodList","metadata":{},"items":[{"metadata":{"name":format!("pod-{name}"),
                "uid":format!("pod-{name}"),"annotations":{EPOCH:epoch,CONTROL_VERSION:version}}}]}));
    }
    let annotations = json!({OWNER:"registration",EPOCH:epoch,SOURCE_UID:"source-sre",NAMESPACE_UID:"namespace-sre"});
    objects.insert(format!("/api/v1/namespaces/kars-sre/serviceaccounts/{ROUTER_SA}"),json!({
        "metadata":{"name":ROUTER_SA,"namespace":"kars-sre","uid":"private-sa","resourceVersion":"1","annotations":annotations},
        "automountServiceAccountToken":false}));
    for name in [PRIVATE_SECRET, AGENT_SECRET] {
        let mut annotations = annotations.clone();
        annotations["kars.azure.com/sre-tls-expiry"] = (chrono::Utc::now().timestamp() + 1_000_000)
            .to_string()
            .into();
        let values = if name == PRIVATE_SECRET {
            vec![
                ("server-cert.pem", "test"),
                ("server-key.pem", "test"),
                ("agent-token", "opaque"),
                ("agent-ca.crt", "ca"),
            ]
        } else {
            vec![
                ("token", "opaque"),
                ("ca.crt", "ca"),
                ("namespace", "kars-sre"),
            ]
        };
        let mut data: BTreeMap<_, _> = values
            .into_iter()
            .map(|(key, value)| (key.to_string(), STANDARD.encode(value)))
            .collect();
        if name == PRIVATE_SECRET {
            data.insert(
                "kube-expires-at".into(),
                STANDARD.encode((chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339()),
            );
            data.insert("kube-token".into(), STANDARD.encode("kube-token"));
        }
        objects.insert(format!("/api/v1/namespaces/kars-sre/secrets/{name}"),json!({
            "kind":"Secret","apiVersion":"v1","metadata":{"name":name,"namespace":"kars-sre","uid":name,"resourceVersion":"1",
                "annotations":annotations},"data":data}));
    }
    for (binding, role, rule) in [
        (
            "kars-sre-private-reader",
            "kars-sre-private-diagnostics",
            json!({"apiGroups":[""],"resources":["secrets","pods"],"verbs":["get","list","watch"]}),
        ),
        (
            "kars-sre-private-author",
            "kars-sre-action-author",
            json!({"apiGroups":["kars.azure.com"],"resources":["karssreactions"],"verbs":["create"]}),
        ),
        (
            "kars-sre-private-renew",
            "kars-sre-router-renew",
            json!({"apiGroups":["kars.azure.com"],"resources":["karssreregistrations"],"resourceNames":["canonical"],"verbs":["get","renew"]}),
        ),
    ] {
        objects.insert(
            format!("/apis/rbac.authorization.k8s.io/v1/clusterroles/{role}"),
            json!({
            "metadata":{"name":role},"rules":[rule]}),
        );
        objects.insert(format!("/apis/rbac.authorization.k8s.io/v1/clusterrolebindings/{binding}"),json!({
            "metadata":{"name":binding,"uid":binding,"resourceVersion":"1","annotations":annotations},
            "roleRef":{"apiGroup":"rbac.authorization.k8s.io","kind":"ClusterRole","name":role},
            "subjects":[{"kind":"ServiceAccount","name":ROUTER_SA,"namespace":"kars-sre"}]}));
    }
    objects.insert("/apis/rbac.authorization.k8s.io/v1/namespaces/kars-sre/roles/sre-api-self-renew".into(),json!({
        "metadata":{"name":"sre-api-self-renew","namespace":"kars-sre","uid":"renew-role","resourceVersion":"1","annotations":annotations},
        "rules":[{"apiGroups":[""],"resources":["serviceaccounts/token"],"resourceNames":[ROUTER_SA],"verbs":["create"]}]}));
    objects.insert("/apis/rbac.authorization.k8s.io/v1/namespaces/kars-sre/rolebindings/sre-api-self-renew".into(),json!({
        "metadata":{"name":"sre-api-self-renew","namespace":"kars-sre","uid":"renew-binding","resourceVersion":"1","annotations":annotations},
        "roleRef":{"apiGroup":"rbac.authorization.k8s.io","kind":"Role","name":"sre-api-self-renew"},
        "subjects":[{"kind":"ServiceAccount","name":ROUTER_SA,"namespace":"kars-sre"}]}));
    State {
        objects,
        calls: Vec::new(),
        allowed: false,
    }
}

async fn fixture() -> (MockServer, Client, Arc<Mutex<State>>) {
    let server = MockServer::start().await;
    let state = Arc::new(Mutex::new(initial()));
    let handler = state.clone();
    Mock::given(|_: &wiremock::Request|true).respond_with(move |request: &wiremock::Request| {
        let mut state=handler.lock().unwrap();
        let path=request.url.path();
        let method=request.method.as_str();
        let body:Value=request.body_json().unwrap_or(Value::Null);
        state.calls.push((method.into(),path.into(),body.clone()));
        if method=="GET" {
            if let Some(value)=state.objects.get(path) {return ResponseTemplate::new(200).set_body_json(value);}
            if path=="/api/v1/secrets" {
                return ResponseTemplate::new(200).set_body_json(json!({"apiVersion":"v1","kind":"SecretList","metadata":{},
                    "items":state.objects.values().filter(|value|value["metadata"]["name"]=="router-services-admin").collect::<Vec<_>>()}));
            }
            if path=="/api/v1/namespaces/kars-sre/secrets" {
                return ResponseTemplate::new(200).set_body_json(json!({"apiVersion":"meta.k8s.io/v1","kind":"PartialObjectMetadataList","metadata":{},"items":[]}));
            }
            if path.ends_with("/clusterrolebindings") || path.ends_with("/rolebindings") {
                return ResponseTemplate::new(200).set_body_json(json!({"apiVersion":"rbac.authorization.k8s.io/v1",
                    "kind":if path.ends_with("/clusterrolebindings") {"ClusterRoleBindingList"} else {"RoleBindingList"},"metadata":{},"items":[]}));
            }
            if path.contains("/validatingadmissionpolicies/") {
                return ResponseTemplate::new(200).set_body_json(json!({"metadata":{"name":path.rsplit('/').next().unwrap(),"generation":1},
                    "spec":{"failurePolicy":"Fail","validations":[]},"status":{"observedGeneration":1,"typeChecking":{}}}));
            }
            if path.contains("/validatingadmissionpolicybindings/") {
                let name=path.rsplit('/').next().unwrap();
                return ResponseTemplate::new(200).set_body_json(json!({"metadata":{"name":name},"spec":{"policyName":name,"validationActions":["Deny"]}}));
            }
        }
        if method=="POST" && path.ends_with("/subjectaccessreviews") {
            return ResponseTemplate::new(201).set_body_json(json!({"apiVersion":"authorization.k8s.io/v1","kind":"SubjectAccessReview",
                "spec":body["spec"],"status":{"allowed":state.allowed}}));
        }
        if method=="PATCH" {
            let key=path.strip_suffix("/status").unwrap_or(path);
            let Some(value)=state.objects.get_mut(key) else {return failure(404)};
            if !path.ends_with("/status") {
                assert_eq!(body["metadata"]["uid"],value["metadata"]["uid"]);
                assert_eq!(body["metadata"]["resourceVersion"],value["metadata"]["resourceVersion"]);
            }
            merge(value,&body);
            if let Some(material)=body["stringData"]["control-token"].as_str() {
                value["data"]["control-token"]=STANDARD.encode(material).into();
                value.as_object_mut().unwrap().remove("stringData");
            }
            value["metadata"]["resourceVersion"]=(value["metadata"]["resourceVersion"].as_str().unwrap().parse::<u32>().unwrap()+1).to_string().into();
            return ResponseTemplate::new(200).set_body_json(value.clone());
        }
        if method=="DELETE" {
            let Some(value)=state.objects.get(path) else {return failure(404)};
            assert_eq!(body["preconditions"]["uid"],value["metadata"]["uid"]);
            assert_eq!(body["preconditions"]["resourceVersion"],value["metadata"]["resourceVersion"]);
            state.objects.remove(path);
            return ResponseTemplate::new(200).set_body_json(json!({"apiVersion":"v1","kind":"Status","status":"Success"}));
        }
        failure(404)
    }).mount(&server).await;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
    (server, client, state)
}

fn context(client: Client) -> Arc<super::super::Context> {
    Arc::new(super::super::Context {
        client,
        wi_client_id: String::new(),
        inference_router_image: String::new(),
        sandbox_image: String::new(),
        openai_endpoint: String::new(),
        foundry_endpoint: String::new(),
        foundry_project_endpoint: String::new(),
        foundry_deployments: String::new(),
        imds_client_id: String::new(),
        content_safety_endpoint: String::new(),
        fedcred: None,
        byo_strict: false,
        dev_openai_api_key: String::new(),
        dev_provider: String::new(),
        dev_copilot_github_token: String::new(),
        anthropic_api_key: String::new(),
        anthropic_endpoint: String::new(),
        ollama_endpoint: String::new(),
        openai_moderation_api_key: String::new(),
        openai_moderation_endpoint: String::new(),
        dev_profile: false,
        cluster_name: None,
        cluster_uid: String::new(),
        agent_id_cache: Arc::new(crate::agent_id_provisioning::ProvisionerCache::new()),
    })
}

fn object<T: serde::de::DeserializeOwned>(state: &Arc<Mutex<State>>, key: &str) -> T {
    serde_json::from_value(state.lock().unwrap().objects[key].clone()).unwrap()
}

#[tokio::test]
async fn full_sandbox_reconciler_quarantines_canonical_sre_before_the_early_privacy_return() {
    let (_server, client, state) = fixture().await;
    state.lock().unwrap().allowed = true;
    let sandbox: KarsSandbox = object(&state, SRE);
    super::super::reconcile(Arc::new(sandbox), context(client))
        .await
        .unwrap();
    let state = state.lock().unwrap();
    assert_eq!(
        state.objects["/apis/apps/v1/namespaces/kars-sre/deployments/sre"]["spec"]["replicas"],
        0
    );
    assert_eq!(
        state.objects["/api/v1/namespaces/kars-sre/secrets/router-services-admin"]["metadata"]["annotations"]
            [credentials::RETIRED],
        "true"
    );
    assert!(
        state
            .calls
            .iter()
            .all(|(_, _, body)| body.get("stringData").is_none())
    );
    assert!(
        !state
            .calls
            .iter()
            .any(|(method, path, _)| method == "POST" && path.ends_with("/secrets"))
    );
}

#[tokio::test]
async fn full_sandbox_reconciler_does_not_quarantine_a_foreign_namespace() {
    let (_server, client, state) = fixture().await;
    {
        let mut state = state.lock().unwrap();
        state.allowed = true;
        state
            .objects
            .get_mut("/api/v1/namespaces/kars-sre")
            .unwrap()["metadata"]["annotations"][SOURCE_UID] = "foreign".into();
    }
    let sandbox: KarsSandbox = object(&state, SRE);
    assert!(
        super::super::reconcile(Arc::new(sandbox), context(client))
            .await
            .is_err()
    );
    let state = state.lock().unwrap();
    assert_eq!(
        state.objects["/apis/apps/v1/namespaces/kars-sre/deployments/sre"]["spec"]["replicas"],
        1
    );
    assert!(
        !state
            .calls
            .iter()
            .any(|(_, path, _)| path.contains("/secrets/"))
    );
}

#[tokio::test]
async fn interleaved_full_sre_and_sandbox_credential_reconcile_keep_qualified_epoch_during_health_delay()
 {
    let (_server, client, state) = fixture().await;
    let sandbox: KarsSandbox = object(&state, NORMAL);
    let namespace: Namespace = object(&state, "/api/v1/namespaces/kars-normal");
    for _ in 0..3 {
        let reg: KarsSRERegistration = object(&state, REG);
        crate::sre_authority::reconcile(&client, &reg)
            .await
            .unwrap();
        super::ensure(&client, &sandbox, &namespace).await.unwrap();
        let state = state.lock().unwrap();
        assert_eq!(state.objects[REG]["status"]["phase"], "Ready");
        for name in ["sre", "normal"] {
            assert_eq!(
                state.objects[&format!("/apis/apps/v1/namespaces/kars-{name}/deployments/{name}")]
                    ["spec"]["replicas"],
                1
            );
        }
        assert!(
            state
                .calls
                .iter()
                .all(|(_, _, body)| body.get("stringData").is_none())
        );
    }
    state.lock().unwrap().allowed = true;
    let reg: KarsSRERegistration = object(&state, REG);
    assert!(
        crate::sre_authority::reconcile(&client, &reg)
            .await
            .is_err()
    );
    assert!(super::ensure(&client, &sandbox, &namespace).await.is_err());
    let state = state.lock().unwrap();
    assert_eq!(state.objects[REG]["status"]["phase"], "Blocked");
    assert_eq!(
        state.objects["/apis/apps/v1/namespaces/kars-normal/deployments/normal"]["spec"]["replicas"],
        0
    );
}

#[tokio::test]
async fn credential_transition_qualifies_before_authority_dependent_sre_availability() {
    let (_server, client, state) = fixture().await;
    {
        let mut state = state.lock().unwrap();
        let mut old = state.objects["/api/v1/namespaces/kars-normal/pods"]["items"][0].clone();
        old["metadata"]["name"] = "old-cached-router".into();
        old["metadata"]["uid"] = "old-cached-router".into();
        old["metadata"]["deletionTimestamp"] = "2026-09-08T00:00:00Z".into();
        old["metadata"]["annotations"][CONTROL_VERSION] = "old-token".into();
        state
            .objects
            .get_mut("/api/v1/namespaces/kars-normal/pods")
            .unwrap()["items"]
            .as_array_mut()
            .unwrap()
            .push(old);
    }
    let reg: KarsSRERegistration = object(&state, REG);
    assert!(
        crate::sre_authority::reconcile(&client, &reg)
            .await
            .is_err()
    );
    let sandbox: KarsSandbox = object(&state, NORMAL);
    let namespace: Namespace = object(&state, "/api/v1/namespaces/kars-normal");
    let sre: KarsSandbox = object(&state, SRE);
    let sre_namespace: Namespace = object(&state, "/api/v1/namespaces/kars-sre");
    assert!(super::ensure(&client, &sandbox, &namespace).await.is_err());
    assert!(
        crate::sre_authority::pod::authorize(&client, &sre, &sre_namespace)
            .await
            .is_err()
    );
    {
        let mut state = state.lock().unwrap();
        assert_eq!(state.objects[REG]["status"]["phase"], "Migrating");
        assert_eq!(
            state.objects["/apis/apps/v1/namespaces/kars-normal/deployments/normal"]["spec"]["replicas"],
            1
        );
        assert!(
            state
                .calls
                .iter()
                .all(|(_, _, body)| body.get("stringData").is_none())
        );
        state
            .objects
            .get_mut("/api/v1/namespaces/kars-normal/pods")
            .unwrap()["items"]
            .as_array_mut()
            .unwrap()
            .retain(|pod| pod["metadata"]["uid"] != "old-cached-router");
        for name in ["sre", "normal"] {
            assert_eq!(
                state.objects[&format!("/apis/apps/v1/namespaces/kars-{name}/deployments/{name}")]
                    ["status"]["availableReplicas"],
                0
            );
        }
    }
    let reg: KarsSRERegistration = object(&state, REG);
    crate::sre_authority::reconcile(&client, &reg)
        .await
        .unwrap();
    super::ensure(&client, &sandbox, &namespace).await.unwrap();
    assert!(
        crate::sre_authority::pod::authorize(&client, &sre, &sre_namespace)
            .await
            .unwrap()
            .is_some()
    );
    let state = state.lock().unwrap();
    assert_eq!(state.objects[REG]["status"]["phase"], "Ready");
    assert_eq!(
        state.objects["/apis/apps/v1/namespaces/kars-sre/deployments/sre"]["status"]["availableReplicas"],
        0
    );
    assert_eq!(
        state.objects["/apis/apps/v1/namespaces/kars-normal/deployments/normal"]["status"]["availableReplicas"],
        0
    );
    assert!(
        state
            .calls
            .iter()
            .all(|(_, _, body)| body.get("stringData").is_none())
    );
}
