// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use std::collections::{BTreeMap, BTreeSet};

use kube::ResourceExt;
use serde_json::Value;

use super::super::{
    reviewed_execution, reviewed_summary,
    revision::{ReadRevision, output_matches_task},
    run_evidence::live,
};
use crate::{
    auth::Principal,
    error::{AppError, AppResult},
    kars::{cluster::Cluster, task::KarsTask},
};

pub(super) struct Worker {
    pub name: String,
    pub role: Option<String>,
    pub events: Option<Vec<Value>>,
}

pub(super) struct Snapshot {
    pub output: Option<BTreeMap<String, String>>,
    pub workers: Vec<Worker>,
    pub phase: String,
}

/// Reuse the Mission stream's retained assignment and reviewed-tree custody checks.
/// No ambient pod-proxy trace or service credential is an evidence fallback.
pub(super) async fn read(
    cluster: &Cluster,
    task: &KarsTask,
    principal: &Principal,
    terminal_budget: Option<&mut reviewed_summary::SummaryBudget>,
) -> AppResult<Snapshot> {
    let name = task.name_any();
    let revision = ReadRevision::capture(task, "kars-system", &name, principal)?;
    let output = cluster
        .read_mission_output(&name)
        .await
        .filter(|output| output_matches_task(task, output));
    let mut workers = Vec::new();
    let launched = task
        .spec
        .execution
        .as_ref()
        .is_some_and(|execution| execution.launch);
    let requested = task.annotations().get("kars.azure.com/run-requested");
    let completed = task.annotations().get("kars.azure.com/run-completed");
    if output.is_none() && launched && requested.is_some() && requested != completed {
        let phases = reviewed_execution::read_live(cluster, task, principal)
            .await
            .map_err(|_| {
                AppError::Conflict("Live reviewed activity is unavailable; refresh the run.".into())
            })?;
        let mut names = BTreeSet::new();
        for phase in phases {
            if phase.state != "active" {
                continue;
            }
            let Some(name) = phase.task_name else {
                continue;
            };
            if !names.insert(name.clone())
                || cluster
                    .running_pod_for_sandbox_checked(&name)
                    .await?
                    .is_none()
            {
                continue;
            }
            workers.push(Worker {
                name,
                role: Some(format!("{} / {}", phase.role, phase.phase)),
                events: phase
                    .live_activity
                    .as_ref()
                    .and_then(|activity| activity.observations().observed_events()),
            });
        }
        if cluster
            .running_pod_for_sandbox_checked(&name)
            .await?
            .is_some()
        {
            let attempt = match task
                .uid()
                .zip(task.annotations().get("kars.azure.com/run-requested"))
            {
                Some((uid, nonce)) => cluster
                    .read_mission_attempt(&uid, nonce)
                    .await
                    .ok()
                    .flatten(),
                None => None,
            };
            let fresh = revision.recheck(cluster, principal).await?;
            workers.push(Worker {
                name: name.clone(),
                role: None,
                events: attempt
                    .as_ref()
                    .and_then(|attempt| live::from_attempt(attempt, &fresh))
                    .and_then(|activity| activity.observations().observed_events()),
            });
        }
    }
    let mut reviewed_direct = false;
    let terminal_phase = if output.is_none()
        && !launched
        && !live::is_stopped(task)
        && task
            .annotations()
            .contains_key("kars.azure.com/reviewed-state")
    {
        if let Some(budget) = terminal_budget {
            let states =
                reviewed_summary::read_attempt_states(cluster, task, principal, budget).await;
            reviewed_direct = states.as_ref().is_some_and(Vec::is_empty);
            Some(inactive_reviewed_phase(states.as_deref()))
        } else {
            None
        }
    } else {
        None
    };
    let fresh = revision.recheck(cluster, principal).await?;
    if fresh.annotations().get("kars.azure.com/reviewed-state")
        != task.annotations().get("kars.azure.com/reviewed-state")
        || fresh
            .spec
            .execution
            .as_ref()
            .is_some_and(|execution| execution.launch)
            != launched
    {
        return Err(AppError::Conflict(
            "Run authority changed while reading activity; refresh the run.".into(),
        ));
    }
    let phase = if !workers.is_empty() {
        "Running".into()
    } else if output
        .as_ref()
        .is_some_and(|output| output.get("status").is_some_and(|status| status == "ok"))
    {
        "Delivered".into()
    } else if output.is_some() {
        "Errored".into()
    } else if live::is_stopped(&fresh)
        || (ordinary_stopped_direct_mission(&fresh)
            && (!fresh
                .annotations()
                .contains_key("kars.azure.com/reviewed-state")
                || reviewed_direct))
    {
        "Stopped".into()
    } else if let Some(phase) = terminal_phase {
        phase.into()
    } else {
        fresh
            .status
            .as_ref()
            .and_then(|status| status.execution_phase.clone())
            .unwrap_or_else(|| {
                if launched {
                    "Starting".into()
                } else {
                    "Idle".into()
                }
            })
    };
    Ok(Snapshot {
        output,
        workers,
        phase,
    })
}

