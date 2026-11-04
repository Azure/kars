// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::kars_task_reconciler::halt;

async fn fail(
    client: &Client,
    memory: &Memory,
    task: &KarsTask,
    status: &str,
    measured: bool,
) -> (String, Value) {
    let (key, mut record) = complete(client, memory, task).await;
    record["phase"] = status.into();
    record["reply"]["status"] = status.into();
    record["reply"]["error"] = "tool execution failed".into();
    record["reply"].as_object_mut().unwrap().remove("output");
    record["reply"].as_object_mut().unwrap().remove("artifacts");
    record["events"][0]["status"] = status.into();
    if !measured {
        record["reply"].as_object_mut().unwrap().remove("evidence");
        record["events"][0]
            .as_object_mut()
            .unwrap()
            .remove("evidence");
    }
    memory.put(
        map_path(&format!("kars-mission-attempt-{key}")),
        &evidence_map(
            task,
            &format!("kars-mission-attempt-{key}"),
            BTreeMap::from([("attempt.json".into(), record.to_string())]),
        ),
    );
    let output_path = map_path(&format!("kars-mission-output-{key}"));
    let mut output = memory.0.lock().unwrap()[&output_path].clone();
    output["data"]["status"] = status.into();
    output["data"]["output"] = "".into();
    output["data"]["error"] = "tool execution failed".into();
    output["data"]["artifactCount"] = "0".into();
    output["data"]["evidence.json"] = record["reply"].to_string().into();
    if !measured {
        output["data"]
            .as_object_mut()
            .unwrap()
            .remove("totalTokens");
        output["data"]["model"] = "".into();
        output["data"]["usageKnown"] = "false".into();
    }
    memory.put(output_path, &output);
    let artifact_path = map_path(&format!("kars-mission-artifacts-{key}"));
    let mut artifacts = memory.0.lock().unwrap()[&artifact_path].clone();
    artifacts["data"] = json!({});
    memory.put(artifact_path, &artifacts);
    (key, record)
}

#[tokio::test]
async fn verified_failed_and_rejected_phases_retire_without_success_or_accounting_mutation() {
    for initial in [root(), hermes_root()] {
        for status in ["failed", "rejected"] {
            for measured in [false, true] {
                let (_server, client, memory, phase) = phase_fixture_with(initial.clone()).await;
                let (key, _) = fail(&client, &memory, &phase, status, measured).await;
                let completed = memory.task(&phase.name_any());
                assert!(phase_compute_retirable(&client, &completed).await.unwrap());
                // Kubernetes omits an empty ConfigMap data map on API round trips.
                let artifact_path = map_path(&format!("kars-mission-artifacts-{key}"));
                let mut artifacts = memory.0.lock().unwrap()[&artifact_path].clone();
                artifacts.as_object_mut().unwrap().remove("data");
                memory.put(artifact_path, &artifacts);
                let before = memory.0.lock().unwrap().clone();
                assert!(phase_compute_retirable(&client, &completed).await.unwrap());
                assert!(tick(&client, &memory).await.is_err());
                assert!(grant(&client, &memory.task("report")).await.is_err());
                assert_eq!(*memory.0.lock().unwrap(), before);
                assert!(
                    launched(&memory.task(&phase.name_any())),
                    "retirement does not rewrite the admitted task"
                );
            }
        }
    }
}

#[tokio::test]
async fn failed_native_python_receipt_preserves_custody_without_publishing_success() {
    for initial in [root(), hermes_root()] {
        for authorized in [false, true] {
            let mut initial = native_activity_root(initial.clone(), 5);
            let capabilities = &mut initial
                .spec
                .blueprint
                .as_mut()
                .unwrap()
                .execution_plan
                .as_mut()
                .unwrap()
                .roles[0]
                .phases[0]
                .capabilities;
            *capabilities = vec![
                "filesystem-read".into(),
                "filesystem-write".into(),
                "network".into(),
            ];
            if authorized {
                capabilities.push("python-execute".into());
            }
            ready(&mut initial);
            let (_server, client, memory, phase) = phase_fixture_with(initial).await;
            let (key, mut record) = fail(&client, &memory, &phase, "failed", true).await;
            let receipt =
                json!({"version":1,"runs":[{"reason":"process_limit","ok":false}]}).to_string();
            record["reply"]["artifactFormat"] = "text-v1".into();
            record["reply"]["artifacts"] = json!({"kars-python-executions.json":receipt});
            memory.put(
                map_path(&format!("kars-mission-attempt-{key}")),
                &evidence_map(
                    &phase,
                    &format!("kars-mission-attempt-{key}"),
                    BTreeMap::from([("attempt.json".into(), record.to_string())]),
                ),
            );
            let output_path = map_path(&format!("kars-mission-output-{key}"));
            let mut output = memory.0.lock().unwrap()[&output_path].clone();
            output["data"]["artifactCount"] = "1".into();
            output["data"]["evidence.json"] = record["reply"].to_string().into();
            memory.put(output_path, &output);
            let artifact_path = map_path(&format!("kars-mission-artifacts-{key}"));
            let mut artifacts = memory.0.lock().unwrap()[&artifact_path].clone();
            artifacts["data"] = record["reply"]["artifacts"].clone();
            memory.put(artifact_path.clone(), &artifacts);
            let completed = memory.task(&phase.name_any());
            let before = memory.0.lock().unwrap().clone();
            assert_eq!(
                matches!(phase_compute_retirable(&client, &completed).await, Ok(true)),
                authorized
            );
            assert_eq!(*memory.0.lock().unwrap(), before);
            artifacts["data"]["kars-python-executions.json"] = "changed receipt".into();
            memory.put(artifact_path, &artifacts);
            assert!(!matches!(
                phase_compute_retirable(&client, &completed).await,
                Ok(true)
            ));
        }
    }
}

