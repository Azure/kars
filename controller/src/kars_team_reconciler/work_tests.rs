// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::requests_tests::{cycle, finish, latest, request, requested_team, reserve, stage};
use super::*;
use crate::kars_team_reconciler::work::contract;

fn item(id: &str, review_required: bool) -> contract::TeamTask {
    contract::TeamTask {
        id: id.into(),
        title: format!("Deliver {id}"),
        status: "pending".into(),
        acceptance_criteria: vec!["No personal data".into()],
        review_required,
        ..Default::default()
    }
}

fn owner(team: &KarsTeam) -> contract::Owner {
    contract::Owner {
        name: team.name_any(),
        namespace: team.namespace().unwrap(),
        uid: team.uid().unwrap(),
    }
}

#[test]
fn revision_source_skips_cancelled_or_failed_revisions_but_not_successful_replacements() {
    let rejected = contract::Attempt {
        run: "rejected-run".into(),
        run_uid: "retained-uid".into(),
        assignment_nonce: "nonce".into(),
        outcome: "changes_requested".into(),
        finished_at: "2026-10-20T16:00:00Z".into(),
        output_sha256: Some("a".repeat(64)),
        feedback: Some("Revise, do not repeat research".into()),
    };
    let mut task = item("report", true);
    assert!(work::revision_source(&task).is_none());
    task.history.push(rejected.clone());
    assert_eq!(work::revision_source(&task), Some(&rejected));
    task.history.push(contract::Attempt {
        outcome: "cancelled".into(),
        ..rejected.clone()
    });
    assert_eq!(work::revision_source(&task), Some(&rejected));
    task.history.push(contract::Attempt {
        outcome: "failed".into(),
        ..rejected.clone()
    });
    assert_eq!(work::revision_source(&task), Some(&rejected));
    for outcome in ["completed", "approved"] {
        task.history.push(contract::Attempt {
            outcome: outcome.into(),
            ..rejected.clone()
        });
        assert!(work::revision_source(&task).is_none(), "{outcome}");
        task.history.pop();
    }
}

#[tokio::test]
async fn revision_input_admission_blocks_manual_and_cadence_before_task_creation() {
    for (runtime, outcome) in ["OpenClaw", "Hermes"]
        .into_iter()
        .flat_map(|runtime| ["changes_requested", "failed"].map(|outcome| (runtime, outcome)))
    {
        for trigger in ["manual", "cadence"] {
            for missing in ["capability", "allowance"] {
                let mut team = requested_team();
                if trigger == "cadence" {
                    team.annotations_mut().insert(
                        crate::kars_team_reconciler::requests::REQUEST.into(),
                        format!("cadence-1-{}", "a".repeat(64)),
                    );
                }
                let mut plan = crate::task_execution_plan::test_filesystem_plan();
                plan.roles[0].name = "benchmark-research".into();
                plan.roles[0].phases[0].execution_mode = Some("native".into());
                plan.synthesis.execution_mode = Some("native".into());
                if missing == "capability" {
                    plan.roles[0].phases[0].capabilities =
                        vec!["network".into(), "filesystem-write".into()];
                } else {
                    plan.roles[0].phases[0].max_tool_calls = 0;
                }
                team.spec.blueprint = Some(crate::kars_task::TaskBlueprint {
                    runtime: Some(runtime.into()),
                    execution_plan: Some(plan),
                    tool_policy: Some("customer-denied-tools".into()),
                    ..Default::default()
                });
                let (server, client, store) = setup(&team).await;
                let mut pending = item("report", true);
                let binding = contract::Binding {
                    backlog_uid: "backlog".into(),
                    item_id: pending.id.clone(),
                    request_digest: contract::request_digest(&pending).unwrap(),
                };
                work::validate_revision_plan(
                    &team,
                    &work::Selection::Item(binding.clone(), pending.clone()),
                )
                .unwrap();
                pending.history.push(contract::Attempt {
                    run: "delivered-run".into(),
                    run_uid: "delivered-uid".into(),
                    assignment_nonce: "delivered-nonce".into(),
                    outcome: outcome.into(),
                    finished_at: "2026-10-31T20:00:00Z".into(),
                    output_sha256: Some("a".repeat(64)),
                    feedback: Some("Improve the package".into()),
                });
                pending.feedback = Some("Improve the package".into());
                queue(&client, &team, vec![pending.clone()]).await;
                let before = server.received_requests().await.unwrap().len();
                let team_before = store.lock().unwrap().team.clone();
                let cms_before = store.lock().unwrap().cms.clone();
                let result = crate::kars_team_reconciler::requests::prepare(
                    &client,
                    &Api::namespaced(client.clone(), "tenant-a"),
                    &team,
                    true,
                )
                .await;
                let error = result
                    .err()
                    .expect("unreadable revision must not reserve a run")
                    .to_string();
                assert!(error.contains("ReviewedInputCapabilityRequired"), "{error}");
                assert!(error.contains("benchmark-research"), "{error}");
                let state = store.lock().unwrap();
                assert!(state.tasks.is_empty());
                assert!(state.activation_reservations.is_empty());
                assert_eq!(state.team, team_before);
                assert_eq!(
                    state.cms, cms_before,
                    "feedback, output hash and history are immutable on refusal"
                );
                drop(state);
                assert!(
                    server.received_requests().await.unwrap()[before..]
                        .iter()
                        .all(|request| request.method.as_str() == "GET")
                );
                let phase = &mut team
                    .spec
                    .blueprint
                    .as_mut()
                    .unwrap()
                    .execution_plan
                    .as_mut()
                    .unwrap()
                    .roles[0]
                    .phases[0];
                phase.capabilities.push("filesystem-read".into());
                phase.max_tool_calls = 8;
                let reviewed = json!(team);
                work::validate_revision_plan(&team, &work::Selection::Item(binding, pending))
                    .unwrap();
                assert_eq!(json!(team), reviewed);
            }
        }
    }
}

