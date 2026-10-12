// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use serde_json::json;

pub(crate) fn fixture() -> (KarsTask, ConfigMap, Value) {
    let task = serde_json::from_value(json!({
        "metadata":{"name":"briefing","namespace":"kars-system","uid":"task-uid",
            "annotations":{"kars.azure.com/run-requested":"run-1"}},
        "spec":{"objective":"Briefing","envelope":{"tier":1,"delegationDepth":0,"authorityCeiling":1}}
    })).unwrap();
    let target = json!({"taskName":"briefing","taskUid":"task-uid","runNonce":"run-1",
        "sandboxUid":"sandbox-uid","podUid":"pod-uid",
        "agentDid":format!("did:mesh:{}", "a".repeat(32)),
        "dispatcherDid":format!("did:mesh:{}", "b".repeat(32))});
    let mut candidate = target.clone();
    candidate["namespace"] = json!("kars-system");
    candidate["agentName"] = json!("briefing");
    candidate["content"] = json!("Write a useful briefing");
    let mut assignment = target;
    for (key, value) in [
        ("version", json!(1)),
        ("type", json!("mission:assign")),
        ("assignmentId", json!("assignment-1")),
        ("bootId", json!("boot-1")),
        ("content", candidate["content"].clone()),
        ("activityFormat", json!("router-observations-v1")),
    ] {
        assignment[key] = value;
    }
    let mut reply = assignment.clone();
    reply.as_object_mut().unwrap().remove("content");
    reply["type"] = json!("mission:reply");
    reply["status"] = json!("running");
    reply["evidence"] = json!({"model":"mini","rounds":1,"usage":{"promptTokens":2,"completionTokens":1,"totalTokens":3}});
    reply["routerObservations"] = json!(
        json!({"version":1,"source":"runtime-forwarded-router-observations",
        "coverage":"returned-responses-only","responses":1,"scope_id":null,"rounds":[],
        "state":"unavailable","reason":"Response correlation changed or was invalid"})
        .to_string()
    );
    let at = "2026-10-05T00:58:55.601Z";
    let attempt = json!({"version":1,"candidate":candidate,"assignment":assignment,
        "contentDigest":format!("sha256:{}",sha256_hex("Write a useful briefing")),
        "ownerSession":"session-1","startedAt":at,"updatedAt":at,"phase":"running",
        "events":[{"at":at,"status":"accepted"},{"at":at,"status":"running","evidence":reply["evidence"]}],"reply":reply});
    let cm = serde_json::from_value(json!({"metadata":{
        "name":"kars-mission-attempt-b91b6299b2f5d3e71056cd50f6e4ac6d9874eae1","namespace":"kars-system",
        "uid":"attempt-uid","resourceVersion":"5","ownerReferences":[{
            "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTask","name":"briefing","uid":"task-uid","controller":true
        }]},"data":{"attempt.json":attempt.to_string()}})).unwrap();
    (task, cm, attempt)
}

#[test]
fn latest_progress_is_projected_without_raw_content_or_result_authority() {
    let (task, cm, _) = fixture();
    let projected = serde_json::to_value(from_attempt(&cm, &task).unwrap()).unwrap();
    assert_eq!(projected.as_object().unwrap().len(), 12);
    assert_eq!(projected["assignment_id"], "assignment-1");
    assert_eq!(projected["progress_sequence"], 2);
    assert_eq!(projected["observations"]["state"], "unavailable");
    for forbidden in [
        "candidate",
        "content",
        "output",
        "artifacts",
        "evidence",
        "status",
    ] {
        assert!(projected.get(forbidden).is_none());
    }
}

