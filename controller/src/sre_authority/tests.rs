// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::sre_registration::*;
use base64::Engine;
use k8s_openapi::api::apps::v1::Deployment;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use wiremock::{Mock, MockServer, ResponseTemplate};

pub(super) fn registration() -> KarsSRERegistration {
    serde_json::from_value(json!({
        "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSRERegistration",
        "metadata":{"name":"canonical","uid":"registration","resourceVersion":"1","generation":1},
        "spec":{"controller":{"namespace":{"name":"kars-system","uid":"control-ns"},
            "deployment":{"name":"kars-controller","uid":"controller"},"release":"kars"},
            "sandbox":{"namespace":"kars-system","name":"sre","uid":"source"},
            "runtimeNamespace":{"name":"kars-sre","uid":"runtime-ns"},"enabled":true,
            "legacyBindings":[{"kind":"ClusterRoleBinding","name":"legacy","uid":"binding","resourceVersion":"1",
                "roleRef":{"apiGroup":"rbac.authorization.k8s.io","kind":"ClusterRole","name":"kars-sre-reader"},
                "subjects":[{"kind":"ServiceAccount","name":"sandbox","namespace":"kars-sre"},
                    {"kind":"User","name":"unrelated","apiGroup":"rbac.authorization.k8s.io"}]}]},
    })).unwrap()
}

fn sandbox() -> Value {
    json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSandbox",
        "metadata":{"name":"sre","namespace":"kars-system","uid":"source","resourceVersion":"1",
            "labels":{"kars.azure.com/role":"sre"},"annotations":{"kars.azure.com/namespace-uid":"runtime-ns"}},
        "spec":{"runtime":{"kind":"Hermes","hermes":{}},"inferenceRef":{"name":"sre-inference"}}})
}

fn namespace() -> Value {
    json!({"apiVersion":"v1","kind":"Namespace","metadata":{"name":"kars-sre","uid":"runtime-ns","resourceVersion":"1",
        "annotations":{"kars.azure.com/namespace-claim-version":"v1","kars.azure.com/sandbox-namespace":"kars-system",
            "kars.azure.com/sandbox-name":"sre","kars.azure.com/sandbox-uid":"source"}}})
}

fn binding() -> Value {
    json!({"apiVersion":"rbac.authorization.k8s.io/v1","kind":"ClusterRoleBinding",
        "metadata":{"name":"legacy","uid":"binding","resourceVersion":"1"},
        "roleRef":{"apiGroup":"rbac.authorization.k8s.io","kind":"ClusterRole","name":"kars-sre-reader"},
        "subjects":[{"kind":"ServiceAccount","name":"sandbox","namespace":"kars-sre"},
            {"kind":"User","name":"unrelated","apiGroup":"rbac.authorization.k8s.io"}]})
}

fn api_error(code: u16) -> ResponseTemplate {
    ResponseTemplate::new(code).set_body_json(json!({"apiVersion":"v1","kind":"Status","status":"Failure",
        "code":code,"reason":match code {404=>"NotFound",409=>"Conflict",422=>"Invalid",_=>"Forbidden"},
        "message":"PRIVATE_SENTINEL"}))
}

fn patched_binding(existing: &Value, patch: &Value, dry_run: bool) -> Value {
    let mut value = existing.clone();
    value["subjects"] = patch["subjects"].clone();
    if let Some(annotations) = patch["metadata"]["annotations"].as_object() {
        if !value["metadata"]["annotations"].is_object() {
            value["metadata"]["annotations"] = json!({});
        }
        for (key, annotation) in annotations {
            value["metadata"]["annotations"][key] = annotation.clone();
        }
    }
    if !dry_run {
        value["metadata"]["resourceVersion"] = (existing["metadata"]["resourceVersion"]
            .as_str()
            .unwrap()
            .parse::<u32>()
            .unwrap()
            + 1)
        .to_string()
        .into();
    }
    value
}

// Model only changed replicas: non-apply updates transfer that leaf to the
// updating manager, pruning empty ownership parents. Other spec fields survive.
fn model_replica_ownership(value: &mut Value, patch: &Value, manager: &str) {
    if value["spec"]["replicas"] == patch["spec"]["replicas"] {
        return;
    }
    if !value["metadata"]["managedFields"].is_array() {
        value["metadata"]["managedFields"] = json!([]);
    }
    let entries = value["metadata"]["managedFields"].as_array_mut().unwrap();
    for entry in entries.iter_mut() {
        if let Some(fields) = entry["fieldsV1"].as_object_mut()
            && let Some(spec) = fields.get_mut("f:spec").and_then(Value::as_object_mut)
        {
            spec.remove("f:replicas");
            if spec.is_empty() {
                fields.remove("f:spec");
            }
        }
    }
    entries.retain(|entry| {
        entry["fieldsV1"]
            .as_object()
            .is_some_and(|fields| !fields.is_empty())
    });
    if let Some(entry) = entries
        .iter_mut()
        .find(|entry| entry["manager"] == manager && entry["operation"] == "Update")
    {
        entry["fieldsV1"]["f:spec"]["f:replicas"] = json!({});
    } else {
        entries.push(
            json!({"manager":manager,"operation":"Update","apiVersion":"apps/v1",
            "fieldsType":"FieldsV1","fieldsV1":{"f:spec":{"f:replicas":{}}}}),
        );
    }
}

pub(super) struct State {
    pub(super) sandbox: Value,
    pub(super) namespace: Value,
    pub(super) binding: Value,
    pub(super) allowed: bool,
    pub(super) calls: Vec<(String, String, Value)>,
    pub(super) api_error: Option<u16>,
    pub(super) objects: BTreeMap<String, Value>,
    pub(super) watch_allowed: Option<(Option<String>, Option<String>)>,
    pub(super) metadata_requests: Vec<String>,
    pub(super) binding_patches: Vec<(String, bool, Value)>,
    pub(super) binding_patch_errors: BTreeMap<(String, bool), u16>,
    pub(super) status_error: Option<u16>,
}

