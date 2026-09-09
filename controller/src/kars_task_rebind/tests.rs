// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use serde_json::Value;
use std::{collections::BTreeMap, sync::Mutex};
use wiremock::{Mock, MockServer, ResponseTemplate};
#[path = "tests/suspension.rs"]
mod suspension;

const TASK: &str = "/apis/kars.azure.com/v1alpha1/namespaces/work/karstasks/run";
const SANDBOX: &str = "/apis/kars.azure.com/v1alpha1/namespaces/work/karssandboxes/run";
const RUNTIME: &str = "/api/v1/namespaces/kars-run";
const DEPLOYMENT: &str = "/apis/apps/v1/namespaces/kars-run/deployments/run";
const RECEIPT: &str = "/apis/kars.azure.com/v1alpha1/namespaces/work/karsreceipts/run";

#[test]
fn runtime_replicas_honor_explicit_suspension_and_credential_holds() {
    for suspended in [None, Some(false), Some(true)] {
        for held in [false, true] {
            let mut sandbox: crate::crd::KarsSandbox = serde_json::from_value(json!({
                "apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsSandbox",
                "metadata":{"name":"run","namespace":"work"},
                "spec":{"inferenceRef":{"name":"policy"},"suspended":suspended}
            }))
            .unwrap();
            if held {
                sandbox.annotations_mut().insert(HOLD.into(), String::new());
            }
            assert_eq!(
                runtime_replicas(&sandbox),
                i64::from(!suspended.unwrap_or(false) && !held)
            );
        }
    }
}

#[derive(Default)]
struct State {
    objects: BTreeMap<String, Value>,
    calls: Vec<(String, String, Value)>,
    pods: Vec<Value>,
}

