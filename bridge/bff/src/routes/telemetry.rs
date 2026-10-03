// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Router traces currently lack revision/runtime custody. Do not attribute them
// by sandbox name; stream only current, bound terminal records.

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
use crate::routes::tasks::run_evidence::{mission_result, scope_activity};
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

/// A stream is pinned to the authenticated owner's Task UID and requested run.
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
        for poll in 0..150 {
            let task = require_owned_task(&cluster, &revision.namespace, &revision.name, &principal).await;
            if !task.as_ref().is_ok_and(|task| revision.matches(task, &principal)) {
                yield Ok(Event::default().event("end").data("revision unavailable"));
                break;
            }
            let output = cluster.read_mission_output(&revision.name).await;
            let trace = cluster.read_current_mission_trace(&revision.name).await;
            // Recheck after all evidence reads, before returning any bytes.
            let task = require_owned_task(&cluster, &revision.namespace, &revision.name, &principal).await;
            if !task.as_ref().is_ok_and(|task| revision.matches(task, &principal)) {
                yield Ok(Event::default().event("end").data("revision unavailable"));
                break;
            }
            if output.as_ref().is_some_and(|output| revision.completed(output)) {
                if !task.as_ref().is_ok_and(|task| {
                    task.annotations().get("kars.azure.com/run-completed") == Some(&revision.nonce)
                }) {
                    yield Ok(Event::default().event("end").data("terminal revision unavailable"));
                    break;
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
            if poll == 0 {
                yield Ok(Event::default().event("unavailable").data("revision-bound detailed trace unavailable"));
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
    use serde_json::json;

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
}