pub(super) async fn fixture() -> (MockServer, Client, Arc<Mutex<State>>) {
    let state = Arc::new(Mutex::new(State {
        sandbox: sandbox(),
        namespace: namespace(),
        binding: binding(),
        allowed: false,
        calls: Vec::new(),
        api_error: None,
        objects: BTreeMap::new(),
        watch_allowed: None,
        metadata_requests: Vec::new(),
        binding_patches: Vec::new(),
        binding_patch_errors: BTreeMap::new(),
        status_error: None,
    }));
    let server = MockServer::start().await;
    let handler = state.clone();
    Mock::given(|_:&wiremock::Request|true).respond_with(move |request:&wiremock::Request| {
        let mut state=handler.lock().unwrap();
        let path=request.url.path();
        let body:Value=request.body_json().unwrap_or(Value::Null);
        let dry_run=request.url.query_pairs().any(|(key,value)|key=="dryRun" && value=="All");
        state.calls.push((request.method.to_string(),path.into(),body.clone()));
        if request.method == "PATCH" && body["subjects"].is_array() {
            state.binding_patches.push((path.into(),dry_run,body.clone()));
            if let Some(code)=state.binding_patch_errors.get(&(path.into(),dry_run)) {
                return api_error(*code);
            }
        }
        if request.method == "GET" && path=="/api/v1/namespaces/kars-sre/secrets" {
            state.metadata_requests.push(request.headers.get("accept").unwrap().to_str().unwrap().into());
        }
        if let Some(code)=state.api_error {return api_error(code)}
        if request.method == "GET" && let Some(value) = state.objects.get(path) {
            return ResponseTemplate::new(200).set_body_json(value);
        }
        if request.method == "DELETE" && let Some(value) = state.objects.get(path) {
            if body["preconditions"]["uid"] != value["metadata"]["uid"] ||
                body["preconditions"]["resourceVersion"] != value["metadata"]["resourceVersion"] { return api_error(409); }
            state.objects.remove(path);
            return ResponseTemplate::new(200).set_body_json(json!({"kind":"Status","apiVersion":"v1","status":"Success"}));
        }
        if request.method == "POST" && (path.ends_with("/secrets") || path.ends_with("/serviceaccounts")
            || path.ends_with("/ciliumnetworkpolicies") || path.ends_with("/configmaps")
            || path.ends_with("/roles") || path.ends_with("/rolebindings") || path.ends_with("/clusterrolebindings")) {
            if state.objects.contains_key(&format!("{path}/{}",body["metadata"]["name"].as_str().unwrap())) {
                return api_error(409);
            }
            let mut value = body.clone();
            value["metadata"]["uid"] = format!("uid-{}-{}", body["metadata"]["name"].as_str().unwrap(),state.calls.len()).into();
            value["metadata"]["resourceVersion"] = "1".into();
            state.objects.insert(format!("{path}/{}",body["metadata"]["name"].as_str().unwrap()),value.clone());
            return ResponseTemplate::new(201).set_body_json(value);
        }
        if request.method == "PUT" && (path.contains("/ciliumnetworkpolicies/")
            || path.contains("/configmaps/") || path.contains("/roles/")) {
            let Some(value) = state.objects.get(path) else { return api_error(404); };
            if body["metadata"]["uid"] != value["metadata"]["uid"]
                || body["metadata"]["resourceVersion"] != value["metadata"]["resourceVersion"] { return api_error(409); }
            let mut updated = body;
            updated["metadata"]["resourceVersion"] =
                (value["metadata"]["resourceVersion"].as_str().unwrap().parse::<u32>().unwrap()+1).to_string().into();
            state.objects.insert(path.into(),updated.clone());
            return ResponseTemplate::new(200).set_body_json(updated);
        }
        if request.method == "PATCH" && path.contains("/secrets/") {
            let Some(value) = state.objects.get_mut(path) else { return api_error(404); };
            if body["metadata"]["uid"] != value["metadata"]["uid"] ||
                body["metadata"]["resourceVersion"] != value["metadata"]["resourceVersion"] { return api_error(409); }
            for (key, annotation) in body["metadata"]["annotations"].as_object().unwrap() {
                value["metadata"]["annotations"][key] = annotation.clone();
            }
            for (key, material) in body["stringData"].as_object().unwrap() {
                value["data"][key] = base64::engine::general_purpose::STANDARD.encode(material.as_str().unwrap()).into();
            }
            value["metadata"]["resourceVersion"] =
                (value["metadata"]["resourceVersion"].as_str().unwrap().parse::<u32>().unwrap()+1).to_string().into();
            return ResponseTemplate::new(200).set_body_json(value.clone());
        }
        if request.method == "PATCH" && body["subjects"].is_array() && state.objects.contains_key(path) {
            let value=&state.objects[path];
            if body["metadata"]["uid"]!=value["metadata"]["uid"] {return api_error(422);}
            if body["metadata"]["resourceVersion"]!=value["metadata"]["resourceVersion"] {return api_error(409);}
            let value=patched_binding(value,&body,dry_run);
            if !dry_run {state.objects.insert(path.into(),value.clone());}
            return ResponseTemplate::new(200).set_body_json(value);
        }
        if request.method == "PATCH" && path.contains("/deployments/") && body["spec"]["replicas"].is_i64() {
            let Some(value) = state.objects.get_mut(path) else { return api_error(404); };
            if body["metadata"]["uid"] != value["metadata"]["uid"]
                || body["metadata"]["resourceVersion"] != value["metadata"]["resourceVersion"] { return api_error(409); }
            if !value["metadata"]["annotations"].is_object() { value["metadata"]["annotations"] = json!({}); }
            for (key, annotation) in body["metadata"]["annotations"].as_object().into_iter().flatten() {
                if annotation.is_null() { value["metadata"]["annotations"].as_object_mut().unwrap().remove(key); }
                else { value["metadata"]["annotations"][key] = annotation.clone(); }
            }
            let manager = request.url.query_pairs().find(|(key, _)| key == "fieldManager")
                .map(|(_, manager)| manager.into_owned()).unwrap_or_else(|| "test-client".into());
            model_replica_ownership(value, &body, &manager);
            value["spec"]["replicas"] = body["spec"]["replicas"].clone();
            value["metadata"]["generation"] = (value["metadata"]["generation"].as_i64().unwrap_or(1) + 1).into();
            value["metadata"]["resourceVersion"] =
                (value["metadata"]["resourceVersion"].as_str().unwrap().parse::<u32>().unwrap()+1).to_string().into();
            return ResponseTemplate::new(200).set_body_json(value.clone());
        }
        let value=match (request.method.as_str(),path) {
            ("GET","/apis/kars.azure.com/v1alpha1/karssreregistrations/canonical")=>serde_json::to_value(registration()).unwrap(),
            ("GET","/api/v1/namespaces/kars-system")=>json!({"metadata":{"name":"kars-system","uid":"control-ns","resourceVersion":"1"}}),
            ("GET","/apis/apps/v1/namespaces/kars-system/deployments/kars-controller")=>json!({
                "metadata":{"name":"kars-controller","namespace":"kars-system","uid":"controller","resourceVersion":"1",
                    "annotations":{"meta.helm.sh/release-name":"kars","meta.helm.sh/release-namespace":"kars-system"}}}),
            ("GET","/apis/kars.azure.com/v1alpha1/namespaces/kars-system/karssandboxes/sre")=>state.sandbox.clone(),
            ("GET","/api/v1/namespaces/kars-sre")=>state.namespace.clone(),
            ("GET","/api/v1/namespaces/kars-sre/secrets")=>json!({
                "apiVersion":"meta.k8s.io/v1","kind":"PartialObjectMetadataList","metadata":{},
                "items":state.objects.iter().filter(|(path,_)|path.starts_with("/api/v1/namespaces/kars-sre/secrets/"))
                    .map(|(_,object)|json!({"apiVersion":"meta.k8s.io/v1","kind":"PartialObjectMetadata","metadata":object["metadata"]}))
                    .collect::<Vec<_>>()}),
            ("GET","/apis/rbac.authorization.k8s.io/v1/clusterrolebindings")=>json!({
                "apiVersion":"rbac.authorization.k8s.io/v1","kind":"ClusterRoleBindingList","metadata":{},
                "items":std::iter::once(state.binding.clone()).chain(state.objects.values()
                    .filter(|object|object["kind"]=="ClusterRoleBinding").cloned()).collect::<Vec<_>>()}),
            ("GET","/apis/rbac.authorization.k8s.io/v1/rolebindings")=>json!({
                "apiVersion":"rbac.authorization.k8s.io/v1","kind":"RoleBindingList","metadata":{},
                "items":state.objects.values().filter(|object|object["kind"]=="RoleBinding").cloned().collect::<Vec<_>>()}),
            ("PATCH","/apis/rbac.authorization.k8s.io/v1/clusterrolebindings/legacy")=>{
                if body["metadata"]["uid"]!=state.binding["metadata"]["uid"] {return api_error(422)}
                if body["metadata"]["resourceVersion"]!=state.binding["metadata"]["resourceVersion"] {return api_error(409)}
                let value=patched_binding(&state.binding,&body,dry_run);
                if !dry_run {state.binding=value.clone();}
                value
            }
            ("POST","/apis/authorization.k8s.io/v1/subjectaccessreviews")=>json!({
                "apiVersion":"authorization.k8s.io/v1","kind":"SubjectAccessReview",
                "spec":body["spec"],"status":{"allowed":state.allowed || state.watch_allowed.as_ref().is_some_and(|(namespace,name)|
                    body["spec"]["resourceAttributes"]["verb"]=="watch"
                    && body["spec"]["resourceAttributes"]["namespace"].as_str()==namespace.as_deref()
                    && body["spec"]["resourceAttributes"]["name"].as_str()==name.as_deref())}}),
            ("PATCH","/apis/kars.azure.com/v1alpha1/karssreregistrations/canonical/status")=>{
                if let Some(code) = state.status_error { return api_error(code); }
                let mut reg=state.objects.get("/apis/kars.azure.com/v1alpha1/karssreregistrations/canonical")
                    .cloned().unwrap_or_else(||serde_json::to_value(registration()).unwrap());
                reg["status"]=body["status"].clone();
                reg["metadata"]["resourceVersion"]="2".into();
                state.objects.insert("/apis/kars.azure.com/v1alpha1/karssreregistrations/canonical".into(),reg.clone());
                reg
            }
            ("POST","/api/v1/namespaces/kars-sre/serviceaccounts/sre-api-router/token")=>json!({
                "apiVersion":"authentication.k8s.io/v1","kind":"TokenRequest","metadata":{},"spec":body["spec"],
                "status":{"token":"PRIVATE_KUBE_TOKEN","expirationTimestamp":(chrono::Utc::now()+chrono::Duration::hours(1)).to_rfc3339()}}),
            _=>return api_error(404),
        };
        ResponseTemplate::new(200).set_body_json(value)
    }).mount(&server).await;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let config = crate::kube_client_config::preserve_request_semantics(kube::Config::new(
        server.uri().parse().unwrap(),
    ));
    let client = Client::try_from(config).unwrap();
    (server, client, state)
}