fn merge(value: &mut Value, patch: &Value) {
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

fn ready(task: &mut KarsTask) {
    task.status = Some(super::super::ready_status(
        None,
        task.metadata.generation,
        task.envelope_digest(),
        Vec::new(),
    ));
}

async fn fixture() -> (
    MockServer,
    Arc<Ctx>,
    Arc<Mutex<State>>,
    crate::kars_team::KarsTeam,
) {
    let binding = |key: &str| {
        json!({"grant":{"name":"workspace","uid":"grant"},"sources":[{
        "scope":"workspace","source":{"name":"kars-credential-input-workspace","uid":"source"},"keys":[key]}]})
    };
    let team:crate::kars_team::KarsTeam=serde_json::from_value(json!({
        "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTeam",
        "metadata":{"name":"team","namespace":"work","uid":"team-uid","generation":2,"resourceVersion":"1"},
        "spec":{"charter":"Keep the team working","envelope":{"tier":3,"authorityCeiling":3,"delegationDepth":2},
            "blueprint":{"model":{"provider":"azure-openai","deployment":"test"},"credentialBindings":binding("SLACK_BOT_TOKEN")}}
    })).unwrap();
    let mut task:KarsTask=serde_json::from_value(json!({
        "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTask",
        "metadata":{"name":"run","namespace":"work","uid":"task-uid","generation":1,"resourceVersion":"1",
            "finalizers":[FINALIZER],"annotations":{"kars.azure.com/team-role":"taskforce"},
            "ownerReferences":[{"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTeam","name":"team","uid":"team-uid","controller":true}]},
        "spec":{"objective":"Keep existing data","envelope":{"tier":2,"authorityCeiling":2,"delegationDepth":1},
            "execution":{"launch":true},"parentRef":{"name":"team-principal"},
            "blueprint":{"model":{"provider":"azure-openai","deployment":"test"},"credentialBindings":binding("TELEGRAM_BOT_TOKEN")}}
    })).unwrap();
    ready(&mut task);
    task.status.as_mut().unwrap().sandbox_ref =
        Some(crate::mcp_server::LocalObjectRef { name: "run".into() });
    task.status.as_mut().unwrap().execution_phase = Some("Running".into());
    let mut parent = task.clone();
    parent.metadata.name = Some("team-principal".into());
    parent.metadata.uid = Some("principal".into());
    // Real Team principals and their runs share the same immutable Team owner.
    parent.metadata.owner_references = task.metadata.owner_references.clone();
    parent.metadata.annotations = None;
    parent.spec.parent_ref = None;
    parent.spec.envelope = team.spec.envelope.clone();
    parent.spec.blueprint = team.spec.blueprint.clone();
    ready(&mut parent);
    let state = Arc::new(Mutex::new(State::default()));
    {
        let mut s = state.lock().unwrap();
        s.objects
            .insert(TASK.into(), serde_json::to_value(&task).unwrap());
        s.objects.insert(
            "/apis/kars.azure.com/v1alpha1/namespaces/work/karstasks/team-principal".into(),
            serde_json::to_value(parent).unwrap(),
        );
        s.objects.insert(
            "/apis/kars.azure.com/v1alpha1/namespaces/work/karsteams/team".into(),
            serde_json::to_value(&team).unwrap(),
        );
        s.objects.insert(
            "/api/v1/namespaces/work".into(),
            json!({"metadata":{"name":"work","uid":"work-ns","resourceVersion":"1"}}),
        );
        s.objects.insert("/apis/kars.azure.com/v1alpha1/namespaces/work/karscredentialgrants/workspace".into(),json!({
            "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsCredentialGrant",
            "metadata":{"name":"workspace","namespace":"work","uid":"grant","resourceVersion":"1","generation":1},
            "spec":{"workspaceUid":"work-ns","writers":[],"enabled":true},
            "status":{"phase":"Ready","observedGeneration":1,"reason":"Test"}}));
        s.objects.insert("/api/v1/namespaces/work/secrets/kars-credential-input-workspace".into(),json!({
            "apiVersion":"v1","kind":"Secret","type":"Opaque","metadata":{"name":"kars-credential-input-workspace","namespace":"work",
                "uid":"source","resourceVersion":"1","annotations":{"kars.azure.com/credential-purpose":"agent-input-v2",
                    "kars.azure.com/credential-workspace":"work","kars.azure.com/credential-target-kind":"Workspace",
                    "kars.azure.com/credential-target":"work","kars.azure.com/credential-grant-uid":"grant",
                    "kars.azure.com/credential-binding-intent":"explicit-reference-v2","kars.azure.com/credential-import-revision":""}},
            "data":{"TELEGRAM_BOT_TOKEN":k8s_openapi::ByteString(b"old".to_vec()),"SLACK_BOT_TOKEN":k8s_openapi::ByteString(b"new".to_vec())}}));
        s.objects.insert(SANDBOX.into(),json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSandbox",
            "metadata":{"name":"run","namespace":"work","uid":"sandbox-uid","resourceVersion":"1","generation":1,
                "annotations":{"kars.azure.com/namespace-uid":"runtime-uid"},
                "ownerReferences":[{"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTask","name":"run","uid":"task-uid","controller":true}]},
            "spec":{"runtime":{"kind":"OpenClaw","openclaw":{}},"inferenceRef":{"name":"run-inference"},"credentialBindings":binding("TELEGRAM_BOT_TOKEN")},
            "status":{"phase":"Running"}}));
        s.objects.insert(RUNTIME.into(),json!({"apiVersion":"v1","kind":"Namespace","metadata":{"name":"kars-run","uid":"runtime-uid",
            "resourceVersion":"1","annotations":{"kars.azure.com/namespace-claim-version":"v1","kars.azure.com/sandbox-namespace":"work",
                "kars.azure.com/sandbox-name":"run","kars.azure.com/sandbox-uid":"sandbox-uid"}}}));
        s.objects.insert(DEPLOYMENT.into(),json!({"apiVersion":"apps/v1","kind":"Deployment",
            "metadata":{"name":"run","namespace":"kars-run","uid":"deployment-uid","resourceVersion":"1",
                "labels":{"kars.azure.com/sandbox":"run","kars.azure.com/component":"sandbox"},
                "annotations":{"kars.azure.com/credential-sandbox-uid":"sandbox-uid","kars.azure.com/credential-namespace-uid":"runtime-uid"}},
            "spec":{"replicas":1,"selector":{"matchLabels":{"app":"agent"}},"template":{"spec":{"containers":[{"name":"agent","image":"test:latest"}]}}}}));
        s.objects.insert("/api/v1/namespaces/kars-run/configmaps/customer-state".into(),json!({
            "metadata":{"name":"customer-state","namespace":"kars-run","uid":"data","resourceVersion":"1"},"data":{"retained":"important"}}));
        s.pods = vec![
            json!({"metadata":{"name":"old","namespace":"kars-run","uid":"old-pod",
            "deletionTimestamp":"2026-01-01T00:00:00Z"},"status":{"phase":"Running"}}),
        ];
    }
    let server = MockServer::start().await;
    let captured = state.clone();
    Mock::given(|_:&wiremock::Request|true).respond_with(move |r:&wiremock::Request| {
        let mut s=captured.lock().unwrap();let path=r.url.path();let body:Value=r.body_json().unwrap_or(Value::Null);
        s.calls.push((r.method.to_string(),path.into(),body.clone()));
        if r.method=="GET" {
            if path=="/api/v1/namespaces/kars-run/pods" {return ResponseTemplate::new(200).set_body_json(json!({
                "apiVersion":"v1","kind":"PodList","metadata":{},"items":s.pods}));}
            if let Some(value)=s.objects.get(path){return ResponseTemplate::new(200).set_body_json(value);}
            for (resource,kind) in [("karstasks","KarsTask"),("karsapprovals","KarsApproval")] {
                if path.ends_with(&format!("/{resource}")) {
                    let items=s.objects.iter().filter(|(key,_)|key.starts_with(&format!("{path}/"))).map(|(_,v)|v.clone()).collect::<Vec<_>>();
                    return ResponseTemplate::new(200).set_body_json(json!({"apiVersion":"kars.azure.com/v1alpha1",
                        "kind":format!("{kind}List"),"metadata":{},"items":items}));
                }
            }
        }
        if r.method=="DELETE" {
            let existed=s.objects.remove(path).is_some();
            return ResponseTemplate::new(if existed {200}else{404}).set_body_json(json!({
                "apiVersion":"v1","kind":"Status","code":if existed{200}else{404},"reason":"NotFound"}));
        }
        if r.method=="PATCH" || r.method=="PUT" || r.method=="POST" {
            if path.contains("/configmaps") {return ResponseTemplate::new(403).set_body_json(json!({
                "apiVersion":"v1","kind":"Status","status":"Failure","code":403,"reason":"Forbidden"}));}
            let key=if r.method=="POST" {format!("{path}/{}",body["metadata"]["name"].as_str().unwrap())}
                else {path.strip_suffix("/status").unwrap_or(path).into()};
            let mut value=s.objects.get(&key).cloned().unwrap_or_else(||json!({"apiVersion":"kars.azure.com/v1alpha1",
                "kind":if key.contains("inferencepolicies"){"InferencePolicy"}else{"KarsReceipt"},
                "metadata":{"uid":"created","resourceVersion":"0","generation":1}}));
            if let Some(uid)=body["metadata"]["uid"].as_str() {assert_eq!(value["metadata"]["uid"],uid);}
            if let Some(rv)=body["metadata"]["resourceVersion"].as_str()
                && value["metadata"]["resourceVersion"]!=rv {
                return ResponseTemplate::new(409).set_body_json(json!({
                    "apiVersion":"v1","kind":"Status","status":"Failure","code":409,"reason":"Conflict"}));
            }
            let version=value["metadata"]["resourceVersion"].as_str().unwrap().parse::<u64>().unwrap()+1;
            let prior=value["spec"].clone();merge(&mut value,&body);
            if !prior.is_null() && value["spec"]!=prior {value["metadata"]["generation"]=(value["metadata"]["generation"].as_i64().unwrap_or(1)+1).into();}
            value["metadata"]["resourceVersion"]=version.to_string().into();
            s.objects.insert(key,value.clone());
            return ResponseTemplate::new(200).set_body_json(value);
        }
        ResponseTemplate::new(404).set_body_json(json!({"apiVersion":"v1","kind":"Status","status":"Failure","code":404,"reason":"NotFound"}))
    }).mount(&server).await;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
    let ctx = Arc::new(Ctx {
        client,
        signer: crate::providers::signing::ReceiptSigner::from_bytes(&[42; 32]),
    });
    (server, ctx, state, team)
}

