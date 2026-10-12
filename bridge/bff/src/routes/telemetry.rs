// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Never attribute a router trace by sandbox name. Live observations require
// the encrypted assignment's durable reply; terminal records retain their own fence.

use axum::extract::{Extension, Path, Query, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use kube::ResourceExt;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::convert::Infallible;
use std::time::Duration;
use tokio_stream::Stream;

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::kars::task::KarsTask;
use crate::routes::ownership::{require_owned_task, task_is_owned_by};
use crate::routes::tasks::run_evidence::{live, mission_result, scope_activity};
use crate::state::AppState;

#[derive(Default, Deserialize)]
pub struct StreamQuery {
    run_nonce: Option<String>,
}

struct Revision {
    namespace: String,
    name: String,
    uid: String,
    nonce: String,
}

impl Revision {
    fn matches(&self, task: &KarsTask, principal: &Principal) -> bool {
        task_is_owned_by(task, principal)
            && task.metadata.deletion_timestamp.is_none()
            && task.namespace().as_deref() == Some(self.namespace.as_str())
            && task.name_any() == self.name
            && task.uid().as_deref() == Some(self.uid.as_str())
            && task.annotations().get("kars.azure.com/run-requested") == Some(&self.nonce)
    }

    fn completed(&self, output: &BTreeMap<String, String>) -> bool {
        output.get("taskName") == Some(&self.name)
            && output.get("taskUid") == Some(&self.uid)
            && output.get("assignmentNonce") == Some(&self.nonce)
            && matches!(
                output.get("status").map(String::as_str),
                Some("ok" | "failed" | "rejected" | "error")
            )
            && (!output.contains_key("evidence.json")
                || mission_result(output).run_evidence.is_some())
    }
}

fn live_revoked_event() -> Event {
    Event::default()
        .event("router-activity-unavailable")
        .data("revision-bound detailed trace revoked")
}

/// A stream is pinned to the authenticated owner's Task UID and requested run.
/// Provisional evidence is cleared and the stream ends if custody or progress changes.
/// Completion means a terminal record exists, not that it is approvable.
pub async fn stream_mission(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
    Query(query): Query<StreamQuery>,
) -> AppResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    let cluster = state.cluster().ok_or(AppError::ClusterUnavailable)?.clone();
    if ns != "kars-system" {
        return Err(AppError::NotFound);
    }
    let task = require_owned_task(&cluster, &ns, &name, &principal).await?;
    let uid = task
        .uid()
        .filter(|s| !s.is_empty())
        .ok_or(AppError::NotFound)?;
    let nonce = task
        .annotations()
        .get("kars.azure.com/run-requested")
        .filter(|s| !s.is_empty())
        .cloned()
        .ok_or_else(|| AppError::Conflict("no mission revision is available to stream".into()))?;
    if query
        .run_nonce
        .as_ref()
        .is_some_and(|expected| expected != &nonce)
    {
        return Err(AppError::Conflict(
            "mission revision changed; refresh the mission".into(),
        ));
    }
    let revision = Revision {
        namespace: ns,
        name,
        uid,
        nonce,
    };
    if !revision.matches(&task, &principal) {
        return Err(AppError::NotFound);
    }
    let s = async_stream::stream! {
        let mut live_pin = live::StreamPin::default();
        for poll in 0..150 {
            let task = require_owned_task(&cluster, &revision.namespace, &revision.name, &principal).await;
            if !task.as_ref().is_ok_and(|task| revision.matches(task, &principal)) {
                if live_pin.has_snapshot() {
                    yield Ok(live_revoked_event());
                }
                yield Ok(Event::default().event("end").data("revision unavailable"));
                return;
            }
            let output = cluster.read_mission_output(&revision.name).await;
            let trace = cluster.read_current_mission_trace(&revision.name).await;
            let attempt = cluster.read_mission_attempt(&revision.uid, &revision.nonce).await;
            // Recheck after all evidence reads, before returning any bytes.
            let task = require_owned_task(&cluster, &revision.namespace, &revision.name, &principal).await;
            if !task.as_ref().is_ok_and(|task| revision.matches(task, &principal)) {
                if live_pin.has_snapshot() {
                    yield Ok(live_revoked_event());
                }
                yield Ok(Event::default().event("end").data("revision unavailable"));
                return;
            }
            if output.as_ref().is_some_and(|output| revision.completed(output)) {
                if !task.as_ref().is_ok_and(|task| {
                    task.annotations().get("kars.azure.com/run-completed") == Some(&revision.nonce)
                }) {
                    if live_pin.has_snapshot() {
                        yield Ok(live_revoked_event());
                    }
                    yield Ok(Event::default().event("end").data("terminal revision unavailable"));
                    return;
                }
                let events = scope_activity(
                    trace.and_then(|raw| serde_json::from_str(&raw).ok()).unwrap_or_default(),
                    Some(&revision.nonce),
                );
                let count = events.len();
                for event in events {
                    if let Ok(event) = Event::default().json_data(event) {
                        yield Ok(event);
                    }
                }
                yield Ok(Event::default().event("done").data(
                    serde_json::json!({"run_nonce": revision.nonce, "events": count}).to_string()
                ));
                break;
            }
            let snapshot = attempt.as_ref().zip(task.as_ref().ok())
                .and_then(|(attempt, task)| live::from_attempt(attempt, task));
            match live_pin.observe(snapshot) {
                live::StreamUpdate::Snapshot(snapshot) => {
                    yield Ok(Event::default().event("router-activity").data(snapshot));
                }
                live::StreamUpdate::Revoked => {
                    yield Ok(live_revoked_event());
                    break;
                }
                live::StreamUpdate::Unavailable if poll == 0 => {
                    yield Ok(Event::default().event("unavailable").data("revision-bound detailed trace unavailable"));
                }
                _ => {}
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        yield Ok(Event::default().event("end").data("stream closed"));
    };
    Ok(Sse::new(s).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::{Method, Request},
        response::{IntoResponse, Response},
    };
    use k8s_openapi::api::core::v1::ConfigMap;
    use live::tests::{append_progress, fixture, observed, record};
    use serde_json::{Value, json};
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    use tower::service_fn;

    fn revision() -> Revision {
        Revision {
            namespace: "kars-system".into(),
            name: "briefing".into(),
            uid: "task-uid".into(),
            nonce: "run-1".into(),
        }
    }

    #[test]
    fn stream_requires_same_task_revision_and_owner() {
        let task: KarsTask = serde_json::from_value(json!({
            "metadata":{"name":"briefing","namespace":"kars-system","uid":"task-uid",
                "annotations":{"kars.azure.com/owner-sub":"owner", "kars.azure.com/run-requested":"run-1"}},
            "spec":{"objective":"Briefing", "envelope":{"tier":1,"delegationDepth":0,"authorityCeiling":1}}
        })).unwrap();
        let principal = Principal {
            sub: "owner".into(),
            name: "Owner".into(),
            roles: vec![],
        };
        assert!(revision().matches(&task, &principal));
        for (key, value) in [
            ("kars.azure.com/owner-sub", "new-owner"),
            ("kars.azure.com/run-requested", "run-2"),
        ] {
            let mut changed = task.clone();
            changed
                .metadata
                .annotations
                .as_mut()
                .unwrap()
                .insert(key.into(), value.into());
            assert!(!revision().matches(&changed, &principal));
        }
        let mut replaced = task.clone();
        replaced.metadata.uid = Some("replacement".into());
        assert!(!revision().matches(&replaced, &principal));
        replaced = task.clone();
        replaced.metadata.namespace = Some("another".into());
        assert!(!revision().matches(&replaced, &principal));
        replaced = task;
        replaced.metadata.deletion_timestamp = Some(
            k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(chrono::Utc::now()),
        );
        assert!(!revision().matches(&replaced, &principal));
    }

    #[test]
    fn only_exact_terminal_output_finishes_stream() {
        let output: BTreeMap<String, String> = [
            ("taskName", "briefing"),
            ("taskUid", "task-uid"),
            ("assignmentNonce", "run-1"),
            ("status", "failed"),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect();
        assert!(revision().completed(&output));
        for (key, value) in [
            ("taskName", "another"),
            ("taskUid", "recreated"),
            ("assignmentNonce", "run-2"),
            ("status", "running"),
            ("evidence.json", "{}"),
        ] {
            let mut changed = output.clone();
            changed.insert(key.into(), value.into());
            assert!(!revision().completed(&changed));
        }
        assert!(!revision().completed(&BTreeMap::from([(
            "briefing.md".into(),
            "artifact bytes".into()
        )])));
    }

    fn owned_task() -> KarsTask {
        let (mut task, _, _) = fixture();
        task.metadata
            .annotations
            .as_mut()
            .unwrap()
            .insert("kars.azure.com/owner-sub".into(), "owner".into());
        task
    }

    fn stream_fixture(
        tasks: Vec<Option<KarsTask>>,
        attempts: Vec<Option<ConfigMap>>,
        outputs: Vec<Option<ConfigMap>>,
    ) -> (AppState, Arc<Mutex<Vec<String>>>) {
        assert!(!tasks.is_empty() && !attempts.is_empty() && !outputs.is_empty());
        let reads = Arc::new([
            AtomicUsize::new(0),
            AtomicUsize::new(0),
            AtomicUsize::new(0),
        ]);
        let calls = Arc::new(Mutex::new(Vec::new()));
        let requests = calls.clone();
        let service = service_fn(move |request: Request<_>| {
            assert_eq!(request.method(), Method::GET);
            let path = request.uri().path();
            requests.lock().unwrap().push(path.to_owned());
            let value = match path {
                "/apis/kars.azure.com/v1alpha1/namespaces/kars-system/karstasks/briefing" => {
                    let index = reads[0].fetch_add(1, Ordering::SeqCst).min(tasks.len() - 1);
                    tasks[index]
                        .as_ref()
                        .map(|v| serde_json::to_value(v).unwrap())
                }
                "/api/v1/namespaces/kars-system/configmaps/kars-mission-attempt-b91b6299b2f5d3e71056cd50f6e4ac6d9874eae1" =>
                {
                    let index = reads[1]
                        .fetch_add(1, Ordering::SeqCst)
                        .min(attempts.len() - 1);
                    attempts[index]
                        .as_ref()
                        .map(|v| serde_json::to_value(v).unwrap())
                }
                "/api/v1/namespaces/kars-system/configmaps/kars-mission-output-briefing" => {
                    let index = reads[2]
                        .fetch_add(1, Ordering::SeqCst)
                        .min(outputs.len() - 1);
                    outputs[index]
                        .as_ref()
                        .map(|v| serde_json::to_value(v).unwrap())
                }
                "/api/v1/namespaces/kars-system/configmaps/kars-mission-trace-briefing" => None,
                _ => panic!("unexpected stream API call: {path}"),
            };
            let status = if value.is_some() { 200 } else { 404 };
            let body = value.unwrap_or_else(|| {
                json!({"apiVersion":"v1","kind":"Status","status":"Failure",
                "reason":"NotFound","code":404,"message":"not found"})
            });
            async move {
                Ok::<_, std::io::Error>(
                    Response::builder()
                        .status(status)
                        .header("content-type", "application/json")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
            }
        });
        (
            AppState::for_test_client(kube::Client::new(service, "kars-system"), "kars-system"),
            calls,
        )
    }

    async fn stream_bytes(state: AppState) -> String {
        let stream = stream_mission(
            State(state),
            Extension(Principal {
                sub: "owner".into(),
                name: "Owner".into(),
                roles: vec![],
            }),
            Path(("kars-system".into(), "briefing".into())),
            Query(StreamQuery::default()),
        )
        .await
        .unwrap();
        let bytes = tokio::time::timeout(
            Duration::from_secs(12),
            to_bytes(stream.into_response().into_body(), 128 * 1024),
        )
        .await
        .unwrap()
        .unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    fn event_names(bytes: &str) -> Vec<&str> {
        bytes
            .lines()
            .filter_map(|line| line.strip_prefix("event:").map(str::trim))
            .collect()
    }

    #[tokio::test]
    async fn live_progress_deduplicates_and_missing_later_evidence_revokes_then_ends() {
        let (_, cm, mut attempt) = fixture();
        append_progress(&mut attempt);
        attempt["reply"]["routerObservations"] = json!(observed().to_string());
        let (state, calls) = stream_fixture(
            vec![Some(owned_task())],
            vec![
                Some(cm.clone()),
                Some(cm.clone()),
                Some(record(&cm, &attempt)),
                None,
            ],
            vec![None],
        );
        let bytes = stream_bytes(state).await;
        assert_eq!(
            event_names(&bytes),
            [
                "router-activity",
                "router-activity",
                "router-activity-unavailable",
                "end"
            ]
        );
        let snapshots: Vec<Value> = bytes
            .split("\n\n")
            .filter(|event| event.starts_with("event: router-activity\n"))
            .map(|event| {
                serde_json::from_str(
                    event
                        .lines()
                        .find_map(|line| line.strip_prefix("data:"))
                        .unwrap()
                        .trim(),
                )
                .unwrap()
            })
            .collect();
        assert_eq!(snapshots.len(), 2);
        assert_eq!(snapshots[0]["progress_sequence"], 2);
        assert_eq!(snapshots[1]["progress_sequence"], 3);
        assert_eq!(snapshots[1]["observations"], observed());
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 21);
        for poll in calls[1..].chunks_exact(5) {
            assert!(poll[0].ends_with("/karstasks/briefing"));
            assert!(poll[3].contains("kars-mission-attempt-"));
            assert!(poll[4].ends_with("/karstasks/briefing"));
        }
    }

    #[tokio::test]
    async fn initial_unavailable_keeps_polling_until_live_evidence_can_be_published() {
        let (_, cm, _) = fixture();
        let (state, _) = stream_fixture(
            vec![Some(owned_task())],
            vec![None, Some(cm), None],
            vec![None],
        );
        assert_eq!(
            event_names(&stream_bytes(state).await),
            [
                "unavailable",
                "router-activity",
                "router-activity-unavailable",
                "end"
            ]
        );
    }

    #[tokio::test]
    async fn invalid_or_recreated_attempt_revokes_previously_published_evidence() {
        let (_, cm, mut attempt) = fixture();
        attempt["events"][0]["evidence"] = json!({"model":"", "rounds":0,"usage":null});
        let mut recreated = cm.clone();
        recreated.metadata.uid = Some("replacement-attempt".into());
        for changed in [record(&cm, &attempt), recreated] {
            let (state, _) = stream_fixture(
                vec![Some(owned_task())],
                vec![Some(cm.clone()), Some(changed)],
                vec![None],
            );
            assert_eq!(
                event_names(&stream_bytes(state).await),
                ["router-activity", "router-activity-unavailable", "end"]
            );
        }
    }

    #[tokio::test]
    async fn fresh_owned_pinned_task_is_required_after_attempt_read_before_any_payload() {
        let task = owned_task();
        let (_, cm, _) = fixture();
        for mutation in ["owner", "uid", "nonce", "deleted", "missing"] {
            let mut changed = task.clone();
            match mutation {
                "owner" => {
                    changed
                        .metadata
                        .annotations
                        .as_mut()
                        .unwrap()
                        .insert("kars.azure.com/owner-sub".into(), "another".into());
                }
                "uid" => changed.metadata.uid = Some("recreated".into()),
                "nonce" => {
                    changed
                        .metadata
                        .annotations
                        .as_mut()
                        .unwrap()
                        .insert("kars.azure.com/run-requested".into(), "run-2".into());
                }
                "deleted" => {
                    changed.metadata.deletion_timestamp = Some(
                        k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(chrono::Utc::now()),
                    )
                }
                _ => {}
            }
            let (state, calls) = stream_fixture(
                vec![
                    Some(task.clone()),
                    Some(task.clone()),
                    (mutation != "missing").then_some(changed),
                ],
                vec![Some(cm.clone())],
                vec![None],
            );
            let bytes = stream_bytes(state).await;
            assert_eq!(event_names(&bytes), ["end"], "{mutation}");
            assert!(!bytes.contains("assignment-1"));
            let calls = calls.lock().unwrap();
            assert!(calls[4].contains("kars-mission-attempt-"));
            assert!(calls[5].ends_with("/karstasks/briefing"));
        }
    }

    #[tokio::test]
    async fn later_task_revocation_clears_live_evidence_before_ending_at_both_read_fences() {
        let task = owned_task();
        let (_, cm, _) = fixture();
        for valid_reads in [3, 4] {
            let mut tasks = vec![Some(task.clone()); valid_reads];
            tasks.push(None);
            let (state, _) = stream_fixture(tasks, vec![Some(cm.clone())], vec![None]);
            assert_eq!(
                event_names(&stream_bytes(state).await),
                ["router-activity", "router-activity-unavailable", "end"]
            );
        }
    }

    fn terminal() -> ConfigMap {
        serde_json::from_value(json!({"metadata":{"name":"kars-mission-output-briefing","namespace":"kars-system",
            "annotations":{"kars.azure.com/mission-task-uid":"task-uid","kars.azure.com/mission-run-nonce":"run-1",
                "kars.azure.com/mission-principal-name":"briefing"},
            "ownerReferences":[{"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTask","name":"briefing","uid":"task-uid","controller":true}]},
            "data":{"taskName":"briefing","taskUid":"task-uid","assignmentNonce":"run-1","status":"failed"}})).unwrap()
    }

    #[tokio::test]
    async fn committed_terminal_uses_done_not_provisional_revocation() {
        let task = owned_task();
        let mut complete = task.clone();
        complete
            .metadata
            .annotations
            .as_mut()
            .unwrap()
            .insert("kars.azure.com/run-completed".into(), "run-1".into());
        let (_, cm, _) = fixture();
        let (state, _) = stream_fixture(
            vec![
                Some(task.clone()),
                Some(task.clone()),
                Some(task),
                Some(complete),
            ],
            vec![Some(cm), None],
            vec![None, Some(terminal())],
        );
        let bytes = stream_bytes(state).await;
        assert_eq!(event_names(&bytes), ["router-activity", "done", "end"]);
        assert!(bytes.contains("\"run_nonce\":\"run-1\""));
    }

    #[tokio::test]
    async fn terminal_commit_marker_is_rechecked_after_all_evidence_reads() {
        let task = owned_task();
        let mut complete = task.clone();
        complete
            .metadata
            .annotations
            .as_mut()
            .unwrap()
            .insert("kars.azure.com/run-completed".into(), "run-1".into());
        let (_, cm, _) = fixture();
        let (state, _) = stream_fixture(
            vec![
                Some(task.clone()),
                Some(task.clone()),
                Some(task.clone()),
                Some(complete.clone()),
                Some(complete),
                Some(task),
            ],
            vec![Some(cm), None],
            vec![None, Some(terminal())],
        );
        let bytes = stream_bytes(state).await;
        assert_eq!(
            event_names(&bytes),
            ["router-activity", "router-activity-unavailable", "end"]
        );
        assert!(bytes.contains("terminal revision unavailable"));
    }
}
