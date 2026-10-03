// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::Cluster;
use super::mission_records::{current_mission_record, select_mission_evidence_records};
use crate::kars::task::KarsTask;
use axum::{
    Json, Router,
    extract::State,
    http::{StatusCode, Uri},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

fn task() -> Value {
    json!({"apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsTask",
        "metadata":{"name":"briefing", "namespace":"kars-system", "uid":"task-uid",
            "annotations":{"kars.azure.com/run-requested":"run-1", "kars.azure.com/run-completed":"run-1"}},
        "spec":{"objective":"Write a briefing", "envelope":{"tier":1, "authorityCeiling":1}}})
}

fn record(kind: &str) -> Value {
    json!({"apiVersion":"v1", "kind":"ConfigMap",
        "metadata":{"name":format!("kars-mission-{kind}-briefing"), "namespace":"kars-system",
            "labels":{format!("kars.azure.com/mission-{kind}"):"evidence-key"},
            "annotations":{"kars.azure.com/mission-task-uid":"task-uid", "kars.azure.com/mission-run-nonce":"run-1",
                "kars.azure.com/mission-principal-name":"briefing", "kars.azure.com/mission-evidence-role":"current"},
            "ownerReferences":[{"apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsTask", "name":"briefing", "uid":"task-uid", "controller":true}]},
        "data": if kind == "output" { json!({"taskName":"briefing", "taskUid":"task-uid", "assignmentNonce":"run-1", "output":"Useful briefing"}) }
            else { json!({"response.md":"Useful briefing"}) }})
}

#[test]
fn current_records_require_matching_committed_identity() {
    let current: KarsTask = serde_json::from_value(task()).unwrap();
    for kind in ["output", "artifacts", "trace"] {
        assert!(current_mission_record(
            &serde_json::from_value(record(kind)).unwrap(),
            &current,
            kind == "output"
        ));
        for (pointer, replacement) in [
            ("/metadata/namespace", json!("another-namespace")),
            ("/metadata/deletionTimestamp", json!("2026-10-02T20:00:00Z")),
            (
                "/metadata/annotations/kars.azure.com~1mission-task-uid",
                json!("old-uid"),
            ),
            (
                "/metadata/annotations/kars.azure.com~1mission-run-nonce",
                json!("old-run"),
            ),
            (
                "/metadata/annotations/kars.azure.com~1mission-principal-name",
                json!("another-task"),
            ),
            ("/metadata/annotations", Value::Null),
            ("/metadata/ownerReferences", json!([])),
            ("/metadata/ownerReferences/0/uid", json!("old-uid")),
            ("/metadata/ownerReferences/0/name", json!("another-task")),
            ("/metadata/ownerReferences/0/kind", json!("KarsTeam")),
            ("/metadata/ownerReferences/0/apiVersion", json!("other/v1")),
            ("/metadata/ownerReferences/0/controller", json!(false)),
        ] {
            let mut cm = record(kind);
            if pointer.ends_with("deletionTimestamp") {
                cm["metadata"]["deletionTimestamp"] = replacement;
            } else {
                *cm.pointer_mut(pointer).unwrap() = replacement;
            }
            assert!(
                !current_mission_record(
                    &serde_json::from_value(cm).unwrap(),
                    &current,
                    kind == "output"
                ),
                "{kind}: {pointer}"
            );
        }
    }
    for (pointer, replacement) in [
        ("/metadata/uid", json!("recreated-task")),
        ("/metadata/uid", json!("")),
        ("/metadata/name", json!("other-task")),
        ("/metadata/namespace", json!("other-namespace")),
        (
            "/metadata/annotations/kars.azure.com~1run-requested",
            json!("run-2"),
        ),
        (
            "/metadata/annotations/kars.azure.com~1run-completed",
            json!("old-run"),
        ),
        ("/metadata/annotations", Value::Null),
    ] {
        let mut current = task();
        *current.pointer_mut(pointer).unwrap() = replacement;
        assert!(
            !current_mission_record(
                &serde_json::from_value(record("output")).unwrap(),
                &serde_json::from_value(current).unwrap(),
                true
            ),
            "{pointer}"
        );
    }
    for key in ["taskName", "taskUid", "assignmentNonce"] {
        let mut cm = record("output");
        cm["data"][key] = json!("mismatch");
        assert!(
            !current_mission_record(&serde_json::from_value(cm).unwrap(), &current, true),
            "{key}"
        );
    }
    let mut deleted = current.clone();
    deleted.metadata.deletion_timestamp =
        Some(serde_json::from_value(json!("2026-10-02T20:00:00Z")).unwrap());
    assert!(!current_mission_record(
        &serde_json::from_value(record("output")).unwrap(),
        &deleted,
        true
    ));
    let mut empty = current;
    empty.metadata.annotations = Some(BTreeMap::from([
        ("kars.azure.com/run-requested".into(), "".into()),
        ("kars.azure.com/run-completed".into(), "".into()),
    ]));
    let mut cm = record("output");
    cm["metadata"]["annotations"]["kars.azure.com/mission-run-nonce"] = json!("");
    cm["data"]["assignmentNonce"] = json!("");
    assert!(!current_mission_record(
        &serde_json::from_value(cm).unwrap(),
        &empty,
        true
    ));
}

#[test]
fn history_scopes_nonce_to_task_identity_and_deduplicates_mirrors() {
    let mut records = Vec::new();
    for (name, uid, nonce) in [
        ("a", "uid-a", "run-1"),
        ("b", "uid-b", "run-1"),
        ("a", "replacement-a", "run-1"),
        ("a", "uid-a", "rev-2"),
    ] {
        let data = BTreeMap::from([
            ("taskName".into(), name.into()),
            ("taskUid".into(), uid.into()),
            ("assignmentNonce".into(), nonce.into()),
        ]);
        records.push((
            format!("{uid}-{nonce}"),
            Some("canonical".into()),
            data.clone(),
        ));
        records.push((name.into(), Some("current".into()), data));
    }
    let selected = select_mission_evidence_records(records);
    assert_eq!(selected.len(), 4);
    assert!(selected.iter().all(|(key, _)| key != "a" && key != "b"));
    let legacy = ["a", "b"]
        .into_iter()
        .map(|name| {
            (
                name.into(),
                None,
                BTreeMap::from([
                    ("taskName".into(), name.into()),
                    ("assignmentNonce".into(), "same-nonce".into()),
                ]),
            )
        })
        .collect();
    assert_eq!(select_mission_evidence_records(legacy).len(), 2);
}

#[derive(Default)]
struct Store {
    task: Value,
    maps: Vec<Value>,
    reads: Vec<String>,
    fail_tasks: bool,
}

async fn handle(State(store): State<Arc<Mutex<Store>>>, uri: Uri) -> (StatusCode, Json<Value>) {
    let mut store = store.lock().unwrap();
    let path = uri.path();
    store.reads.push(path.into());
    let task_path = "/apis/kars.azure.com/v1alpha1/namespaces/kars-system/karstasks";
    if path.starts_with(task_path) && store.fail_tasks {
        return (
            StatusCode::FORBIDDEN,
            Json(
                json!({"apiVersion":"v1", "kind":"Status", "status":"Failure", "reason":"Forbidden", "message":"denied", "code":403}),
            ),
        );
    }
    let body = if path == task_path {
        json!({"apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsTaskList", "metadata":{}, "items":[store.task]})
    } else if path == format!("{task_path}/briefing") {
        store.task.clone()
    } else if path.ends_with("/configmaps") {
        json!({"apiVersion":"v1", "kind":"ConfigMapList", "metadata":{}, "items":store.maps})
    } else if let Some(cm) = store
        .maps
        .iter()
        .find(|cm| path.ends_with(&format!("/{}", cm["metadata"]["name"].as_str().unwrap())))
    {
        cm.clone()
    } else {
        return (
            StatusCode::NOT_FOUND,
            Json(
                json!({"apiVersion":"v1", "kind":"Status", "status":"Failure", "reason":"NotFound", "message":"missing", "code":404}),
            ),
        );
    };
    (StatusCode::OK, Json(body))
}

#[tokio::test]
async fn current_readers_and_downloads_hide_stale_or_unverifiable_results() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let mut artifacts = record("artifacts");
    artifacts["binaryData"] = json!({"image.bin":"AAH/"});
    let state = Arc::new(Mutex::new(Store {
        task: task(),
        maps: vec![record("output"), artifacts, {
            let mut trace = record("trace");
            trace["data"] = json!({"trace.json":"[]"});
            trace
        }],
        ..Default::default()
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = kube::Client::try_from(kube::Config::new(
        format!("http://{}", listener.local_addr().unwrap())
            .parse()
            .unwrap(),
    ))
    .unwrap();
    let server = tokio::spawn(
        axum::serve(
            listener,
            Router::new().fallback(handle).with_state(state.clone()),
        )
        .into_future(),
    );
    let cluster = Cluster { client };
    assert_eq!(
        cluster.read_mission_output("briefing").await.unwrap()["output"],
        "Useful briefing"
    );
    assert_eq!(
        cluster.read_mission_artifacts("briefing").await.unwrap()["response.md"],
        "Useful briefing"
    );
    assert_eq!(
        cluster
            .read_mission_artifact_bytes("briefing", "response.md")
            .await,
        Some((b"Useful briefing".to_vec(), false))
    );
    assert_eq!(
        cluster
            .read_mission_artifact_bytes("briefing", "image.bin")
            .await,
        Some((vec![0, 1, 255], true))
    );
    assert!(
        cluster
            .read_mission_artifact_bytes("briefing", "missing")
            .await
            .is_none()
    );
    assert_eq!(
        cluster
            .read_current_mission_trace("briefing")
            .await
            .as_deref(),
        Some("[]")
    );
    assert_eq!(cluster.list_mission_outputs().await.len(), 1);
    {
        let store = state.lock().unwrap();
        assert!(store.reads[0].ends_with("/configmaps/kars-mission-output-briefing"));
        assert!(store.reads[1].ends_with("/karstasks/briefing"));
    }
    for mode in [
        "pending",
        "recreated",
        "stale-completed",
        "forbidden",
        "unbound",
    ] {
        {
            let mut store = state.lock().unwrap();
            store.task = task();
            store.fail_tasks = mode == "forbidden";
            if mode == "recreated" {
                store.task["metadata"]["uid"] = json!("new-uid");
            }
            if mode == "pending" || mode == "stale-completed" {
                store.task["metadata"]["annotations"]["kars.azure.com/run-requested"] =
                    json!("run-2");
            }
            if mode == "stale-completed" {
                store.task["metadata"]["annotations"]["kars.azure.com/run-completed"] =
                    json!("run-2");
            }
            if mode == "unbound" {
                for cm in &mut store.maps {
                    cm["metadata"]["annotations"] = Value::Null;
                }
            }
        }
        assert!(
            cluster.read_mission_output("briefing").await.is_none(),
            "{mode}"
        );
        assert!(
            cluster.read_mission_artifacts("briefing").await.is_none(),
            "{mode}"
        );
        assert!(
            cluster
                .read_mission_artifact_bytes("briefing", "response.md")
                .await
                .is_none(),
            "{mode}"
        );
        assert!(
            cluster
                .read_mission_artifact_bytes("briefing", "image.bin")
                .await
                .is_none(),
            "{mode}"
        );
        assert!(
            cluster
                .read_current_mission_trace("briefing")
                .await
                .is_none(),
            "{mode}"
        );
        assert!(cluster.list_mission_outputs().await.is_empty(), "{mode}");
        assert_eq!(
            cluster.list_mission_output_evidence().await.len(),
            1,
            "history retained: {mode}"
        );
    }
    server.abort();
    let _ = server.await;
}