#[tokio::test]
async fn canonical_source_and_namespace_uids_authorize_not_labels_or_names() {
    let (_server, client, state) = fixture().await;
    let reg = registration();
    assert!(live::verify(&client, &reg).await.is_ok());
    for target in ["source", "namespace", "foreign-workspace", "controller"] {
        let mut reg = registration();
        match target {
            "source" => reg.spec.sandbox.uid = "recreated".into(),
            "namespace" => reg.spec.runtime_namespace.uid = "recreated".into(),
            "controller" => reg.spec.controller.deployment.uid = "recreated".into(),
            _ => {
                state.lock().unwrap().namespace["metadata"]["annotations"]["kars.azure.com/sandbox-namespace"] =
                    "untrusted".into()
            }
        }
        assert!(live::verify(&client, &reg).await.is_err(), "{target}");
    }
    assert!(
        state
            .lock()
            .unwrap()
            .calls
            .iter()
            .all(|(method, _, _)| method == "GET")
    );
}

#[tokio::test]
async fn reviewed_retirement_preserves_other_subjects_and_has_cas_preconditions() {
    let (_server, client, state) = fixture().await;
    let reg = registration();
    let reviewed = bindings::review(&client, &reg).await.unwrap();
    bindings::retire_legacy(&client, &reg, &reviewed)
        .await
        .unwrap();
    let state = state.lock().unwrap();
    assert_eq!(
        state.binding["subjects"],
        json!([{"kind":"User","name":"unrelated","apiGroup":"rbac.authorization.k8s.io"}])
    );
    let patch = &state
        .calls
        .iter()
        .find(|(method, _, _)| method == "PATCH")
        .unwrap()
        .2;
    assert_eq!(patch["metadata"]["uid"], "binding");
    assert_eq!(patch["metadata"]["resourceVersion"], "1");
}

#[tokio::test]
async fn unreviewed_or_replaced_bindings_stop_before_mutation() {
    let (_server, client, state) = fixture().await;
    for changed in ["missing-review", "uid", "version"] {
        let mut reg = registration();
        match changed {
            "missing-review" => reg.spec.legacy_bindings.clear(),
            "uid" => reg.spec.legacy_bindings[0].uid = "other".into(),
            _ => reg.spec.legacy_bindings[0].resource_version = "other".into(),
        }
        assert!(bindings::review(&client, &reg).await.is_err());
    }
    assert!(
        state
            .lock()
            .unwrap()
            .calls
            .iter()
            .all(|(method, _, _)| method == "GET")
    );
}

#[tokio::test]
async fn real_authorization_review_denial_is_required_and_errors_do_not_hide_access() {
    let (_server, client, state) = fixture().await;
    check_secret_denial(&client, "kars-sre").await.unwrap();
    assert_eq!(
        state
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|(method, _, _)| method == "POST")
            .count(),
        18
    );
    state.lock().unwrap().allowed = true;
    assert!(check_secret_denial(&client, "kars-sre").await.is_err());
    state.lock().unwrap().api_error = Some(403);
    let error = check_secret_denial(&client, "kars-sre").await.unwrap_err();
    assert!(!error.contains("PRIVATE_SENTINEL"));
}

#[test]
fn proxy_projection_preserves_azure_identity_and_pinned_agent_image() {
    let projection = pod::Projection {
        epoch: "epoch".into(),
        registration_uid: "registration".into(),
        tls_expiry: "expiry".into(),
        credential_uid: "credential-uid".into(),
    };
    let mut spec = json!({"serviceAccountName":"sandbox","volumes":[],"containers":[
        {"name":"agent","image":"customer/hermes:pinned","env":[],"volumeMounts":[]},
        {"name":"inference-router","env":[],"volumeMounts":[]} ]});
    pod::project(&mut spec);
    assert_eq!(spec["serviceAccountName"], "sandbox");
    assert_eq!(spec["containers"][0]["image"], "customer/hermes:pinned");
    assert_eq!(spec["automountServiceAccountToken"], false);
    let agent = serde_json::to_string(&spec["containers"][0]).unwrap();
    assert!(!agent.contains(PRIVATE_SECRET));
    assert!(!agent.contains("router-kubernetes"));
    assert!(agent.contains("sre-api-agent"));
    assert!(agent.contains("127.0.0.1"));
    assert!(agent.contains("9446"));
    assert_eq!(
        pod::annotations(&projection, "agent")["azure.workload.identity/skip-containers"],
        "agent,egress-guard"
    );
}

#[test]
fn registration_is_cluster_scoped_and_requires_complete_exact_identity() {
    use kube::CustomResourceExt;
    let crd = KarsSRERegistration::crd();
    assert_eq!(crd.spec.scope, "Cluster");
    let mut reg = registration();
    reg.validate().unwrap();
    reg.spec.sandbox.namespace = "untrusted".into();
    assert!(reg.validate().is_err());
    reg = registration();
    reg.metadata.uid = None;
    assert!(reg.validate().is_err());
}

