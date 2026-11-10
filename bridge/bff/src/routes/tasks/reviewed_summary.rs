// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use kube::ResourceExt;
use serde::Serialize;
use tokio::time::{Duration, Instant, timeout_at};

use super::reviewed_execution::{ReviewedPhaseDto, SummaryOutputs, load};
use crate::{
    auth::Principal,
    kars::{cluster::Cluster, task::KarsTask},
};

const PIN: &str = "kars.azure.com/reviewed-state";
const RUN: &str = "kars.azure.com/run-requested";
const MAX_READS: usize = 256;

#[derive(Debug, Serialize)]
pub struct ReviewedProgressDto {
    pub run_nonce: String,
    pub states: Vec<&'static str>,
    pub issue: Option<String>,
}

/// One budget for the entire list, not a detail-sized fetch for every row.
pub(super) struct SummaryBudget {
    remaining: usize,
    deadline: Instant,
}

impl SummaryBudget {
    pub(super) fn new() -> Self {
        Self {
            remaining: MAX_READS,
            deadline: Instant::now() + Duration::from_secs(5),
        }
    }

    fn reserve(&mut self, root: &KarsTask) -> bool {
        let Some(plan) = root
            .spec
            .blueprint
            .as_ref()
            .and_then(|b| b.execution_plan.as_ref())
        else {
            return false;
        };
        let roles = plan.roles.len();
        if roles > 64 {
            return false;
        }
        let phases = plan
            .roles
            .iter()
            .map(|role| role.phases.len())
            .sum::<usize>();
        if phases > 64 {
            return false;
        }
        // State + state recheck + root recheck; two reads per role; task,
        // attempt and task recheck per phase. Outputs reuse the list snapshot.
        let reads = 3 + 2 * roles + 3 * phases;
        if reads > self.remaining || Instant::now() >= self.deadline {
            return false;
        }
        self.remaining -= reads;
        true
    }
}

/// Terminal fleet rows need attempt states, not artifact bodies or live pod reads.
pub(super) async fn read_attempt_states(
    cluster: &Cluster,
    root: &KarsTask,
    principal: &Principal,
    budget: &mut SummaryBudget,
) -> Option<Vec<&'static str>> {
    if !budget.reserve(root) {
        return None;
    }
    match timeout_at(
        budget.deadline,
        super::reviewed_execution::read_live(cluster, root, principal),
    )
    .await
    {
        Ok(Ok(phases)) => Some(phases.iter().map(|phase| phase.state).collect()),
        _ => None,
    }
}

pub(super) fn unavailable(root: &KarsTask) -> Option<ReviewedProgressDto> {
    // Inherited phase pins are not root execution plans.
    if root.spec.parent_ref.is_some() || !root.annotations().contains_key(PIN) {
        return None;
    }
    Some(ReviewedProgressDto {
        run_nonce: root.annotations().get(RUN).cloned().unwrap_or_default(),
        states: Vec::new(),
        issue: Some(
            "Reviewed phase evidence is unavailable or changed; open the mission for details."
                .into(),
        ),
    })
}

pub(super) fn same_revision(before: &KarsTask, after: &KarsTask) -> bool {
    before.uid().is_some()
        && before.uid() == after.uid()
        && before.name_any() == after.name_any()
        && before.namespace() == after.namespace()
        && after.metadata.deletion_timestamp.is_none()
        && before.annotations() == after.annotations()
        && serde_json::to_value(&before.spec).ok() == serde_json::to_value(&after.spec).ok()
}

fn compact_state(phase: &ReviewedPhaseDto) -> &'static str {
    let bound = phase.result.as_ref().is_some_and(|result| {
        result.run_evidence.as_ref().is_some_and(|evidence| {
            phase.task_uid.as_deref() == Some(evidence.task_uid.as_str())
                && phase.run_nonce.as_deref() == Some(evidence.run_nonce.as_str())
                && result.assignment_nonce == phase.run_nonce
        })
    });
    if (phase.result.is_some() && !bound)
        || (phase.state == "completed"
            && (!bound || !phase.result.as_ref().is_some_and(|r| r.reviewable)))
    {
        "unavailable"
    } else {
        phase.state
    }
}

