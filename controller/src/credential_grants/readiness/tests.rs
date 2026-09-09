// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TASK: &str = "/apis/kars.azure.com/v1alpha1/namespaces/work/karstasks/task";
const GRANT: &str = "/apis/kars.azure.com/v1alpha1/namespaces/work/karscredentialgrants/workspace";
const SOURCE: &str = "/api/v1/namespaces/work/secrets/kars-credential-input-workspace";
const SANDBOX: &str = "/apis/kars.azure.com/v1alpha1/namespaces/work/karssandboxes/task";
const RUNTIME: &str = "/api/v1/namespaces/kars-task";
const DEPLOYMENT: &str = "/apis/apps/v1/namespaces/kars-task/deployments/task";

#[derive(Default)]
struct State {
    objects: BTreeMap<String, Value>,
    calls: Vec<(String, String, Value)>,
}

fn task() -> KarsTask {
    serde_json::from_value(json!({
        "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTask",
        "metadata":{"name":"task","namespace":"work","uid":"task-uid","resourceVersion":"1","generation":1},
        "spec":{"objective":"Credential readiness test","envelope":{"tier":2,"authorityCeiling":2,"delegationDepth":1},
            "execution":{"launch":true},
            "blueprint":{"model":{"provider":"azure-openai","deployment":"test"},"credentialBindings":{
                "grant":{"name":"workspace","uid":"grant"},"sources":[{
                    "scope":"workspace","source":{"name":"kars-credential-input-workspace","uid":"source"},"keys":[]
                }]}}
        }
    })).unwrap()
}

async fn fixture() -> (MockServer, Client, Arc<Mutex<State>>, KarsTask) {
    let server = MockServer::start().await;
    let task = task();
    let state = Arc::new(Mutex::new(State::default()));
    {
        let mut data = state.lock().unwrap();
        data.objects
            .insert(TASK.into(), serde_json::to_value(&task).unwrap());
        data.objects.insert(GRANT.into(), json!({
            "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsCredentialGrant",
            "metadata":{"name":"workspace","namespace":"work","uid":"grant","resourceVersion":"1","generation":1},
            "spec":{"workspaceUid":"work","writers":[{"namespace":"bridge","name":"bff","uid":"writer"}]},
            "status":{"phase":"Ready","observedGeneration":1,"reason":"Fixture"}
        }));
        data.objects.insert("/api/v1/namespaces/work".into(), json!({
            "apiVersion":"v1","kind":"Namespace","metadata":{"name":"work","uid":"work","resourceVersion":"1"}
        }));
        data.objects.insert("/api/v1/namespaces/bridge/serviceaccounts/bff".into(), json!({
            "apiVersion":"v1","kind":"ServiceAccount","metadata":{"name":"bff","namespace":"bridge","uid":"writer","resourceVersion":"1"}
        }));
        data.objects.insert(SOURCE.into(), json!({
            "apiVersion":"v1","kind":"Secret","type":"Opaque",
            "metadata":{"name":"kars-credential-input-workspace","namespace":"work","uid":"source","resourceVersion":"1",
                "annotations":{"kars.azure.com/credential-purpose":"agent-input-v2","kars.azure.com/credential-workspace":"work",
                    "kars.azure.com/credential-target-kind":"Workspace","kars.azure.com/credential-target":"work",
                    "kars.azure.com/credential-grant-uid":"grant","kars.azure.com/credential-binding-intent":"explicit-reference-v2"}}
        }));
    }
    let recorded = state.clone();
    Mock::given(|_: &wiremock::Request| true).respond_with(move |request: &wiremock::Request| {
        let mut data = recorded.lock().unwrap();
        let path = request.url.path();
        let body: Value = request.body_json().unwrap_or(Value::Null);
        data.calls.push((request.method.to_string(), path.into(), body.clone()));
        if request.method == "GET" {
            if let Some(value) = data.objects.get(path) {
                return ResponseTemplate::new(200).set_body_json(value);
            }
            if path.starts_with("/apis/kars.azure.com/v1alpha1/namespaces/work/") {
                for (resource,kind) in [("karstasks","KarsTaskList"),("karsteams","KarsTeamList"),("karssandboxes","KarsSandboxList")] {
                    if path.ends_with(&format!("/{resource}")) {
                        return ResponseTemplate::new(200).set_body_json(json!({
                            "apiVersion":"kars.azure.com/v1alpha1","kind":kind,"metadata":{},"items":[]
                        }));
                    }
                }
            }
        }
        if request.method == "PATCH" && path == DEPLOYMENT {
            let object = data.objects.get_mut(path).unwrap();
            assert_eq!(body["metadata"]["uid"], object["metadata"]["uid"]);
            assert_eq!(body["metadata"]["resourceVersion"], object["metadata"]["resourceVersion"]);
            object["spec"]["replicas"] = body["spec"]["replicas"].clone();
            object["spec"]["strategy"] = body["spec"]["strategy"].clone();
            return ResponseTemplate::new(200).set_body_json(object.clone());
        }
        ResponseTemplate::new(404).set_body_json(json!({
            "apiVersion":"v1","kind":"Status","reason":"NotFound","status":"Failure","code":404
        }))
    }).mount(&server).await;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
    (server, client, state, task)
}