#[tokio::test]
async fn admission_requires_observed_enforced_policies_without_type_warnings() {
    let (_server, client, state) = fixture().await;
    let path = "/apis/admissionregistration.k8s.io/v1";
    for name in admission::POLICIES {
        state.lock().unwrap().objects.insert(format!("{path}/validatingadmissionpolicies/{name}"),json!({
            "metadata":{"name":name,"generation":2},"spec":{"failurePolicy":"Fail","validations":[]},
            "status":{"observedGeneration":2,"typeChecking":{}}}));
        state.lock().unwrap().objects.insert(format!("{path}/validatingadmissionpolicybindings/{name}"),json!({
            "metadata":{"name":name},"spec":{"policyName":name,"validationActions":["Deny","Audit"]}}));
    }
    admission::verify(&client).await.unwrap();
    let binding = format!(
        "{path}/validatingadmissionpolicybindings/{}",
        admission::POLICIES[0]
    );
    let original = state.lock().unwrap().objects[&binding].clone();
    for spec in [
        json!({}),
        json!({"policyName":admission::POLICIES[0],"validationActions":[]}),
        json!({"policyName":admission::POLICIES[0],"validationActions":["Audit"]}),
        json!({"policyName":"wrong-policy","validationActions":["Deny"]}),
    ] {
        state.lock().unwrap().objects.get_mut(&binding).unwrap()["spec"] = spec;
        assert!(admission::verify(&client).await.is_err());
    }
    state.lock().unwrap().objects.insert(binding, original);
    admission::verify(&client).await.unwrap();
    let policy = format!(
        "{path}/validatingadmissionpolicies/{}",
        admission::POLICIES[0]
    );
    state.lock().unwrap().objects.get_mut(&policy).unwrap()["status"]["typeChecking"] = json!({"expressionWarnings":[{"fieldRef":"spec.validations[0].expression","warning":"undeclared field"}]});
    assert!(admission::verify(&client).await.is_err());
    state.lock().unwrap().objects.get_mut(&policy).unwrap()["status"] =
        json!({"observedGeneration":1,"typeChecking":{}});
    assert!(admission::verify(&client).await.is_err());
    assert!(
        state
            .lock()
            .unwrap()
            .calls
            .iter()
            .all(|(method, _, _)| method == "GET")
    );
}

#[tokio::test]
async fn credentials_are_private_bound_and_renewable_without_agent_jwt() {
    let (_server, client, state) = fixture().await;
    let reg = registration();
    let authority = live::verify(&client, &reg).await.unwrap();
    let sa = credentials::ensure_service_account(&client, &reg, &authority)
        .await
        .unwrap();
    credentials::ensure_for_test(&client, &reg, &authority, &sa)
        .await
        .unwrap();
    let private_path = "/api/v1/namespaces/kars-sre/secrets/sre-api-router-identity";
    let agent_path = "/api/v1/namespaces/kars-sre/secrets/sre-api-agent";
    let decode = |value: &Value, key: &str| {
        String::from_utf8(
            base64::engine::general_purpose::STANDARD
                .decode(value["data"][key].as_str().unwrap())
                .unwrap(),
        )
        .unwrap()
    };
    let before = {
        let locked = state.lock().unwrap();
        let private = &locked.objects[private_path];
        let agent = &locked.objects[agent_path];
        assert_eq!(decode(private, "kube-token"), "PRIVATE_KUBE_TOKEN");
        assert_eq!(decode(agent, "token"), decode(private, "agent-token"));
        assert_eq!(decode(agent, "ca.crt"), decode(private, "agent-ca.crt"));
        assert_eq!(agent["data"].as_object().unwrap().len(), 3);
        assert!(!serde_json::to_string(agent).unwrap().contains("kube-token"));
        let request = locked
            .calls
            .iter()
            .find(|(_, path, _)| path.ends_with("/token"))
            .unwrap();
        assert_eq!(request.2["spec"]["boundObjectRef"]["kind"], "Secret");
        assert_eq!(
            request.2["spec"]["boundObjectRef"]["uid"],
            private["metadata"]["uid"]
        );
        assert!(decode(private, "server-key.pem").starts_with("-----BEGIN PRIVATE KEY-----"));
        private.clone()
    };
    let writes = state
        .lock()
        .unwrap()
        .calls
        .iter()
        .filter(|(method, _, _)| method == "PATCH")
        .count();
    credentials::ensure_for_test(&client, &reg, &authority, &sa)
        .await
        .unwrap();
    assert_eq!(
        writes,
        state
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|(method, _, _)| method == "PATCH")
            .count()
    );
    state.lock().unwrap().objects.get_mut(private_path).unwrap()["metadata"]["annotations"]["kars.azure.com/sre-tls-expiry"] =
        "0".into();
    credentials::ensure_for_test(&client, &reg, &authority, &sa)
        .await
        .unwrap();
    let locked = state.lock().unwrap();
    assert_ne!(
        decode(&before, "agent-token"),
        decode(&locked.objects[private_path], "agent-token")
    );
    assert_eq!(
        decode(&locked.objects[private_path], "agent-token"),
        decode(&locked.objects[agent_path], "token")
    );
}

#[tokio::test]
async fn incomplete_token_responses_never_publish_private_credentials() {
    for field in ["token", "expirationTimestamp"] {
        let (server, client, state) = fixture().await;
        let reg = registration();
        let authority = live::verify(&client, &reg).await.unwrap();
        let sa = credentials::ensure_service_account(&client, &reg, &authority)
            .await
            .unwrap();
        let mut status = json!({"token":"test-token","expirationTimestamp":
            (chrono::Utc::now()+chrono::Duration::hours(1)).to_rfc3339()});
        status.as_object_mut().unwrap().remove(field);
        Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path(format!(
                "/api/v1/namespaces/{RUNTIME_NAMESPACE}/serviceaccounts/{ROUTER_SA}/token"
            )))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "apiVersion":"authentication.k8s.io/v1","kind":"TokenRequest",
                "spec":{"audiences":[]},"status":status
            })))
            .with_priority(1)
            .expect(1)
            .mount(&server)
            .await;
        let error = credentials::ensure_for_test(&client, &reg, &authority, &sa)
            .await
            .unwrap_err();
        assert!(error.contains("TokenRequest omitted"), "{error}");
        assert!(!error.contains("test-token"));
        let locked = state.lock().unwrap();
        let private =
            &locked.objects["/api/v1/namespaces/kars-sre/secrets/sre-api-router-identity"];
        assert!(private["data"].get("kube-token").is_none());
    }
}

#[tokio::test]
async fn unsafe_authorization_or_recreated_targets_never_receive_private_material() {
    for problem in [
        "secret-access",
        "source-recreated",
        "namespace-recreated",
        "foreign-secret",
        "api-error",
    ] {
        let (_server, client, state) = fixture().await;
        let reg = registration();
        let authority = live::verify(&client, &reg).await.unwrap();
        let sa = credentials::ensure_service_account(&client, &reg, &authority)
            .await
            .unwrap();
        {
            let mut locked = state.lock().unwrap();
            locked.calls.clear();
            match problem {
                "secret-access" => locked.allowed = true,
                "source-recreated" => locked.sandbox["metadata"]["uid"] = "other".into(),
                "namespace-recreated" => locked.namespace["metadata"]["uid"] = "other".into(),
                "foreign-secret" => {
                    locked.objects.insert("/api/v1/namespaces/kars-sre/secrets/sre-api-router-identity".into(),
                    json!({"apiVersion":"v1","kind":"Secret","metadata":{"name":"sre-api-router-identity","namespace":"kars-sre","uid":"foreign","resourceVersion":"1"}}));
                }
                _ => locked.api_error = Some(403),
            }
        }
        let error = credentials::ensure_for_test(&client, &reg, &authority, &sa)
            .await
            .unwrap_err();
        assert!(!error.contains("PRIVATE_SENTINEL"));
        assert!(state.lock().unwrap().calls.iter().all(|(method,path,_)|
            method=="GET" || path.ends_with("/subjectaccessreviews")), "{problem}");
    }
}