pub(crate) async fn failed_team_fixture(
    runtime: &str,
    binding: String,
    status: &str,
) -> (
    MockServer,
    Client,
    Arc<Mutex<BTreeMap<String, Value>>>,
    crate::kars_team::KarsTeam,
    KarsTask,
) {
    let mut root = team_root(runtime);
    root.annotations_mut()
        .insert("kars.azure.com/team-work".into(), binding);
    root.annotations_mut()
        .insert("kars.azure.com/team-role".into(), "taskforce".into());
    seal_team_test_run(&mut root);
    let (server, client, memory, phase) = phase_fixture_with(root).await;
    fail(&client, &memory, &phase, status, false).await;
    let mut team = crate::kars_team::KarsTeam::new("clinic", Default::default());
    team.metadata.namespace = Some("default".into());
    team.metadata.uid = Some("clinic-uid".into());
    let root = memory.task("report");
    (server, client, memory.0, team, root)
}

pub(crate) async fn completed_team_fixture(
    runtime: &str,
    binding: String,
) -> (
    MockServer,
    Client,
    Arc<Mutex<BTreeMap<String, Value>>>,
    crate::kars_team::KarsTeam,
    KarsTask,
) {
    completed_team_fixture_with_exports(
        runtime,
        binding,
        BTreeMap::from([(
            "report.md".into(),
            "Retain complete evidence and uncertainty.\n".repeat(1000),
        )]),
    )
    .await
}

async fn completed_team_fixture_with_exports(
    runtime: &str,
    binding: String,
    exports: BTreeMap<String, String>,
) -> (
    MockServer,
    Client,
    Arc<Mutex<BTreeMap<String, Value>>>,
    crate::kars_team::KarsTeam,
    KarsTask,
) {
    let mut root = team_root(runtime);
    let plan = root
        .spec
        .blueprint
        .as_mut()
        .unwrap()
        .execution_plan
        .as_mut()
        .unwrap();
    plan.deliverables = serde_json::from_value(json!(
        exports
            .keys()
            .map(|name| json!({"name":name}))
            .collect::<Vec<_>>()
    ))
    .unwrap();
    let synthesis_calls = (plan.roles.len() + plan.deliverables.len()) as u64;
    plan.synthesis.max_tool_calls = plan.synthesis.max_tool_calls.max(synthesis_calls as i32);
    crate::task_execution_plan::validate_filesystem_execution(plan).unwrap();
    root.annotations_mut()
        .insert("kars.azure.com/team-work".into(), binding);
    root.annotations_mut()
        .insert("kars.azure.com/team-role".into(), "taskforce".into());
    seal_team_test_run(&mut root);
    ready(&mut root);
    assert!(crate::kars_task_reconciler::task_is_ready(&root));
    let (server, client, memory, phase) = phase_fixture_with(root).await;
    complete(&client, &memory, &phase).await;
    assert!(tick(&client, &memory).await.unwrap());
    let root = memory.task("report");
    let (key, _) =
        complete_with_artifacts(&client, &memory, &root, synthesis_calls, exports).await;
    let mut projection: ConfigMap = serde_json::from_value(
        memory.0.lock().unwrap()[&map_path(&format!("kars-mission-output-{key}"))].clone(),
    )
    .unwrap();
    projection.metadata.name = Some("kars-mission-output-report".into());
    projection.metadata.uid = Some("retained-output-uid".into());
    memory.put(map_path(&projection.name_any()), &projection);
    let mut root = memory.task("report");
    root.spec.execution.as_mut().unwrap().launch = false;
    root.metadata.generation = Some(root.metadata.generation.unwrap() + 1);
    memory.put(object_path(&root), &root);
    let mut team = crate::kars_team::KarsTeam::new("clinic", Default::default());
    team.metadata.namespace = Some("default".into());
    team.metadata.uid = Some("clinic-uid".into());
    (server, client, memory.0, team, root)
}

pub(crate) async fn failed_revision_team_fixture(
    runtime: &str,
    binding: String,
    synthesis: bool,
) -> (
    MockServer,
    Client,
    Arc<Mutex<BTreeMap<String, Value>>>,
    crate::kars_team::KarsTeam,
    KarsTask,
    KarsTask,
    BTreeMap<String, String>,
) {
    failed_revision_team_fixture_with_package(runtime, binding, synthesis, false).await
}

async fn failed_revision_team_fixture_with_package(
    runtime: &str,
    binding: String,
    synthesis: bool,
    package: bool,
) -> (
    MockServer,
    Client,
    Arc<Mutex<BTreeMap<String, Value>>>,
    crate::kars_team::KarsTeam,
    KarsTask,
    KarsTask,
    BTreeMap<String, String>,
) {
    let (server, client, objects, team, prior) = if package {
        completed_team_fixture_with_exports(
            runtime,
            binding.clone(),
            revision_package_exports().remove(0),
        )
        .await
    } else {
        completed_team_fixture(runtime, binding.clone()).await
    };
    let memory = Memory(objects.clone());
    let mut root = if package {
        revision_package_root(runtime)
    } else {
        team_root_with_roles(runtime, &["builder", "reviewer", "packager", "qa"])
    };
    root.metadata.name = Some("failed-report".into());
    root.metadata.uid = Some("failed-root-uid".into());
    root.spec.blueprint.as_mut().unwrap().runtime = Some(runtime.into());
    root.annotations_mut()
        .insert(RUN.into(), "failed-report".into());
    root.annotations_mut()
        .insert("kars.azure.com/team-work".into(), binding);
    root.annotations_mut()
        .insert("kars.azure.com/team-role".into(), "taskforce".into());
    if package {
        let archive = retained_team_run(&client, &team, &prior)
            .await
            .unwrap()
            .unwrap();
        let documents = archive
            .documents
            .into_iter()
            .enumerate()
            .map(|(index, mut document)| {
                document.name = format!("revision-{index}");
                document
            })
            .collect::<Vec<_>>();
        seal_team_revision(&mut root, &documents, &[]).unwrap();
    }
    seal_team_test_run(&mut root);
    ready(&mut root);
    assert!(crate::kars_task_reconciler::task_is_ready(&root));
    memory.put(object_path(&root), &root);
    let mut failure_data = None;
    'graph: for _ in 0..100 {
        let root = memory.task("failed-report");
        if reconcile(&client, &root).await.unwrap() {
            let (key, _) = fail(&client, &memory, &root, "failed", false).await;
            let mut output: ConfigMap = serde_json::from_value(
                objects.lock().unwrap()[&map_path(&format!("kars-mission-output-{key}"))].clone(),
            )
            .unwrap();
            output.metadata.name = Some("kars-mission-output-failed-report".into());
            output.metadata.uid = Some("failed-output-uid".into());
            memory.put(map_path(&output.name_any()), &output);
            failure_data = output.data;
            break;
        }
        let children: Vec<KarsTask> = objects
            .lock()
            .unwrap()
            .values()
            .filter_map(|value| serde_json::from_value::<KarsTask>(value.clone()).ok())
            .filter(|task| ann(task, ROOT) == Some("failed-report"))
            .collect();
        for task in children {
            if ann(&task, COMPLETE).is_some() {
                continue;
            }
            let task = memory.ready(&task.name_any());
            if launched(&task) {
                if !synthesis {
                    let (_, record) = fail(&client, &memory, &task, "failed", false).await;
                    failure_data = Some(failure_summary(&task, &record).unwrap());
                    break 'graph;
                }
                let mut exports = BTreeMap::from([(
                    "package.txt".into(),
                    format!("Repaired export from {}", ann(&task, SLOT).unwrap()),
                )]);
                if ann(&task, SLOT) == Some("r3-p0") {
                    exports.insert(
                        "kars-python-executions.json".into(),
                        json!({"version":1,"runs":[{"reason":"process_limit","ok":false}]})
                            .to_string(),
                    );
                }
                if package {
                    let index = ["r0-p0", "r1-p0", "r2-p0", "r3-p0"]
                        .iter()
                        .position(|slot| Some(*slot) == ann(&task, SLOT))
                        .unwrap();
                    exports = revision_package_exports().remove(index + 1);
                }
                complete_with_artifacts(&client, &memory, &task, 1, exports).await;
            }
        }
    }
    let failure_data = failure_data.expect("fixture graph did not reach failure");
    let mut root = memory.task("failed-report");
    root.spec.execution.as_mut().unwrap().launch = false;
    root.metadata.generation = Some(root.metadata.generation.unwrap() + 1);
    memory.put(object_path(&root), &root);
    (server, client, objects, team, prior, root, failure_data)
}