#[test]
fn rejects_changed_assignment_capability_journal_and_terminal_snapshots() {
    let (task, cm, attempt) = fixture();
    for (pointer, value) in [
        ("/phase", json!("uncertain")),
        ("/phase", json!("succeeded")),
        ("/candidate/taskUid", json!("replacement")),
        ("/candidate/runNonce", json!("run-2")),
        ("/candidate/namespace", json!("elsewhere")),
        ("/candidate/content", json!("changed")),
        ("/contentDigest", json!("sha256:bad")),
        ("/assignment/bootId", json!("boot-2")),
        ("/assignment/assignmentId", json!("assignment-2")),
        ("/assignment/podUid", json!("pod-2")),
        ("/assignment/activityFormat", json!("unsupported")),
        ("/reply/status", json!("succeeded")),
        ("/reply/evidence/usage/totalTokens", json!(9)),
        ("/reply/routerObservations", json!("{}")),
        ("/events/1/evidence/rounds", json!(2)),
        ("/events/1/at", json!("2020-01-01T00:00:00Z")),
    ] {
        let mut changed = attempt.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        let mut record = cm.clone();
        record
            .data
            .as_mut()
            .unwrap()
            .insert("attempt.json".into(), changed.to_string());
        assert!(from_attempt(&record, &task).is_none(), "{pointer}");
    }
}

#[test]
fn rejects_deleted_recreated_completed_or_unowned_records() {
    let (task, cm, _) = fixture();
    for pointer in [
        "/metadata/uid",
        "/metadata/name",
        "/metadata/namespace",
        "/metadata/resourceVersion",
        "/metadata/ownerReferences/0/uid",
    ] {
        let mut changed = serde_json::to_value(&cm).unwrap();
        *changed.pointer_mut(pointer).unwrap() = json!("");
        assert!(
            from_attempt(&serde_json::from_value(changed).unwrap(), &task).is_none(),
            "{pointer}"
        );
    }
    let mut changed = cm.clone();
    changed
        .metadata
        .owner_references
        .as_mut()
        .unwrap()
        .push(cm.metadata.owner_references.as_ref().unwrap()[0].clone());
    assert!(from_attempt(&changed, &task).is_none());
    changed = cm.clone();
    changed.metadata.deletion_timestamp = Some(
        k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(chrono::Utc::now()),
    );
    assert!(from_attempt(&changed, &task).is_none());
    let mut changed_task = task.clone();
    changed_task.metadata.uid = Some("replacement".into());
    assert!(from_attempt(&cm, &changed_task).is_none());
    changed_task = task;
    changed_task
        .metadata
        .annotations
        .as_mut()
        .unwrap()
        .insert("kars.azure.com/run-completed".into(), "run-1".into());
    assert!(from_attempt(&cm, &changed_task).is_none());
}

pub(crate) fn record(cm: &ConfigMap, attempt: &Value) -> ConfigMap {
    let mut cm = cm.clone();
    cm.data
        .as_mut()
        .unwrap()
        .insert("attempt.json".into(), attempt.to_string());
    cm
}

pub(crate) fn observed() -> Value {
    serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../inference-router/tests/fixtures/router-observations-v1.json"
    )))
    .unwrap()
}