#[tokio::test]
async fn revision_documents_reuse_verified_rejected_exports_and_reject_changed_custody() {
    for runtime in ["OpenClaw", "Hermes"] {
        for mutation in [
            "none",
            "uid",
            "nonce",
            "team",
            "work",
            "backlog",
            "output",
            "missing-output",
            "artifact",
        ] {
            let mut selected = contract::Binding {
                backlog_uid: "backlog-uid".into(),
                item_id: "report".into(),
                request_digest: "b".repeat(64),
            };
            let (_server, client, memory, mut team, root) =
                crate::reviewed_execution::completed_team_fixture(
                    runtime,
                    serde_json::to_string(&selected).unwrap(),
                )
                .await;
            let output_path = "/api/v1/namespaces/default/configmaps/kars-mission-output-report";
            let data: BTreeMap<String, String> =
                serde_json::from_value(memory.lock().unwrap()[output_path]["data"].clone())
                    .unwrap();
            let mut task = item("report", true);
            task.history.push(contract::Attempt {
                run: root.name_any(),
                run_uid: root.uid().unwrap(),
                assignment_nonce: root.annotations()[ANNOT_RUN_REQUESTED].clone(),
                outcome: "changes_requested".into(),
                finished_at: data["finishedAt"].clone(),
                output_sha256: Some(crate::providers::signing::sha256_hex(
                    &serde_json::to_vec(&data).unwrap(),
                )),
                feedback: Some("Reuse this report; do not repeat research".into()),
            });
            task.history.push(contract::Attempt {
                run: "cancelled-revision".into(),
                run_uid: "cancelled-uid".into(),
                assignment_nonce: "cancelled-nonce".into(),
                outcome: "cancelled".into(),
                finished_at: "2026-10-28T00:54:48Z".into(),
                output_sha256: Some("c".repeat(64)),
                feedback: Some("Execution failed".into()),
            });
            match mutation {
                "uid" => task.history[0].run_uid = "replacement-uid".into(),
                "nonce" => task.history[0].assignment_nonce = "different-run".into(),
                "team" => team.metadata.uid = Some("replacement-team".into()),
                "work" => selected.item_id = "unrelated-work".into(),
                "backlog" => selected.backlog_uid = "replacement-backlog".into(),
                "output" => {
                    memory.lock().unwrap().get_mut(output_path).unwrap()["data"]["output"] =
                        json!("Changed output")
                }
                "missing-output" => {
                    memory.lock().unwrap().remove(output_path);
                }
                "artifact" => {
                    let mut store = memory.lock().unwrap();
                    let (_, artifacts) = store
                        .iter_mut()
                        .find(|(path, value)| {
                            path.contains("kars-mission-artifacts-")
                                && value["data"].get("report.md").is_some()
                        })
                        .unwrap();
                    artifacts["data"]["report.md"] = json!("Changed document");
                }
                _ => {}
            }
            let before = memory.lock().unwrap().clone();
            let result =
                work::revision_documents(&client, &team, &work::Selection::Item(selected, task))
                    .await;
            if mutation == "none" {
                let (documents, roles) = result.unwrap();
                assert!(roles.is_empty());
                assert_eq!(documents.len(), 1, "exclude undeclared response.md");
                assert_eq!(documents[0].name, "revision-0");
                assert_eq!(documents[0].source.artifact_name, "report.md");
                assert_eq!(documents[0].source.task_uid, root.uid().unwrap());
                assert!(
                    documents[0].content.len() > 32768,
                    "do not truncate to approved-memory capacity"
                );
                assert_eq!(
                    documents[0].content,
                    "Retain complete evidence and uncertainty.\n".repeat(1000)
                );
            } else {
                assert!(result.is_err(), "{runtime}: {mutation}");
            }
            assert_eq!(
                *memory.lock().unwrap(),
                before,
                "revision source reads must not mutate retained history"
            );
        }
    }
}

fn retained_attempt(
    root: &KarsTask,
    data: &BTreeMap<String, String>,
    outcome: &str,
) -> contract::Attempt {
    contract::Attempt {
        run: root.name_any(),
        run_uid: root.uid().unwrap(),
        assignment_nonce: root.annotations()[ANNOT_RUN_REQUESTED].clone(),
        outcome: outcome.into(),
        finished_at: data["finishedAt"].clone(),
        output_sha256: Some(crate::providers::signing::sha256_hex(
            &serde_json::to_vec(data).unwrap(),
        )),
        feedback: Some(
            "Use the repaired exports and independent QA; do not repeat old research".into(),
        ),
    }
}

