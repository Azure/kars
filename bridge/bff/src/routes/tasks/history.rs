use axum::Json;

use crate::auth::Principal;
use crate::error::{AppError, AppResult};

use super::artifacts::build_artifact_set;
use super::evidence::{
    canonicalize_assignment_event_roles, merge_trace_total_tokens, select_task_checkpoint,
    structured_team_evidence, subagent_trace_from_artifacts,
};
use super::mapping::is_task_owner;
use super::presentation::deliverable_pull_requests;
use super::{
    EnvelopeDto, MissionResultDto, MissionTelemetryDto, TaskAssignmentEventDto, TaskDetailDto,
    classify_blocked, deliverable_text, map_kube_err,
};

/// Build a read-only mission detail purely from persisted ConfigMaps when the
/// KarsTask CR is gone (retired-run GC). Returns `NotFound` only when there is
/// genuinely no persisted output for the name. The envelope/composition are
/// left empty (the CR that carried them is gone) but the deliverable, artifacts,
/// activity trace, and telemetry — the parts a reviewer actually needs after the
/// fact — are surfaced, along with a terminal phase.
pub(super) async fn synth_detail_from_output(
    cluster: &crate::kars::cluster::Cluster,
    ns: &str,
    name: &str,
    principal: &Principal,
) -> AppResult<Json<TaskDetailDto>> {
    let output_data = match cluster.read_mission_output(name).await {
        Some(d) => d,
        None => return Err(AppError::NotFound),
    };
    if output_data.get("ownerSub").map(String::as_str) != Some(principal.sub.as_str()) {
        return Err(AppError::NotFound);
    }
    let assignment_nonce = output_data.get("assignmentNonce").cloned();
    let historical_task = match output_data.get("taskName") {
        Some(task_name) => cluster
            .tasks(ns)
            .get_opt(task_name)
            .await
            .map_err(map_kube_err)?
            .filter(|task| is_task_owner(task, principal)),
        None => None,
    };
    let mut assignment_events = historical_task
        .as_ref()
        .and_then(|task| task.status.as_ref())
        .map(|status| {
            status
                .assignment_events
                .iter()
                .filter(|event| {
                    assignment_nonce
                        .as_deref()
                        .is_none_or(|nonce| event.task_id == nonce)
                })
                .map(TaskAssignmentEventDto::from)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let mut activity: Vec<serde_json::Value> = cluster
        .read_mission_trace(name)
        .await
        .and_then(|raw| serde_json::from_str::<Vec<serde_json::Value>>(&raw).ok())
        .unwrap_or_default();
    let status = output_data.get("status").map(String::as_str);
    let mut result = {
        let output = deliverable_text(output_data.get("output").map(String::as_str).unwrap_or(""));
        let blocked = classify_blocked(output_data.get("status").map(String::as_str), &output);
        Some(MissionResultDto {
            output,
            status: output_data.get("status").cloned(),
            model: output_data.get("model").cloned(),
            total_tokens: output_data.get("totalTokens").and_then(|v| v.parse().ok()),
            prompt_tokens: output_data.get("promptTokens").and_then(|v| v.parse().ok()),
            completion_tokens: output_data
                .get("completionTokens")
                .and_then(|v| v.parse().ok()),
            finished_at: output_data.get("finishedAt").cloned(),
            assignment_nonce: output_data.get("assignmentNonce").cloned(),
            source: output_data.get("source").cloned(),
            blocked,
            artifact_persistence: output_data.get("artifactPersistence").cloned(),
            artifact_count: output_data
                .get("artifactCount")
                .and_then(|v| v.parse().ok()),
            declared_artifact_count: output_data
                .get("declaredArtifactCount")
                .and_then(|v| v.parse().ok()),
        })
    };
    let artifacts = build_artifact_set(cluster, name, Some(&output_data)).await;
    let successful_result = result.as_ref().is_some_and(|result| {
        result.status.as_deref() != Some("error") && result.blocked.is_none()
    });
    let checkpoint = select_task_checkpoint(None, &artifacts, successful_result);
    activity.extend(subagent_trace_from_artifacts(&artifacts));
    activity.sort_by(|left, right| {
        left.get("ts")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .cmp(
                right
                    .get("ts")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(""),
            )
    });
    let trace_round_events = activity
        .iter()
        .filter(|event| event.get("kind").and_then(serde_json::Value::as_str) == Some("round"))
        .count() as i64;
    let trace_tool_events = activity
        .iter()
        .filter(|event| event.get("kind").and_then(serde_json::Value::as_str) == Some("tool"))
        .count() as i64;
    let trace_total_tokens: i64 = activity
        .iter()
        .filter(|event| event.get("kind").and_then(serde_json::Value::as_str) == Some("round"))
        .filter_map(|event| {
            event
                .get("total_tokens")
                .and_then(serde_json::Value::as_i64)
        })
        .sum();
    merge_trace_total_tokens(&mut result, trace_total_tokens);

    let telemetry = {
        let mut rounds = output_data
            .get("rounds")
            .and_then(|v| v.parse::<i64>().ok());
        let mut tool_calls = output_data
            .get("toolCalls")
            .and_then(|v| v.parse::<i64>().ok());
        if trace_round_events > 0 {
            rounds = Some(rounds.unwrap_or_default().max(trace_round_events));
        }
        if trace_tool_events > 0 {
            tool_calls = Some(tool_calls.unwrap_or_default().max(trace_tool_events));
        }
        if rounds.is_some() || tool_calls.is_some() {
            Some(MissionTelemetryDto { rounds, tool_calls })
        } else {
            None
        }
    };

    let phase = if status == Some("error") {
        "Failed"
    } else {
        "Delivered"
    };
    let (role_plan, collaboration_events) = structured_team_evidence(&artifacts);
    canonicalize_assignment_event_roles(&mut assignment_events, &collaboration_events);

    Ok(Json(TaskDetailDto {
        name: name.to_string(),
        namespace: ns.to_string(),
        objective: output_data.get("objective").cloned().unwrap_or_default(),
        display_name: output_data
            .get("displayName")
            .cloned()
            .filter(|s| !s.trim().is_empty()),
        created_at: output_data.get("startedAt").cloned(),
        envelope: EnvelopeDto {
            tier: output_data.get("tier").and_then(|v| v.parse().ok()).unwrap_or(0),
            authority_ceiling: 0,
            delegation_depth: 0,
            budget: None,
            tool_policy: None,
            egress_allowlist: None,
        },
        phase: phase.to_string(),
        envelope_digest: None,
        observed_generation: None,
        lineage: Vec::new(),
        parent: None,
        team: output_data.get("team").cloned(),
        status_message: Some(
            "This run's governance record was retired (garbage-collected); the deliverable and audit trail below are read from the persisted mission output.".to_string(),
        ),
        children: Vec::new(),
        launched: true,
        execution_phase: Some("Idle".to_string()),
        sandbox: None,
        egress_mode: None,
        execution_detail: None,
        assignment: None,
        assignment_events,
        assignment_sequence: None,
        composition: None,
        sub_agents: Vec::new(),
        result,
        artifacts,
        role_plan,
        collaboration_events,
        pull_requests: deliverable_pull_requests(&output_data),
        activity,
        telemetry,
        checkpoint,
        agent_identity: None,
        harness_corrected: None,
        halted: None,
        // This view is reconstructed from a delivered/terminal output, so a run
        // was necessarily requested — never auto-kickoff it again.
        run_requested: true,
        current_run_nonce: assignment_nonce,
    }))
}