pub(crate) fn append_progress(attempt: &mut Value) {
    let mut event = attempt["events"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    event["at"] = json!("2026-10-05T00:59:00.601Z");
    attempt["updatedAt"] = event["at"].clone();
    attempt["events"].as_array_mut().unwrap().push(event);
}

fn projected(attempt: &Value) -> LiveActivity {
    let (task, cm, _) = fixture();
    from_attempt(&record(&cm, attempt), &task).unwrap()
}

#[test]
fn attempt_name_matches_independently_calculated_compact_json_sha256() {
    assert_eq!(
        Cluster::mission_attempt_name("task-uid", "run-1"),
        "kars-mission-attempt-b91b6299b2f5d3e71056cd50f6e4ac6d9874eae1"
    );
    assert_ne!(
        Cluster::mission_attempt_name("ab", "c"),
        Cluster::mission_attempt_name("a", "bc")
    );
    assert_ne!(
        Cluster::mission_attempt_name("task-uid", "run-1"),
        Cluster::mission_attempt_name("run-1", "task-uid")
    );
}

#[test]
fn actual_observed_fixture_is_sanitized_without_promoting_provisional_authority() {
    let (_, _, mut attempt) = fixture();
    let mut observations = observed();
    observations["untrusted"] = json!("secret raw text");
    observations["trace"]["input"] = json!("secret input");
    observations["trace"]["events"][0]["output"] = json!("secret output");
    attempt["reply"]["routerObservations"] = json!(observations.to_string());
    let projected = serde_json::to_value(projected(&attempt)).unwrap();
    assert_eq!(projected.as_object().unwrap().len(), 12);
    assert_eq!(projected["observations"], observed());
    assert!(!projected.to_string().contains("secret"));
    assert!(projected.get("source").is_none());
}

#[test]
fn validates_accepted_evidence_when_present_including_optional_v1_evidence() {
    let (task, cm, attempt) = fixture();
    assert!(from_attempt(&cm, &task).is_some());
    let mut valid = attempt.clone();
    valid["events"][0]["evidence"] = attempt["reply"]["evidence"].clone();
    assert!(from_attempt(&record(&cm, &valid), &task).is_some());
    for evidence in [
        Value::Null,
        json!({"model":"", "rounds":0, "usage":null}),
        json!({"model":"  ", "rounds":0, "usage":null}),
        json!({"model":"x".repeat(254), "rounds":0, "usage":null}),
        json!({"model":"mini", "rounds":-1, "usage":null}),
        json!({"model":"mini", "rounds":9_007_199_254_740_992_i64, "usage":null}),
        json!({"model":"mini", "rounds":0}),
        json!({"model":"mini", "rounds":0, "usage":{"promptTokens":-1,"completionTokens":0,"totalTokens":-1}}),
        json!({"model":"mini", "rounds":0, "usage":{"promptTokens":1,"completionTokens":0,"totalTokens":0}}),
        json!({"model":"mini", "rounds":0, "usage":{"promptTokens":0,"completionTokens":0,"totalTokens":0,"extra":0}}),
        json!({"model":"mini", "rounds":0, "usage":null,"unknown":"payload"}),
    ] {
        let mut changed = attempt.clone();
        changed["events"][0]["evidence"] = evidence.clone();
        assert!(
            from_attempt(&record(&cm, &changed), &task).is_none(),
            "{evidence}"
        );
    }
    let mut absent = attempt;
    absent["reply"].as_object_mut().unwrap().remove("evidence");
    absent["events"][1]
        .as_object_mut()
        .unwrap()
        .remove("evidence");
    assert!(from_attempt(&record(&cm, &absent), &task).is_some());
}

#[test]
fn reviewed_versions_allow_accepted_without_evidence_but_require_valid_running_evidence() {
    let (task, cm, base) = fixture();
    let phase = json!({"name":"read", "objective":"Read the assigned source documents",
        "capabilities":["filesystem-read"],"minToolCalls":0,"maxToolCalls":1,"freshContext":true});
    let digest = contract::phase_digest(&serde_json::from_value(phase.clone()).unwrap()).unwrap();
    for version in [2, 3, 4] {
        let mut attempt = base.clone();
        if version == 4 {
            for key in ["candidate", "assignment", "reply"] {
                attempt[key]["requiredOutputs"] = json!(["handoff.md", "sources.json"]);
            }
        }
        attempt["candidate"]["reviewedPhase"] = phase.clone();
        for key in ["assignment", "reply"] {
            attempt[key]["version"] = json!(version);
            attempt[key]["reviewedPhase"] = phase.clone();
            attempt[key]["phaseDigest"] = json!(digest);
            if version == 3 {
                attempt[key]["inputDigest"] = json!(format!("sha256:{}", "c".repeat(64)));
            }
        }
        attempt["reply"]["evidence"]["phase"] = json!({"name":"read", "attemptedToolCalls":0,
            "successfulToolCalls":0, "minToolCalls":0,"maxToolCalls":1});
        attempt["events"][1]["evidence"] = attempt["reply"]["evidence"].clone();
        assert!(
            from_attempt(&record(&cm, &attempt), &task).is_some(),
            "v{version} accepted absent"
        );
        if version == 4 {
            for key in ["candidate", "assignment", "reply"] {
                for names in [
                    None,
                    Some(Value::Null),
                    Some(json!([])),
                    Some(json!(["different.md"])),
                ] {
                    let mut changed = attempt.clone();
                    if let Some(names) = names {
                        changed[key]["requiredOutputs"] = names;
                    } else {
                        changed[key]
                            .as_object_mut()
                            .unwrap()
                            .remove("requiredOutputs");
                    }
                    assert!(
                        from_attempt(&record(&cm, &changed), &task).is_none(),
                        "changed {key} output contract"
                    );
                }
            }
            let mut inputs = attempt.clone();
            for key in ["assignment", "reply"] {
                inputs[key]["inputDigest"] = json!(format!("sha256:{}", "c".repeat(64)));
            }
            assert!(from_attempt(&record(&cm, &inputs), &task).is_some());
        }
        let mut accepted = attempt.clone();
        accepted["events"][0]["evidence"] = attempt["reply"]["evidence"].clone();
        assert!(from_attempt(&record(&cm, &accepted), &task).is_some());
        accepted["events"][0]["evidence"]["phase"]["attemptedToolCalls"] = json!(2);
        assert!(from_attempt(&record(&cm, &accepted), &task).is_none());
        attempt["reply"].as_object_mut().unwrap().remove("evidence");
        attempt["events"][1]
            .as_object_mut()
            .unwrap()
            .remove("evidence");
        assert!(
            from_attempt(&record(&cm, &attempt), &task).is_none(),
            "v{version} running absent"
        );
    }
}

#[test]
fn rejects_journal_payload_contamination_in_accepted_and_running_events() {
    let (task, cm, attempt) = fixture();
    for index in [0, 1] {
        for key in [
            "routerObservations",
            "output",
            "artifacts",
            "error",
            "assignmentId",
            "taskUid",
            "unknown",
        ] {
            let mut changed = attempt.clone();
            changed["events"][index][key] = Value::Null;
            assert!(
                from_attempt(&record(&cm, &changed), &task).is_none(),
                "event {index} {key}"
            );
        }
    }
}

#[test]
fn pin_allows_initial_absence_duplicates_and_append_only_progress() {
    let (_, cm, mut attempt) = fixture();
    let mut pin = StreamPin::default();
    for _ in 0..2 {
        assert_eq!(pin.observe(None), StreamUpdate::Unavailable);
    }
    assert!(!pin.has_snapshot());
    assert!(matches!(
        pin.observe(Some(projected(&attempt))),
        StreamUpdate::Snapshot(_)
    ));
    assert_eq!(
        pin.observe(Some(projected(&attempt))),
        StreamUpdate::Unchanged
    );
    let (task, _, _) = fixture();
    let mut new_resource_version = cm;
    new_resource_version.metadata.resource_version = Some("6".into());
    assert_eq!(
        pin.observe(from_attempt(&new_resource_version, &task)),
        StreamUpdate::Unchanged
    );
    append_progress(&mut attempt);
    attempt["reply"]["routerObservations"] = json!(observed().to_string());
    assert!(matches!(
        pin.observe(Some(projected(&attempt))),
        StreamUpdate::Snapshot(_)
    ));
    append_progress(&mut attempt);
    assert!(matches!(
        pin.observe(Some(projected(&attempt))),
        StreamUpdate::Snapshot(_)
    ));
    assert!(pin.has_snapshot());
}

#[test]
fn pin_rejects_each_custody_identity_change_even_with_advanced_progress() {
    let (_, _, attempt) = fixture();
    for field in 0..10 {
        let mut pin = StreamPin::default();
        pin.observe(Some(projected(&attempt)));
        let mut changed = projected(&attempt);
        changed.progress_sequence += 1;
        let target = match field {
            0 => &mut changed.task_uid,
            1 => &mut changed.run_nonce,
            2 => &mut changed.assignment_id,
            3 => &mut changed.agent_name,
            4 => &mut changed.agent_did,
            5 => &mut changed.dispatcher_did,
            6 => &mut changed.sandbox_uid,
            7 => &mut changed.pod_uid,
            8 => &mut changed.runtime_boot_id,
            _ => &mut changed.attempt_uid,
        };
        target.push_str("-replacement");
        assert_eq!(
            pin.observe(Some(changed)),
            StreamUpdate::Revoked,
            "identity {field}"
        );
        assert!(!pin.has_snapshot());
        assert_eq!(
            pin.observe(Some(projected(&attempt))),
            StreamUpdate::Revoked
        );
    }
}

#[test]
fn pin_rejects_regression_equal_sequence_mutation_and_scope_replacement_or_loss() {
    let (_, _, mut attempt) = fixture();
    attempt["reply"]["routerObservations"] = json!(observed().to_string());
    for case in [
        "sequence",
        "responses",
        "same-sequence",
        "scope",
        "lost-scope",
        "state",
        "missing",
    ] {
        let mut pin = StreamPin::default();
        pin.observe(Some(projected(&attempt)));
        let mut changed = projected(&attempt);
        let mut observations = observed();
        changed.progress_sequence += 1;
        match case {
            "sequence" => changed.progress_sequence = 1,
            "responses" => observations["responses"] = json!(2),
            "same-sequence" => {
                changed.progress_sequence -= 1;
                observations["responses"] = json!(4);
            }
            "scope" => {
                observations =
                    serde_json::from_str(&observations.to_string().replace("scope-a", "scope-b"))
                        .unwrap();
            }
            "lost-scope" | "state" => {
                observations = json!({"version":1,"source":"runtime-forwarded-router-observations",
                    "coverage":"returned-responses-only","responses":3,"scope_id":null,"rounds":[],
                    "state":"unavailable","reason":"Response correlation changed or was invalid"});
                if case == "state" {
                    observations["scope_id"] = json!("scope-a");
                    changed.progress_sequence -= 1;
                }
            }
            _ => {}
        }
        changed.observations = router_activity::parse(&observations.to_string()).unwrap();
        assert_eq!(
            pin.observe((case != "missing").then_some(changed)),
            StreamUpdate::Revoked,
            "{case}"
        );
        assert_eq!(pin.observe(None), StreamUpdate::Revoked);
    }
}

#[test]
fn pin_rejects_source_mutation_even_when_display_observations_are_unchanged() {
    let (_, _, attempt) = fixture();
    for (case, advance) in [
        ("journal", false),
        ("journal", true),
        ("owner", false),
        ("owner", true),
        ("content", false),
        ("content", true),
        ("updated", false),
        ("regressed-time", true),
    ] {
        let mut initial = attempt.clone();
        if case == "regressed-time" {
            initial["updatedAt"] = json!("2026-10-05T01:00:00Z");
        }
        let mut pin = StreamPin::default();
        pin.observe(Some(projected(&initial)));
        let mut changed = attempt.clone();
        if advance {
            append_progress(&mut changed);
        }
        match case {
            "journal" => changed["events"][0]["evidence"] = attempt["reply"]["evidence"].clone(),
            "owner" => changed["ownerSession"] = json!("session-replacement"),
            "content" => {
                let content = "Changed source objective";
                changed["candidate"]["content"] = json!(content);
                changed["assignment"]["content"] = json!(content);
                changed["contentDigest"] = json!(format!("sha256:{}", sha256_hex(content)));
            }
            "updated" => changed["updatedAt"] = json!("2026-10-05T01:00:00Z"),
            _ => {}
        }
        assert_eq!(
            pin.observe(Some(projected(&changed))),
            StreamUpdate::Revoked,
            "{case}, advancing: {advance}"
        );
    }
}
