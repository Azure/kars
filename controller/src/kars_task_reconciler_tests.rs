// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::kars_task::{KarsTaskSpec, TaskEnvelope};

fn task_with(tier: i32, authority_ceiling: i32, delegation_depth: i32) -> KarsTask {
    let mut task = KarsTask::new(
        "t",
        KarsTaskSpec {
            objective: "do the thing".into(),
            envelope: TaskEnvelope {
                tier,
                authority_ceiling,
                delegation_depth,
                ..TaskEnvelope::default()
            },
            parent_ref: None,
            execution: None,
            blueprint: None,
            display_name: None,
        },
    );
    task.metadata.namespace = Some("default".into());
    task
}

#[test]
fn valid_envelope_passes() {
    let task = task_with(3, 3, 2);
    assert!(matches!(check_envelope(&task), EnvelopeCheck::Valid));
}

#[tokio::test]
async fn retained_task_bursts_are_bounded_and_drain_without_dropping_work() {
    use kube::runtime::{controller::applier, reflector, watcher::Event};
    use tokio::sync::{Semaphore, mpsc};
    use tokio::time::timeout;

    let (store, mut writer) = reflector::store();
    writer.apply_watcher_event(&Event::Init);
    let mut requests = Vec::new();
    for index in 0..58 {
        let mut task = task_with(3, 3, 2);
        task.metadata.name = Some(format!("retained-{index}"));
        writer.apply_watcher_event(&Event::InitApply(task.clone()));
        requests.push(Ok::<_, std::io::Error>(reflector::ObjectRef::from_obj(
            &task,
        )));
    }
    writer.apply_watcher_event(&Event::InitDone);
    let permits = Arc::new(Semaphore::new(0));
    let gate = permits.clone();
    let (started, mut receiver) = mpsc::unbounded_channel();
    let runner = tokio::spawn(async move {
        applier(
            move |task: Arc<KarsTask>, _: Arc<()>| {
                let gate = gate.clone();
                let started = started.clone();
                Box::pin(async move {
                    started.send(task.name_any()).unwrap();
                    gate.acquire().await.unwrap().forget();
                    Ok::<_, std::io::Error>(Action::await_change())
                })
            },
            |_, _, _| Action::await_change(),
            Arc::new(()),
            store,
            futures::stream::iter(requests).chain(futures::stream::pending()),
            reconciliation_config(),
        )
        .take(58)
        .collect::<Vec<_>>()
        .await
    });
    for _ in 0..4 {
        assert!(
            timeout(Duration::from_secs(5), receiver.recv())
                .await
                .unwrap()
                .is_some()
        );
    }
    assert!(
        timeout(Duration::from_millis(50), receiver.recv())
            .await
            .is_err(),
        "a fifth lineage must wait for an available reconciliation slot"
    );
    permits.add_permits(1);
    assert!(
        timeout(Duration::from_secs(5), receiver.recv())
            .await
            .unwrap()
            .is_some()
    );
    permits.add_permits(57);
    let results = timeout(Duration::from_secs(5), runner)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(results.len(), 58);
    assert!(results.iter().all(Result::is_ok));
    let completed = results
        .into_iter()
        .map(|result| result.unwrap().0.name)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(completed.len(), 58);
}

#[test]
fn retired_funded_phases_use_idle_polling_without_changing_live_admission() {
    let mut task = task_with(3, 3, 2);
    task.spec.envelope.budget = Some(
        serde_json::from_value(json!({"tokens": 150000, "scope": "GovernedInference"})).unwrap(),
    );
    task.spec.execution = Some(crate::kars_task::TaskExecution {
        launch: true,
        runtime: None,
    });
    let mut status = ready_status(None, Some(1), task.envelope_digest(), vec![]);
    status.execution_phase = Some("Idle".into());
    assert_eq!(requeue_delay(&task, &status), REQUEUE_IDLE);
    task.spec.execution.as_mut().unwrap().launch = false;
    assert_eq!(requeue_delay(&task, &status), REQUEUE_IDLE);

    task.spec.execution.as_mut().unwrap().launch = true;
    for phase in ["Running", "Launching", "Stopping", PHASE_DEGRADED] {
        status.execution_phase = Some(phase.into());
        assert_eq!(requeue_delay(&task, &status), REQUEUE_PENDING, "{phase}");
    }
    status.execution_phase = Some("Idle".into());
    status.sandbox_ref = Some(crate::mcp_server::LocalObjectRef {
        name: "still-present".into(),
    });
    assert_eq!(requeue_delay(&task, &status), REQUEUE_PENDING);
    status.sandbox_ref = None;
    for phase in [PHASE_PENDING, PHASE_DEGRADED] {
        status.phase = Some(phase.into());
        assert_eq!(requeue_delay(&task, &status), REQUEUE_PENDING, "{phase}");
    }
}

