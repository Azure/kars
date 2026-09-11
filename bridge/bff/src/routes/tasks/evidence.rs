use super::{
    MissionArtifactDto, MissionResultDto, TaskAssignmentEventDto, TeamCollaborationEventDto,
    TeamRolePlanDto,
};

pub(super) const ARTIFACT_PREVIEW_MAX_BYTES: usize = 8 * 1024;
pub(super) const ARTIFACT_PREVIEW_TOTAL_BYTES: usize = 64 * 1024;

pub(super) fn artifact_preview(
    content: Option<String>,
    remaining_budget: &mut usize,
) -> (Option<String>, Option<i64>, bool, Option<String>) {
    let Some(full) = content else {
        return (None, None, false, None);
    };
    let content_bytes = full.len() as i64;
    if full.is_empty() {
        return (Some(String::new()), Some(0), false, Some(full));
    }

    let max_bytes = ARTIFACT_PREVIEW_MAX_BYTES
        .min(*remaining_budget)
        .min(full.len());
    if max_bytes == 0 {
        return (None, Some(content_bytes), true, Some(full));
    }
    let mut end = max_bytes;
    while end > 0 && !full.is_char_boundary(end) {
        end -= 1;
    }
    let preview = full[..end].to_string();
    *remaining_budget = remaining_budget.saturating_sub(preview.len());
    let truncated = end < full.len();
    (Some(preview), Some(content_bytes), truncated, Some(full))
}