#[tokio::test]
async fn revision_failed_synthesis_recovers_all_specialists_as_unaccepted_without_replacing_delivery()
 {
    for runtime in ["OpenClaw", "Hermes"] {
        for synthesis in [false, true] {
            for include_prior in [false, true] {
                let binding = contract::Binding {
                    backlog_uid: "backlog-uid".into(),
                    item_id: "report".into(),
                    request_digest: "b".repeat(64),
                };
                let (_server, client, memory, team, prior, failed, failure_data) =
                    crate::reviewed_execution::failed_revision_team_fixture(
                        runtime,
                        serde_json::to_string(&binding).unwrap(),
                        synthesis,
                    )
                    .await;
                let mut task = item("report", true);
                if include_prior {
                    let data = serde_json::from_value(memory.lock().unwrap()[
                        "/api/v1/namespaces/default/configmaps/kars-mission-output-report"
                    ]["data"].clone()).unwrap();
                    task.history
                        .push(retained_attempt(&prior, &data, "changes_requested"));
                }
                task.history
                    .push(retained_attempt(&failed, &failure_data, "failed"));
                let before = memory.lock().unwrap().clone();
                let (documents, roles) =
                    work::revision_documents(&client, &team, &work::Selection::Item(binding, task))
                        .await
                        .unwrap();
                assert_eq!(
                    documents.len(),
                    usize::from(include_prior) + if synthesis { 9 } else { 0 }
                );
                if include_prior {
                    assert_eq!(documents[0].name, "revision-0");
                    assert_eq!(documents[0].source.task_uid, prior.uid().unwrap());
                    assert_eq!(
                        documents[0].content,
                        "Retain complete evidence and uncertainty.\n".repeat(1000)
                    );
                }
                let diagnostics = &documents[usize::from(include_prior)..];
                let mut producers = std::collections::BTreeSet::new();
                let mut aliases = std::collections::BTreeSet::new();
                for document in diagnostics {
                    assert!(document.name.starts_with("revision-unaccepted-r"));
                    assert!(aliases.insert(document.name.clone()));
                    assert_ne!(document.source.task_uid, failed.uid().unwrap());
                    assert_ne!(document.source.task_uid, prior.uid().unwrap());
                    assert_eq!(
                        document.sha256,
                        format!(
                            "sha256:{}",
                            crate::providers::signing::sha256_hex(document.content.as_bytes())
                        )
                    );
                    if document.source.artifact_name == "package.txt" {
                        assert!(document.content.starts_with("Repaired export from r"));
                        producers.insert(document.source.task_uid.clone());
                    }
                }
                assert_eq!(producers.len(), if synthesis { 4 } else { 0 });
                if synthesis {
                    let receipt = diagnostics
                        .iter()
                        .find(|d| d.source.artifact_name == "kars-python-executions.json")
                        .unwrap();
                    assert!(receipt.name.starts_with("revision-unaccepted-r3-p0--"));
                    assert_ne!(
                        receipt.name, "kars-python-executions.json",
                        "prior QA receipt must not impersonate current runtime evidence"
                    );
                    assert_eq!(
                        receipt.content,
                        json!({"version":1,"runs":[{"reason":"process_limit","ok":false}]})
                            .to_string()
                    );
                }
                let mut next = failed.clone();
                crate::reviewed_execution::seal_team_revision(&mut next, &documents, &roles)
                    .unwrap();
                assert!(
                    crate::reviewed_execution::retained_team_run(&client, &team, &failed)
                        .await
                        .is_err()
                );
                assert_eq!(
                    *memory.lock().unwrap(),
                    before,
                    "no grants, replay, rewritten failure or root export"
                );
            }
        }
    }
}