#[test]
fn root_policy_conflict_never_becomes_ready() {
    let mut task = task_with(3, 3, 2);
    task.spec.envelope.tool_policy_ref = Some(crate::mcp_server::LocalObjectRef {
        name: "read".into(),
    });
    task.spec.blueprint = Some(crate::kars_task::TaskBlueprint {
        tool_policy: Some("write".into()),
        ..Default::default()
    });
    assert!(matches!(check_envelope(&task), EnvelopeCheck::Invalid(_)));
}

#[test]
fn readiness_requires_current_generation_digest_and_valid_contract() {
    let mut task = task_with(3, 3, 2);
    task.metadata.generation = Some(1);
    task.status = Some(ready_status(None, Some(1), task.envelope_digest(), vec![]));
    assert!(task_is_ready(&task));
    task.metadata.generation = Some(2);
    assert!(!task_is_ready(&task));
    task.metadata.generation = Some(1);
    task.spec.envelope.tier = 4;
    assert!(!task_is_ready(&task));
}

#[test]
fn execution_plan_readiness_requires_a_retained_supported_contract() {
    for marker in ["execution-plan/v1", "execution-plan/v2"] {
        let mut task = task_with(1, 1, 0);
        task.metadata.generation = Some(1);
        task.status = Some(ready_status(None, Some(1), task.envelope_digest(), vec![]));
        assert!(task_is_ready(&task));
        task.annotations_mut()
            .insert("kars.azure.com/mission-decomposition".into(), marker.into());
        assert_eq!(
            task.status.as_ref().unwrap().envelope_digest,
            Some(task.envelope_digest())
        );
        match check_envelope(&task) {
            EnvelopeCheck::Invalid(why) => assert!(why.contains("ReviewedExecutionPlanMissing")),
            EnvelopeCheck::Valid => panic!("missing plan accepted for {marker}"),
        }
        assert!(!task_is_ready(&task));
    }
}

#[test]
fn execution_plan_drafts_are_ready_but_unsupported_launches_are_not() {
    let mut task = task_with(1, 1, 0);
    task.metadata.generation = Some(1);
    task.spec.blueprint = Some(crate::kars_task::TaskBlueprint {
        execution_plan: Some(crate::task_execution_plan::test_plan()),
        ..Default::default()
    });
    task.spec.execution = Some(crate::kars_task::TaskExecution {
        launch: false,
        runtime: None,
    });
    task.status = Some(ready_status(None, Some(1), task.envelope_digest(), vec![]));
    assert!(task_is_ready(&task));
    task.spec.execution.as_mut().unwrap().launch = true;
    task.status = Some(ready_status(None, Some(1), task.envelope_digest(), vec![]));
    match check_envelope(&task) {
        EnvelopeCheck::Invalid(why) => assert!(why.contains("TypedPlanExecutionUnavailable")),
        EnvelopeCheck::Valid => panic!("unsupported plan execution accepted"),
    }
    assert!(!task_is_ready(&task));
    task.spec.execution.as_mut().unwrap().launch = false;
    task.status = Some(ready_status(None, Some(1), task.envelope_digest(), vec![]));
    assert!(task_is_ready(&task));
    task.spec.blueprint.as_mut().unwrap().execution_plan = None;
    task.spec.execution.as_mut().unwrap().launch = true;
    task.status = Some(ready_status(None, Some(1), task.envelope_digest(), vec![]));
    assert!(
        task_is_ready(&task),
        "legacy unmarked launch remains supported"
    );
}

#[test]
fn completeness_floor_is_not_inferred_from_resource_names() {
    let completeness = gather_completeness();
    assert!(!completeness.floor_enforced);
    assert!(!completeness.task_namespace_floor_vap);
    assert!(!completeness.exec_ban_vap);
    assert!(!completeness.posture_lock_vap);
    assert!(!completeness.default_deny_egress);
}