#[tokio::test]
async fn revision_manifest_hydrates_28_retained_exports_for_native_phases_and_rejects_changed_custody()
 {
    for runtime in ["OpenClaw", "Hermes"] {
        let work = crate::kars_team_reconciler::TeamWorkBinding {
            backlog_uid: "backlog-uid".into(),
            item_id: "monitoring-package".into(),
            request_digest: "retained-request".into(),
        };
        let binding = serde_json::to_string(&work).unwrap();
        let (_server, client, objects, team, prior, failed, _) =
            failed_revision_team_fixture_with_package(runtime, binding.clone(), true, true).await;
        let memory = Memory(objects.clone());
        let archive = retained_team_run(&client, &team, &prior)
            .await
            .unwrap()
            .unwrap();
        let mut documents = archive
            .documents
            .into_iter()
            .enumerate()
            .map(|(i, mut d)| {
                d.name = format!("revision-{i}");
                d
            })
            .collect::<Vec<_>>();
        assert_eq!(documents.len(), 8);
        let output: ConfigMap = serde_json::from_value(
            objects.lock().unwrap()[&map_path("kars-mission-output-failed-report")].clone(),
        )
        .unwrap();
        documents.extend(
            retained_failed_team_documents(&client, &team, &failed, Some(&output), "")
                .await
                .unwrap(),
        );
        assert_eq!(documents.len(), 28);
        assert_eq!(
            documents.iter().map(|d| d.content.len()).sum::<usize>(),
            281297
        );
        assert!(
            documents[8..]
                .iter()
                .all(|d| d.name.starts_with("revision-unaccepted-")
                    && d.source.task_name != failed.name_any())
        );
        assert_eq!(
            documents
                .iter()
                .filter(|d| d.source.artifact_name == "server.py")
                .count(),
            2
        );
        assert!(
            documents
                .iter()
                .any(|d| d.source.artifact_name == "kars-python-executions.json"
                    && d.name.starts_with("revision-unaccepted-r3-p0--"))
        );
        let mut fresh = revision_package_root(runtime);
        fresh.metadata.name = Some("fresh-report".into());
        fresh.metadata.uid = Some("fresh-root-uid".into());
        fresh
            .annotations_mut()
            .insert(RUN.into(), "fresh-report".into());
        fresh.annotations_mut().insert(
            "kars.azure.com/team-work".into(),
            serde_json::to_string(&crate::kars_team_reconciler::TeamWorkBinding {
                request_digest: "current-revision-request".into(),
                ..work.clone()
            })
            .unwrap(),
        );
        fresh
            .spec
            .blueprint
            .as_mut()
            .unwrap()
            .execution_plan
            .as_mut()
            .unwrap()
            .roles[0]
            .phases[0]
            .execution_mode = Some("native".into());
        seal_team_revision(
            &mut fresh,
            &documents,
            &revision_roles(&plan(&failed).unwrap()),
        )
        .unwrap();
        seal_team_test_run(&mut fresh);
        memory.put(object_path(&fresh), &fresh);
        let unready = objects.lock().unwrap().clone();
        assert_ne!(
            fresh.status.as_ref().unwrap().envelope_digest.as_deref(),
            Some(fresh.envelope_digest().as_str())
        );
        assert!(!crate::kars_task_reconciler::task_is_ready(&fresh));
        assert!(!reconcile(&client, &fresh).await.unwrap());
        assert!(ann(&memory.task("fresh-report"), PIN).is_none());
        assert_eq!(
            *objects.lock().unwrap(),
            unready,
            "stale blueprint readiness must not reserve or change reviewed state"
        );
        ready(&mut fresh);
        assert!(crate::kars_task_reconciler::task_is_ready(&fresh));
        memory.put(object_path(&fresh), &fresh);
        let baseline = objects.lock().unwrap().clone();
        assert_eq!(
            retained_base_inputs(&client, &fresh).await.unwrap(),
            documents
        );
        assert_eq!(
            *objects.lock().unwrap(),
            baseline,
            "retention reads must not change historical failures"
        );
        let mut diagnostics_only = fresh.clone();
        seal_team_revision(
            &mut diagnostics_only,
            &documents[8..],
            &revision_roles(&plan(&failed).unwrap()),
        )
        .unwrap();
        seal_team_test_run(&mut diagnostics_only);
        memory.put(object_path(&diagnostics_only), &diagnostics_only);
        objects.lock().unwrap().remove(&object_path(&prior));
        assert_eq!(
            retained_base_inputs(&client, &diagnostics_only)
                .await
                .unwrap(),
            documents[8..],
            "the failed principal's canonical inputs do not recursively reopen earlier delivery graphs"
        );
        *objects.lock().unwrap() = baseline.clone();
        let pin: Pin = serde_json::from_str(ann(&failed, PIN).unwrap()).unwrap();
        let source = &documents[8].source;
        let producer = memory.task(&source.task_name);
        let key = digest(serde_json::to_vec(&json!([source.task_uid, source.run_nonce])).unwrap())
            [7..47]
            .to_string();
        for mutation in [
            "root-uid",
            "root-spec",
            "work",
            "work-item",
            "work-empty-backlog",
            "work-empty-item",
            "work-camel-case",
            "team",
            "state",
            "child-uid",
            "child-spec",
            "child-missing",
            "artifact",
            "record",
            "output",
            "assembled-bytes",
            "routing-role",
            "routing-phase",
            "routing-alias",
        ] {
            let mut changed = baseline.clone();
            let mut target = fresh.clone();
            match mutation {
                "root-uid" => {
                    changed.get_mut(&object_path(&failed)).unwrap()["metadata"]["uid"] =
                        json!("replaced-root")
                }
                "root-spec" => {
                    changed.get_mut(&object_path(&failed)).unwrap()["spec"]["objective"] =
                        json!("changed")
                }
                "work" | "work-item" | "work-empty-backlog" | "work-empty-item"
                | "work-camel-case" => {
                    let mut prior = failed.clone();
                    let mut changed_work = work.clone();
                    match mutation {
                        "work" => changed_work.backlog_uid = "other".into(),
                        "work-item" => changed_work.item_id = "other-item".into(),
                        "work-empty-backlog" => changed_work.backlog_uid.clear(),
                        "work-empty-item" => changed_work.item_id.clear(),
                        _ => {}
                    }
                    let binding = if mutation == "work-camel-case" {
                        json!({
                            "backlogUid":work.backlog_uid,
                            "itemId":work.item_id,
                            "requestDigest":work.request_digest
                        })
                        .to_string()
                    } else {
                        serde_json::to_string(&changed_work).unwrap()
                    };
                    prior
                        .annotations_mut()
                        .insert("kars.azure.com/team-work".into(), binding);
                    seal_team_test_run(&mut prior);
                    changed.insert(object_path(&prior), serde_json::to_value(prior).unwrap());
                }
                "team" => {
                    changed.get_mut(&object_path(&failed)).unwrap()["metadata"]["ownerReferences"]
                        [0]["uid"] = json!("another-team")
                }
                "state" => {
                    changed.get_mut(&map_path(&pin.name)).unwrap()["metadata"]["uid"] =
                        json!("replaced-state")
                }
                "child-uid" => {
                    changed.get_mut(&object_path(&producer)).unwrap()["metadata"]["uid"] =
                        json!("replaced-child")
                }
                "child-spec" => {
                    changed.get_mut(&object_path(&producer)).unwrap()["spec"]["objective"] =
                        json!("changed")
                }
                "child-missing" => {
                    changed.remove(&object_path(&producer));
                }
                "artifact" => {
                    changed
                        .get_mut(&map_path(&format!("kars-mission-artifacts-{key}")))
                        .unwrap()["data"][&source.artifact_name] = json!("changed")
                }
                "record" => {
                    changed.remove(&map_path(&format!("kars-mission-attempt-{key}")));
                }
                "output" => {
                    changed
                        .get_mut(&map_path(&format!("kars-mission-output-{key}")))
                        .unwrap()["data"]["output"] = json!("changed")
                }
                "routing-role" | "routing-phase" | "routing-alias" => {
                    let mut manifest: RevisionManifest =
                        serde_json::from_str(ann(&target, TEAM_REVISION).unwrap()).unwrap();
                    match mutation {
                        "routing-role" => {
                            manifest.diagnostic_roles[0].name = "different-role".into()
                        }
                        "routing-phase" => {
                            manifest.diagnostic_roles[0].phases[0] = "different-phase".into()
                        }
                        _ => {
                            manifest.inputs[8].name =
                                "revision-unaccepted-r0-p0--99-other.json".into()
                        }
                    }
                    target.annotations_mut().insert(
                        TEAM_REVISION.into(),
                        serde_json::to_string(&manifest).unwrap(),
                    );
                    seal_team_test_run(&mut target);
                    changed.insert(object_path(&target), serde_json::to_value(&target).unwrap());
                }
                "assembled-bytes" => {
                    let memories = (0..2)
                        .map(|i| {
                            let mut d = documents[0].clone();
                            d.name = format!("memory-{i}.md");
                            d.content = "x".repeat(65536);
                            d.sha256 = digest(&d.content);
                            d
                        })
                        .collect::<Vec<_>>();
                    target.annotations_mut().insert(
                        TEAM_MEMORY.into(),
                        serde_json::to_string(&memories).unwrap(),
                    );
                    seal_team_test_run(&mut target);
                    changed.insert(object_path(&target), serde_json::to_value(&target).unwrap());
                }
                _ => unreachable!(),
            }
            *objects.lock().unwrap() = changed;
            assert!(
                retained_base_inputs(&client, &target).await.is_err(),
                "{runtime}: {mutation}"
            );
        }
        *objects.lock().unwrap() = baseline;
        let mut visited = Vec::new();
        let mut synthesis_complete = false;
        for _ in 0..100 {
            let root = memory.task("fresh-report");
            if reconcile(&client, &root).await.unwrap() {
                let root = memory.task("fresh-report");
                let package = revision_package_exports();
                let exports = package[3]
                    .iter()
                    .chain(package[4].iter())
                    .filter(|(name, _)| package[0].contains_key(*name))
                    .map(|(name, content)| (name.clone(), content.clone()))
                    .collect();
                let (_, record) =
                    complete_with_artifacts(&client, &memory, &root, 9, exports).await;
                let actual: Vec<Input> =
                    serde_json::from_value(record["candidate"]["inputArtifacts"].clone()).unwrap();
                assert_eq!(actual.len(), 28);
                assert_eq!(
                    actual
                        .iter()
                        .filter(|d| d.name.starts_with("revision-unaccepted-"))
                        .count(),
                    0
                );
                assert_eq!(
                    actual
                        .iter()
                        .filter(|d| d.name.starts_with("revision-"))
                        .count(),
                    8
                );
                assert!(serde_json::to_vec(&actual).unwrap().len() < MAX_ASSIGNMENT_INPUT_BYTES);
                let retained = retained_team_run(&client, &team, &memory.task("fresh-report"))
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(retained.documents.len(), 8);
                synthesis_complete = true;
                break;
            }
            let children = objects
                .lock()
                .unwrap()
                .values()
                .filter_map(|v| serde_json::from_value::<KarsTask>(v.clone()).ok())
                .filter(|t| ann(t, ROOT) == Some("fresh-report"))
                .collect::<Vec<_>>();
            for child in children {
                if ann(&child, COMPLETE).is_some() {
                    continue;
                }
                let child = memory.ready(&child.name_any());
                if !launched(&child) {
                    continue;
                }
                let slot = ann(&child, SLOT).unwrap().to_owned();
                let index = ["r0-p0", "r1-p0", "r2-p0", "r3-p0"]
                    .iter()
                    .position(|s| *s == slot)
                    .unwrap();
                if index == 2 {
                    let graph = objects.lock().unwrap().clone();
                    let root = memory.task("fresh-report");
                    let (map, state) = load(&client, &root).await.unwrap();
                    // Superseded history must still pass complete retained custody.
                    objects
                        .lock()
                        .unwrap()
                        .get_mut(&map_path(&format!("kars-mission-artifacts-{key}")))
                        .unwrap()["data"][&source.artifact_name] =
                        json!("changed historical source");
                    assert!(
                        inputs(&client, &root, &map, &state, &[(0, 0), (1, 0)])
                            .await
                            .is_err()
                    );
                    *objects.lock().unwrap() = graph.clone();
                    // Two valid <=128-KiB current exports add genuinely new files.
                    // They cannot supersede any retained filename or bypass 384 KiB.
                    for role in 0..2 {
                        let task = existing(&client, &root, &map, &state, role, 0)
                            .await
                            .unwrap()
                            .unwrap();
                        let mut exports = revision_package_exports().remove(role + 1);
                        exports.insert("additional-evidence.txt".into(), "x".repeat(65536));
                        complete_with_artifacts(&client, &memory, &task, 9, exports).await;
                    }
                    let error = inputs(&client, &root, &map, &state, &[(0, 0), (1, 0)])
                        .await
                        .unwrap_err();
                    assert!(
                        error.to_string().contains("transport capacity"),
                        "{error:#}"
                    );
                    *objects.lock().unwrap() = graph;
                }
                let (_, record) = complete_with_artifacts(
                    &client,
                    &memory,
                    &child,
                    9,
                    revision_package_exports().remove(index + 1),
                )
                .await;
                let actual: Vec<Input> =
                    serde_json::from_value(record["candidate"]["inputArtifacts"].clone()).unwrap();
                assert_eq!(actual.len(), 28, "{runtime}: {slot}");
                assert_eq!(
                    actual
                        .iter()
                        .filter(|d| d.name.starts_with("revision-unaccepted-"))
                        .count(),
                    [20, 20, 14, 13][index]
                );
                assert_eq!(
                    actual
                        .iter()
                        .filter(|d| d.name.starts_with("revision-")
                            && !d.name.starts_with("revision-unaccepted-"))
                        .count(),
                    8
                );
                assert!(serde_json::to_vec(&actual).unwrap().len() < MAX_ASSIGNMENT_INPUT_BYTES);
                for prior in &documents[..8] {
                    assert!(actual.contains(prior));
                }
                if index < 2 {
                    let mut expected = documents.clone();
                    expected.sort_by(|a, b| a.name.cmp(&b.name));
                    assert_eq!(actual, expected);
                }
                assert_eq!(
                    record["candidate"]["reviewedPhase"]["executionMode"],
                    "native"
                );
                visited.push(slot);
            }
        }
        assert!(
            synthesis_complete,
            "complete four-role and synthesis journey must fit"
        );
        assert_eq!(visited, ["r0-p0", "r1-p0", "r2-p0", "r3-p0"]);
        assert_eq!(
            objects.lock().unwrap()[&map_path("kars-mission-output-failed-report")],
            serde_json::to_value(output).unwrap()
        );
    }
}

