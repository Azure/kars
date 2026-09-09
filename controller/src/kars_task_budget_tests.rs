// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::{
    inference_budget_contract::{
        AccountReference, BudgetScope, ResourceIdentity, RootIdentity, RootKind, TaskBudgetBinding,
    },
    kars_task::{KarsTaskSpec, TaskBlueprint, TaskBudget, TaskEnvelope, TaskExecution, TaskModel},
    mcp_server::LocalObjectRef,
};
use serde_json::Value;
use std::sync::Mutex;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

struct Objects {
    task: KarsTask,
    parent: Option<KarsTask>,
    sandbox: bool,
    deletes: usize,
}
#[derive(Clone)]
struct ApiServer(Arc<Mutex<Objects>>);
impl Respond for ApiServer {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let mut objects = self.0.lock().unwrap();
        let path = request.url.path();
        if request.method == "GET" && path == "/api/v1/namespaces/workspace" {
            return ResponseTemplate::new(200).set_body_json(json!({
                "apiVersion":"v1","kind":"Namespace","metadata":{"name":"workspace","uid":"namespace-uid","resourceVersion":"1"}
            }));
        }
        if request.method == "GET" && path.ends_with("/karstasks/task") {
            return ResponseTemplate::new(200).set_body_json(&objects.task);
        }
        if request.method == "GET"
            && path.ends_with("/karstasks/parent")
            && let Some(parent) = &objects.parent
        {
            return ResponseTemplate::new(200).set_body_json(parent);
        }
        if request.method == "PATCH" && path.ends_with("/karstasks/task/status") {
            let patch: Value = serde_json::from_slice(&request.body).unwrap();
            assert_eq!(patch["metadata"]["uid"], objects.task.uid().unwrap());
            assert_eq!(
                patch["metadata"]["resourceVersion"],
                objects.task.resource_version().unwrap()
            );
            objects.task.status = Some(serde_json::from_value(patch["status"].clone()).unwrap());
            objects.task.metadata.resource_version = Some("2".into());
            return ResponseTemplate::new(200).set_body_json(&objects.task);
        }
        if path.ends_with("/karssandboxes/task") && objects.sandbox {
            let sandbox = json!({
                "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSandbox",
                "metadata":{"name":"task","namespace":"workspace","uid":"sandbox-uid","resourceVersion":"1",
                    "ownerReferences":[{"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTask",
                        "name":"task","uid":objects.task.uid(),"controller":true}]},
                "spec":{}
            });
            if request.method == "GET" {
                return ResponseTemplate::new(200).set_body_json(sandbox);
            }
            if request.method == "DELETE" {
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                assert_eq!(body["preconditions"]["uid"], "sandbox-uid");
                assert_eq!(body["preconditions"]["resourceVersion"], "1");
                objects.deletes += 1;
                objects.sandbox = false;
                return ResponseTemplate::new(200).set_body_json(sandbox);
            }
        }
        ResponseTemplate::new(404).set_body_json(json!({
            "apiVersion":"v1","kind":"Status","status":"Failure",
            "code":404,"reason":"NotFound","message":"fixture"
        }))
    }
}