#[tokio::test]
async fn migration_waits_for_consumers_and_retires_only_owned_private_material() {
    let (_server, client, state) = fixture().await;
    let mut reg = registration();
    reg.spec.legacy_consumer = Some(ConsumerReview {
        namespace: RUNTIME_NAMESPACE.into(),
        name: "sre".into(),
        uid: "consumer".into(),
        resource_version: "1".into(),
    });
    state.lock().unwrap().objects.insert("/apis/apps/v1/namespaces/kars-sre/deployments/sre".into(),
        json!({"metadata":{"name":"sre","namespace":"kars-sre","uid":"consumer","resourceVersion":"1"},
            "spec":{"replicas":0,"selector":{"matchLabels":{"app":"sre"}},"template":{"metadata":{},"spec":{"containers":[]}}}}));
    state.lock().unwrap().objects.insert(
        "/api/v1/namespaces/kars-sre/pods".into(),
        json!({"kind":"PodList","apiVersion":"v1","metadata":{},"items":[{"metadata":{"name":"old"},
            "spec":{"containers":[],"serviceAccountName":"sandbox"}}]}),
    );
    assert_eq!(
        migration::stop_legacy_consumer(&client, &reg)
            .await
            .unwrap_err(),
        migration::WAITING_FOR_CONSUMERS
    );
    assert!(
        state
            .lock()
            .unwrap()
            .calls
            .iter()
            .all(|(method, _, _)| method == "GET")
    );
    let authority = live::verify(&client, &reg).await.unwrap();
    let sa = credentials::ensure_service_account(&client, &reg, &authority)
        .await
        .unwrap();
    credentials::ensure_for_test(&client, &reg, &authority, &sa)
        .await
        .unwrap();
    credentials::retire(&client, &reg).await.unwrap();
    let locked = state.lock().unwrap();
    assert!(
        !locked
            .objects
            .contains_key("/api/v1/namespaces/kars-sre/secrets/sre-api-router-identity")
    );
    assert!(
        locked
            .calls
            .iter()
            .filter(|(method, _, _)| method == "DELETE")
            .all(
                |(_, _, body)| body["preconditions"]["uid"].as_str().is_some()
                    && body["preconditions"]["resourceVersion"].as_str().is_some()
            )
    );
}

fn control_consumer(state: &Arc<Mutex<State>>, reg: &KarsSRERegistration, replicas: i32) {
    let epoch = reg.epoch();
    {
        let mut locked = state.lock().unwrap();
        locked.objects.insert("/api/v1/secrets".into(),json!({
        "kind":"SecretList","apiVersion":"v1","metadata":{},"items":[{
            "kind":"Secret","apiVersion":"v1","metadata":{"name":"router-services-admin","namespace":"kars-sre","uid":"control","resourceVersion":"1",
                "labels":{"app.kubernetes.io/managed-by":"kars-controller"},
                "annotations":{"kars.azure.com/sandbox-uid":"source","kars.azure.com/namespace-uid":"runtime-ns",EPOCH:epoch}}}]}));
        locked.objects.insert("/apis/apps/v1/namespaces/kars-sre/deployments/sre".into(),json!({
        "metadata":{"name":"sre","namespace":"kars-sre","uid":"consumer","resourceVersion":"1","generation":2,
            "labels":{"kars.azure.com/sandbox":"sre"},
            "managedFields":[{"manager":crate::field_managers::CLAWSANDBOX,"operation":"Apply","apiVersion":"apps/v1",
                "fieldsType":"FieldsV1","fieldsV1":{"f:spec":{"f:replicas":{}}}}]},
        "spec":{"replicas":replicas,"selector":{"matchLabels":{"app":"sre"}},"template":{"metadata":{"annotations":{EPOCH:epoch,CONTROL_VERSION:"control:1"}},
            "spec":{"containers":[]}}},
        "status":{"observedGeneration":2,"updatedReplicas":0,"availableReplicas":0}}));
        locked.objects.insert(
            "/api/v1/namespaces/kars-sre/pods".into(),
            json!({
        "kind":"PodList","apiVersion":"v1","metadata":{},"items":[{"metadata":{"name":"old-router",
            "deletionTimestamp":"2026-09-08T00:00:00Z"},"spec":{"containers":[]}}]}),
        );
    }
}

#[tokio::test]
async fn retired_control_without_a_deployment_requires_verified_absent_consumers() {
    for problem in [
        "none",
        "unretired",
        "false-marker",
        "pod",
        "pods-api",
        "changed-claim",
    ] {
        let (server, client, state) = fixture().await;
        let reg = registration();
        control_consumer(&state, &reg, 0);
        {
            let mut locked = state.lock().unwrap();
            locked
                .objects
                .remove("/apis/apps/v1/namespaces/kars-sre/deployments/sre");
            let secret = &mut locked.objects.get_mut("/api/v1/secrets").unwrap()["items"][0];
            if problem != "unretired" {
                secret["metadata"]["annotations"]
                    [crate::reconciler::governed_services::credentials::RETIRED] =
                    if problem == "false-marker" {
                        "false"
                    } else {
                        "true"
                    }
                    .into();
            }
            if problem != "pod" {
                locked
                    .objects
                    .get_mut("/api/v1/namespaces/kars-sre/pods")
                    .unwrap()["items"] = json!([]);
            }
            if problem == "changed-claim" {
                locked.namespace["metadata"]["annotations"]["kars.azure.com/sandbox-uid"] =
                    "replacement".into();
            }
        }
        if problem == "pods-api" {
            Mock::given(wiremock::matchers::method("GET"))
                .and(wiremock::matchers::path("/api/v1/namespaces/kars-sre/pods"))
                .respond_with(api_error(403))
                .with_priority(1)
                .mount(&server)
                .await;
        }
        let before = state.lock().unwrap().objects.clone();
        let result = migration::rotate_owned_control_credentials(&client, &reg).await;
        assert_eq!(result.is_ok(), problem == "none", "{problem}: {result:?}");
        let locked = state.lock().unwrap();
        assert_eq!(locked.objects, before, "{problem}");
        assert!(locked.calls.iter().all(|(method, path, _)|
            method == "GET" || path.ends_with("/subjectaccessreviews")), "{problem}");
    }
}

#[tokio::test]
async fn newly_issued_control_without_deployment_preserves_only_qualified_empty_state() {
    for problem in [
        "none",
        "unqualified",
        "old-epoch",
        "invalid-token",
        "false-retired-marker",
        "changed-claim",
        "pod",
        "pods-api",
        "allowed",
    ] {
        let (server, client, state) = fixture().await;
        let mut reg = registration();
        reg.status = Some(
            serde_json::from_value(json!({
                "phase":"Ready", "observedGeneration":1,
                "legacySecretAccessDenied":true,
                "privacyRevision":crate::sre_privacy::REVISION,
                "privacyEpoch":reg.epoch()
            }))
            .unwrap(),
        );
        control_consumer(&state, &reg, 1);
        {
            let mut locked = state.lock().unwrap();
            locked
                .objects
                .remove("/apis/apps/v1/namespaces/kars-sre/deployments/sre");
            let secret = &mut locked.objects.get_mut("/api/v1/secrets").unwrap()["items"][0];
            secret["data"] = json!({"control-token":base64::engine::general_purpose::STANDARD.encode("a".repeat(64))});
            if problem == "old-epoch" {
                secret["metadata"]["annotations"][EPOCH] = "old".into();
            }
            if problem == "invalid-token" {
                secret["data"] = json!({});
            }
            if problem == "false-retired-marker" {
                secret["metadata"]["annotations"]
                    [crate::reconciler::governed_services::credentials::RETIRED] = "false".into();
            }
            if problem != "pod" {
                locked
                    .objects
                    .get_mut("/api/v1/namespaces/kars-sre/pods")
                    .unwrap()["items"] = json!([]);
            }
            if problem == "changed-claim" {
                locked.namespace["metadata"]["annotations"]["kars.azure.com/sandbox-uid"] =
                    "replacement".into();
            }
            locked.allowed = problem == "allowed";
        }
        if problem == "unqualified" {
            reg.status.as_mut().unwrap().phase = "Migrating".into();
        }
        if problem == "pods-api" {
            Mock::given(wiremock::matchers::method("GET"))
                .and(wiremock::matchers::path("/api/v1/namespaces/kars-sre/pods"))
                .respond_with(api_error(403))
                .with_priority(1)
                .mount(&server)
                .await;
        }
        let before = state.lock().unwrap().objects.clone();
        let result = migration::rotate_owned_control_credentials(&client, &reg).await;
        assert_eq!(result.is_ok(), problem == "none", "{problem}: {result:?}");
        let locked = state.lock().unwrap();
        assert_eq!(locked.objects, before, "{problem}");
        assert!(locked.calls.iter().all(|(method, path, _)|
            method == "GET" || path.ends_with("/subjectaccessreviews")), "{problem}");
    }
}