#[tokio::test]
async fn credential_readiness_retains_delivery_after_writer_uninstall_but_not_source_deletion() {
    let (_server, client, state, mut task) = fixture().await;
    task.spec
        .blueprint
        .as_mut()
        .unwrap()
        .credential_bindings
        .as_mut()
        .unwrap()
        .sources[0]
        .keys = vec!["TELEGRAM_BOT_TOKEN".into()];
    let values =
        json!({"TELEGRAM_BOT_TOKEN":k8s_openapi::ByteString(b"retained-credential".to_vec())});
    {
        let mut data = state.lock().unwrap();
        data.objects
            .remove("/api/v1/namespaces/bridge/serviceaccounts/bff");
        data.objects
            .insert(TASK.into(), serde_json::to_value(&task).unwrap());
        data.objects.get_mut(SOURCE).unwrap()["data"] = values.clone();
        data.objects.insert(DEPLOYMENT.into(), json!({"apiVersion":"apps/v1","kind":"Deployment",
            "metadata":{"name":"task","namespace":"kars-task","uid":"consumer","resourceVersion":"1"},
            "spec":{"replicas":1,"selector":{"matchLabels":{"app":"agent"}},
                "template":{"metadata":{"labels":{"app":"agent"}},
                    "spec":{"containers":[{"name":"agent","image":"test:latest"}]}}}}));
    }
    preflight(&client, &task).await.unwrap();
    let mut status: KarsTaskStatus = serde_json::from_value(json!({
        "phase":"Ready","observedGeneration":1,"envelopeDigest":task.envelope_digest(),
        "sandboxRef":{"name":"task"}
    }))
    .unwrap();
    enforce(&client, &task, &mut status).await;
    assert_eq!(status.phase.as_deref(), Some("Ready"));
    {
        let mut data = state.lock().unwrap();
        assert_eq!(data.objects[SOURCE]["metadata"]["uid"], "source");
        assert_eq!(data.objects[SOURCE]["data"], values);
        assert_eq!(data.objects[DEPLOYMENT]["spec"]["replicas"], 1);
        assert!(data.calls.iter().all(|(method, _, _)| method == "GET"));
        data.objects.remove(SOURCE);
    }
    assert!(preflight(&client, &task).await.is_err());
}

#[tokio::test]
async fn credential_readiness_preflight_bootstraps_an_unready_task_without_writes_or_runtime_creation()
 {
    let (_server, client, state, task) = fixture().await;
    assert!(!crate::kars_task_reconciler::task_is_ready(&task));
    preflight(&client, &task).await.unwrap();
    let data = state.lock().unwrap();
    assert!(data.calls.iter().all(|(method, _, _)| method == "GET"));
    assert!(
        data.objects[SOURCE]["metadata"]
            .get("ownerReferences")
            .is_none()
    );
    assert!(!data.objects.contains_key(RUNTIME));
    assert!(!data.objects.contains_key(SANDBOX));
}

