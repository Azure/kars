// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

#[tokio::test]
async fn failed_retirement_requires_complete_current_canonical_custody() {
    for damage in [
        "completion",
        "nonce",
        "reply",
        "journal",
        "owner",
        "immutable",
        "projection",
        "artifacts",
        "unexpected-artifacts",
        "uncertain",
        "running",
    ] {
        let (_server, client, memory, phase) = phase_fixture().await;
        let (key, mut record) = fail(&client, &memory, &phase, "failed", true).await;
        let mut completed = memory.task(&phase.name_any());
        let output_path = map_path(&format!("kars-mission-output-{key}"));
        let mut output = memory.0.lock().unwrap()[&output_path].clone();
        match damage {
            "completion" => {
                completed.annotations_mut().remove(COMPLETE);
            }
            "nonce" => record["candidate"]["runNonce"] = "stale".into(),
            "reply" => record["reply"]["status"] = "running".into(),
            "journal" => record["events"][0]["status"] = "running".into(),
            "owner" => output["metadata"]["ownerReferences"][0]["uid"] = "foreign".into(),
            "immutable" => output["immutable"] = false.into(),
            "projection" => output["data"]["totalTokens"] = "999".into(),
            "artifacts" => {
                memory
                    .0
                    .lock()
                    .unwrap()
                    .remove(&map_path(&format!("kars-mission-artifacts-{key}")));
            }
            "unexpected-artifacts" => {
                let artifact_path = map_path(&format!("kars-mission-artifacts-{key}"));
                let mut artifacts = memory.0.lock().unwrap()[&artifact_path].clone();
                artifacts["data"] = json!({"handoff.md": "unverified output"});
                memory.put(artifact_path, &artifacts);
            }
            "uncertain" | "running" => record["phase"] = damage.into(),
            _ => unreachable!(),
        }
        memory.put(object_path(&completed), &completed);
        memory.put(output_path, &output);
        memory.put(
            map_path(&format!("kars-mission-attempt-{key}")),
            &evidence_map(
                &phase,
                &format!("kars-mission-attempt-{key}"),
                BTreeMap::from([("attempt.json".into(), record.to_string())]),
            ),
        );
        assert!(
            !matches!(phase_compute_retirable(&client, &completed).await, Ok(true)),
            "{damage}"
        );
    }
}

#[tokio::test]
async fn budget_pending_retires_verified_failure_but_retains_ambiguous_or_inflight_compute() {
    for mode in ["failed", "inflight", "uncertain"] {
        let (_server, client, memory, phase) = phase_fixture().await;
        let (key, mut record) = fail(&client, &memory, &phase, "failed", false).await;
        let mut task = memory.task(&phase.name_any());
        if mode == "inflight" {
            task.annotations_mut().remove(COMPLETE);
        } else if mode == "uncertain" {
            record["phase"] = "uncertain".into();
            memory.put(
                map_path(&format!("kars-mission-attempt-{key}")),
                &evidence_map(
                    &task,
                    &format!("kars-mission-attempt-{key}"),
                    BTreeMap::from([("attempt.json".into(), record.to_string())]),
                ),
            );
        }
        let name = task.name_any();
        let status = task.status.as_mut().unwrap();
        status.execution_phase = Some("Running".into());
        status.sandbox_ref = Some(crate::mcp_server::LocalObjectRef { name: name.clone() });
        memory.put(object_path(&task), &task);
        for (kind, plural, resource_name) in [
            ("KarsSandbox", "karssandboxes", name.clone()),
            (
                "InferencePolicy",
                "inferencepolicies",
                format!("{name}-inference"),
            ),
        ] {
            memory.put(format!("/apis/kars.azure.com/v1alpha1/namespaces/default/{plural}/{resource_name}"), &json!({
                "apiVersion":"kars.azure.com/v1alpha1", "kind":kind,
                "metadata":{"name":resource_name,"namespace":"default","uid":format!("{resource_name}-uid"),
                    "resourceVersion":"1","ownerReferences":[task.controller_owner_ref(&()).unwrap()]},"spec":{}
            }));
        }
        let before = memory.0.lock().unwrap().clone();
        let mut status = task.status.clone().unwrap();
        crate::kars_task_reconciler::reconcile_execution(
            &client,
            "default",
            &task,
            &mut status,
            true,
        )
        .await;
        assert_eq!(
            status.execution_phase.as_deref(),
            Some(match mode {
                "failed" => "Idle",
                "inflight" => "Running",
                _ => "Degraded",
            }),
            "{mode}: {:?}",
            status.execution_detail
        );
        for (path, object) in before {
            if mode == "failed"
                && (path.contains("/karssandboxes/") || path.contains("/inferencepolicies/"))
            {
                assert!(!memory.0.lock().unwrap().contains_key(&path));
            } else {
                assert_eq!(memory.0.lock().unwrap().get(&path), Some(&object));
            }
        }
        assert_eq!(status.sandbox_ref.is_none(), mode == "failed");
    }
}

#[tokio::test]
async fn halted_phase_cannot_be_relaunched_by_reviewed_root() {
    let (_server, client, memory, mut phase) = phase_fixture().await;
    phase
        .annotations_mut()
        .insert(halt::HALTED.into(), "operator stop".into());
    phase.spec.execution.as_mut().unwrap().launch = false;
    memory.put(object_path(&phase), &phase);
    assert!(tick(&client, &memory).await.is_err());
    assert!(!launched(&memory.task(&phase.name_any())));
    // Even another writer flipping the gate cannot erase the durable stop.
    phase.spec.execution.as_mut().unwrap().launch = true;
    memory.put(object_path(&phase), &phase);
    assert!(grant(&client, &phase).await.is_err());
}