#[tokio::test]
async fn revision_failed_synthesis_rejects_changed_or_incomplete_evidence() {
    for mutation in [
        "uid",
        "nonce",
        "team",
        "work",
        "backlog",
        "root-spec",
        "root-output",
        "missing-output",
        "history-digest",
        "state-uid",
        "state-admission",
        "child-uid",
        "child-spec",
        "missing-child",
        "child-completion",
        "artifact",
        "missing-record",
        "record-owner",
        "record-identity",
        "root-inputs",
    ] {
        let mut binding = contract::Binding {
            backlog_uid: "backlog-uid".into(),
            item_id: "report".into(),
            request_digest: "b".repeat(64),
        };
        let (_server, client, memory, mut team, _prior, failed, failure_data) =
            crate::reviewed_execution::failed_revision_team_fixture(
                "OpenClaw",
                serde_json::to_string(&binding).unwrap(),
                true,
            )
            .await;
        let mut task = item("report", true);
        task.history
            .push(retained_attempt(&failed, &failure_data, "failed"));
        let root_path = "/apis/kars.azure.com/v1alpha1/namespaces/default/karstasks/failed-report";
        let output_path = "/api/v1/namespaces/default/configmaps/kars-mission-output-failed-report";
        {
            let mut objects = memory.lock().unwrap();
            let pin: serde_json::Value =
                serde_json::from_str(&failed.annotations()["kars.azure.com/reviewed-state"])
                    .unwrap();
            let state_path = format!(
                "/api/v1/namespaces/default/configmaps/{}",
                pin["name"].as_str().unwrap()
            );
            let state: serde_json::Value =
                serde_json::from_str(objects[&state_path]["data"]["state.json"].as_str().unwrap())
                    .unwrap();
            let child_name = state["children"]["r0-p0"]["name"].as_str().unwrap();
            let child_path =
                format!("/apis/kars.azure.com/v1alpha1/namespaces/default/karstasks/{child_name}");
            let record_path = objects
                .iter()
                .find(|(path, value)| {
                    path.contains("kars-mission-attempt-")
                        && value["metadata"]["ownerReferences"][0]["name"] == child_name
                })
                .unwrap()
                .0
                .clone();
            match mutation {
                "uid" => task.history[0].run_uid = "replacement".into(),
                "nonce" => task.history[0].assignment_nonce = "replacement".into(),
                "team" => team.metadata.uid = Some("replacement".into()),
                "work" => binding.item_id = "other-work".into(),
                "backlog" => binding.backlog_uid = "other-backlog".into(),
                "history-digest" => task.history[0].output_sha256 = Some("0".repeat(64)),
                "root-spec" => {
                    objects.get_mut(root_path).unwrap()["spec"]["objective"] = json!("changed")
                }
                "root-output" => {
                    objects.get_mut(output_path).unwrap()["data"]["error"] = json!("changed")
                }
                "missing-output" => {
                    objects.remove(output_path);
                }
                "state-uid" => {
                    objects.get_mut(&state_path).unwrap()["metadata"]["uid"] = json!("replacement")
                }
                "state-admission" => {
                    let mut changed = state.clone();
                    changed["admission"]["taskGeneration"] = json!(9999);
                    objects.get_mut(&state_path).unwrap()["data"]["state.json"] =
                        json!(changed.to_string());
                }
                "child-uid" => {
                    objects.get_mut(&child_path).unwrap()["metadata"]["uid"] = json!("replacement")
                }
                "child-spec" => {
                    objects.get_mut(&child_path).unwrap()["spec"]["objective"] = json!("changed")
                }
                "missing-child" => {
                    objects.remove(&child_path);
                }
                "child-completion" => {
                    objects.get_mut(&child_path).unwrap()["metadata"]["annotations"]
                        .as_object_mut()
                        .unwrap()
                        .remove("kars.azure.com/run-completed");
                }
                "artifact" => {
                    let (_, value) = objects
                        .iter_mut()
                        .find(|(path, value)| {
                            path.contains("kars-mission-artifacts-")
                                && value["metadata"]["ownerReferences"][0]["name"] == child_name
                        })
                        .unwrap();
                    value["data"]["package.txt"] = json!("stale bytes");
                }
                "missing-record" => {
                    objects.remove(&record_path);
                }
                "record-owner" => {
                    objects.get_mut(&record_path).unwrap()["metadata"]["ownerReferences"][0]["uid"] =
                        json!("wrong-owner")
                }
                "record-identity" | "root-inputs" => {
                    let path = if mutation == "root-inputs" {
                        objects
                            .iter()
                            .find(|(path, value)| {
                                path.contains("kars-mission-attempt-")
                                    && value["metadata"]["ownerReferences"][0]["name"]
                                        == "failed-report"
                            })
                            .unwrap()
                            .0
                            .clone()
                    } else {
                        record_path
                    };
                    let value = objects.get_mut(&path).unwrap();
                    let mut record: serde_json::Value =
                        serde_json::from_str(value["data"]["attempt.json"].as_str().unwrap())
                            .unwrap();
                    if mutation == "root-inputs" {
                        record["candidate"]["inputArtifacts"][0]["content"] = json!("stale bytes");
                    } else {
                        record["reply"]["taskUid"] = json!("replacement");
                    }
                    value["data"]["attempt.json"] = json!(record.to_string());
                }
                _ => unreachable!(),
            }
        }
        let before = memory.lock().unwrap().clone();
        let result =
            work::revision_documents(&client, &team, &work::Selection::Item(binding, task)).await;
        assert!(result.is_err(), "{mutation}");
        assert_eq!(*memory.lock().unwrap(), before, "{mutation}");
    }
}

