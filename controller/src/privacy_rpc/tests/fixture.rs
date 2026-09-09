// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::service_observer::{Binding, Grant, Recipient};
use k8s_openapi::ByteString;
use std::{collections::BTreeMap, sync::Mutex};
use wiremock::{Mock, MockServer, ResponseTemplate};

pub const SANDBOX: &str = "/apis/kars.azure.com/v1alpha1/namespaces/workspace/karssandboxes/agent";
pub const GRANT: &str =
    "/apis/kars.azure.com/v1alpha1/namespaces/workspace/karscredentialgrants/workspace";
pub const SOURCE: &str = "/api/v1/namespaces/kars-agent/secrets/router-services-observer";
pub const REG: &str = "/apis/kars.azure.com/v1alpha1/karssreregistrations/canonical";
pub const ALIASES: &str = "/api/v1/namespaces/kars-sre/secrets";
pub const TOKEN: &str = "oooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooo";

#[derive(Default)]
pub struct Data {
    pub objects: BTreeMap<String, serde_json::Value>,
    pub calls: Vec<(String, String, serde_json::Value)>,
    pub alias: bool,
    pub policy: bool,
    pub allowed: bool,
    pub delay: bool,
    pub writes: bool,
}

fn merge(value: &mut serde_json::Value, patch: &serde_json::Value) {
    if let Some(fields) = patch.as_object() {
        if !value.is_object() {
            *value = json!({});
        }
        for (key, entry) in fields {
            if entry.is_null() {
                value.as_object_mut().unwrap().remove(key);
            } else {
                merge(&mut value[key], entry);
            }
        }
    } else {
        *value = patch.clone();
    }
}

pub fn namespace(name: &str, uid: &str) -> serde_json::Value {
    json!({"apiVersion":"v1","kind":"Namespace","metadata":{"name":name,"uid":uid,"resourceVersion":"1",
        "labels":{"kubernetes.io/metadata.name":name}},"spec":{"finalizers":["kubernetes"]}})
}

pub fn enroll(data: &mut Data) -> String {
    let mut registration: crate::sre_registration::KarsSRERegistration = serde_json::from_value(json!({
        "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSRERegistration",
        "metadata":{"name":"canonical","uid":"registration","resourceVersion":"1","generation":1},
        "spec":{"controller":{"namespace":{"name":"kars-system","uid":"system"},
            "deployment":{"name":"kars-controller","uid":"controller-deploy"},"release":"kars"},
            "sandbox":{"namespace":"kars-system","name":"sre","uid":"sre-source"},
            "runtimeNamespace":{"name":"kars-sre","uid":"sre-runtime"},"enabled":true}
    })).unwrap();
    let epoch = registration.epoch();
    registration.status = Some(serde_json::from_value(json!({"phase":"Ready","observedGeneration":1,
        "legacySecretAccessDenied":true,"privacyRevision":crate::sre_privacy::REVISION,"privacyEpoch":epoch,
        "routerServiceAccountUid":"sre-router"})).unwrap());
    data.objects
        .insert(REG.into(), serde_json::to_value(registration).unwrap());
    data.objects.insert("/apis/apps/v1/namespaces/kars-system/deployments/kars-controller".into(),json!({
        "metadata":{"name":"kars-controller","namespace":"kars-system","uid":"controller-deploy","resourceVersion":"1"}
    }));
    data.objects.insert("/apis/kars.azure.com/v1alpha1/namespaces/kars-system/karssandboxes/sre".into(),json!({
        "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSandbox",
        "metadata":{"name":"sre","namespace":"kars-system","uid":"sre-source","resourceVersion":"1",
            "labels":{"kars.azure.com/role":"sre"},"annotations":{"kars.azure.com/namespace-uid":"sre-runtime"}},
        "spec":{"runtime":{"kind":"Hermes","hermes":{}},"inferenceRef":{"name":"test"}}
    }));
    let mut ns = namespace("kars-sre", "sre-runtime");
    ns["metadata"]["annotations"] = json!({"kars.azure.com/namespace-claim-version":"v1",
        "kars.azure.com/sandbox-namespace":"kars-system","kars.azure.com/sandbox-name":"sre","kars.azure.com/sandbox-uid":"sre-source"});
    data.objects
        .insert("/api/v1/namespaces/kars-sre".into(), ns);
    data.objects.insert("/api/v1/namespaces/kars-sre/serviceaccounts/sre-api-router".into(),json!({
        "metadata":{"name":"sre-api-router","namespace":"kars-sre","uid":"sre-router","resourceVersion":"1"}
    }));
    epoch
}