#[tokio::test]
async fn failed_reviewed_team_run_exposes_terminal_provenance_without_synthesis_or_usage() {
    for runtime in ["OpenClaw", "Hermes"] {
        for status in ["failed", "rejected"] {
            for measured in [false, true] {
                let (_server, client, memory, phase) = phase_fixture_with(team_root(runtime)).await;
                let (_, record) = fail(&client, &memory, &phase, status, measured).await;
                let mut team = crate::kars_team::KarsTeam::new("clinic", Default::default());
                team.metadata.namespace = Some("default".into());
                team.metadata.uid = Some("clinic-uid".into());
                let root = memory.task("report");
                let before = memory.0.lock().unwrap().clone();
                let data = failed_team_run(&client, &team, &root, None)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(data["status"], "failed");
                assert_eq!(data["finishedAt"], record["updatedAt"]);
                assert_eq!(data["sourceTaskUid"], phase.uid().unwrap());
                assert_eq!(data["sourceRunNonce"], ann(&phase, RUN).unwrap());
                assert_eq!(
                    data["sourceAttemptSha256"],
                    digest(serde_json::to_vec(&record).unwrap())
                );
                assert!(!data.contains_key("totalTokens"));
                assert!(!data.contains_key("output"));
                assert!(!data.contains_key("artifactCount"));
                let checkpoint =
                    crate::providers::signing::sha256_hex(&serde_json::to_vec(&data).unwrap());
                assert_eq!(
                    failed_team_run(&client, &team, &root, Some(&checkpoint))
                        .await
                        .unwrap(),
                    Some(data)
                );
                assert!(
                    failed_team_run(&client, &team, &root, Some(&"0".repeat(64)))
                        .await
                        .is_err()
                );
                assert!(tick(&client, &memory).await.is_err());
                assert!(grant(&client, &root).await.is_err());
                assert_eq!(*memory.0.lock().unwrap(), before);
                assert!(ann(&root, COMPLETE).is_none());
            }
        }
    }
}