#[test]
fn revision_diagnostic_selection_and_native_admission_without_prior_delivery() {
    let mut task = item("report", true);
    let failed = contract::Attempt {
        run: "failed".into(),
        run_uid: "uid".into(),
        assignment_nonce: "nonce".into(),
        outcome: "failed".into(),
        finished_at: "2026-11-03T01:03:32Z".into(),
        output_sha256: Some("a".repeat(64)),
        feedback: None,
    };
    task.history.push(failed.clone());
    assert!(work::revision_source(&task).is_none());
    assert_eq!(contract::revision_diagnostic_source(&task), Some(&failed));
    task.history.push(contract::Attempt {
        outcome: "cancelled".into(),
        ..failed.clone()
    });
    assert_eq!(contract::revision_diagnostic_source(&task), Some(&failed));
    for outcome in ["changes_requested", "completed", "approved"] {
        task.history.push(contract::Attempt {
            outcome: outcome.into(),
            ..failed.clone()
        });
        assert!(contract::revision_diagnostic_source(&task).is_none());
        task.history.pop();
    }
    for runtime in ["OpenClaw", "Hermes"] {
        let mut team = requested_team();
        let mut plan = crate::task_execution_plan::test_filesystem_plan();
        plan.roles[0].phases[0].execution_mode = Some("native".into());
        plan.roles[0].phases[0].capabilities = vec!["filesystem-write".into()];
        team.spec.blueprint = Some(crate::kars_task::TaskBlueprint {
            runtime: Some(runtime.into()),
            execution_plan: Some(plan),
            ..Default::default()
        });
        let binding = contract::Binding {
            backlog_uid: "backlog".into(),
            item_id: task.id.clone(),
            request_digest: contract::request_digest(&task).unwrap(),
        };
        let error =
            work::validate_revision_plan(&team, &work::Selection::Item(binding, task.clone()))
                .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("ReviewedInputCapabilityRequired")
        );
    }
}

async fn queue(client: &Client, team: &KarsTeam, items: Vec<contract::TeamTask>) {
    contract::update(
        &Api::namespaced(client.clone(), "tenant-a"),
        &owner(team),
        None,
        true,
        |current| {
            *current = items.clone();
            Ok(())
        },
    )
    .await
    .unwrap();
}

fn backlog(store: &Arc<Mutex<Store>>) -> Vec<contract::TeamTask> {
    contract::parse(
        store.lock().unwrap().cms["kars-team-tasks-eng"]["data"]["tasks.json"]
            .as_str()
            .unwrap(),
    )
    .unwrap()
}

fn current_run(store: &Arc<Mutex<Store>>) -> String {
    store.lock().unwrap().team["status"]["runAdmission"]["taskName"]
        .as_str()
        .unwrap()
        .into()
}

fn complete(store: &Arc<Mutex<Store>>, name: &str, successful: bool) {
    let mut state = store.lock().unwrap();
    let task = state.tasks.get_mut(name).unwrap();
    task["metadata"]["annotations"]["kars.azure.com/run-completed"] = json!(name);
    let mut output = super::harvest::output(task);
    if !successful {
        output["data"]["status"] = json!("error");
    }
    state
        .cms
        .insert(format!("kars-mission-output-{name}"), output);
}

fn decide(store: &Arc<Mutex<Store>>, run: &str, approved: bool) -> String {
    let mut state = store.lock().unwrap();
    let task: crate::kars_task::KarsTask =
        serde_json::from_value(state.tasks[run].clone()).unwrap();
    let (name, approval) = state
        .approvals
        .iter_mut()
        .find(|(_, approval)| approval["spec"]["taskRef"]["name"] == run)
        .unwrap();
    let typed: crate::kars_approval::KarsApproval =
        serde_json::from_value(approval.clone()).unwrap();
    approval["status"] = json!({
        "phase": if approved { "Approved" } else { "Denied" },
        "boundTaskUid": approval["metadata"]["annotations"][contract::TASK_UID],
        "boundEnvelopeDigest": task.envelope_digest(),
        "boundRequest": crate::kars_approval::request_snapshot(&typed.spec),
    });
    approval["spec"]["decision"] = json!({
        "verdict": if approved { "approve" } else { "deny" },
        "decider":"reviewer",
        "reason":"Remove personal data; keep the checklist useful.",
    });
    name.clone()
}

#[tokio::test]
async fn operator_stop_without_root_output_requeues_only_its_claim_and_retains_cancellation() {
    let team = requested_team();
    let (_server, client, store) = setup(&team).await;
    queue(&client, &team, vec![item("checklist", true)]).await;
    cycle(&client, &store).await.unwrap();
    let first = current_run(&store);
    {
        let mut state = store.lock().unwrap();
        state.team["spec"]["paused"] = json!(true);
        let task = state.tasks.get_mut(&first).unwrap();
        task["spec"]["execution"]["launch"] = json!(false);
        task["metadata"]["annotations"]["kars.azure.com/halted"] =
            json!("halted by operator at 2026-10-09T01:14:00Z: Investigate failed phase");
    }
    cycle(&client, &store).await.unwrap();
    cycle(&client, &store).await.unwrap();
    let items = backlog(&store);
    assert_eq!(items[0].status, "pending");
    assert!(items[0].run.is_none());
    assert_eq!(items[0].history.len(), 1);
    let attempt = &items[0].history[0];
    assert_eq!(attempt.run, first);
    assert_eq!(attempt.outcome, "cancelled");
    assert_eq!(attempt.finished_at, "2026-10-09T01:14:00Z");
    assert_eq!(
        attempt.feedback.as_deref(),
        Some("Investigate failed phase")
    );
    assert!(
        attempt.output_sha256.is_none(),
        "a stop must not invent runtime output"
    );
    assert!(store.lock().unwrap().approvals.is_empty());
    assert_eq!(
        crate::team_commons::entry_count(&client, &latest(&store))
            .await
            .unwrap(),
        0
    );
    store.lock().unwrap().team["spec"]["paused"] = json!(false);
    ask_again(&store, 2);
    cycle(&client, &store).await.unwrap();
    assert_ne!(current_run(&store), first);
    assert_eq!(backlog(&store)[0].history, items[0].history);
    assert_eq!(backlog(&store)[0].status, "active");
}