pub(super) async fn read(
    cluster: &Cluster,
    root: &KarsTask,
    principal: &Principal,
    outputs: &SummaryOutputs<'_>,
    budget: &mut SummaryBudget,
) -> Option<ReviewedProgressDto> {
    let mut progress = unavailable(root)?;
    if !budget.reserve(root) {
        return Some(progress);
    }
    match timeout_at(
        budget.deadline,
        load(cluster, root, principal, Some(outputs)),
    )
    .await
    {
        Ok(Ok(phases)) => {
            progress.states = phases.iter().map(compact_state).collect();
            progress.issue = None;
        }
        Ok(Err(error)) => {
            tracing::warn!(task = %root.name_any(), %error, "Reviewed summary evidence unavailable")
        }
        Err(_) => tracing::warn!("Reviewed summary read budget expired"),
    }
    Some(progress)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn task() -> KarsTask {
        serde_json::from_value(json!({
            "apiVersion": "kars.azure.com/v1alpha1", "kind": "KarsTask",
            "metadata": {"name": "root", "namespace": "kars-system", "uid": "uid",
                "annotations": {"kars.azure.com/reviewed-state": "pin", "kars.azure.com/run-requested": "run"}},
            "spec": {"objective": "Write", "envelope": {"tier": 1, "authorityCeiling": 1},
                "blueprint": {"executionPlan": {"schema": "kars.execution-plan/v1", "maxParallel": 1,
                    "roles": [{"name": "writer", "objective": "Write", "dependsOn": [],
                        "phases": [{"name": "draft", "objective": "Draft", "capabilities": [],
                            "minToolCalls": 0, "maxToolCalls": 6, "freshContext": true}]}],
                    "synthesis": {"objective": "Collect", "capabilities": [], "maxToolCalls": 6},
                    "deliverables": [{"name": "checklist.md"}]}}}
        })).unwrap()
    }

    #[test]
    fn reviewed_summary_budget_is_shared_and_bounded() {
        let task = task();
        let mut budget = SummaryBudget::new();
        for _ in 0..32 {
            assert!(budget.reserve(&task));
        }
        assert!(!budget.reserve(&task));
        assert_eq!(budget.remaining, 0);
        let mut budget = SummaryBudget::new();
        budget.deadline = Instant::now();
        assert!(!budget.reserve(&task));
        let mut task = task.clone();
        let plan = task
            .spec
            .blueprint
            .as_mut()
            .unwrap()
            .execution_plan
            .as_mut()
            .unwrap();
        plan.roles[0].phases = vec![plan.roles[0].phases[0].clone(); 65];
        assert!(!SummaryBudget::new().reserve(&task));
    }

    #[test]
    fn reviewed_summary_revision_rejects_replacement_rerun_and_changed_plan() {
        let task = task();
        assert!(same_revision(&task, &task));
        let mut changed = task.clone();
        changed.metadata.uid = Some("replacement".into());
        assert!(!same_revision(&task, &changed));
        changed = task.clone();
        changed.annotations_mut().insert(RUN.into(), "rerun".into());
        assert!(!same_revision(&task, &changed));
        changed = task.clone();
        changed.spec.objective = "Changed".into();
        assert!(!same_revision(&task, &changed));
        changed = task.clone();
        changed.metadata.resource_version = Some("new status only".into());
        assert!(same_revision(&task, &changed));
        assert_eq!(unavailable(&task).unwrap().run_nonce, "run");
        changed.annotations_mut().remove(PIN);
        assert!(unavailable(&changed).is_none());
    }

    #[test]
    fn reviewed_summary_failed_assignment_requires_current_completed_nonce() {
        let mut task = task();
        task.status = Some(
            serde_json::from_value(json!({
                "assignment": {"taskId": "run", "state": "Failed", "completedAt": "now"}
            }))
            .unwrap(),
        );
        let summary = super::super::mapping::to_summary(&task);
        assert!(summary.failed);
        assert_eq!(summary.current_run_nonce.as_deref(), Some("run"));
        assert!(summary.reviewed_progress.unwrap().issue.is_some());
        task.status
            .as_mut()
            .unwrap()
            .assignment
            .as_mut()
            .unwrap()
            .completed_at = None;
        assert!(!super::super::mapping::to_summary(&task).failed);
        task.status
            .as_mut()
            .unwrap()
            .assignment
            .as_mut()
            .unwrap()
            .completed_at = Some("now".into());
        task.annotations_mut().insert(RUN.into(), "rerun".into());
        assert!(!super::super::mapping::to_summary(&task).failed);
    }

    #[test]
    fn reviewed_summary_completion_requires_bound_reviewable_result() {
        use super::super::run_evidence::{MissionRunEvidenceDto, mission_result};
        let mut result = mission_result(&Default::default());
        result.reviewable = true;
        result.assignment_nonce = Some("run".into());
        result.run_evidence = Some(MissionRunEvidenceDto {
            task_uid: "uid".into(),
            run_nonce: "run".into(),
            assignment_id: "assignment".into(),
            agent_name: "phase".into(),
            agent_did: "agent".into(),
            dispatcher_did: "dispatcher".into(),
            sandbox_uid: "sandbox".into(),
            pod_uid: "pod".into(),
            runtime_boot_id: "boot".into(),
            runtime: None,
            status: "succeeded".into(),
            started_at: "start".into(),
            finished_at: "end".into(),
            rounds: Some(1),
        });
        let mut phase = ReviewedPhaseDto {
            role: "writer".into(),
            phase: "draft".into(),
            task_name: Some("phase".into()),
            task_uid: Some("uid".into()),
            run_nonce: Some("run".into()),
            state: "completed",
            result: Some(result),
            failure_diagnostic: None,
            router_activity: None,
            live_activity: None,
            stopped_activity: None,
        };
        assert_eq!(compact_state(&phase), "completed");
        phase.task_uid = Some("replacement".into());
        assert_eq!(compact_state(&phase), "unavailable");
        phase.task_uid = Some("uid".into());
        phase.run_nonce = Some("rerun".into());
        assert_eq!(compact_state(&phase), "unavailable");
        phase.run_nonce = Some("run".into());
        phase.result.as_mut().unwrap().assignment_nonce = Some("old".into());
        assert_eq!(compact_state(&phase), "unavailable");
        phase.state = "active";
        assert_eq!(compact_state(&phase), "unavailable");
        phase.state = "completed";
        phase.result.as_mut().unwrap().assignment_nonce = Some("run".into());
        phase.result.as_mut().unwrap().reviewable = false;
        assert_eq!(compact_state(&phase), "unavailable");
    }

    #[test]
    fn reviewed_summary_does_not_invent_completion_or_serialize_detail() {
        let mut phase = ReviewedPhaseDto {
            role: "writer".into(),
            phase: "draft".into(),
            task_name: Some("phase".into()),
            task_uid: Some("uid".into()),
            run_nonce: Some("run".into()),
            state: "completed",
            result: None,
            failure_diagnostic: None,
            router_activity: None,
            live_activity: None,
            stopped_activity: None,
        };
        assert_eq!(compact_state(&phase), "unavailable");
        for state in [
            "pending",
            "active",
            "failed",
            "uncertain",
            "unavailable",
            "stopped",
        ] {
            phase.state = state;
            assert_eq!(compact_state(&phase), state);
        }
        let progress = ReviewedProgressDto {
            run_nonce: "run".into(),
            states: vec![compact_state(&phase)],
            issue: None,
        };
        let value = serde_json::to_value(progress).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 3);
        assert!(value.get("result").is_none());
        assert!(value.get("router_activity").is_none());
    }
}