#[tokio::test]
async fn credential_readiness_revocation_clears_the_canonical_ready_proof_without_losing_other_status()
 {
    let (_server, client, state, mut task) = fixture().await;
    state.lock().unwrap().objects.get_mut(GRANT).unwrap()["spec"]["enabled"] = false.into();
    task.status = Some(serde_json::from_value(json!({
        "conditions":[{"type":"Ready","status":"False","reason":"CredentialAuthorityUnavailable",
            "message":"previous failure","lastTransitionTime":"2026-01-01T00:00:00Z"}]
    })).unwrap());
    let mut status: KarsTaskStatus = serde_json::from_value(json!({
        "phase":"Ready","observedGeneration":1,"envelopeDigest":task.envelope_digest(),
        "lineage":["retained-ancestor"],"sandboxRef":{"name":"task"}
    }))
    .unwrap();
    enforce(&client, &task, &mut status).await;
    assert_eq!(status.phase.as_deref(), Some(PHASE_DEGRADED));
    assert!(status.envelope_digest.is_none());
    assert_eq!(status.lineage, vec!["retained-ancestor"]);
    assert_eq!(status.sandbox_ref.as_ref().unwrap().name, "task");
    let ready = conditions::find(status.conditions.as_ref().unwrap(), "Ready").unwrap();
    assert_eq!(ready.reason, "CredentialAuthorityUnavailable");
    assert_eq!(
        serde_json::to_value(&ready.last_transition_time).unwrap(),
        "2026-01-01T00:00:00Z"
    );
    task.status = Some(status);
    assert!(!crate::kars_task_reconciler::task_is_ready(&task));
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
async fn credential_readiness_team_owner_bootstraps_but_never_bypasses_an_unready_parent() {
    let (_server, client, state, mut task) = fixture().await;
    let team = crate::credential_grant::CredentialTarget {
        kind: "KarsTeam".into(),
        namespace: "work".into(),
        name: "team".into(),
        uid: "team".into(),
    };
    let selection = &mut task
        .spec
        .blueprint
        .as_mut()
        .unwrap()
        .credential_bindings
        .as_mut()
        .unwrap()
        .sources[0];
    selection.scope = crate::credential_grant::CredentialScope::Team;
    selection.owner = Some(team.clone());
    selection.source.name = "kars-credential-input-team-team".into();
    task.metadata.owner_references = Some(vec![serde_json::from_value(json!({
        "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTeam","name":"team","uid":"team","controller":true
    })).unwrap()]);
    {
        let mut data = state.lock().unwrap();
        data.objects
            .insert(TASK.into(), serde_json::to_value(&task).unwrap());
        data.objects.insert("/apis/kars.azure.com/v1alpha1/namespaces/work/karsteams/team".into(), json!({
            "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTeam","metadata":{"name":"team","namespace":"work","uid":"team","resourceVersion":"1"}
        }));
        let mut source = data.objects[SOURCE].clone();
        source["metadata"]["name"] = "kars-credential-input-team-team".into();
        source["metadata"]["annotations"]["kars.azure.com/credential-target-kind"] =
            "KarsTeam".into();
        source["metadata"]["annotations"]["kars.azure.com/credential-target"] = "team".into();
        data.objects.insert(
            "/api/v1/namespaces/work/secrets/kars-credential-input-team-team".into(),
            source,
        );
    }
    preflight(&client, &task).await.unwrap();
    task.metadata.owner_references = None;
    task.spec.parent_ref = Some(crate::mcp_server::LocalObjectRef {
        name: "parent".into(),
    });
    {
        let mut data = state.lock().unwrap();
        data.objects
            .insert(TASK.into(), serde_json::to_value(&task).unwrap());
        let mut parent = task.clone();
        parent.metadata.name = Some("parent".into());
        parent.metadata.uid = Some("parent".into());
        parent.spec.parent_ref = None;
        data.objects.insert(
            "/apis/kars.azure.com/v1alpha1/namespaces/work/karstasks/parent".into(),
            serde_json::to_value(parent).unwrap(),
        );
    }
    assert!(preflight(&client, &task).await.is_err());
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
async fn credential_readiness_pause_preserves_namespace_state_and_rejects_foreign_sandbox_ownership()
 {
    let (_server, client, state, task) = fixture().await;
    {
        let mut data = state.lock().unwrap();
        data.objects.insert(SANDBOX.into(), json!({
            "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSandbox",
            "metadata":{"name":"task","namespace":"work","uid":"sandbox","resourceVersion":"1",
                "annotations":{"kars.azure.com/namespace-uid":"runtime"},
                "ownerReferences":[{"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTask","name":"task","uid":"task-uid","controller":true}]},
            "spec":{"runtime":{"kind":"OpenClaw","openclaw":{}},"inferenceRef":{"name":"test"},
                "credentialBindings":task.spec.blueprint.as_ref().unwrap().credential_bindings}
        }));
        data.objects.insert(RUNTIME.into(), json!({"apiVersion":"v1","kind":"Namespace",
            "metadata":{"name":"kars-task","uid":"runtime","resourceVersion":"1","annotations":{
                "kars.azure.com/namespace-claim-version":"v1","kars.azure.com/sandbox-namespace":"work",
                "kars.azure.com/sandbox-name":"task","kars.azure.com/sandbox-uid":"sandbox"}}}));
        data.objects.insert(DEPLOYMENT.into(), json!({"apiVersion":"apps/v1","kind":"Deployment",
            "metadata":{"name":"task","namespace":"kars-task","uid":"deployment","resourceVersion":"1",
                "labels":{"kars.azure.com/sandbox":"task","kars.azure.com/component":"sandbox"},
                "annotations":{"kars.azure.com/credential-sandbox-uid":"sandbox","kars.azure.com/credential-namespace-uid":"runtime"}},
            "spec":{"replicas":1,"selector":{"matchLabels":{"app":"agent"}},"template":{"spec":{"containers":[{"name":"agent","image":"test"}]}}}}));
    }
    assert!(
        crate::kars_task_execution::pause_credentials(&client, &task)
            .await
            .unwrap()
    );
    {
        let mut data = state.lock().unwrap();
        assert_eq!(data.objects[DEPLOYMENT]["spec"]["replicas"], 0);
        assert_eq!(data.objects[RUNTIME]["metadata"]["uid"], "runtime");
        assert_eq!(data.objects[SOURCE]["metadata"]["uid"], "source");
        assert!(
            data.calls
                .iter()
                .all(|(method, path, _)| method == "GET"
                    || (method == "PATCH" && path == DEPLOYMENT))
        );
        data.calls.clear();
        data.objects.get_mut(SANDBOX).unwrap()["metadata"]["ownerReferences"][0]["uid"] =
            "foreign".into();
    }
    assert!(
        crate::kars_task_execution::pause_credentials(&client, &task)
            .await
            .is_err()
    );
    assert!(
        state
            .lock()
            .unwrap()
            .calls
            .iter()
            .all(|(method, _, _)| method == "GET")
    );
}