#[tokio::test]
async fn scheduling_pause_finishes_current_work_and_resume_starts_only_fresh_work() {
    let team = requested_team();
    let (_server, client, store) = setup(&team).await;
    let mut later = item("later", true);
    later.depends_on.push("checklist".into());
    queue(&client, &team, vec![item("checklist", true), later]).await;
    cycle(&client, &store).await.unwrap();
    let first = current_run(&store);
    let claimed = serde_json::to_value(backlog(&store)).unwrap();
    store.lock().unwrap().team["spec"]["paused"] = json!(true);
    cycle(&client, &store).await.unwrap();
    cycle(&client, &store).await.unwrap();
    assert_eq!(serde_json::to_value(backlog(&store)).unwrap(), claimed);
    assert_eq!(
        store.lock().unwrap().tasks[&first]["spec"]["execution"]["launch"],
        true
    );
    assert!(store.lock().unwrap().approvals.is_empty());
    complete(&store, &first, true);
    cycle(&client, &store).await.unwrap();
    cycle(&client, &store).await.unwrap();
    assert_eq!(backlog(&store)[0].status, "awaiting_review");
    decide(&store, &first, true);
    cycle(&client, &store).await.unwrap();
    cycle(&client, &store).await.unwrap();
    let completed = backlog(&store)[0].clone();
    assert_eq!(completed.status, "done");
    assert_eq!(completed.history.len(), 1);
    assert_eq!(completed.history[0].run, first);
    assert_eq!(completed.history[0].outcome, "approved");
    assert_eq!(
        crate::team_commons::entry_count(&client, &latest(&store))
            .await
            .unwrap(),
        1
    );
    ask_again(&store, 2);
    cycle(&client, &store).await.unwrap();
    assert_eq!(current_run(&store), first);
    assert_eq!(backlog(&store)[1].status, "pending");
    assert!(backlog(&store)[1].run.is_none());
    store.lock().unwrap().team["spec"]["paused"] = json!(false);
    cycle(&client, &store).await.unwrap();
    let next = current_run(&store);
    assert_ne!(next, first);
    assert_eq!(backlog(&store)[0].history, completed.history);
    assert_eq!(backlog(&store)[1].status, "active");
    assert_eq!(backlog(&store)[1].run.as_deref(), Some(next.as_str()));
    assert_eq!(
        store.lock().unwrap().tasks[&first]["spec"]["execution"]["launch"],
        false
    );
}

#[tokio::test]
async fn unannotated_stop_does_not_invent_cancellation_and_changed_claim_is_rejected() {
    let team = requested_team();
    let (_server, client, store) = setup(&team).await;
    queue(&client, &team, vec![item("checklist", true)]).await;
    cycle(&client, &store).await.unwrap();
    let first = current_run(&store);
    {
        let mut state = store.lock().unwrap();
        state.tasks.get_mut(&first).unwrap()["spec"]["execution"]["launch"] = json!(false);
    }
    cycle(&client, &store).await.unwrap();
    assert_eq!(backlog(&store)[0].status, "active");
    assert!(backlog(&store)[0].history.is_empty());
    {
        let mut state = store.lock().unwrap();
        state.tasks.get_mut(&first).unwrap()["metadata"]["annotations"]["kars.azure.com/halted"] =
            json!("halted by operator at 2026-10-09T01:14:00Z: Investigate");
        let raw = &mut state.cms.get_mut("kars-team-tasks-eng").unwrap()["data"]["tasks.json"];
        let mut items = contract::parse(raw.as_str().unwrap()).unwrap();
        items[0].run_uid = Some("different-run-uid".into());
        *raw = json!(serde_json::to_string(&items).unwrap());
    }
    assert!(cycle(&client, &store).await.is_err());
    assert_eq!(backlog(&store)[0].status, "active");
    assert!(backlog(&store)[0].history.is_empty());
}

fn ask_again(store: &Arc<Mutex<Store>>, sequence: u64) {
    store.lock().unwrap().team["metadata"]["annotations"]["kars.azure.com/run-now"] =
        json!(request(sequence));
}

