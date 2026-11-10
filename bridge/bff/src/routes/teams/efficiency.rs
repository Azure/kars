// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{lifecycle::owned_run, work, work_error, work_owner};
use crate::error::AppResult;
use crate::kars::{
    cluster::{Cluster, MissionDeliverableRecord},
    team::KarsTeam,
};
use k8s_openapi::api::core::v1::ConfigMap;
use kube::ResourceExt;

pub(crate) async fn accepted_work_output(
    cluster: &Cluster,
    record: &MissionDeliverableRecord,
) -> AppResult<bool> {
    let Some(task) = record.task.as_ref() else {
        return Ok(false);
    };
    if !task.annotations().contains_key(work::ANNOTATION) {
        return Ok(false);
    }
    let Some(owner) = task.metadata.owner_references.as_ref().and_then(|owners| {
        owners
            .iter()
            .find(|owner| owner.controller == Some(true) && owner.kind == "KarsTeam")
    }) else {
        return Ok(false);
    };
    let Some(ns) = task.metadata.namespace.as_deref() else {
        return Ok(false);
    };
    let Some(team) = cluster.teams(ns).get_opt(&owner.name).await? else {
        return Ok(false);
    };
    if !owned_run(&task.metadata, &team) || team.metadata.deletion_timestamp.is_some() {
        return Ok(false);
    }
    let owner = work_owner(&team)?;
    let Some((cm, items)) = work::read(&cluster.configmaps(ns), &owner)
        .await
        .map_err(work_error)?
    else {
        return Ok(false);
    };
    Ok(approved_history_matches(record, &team, &cm, &items))
}