fn fixture(name: &str, parent: Option<&str>, governed: bool, launched: bool) -> KarsTask {
    let mut task = KarsTask::new(
        name,
        KarsTaskSpec {
            objective: "fixture".into(),
            envelope: TaskEnvelope {
                tier: if parent.is_some() { 2 } else { 3 },
                authority_ceiling: if parent.is_some() { 2 } else { 3 },
                delegation_depth: if parent.is_some() { 1 } else { 2 },
                budget: Some(TaskBudget {
                    scope: governed.then_some(BudgetScope::GovernedInference),
                    tokens: Some(30),
                    usd_micros: Some(50),
                }),
                ..Default::default()
            },
            parent_ref: parent.map(|name| LocalObjectRef { name: name.into() }),
            execution: Some(TaskExecution {
                launch: launched,
                runtime: None,
            }),
            blueprint: Some(TaskBlueprint {
                model: Some(TaskModel {
                    provider: "azure-openai".into(),
                    deployment: "fixture".into(),
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
    );
    task.metadata.namespace = Some("workspace".into());
    task.metadata.uid = Some(format!("{name}-uid"));
    task.metadata.generation = Some(1);
    task.metadata.resource_version = Some("1".into());
    task.metadata.finalizers = Some(vec![FINALIZER.into()]);
    task.status = Some(ready_status(None, Some(1), task.envelope_digest(), vec![]));
    if governed {
        let root_name = parent.unwrap_or(name);
        let root_uid = format!("{root_name}-uid");
        task.status.as_mut().unwrap().inference_budget = Some(TaskBudgetBinding {
            scope: BudgetScope::GovernedInference,
            account: AccountReference {
                namespace: "accounting".into(),
                name: "inference-budget-root".into(),
                uid: "account-uid".into(),
            },
            root: RootIdentity {
                kind: RootKind::KarsTask,
                resource: ResourceIdentity {
                    namespace: "workspace".into(),
                    name: root_name.into(),
                    uid: root_uid.clone(),
                },
                workspace_uid: "namespace-uid".into(),
                cluster_uid: "cluster-uid".into(),
            },
            task_uid: task.uid().unwrap(),
            parent_task_uid: parent.map(|name| format!("{name}-uid")),
            root_task_uid: root_uid,
            authorization_digest: task.envelope_digest(),
        });
    }
    if launched {
        let status = task.status.as_mut().unwrap();
        status.execution_phase = Some("Running".into());
        status.sandbox_ref = Some(LocalObjectRef { name: name.into() });
    }
    task
}

async fn run(task: KarsTask, parent: Option<KarsTask>, sandbox: bool) -> Arc<Mutex<Objects>> {
    let server = MockServer::start().await;
    let objects = Arc::new(Mutex::new(Objects {
        task: task.clone(),
        parent,
        sandbox,
        deletes: 0,
    }));
    Mock::given(wiremock::matchers::any())
        .respond_with(ApiServer(objects.clone()))
        .mount(&server)
        .await;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
    reconcile(
        Arc::new(task),
        Arc::new(Ctx {
            client,
            signer: crate::providers::signing::ReceiptSigner::from_bytes(&[7; 32]),
        }),
    )
    .await
    .unwrap();
    objects
}

#[tokio::test]
async fn legacy_positive_plans_and_their_parent_readiness_are_unchanged_without_opt_in() {
    let root = fixture("task", None, false, false);
    let objects = run(root, None, false).await;
    assert!(task_is_ready(&objects.lock().unwrap().task));
    let child = fixture("task", Some("parent"), false, false);
    let parent = fixture("parent", None, false, false);
    let objects = run(child, Some(parent), false).await;
    let objects = objects.lock().unwrap();
    assert!(task_is_ready(&objects.task));
    assert!(
        objects
            .task
            .status
            .as_ref()
            .unwrap()
            .inference_budget
            .is_none()
    );
    assert_eq!(objects.deletes, 0);
}

#[tokio::test]
async fn unavailable_budget_preparation_keeps_owned_execution_but_denies_new_authority() {
    let objects = run(fixture("task", None, true, true), None, true).await;
    let objects = objects.lock().unwrap();
    assert!(!task_is_ready(&objects.task));
    assert!(objects.sandbox);
    assert_eq!(objects.deletes, 0);
    assert_eq!(objects.task.uid().as_deref(), Some("task-uid"));
    assert_eq!(
        objects
            .task
            .status
            .as_ref()
            .unwrap()
            .execution_phase
            .as_deref(),
        Some("Running")
    );
    assert!(
        objects
            .task
            .status
            .as_ref()
            .unwrap()
            .inference_budget
            .is_some()
    );
}

#[tokio::test]
async fn budget_pending_parent_preserves_funded_child_but_pause_and_uid_revocation_still_stop() {
    let mut parent = fixture("parent", None, true, false);
    let mut status = parent.status.clone().unwrap();
    status.phase = Some("Degraded".into());
    crate::inference_budget::launch::mark_pending(
        &mut status,
        &parent,
        "fixture budget unavailable",
    );
    parent.status = Some(status);
    let child = fixture("task", Some("parent"), true, true);
    let held = run(child.clone(), Some(parent.clone()), true).await;
    assert!(held.lock().unwrap().sandbox);
    assert_eq!(held.lock().unwrap().deletes, 0);
    let mut paused = child.clone();
    paused.spec.execution.as_mut().unwrap().launch = false;
    let stopped = run(paused, Some(parent.clone()), true).await;
    assert!(!stopped.lock().unwrap().sandbox);
    assert_eq!(stopped.lock().unwrap().deletes, 1);
    parent.metadata.uid = Some("recreated-parent".into());
    let revoked = run(child, Some(parent), true).await;
    assert!(!revoked.lock().unwrap().sandbox);
    assert_eq!(revoked.lock().unwrap().deletes, 1);
}

#[tokio::test]
async fn a_pinned_account_cannot_escape_by_removing_governed_scope() {
    let mut task = fixture("task", None, true, false);
    task.spec.envelope.budget.as_mut().unwrap().scope = None;
    let objects = run(task, None, false).await;
    let objects = objects.lock().unwrap();
    assert!(!task_is_ready(&objects.task));
    assert!(
        objects
            .task
            .status
            .as_ref()
            .unwrap()
            .inference_budget
            .is_some()
    );
}
