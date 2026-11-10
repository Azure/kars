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
    let (key, _) = complete_with_artifacts(&client, &memory, &root, synthesis_calls, exports).await;
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

#[path = "lifecycle_revision_custody_tests.rs"]
mod lifecycle_revision_custody_tests;

#[path = "lifecycle_failure_tests.rs"]
mod lifecycle_failure_tests;

#[path = "lifecycle_activity_tests.rs"]
mod lifecycle_activity_tests;

#[path = "lifecycle_retirement_tests.rs"]
mod lifecycle_retirement_tests;
