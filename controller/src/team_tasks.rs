//! Team task backlog — a durable, ConfigMap-backed queue of discrete tasks an
//! operator assigns to a standing team (beyond its always-on charter). A team is
//! a persistent org: you give a "finance" or "marketing" team a backlog of
//! tasks (a, b, c, d), and each standing run picks up the next `pending` task,
//! works it, and marks it `done` — so progress is durable and visible.
//!
//! Stored in `kars-team-tasks-<team>` (data key `tasks.json`) rather than on the
//! KarsTeam CRD, so the queue can be mutated by the Bridge and drained by the
//! controller without CRD schema churn or admission-webhook contention. Same
//! pattern as the team commons.

use k8s_openapi::api::core::v1::ConfigMap;
use kube::{
    Client,
    api::{Api, Patch, PatchParams},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;

/// One backlog task. `run` links the task to the KarsTask that is (or was)
/// working it, so harvest can mark it done when that run delivers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamTask {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    /// `pending` | `active` | `done`.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done_at: Option<String>,
    /// When this task first became `active`. A run that dies without delivering
    /// (mesh peer lost, agent crashed) would otherwise leave the task `active`
    /// forever, blocking the whole backlog; this timestamp lets the reconciler
    /// reset a stale `active` task back to `pending`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stuck_since: Option<String>,
}

fn namespace() -> String {
    std::env::var("KARS_NAMESPACE").unwrap_or_else(|_| "kars-system".into())
}

/// ConfigMap name holding a team's task backlog.
pub fn tasks_cm_name(team: &str) -> String {
    format!("kars-team-tasks-{team}")
}

/// Read a team's task backlog. Missing/empty ⇒ `[]`.
pub async fn read_tasks(client: &Client, team: &str) -> Vec<TeamTask> {
    let cms: Api<ConfigMap> = Api::namespaced(client.clone(), &namespace());
    let Some(cm) = cms.get_opt(&tasks_cm_name(team)).await.ok().flatten() else {
        return Vec::new();
    };
    cm.data
        .as_ref()
        .and_then(|d| d.get("tasks.json"))
        .and_then(|s| serde_json::from_str::<Vec<TeamTask>>(s).ok())
        .unwrap_or_default()
}

/// The next task an idle team should pick up: the oldest `pending` task.
pub fn next_pending(tasks: &[TeamTask]) -> Option<&TeamTask> {
    tasks.iter().find(|t| t.status == "pending")
}

/// Whether the team already has a task in flight (its run hasn't delivered yet),
/// so we don't start a second task concurrently.
pub fn has_active(tasks: &[TeamTask]) -> bool {
    tasks.iter().any(|t| t.status == "active")
}

/// Persist the full task list (server-side apply; the ConfigMap is small).
async fn write_tasks(client: &Client, team: &str, tasks: &[TeamTask]) -> Result<(), kube::Error> {
    let cms: Api<ConfigMap> = Api::namespaced(client.clone(), &namespace());
    let name = tasks_cm_name(team);
    let mut data = BTreeMap::new();
    data.insert(
        "tasks.json".to_string(),
        serde_json::to_string(tasks).unwrap_or_else(|_| "[]".into()),
    );
    let patch = json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": { "name": name, "labels": { "kars.azure.com/team-tasks": team } },
        "data": data,
    });
    cms.patch(
        &name,
        &PatchParams::apply(crate::field_managers::CLAW_TEAM).force(),
        &Patch::Apply(patch),
    )
    .await?;
    Ok(())
}

/// Mark a task `active` and bind it to the run that will work it. Returns the
/// updated list (already persisted).
pub async fn mark_active(
    client: &Client,
    team: &str,
    task_id: &str,
    run: &str,
) -> Result<(), kube::Error> {
    let now = chrono::Utc::now().to_rfc3339();
    let mut tasks = read_tasks(client, team).await;
    for t in tasks.iter_mut() {
        if t.id == task_id {
            t.status = "active".into();
            t.run = Some(run.to_string());
            t.stuck_since = Some(now.clone());
        }
    }
    write_tasks(client, team, &tasks).await
}

/// A task active longer than this (with its run still present) is treated as
/// hung and requeued. Set comfortably above the mesh delivery ceiling
/// (ABS_MAX_SECS = 1800s) plus harvest, so a legitimately long run is never
/// reset out from under itself.
const STUCK_TASK_TIMEOUT_MINS: i64 = 60;

