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
fn stopped_progress_preserves_custody_and_never_becomes_a_terminal_result() {
    for (runtime, halted) in [
        ("OpenClaw", false),
        ("Hermes", false),
        ("OpenClaw", true),
        ("Hermes", true),
    ] {
        let (task, cm, mut attempt) = fixture();
        assert!(from_stopped_attempt(&cm, &task).is_none());
        let mut value = serde_json::to_value(task).unwrap();
        if halted {
            value["metadata"]["annotations"]["kars.azure.com/halted"] =
                json!("Operator halted this run");
        }
        value["spec"]["execution"] = json!({"launch":false});
        let stopped: KarsTask = serde_json::from_value(value.clone()).unwrap();
        attempt["candidate"]["runtime"] = json!(runtime);
        let record = record(&cm, &attempt);
        let projected = from_stopped_attempt(&record, &stopped).unwrap();
        assert_eq!(projected.runtime.as_deref(), Some(runtime));
        assert_eq!(projected.progress_sequence, 2);
        let dto = serde_json::to_value(projected).unwrap();
        for forbidden in [
            "result",
            "output",
            "artifacts",
            "usage",
            "source",
            "candidate",
        ] {
            assert!(dto.get(forbidden).is_none(), "{forbidden}");
        }
        for (path, replacement) in [
            ("/spec/execution/launch", json!(true)),
            ("/spec/execution", json!(null)),
            (
                "/metadata/annotations/kars.azure.com~1run-requested",
                json!(""),
            ),
            ("/metadata/uid", json!("replaced-task")),
            (
                "/metadata/annotations/kars.azure.com~1run-requested",
                json!("another-run"),
            ),
        ] {
            let mut changed = value.clone();
            *changed.pointer_mut(path).unwrap() = replacement;
            let changed = serde_json::from_value(changed).unwrap();
            assert!(from_stopped_attempt(&record, &changed).is_none(), "{path}");
        }
        value["metadata"]["annotations"]["kars.azure.com/run-completed"] = json!("run-1");
        assert!(from_stopped_attempt(&record, &serde_json::from_value(value).unwrap()).is_none());
        let mut changed = record.clone();
        changed.metadata.owner_references.as_mut().unwrap()[0].uid = "replaced-task".into();
        assert!(from_stopped_attempt(&changed, &stopped).is_none());
    }
}

#[test]
fn stopped_uncertain_dispatch_retains_only_validated_observations() {
    for runtime in ["OpenClaw", "Hermes"] {
        let (task, cm, mut attempt) = fixture();
        let mut value = serde_json::to_value(task).unwrap();
        value["spec"]["execution"] = json!({"launch":true});
        let mut task: KarsTask = serde_json::from_value(value).unwrap();
        attempt["candidate"]["runtime"] = json!(runtime);
        attempt["phase"] = json!("uncertain");
        attempt["error"] = json!("Run changed before annotation");
        let retained = record(&cm, &attempt);
        assert!(from_attempt(&retained, &task).is_none());
        assert!(from_stopped_attempt(&retained, &task).is_none());
        task.spec.execution.as_mut().unwrap().launch = false;
        assert!(from_attempt(&retained, &task).is_none());
        let projected = from_stopped_attempt(&retained, &task).unwrap();
        assert_eq!(projected.runtime.as_deref(), Some(runtime));
        let dto = serde_json::to_value(projected).unwrap();
        for key in ["error", "output", "artifacts", "result", "usage", "source"] {
            assert!(dto.get(key).is_none(), "{key}");
        }
        assert_eq!(attempt["phase"], "uncertain");
        assert!(attempt.get("error").is_some());
        for (path, replacement) in [
            ("/phase", json!("succeeded")),
            ("/phase", json!("failed")),
            ("/reply/status", json!("succeeded")),
            ("/candidate/taskUid", json!("another-task")),
            ("/assignment/runNonce", json!("another-run")),
            ("/error", json!({"message":"not a recorded error"})),
        ] {
            let mut changed = attempt.clone();
            *changed.pointer_mut(path).unwrap() = replacement;
            assert!(
                from_stopped_attempt(&record(&cm, &changed), &task).is_none(),
                "{path}"
            );
        }
        for field in ["output", "artifacts", "error"] {
            let mut changed = attempt.clone();
            changed["reply"][field] = json!("uncommitted content");
            assert!(
                from_stopped_attempt(&record(&cm, &changed), &task).is_none(),
                "{field}"
            );
        }
    }
}