pub fn bind(data: &mut Data, request: &wire::Request) {
    let binding = Binding {
        capability: crate::service_observer::CAPABILITY.into(),
        identity: request.identity.clone(),
        grant: Grant {
            namespace: "workspace".into(),
            name: "workspace".into(),
            uid: "grant-uid".into(),
            generation: 1,
        },
        recipients: request.recipients.clone(),
        privacy_revision: crate::sre_privacy::REVISION.into(),
        privacy_epoch: request.epoch.clone(),
        server_name: "observer-target-uid.kars.internal".into(),
        ca_pem: request.verifier.ca_pem.clone(),
        workspace_uid: "workspace-uid".into(),
        expires_at: chrono::Utc::now().timestamp() + 600,
        verifier: Some(request.verifier.clone()),
    };
    data.objects.insert(SOURCE.into(),json!({"apiVersion":"v1","kind":"Secret","type":"Opaque",
        "metadata":{"name":crate::service_observer::SECRET,"namespace":"kars-agent","uid":"observer-secret","resourceVersion":"1",
            "labels":{"app.kubernetes.io/managed-by":"kars-controller"},"annotations":{"kars.azure.com/sandbox-uid":"target-uid",
                "kars.azure.com/namespace-uid":"runtime-uid","kars.azure.com/services-privacy-revision":crate::sre_privacy::REVISION}},
        "data":{"observation-token":ByteString(TOKEN.as_bytes().to_vec()),
            "config.json":ByteString(serde_json::to_vec(&binding).unwrap())}}));
    if let Some(epoch) = &request.epoch {
        data.objects.get_mut(SOURCE).unwrap()["metadata"]["annotations"]
            [crate::sre_registration::EPOCH] = epoch.clone().into();
    }
    data.objects.get_mut(SANDBOX).unwrap()["status"]["serviceObservation"]["privacyEpoch"] =
        json!(request.epoch);
}