#[tokio::test]
async fn failed_reviewed_team_run_requires_exact_completed_evidence_and_custody() {
    for fault in [
        "team",
        "root-uid",
        "root-nonce",
        "root-objective",
        "state-uid",
        "phase-uid",
        "completion",
        "attempt-owner",
        "attempt-missing",
        "attempt-identity",
        "output",
        "artifacts",
        "uncertain",
        "running",
        "success",
        "stopped",
    ] {
        let (_server, client, memory, phase) = phase_fixture_with(team_root("OpenClaw")).await;
        let (key, _) = fail(&client, &memory, &phase, "failed", false).await;
        let mut team = crate::kars_team::KarsTeam::new("clinic", Default::default());
        team.metadata.namespace = Some("default".into());
        team.metadata.uid = Some("clinic-uid".into());
        let mut root = memory.task("report");
        let mut phase = memory.task(&phase.name_any());
        match fault {
            "team" => team.metadata.uid = Some("other-team".into()),
            "root-uid" => root.metadata.uid = Some("replacement".into()),
            "root-nonce" => {
                root.annotations_mut()
                    .insert(RUN.into(), "other-run".into());
            }
            "root-objective" => root.spec.objective.push_str(" changed"),
            "state-uid" => {
                let mut pin: Pin = serde_json::from_str(ann(&root, PIN).unwrap()).unwrap();
                pin.uid = "replacement".into();
                root.annotations_mut()
                    .insert(PIN.into(), serde_json::to_string(&pin).unwrap());
            }
            "phase-uid" => phase.metadata.uid = Some("replacement".into()),
            "completion" => {
                phase.annotations_mut().remove(COMPLETE);
            }
            "stopped" => root.spec.execution.as_mut().unwrap().launch = false,
            "success" => {
                complete(&client, &memory, &phase).await;
                phase = memory.task(&phase.name_any());
            }
            _ => {
                let path = map_path(&format!("kars-mission-attempt-{key}"));
                let mut objects = memory.0.lock().unwrap();
                match fault {
                    "attempt-missing" => {
                        objects.remove(&path);
                    }
                    "attempt-owner" => {
                        objects.get_mut(&path).unwrap()["metadata"]["ownerReferences"][0]["uid"] =
                            "other-task".into()
                    }
                    "output" => {
                        objects
                            .get_mut(&map_path(&format!("kars-mission-output-{key}")))
                            .unwrap()["data"]["error"] = "changed".into()
                    }
                    "artifacts" => {
                        objects
                            .get_mut(&map_path(&format!("kars-mission-artifacts-{key}")))
                            .unwrap()["data"] = json!({"fake.md":"not a deliverable"})
                    }
                    _ => {
                        let value = objects.get_mut(&path).unwrap();
                        let mut record: Value =
                            serde_json::from_str(value["data"]["attempt.json"].as_str().unwrap())
                                .unwrap();
                        if fault == "attempt-identity" {
                            record["reply"]["taskUid"] = "other-task".into();
                        } else {
                            record["phase"] = fault.into();
                        }
                        value["data"]["attempt.json"] = record.to_string().into();
                    }
                }
            }
        }
        memory.put(object_path(&root), &root);
        memory.put(object_path(&phase), &phase);
        let before = memory.0.lock().unwrap().clone();
        let result = failed_team_run(&client, &team, &root, None).await;
        assert!(!matches!(result, Ok(Some(_))), "{fault}");
        assert_eq!(*memory.0.lock().unwrap(), before, "{fault}");
    }
}