fn ordinary_stopped_direct_mission(task: &KarsTask) -> bool {
    let annotations = task.annotations();
    let requested = annotations.get("kars.azure.com/run-requested");
    // A retained nonce alone admits Activity reads, not a finished Stop outcome.
    live::can_read_stopped_activity(task)
        && requested == annotations.get("kars.azure.com/run-ack")
        && requested != annotations.get("kars.azure.com/run-completed")
        && task.spec.parent_ref.is_none()
        && !task.labels().contains_key("kars.azure.com/team")
        && ![
            "kars.azure.com/team-role",
            "kars.azure.com/reviewed-root",
            "kars.azure.com/reviewed-slot",
        ]
        .iter()
        .any(|key| annotations.contains_key(*key))
        && task
            .spec
            .blueprint
            .as_ref()
            .and_then(|blueprint| blueprint.execution_plan.as_ref())
            .is_none_or(|plan| plan.roles.is_empty())
        && task.status.as_ref().is_some_and(|status| {
            task.metadata.generation.is_some()
                && status.observed_generation == task.metadata.generation
                && status.execution_phase.as_deref() == Some("Idle")
                && status.sandbox_ref.is_none()
        })
}

fn inactive_reviewed_phase(states: Option<&[&str]>) -> &'static str {
    let Some(states) = states.filter(|states| !states.is_empty()) else {
        return "Unavailable";
    };
    if states.contains(&"failed") {
        "Errored"
    } else if states.contains(&"uncertain") {
        "Held"
    } else if states
        .iter()
        .any(|state| !matches!(*state, "active" | "pending" | "stopped" | "completed"))
    {
        "Unavailable"
    } else if states.iter().all(|state| *state == "completed") {
        // Specialist completion alone does not establish principal delivery.
        "Incomplete"
    } else {
        "Held"
    }
}

#[derive(Default)]
pub(super) struct Counts {
    pub rounds: usize,
    pub tools: usize,
    pub tokens: Option<i64>,
}

pub(super) fn counts(events: &[Value]) -> Counts {
    let mut result = Counts {
        tokens: Some(0),
        ..Default::default()
    };
    let mut calls = BTreeSet::new();
    let mut rounds = BTreeSet::new();
    for event in events {
        match event.get("kind").and_then(Value::as_str) {
            Some("round") if rounds.insert(event.get("round").and_then(Value::as_u64)) => {
                result.rounds += 1;
                result.tokens = result
                    .tokens
                    .zip(event.pointer("/usage/total_tokens").and_then(Value::as_i64))
                    .and_then(|(total, tokens)| total.checked_add(tokens));
            }
            Some("tool_result")
                if calls.insert((
                    event.get("round").and_then(Value::as_u64),
                    event.get("call_id").and_then(Value::as_str),
                )) =>
            {
                result.tools += 1
            }
            _ => {}
        }
    }
    // Native tool-only selection does not establish model usage.
    if result.rounds == 0 {
        result.tokens = None;
    }
    result
}

#[cfg(test)]
#[path = "fleet_observations_tests.rs"]
mod integration_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn inactive_reviewed_work_never_uses_idle_as_an_outcome() {
        for (states, expected) in [
            (None, "Unavailable"),
            (Some(vec![]), "Unavailable"),
            (Some(vec!["completed", "failed", "pending"]), "Errored"),
            (Some(vec!["unavailable", "failed"]), "Errored"),
            (Some(vec!["uncertain", "pending"]), "Held"),
            (Some(vec!["active"]), "Held"),
            (Some(vec!["pending"]), "Held"),
            (Some(vec!["stopped"]), "Held"),
            (Some(vec!["completed"]), "Incomplete"),
            (Some(vec!["unavailable"]), "Unavailable"),
            (Some(vec!["unknown"]), "Unavailable"),
        ] {
            assert_eq!(
                inactive_reviewed_phase(states.as_deref()),
                expected,
                "{states:?}"
            );
        }
    }

    #[test]
    fn counts_only_observed_results_not_proposals_or_policy_decisions() {
        let events = vec![
            json!({"kind":"tool_proposed", "call_id":"a"}),
            json!({"kind":"router_operation", "call_id":"a"}),
            json!({"kind":"tool_result", "call_id":"a", "ok":false}),
            json!({"kind":"tool_result", "call_id":"a", "ok":false}),
            json!({"kind":"round", "round":1,"usage":{"total_tokens":42}}),
        ];
        let result = counts(&events);
        assert_eq!(
            (result.rounds, result.tools, result.tokens),
            (1, 1, Some(42))
        );
    }

    #[test]
    fn absent_usage_is_not_zero_and_overflow_is_unavailable() {
        for events in [
            vec![],
            vec![json!({"kind":"tool_result","call_id":"a"})],
            vec![json!({"kind":"round","round":1,"usage":null})],
            vec![
                json!({"kind":"round","round":1,"usage":{"total_tokens":i64::MAX}}),
                json!({"kind":"round","round":2,"usage":{"total_tokens":1}}),
            ],
        ] {
            assert_eq!(counts(&events).tokens, None);
        }
    }
}
