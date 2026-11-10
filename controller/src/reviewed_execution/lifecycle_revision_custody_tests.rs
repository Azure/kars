// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

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
            &revision_roles(plan(&failed).unwrap()),
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
            &revision_roles(plan(&failed).unwrap()),
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