fn native_activity_root(mut initial: KarsTask, tools: i32) -> KarsTask {
    let plan = initial
        .spec
        .blueprint
        .as_mut()
        .unwrap()
        .execution_plan
        .as_mut()
        .unwrap();
    plan.roles[0].phases[0].execution_mode = Some("native".into());
    plan.roles[0].phases[0].max_tool_calls = tools;
    plan.synthesis.execution_mode = Some("native".into());
    ready(&mut initial);
    initial
}

fn large_router_observations() -> String {
    let cycle: Vec<Value> = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../inference-router/tests/fixtures/router-observation-tool-cycle.json"
    )))
    .unwrap();
    let mut events = Vec::new();
    for round in 1..=128 {
        for template in &cycle {
            if template.get("reported_in_round").is_some() {
                continue;
            }
            let mut event = template.clone();
            event["round"] = json!(round);
            event["seq"] = json!((round - 1) * 7 + template["seq"].as_u64().unwrap());
            if event.get("call_id").is_some() {
                event["call_id"] = json!(format!("call-{round}"));
            }
            events.push(event);
        }
    }
    let rounds: Vec<u64> = (1..=128).collect();
    let report = json!({"version":1,"source":"runtime-forwarded-router-observations",
        "coverage":"native-tool-correlated-only","correlated_tools":128,
        "scope_id":cycle[0]["scope_id"],"rounds":rounds,"state":"observed",
        "trace":{"scope_id":cycle[0]["scope_id"],"rounds":rounds,
            "coverage":"native-tool-correlated-only","durable":false,
            "missing_rounds":[],"dropped_events":0,"truncated":false,"events":events}})
    .to_string();
    assert!(report.len() > 128 * 1024);
    assert!(
        json!({"kars-router-observations.json": report})
            .to_string()
            .len()
            < 384 * 1024
    );
    report
}