#[tokio::test]
async fn reviewed_wait_and_outage_preserve_accepted_sandbox_without_materialization_or_teardown() {
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};
    for unavailable in [false, true] {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("reviewed_execution/protocol-fixture.json")).unwrap();
        let mut resources = fixture["resources"].as_object().unwrap().clone();
        let root_path = "/apis/kars.azure.com/v1alpha1/namespaces/default/karstasks/report";
        resources.get_mut(root_path).unwrap()["status"]["sandboxRef"] =
            json!({"name":"retained-runtime"});
        if unavailable {
            resources.remove(&format!(
                "/api/v1/namespaces/default/configmaps/{}",
                fixture["synthesisGrant"]["authority"]["stateName"]
                    .as_str()
                    .unwrap()
            ));
        } else {
            let phase = resources
                .get_mut(&format!(
                    "/apis/kars.azure.com/v1alpha1/namespaces/default/karstasks/{}",
                    fixture["phaseName"].as_str().unwrap()
                ))
                .unwrap();
            phase["metadata"]["annotations"]
                .as_object_mut()
                .unwrap()
                .remove("kars.azure.com/run-completed");
        }
        let task: KarsTask = serde_json::from_value(resources[root_path].clone()).unwrap();
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::path_regex(".*")).respond_with(move |request: &Request| {
            assert_eq!(request.method, "GET", "held work must not be mutated");
            resources.get(request.url.path()).map_or_else(|| ResponseTemplate::new(404).set_body_json(json!({
                "apiVersion":"v1","kind":"Status","status":"Failure","reason":"NotFound","code":404,"message":"fixture unavailable"
            })), |object| ResponseTemplate::new(200).set_body_json(object))
        }).mount(&server).await;
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let client =
            kube::Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
        let mut status = task.status.clone().unwrap();
        status.sandbox_ref = None;
        reconcile_execution(&client, "default", &task, &mut status).await;
        assert_eq!(
            status.execution_phase.as_deref(),
            Some(if unavailable { "Degraded" } else { "Launching" })
        );
        assert_eq!(
            status.sandbox_ref.as_ref().unwrap().name,
            "retained-runtime"
        );
        assert_eq!(status.phase.as_deref(), Some(PHASE_READY));
        assert!(
            !server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .any(|request| request.url.path().contains("karssandboxes"))
        );
    }
}

#[tokio::test]
async fn stopped_reviewed_root_retires_only_owned_phase_compute_and_retries_cleanup() {
    use kube::Resource;
    use std::sync::Mutex;
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};
    for mode in ["stopped", "foreign-root", "denied", "root-race"] {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("reviewed_execution/protocol-fixture.json")).unwrap();
        let mut resources = fixture["resources"].as_object().unwrap().clone();
        let root_path = "/apis/kars.azure.com/v1alpha1/namespaces/default/karstasks/report";
        resources.get_mut(root_path).unwrap()["spec"]["execution"]["launch"] = json!(false);
        if mode == "foreign-root" {
            resources.get_mut(root_path).unwrap()["metadata"]["uid"] = json!("replacement-root");
        }
        let name = fixture["phaseName"].as_str().unwrap();
        let task: KarsTask = serde_json::from_value(
            resources
                [&format!("/apis/kars.azure.com/v1alpha1/namespaces/default/karstasks/{name}")]
                .clone(),
        )
        .unwrap();
        let records = resources.clone();
        for (kind, plural, resource_name) in [
            ("KarsSandbox", "karssandboxes", name.to_string()),
            (
                "InferencePolicy",
                "inferencepolicies",
                format!("{name}-inference"),
            ),
        ] {
            resources.insert(
                format!("/apis/kars.azure.com/v1alpha1/namespaces/default/{plural}/{resource_name}"),
                json!({"apiVersion":"kars.azure.com/v1alpha1","kind":kind,"metadata":{
                    "name":resource_name,"namespace":"default","uid":format!("{kind}-uid"),
                    "resourceVersion":"42","ownerReferences":[task.controller_owner_ref(&()).unwrap()]
                },"spec":{}}),
            );
        }
        let store = Arc::new(Mutex::new(resources));
        let responder_store = store.clone();
        let reads = Arc::new(Mutex::new(0));
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::path_regex(".*"))
            .respond_with(move |request: &Request| {
                let mut values = responder_store.lock().unwrap();
                let path = request.url.path();
                let failure = |code, reason| {
                    ResponseTemplate::new(code).set_body_json(json!({
                        "apiVersion":"v1","kind":"Status","status":"Failure","reason":reason,
                        "code":code,"message":reason
                    }))
                };
                match request.method.as_str() {
                    "GET" => {
                        if mode == "root-race" && path == root_path {
                            let mut reads = reads.lock().unwrap();
                            *reads += 1;
                            if *reads == 2 {
                                values.get_mut(path).unwrap()["metadata"]["resourceVersion"] =
                                    json!("changed");
                            }
                        }
                        values.get(path).map_or_else(
                            || failure(404, "NotFound"),
                            |value| ResponseTemplate::new(200).set_body_json(value),
                        )
                    }
                    "DELETE" => {
                        assert!(
                            path.contains("/karssandboxes/")
                                || path.contains("/inferencepolicies/")
                        );
                        let body: serde_json::Value =
                            serde_json::from_slice(&request.body).unwrap();
                        let value = values.get(path).unwrap();
                        assert_eq!(body["preconditions"]["uid"], value["metadata"]["uid"]);
                        assert_eq!(body["preconditions"]["resourceVersion"], "42");
                        if mode == "denied" {
                            return failure(403, "Forbidden");
                        }
                        values.remove(path);
                        ResponseTemplate::new(200).set_body_json(json!({
                            "apiVersion":"v1","kind":"Status","status":"Success"
                        }))
                    }
                    other => panic!("unexpected mutation: {other} {path}"),
                }
            })
            .mount(&server)
            .await;
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
        let mut status = task.status.clone().unwrap();
        reconcile_execution(&client, "default", &task, &mut status).await;
        assert_eq!(
            status.execution_phase.as_deref(),
            Some(match mode {
                "stopped" => "Idle",
                "denied" => "Stopping",
                _ => "Degraded",
            }),
            "{mode}: {:?}",
            status.execution_detail
        );
        let deletes = server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .filter(|request| request.method == "DELETE")
            .count();
        assert_eq!(
            deletes,
            match mode {
                "stopped" => 2,
                "denied" => 1,
                _ => 0,
            }
        );
        for (path, record) in records {
            if mode == "root-race" && path == root_path {
                continue;
            }
            assert_eq!(store.lock().unwrap().get(&path), Some(&record), "{path}");
        }
        if mode == "stopped" {
            assert!(status.sandbox_ref.is_none());
            reconcile_execution(&client, "default", &task, &mut status).await;
            assert_eq!(status.execution_phase.as_deref(), Some("Idle"));
        }
    }
}