#[tokio::test]
async fn root_halt_reports_pending_descendant_cleanup_and_retries_without_relaunch() {
    let (server, client, memory, phase) = phase_fixture().await;
    let sandbox_path = format!(
        "/apis/kars.azure.com/v1alpha1/namespaces/default/karssandboxes/{}",
        phase.name_any()
    );
    memory.put(sandbox_path.clone(), &json!({
        "apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsSandbox",
        "metadata":{"name":phase.name_any(),"namespace":"default","uid":"sandbox-uid", "resourceVersion":"1",
            "ownerReferences":[phase.controller_owner_ref(&()).unwrap()]}, "spec":{}
    }));
    Mock::given(wiremock::matchers::method("DELETE"))
        .and(wiremock::matchers::path(sandbox_path.clone()))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "apiVersion":"v1","kind":"Status","status":"Failure","reason":"Forbidden","code":403,"message":"retry cleanup"
        })))
        .with_priority(1).mount(&server).await;
    let mut root = memory.task("report");
    root.annotations_mut()
        .insert(halt::HALTED.into(), "operator stop".into());
    root.spec.execution.as_mut().unwrap().launch = false;
    memory.put(object_path(&root), &root);
    assert!(!halt::reconcile(&client, &root).await.unwrap());
    let stopped = memory.task("report");
    assert_eq!(
        stopped.status.as_ref().unwrap().execution_phase.as_deref(),
        Some("Stopping")
    );
    assert!(stopped.status.as_ref().unwrap().sandbox_ref.is_none());
    assert!(memory.0.lock().unwrap().contains_key(&sandbox_path));
    assert!(!launched(&memory.task(&phase.name_any())));
    server.reset().await;
    Mock::given(wiremock::matchers::path_regex(".*"))
        .respond_with(memory.clone())
        .mount(&server)
        .await;
    assert!(halt::reconcile(&client, &stopped).await.unwrap());
    assert_eq!(
        memory
            .task("report")
            .status
            .unwrap()
            .execution_phase
            .as_deref(),
        Some("Idle")
    );
    assert!(!memory.0.lock().unwrap().contains_key(&sandbox_path));
}

#[tokio::test]
async fn root_halt_stops_owned_tree_preserves_evidence_and_retries_idempotently() {
    for initial in [root(), hermes_root()] {
        let (_server, client, memory, phase) = phase_fixture_with(initial).await;
        fail(&client, &memory, &phase, "failed", true).await;
        let mut root = memory.task("report");
        let (map, state) = load(&client, &root).await.unwrap();
        let role_name = state.children["r0"].name.clone();
        // A colliding label/name is insufficient: the UID edge must match.
        let mut foreign = phase.clone();
        foreign.metadata.name = Some("foreign".into());
        foreign.metadata.uid = Some("foreign-uid".into());
        foreign.metadata.owner_references.as_mut().unwrap()[0].uid = "old-role-uid".into();
        memory.put(object_path(&foreign), &foreign);
        for member in [&root, &phase, &foreign] {
            for (kind, plural, name) in [
                ("KarsSandbox", "karssandboxes", member.name_any()),
                (
                    "InferencePolicy",
                    "inferencepolicies",
                    format!("{}-inference", member.name_any()),
                ),
            ] {
                memory.put(format!("/apis/kars.azure.com/v1alpha1/namespaces/default/{plural}/{name}"), &json!({
                    "apiVersion":"kars.azure.com/v1alpha1","kind":kind,
                    "metadata":{"name":name,"namespace":"default","uid":format!("{name}-uid"),"resourceVersion":"1",
                        "ownerReferences":[member.controller_owner_ref(&()).unwrap()]},"spec":{}
                }));
            }
        }
        let retained: BTreeMap<_, _> = memory
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|(path, _)| path.contains("/configmaps/"))
            .map(|(path, value)| (path.clone(), value.clone()))
            .collect();
        root.annotations_mut()
            .insert(halt::HALTED.into(), "operator stop".into());
        root.spec.execution.as_mut().unwrap().launch = false;
        memory.put(object_path(&root), &root);
        assert!(halt::reconcile(&client, &root).await.unwrap());
        let phase_name = phase.name_any();
        for name in ["report", role_name.as_str(), phase_name.as_str()] {
            let stopped = memory.task(name);
            assert!(!launched(&stopped));
            assert!(halt::halted(&stopped));
            assert_eq!(
                stopped.status.as_ref().unwrap().execution_phase.as_deref(),
                Some("Idle")
            );
        }
        assert!(launched(&memory.task("foreign")));
        assert!(!halt::halted(&memory.task("foreign")));
        for (path, value) in retained {
            assert_eq!(memory.0.lock().unwrap().get(&path), Some(&value));
        }
        assert_eq!(
            memory.0.lock().unwrap()[&map_path(&map.name_any())],
            serde_json::to_value(map).unwrap()
        );
        let after = memory.0.lock().unwrap().clone();
        assert!(
            halt::reconcile(&client, &memory.task("report"))
                .await
                .unwrap()
        );
        assert_eq!(*memory.0.lock().unwrap(), after);
        assert!(tick(&client, &memory).await.is_err());
        assert!(!memory.0.lock().unwrap().contains_key(&format!(
            "/apis/kars.azure.com/v1alpha1/namespaces/default/karssandboxes/{}",
            phase.name_any()
        )));
        assert!(memory.0.lock().unwrap().contains_key(
            "/apis/kars.azure.com/v1alpha1/namespaces/default/karssandboxes/foreign"
        ));
    }
}