#[tokio::test]
async fn failed_router_observations_preserve_canonical_custody_without_permitting_synthesis() {
    for initial in [root(), hermes_root()] {
        for damage in [
            "none",
            "large",
            "limit",
            "bytes",
            "count",
            "ordinary-file",
            "missing",
            "oversized",
            "wire-overflow",
            "malformed",
            "unnegotiated",
            "rejected",
        ] {
            let initial = if matches!(damage, "large" | "limit" | "oversized") {
                native_activity_root(initial.clone(), 128)
            } else {
                initial.clone()
            };
            let (_server, client, memory, phase) = phase_fixture_with(initial).await;
            let (key, mut record) = fail(&client, &memory, &phase, "failed", true).await;
            let mut report = if matches!(damage, "large" | "limit" | "oversized") {
                large_router_observations()
            } else {
                json!({"version": 1, "source": "runtime-forwarded-router-observations",
                    "coverage": "returned-responses-only", "responses": 1, "rounds": [1],
                    "state": "unavailable", "reason": "Router observations unavailable after execution (reset, expiry or read failure)"}).to_string()
            };
            if matches!(damage, "limit" | "oversized") {
                report.push_str(
                    &" ".repeat(384 * 1024 + usize::from(damage == "oversized") - report.len()),
                );
            } else if damage == "wire-overflow" {
                report.push_str(&"\n".repeat(300 * 1024));
                assert!(report.len() < 384 * 1024);
            }
            record["assignment"]["activityFormat"] = "router-observations-v1".into();
            record["reply"]["activityFormat"] = "router-observations-v1".into();
            record["reply"]["routerObservations"] = report.clone().into();
            let output_path = map_path(&format!("kars-mission-output-{key}"));
            let artifact_path = map_path(&format!("kars-mission-artifacts-{key}"));
            let mut output = memory.0.lock().unwrap()[&output_path].clone();
            let mut artifacts = memory.0.lock().unwrap()[&artifact_path].clone();
            output["data"]["artifactCount"] = "1".into();
            artifacts["data"] = json!({"kars-router-observations.json": report});
            match damage {
                "bytes" => artifacts["data"]["kars-router-observations.json"] = "changed".into(),
                "count" => output["data"]["artifactCount"] = "0".into(),
                "ordinary-file" => artifacts["data"]["handoff.md"] = "unverified output".into(),
                "missing" => {
                    record["reply"]
                        .as_object_mut()
                        .unwrap()
                        .remove("routerObservations");
                }
                "malformed" => {
                    record["reply"]["routerObservations"] = "{}".into();
                    artifacts["data"]["kars-router-observations.json"] = "{}".into();
                }
                "unnegotiated" => {
                    record["assignment"]
                        .as_object_mut()
                        .unwrap()
                        .remove("activityFormat");
                    record["reply"]
                        .as_object_mut()
                        .unwrap()
                        .remove("activityFormat");
                }
                "rejected" => {
                    record["phase"] = "rejected".into();
                    record["reply"]["status"] = "rejected".into();
                    record["events"][0]["status"] = "rejected".into();
                    output["data"]["status"] = "rejected".into();
                }
                "none" | "large" | "limit" | "oversized" | "wire-overflow" => {}
                _ => unreachable!(),
            }
            output["data"]["evidence.json"] = record["reply"].to_string().into();
            memory.put(output_path, &output);
            memory.put(artifact_path, &artifacts);
            memory.put(
                map_path(&format!("kars-mission-attempt-{key}")),
                &evidence_map(
                    &phase,
                    &format!("kars-mission-attempt-{key}"),
                    BTreeMap::from([("attempt.json".into(), record.to_string())]),
                ),
            );
            let before = memory.0.lock().unwrap().clone();
            assert_eq!(
                matches!(
                    phase_compute_retirable(&client, &memory.task(&phase.name_any())).await,
                    Ok(true)
                ),
                matches!(damage, "none" | "large" | "limit"),
                "{damage}",
            );
            assert!(tick(&client, &memory).await.is_err());
            assert!(grant(&client, &memory.task("report")).await.is_err());
            assert_eq!(*memory.0.lock().unwrap(), before);
        }
    }
}