#[test]
fn live_runtime_labels_use_pinned_candidate_and_revoke_on_change() {
    for runtime in ["OpenClaw", "Hermes"] {
        let (task, cm, mut attempt) = fixture();
        assert!(from_attempt(&cm, &task).unwrap().runtime.is_none());
        attempt["candidate"]["runtime"] = json!(runtime);
        let activity = from_attempt(&record(&cm, &attempt), &task).unwrap();
        assert_eq!(activity.runtime.as_deref(), Some(runtime));
        let mut pin = StreamPin::default();
        assert!(matches!(
            pin.observe(Some(activity)),
            StreamUpdate::Snapshot(_)
        ));
        attempt["candidate"]["runtime"] = json!(if runtime == "Hermes" {
            "OpenClaw"
        } else {
            "Hermes"
        });
        assert_eq!(
            pin.observe(from_attempt(&record(&cm, &attempt), &task)),
            StreamUpdate::Revoked
        );
        attempt["candidate"]["runtime"] = json!("unrecognized");
        assert!(
            from_attempt(&record(&cm, &attempt), &task)
                .unwrap()
                .runtime
                .is_none()
        );
    }
}

#[test]
fn peer_context_progress_requires_candidate_assignment_and_reply_agreement() {
    let (task, cm, mut attempt) = fixture();
    let peer = super::super::contract_tests::peer_reply();
    for key in ["reviewedPhase", "requiredOutputs", "peerContext"] {
        attempt["candidate"][key] = peer[key].clone();
        attempt["assignment"][key] = peer[key].clone();
        attempt["reply"][key] = peer[key].clone();
    }
    for key in ["version", "phaseDigest"] {
        attempt["assignment"][key] = peer[key].clone();
        attempt["reply"][key] = peer[key].clone();
    }
    attempt["reply"]["evidence"]["phase"] = peer["evidence"]["phase"].clone();
    attempt["events"][1]["evidence"] = attempt["reply"]["evidence"].clone();
    assert!(from_attempt(&record(&cm, &attempt), &task).is_some());
    for source in ["candidate", "assignment", "reply"] {
        let mut changed = attempt.clone();
        changed[source]["peerContext"]["rootUid"] = json!("different-root");
        assert!(from_attempt(&record(&cm, &changed), &task).is_none());
        let mut changed = attempt.clone();
        changed[source]
            .as_object_mut()
            .unwrap()
            .remove("peerContext");
        assert!(from_attempt(&record(&cm, &changed), &task).is_none());
    }
}

#[test]
fn native_memory_progress_keeps_revision_binding_and_producer_evidence() {
    let (task, cm, mut attempt) = fixture();
    let mut peer = super::super::contract_tests::peer_reply();
    peer["reviewedPhase"]["capabilities"] =
        json!(["filesystem-read", "filesystem-write", "memory", "network"]);
    let phase = serde_json::from_value(peer["reviewedPhase"].clone()).unwrap();
    peer["phaseDigest"] = json!(contract::phase_digest(&phase).unwrap());
    for source in ["candidate", "assignment", "reply"] {
        for key in ["reviewedPhase", "requiredOutputs", "peerContext"] {
            attempt[source][key] = peer[key].clone();
        }
    }
    for source in ["assignment", "reply"] {
        for key in ["version", "phaseDigest"] {
            attempt[source][key] = peer[key].clone();
        }
    }
    attempt["reply"]["evidence"]["phase"] = peer["evidence"]["phase"].clone();
    attempt["events"][1]["evidence"] = attempt["reply"]["evidence"].clone();
    let projected = from_attempt(&record(&cm, &attempt), &task).unwrap();
    assert_eq!(projected.progress_sequence, 2);
    for source in ["candidate", "assignment", "reply"] {
        let mut changed = attempt.clone();
        changed[source]["reviewedPhase"]["capabilities"] =
            json!(["filesystem-read", "filesystem-write", "network"]);
        assert!(from_attempt(&record(&cm, &changed), &task).is_none());
    }
}