#[tokio::test]
async fn work_review_revision_and_dependency_advancement_keep_distinct_run_identity() {
    let team = requested_team();
    let (_server, client, store) = setup(&team).await;
    let mut follow_up = item("follow-up", false);
    follow_up.depends_on.push("checklist".into());
    queue(&client, &team, vec![item("checklist", true), follow_up]).await;
    cycle(&client, &store).await.unwrap();
    let first = current_run(&store);
    let claimed = backlog(&store);
    assert_eq!(claimed[0].status, "active");
    assert_eq!(claimed[0].run.as_deref(), Some(first.as_str()));
    assert!(
        store.lock().unwrap().tasks[&first]["spec"]["objective"]
            .as_str()
            .unwrap()
            .contains("No personal data")
    );
    complete(&store, &first, true);
    cycle(&client, &store).await.unwrap();
    assert!(
        store.lock().unwrap().approvals.is_empty(),
        "checkpoint must not bind before execution retires"
    );
    cycle(&client, &store).await.unwrap();
    assert_eq!(backlog(&store)[0].status, "awaiting_review");
    assert_eq!(
        store.lock().unwrap().team["status"]["health"],
        "AwaitingReview"
    );
    assert_eq!(
        store.lock().unwrap().tasks[&first]["spec"]["execution"]["launch"],
        false
    );
    assert_eq!(
        crate::team_commons::entry_count(&client, &latest(&store))
            .await
            .unwrap(),
        0
    );
    ask_again(&store, 2);
    cycle(&client, &store).await.unwrap();
    assert_eq!(
        current_run(&store),
        first,
        "review barrier must block a pending later request"
    );
    let first_approval = decide(&store, &first, false);
    cycle(&client, &store).await.unwrap();
    let second = current_run(&store);
    assert_ne!(first, second);
    let state = backlog(&store);
    assert_eq!(state[0].history.len(), 1);
    assert_eq!(state[0].history[0].outcome, "changes_requested");
    assert_ne!(
        state[0].history[0].run_uid,
        state[0].run_uid.clone().unwrap()
    );
    assert!(
        store.lock().unwrap().tasks[&second]["spec"]["objective"]
            .as_str()
            .unwrap()
            .contains("Remove personal data")
    );
    assert_eq!(state[1].status, "pending");
    complete(&store, &second, true);
    cycle(&client, &store).await.unwrap();
    cycle(&client, &store).await.unwrap();
    let second_approval = decide(&store, &second, true);
    assert_ne!(first_approval, second_approval);
    cycle(&client, &store).await.unwrap();
    cycle(&client, &store).await.unwrap();
    assert_eq!(backlog(&store)[0].status, "done");
    assert_eq!(backlog(&store)[0].history.len(), 2);
    assert_eq!(
        crate::team_commons::entry_count(&client, &latest(&store))
            .await
            .unwrap(),
        1
    );
    ask_again(&store, 3);
    cycle(&client, &store).await.unwrap();
    let third = current_run(&store);
    assert_ne!(third, second);
    assert_eq!(backlog(&store)[1].run.as_deref(), Some(third.as_str()));
    complete(&store, &third, true);
    cycle(&client, &store).await.unwrap();
    assert_eq!(
        backlog(&store)[1].history[0].outcome,
        "completed",
        "automatic completion is not human approval"
    );
}

#[tokio::test]
async fn changed_or_replaced_work_cannot_activate_a_reserved_run() {
    for fault in [
        "payload",
        "store-uid",
        "foreign-owner",
        "malformed",
        "missing-key",
        "second-claim",
    ] {
        let team = requested_team();
        let (_server, client, store) = setup(&team).await;
        queue(&client, &team, vec![item("checklist", true)]).await;
        let prepared = stage(&client, &store).await;
        let persisted = reserve(&client, &store, &prepared).await;
        {
            let mut state = store.lock().unwrap();
            let cm = state.cms.get_mut("kars-team-tasks-eng").unwrap();
            match fault {
                "store-uid" => cm["metadata"]["uid"] = json!("replacement"),
                "foreign-owner" => {
                    cm["metadata"]["ownerReferences"][0]["uid"] = json!("other-team")
                }
                "malformed" => cm["data"]["tasks.json"] = json!("not json"),
                "missing-key" => {
                    cm["data"].as_object_mut().unwrap().remove("tasks.json");
                }
                _ => {
                    let mut items =
                        contract::parse(cm["data"]["tasks.json"].as_str().unwrap()).unwrap();
                    if fault == "payload" {
                        items[0].description = "Changed work".into();
                    } else {
                        items[0].run_uid = Some("other-run-uid".into());
                    }
                    cm["data"]["tasks.json"] = json!(serde_json::to_string(&items).unwrap());
                }
            }
        }
        assert!(
            finish(&client, &persisted, &prepared).await.is_err(),
            "{fault}"
        );
        assert_ne!(
            store.lock().unwrap().tasks[&prepared.record.task_name]["spec"]["execution"]["launch"],
            true
        );
        assert!(store.lock().unwrap().activation_reservations.is_empty());
    }
}