#[tokio::test]
async fn retained_success_activity_has_separate_budgets_for_both_runtimes() {
    for initial in [root(), hermes_root()] {
        let (_server, client, memory, task) =
            phase_fixture_with(native_activity_root(initial, 128)).await;
        let admitted = grant(&client, &task).await.unwrap().unwrap();
        let phase: ExecutionPhase =
            serde_json::from_value(admitted.contract["reviewedPhase"].clone()).unwrap();
        let peers = admitted.contract.get("peerContext");
        let (key, mut record) = complete_with_artifacts(
            &client,
            &memory,
            &task,
            128,
            BTreeMap::from([(
                "kars-router-observations.json".into(),
                large_router_observations(),
            )]),
        )
        .await;
        for field in ["assignment", "reply"] {
            record[field]["activityFormat"] = "router-observations-v1".into();
        }
        assert!(evidence::success(&record, &task, &phase, peers).is_ok());
        for case in [
            "activity-limit",
            "activity-overflow",
            "file-limit",
            "file-overflow",
            "aggregate-limit",
            "aggregate-overflow",
            "count-limit",
            "count-overflow",
            "reserved-name",
            "wire-limit",
            "wire-overflow",
        ] {
            let mut candidate = record.clone();
            let artifacts = candidate["reply"]["artifacts"].as_object_mut().unwrap();
            match case {
                "activity-limit" | "activity-overflow" => {
                    let mut report = artifacts["kars-router-observations.json"]
                        .as_str()
                        .unwrap()
                        .to_owned();
                    let wrapped = json!({"kars-router-observations.json": report})
                        .to_string()
                        .len();
                    report.push_str(
                        &" ".repeat(
                            384 * 1024 + usize::from(case == "activity-overflow") - wrapped,
                        ),
                    );
                    artifacts.insert("kars-router-observations.json".into(), report.into());
                }
                "file-limit" | "file-overflow" => {
                    let mut content = "é".repeat(32768);
                    if case == "file-overflow" {
                        content.push('x');
                    }
                    artifacts.insert("notes.md".into(), content.into());
                }
                "aggregate-limit" | "aggregate-overflow" => {
                    artifacts.insert(HANDOFF.into(), "x".repeat(30_000).into());
                    artifacts.insert("notes.md".into(), "x".repeat(65536).into());
                    artifacts.insert("extra.md".into(), "".into());
                    let mut ordinary = artifacts.clone();
                    ordinary.remove("kars-router-observations.json");
                    let padding = 128 * 1024 + usize::from(case == "aggregate-overflow")
                        - serde_json::to_vec(&ordinary).unwrap().len();
                    assert!(padding < 65536);
                    artifacts.insert("extra.md".into(), "x".repeat(padding).into());
                }
                "count-limit" | "count-overflow" => {
                    for index in 0..14 + usize::from(case == "count-overflow") {
                        artifacts.insert(format!("extra-{index}.md"), "retained".into());
                    }
                }
                "reserved-name" => {
                    artifacts.insert("response.md".into(), "not an explicit artifact".into());
                }
                "wire-limit" | "wire-overflow" => {
                    candidate["reply"]["output"] = "".into();
                    let mut base = candidate["reply"].clone();
                    base["artifacts"]
                        .as_object_mut()
                        .unwrap()
                        .remove("kars-router-observations.json");
                    let padding = 192 * 1024 + usize::from(case == "wire-overflow")
                        - serde_json::to_vec(&base).unwrap().len();
                    candidate["reply"]["output"] = "x".repeat(padding).into();
                }
                _ => unreachable!(),
            }
            let result = evidence::success(&candidate, &task, &phase, peers);
            assert_eq!(
                result.is_ok(),
                case.ends_with("-limit"),
                "{case}: {result:?}"
            );
        }
        {
            let mut objects = memory.0.lock().unwrap();
            objects
                .get_mut(&map_path(&format!("kars-mission-attempt-{key}")))
                .unwrap()["data"]["attempt.json"] = record.to_string().into();
            objects
                .get_mut(&map_path(&format!("kars-mission-output-{key}")))
                .unwrap()["data"]["evidence.json"] = record["reply"].to_string().into();
        }
        let completed = memory.task(&task.name_any());
        assert!(phase_compute_retirable(&client, &completed).await.unwrap());
        let artifact_path = map_path(&format!("kars-mission-artifacts-{key}"));
        let artifacts = memory.0.lock().unwrap()[&artifact_path].clone();
        let mut changed = artifacts.clone();
        changed["data"]["kars-router-observations.json"] = "changed".into();
        memory.put(artifact_path.clone(), &changed);
        assert!(phase_compute_retirable(&client, &completed).await.is_err());
        assert!(tick(&client, &memory).await.is_err());
        memory.put(artifact_path, &artifacts);
        assert!(tick(&client, &memory).await.unwrap());
        let synthesis = grant(&client, &memory.task("report"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(synthesis.inputs.as_array().unwrap().len(), 1);
        assert_eq!(
            synthesis.inputs[0]["sha256"],
            digest("Reviewed handoff — 日本語")
        );
    }
}

#[tokio::test]
async fn native_terminal_journals_expand_without_relaxing_legacy_or_evidence_checks() {
    for initial in [root(), hermes_root()] {
        for (native, tools, limit) in [(true, 128, 646), (true, 4, 128), (false, 32, 128)] {
            for success in [true, false] {
                let mut initial = initial.clone();
                if native {
                    initial = native_activity_root(initial, tools);
                } else {
                    initial
                        .spec
                        .blueprint
                        .as_mut()
                        .unwrap()
                        .execution_plan
                        .as_mut()
                        .unwrap()
                        .roles[0]
                        .phases[0]
                        .max_tool_calls = tools;
                    ready(&mut initial);
                }
                let (_server, client, memory, task) = phase_fixture_with(initial).await;
                let admitted = grant(&client, &task).await.unwrap().unwrap();
                let phase: ExecutionPhase =
                    serde_json::from_value(admitted.contract["reviewedPhase"].clone()).unwrap();
                let peers = admitted.contract.get("peerContext");
                let (key, mut record) = if success {
                    complete(&client, &memory, &task).await
                } else {
                    fail(&client, &memory, &task, "failed", true).await
                };
                let terminal = record["events"][0].clone();
                let mut running = terminal.clone();
                running["status"] = "running".into();
                let validate = |record: &Value| {
                    if success {
                        evidence::success(record, &task, &phase, peers)
                    } else {
                        evidence::failure_with_outputs(record, &task, &phase, vec![HANDOFF.into()], peers)
                    }
                };
                for count in [0, 128, 129, 646, 647] {
                    let mut events = vec![running.clone(); count];
                    if let Some(last) = events.last_mut() {
                        *last = terminal.clone();
                    }
                    record["events"] = events.into();
                    assert_eq!(
                        validate(&record).is_ok(),
                        count > 0 && count <= limit,
                        "native={native}, tools={tools}, success={success}, events={count}"
                    );
                }
                let mut events = vec![running.clone(); limit];
                events[limit - 1] = terminal.clone();
                record["events"] = events.into();
                if limit > 128 {
                    for damage in [
                        "ordering",
                        "rounds",
                        "tools",
                        "usage",
                        "unknown-usage",
                        "required",
                        "terminal",
                    ] {
                        let mut changed = record.clone();
                        match damage {
                            "ordering" => changed["events"][128]["status"] = "accepted".into(),
                            "rounds" => changed["events"][128]["evidence"]["rounds"] = 0.into(),
                            "tools" => {
                                changed["events"][128]["evidence"]["phase"]["attemptedToolCalls"] =
                                    129.into()
                            }
                            "usage" => {
                                changed["events"][128]["evidence"]["usage"] =
                                    json!({"promptTokens":1,"completionTokens":1,"totalTokens":2})
                            }
                            "unknown-usage" => {
                                changed["events"][128]["evidence"]["usage"] = Value::Null
                            }
                            "required" => {
                                changed["events"][128]["evidence"]["phase"]["completedRequiredToolCalls"] =
                                    1.into()
                            }
                            "terminal" => {
                                changed["events"][limit - 1]["evidence"]["rounds"] = 2.into()
                            }
                            _ => unreachable!(),
                        }
                        assert!(validate(&changed).is_err(), "{damage}");
                    }
                }
                memory.put(
                    map_path(&format!("kars-mission-attempt-{key}")),
                    &evidence_map(
                        &task,
                        &format!("kars-mission-attempt-{key}"),
                        BTreeMap::from([("attempt.json".into(), record.to_string())]),
                    ),
                );
                let before = memory.0.lock().unwrap().clone();
                assert!(
                    phase_compute_retirable(&client, &memory.task(&task.name_any()))
                        .await
                        .unwrap()
                );
                if success {
                    assert!(tick(&client, &memory).await.unwrap());
                    assert!(
                        grant(&client, &memory.task("report"))
                            .await
                            .unwrap()
                            .is_some()
                    );
                } else {
                    assert!(tick(&client, &memory).await.is_err());
                    assert!(grant(&client, &memory.task("report")).await.is_err());
                    assert_eq!(*memory.0.lock().unwrap(), before);
                }
            }
        }
    }
}

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