#[test]
fn native_progress_beyond_legacy_event_window_retains_exact_evidence() {
    let (task, cm, mut attempt) = fixture();
    let mut peer = super::super::contract_tests::peer_reply();
    peer["reviewedPhase"]["maxToolCalls"] = json!(128);
    peer["evidence"]["phase"]["maxToolCalls"] = json!(128);
    let phase = serde_json::from_value(peer["reviewedPhase"].clone()).unwrap();
    peer["phaseDigest"] = json!(contract::phase_digest(&phase).unwrap());
    for source in ["candidate", "assignment", "reply"] {
        for key in ["reviewedPhase", "requiredOutputs", "peerContext"] {
            attempt[source][key] = peer[key].clone();
        }
    }
    for source in ["assignment", "reply"] {
        for key in ["version", "phaseDigest"] {
            attempt[source][key] = peer[key].clone();
        }
    }
    attempt["reply"]["evidence"]["phase"] = peer["evidence"]["phase"].clone();
    attempt["events"][1]["evidence"] = attempt["reply"]["evidence"].clone();
    let event = attempt["events"][1].clone();
    for _ in 0..260 {
        attempt["events"]
            .as_array_mut()
            .unwrap()
            .push(event.clone());
    }
    let projected = from_attempt(&record(&cm, &attempt), &task).unwrap();
    assert_eq!(projected.progress_sequence, 262);
    attempt["events"][200]["evidence"]["phase"]["successfulToolCalls"] = json!(129);
    assert!(from_attempt(&record(&cm, &attempt), &task).is_none());
}

#[test]
fn latest_progress_is_projected_without_raw_content_or_result_authority() {
    let (task, cm, _) = fixture();
    let projected = serde_json::to_value(from_attempt(&cm, &task).unwrap()).unwrap();
    assert_eq!(projected.as_object().unwrap().len(), 13);
    assert!(projected["runtime"].is_null());
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
        k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(k8s_openapi::jiff::Timestamp::now()),
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
    assert_eq!(projected.as_object().unwrap().len(), 13);
    assert!(projected["runtime"].is_null());
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
fn pin_uses_native_tool_count_and_rejects_coverage_switches() {
    let (_, _, attempt) = fixture();
    let native = |count| {
        router_activity::parse(&json!({
        "version":1,"source":"runtime-forwarded-router-observations",
        "coverage":"native-tool-correlated-only","correlated_tools":count,
        "scope_id":"scope-a","rounds":[1],"state":"unavailable",
        "reason":"Native tool-correlated observations unavailable (reset, expiry, truncation or read failure)"
    }).to_string()).unwrap()
    };
    for (count, allowed) in [(1, false), (2, true), (3, true)] {
        let mut pin = StreamPin::default();
        let mut first = projected(&attempt);
        first.observations = native(2);
        assert!(matches!(
            pin.observe(Some(first)),
            StreamUpdate::Snapshot(_)
        ));
        let mut next = projected(&attempt);
        next.progress_sequence += 1;
        next.observations = native(count);
        assert_eq!(
            matches!(pin.observe(Some(next)), StreamUpdate::Snapshot(_)),
            allowed
        );
    }
    for native_first in [false, true] {
        let mut pin = StreamPin::default();
        let mut first = projected(&attempt);
        first.observations = if native_first {
            native(3)
        } else {
            router_activity::parse(&observed().to_string()).unwrap()
        };
        pin.observe(Some(first));
        let mut next = projected(&attempt);
        next.progress_sequence += 1;
        next.observations = if native_first {
            router_activity::parse(&observed().to_string()).unwrap()
        } else {
            native(3)
        };
        assert_eq!(pin.observe(Some(next)), StreamUpdate::Revoked);
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
