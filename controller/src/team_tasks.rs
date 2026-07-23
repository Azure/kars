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
    Client, ResourceExt,
    api::{Api, PostParams},
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
    /// Stable task IDs that must be `done` before this milestone is eligible.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    /// Human/model-verifiable conditions that define completion.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub acceptance_criteria: Vec<String>,
    /// Require a human decision before this milestone unlocks dependents.
    #[serde(default)]
    pub review_required: bool,
    /// `pending` | `active` | `awaiting_review` | `done`.
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
    tasks.iter().find(|task| {
        task.status == "pending"
            && task.depends_on.iter().all(|dependency| {
                tasks
                    .iter()
                    .any(|candidate| candidate.id == *dependency && candidate.status == "done")
            })
    })
}

/// Whether the team already has a task in flight (its run hasn't delivered yet),
/// so we don't start a second task concurrently.
pub fn has_active(tasks: &[TeamTask]) -> bool {
    tasks.iter().any(|t| t.status == "active")
}

const MAX_TASK_UPDATE_RETRIES: usize = 8;

async fn persist_tasks(
    cms: &Api<ConfigMap>,
    team: &str,
    existing: Option<ConfigMap>,
    tasks: &[TeamTask],
) -> Result<(), kube::Error> {
    let name = tasks_cm_name(team);
    let mut data = BTreeMap::new();
    data.insert(
        "tasks.json".to_string(),
        serde_json::to_string(tasks).unwrap_or_else(|_| "[]".into()),
    );
    if let Some(mut cm) = existing {
        cm.data = Some(data);
        cm.metadata
            .labels
            .get_or_insert_with(BTreeMap::new)
            .insert("kars.azure.com/team-tasks".into(), team.into());
        cms.replace(&name, &PostParams::default(), &cm)
            .await
            .map(|_| ())
    } else {
        let cm: ConfigMap = serde_json::from_value(json!({
            "apiVersion": "v1",
            "kind": "ConfigMap",
            "metadata": {
                "name": name,
                "namespace": namespace(),
                "labels": { "kars.azure.com/team-tasks": team }
            },
            "data": data,
        }))
        .expect("team task ConfigMap is valid");
        cms.create(&PostParams::default(), &cm).await.map(|_| ())
    }
}

async fn mutate_tasks<R, F>(client: &Client, team: &str, mutator: F) -> Result<R, kube::Error>
where
    F: Fn(&mut Vec<TeamTask>) -> (R, bool),
{
    let cms: Api<ConfigMap> = Api::namespaced(client.clone(), &namespace());
    let name = tasks_cm_name(team);
    let mut last_conflict = None;
    for _ in 0..MAX_TASK_UPDATE_RETRIES {
        let existing = cms.get_opt(&name).await?;
        let mut tasks = existing
            .as_ref()
            .and_then(|cm| cm.data.as_ref())
            .and_then(|data| data.get("tasks.json"))
            .and_then(|raw| serde_json::from_str::<Vec<TeamTask>>(raw).ok())
            .unwrap_or_default();
        let (result, changed) = mutator(&mut tasks);
        if !changed {
            return Ok(result);
        }
        match persist_tasks(&cms, team, existing, &tasks).await {
            Ok(()) => return Ok(result),
            Err(kube::Error::Api(error)) if error.code == 409 => {
                last_conflict = Some(kube::Error::Api(error));
            }
            Err(error) => return Err(error),
        }
    }
    Err(last_conflict.expect("a retry loop exits only after conflicts"))
}

/// Mark a task `active` and bind it to the run that will work it. Returns the
/// updated list (already persisted).
pub async fn mark_active(
    client: &Client,
    team: &str,
    task_id: &str,
    run: &str,
) -> Result<bool, kube::Error> {
    let now = chrono::Utc::now().to_rfc3339();
    mutate_tasks(client, team, |tasks| {
        let changed = claim_task(tasks, task_id, run, &now);
        (changed, changed)
    })
    .await
}

fn claim_task(tasks: &mut [TeamTask], task_id: &str, run: &str, now: &str) -> bool {
    let Some(task) = tasks
        .iter_mut()
        .find(|task| task.id == task_id && task.status == "pending")
    else {
        return false;
    };
    task.status = "active".into();
    task.run = Some(run.to_string());
    task.stuck_since = Some(now.to_string());
    true
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
    let cms: Api<ConfigMap> = Api::namespaced(client.clone(), &namespace());
    let name = tasks_cm_name(team);
    let runs: Api<KarsTask> = Api::namespaced(client.clone(), &namespace());
    let mut last_conflict = None;
    for _ in 0..MAX_TASK_UPDATE_RETRIES {
        let existing = cms.get_opt(&name).await?;
        let mut tasks = existing
            .as_ref()
            .and_then(|cm| cm.data.as_ref())
            .and_then(|data| data.get("tasks.json"))
            .and_then(|raw| serde_json::from_str::<Vec<TeamTask>>(raw).ok())
            .unwrap_or_default();
        if !tasks.iter().any(|task| task.status == "active") {
            return Ok(false);
        }
        let now = chrono::Utc::now();
        let mut changed = false;
        for task in tasks.iter_mut() {
            if task.status != "active" {
                continue;
            }
            let (run_exists, run_halted) = match &task.run {
                Some(run_name) => match runs.get_opt(run_name).await? {
                    Some(run) => (
                        true,
                        run.annotations()
                            .get("kars.azure.com/halted")
                            .is_some_and(|decision| !decision.trim().is_empty()),
                    ),
                    None => (false, false),
                },
                None => (false, false),
            };
            let stuck_mins = task
                .stuck_since
                .as_deref()
                .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                .map(|started| (now - started.with_timezone(&chrono::Utc)).num_minutes())
                .unwrap_or(0);
            if should_requeue(run_exists, run_halted, stuck_mins) {
                task.status = "pending".into();
                task.run = None;
                task.stuck_since = None;
                changed = true;
            } else if task.stuck_since.is_none() {
                task.stuck_since = Some(now.to_rfc3339());
                changed = true;
            }
        }
        if !changed {
            return Ok(false);
        }
        match persist_tasks(&cms, team, existing, &tasks).await {
            Ok(()) => return Ok(true),
            Err(kube::Error::Api(error)) if error.code == 409 => {
                last_conflict = Some(kube::Error::Api(error));
            }
            Err(error) => return Err(error),
        }
    }
    Err(last_conflict.expect("a retry loop exits only after conflicts"))
}