fn current(state: &Arc<Mutex<State>>) -> KarsTask {
    serde_json::from_value(state.lock().unwrap().objects[TASK].clone()).unwrap()
}

#[tokio::test]
async fn credential_rebind_full_task_reconcile_preserves_uids_data_and_regenerates_authority_before_resume()
 {
    let (_server, ctx, state, team) = fixture().await;
    let api = Api::<KarsTask>::namespaced(ctx.client.clone(), "work");
    let original = current(&state);
    super::super::reconcile_receipt(
        &ctx.client,
        "work",
        &original,
        original.status.as_ref().unwrap(),
        &ctx.signer,
    )
    .await;
    crate::kars_team_reconciler::credential_bindings::reconcile(&ctx.client, &api, &team)
        .await
        .unwrap();
    assert!(pending(&current(&state)));
    assert!(current(&state).spec.execution.unwrap().launch);
    super::super::reconcile(Arc::new(current(&state)), ctx.clone())
        .await
        .unwrap();
    {
        let s = state.lock().unwrap();
        assert_eq!(s.objects[DEPLOYMENT]["spec"]["replicas"], 0);
        assert_eq!(
            s.objects[TASK]["status"]["executionPhase"],
            "PausingCredentials"
        );
        assert!(s.objects[TASK]["status"]["envelopeDigest"].is_null());
        assert!(!s.objects.contains_key(RECEIPT));
        let ready = s
            .calls
            .iter()
            .position(|(_, path, body)| {
                path == &format!("{TASK}/status") && body["status"]["envelopeDigest"].is_null()
            })
            .unwrap();
        let pause = s
            .calls
            .iter()
            .position(|(method, path, _)| method == "PATCH" && path == DEPLOYMENT)
            .unwrap();
        assert!(ready < pause);
    }
    crate::kars_team_reconciler::credential_bindings::reconcile(&ctx.client, &api, &team)
        .await
        .unwrap();
    assert!(pending(&current(&state)));
    state.lock().unwrap().pods.clear();
    {
        let (sandbox, mut deployment): (
            crate::crd::KarsSandbox,
            k8s_openapi::api::apps::v1::Deployment,
        ) = {
            let s = state.lock().unwrap();
            (
                serde_json::from_value(s.objects[SANDBOX].clone()).unwrap(),
                serde_json::from_value(s.objects[DEPLOYMENT].clone()).unwrap(),
            )
        };
        deployment.spec.as_mut().unwrap().replicas = Some(1);
        apply_deployment(
            &ctx.client,
            &sandbox,
            deployment,
            &json!({"task_authorization":original.envelope_digest(),"task_generation":1}),
        )
        .await
        .unwrap();
        assert_eq!(
            state.lock().unwrap().objects[DEPLOYMENT]["spec"]["replicas"],
            0
        );
    }
    super::super::reconcile(Arc::new(current(&state)), ctx.clone())
        .await
        .unwrap();
    assert_eq!(
        current(&state).status.unwrap().execution_phase.as_deref(),
        Some(PAUSED)
    );
    crate::kars_team_reconciler::credential_bindings::reconcile(&ctx.client, &api, &team)
        .await
        .unwrap();
    assert!(!pending(&current(&state)));
    assert_ne!(
        current(&state).envelope_digest(),
        original.envelope_digest()
    );
    super::super::reconcile(Arc::new(current(&state)), ctx.clone())
        .await
        .unwrap();
    let task = current(&state);
    assert!(super::super::task_is_ready(&task));
    {
        let s = state.lock().unwrap();
        assert_eq!(s.objects[TASK]["metadata"]["uid"], "task-uid");
        assert_eq!(s.objects[SANDBOX]["metadata"]["uid"], "sandbox-uid");
        assert_eq!(s.objects[RUNTIME]["metadata"]["uid"], "runtime-uid");
        assert_eq!(
            s.objects["/api/v1/namespaces/kars-run/configmaps/customer-state"]["data"]["retained"],
            "important"
        );
        assert!(
            s.objects[SANDBOX]["metadata"]["annotations"]
                .get(HOLD)
                .is_none()
        );
        assert_eq!(
            s.objects[RECEIPT]["spec"]["envelopeDigest"],
            task.envelope_digest()
        );
        assert_eq!(
            s.objects[SANDBOX]["spec"]["credentialBindings"],
            serde_json::to_value(task.spec.blueprint.unwrap().credential_bindings).unwrap()
        );
        assert!(
            s.calls
                .iter()
                .all(|(method, path, _)| method != "DELETE" || path == RECEIPT)
        );
        assert!(
            s.calls
                .iter()
                .filter(|(_, path, _)| path == TASK)
                .all(|(_, _, body)| body["spec"]["execution"]["launch"] != false)
        );
    }
    let (sandbox, namespace, mut deployment): (
        crate::crd::KarsSandbox,
        k8s_openapi::api::core::v1::Namespace,
        k8s_openapi::api::apps::v1::Deployment,
    ) = {
        let s = state.lock().unwrap();
        (
            serde_json::from_value(s.objects[SANDBOX].clone()).unwrap(),
            serde_json::from_value(s.objects[RUNTIME].clone()).unwrap(),
            serde_json::from_value(s.objects[DEPLOYMENT].clone()).unwrap(),
        )
    };
    deployment.spec.as_mut().unwrap().replicas = Some(1);
    let identity =
        crate::reconciler::governed_services::identity_read_only(&ctx.client, &sandbox, &namespace)
            .await
            .unwrap();
    assert!(
        apply_deployment(
            &ctx.client,
            &sandbox,
            deployment.clone(),
            &json!({"task_authorization":original.envelope_digest(),"task_generation":1})
        )
        .await
        .is_err()
    );
    apply_deployment(&ctx.client, &sandbox, deployment, &identity)
        .await
        .unwrap();
    assert_eq!(
        state.lock().unwrap().objects[DEPLOYMENT]["spec"]["replicas"],
        1
    );
    let now = current(&state);
    api.patch("run",&PatchParams::default(),&Patch::Merge(json!({
        "metadata":{"uid":now.metadata.uid,"resourceVersion":now.metadata.resource_version},"spec":{"execution":{"launch":false}}
    }))).await.unwrap();
    super::super::reconcile(Arc::new(current(&state)), ctx.clone())
        .await
        .unwrap();
    assert!(
        state
            .lock()
            .unwrap()
            .calls
            .iter()
            .any(|(method, path, _)| method == "DELETE" && path == SANDBOX)
    );
}