#[tokio::test]
async fn failed_work_retains_attempt_and_never_creates_review_or_knowledge() {
    let team = requested_team();
    let (_server, client, store) = setup(&team).await;
    queue(&client, &team, vec![item("checklist", true)]).await;
    cycle(&client, &store).await.unwrap();
    let run = current_run(&store);
    complete(&store, &run, false);
    cycle(&client, &store).await.unwrap();
    cycle(&client, &store).await.unwrap();
    let items = backlog(&store);
    assert_eq!(items[0].status, "pending");
    assert!(items[0].run.is_none());
    assert_eq!(items[0].history.len(), 1);
    assert_eq!(items[0].history[0].outcome, "failed");
    assert!(store.lock().unwrap().approvals.is_empty());
    assert_eq!(
        crate::team_commons::entry_count(&client, &latest(&store))
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn failed_revision_preserves_review_feedback_for_the_next_fresh_run() {
    let team = requested_team();
    let (_server, client, store) = setup(&team).await;
    queue(&client, &team, vec![item("checklist", true)]).await;
    cycle(&client, &store).await.unwrap();
    let first = current_run(&store);
    complete(&store, &first, true);
    cycle(&client, &store).await.unwrap();
    cycle(&client, &store).await.unwrap();
    decide(&store, &first, false);
    cycle(&client, &store).await.unwrap();
    let feedback = backlog(&store)[0].feedback.clone().unwrap();
    assert!(feedback.contains("Remove personal data"));

    ask_again(&store, 2);
    cycle(&client, &store).await.unwrap();
    let second = current_run(&store);
    assert_ne!(second, first);
    complete(&store, &second, false);
    cycle(&client, &store).await.unwrap();
    cycle(&client, &store).await.unwrap();
    let items = backlog(&store);
    assert_eq!(items[0].status, "pending");
    assert_eq!(items[0].feedback.as_deref(), Some(feedback.as_str()));
    assert_eq!(items[0].history.len(), 2);
    assert_eq!(items[0].history[0].outcome, "changes_requested");
    assert_eq!(items[0].history[1].outcome, "failed");
    assert_eq!(
        items[0].history[1].feedback.as_deref(),
        Some("Previous run did not deliver successful output; inspect its retained records.")
    );
    assert_eq!(
        crate::team_commons::entry_count(&client, &latest(&store))
            .await
            .unwrap(),
        0
    );

    ask_again(&store, 3);
    cycle(&client, &store).await.unwrap();
    let third = current_run(&store);
    assert_ne!(third, second);
    assert_ne!(third, first);
    assert_eq!(backlog(&store)[0].history, items[0].history);
    assert!(
        store.lock().unwrap().tasks[&third]["spec"]["objective"]
            .as_str()
            .unwrap()
            .contains(&feedback)
    );
}

#[tokio::test]
async fn altered_checkpoint_or_output_cannot_release_the_review_gate() {
    for fault in [
        "owner",
        "nonce",
        "task-uid",
        "request",
        "output",
        "sealed-objective",
    ] {
        let team = requested_team();
        let (_server, client, store) = setup(&team).await;
        queue(&client, &team, vec![item("checklist", true)]).await;
        cycle(&client, &store).await.unwrap();
        let run = current_run(&store);
        complete(&store, &run, true);
        cycle(&client, &store).await.unwrap();
        cycle(&client, &store).await.unwrap();
        let name = decide(&store, &run, true);
        {
            let mut state = store.lock().unwrap();
            match fault {
                "output" => {
                    state
                        .cms
                        .get_mut(&format!("kars-mission-output-{run}"))
                        .unwrap()["data"]["output"] = json!("Changed bytes")
                }
                "sealed-objective" => {
                    state.tasks.get_mut(&run).unwrap()["spec"]["objective"] =
                        json!("Different task")
                }
                _ => {
                    let approval = state.approvals.get_mut(&name).unwrap();
                    match fault {
                        "owner" => {
                            approval["metadata"]["ownerReferences"][0]["uid"] = json!("other")
                        }
                        "nonce" => {
                            approval["metadata"]["annotations"][contract::RUN_NONCE] =
                                json!("other")
                        }
                        "task-uid" => approval["status"]["boundTaskUid"] = json!("other"),
                        _ => approval["spec"]["action"]["detail"] = json!("Changed request"),
                    }
                }
            }
        }
        assert!(cycle(&client, &store).await.is_err(), "{fault}");
        assert_eq!(backlog(&store)[0].status, "awaiting_review");
        assert_eq!(
            crate::team_commons::entry_count(&client, &latest(&store))
                .await
                .unwrap(),
            0
        );
    }
}

#[tokio::test]
async fn cadence_uses_the_same_work_claim_and_durable_admission_as_manual_runs() {
    let mut team = team();
    team.spec.envelope.budget = None;
    team.spec.cadence = Some(TeamCadence {
        every_minutes: Some(1),
        ..Default::default()
    });
    let (_server, client, store) = setup(&team).await;
    queue(&client, &team, vec![item("checklist", true)]).await;
    cycle(&client, &store).await.unwrap();
    assert!(latest(&store).annotations()["kars.azure.com/run-now"].starts_with("cadence-1-"));
    assert!(store.lock().unwrap().activation_reservations.is_empty());
    cycle(&client, &store).await.unwrap();
    let state = store.lock().unwrap();
    assert_eq!(state.activation_reservations.len(), 1);
    let run = state.team["status"]["runAdmission"]["taskName"]
        .as_str()
        .unwrap();
    let items = contract::parse(
        state.cms["kars-team-tasks-eng"]["data"]["tasks.json"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(items[0].run.as_deref(), Some(run));
    assert_eq!(
        items[0].run_uid.as_deref(),
        state.tasks[run]["metadata"]["uid"].as_str()
    );
}