pub async fn fixture() -> (
    MockServer,
    Arc<ServerState>,
    Arc<Mutex<Data>>,
    wire::Request,
) {
    let server = MockServer::start().await;
    let data = Arc::new(Mutex::new(Data::default()));
    let endpoint = Endpoint {
        capability: wire::CAPABILITY.into(),
        namespace: "kars-system".into(),
        namespace_uid: "system".into(),
        controller_uid: "controller-sa".into(),
        service_uid: "service".into(),
        port: wire::PORT,
        descriptor_uid: "descriptor".into(),
        tls_uid: "tls".into(),
        tls_version: "1".into(),
        server_name: "privacy-system.kars.internal".into(),
        ca_pem: "-----BEGIN CERTIFICATE-----fixture".into(),
        expires_at: chrono::Utc::now().timestamp() + 3600,
    };
    let request = wire::Request {
        capability: wire::CAPABILITY.into(),
        purpose: wire::PURPOSE.into(),
        target: wire::Target {
            workspace: "workspace".into(),
            workspace_uid: "workspace-uid".into(),
            name: "agent".into(),
            uid: "target-uid".into(),
            namespace_uid: "runtime-uid".into(),
        },
        grant_uid: "grant-uid".into(),
        grant_generation: 1,
        recipients: vec![Recipient {
            namespace: "bridge".into(),
            namespace_uid: "bridge-uid".into(),
            name: "bff".into(),
            uid: "writer".into(),
        }],
        credential_version: "observer-secret:1".into(),
        identity: json!({"sandbox":{"namespace":"workspace","name":"agent","uid":"target-uid"},
            "namespace_uid":"runtime-uid","task":null,"task_authorization":null,"task_generation":null,"managed":true}),
        scope_id: "current-scope".into(),
        operation: wire::Operation::Learned,
        epoch: None,
        nonce: "a".repeat(64),
        verifier: endpoint.clone(),
    };
    {
        let mut d = data.lock().unwrap();
        for (name, uid) in [
            ("kars-system", "system"),
            ("workspace", "workspace-uid"),
            ("bridge", "bridge-uid"),
            ("kars-agent", "runtime-uid"),
        ] {
            d.objects
                .insert(format!("/api/v1/namespaces/{name}"), namespace(name, uid));
        }
        d.objects.get_mut("/api/v1/namespaces/kars-agent").unwrap()["metadata"]["annotations"] = json!({
            "kars.azure.com/namespace-claim-version":"v1","kars.azure.com/sandbox-namespace":"workspace",
            "kars.azure.com/sandbox-name":"agent","kars.azure.com/sandbox-uid":"target-uid"});
        d.objects.insert(SANDBOX.into(),json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSandbox",
            "metadata":{"name":"agent","namespace":"workspace","uid":"target-uid","resourceVersion":"1","generation":1,
                "annotations":{"kars.azure.com/namespace-uid":"runtime-uid"}},
            "spec":{"runtime":{"kind":"OpenClaw","openclaw":{}},"inferenceRef":{"name":"test"}},
            "status":{"serviceObservation":{"capability":crate::service_observer::CAPABILITY,"phase":"Ready","reason":"Test",
                "version":"observer-secret:1","grant":{"name":"workspace","uid":"grant-uid"},
                "secret":{"name":crate::service_observer::SECRET,"uid":"observer-secret"},"namespaceUid":"runtime-uid",
                "privacyRevision":crate::sre_privacy::REVISION,"privacyEpoch":null}}}));
        d.objects.insert(GRANT.into(),json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsCredentialGrant",
            "metadata":{"name":"workspace","namespace":"workspace","uid":"grant-uid","generation":1,"resourceVersion":"1"},
            "spec":{"workspaceUid":"workspace-uid","enabled":true,"writers":[{"namespace":"bridge","name":"bff","uid":"writer"}],
                "observationTargets":[{"kind":"KarsSandbox","namespace":"workspace","name":"agent","uid":"target-uid"}]},
            "status":{"phase":"Ready","observedGeneration":1,"reason":"Test","conditions":[{
                "type":"WriterReady","status":"True","reason":"Test","message":"Test","observedGeneration":1,
                "lastTransitionTime":"2026-01-01T00:00:00Z"}]}}));
        for (ns, name, uid) in [
            ("bridge", "bff", "writer"),
            ("kars-system", "kars-controller", "controller-sa"),
        ] {
            d.objects.insert(format!("/api/v1/namespaces/{ns}/serviceaccounts/{name}"),json!({
                "apiVersion":"v1","kind":"ServiceAccount","metadata":{"name":name,"namespace":ns,"uid":uid,"resourceVersion":"1"}}));
        }
        for path in [
            "/api/v1/namespaces/bridge",
            "/api/v1/namespaces/bridge/serviceaccounts/bff",
        ] {
            let m = &mut d.objects.get_mut(path).unwrap()["metadata"];
            m["finalizers"] = json!(["kars.azure.com/credential-reader-grant-uid"]);
            m["annotations"]["kars.azure.com/credential-reader-grant-uid"] = "controller-sa".into();
            m["labels"]["kars.azure.com/credential-reader-grant-uid"] = "bridge-uid".into();
        }
        let meta = |name: &str, uid: &str| {
            json!({"name":name,"namespace":"kars-system","uid":uid,"resourceVersion":"1",
            "annotations":{wire::CONTROLLER_UID:"controller-sa",wire::NAMESPACE_UID:"system"}})
        };
        d.objects.insert(format!("/api/v1/namespaces/kars-system/secrets/{}",wire::SECRET),
            json!({"apiVersion":"v1","kind":"Secret","metadata":meta(wire::SECRET,"tls"),"type":"Opaque"}));
        d.objects.insert(format!("/api/v1/namespaces/kars-system/configmaps/{}",wire::DESCRIPTOR),
            json!({"apiVersion":"v1","kind":"ConfigMap","metadata":meta(wire::DESCRIPTOR,"descriptor"),
                "data":{"config.json":serde_json::to_string(&endpoint).unwrap()}}));
        d.objects.insert(format!("/api/v1/namespaces/kars-system/services/{}",wire::SERVICE),
            json!({"apiVersion":"v1","kind":"Service","metadata":meta(wire::SERVICE,"service"),"spec":{"type":"ClusterIP",
                "clusterIP":"10.0.0.20","ports":[{"port":9448,"protocol":"TCP","targetPort":9448}],
                "selector":{"app.kubernetes.io/name":"kars","app.kubernetes.io/component":"controller",
                    wire::REVISION_LABEL:endpoint.revision()}}}));
        bind(&mut d, &request);
    }
    let captured = data.clone();
    Mock::given(|_: &wiremock::Request| true).respond_with(move |r: &wiremock::Request| {
        let mut d=captured.lock().unwrap(); let path=r.url.path(); let body=r.body_json().unwrap_or(serde_json::Value::Null);
        d.calls.push((r.method.to_string(),path.into(),body));
        if d.writes && (r.method=="PATCH" || r.method=="POST") && path.starts_with("/api/v1/namespaces/kars-system/") {
            let body=r.body_json::<serde_json::Value>().unwrap();
            let key=if r.method=="POST" {format!("{path}/{}",body["metadata"]["name"].as_str().unwrap())} else {path.into()};
            let mut value=if r.method=="PATCH" {
                let Some(old)=d.objects.get(&key) else {return ResponseTemplate::new(404)};
                assert_eq!(old["metadata"]["uid"],body["metadata"]["uid"]);
                assert_eq!(old["metadata"]["resourceVersion"],body["metadata"]["resourceVersion"]);
                old.clone()
            } else {json!({"metadata":{"uid":format!("created-{}",d.calls.len()),"resourceVersion":"0"}})};
            let next=value["metadata"]["resourceVersion"].as_str().unwrap().parse::<u64>().unwrap()+1;
            merge(&mut value,&body);
            for (key,entry) in body["stringData"].as_object().into_iter().flatten() {
                value["data"][key]=json!(ByteString(entry.as_str().unwrap().as_bytes().to_vec()));
            }
            value.as_object_mut().unwrap().remove("stringData");
            value["metadata"]["resourceVersion"]=next.to_string().into();
            d.objects.insert(key,value.clone());
            return ResponseTemplate::new(if r.method=="POST" {201}else{200}).set_body_json(value);
        }
        if r.method=="POST" && path.ends_with("/subjectaccessreviews") {
            return ResponseTemplate::new(201).set_body_json(json!({"apiVersion":"authorization.k8s.io/v1","kind":"SubjectAccessReview",
                "spec":r.body_json::<serde_json::Value>().unwrap()["spec"],"status":{"allowed":d.allowed}}));
        }
        if r.method=="POST" && path.ends_with("/selfsubjectreviews") {
            return ResponseTemplate::new(201).set_body_json(json!({"apiVersion":"authentication.k8s.io/v1","kind":"SelfSubjectReview",
                "status":{"userInfo":{"username":"system:serviceaccount:kars-system:kars-controller","uid":"controller-sa"}}}));
        }
        if r.method=="GET" {
            if let Some(value)=d.objects.get(path) { return ResponseTemplate::new(200).set_body_json(value); }
            if path.contains("/validatingadmissionpolicies/") { return ResponseTemplate::new(200).set_body_json(json!({
                "metadata":{"name":path.rsplit('/').next().unwrap(),"generation":1},"spec":{"failurePolicy":if d.policy {"Ignore"}else{"Fail"}},
                "status":{"observedGeneration":1,"typeChecking":{}}})); }
            if path.contains("/validatingadmissionpolicybindings/") { return ResponseTemplate::new(200).set_body_json(json!({
                "metadata":{},"spec":{"policyName":path.rsplit('/').next().unwrap(),"validationActions":["Deny"]}})); }
            if path==ALIASES {
                assert!(r.headers.get("accept").unwrap().to_str().unwrap().contains("PartialObjectMetadataList"));
                let response=ResponseTemplate::new(200).set_body_json(json!({"metadata":{},"items":if d.alias {
                    vec![json!({"metadata":{"name":"PRIVATE_ALIAS","uid":"alias","resourceVersion":"1",
                        "annotations":{"kubernetes.io/service-account.name":"sre-api-router"}}})]}else{vec![]}}));
                return if d.delay {response.set_delay(Duration::from_secs(9))}else{response};
            }
        }
        ResponseTemplate::new(404).set_body_json(json!({"apiVersion":"v1","kind":"Status","status":"Failure","code":404,
            "reason":"NotFound","message":"PRIVATE_ERROR"}))
    }).mount(&server).await;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
    let state = Arc::new(ServerState {
        client,
        endpoint: RwLock::new(Some(endpoint)),
        capacity: Arc::new(Semaphore::new(4)),
    });
    (server, state, data, request)
}