fn approved_history_matches(
    record: &MissionDeliverableRecord,
    team: &KarsTeam,
    cm: &ConfigMap,
    items: &[work::TeamTask],
) -> bool {
    let Some(task) = record.task.as_ref() else {
        return false;
    };
    let Ok(owner) = work_owner(team) else {
        return false;
    };
    if task.metadata.deletion_timestamp.is_some()
        || !owned_run(&task.metadata, team)
        || !owner.owns(cm)
        || work::validate(items).is_err()
    {
        return false;
    }
    let Some(binding) = task
        .annotations()
        .get(work::ANNOTATION)
        .and_then(|raw| serde_json::from_str::<work::Binding>(raw).ok())
    else {
        return false;
    };
    if cm.metadata.uid.as_ref() != Some(&binding.backlog_uid) {
        return false;
    }
    let data = &record.output.data;
    let (Some(uid), Some(name), Some(nonce), Some(digest)) = (
        data.get("taskUid"),
        data.get("taskName"),
        data.get("assignmentNonce"),
        record.output_sha256.as_ref(),
    ) else {
        return false;
    };
    if task.metadata.uid.as_ref() != Some(uid) || task.metadata.name.as_ref() != Some(name) {
        return false;
    }
    // The controller journals "approved" only after consuming the exact bound
    // checkpoint. Ordinary completion and descendant handoffs are not approval.
    // Use immutable history, not the work item's editable next-run request.
    items
        .iter()
        .filter(|item| item.id == binding.item_id)
        .any(|item| {
            item.history.iter().any(|attempt| {
                attempt.outcome == "approved"
                    && &attempt.run == name
                    && &attempt.run_uid == uid
                    && &attempt.assignment_nonce == nonce
                    && attempt.output_sha256.as_ref() == Some(digest)
                    && data.get("finishedAt") == Some(&attempt.finished_at)
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kars::cluster::MissionOutputRecord;
    use serde_json::{Value, json};

    fn fixture() -> (
        MissionDeliverableRecord,
        KarsTeam,
        ConfigMap,
        Vec<work::TeamTask>,
    ) {
        let owner = json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTeam","name":"team","uid":"team-uid","controller":true});
        let task = serde_json::from_value(json!({
            "metadata":{"name":"run","namespace":"kars-system","uid":"run-uid","ownerReferences":[owner.clone()],
                "annotations":{(work::ANNOTATION):json!({"backlog_uid":"backlog-uid","item_id":"item","request_digest":"request"}).to_string()}},
            "spec":{"objective":"Write a report","envelope":{"tier":1,"authorityCeiling":1}}
        })).unwrap();
        let team = serde_json::from_value(json!({
            "metadata":{"name":"team","namespace":"kars-system","uid":"team-uid"},
            "spec":{"charter":"Reports","envelope":{"tier":1,"authorityCeiling":1}}
        }))
        .unwrap();
        let cm = serde_json::from_value(json!({
            "metadata":{"name":"kars-team-tasks-team","namespace":"kars-system","uid":"backlog-uid","resourceVersion":"1","ownerReferences":[owner]}
        })).unwrap();
        let digest = "a".repeat(64);
        let at = "2026-10-15T19:47:20Z";
        let item = work::TeamTask {
            id: "item".into(),
            title: "Report".into(),
            status: "done".into(),
            review_required: true,
            run: Some("run".into()),
            run_uid: Some("run-uid".into()),
            assignment_nonce: Some("nonce".into()),
            history: vec![work::Attempt {
                run: "run".into(),
                run_uid: "run-uid".into(),
                assignment_nonce: "nonce".into(),
                outcome: "approved".into(),
                finished_at: at.into(),
                output_sha256: Some(digest.clone()),
                feedback: None,
            }],
            ..Default::default()
        };
        let record = MissionDeliverableRecord {
            output: MissionOutputRecord {
                task_name: "run".into(),
                evidence_key: "nonce".into(),
                data: [
                    ("taskName", "run"),
                    ("taskUid", "run-uid"),
                    ("assignmentNonce", "nonce"),
                    ("finishedAt", at),
                ]
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
            },
            task: Some(task),
            output_sha256: Some(digest),
            current: true,
            artifacts: None,
        };
        (record, team, cm, vec![item])
    }

    #[tokio::test]
    async fn team_efficiency_reads_bound_history_and_propagates_failed_reads() {
        use axum::{
            Json, Router,
            extract::State,
            http::{StatusCode, Uri},
        };
        use std::sync::{Arc, Mutex};
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let (record, team, mut cm, items) = fixture();
        cm.data = Some([("tasks.json".into(), serde_json::to_string(&items).unwrap())].into());
        let responses = Arc::new(Mutex::new((
            serde_json::to_value(team).unwrap(),
            serde_json::to_value(cm).unwrap(),
            String::new(),
        )));
        async fn handle(
            State(state): State<Arc<Mutex<(Value, Value, String)>>>,
            uri: Uri,
        ) -> (StatusCode, Json<Value>) {
            let state = state.lock().unwrap();
            if !state.2.is_empty() && uri.path().contains(&state.2) {
                return (
                    StatusCode::FORBIDDEN,
                    Json(
                        json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"Forbidden","code":403,"message":"denied"}),
                    ),
                );
            }
            if uri.path().ends_with("/karsteams/team") {
                (StatusCode::OK, Json(state.0.clone()))
            } else if uri.path().ends_with("/configmaps/kars-team-tasks-team") {
                (StatusCode::OK, Json(state.1.clone()))
            } else {
                (
                    StatusCode::NOT_FOUND,
                    Json(
                        json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"NotFound","code":404,"message":"missing"}),
                    ),
                )
            }
        }
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
                Router::new().fallback(handle).with_state(responses.clone()),
            )
            .into_future(),
        );
        let cluster = Cluster::for_test_client(client);
        assert!(accepted_work_output(&cluster, &record).await.unwrap());
        for path in ["karsteams", "configmaps"] {
            responses.lock().unwrap().2 = path.into();
            assert!(
                accepted_work_output(&cluster, &record).await.is_err(),
                "{path}"
            );
        }
        server.abort();
        let _ = server.await;
    }

    #[test]
    fn only_exact_controller_journaled_approval_counts() {
        let (mut record, team, cm, items) = fixture();
        assert!(approved_history_matches(&record, &team, &cm, &items));
        for outcome in ["completed", "changes_requested", "failed", "cancelled"] {
            let mut changed = items.clone();
            changed[0].history[0].outcome = outcome.into();
            assert!(
                !approved_history_matches(&record, &team, &cm, &changed),
                "{outcome}"
            );
        }
        for field in [
            "run",
            "run_uid",
            "assignment_nonce",
            "output_sha256",
            "finished_at",
        ] {
            let mut changed = serde_json::to_value(&items).unwrap();
            changed[0]["history"][0][field] = json!("different");
            assert!(
                !approved_history_matches(
                    &record,
                    &team,
                    &cm,
                    &serde_json::from_value::<Vec<work::TeamTask>>(changed).unwrap()
                ),
                "{field}"
            );
        }
        for field in ["taskName", "taskUid", "assignmentNonce", "finishedAt"] {
            let old = record
                .output
                .data
                .insert(field.into(), "different".into())
                .unwrap();
            assert!(
                !approved_history_matches(&record, &team, &cm, &items),
                "{field}"
            );
            record.output.data.insert(field.into(), old);
        }
        record.output_sha256 = Some("b".repeat(64));
        assert!(!approved_history_matches(&record, &team, &cm, &items));
        record.output_sha256 = None;
        assert!(!approved_history_matches(&record, &team, &cm, &items));
    }

    #[test]
    fn history_survives_next_work_request_without_transferring_acceptance() {
        let (mut record, team, cm, mut items) = fixture();
        record.current = false;
        items[0].status = "pending".into();
        items[0].run = None;
        items[0].run_uid = None;
        items[0].assignment_nonce = None;
        items[0].feedback = Some("Changed next-run requirements".into());
        assert!(approved_history_matches(&record, &team, &cm, &items));
        items[0].id = "another-item".into();
        assert!(!approved_history_matches(&record, &team, &cm, &items));
    }

    #[test]
    fn replaced_team_backlog_task_and_child_handoffs_do_not_inherit_approval() {
        let (mut record, team, cm, items) = fixture();
        for (pointer, value) in [
            ("/metadata/uid", json!("replaced")),
            ("/metadata/namespace", json!("foreign")),
            ("/metadata/ownerReferences/0/uid", json!("foreign")),
            ("/metadata/ownerReferences/0/controller", json!(false)),
        ] {
            let mut changed = serde_json::to_value(&cm).unwrap();
            *changed.pointer_mut(pointer).unwrap() = value;
            assert!(
                !approved_history_matches(
                    &record,
                    &team,
                    &serde_json::from_value(changed).unwrap(),
                    &items
                ),
                "{pointer}"
            );
        }
        for (pointer, value) in [
            ("/metadata/uid", json!("replaced-task")),
            ("/metadata/ownerReferences/0/kind", json!("KarsTask")),
            ("/metadata/ownerReferences/0/uid", json!("foreign")),
            ("/metadata/annotations", Value::Null),
        ] {
            let original = record.task.take().unwrap();
            let mut changed = serde_json::to_value(&original).unwrap();
            *changed.pointer_mut(pointer).unwrap() = value;
            record.task = Some(serde_json::from_value(changed).unwrap());
            assert!(
                !approved_history_matches(&record, &team, &cm, &items),
                "{pointer}"
            );
            record.task = Some(original);
        }
        let mut replacement = team.clone();
        replacement.metadata.uid = Some("replaced-team".into());
        assert!(!approved_history_matches(
            &record,
            &replacement,
            &cm,
            &items
        ));
    }
}
