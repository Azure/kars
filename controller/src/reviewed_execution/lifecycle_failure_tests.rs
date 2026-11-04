// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

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