#[tokio::test]
async fn cleanup_errors_preserve_stopping_reference_until_a_successful_retry() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
    let mut task = task_with(3, 3, 1);
    task.metadata.uid = Some("task-uid".into());
    task.status = Some(KarsTaskStatus {
        sandbox_ref: Some(crate::mcp_server::LocalObjectRef { name: "t".into() }),
        execution_phase: Some("Running".into()),
        ..Default::default()
    });
    let sandbox_path = "/apis/kars.azure.com/v1alpha1/namespaces/default/karssandboxes/t";
    Mock::given(method("GET"))
        .and(path(sandbox_path))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "apiVersion": "kars.azure.com/v1alpha1", "kind": "KarsSandbox",
            "metadata": {
                "name": "t", "uid": "sandbox-uid", "resourceVersion": "42",
                "ownerReferences": [{
                    "apiVersion": "kars.azure.com/v1alpha1", "kind": "KarsTask",
                    "name": "t", "uid": "task-uid", "controller": true,
                }],
            }, "spec": {},
        })))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(sandbox_path))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "status": "Failure", "reason": "Forbidden", "message": "denied", "code": 403,
        })))
        .mount(&server)
        .await;
    let mut status = KarsTaskStatus::default();
    reconcile_execution(&client, "default", &task, &mut status).await;
    assert_eq!(status.execution_phase.as_deref(), Some("Stopping"));
    assert_eq!(status.sandbox_ref.as_ref().unwrap().name, "t");
    assert!(
        status
            .execution_detail
            .as_ref()
            .unwrap()
            .contains("retrying")
    );
    server.reset().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "status": "Failure", "reason": "NotFound", "message": "gone", "code": 404,
        })))
        .mount(&server)
        .await;
    reconcile_execution(&client, "default", &task, &mut status).await;
    assert_eq!(status.execution_phase.as_deref(), Some("Idle"));
    assert!(status.sandbox_ref.is_none());
}

#[test]
fn authority_ceiling_above_tier_is_rejected() {
    let task = task_with(2, 4, 1);
    match check_envelope(&task) {
        EnvelopeCheck::Invalid(why) => assert!(why.contains("authorityCeiling")),
        EnvelopeCheck::Valid => panic!("expected rejection"),
    }
}

#[test]
fn tier_out_of_range_is_rejected() {
    let task = task_with(9, 5, 0);
    assert!(matches!(check_envelope(&task), EnvelopeCheck::Invalid(_)));
}

#[test]
fn finalizer_roundtrip() {
    let mut task = task_with(1, 1, 0);
    assert!(!has_finalizer(&task));
    task.metadata.finalizers = Some(vec![FINALIZER.to_string(), "other/keep".to_string()]);
    assert!(has_finalizer(&task));
    let dropped = drop_finalizer(&task);
    assert_eq!(dropped, vec!["other/keep".to_string()]);
}