fn should_requeue(run_exists: bool, run_halted: bool, stuck_mins: i64) -> bool {
    !run_exists || run_halted || stuck_mins > STUCK_TASK_TIMEOUT_MINS
}

/// Mark the `active` task bound to `run` as `done`. No-op if none matches.
/// Returns true when a task was transitioned (so the caller can log/act).
pub async fn mark_done_for_run(
    client: &Client,
    team: &str,
    run: &str,
    now: &str,
) -> Result<bool, kube::Error> {
    mutate_tasks(client, team, |tasks| {
        let changed = mark_done(tasks, run, now);
        (changed, changed)
    })
    .await
}

/// Requeue the `active` backlog task bound to a failed run. The failed run stays
/// linked in its own durable evidence, while the work item returns to `pending`
/// for an explicit Run now or the next cadence tick.
pub async fn requeue_for_run(client: &Client, team: &str, run: &str) -> Result<bool, kube::Error> {
    mutate_tasks(client, team, |tasks| {
        let changed = requeue_run(tasks, run);
        (changed, changed)
    })
    .await
}

pub async fn awaiting_review_for_run(client: &Client, team: &str, run: &str) -> Option<TeamTask> {
    read_tasks(client, team)
        .await
        .into_iter()
        .find(|task| task.status == "awaiting_review" && task.run.as_deref() == Some(run))
}

pub async fn resolve_review_for_run(
    client: &Client,
    team: &str,
    run: &str,
    approved: bool,
    feedback: Option<&str>,
) -> Result<bool, kube::Error> {
    let feedback = feedback.map(str::trim).filter(|value| !value.is_empty());
    mutate_tasks(client, team, |tasks| {
        let Some(task) = tasks
            .iter_mut()
            .find(|task| task.status == "awaiting_review" && task.run.as_deref() == Some(run))
        else {
            return (false, false);
        };
        if approved {
            task.status = "done".into();
        } else {
            if let Some(feedback) = feedback {
                task.description.push_str(&format!(
                    "\n\nREVIEW FEEDBACK (source run {run}):\n{feedback}"
                ));
            }
            task.status = "pending".into();
            task.run = None;
            task.done_at = None;
            task.stuck_since = None;
        }
        (true, true)
    })
    .await
}

fn mark_done(tasks: &mut [TeamTask], run: &str, now: &str) -> bool {
    let mut changed = false;
    for task in tasks {
        if task.status == "active" && task.run.as_deref() == Some(run) {
            task.status = if task.review_required {
                "awaiting_review".into()
            } else {
                "done".into()
            };
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
            depends_on: Vec::new(),
            acceptance_criteria: Vec::new(),
            review_required: false,
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
    fn next_pending_waits_for_milestone_dependencies() {
        let first = t("scaffold", "pending", None);
        let mut second = t("acceptance", "pending", None);
        second.depends_on = vec!["scaffold".into()];
        let tasks = vec![first, second];
        assert_eq!(next_pending(&tasks).unwrap().id, "scaffold");

        let mut done_first = tasks;
        done_first[0].status = "done".into();
        assert_eq!(next_pending(&done_first).unwrap().id, "acceptance");
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
    fn claim_only_transitions_the_expected_pending_task() {
        let mut tasks = vec![
            t("pending", "pending", None),
            t("active", "active", Some("run-old")),
        ];
        assert!(claim_task(
            &mut tasks,
            "pending",
            "run-new",
            "2026-07-20T12:00:00Z"
        ));
        assert_eq!(tasks[0].status, "active");
        assert_eq!(tasks[0].run.as_deref(), Some("run-new"));
        assert!(!claim_task(
            &mut tasks,
            "active",
            "run-rebind",
            "2026-07-20T12:01:00Z"
        ));
        assert_eq!(tasks[1].run.as_deref(), Some("run-old"));
        assert!(!claim_task(
            &mut tasks,
            "missing",
            "run-missing",
            "2026-07-20T12:01:00Z"
        ));
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
    fn review_required_milestone_waits_for_human_decision() {
        let mut milestone = t("release-review", "active", Some("run-1"));
        milestone.review_required = true;
        let mut tasks = vec![milestone];
        assert!(mark_done(&mut tasks, "run-1", "2026-07-20T12:00:00Z"));
        assert_eq!(tasks[0].status, "awaiting_review");
        assert_eq!(tasks[0].done_at.as_deref(), Some("2026-07-20T12:00:00Z"));
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

    #[test]
    fn halted_run_requeues_without_waiting_for_stale_timeout() {
        assert!(should_requeue(true, true, 0));
        assert!(!should_requeue(true, false, 0));
    }
}