/// Requeue any `active` backlog task that is stuck: its bound run KarsTask no
/// longer exists (GC'd / deleted / never materialized), or it has been active
/// past STUCK_TASK_TIMEOUT_MINS without delivering. Without this, a run that
/// dies (mesh peer lost, agent crash) leaves the task `active` forever, so
/// `has_active()` stays true and the whole backlog is permanently blocked.
/// Returns true if any task was reset. Best-effort per-task run lookups.
pub async fn reset_stale_active_tasks(client: &Client, team: &str) -> Result<bool, kube::Error> {
    use crate::kars_task::KarsTask;
    let mut tasks = read_tasks(client, team).await;
    if !tasks.iter().any(|t| t.status == "active") {
        return Ok(false);
    }
    let runs: Api<KarsTask> = Api::namespaced(client.clone(), &namespace());
    let now = chrono::Utc::now();
    let mut changed = false;
    for t in tasks.iter_mut() {
        if t.status != "active" {
            continue;
        }
        let run_exists = match &t.run {
            Some(r) => runs.get_opt(r).await?.is_some(),
            None => false,
        };
        let stuck_mins = t
            .stuck_since
            .as_deref()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|s| (now - s.with_timezone(&chrono::Utc)).num_minutes())
            .unwrap_or(0);
        if !run_exists || stuck_mins > STUCK_TASK_TIMEOUT_MINS {
            t.status = "pending".into();
            t.run = None;
            t.stuck_since = None;
            changed = true;
        } else if t.stuck_since.is_none() {
            // Legacy active task (pre-field) — start its clock now.
            t.stuck_since = Some(now.to_rfc3339());
            changed = true;
        }
    }
    if changed {
        write_tasks(client, team, &tasks).await?;
    }
    Ok(changed)
}

/// Mark the `active` task bound to `run` as `done`. No-op if none matches.
/// Returns true when a task was transitioned (so the caller can log/act).
pub async fn mark_done_for_run(
    client: &Client,
    team: &str,
    run: &str,
    now: &str,
) -> Result<bool, kube::Error> {
    let mut tasks = read_tasks(client, team).await;
    let changed = mark_done(&mut tasks, run, now);
    if changed {
        write_tasks(client, team, &tasks).await?;
    }
    Ok(changed)
}

/// Requeue the `active` backlog task bound to a failed run. The failed run stays
/// linked in its own durable evidence, while the work item returns to `pending`
/// for an explicit Run now or the next cadence tick.
pub async fn requeue_for_run(client: &Client, team: &str, run: &str) -> Result<bool, kube::Error> {
    let mut tasks = read_tasks(client, team).await;
    let changed = requeue_run(&mut tasks, run);
    if changed {
        write_tasks(client, team, &tasks).await?;
    }
    Ok(changed)
}

fn mark_done(tasks: &mut [TeamTask], run: &str, now: &str) -> bool {
    let mut changed = false;
    for task in tasks {
        if task.status == "active" && task.run.as_deref() == Some(run) {
            task.status = "done".into();
            task.done_at = Some(now.to_string());
            task.stuck_since = None;
            changed = true;
        }
    }
    changed
}

fn requeue_run(tasks: &mut [TeamTask], run: &str) -> bool {
    let mut changed = false;
    for task in tasks {
        if task.status == "active" && task.run.as_deref() == Some(run) {
            task.status = "pending".into();
            task.run = None;
            task.done_at = None;
            task.stuck_since = None;
            changed = true;
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(id: &str, status: &str, run: Option<&str>) -> TeamTask {
        TeamTask {
            id: id.into(),
            title: id.into(),
            description: String::new(),
            status: status.into(),
            run: run.map(String::from),
            created_at: None,
            done_at: None,
            stuck_since: None,
        }
    }

    #[test]
    fn next_pending_is_oldest_pending() {
        let tasks = vec![
            t("a", "done", None),
            t("b", "pending", None),
            t("c", "pending", None),
        ];
        assert_eq!(next_pending(&tasks).unwrap().id, "b");
    }

    #[test]
    fn has_active_detects_in_flight() {
        assert!(has_active(&[t("a", "active", Some("run-1"))]));
        assert!(!has_active(&[
            t("a", "pending", None),
            t("b", "done", None)
        ]));
    }

    #[test]
    fn tasks_cm_name_is_stable() {
        assert_eq!(tasks_cm_name("finance"), "kars-team-tasks-finance");
    }

    #[test]
    fn successful_run_completes_backlog_task() {
        let mut tasks = vec![t("a", "active", Some("run-1"))];
        assert!(mark_done(&mut tasks, "run-1", "2026-07-20T12:00:00Z"));
        assert_eq!(tasks[0].status, "done");
        assert_eq!(tasks[0].done_at.as_deref(), Some("2026-07-20T12:00:00Z"));
        assert!(tasks[0].stuck_since.is_none());
    }

    #[test]
    fn failed_run_requeues_backlog_task() {
        let mut tasks = vec![t("a", "active", Some("run-1"))];
        tasks[0].stuck_since = Some("2026-07-20T11:00:00Z".into());
        assert!(requeue_run(&mut tasks, "run-1"));
        assert_eq!(tasks[0].status, "pending");
        assert!(tasks[0].run.is_none());
        assert!(tasks[0].done_at.is_none());
        assert!(tasks[0].stuck_since.is_none());
    }
}