#[tokio::test]
async fn receipt_collector_signs_d0_history_after_promotion_to_d1() {
    use crate::kars_approval::{KarsApproval, KarsApprovalStatus, request_snapshot};
    use base64::Engine as _;
    use wiremock::matchers::{method, path, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
    let mut task = task_with(1, 1, 0);
    task.metadata.uid = Some("task-uid".into());
    task.metadata.generation = Some(1);
    task.spec.blueprint = Some(crate::kars_task::TaskBlueprint {
        model: Some(crate::kars_task::TaskModel {
            deployment: "reviewed-model".into(),
            provider: "azure-openai".into(),
        }),
        ..Default::default()
    });
    let d0 = task.envelope_digest();
    let mut approval = KarsApproval::new(
        "promotion",
        serde_json::from_value(json!({
            "taskRef": { "name": "t" },
            "action": { "kind": "tierRaise", "summary": "Permit tier 2", "requestedTier": 2 },
            "ttl": "PT1H",
            "decision": { "verdict": "approve", "decider": "alice" },
        }))
        .unwrap(),
    );
    approval.metadata.uid = Some("approval-uid".into());
    approval.metadata.namespace = Some("default".into());
    approval.metadata.generation = Some(2);
    approval.status = Some(KarsApprovalStatus {
        phase: Some("Approved".into()),
        observed_generation: Some(2),
        bound_envelope_digest: Some(d0.clone()),
        bound_task_uid: task.metadata.uid.clone(),
        bound_request: Some(request_snapshot(&approval.spec)),
        requested_at: Some("2026-09-07T08:00:00Z".into()),
        decided_at: Some("2026-09-07T08:15:00Z".into()),
        expires_at: Some("2026-09-07T09:00:00Z".into()),
        decider: Some("alice".into()),
        ..Default::default()
    });
    task.spec.envelope.tier = 2;
    task.metadata.generation = Some(2);
    let status = ready_status(None, Some(2), task.envelope_digest(), Vec::new());
    task.status = Some(status.clone());
    assert!(!crate::kars_approval::approval_authorizes_task(
        &approval, &task
    ));
    Mock::given(method("GET"))
        .and(path(
            "/apis/kars.azure.com/v1alpha1/namespaces/default/karsapprovals",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "apiVersion": "kars.azure.com/v1alpha1", "kind": "KarsApprovalList",
            "metadata": { "resourceVersion": "1" }, "items": [approval],
        })))
        .mount(&server)
        .await;
    let receipt_path = "/apis/kars.azure.com/v1alpha1/namespaces/default/karsreceipts/t";
    Mock::given(method("PATCH"))
        .and(path(receipt_path))
        .respond_with(|request: &wiremock::Request| {
            ResponseTemplate::new(200)
                .set_body_json(request.body_json::<serde_json::Value>().unwrap())
        })
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex("/api/v1/namespaces/[^/]+/configmaps"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "status": "Failure", "code": 403, "reason": "Forbidden", "message": "log unavailable",
        })))
        .mount(&server)
        .await;
    Mock::given(method("PATCH")).and(path(format!("{receipt_path}/status")))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({
            "status": "Failure", "code": 503, "reason": "ServiceUnavailable", "message": "echo unavailable",
        }))).mount(&server).await;
    let signer = crate::providers::signing::ReceiptSigner::from_bytes(&[7; 32]);
    reconcile_receipt(&client, "default", &task, &status, &signer).await;
    let requests = server.received_requests().await.unwrap();
    let receipt: serde_json::Value = requests
        .iter()
        .find(|request| request.method == "PATCH" && request.url.path() == receipt_path)
        .unwrap()
        .body_json()
        .unwrap();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(receipt["spec"]["dsse"]["payload"].as_str().unwrap())
        .unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let history = &payload["predicate"]["approvalHistory"][0];
    assert_eq!(
        receipt["spec"]["dsse"]["signatures"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(history["boundEnvelopeDigest"], d0);
    assert_eq!(history["taskUid"], "task-uid");
    assert_eq!(history["decision"]["verdict"], "approve");
    assert_eq!(history["evidenceScope"], "historicalDecision");
    assert_eq!(history["authorizesCurrentTask"], false);
    assert_eq!(history["consumptionAttested"], false);
    assert_eq!(
        payload["subject"][0]["digest"]["sha256"],
        task.envelope_digest().trim_start_matches("sha256:")
    );
    assert!(payload["predicate"].get("approvals").is_none());
}