#[tokio::test]
async fn owned_control_retirement_waits_without_rotating_private_credentials() {
    for retirement in [
        "namespace",
        "secret",
        "source",
        "owner-gone",
        "namespace-gone",
    ] {
        let (server, client, state) = fixture().await;
        let reg = registration();
        let authority = live::verify(&client, &reg).await.unwrap();
        let sa = credentials::ensure_service_account(&client, &reg, &authority)
            .await
            .unwrap();
        credentials::ensure_for_test(&client, &reg, &authority, &sa)
            .await
            .unwrap();
        control_consumer(&state, &reg, 1);
        {
            let mut locked = state.lock().unwrap();
            if retirement == "source" {
                locked.sandbox["metadata"]["deletionTimestamp"] = "2026-10-15T00:00:00Z".into();
            } else {
                locked.namespace["metadata"]["deletionTimestamp"] = "2026-10-15T00:00:00Z".into();
            }
            if retirement == "secret" {
                locked.objects.get_mut("/api/v1/secrets").unwrap()["items"][0]["metadata"]["deletionTimestamp"] =
                    "2026-10-15T00:00:00Z".into();
            }
            locked.calls.clear();
        }
        if retirement == "namespace-gone" {
            let secret = state.lock().unwrap().objects["/api/v1/secrets"]["items"][0].clone();
            Mock::given(wiremock::matchers::method("GET"))
                .and(wiremock::matchers::path(
                    "/api/v1/namespaces/kars-sre/secrets/router-services-admin",
                ))
                .respond_with(ResponseTemplate::new(200).set_body_json(secret))
                .with_priority(1)
                .mount(&server)
                .await;
        }
        if retirement == "owner-gone" || retirement == "namespace-gone" {
            let path = if retirement == "owner-gone" {
                "/apis/kars.azure.com/v1alpha1/namespaces/kars-system/karssandboxes/sre"
            } else {
                "/api/v1/namespaces/kars-sre"
            };
            Mock::given(wiremock::matchers::method("GET"))
                .and(wiremock::matchers::path(path))
                .respond_with(api_error(404))
                .with_priority(1)
                .mount(&server)
                .await;
        }
        let before = state.lock().unwrap().objects.clone();
        let error = migration::rotate_owned_control_credentials(&client, &reg)
            .await
            .unwrap_err();
        assert!(migration::is_waiting(&error), "{retirement}: {error}");
        {
            let locked = state.lock().unwrap();
            assert_eq!(locked.objects, before, "{retirement}");
            assert!(
                locked
                    .calls
                    .iter()
                    .all(|(method, path, _)| method == "GET"
                        || path.ends_with("/subjectaccessreviews")),
                "{retirement}"
            );
        }
        state
            .lock()
            .unwrap()
            .objects
            .get_mut("/api/v1/secrets")
            .unwrap()["items"] = json!([]);
        migration::rotate_owned_control_credentials(&client, &reg)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn qualified_control_cleanup_does_not_interrupt_other_workers() {
    for case in [
        "empty",
        "current",
        "owner-gone",
        "old-secret",
        "old-pod",
        "old-version",
        "unqualified",
        "retired-secret",
        "allowed",
        "pods-api",
        "source-only",
    ] {
        let (server, client, state) = fixture().await;
        let mut reg = registration();
        reg.status = Some(
            serde_json::from_value(json!({
                "phase":"Ready", "observedGeneration":1,
                "legacySecretAccessDenied":true,
                "privacyRevision":crate::sre_privacy::REVISION,
                "privacyEpoch":reg.epoch()
            }))
            .unwrap(),
        );
        control_consumer(&state, &reg, 1);
        {
            let mut locked = state.lock().unwrap();
            if case == "source-only" {
                locked.sandbox["metadata"]["deletionTimestamp"] = "2026-10-21T23:06:00Z".into();
            } else {
                locked.namespace["metadata"]["deletionTimestamp"] = "2026-10-21T23:06:00Z".into();
            }
            let pod = &mut locked
                .objects
                .get_mut("/api/v1/namespaces/kars-sre/pods")
                .unwrap()["items"][0];
            pod["metadata"]["annotations"] =
                json!({EPOCH:reg.epoch(), CONTROL_VERSION:"control:1"});
            if case == "old-pod" {
                pod["metadata"]["annotations"][EPOCH] = "old".into();
            }
            if case == "old-version" {
                pod["metadata"]["annotations"][CONTROL_VERSION] = "control:0".into();
            }
            if case == "empty" {
                locked
                    .objects
                    .get_mut("/api/v1/namespaces/kars-sre/pods")
                    .unwrap()["items"] = json!([]);
                reg.status = None;
            }
            if case == "unqualified" {
                reg.status.as_mut().unwrap().phase = "Migrating".into();
            }
            let secret = &mut locked.objects.get_mut("/api/v1/secrets").unwrap()["items"][0];
            if case == "old-secret" {
                secret["metadata"]["annotations"][EPOCH] = "old".into();
            }
            if case == "retired-secret" {
                secret["metadata"]["annotations"]
                    [crate::reconciler::governed_services::credentials::RETIRED] = "true".into();
            }
            locked.allowed = case == "allowed";
        }
        if case == "pods-api" || case == "owner-gone" {
            let path = if case == "pods-api" {
                "/api/v1/namespaces/kars-sre/pods"
            } else {
                "/apis/kars.azure.com/v1alpha1/namespaces/kars-system/karssandboxes/sre"
            };
            Mock::given(wiremock::matchers::method("GET"))
                .and(wiremock::matchers::path(path))
                .respond_with(api_error(if case == "pods-api" { 403 } else { 404 }))
                .with_priority(1)
                .mount(&server)
                .await;
        }
        let before = state.lock().unwrap().objects.clone();
        let result = migration::rotate_owned_control_credentials(&client, &reg).await;
        assert_eq!(
            result.is_ok(),
            matches!(case, "empty" | "current" | "owner-gone"),
            "{case}: {result:?}"
        );
        let locked = state.lock().unwrap();
        assert_eq!(locked.objects, before, "{case}");
        assert!(locked.calls.iter().all(|(method, path, _)|
            method == "GET" || path.ends_with("/subjectaccessreviews")), "{case}");
    }
}

#[tokio::test]
async fn collected_control_namespace_requires_fresh_credential_absence() {
    for code in [404, 403, 500] {
        let (server, client, state) = fixture().await;
        let reg = registration();
        control_consumer(&state, &reg, 1);
        Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/api/v1/namespaces/kars-sre"))
            .respond_with(api_error(404))
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(
                "/api/v1/namespaces/kars-sre/secrets/router-services-admin",
            ))
            .respond_with(api_error(code))
            .with_priority(1)
            .mount(&server)
            .await;
        let before = state.lock().unwrap().objects.clone();
        let result = migration::rotate_owned_control_credentials(&client, &reg).await;
        assert_eq!(result.is_ok(), code == 404, "{code}: {result:?}");
        let locked = state.lock().unwrap();
        assert_eq!(locked.objects, before);
        assert!(locked.calls.iter().all(|(method, _, _)| method == "GET"));
    }
}

#[tokio::test]
async fn control_retirement_does_not_hide_changed_identity_or_invalid_claims() {
    for problem in [
        "namespace-uid",
        "source-uid",
        "claim-uid",
        "claim-version",
        "foreign-owner",
        "prestage",
        "namespace-rv",
        "secret-uid",
        "owner-gone-active",
        "namespace-api",
    ] {
        let (server, client, state) = fixture().await;
        let reg = registration();
        control_consumer(&state, &reg, 1);
        {
            let mut locked = state.lock().unwrap();
            locked.namespace["metadata"]["deletionTimestamp"] = "2026-10-15T00:00:00Z".into();
            match problem {
                "namespace-uid" => locked.namespace["metadata"]["uid"] = "replacement".into(),
                "source-uid" => locked.sandbox["metadata"]["uid"] = "replacement".into(),
                "claim-uid" => {
                    locked.namespace["metadata"]["annotations"]["kars.azure.com/sandbox-uid"] =
                        "replacement".into()
                }
                "claim-version" => {
                    locked.namespace["metadata"]["annotations"]["kars.azure.com/namespace-claim-version"] =
                        "untrusted".into()
                }
                "foreign-owner" => {
                    locked.namespace["metadata"]["ownerReferences"] = json!([{"apiVersion":"v1","kind":"Namespace","name":"foreign","uid":"foreign"}])
                }
                "prestage" => {
                    locked.namespace["metadata"]["annotations"]["kars.azure.com/namespace-prestage"] =
                        "bind-next-sandbox".into()
                }
                "namespace-rv" => locked.namespace["metadata"]["resourceVersion"] = "".into(),
                "secret-uid" => {
                    locked.objects.get_mut("/api/v1/secrets").unwrap()["items"][0]["metadata"]["uid"] =
                        "".into()
                }
                "owner-gone-active" => {
                    locked.namespace["metadata"]
                        .as_object_mut()
                        .unwrap()
                        .remove("deletionTimestamp");
                }
                _ => {}
            }
        }
        if problem == "namespace-api" {
            Mock::given(wiremock::matchers::method("GET"))
                .and(wiremock::matchers::path("/api/v1/namespaces/kars-sre"))
                .respond_with(api_error(403))
                .with_priority(1)
                .mount(&server)
                .await;
        } else if !matches!(problem, "namespace-uid" | "source-uid" | "secret-uid") {
            Mock::given(wiremock::matchers::method("GET"))
                .and(wiremock::matchers::path(
                    "/apis/kars.azure.com/v1alpha1/namespaces/kars-system/karssandboxes/sre",
                ))
                .respond_with(api_error(404))
                .with_priority(1)
                .mount(&server)
                .await;
        }
        let error = migration::rotate_owned_control_credentials(&client, &reg)
            .await
            .unwrap_err();
        assert!(!migration::is_waiting(&error), "{problem}: {error}");
        assert!(
            state
                .lock()
                .unwrap()
                .calls
                .iter()
                .all(|(method, _, _)| method == "GET"),
            "{problem}"
        );
    }
}

#[tokio::test]
async fn old_control_token_consumers_must_disappear_even_after_rollout_counters_converge() {
    let (_server, client, state) = fixture().await;
    let reg = registration();
    control_consumer(&state, &reg, 0);
    let error = migration::rotate_owned_control_credentials(&client, &reg)
        .await
        .unwrap_err();
    assert!(migration::is_waiting(&error));
    state
        .lock()
        .unwrap()
        .objects
        .get_mut("/api/v1/namespaces/kars-sre/pods")
        .unwrap()["items"] = json!([]);
    migration::rotate_owned_control_credentials(&client, &reg)
        .await
        .unwrap();
    assert!(
        state
            .lock()
            .unwrap()
            .calls
            .iter()
            .all(|(method, path, _)| method == "GET" || path.ends_with("/subjectaccessreviews"))
    );
}

#[tokio::test]
async fn reviewed_legacy_stop_preserves_existing_ownership_without_adopting_consumers() {
    for ownership in ["controller", "legacy", "wrong-label"] {
        let (server, client, state) = fixture().await;
        let mut reg = registration();
        reg.spec.legacy_consumer = Some(ConsumerReview {
            namespace: RUNTIME_NAMESPACE.into(),
            name: "sre".into(),
            uid: "consumer".into(),
            resource_version: "1".into(),
        });
        control_consumer(&state, &reg, 3);
        let path = "/apis/apps/v1/namespaces/kars-sre/deployments/sre";
        {
            let mut locked = state.lock().unwrap();
            let deployment = locked.objects.get_mut(path).unwrap();
            if ownership == "legacy" {
                deployment["metadata"]["managedFields"][0]["manager"] = "legacy-fixture".into();
            } else if ownership == "wrong-label" {
                deployment["metadata"]["labels"]["kars.azure.com/sandbox"] = "other".into();
            }
        }
        migration::stop_legacy_consumer(&client, &reg)
            .await
            .unwrap();
        let requests = server.received_requests().await.unwrap();
        let patches: Vec<_> = requests
            .iter()
            .filter(|request| request.method == "PATCH" && request.url.path() == path)
            .collect();
        assert_eq!(patches.len(), 1);
        let manager = patches[0]
            .url
            .query_pairs()
            .find(|(key, _)| key == "fieldManager")
            .map(|(_, value)| value.into_owned());
        assert_eq!(
            manager.as_deref(),
            (ownership == "controller").then_some(crate::field_managers::CLAWSANDBOX)
        );
        let body: Value = patches[0].body_json().unwrap();
        assert_eq!(body["metadata"]["uid"], "consumer");
        assert_eq!(body["metadata"]["resourceVersion"], "1");
        assert_eq!(body["spec"]["replicas"], 0);
        let deployment: Deployment =
            serde_json::from_value(state.lock().unwrap().objects[path].clone()).unwrap();
        assert_eq!(
            migration::controller_managed(&deployment, "sre"),
            ownership == "controller"
        );
    }
}

#[tokio::test]
async fn control_replica_update_models_ownership_loss_and_explicit_manager_preservation() {
    use kube::api::{Patch, PatchParams};
    for explicit in [false, true] {
        for replicas in [0, 3] {
            let (_server, client, state) = fixture().await;
            control_consumer(&state, &registration(), 3);
            let params = PatchParams {
                field_manager: explicit.then(|| crate::field_managers::CLAWSANDBOX.into()),
                ..Default::default()
            };
            let updated = Api::<Deployment>::namespaced(client, RUNTIME_NAMESPACE)
                .patch(
                    "sre",
                    &params,
                    &Patch::Merge(json!({
                        "metadata":{"uid":"consumer","resourceVersion":"1"},
                        "spec":{"replicas":replicas},
                    })),
                )
                .await
                .unwrap();
            assert_eq!(
                migration::controller_managed(&updated, "sre"),
                explicit || replicas == 3
            );
            assert_eq!(updated.metadata.uid.as_deref(), Some("consumer"));
        }
    }
}

#[tokio::test]
async fn control_restart_preserves_manager_and_exact_identity_fences() {
    let (server, client, state) = fixture().await;
    let reg = registration();
    control_consumer(&state, &reg, 3);
    let path = "/apis/apps/v1/namespaces/kars-sre/deployments/sre";
    let before = {
        let mut locked = state.lock().unwrap();
        let deployment = locked.objects.get_mut(path).unwrap();
        deployment["spec"]["template"]["metadata"]["annotations"] = json!({});
        deployment.clone()
    };
    Mock::given(wiremock::matchers::method("PATCH"))
        .and(wiremock::matchers::path(path))
        .respond_with(ResponseTemplate::new(200).set_body_json(&before))
        .expect(1)
        .with_priority(1)
        .mount(&server)
        .await;
    assert!(migration::is_waiting(
        &migration::rotate_owned_control_credentials(&client, &reg)
            .await
            .unwrap_err()
    ));
    let requests = server.received_requests().await.unwrap();
    let patches: Vec<_> = requests
        .iter()
        .filter(|request| request.method == "PATCH" && request.url.path() == path)
        .collect();
    assert_eq!(patches.len(), 1);
    let request = patches[0];
    assert!(
        request.url.query_pairs().any(
            |(key, value)| key == "fieldManager" && value == crate::field_managers::CLAWSANDBOX
        )
    );
    let body: Value = request.body_json().unwrap();
    assert_eq!(body["metadata"]["uid"], before["metadata"]["uid"]);
    assert_eq!(
        body["metadata"]["resourceVersion"],
        before["metadata"]["resourceVersion"]
    );
    assert_eq!(
        body["spec"]["template"]["metadata"]["annotations"][EPOCH],
        reg.epoch()
    );
    assert_eq!(
        body["spec"]["template"]["metadata"]["annotations"][CONTROL_VERSION],
        "control:1"
    );
    assert!(body["spec"].get("replicas").is_none());
}

#[tokio::test]
async fn control_rotation_drains_unready_consumers_and_restores_exact_replica_intent() {
    let (server, client, state) = fixture().await;
    let reg = registration();
    control_consumer(&state, &reg, 3);
    let path = "/apis/apps/v1/namespaces/kars-sre/deployments/sre";
    let pods = "/api/v1/namespaces/kars-sre/pods";
    let marker = "kars.azure.com/sre-control-drain";
    assert!(migration::is_waiting(
        &migration::rotate_owned_control_credentials(&client, &reg)
            .await
            .unwrap_err()
    ));
    {
        let locked = state.lock().unwrap();
        assert_eq!(locked.objects[path]["spec"]["replicas"], 0);
        assert_eq!(
            locked.objects[path]["metadata"]["annotations"][marker],
            format!("{}:3", reg.epoch())
        );
    }
    // Neither stale Deployment status nor an empty Pod list alone completes the drain.
    state.lock().unwrap().objects.get_mut(pods).unwrap()["items"] = json!([]);
    assert!(migration::is_waiting(
        &migration::rotate_owned_control_credentials(&client, &reg)
            .await
            .unwrap_err()
    ));
    assert_eq!(state.lock().unwrap().objects[path]["spec"]["replicas"], 0);
    state.lock().unwrap().objects.get_mut(path).unwrap()["status"] =
        json!({"observedGeneration":3,"replicas":0,"availableReplicas":0});
    state.lock().unwrap().objects.get_mut(pods).unwrap()["items"] = json!([{
        "metadata":{"name":"terminating","deletionTimestamp":"2026-10-12T00:00:00Z"},
        "spec":{"containers":[]}
    }]);
    assert!(migration::is_waiting(
        &migration::rotate_owned_control_credentials(&client, &reg)
            .await
            .unwrap_err()
    ));
    assert_eq!(state.lock().unwrap().objects[path]["spec"]["replicas"], 0);
    state.lock().unwrap().objects.get_mut(pods).unwrap()["items"] = json!([]);
    assert!(migration::is_waiting(
        &migration::rotate_owned_control_credentials(&client, &reg)
            .await
            .unwrap_err()
    ));
    {
        let locked = state.lock().unwrap();
        assert_eq!(locked.objects[path]["spec"]["replicas"], 3);
        assert!(
            locked.objects[path]["metadata"]["annotations"]
                .get(marker)
                .is_none()
        );
        let patches: Vec<_> = locked
            .calls
            .iter()
            .filter(|(method, _, _)| method == "PATCH")
            .collect();
        assert_eq!(patches.len(), 2);
        for ((_, target, body), version) in patches.into_iter().zip(["1", "2"]) {
            assert_eq!(target, path);
            assert_eq!(body["metadata"]["uid"], "consumer");
            assert_eq!(body["metadata"]["resourceVersion"], version);
        }
        let deployment: Deployment = serde_json::from_value(locked.objects[path].clone()).unwrap();
        assert!(migration::controller_managed(&deployment, "sre"));
    }
    let requests = server.received_requests().await.unwrap();
    let patches: Vec<_> = requests
        .iter()
        .filter(|request| request.method == "PATCH" && request.url.path() == path)
        .collect();
    assert_eq!(patches.len(), 2);
    for request in patches {
        assert!(request.url.query_pairs().any(
            |(key, value)| key == "fieldManager" && value == crate::field_managers::CLAWSANDBOX
        ));
    }
    // Restored containers need not be Ready to complete credential retirement.
    migration::rotate_owned_control_credentials(&client, &reg)
        .await
        .unwrap();
}

#[tokio::test]
async fn control_drain_rejects_changed_epoch_or_replica_intent_without_writing() {
    for (marker, replicas) in [
        ("wrong:3".to_string(), 0),
        ("invalid".to_string(), 0),
        (format!("{}:3", registration().epoch()), 4),
    ] {
        let (_server, client, state) = fixture().await;
        let reg = registration();
        control_consumer(&state, &reg, replicas);
        let path = "/apis/apps/v1/namespaces/kars-sre/deployments/sre";
        state.lock().unwrap().objects.get_mut(path).unwrap()["metadata"]["annotations"] =
            json!({"kars.azure.com/sre-control-drain":marker});
        let error = migration::rotate_owned_control_credentials(&client, &reg)
            .await
            .unwrap_err();
        assert!(!migration::is_waiting(&error));
        assert!(
            state
                .lock()
                .unwrap()
                .calls
                .iter()
                .all(|(method, _, _)| method != "PATCH")
        );
    }
}

#[tokio::test]
async fn control_drain_and_restore_propagate_compare_and_swap_conflicts() {
    for restoring in [false, true] {
        let (server, client, state) = fixture().await;
        let reg = registration();
        control_consumer(&state, &reg, if restoring { 0 } else { 3 });
        let path = "/apis/apps/v1/namespaces/kars-sre/deployments/sre";
        if restoring {
            let mut locked = state.lock().unwrap();
            locked.objects.get_mut(path).unwrap()["metadata"]["annotations"] =
                json!({"kars.azure.com/sre-control-drain":format!("{}:3", reg.epoch())});
            locked
                .objects
                .get_mut("/api/v1/namespaces/kars-sre/pods")
                .unwrap()["items"] = json!([]);
        }
        let before = state.lock().unwrap().objects[path].clone();
        Mock::given(wiremock::matchers::method("PATCH"))
            .and(wiremock::matchers::path(path))
            .respond_with(api_error(409))
            .with_priority(1)
            .mount(&server)
            .await;
        let error = migration::rotate_owned_control_credentials(&client, &reg)
            .await
            .unwrap_err();
        assert!(!migration::is_waiting(&error));
        assert!(!error.contains("PRIVATE_SENTINEL"));
        assert_eq!(state.lock().unwrap().objects[path], before);
    }
}

#[tokio::test]
async fn control_drain_requires_live_complete_consumer_identity_before_writing() {
    for field in [
        "uid",
        "resourceVersion",
        "generation",
        "deletionTimestamp",
        "managedFields",
        "labels",
    ] {
        let (_server, client, state) = fixture().await;
        let reg = registration();
        control_consumer(&state, &reg, 3);
        {
            let mut locked = state.lock().unwrap();
            let metadata = locked
                .objects
                .get_mut("/apis/apps/v1/namespaces/kars-sre/deployments/sre")
                .unwrap()["metadata"]
                .as_object_mut()
                .unwrap();
            if field == "deletionTimestamp" {
                metadata.insert(field.into(), "2026-10-12T00:00:00Z".into());
            } else {
                metadata.remove(field);
            }
        }
        let error = migration::rotate_owned_control_credentials(&client, &reg)
            .await
            .unwrap_err();
        assert!(!migration::is_waiting(&error));
        assert!(
            state
                .lock()
                .unwrap()
                .calls
                .iter()
                .all(|(method, _, _)| method != "PATCH")
        );
    }
}