fn string_field(value: &serde_json::Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

fn bounded_text(value: Option<String>, max_bytes: usize) -> Option<String> {
    let value = value?;
    if value.len() <= max_bytes {
        return Some(value);
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    Some(value[..end].to_string())
}

fn bounded_string_field(
    value: &serde_json::Value,
    field: &str,
    max_bytes: usize,
) -> Option<String> {
    bounded_text(string_field(value, field), max_bytes)
}

fn collect_role_names(
    value: Option<&serde_json::Value>,
    target: &mut Vec<String>,
    seen: &mut std::collections::HashSet<String>,
) {
    const MAX_ROLE_NAMES: usize = 128;
    const MAX_ROLE_NAME_BYTES: usize = 256;
    if target.len() >= MAX_ROLE_NAMES {
        return;
    }
    match value {
        Some(serde_json::Value::Array(entries)) => {
            for entry in entries {
                let role = entry
                    .as_str()
                    .and_then(|role| bounded_text(Some(role.to_string()), MAX_ROLE_NAME_BYTES))
                    .or_else(|| bounded_string_field(entry, "role", MAX_ROLE_NAME_BYTES))
                    .or_else(|| bounded_string_field(entry, "name", MAX_ROLE_NAME_BYTES));
                if let Some(role) = role
                    && seen.insert(role.clone())
                {
                    target.push(role);
                    if target.len() >= MAX_ROLE_NAMES {
                        break;
                    }
                }
            }
        }
        Some(serde_json::Value::Object(entries)) => {
            for role in entries.keys() {
                let role = bounded_text(Some(role.clone()), MAX_ROLE_NAME_BYTES)
                    .expect("object keys are present");
                if seen.insert(role.clone()) {
                    target.push(role);
                    if target.len() >= MAX_ROLE_NAMES {
                        break;
                    }
                }
            }
        }
        _ => {}
    }
}

pub(super) fn structured_team_evidence(
    artifacts: &[MissionArtifactDto],
) -> (TeamRolePlanDto, Vec<TeamCollaborationEventDto>) {
    const MAX_COLLABORATION_EVENTS: usize = 1_000;
    const MAX_COLLABORATION_METADATA_BYTES: usize = 512;
    const MAX_COLLABORATION_PREVIEW_BYTES: usize = 2 * 1024;

    let mut role_plan = TeamRolePlanDto::default();
    let mut selected_seen = std::collections::HashSet::new();
    let mut skipped_seen = std::collections::HashSet::new();
    for artifact in artifacts
        .iter()
        .filter(|artifact| artifact.name.ends_with(".json"))
    {
        let Some(content) = artifact
            .full_content
            .as_deref()
            .or(artifact.content.as_deref())
        else {
            continue;
        };
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(content) else {
            continue;
        };
        collect_role_names(
            parsed.get("selected_roles"),
            &mut role_plan.selected_roles,
            &mut selected_seen,
        );
        collect_role_names(
            parsed.get("skipped_roles"),
            &mut role_plan.skipped_roles,
            &mut skipped_seen,
        );
    }

    let collaboration = artifacts
        .iter()
        .find(|artifact| {
            artifact.name == "collaboration.jsonl"
                || artifact
                    .source_path
                    .as_deref()
                    .is_some_and(|path| path.ends_with("/collaboration.jsonl"))
        })
        .and_then(|artifact| {
            artifact
                .full_content
                .as_deref()
                .or(artifact.content.as_deref())
        })
        .map(|content| {
            content
                .lines()
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .take(MAX_COLLABORATION_EVENTS)
                .map(|event| TeamCollaborationEventDto {
                    at: bounded_string_field(&event, "at", MAX_COLLABORATION_METADATA_BYTES),
                    event: bounded_string_field(&event, "event", MAX_COLLABORATION_METADATA_BYTES)
                        .unwrap_or_else(|| "event".to_string()),
                    agent: bounded_string_field(&event, "agent", MAX_COLLABORATION_METADATA_BYTES),
                    member: bounded_string_field(
                        &event,
                        "member",
                        MAX_COLLABORATION_METADATA_BYTES,
                    )
                    .or_else(|| {
                        bounded_string_field(&event, "from_agent", MAX_COLLABORATION_METADATA_BYTES)
                    })
                    .or_else(|| {
                        bounded_string_field(&event, "to_agent", MAX_COLLABORATION_METADATA_BYTES)
                    }),
                    outcome: bounded_string_field(
                        &event,
                        "outcome",
                        MAX_COLLABORATION_METADATA_BYTES,
                    ),
                    message_id: bounded_string_field(
                        &event,
                        "message_id",
                        MAX_COLLABORATION_METADATA_BYTES,
                    ),
                    reply_preview: bounded_text(
                        string_field(&event, "reply_preview"),
                        MAX_COLLABORATION_PREVIEW_BYTES,
                    ),
                    content_preview: bounded_text(
                        string_field(&event, "content_preview"),
                        MAX_COLLABORATION_PREVIEW_BYTES,
                    ),
                })
                .collect()
        })
        .unwrap_or_default();

    (role_plan, collaboration)
}

pub(super) fn canonicalize_assignment_event_roles(
    events: &mut [TaskAssignmentEventDto],
    collaboration: &[TeamCollaborationEventDto],
) {
    let roles_by_child_task = collaboration
        .iter()
        .filter_map(|event| {
            Some((
                event.message_id.as_deref()?.to_string(),
                event.member.as_deref()?.to_string(),
            ))
        })
        .collect::<std::collections::HashMap<_, _>>();

    for event in events {
        let Some(child_task_id) = event.child_task_id.as_deref() else {
            continue;
        };
        if let Some(role) = roles_by_child_task.get(child_task_id) {
            event.child_role = Some(role.clone());
        }
    }
}

pub(super) fn select_task_checkpoint(
    progress: Option<serde_json::Value>,
    artifacts: &[MissionArtifactDto],
    successful_result: bool,
) -> Option<serde_json::Value> {
    let artifact_checkpoint = artifacts
        .iter()
        .find(|artifact| artifact.name.ends_with("task-checkpoint.json"))
        .and_then(|artifact| {
            artifact
                .full_content
                .as_deref()
                .or(artifact.content.as_deref())
        })
        .and_then(|content| serde_json::from_str(content).ok())
        .and_then(valid_task_checkpoint);
    let checkpoint = artifact_checkpoint.or_else(|| progress.and_then(valid_task_checkpoint));

    checkpoint.filter(|checkpoint| {
        !successful_result
            || !matches!(
                checkpoint.get("status").and_then(serde_json::Value::as_str),
                Some("pending" | "in_progress")
            )
    })
}

pub(super) fn merge_trace_total_tokens(
    result: &mut Option<MissionResultDto>,
    trace_total_tokens: i64,
) {
    if trace_total_tokens <= 0 {
        return;
    }
    if let Some(result) = result {
        result.total_tokens = Some(
            result
                .total_tokens
                .unwrap_or_default()
                .max(trace_total_tokens),
        );
    }
}

pub(super) fn subagent_trace_from_artifacts(
    artifacts: &[MissionArtifactDto],
) -> Vec<serde_json::Value> {
    let mut events = Vec::new();
    for artifact in artifacts {
        if !artifact.name.ends_with("subagent-telemetry.jsonl") {
            continue;
        }
        let Some(content) = artifact
            .full_content
            .as_deref()
            .or(artifact.content.as_deref())
        else {
            continue;
        };
        for line in content
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
        {
            let Ok(record) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if record.get("event").and_then(serde_json::Value::as_str) != Some("subagent_trace") {
                continue;
            }
            let Some(mut trace) = record.get("trace").cloned() else {
                continue;
            };
            if let Some(object) = trace.as_object_mut() {
                let member = record
                    .get("member")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("subagent");
                object.insert("agent".into(), serde_json::json!(member));
                if let Some(mesh_name) = record.get("mesh_name").and_then(serde_json::Value::as_str)
                {
                    object.insert("agentInstance".into(), serde_json::json!(mesh_name));
                }
                object.insert("agentRole".into(), serde_json::json!("subagent"));
                if object.get("ts").is_none()
                    && let Some(at) = record.get("at").cloned()
                {
                    object.insert("ts".into(), at);
                }
            }
            events.push(trace);
        }
    }
    events
}

pub(super) fn valid_task_checkpoint(value: serde_json::Value) -> Option<serde_json::Value> {
    let schema = value.get("schema")?.as_str()?;
    let milestone = value.get("milestone_id")?.as_str()?.trim();
    let status = value.get("status")?.as_str()?;
    let summary = value.get("summary")?.as_str()?.trim();
    let string_array = |key: &str| {
        value.get(key).is_none_or(|field| {
            field
                .as_array()
                .is_some_and(|items| items.iter().all(serde_json::Value::is_string))
        })
    };
    (schema == "kars.checkpoint/v1"
        && !milestone.is_empty()
        && !summary.is_empty()
        && matches!(status, "pending" | "in_progress" | "completed" | "blocked")
        && string_array("acceptance_criteria")
        && string_array("artifacts")
        && string_array("next_steps"))
    .then_some(value)
}