#[tokio::test]
async fn credential_rebind_never_adopts_foreign_runtime_or_overrides_explicit_unlaunch() {
    let (_server, ctx, state, team) = fixture().await;
    let api = Api::<KarsTask>::namespaced(ctx.client.clone(), "work");
    crate::kars_team_reconciler::credential_bindings::reconcile(&ctx.client, &api, &team)
        .await
        .unwrap();
    state.lock().unwrap().objects.get_mut(SANDBOX).unwrap()["metadata"]["ownerReferences"][0]["uid"] =
        "foreign".into();
    super::super::reconcile(Arc::new(current(&state)), ctx.clone())
        .await
        .unwrap();
    assert_ne!(
        current(&state).status.unwrap().execution_phase.as_deref(),
        Some(PAUSED)
    );
    assert_eq!(
        state.lock().unwrap().objects[DEPLOYMENT]["spec"]["replicas"],
        1
    );
    state.lock().unwrap().objects.get_mut(TASK).unwrap()["spec"]["execution"]["launch"] =
        false.into();
    crate::kars_team_reconciler::credential_bindings::reconcile(&ctx.client, &api, &team)
        .await
        .unwrap();
    assert!(!current(&state).spec.execution.unwrap().launch);
    assert!(!pending(&current(&state)));
}
