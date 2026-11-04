// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

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
                        evidence::failure_with_outputs(
                            record,
                            &task,
                            &phase,
                            vec![HANDOFF.into()],
                            peers,
                        )
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
