use kube::ResourceExt;
use serde::Serialize;

use crate::auth::Principal;
use crate::kars::task::KarsTask;
use crate::routes::ownership::task_is_owned_by;

use super::evidence::{canonicalize_assignment_event_roles, structured_team_evidence};
use super::{
    BudgetDto, CompositionDto, EnvelopeDto, MissionArtifactDto, MissionResultDto,
    MissionTelemetryDto, PullRequestRef, TaskAssignmentEventDto, TaskAssignmentStatusDto,
    TaskDetailDto, TaskSummaryDto, clean_display_name, clean_objective,
};

fn phase_of(task: &KarsTask) -> String {
    task.status
        .as_ref()
        .and_then(|s| s.phase.clone())
        .unwrap_or_else(|| "Pending".to_string())
}

pub(super) fn to_summary(task: &KarsTask) -> TaskSummaryDto {
    TaskSummaryDto {
        name: task.name_any(),
        namespace: task.namespace().unwrap_or_default(),
        objective: clean_objective(&task.spec.objective),
        display_name: clean_display_name(&task.spec.display_name, &task.spec.objective),
        created_at: task
            .metadata
            .creation_timestamp
            .as_ref()
            .map(|timestamp| timestamp.0.to_rfc3339()),
        tier: task.spec.envelope.tier,
        phase: phase_of(task),
        envelope_digest: task.status.as_ref().and_then(|s| s.envelope_digest.clone()),
        team: task
            .metadata
            .labels
            .as_ref()
            .and_then(|l| l.get("kars.azure.com/team").cloned()),
        delivered: false,
        failed: false,
        launched: task
            .spec
            .execution
            .as_ref()
            .map(|e| e.launch)
            .unwrap_or(false),
        execution_phase: task.status.as_ref().and_then(|s| s.execution_phase.clone()),
    }
}

pub(super) fn is_task_owner(task: &KarsTask, principal: &Principal) -> bool {
    task_is_owned_by(task, principal)
}

fn ready_message(task: &KarsTask) -> Option<String> {
    task.status
        .as_ref()?
        .conditions
        .iter()
        .find(|c| c.type_ == "Ready")
        .and_then(|c| c.message.clone())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn to_detail(
    task: &KarsTask,
    children: Vec<TaskSummaryDto>,
    sub_agents: Vec<SubAgentDto>,
    effective: Option<CompositionDto>,
    result: Option<MissionResultDto>,
    artifacts: Vec<MissionArtifactDto>,
    pull_requests: Vec<PullRequestRef>,
    activity: Vec<serde_json::Value>,
    telemetry: Option<MissionTelemetryDto>,
    checkpoint: Option<serde_json::Value>,
    agent_identity: Option<crate::kars::cluster::AgentIdentity>,
    egress_mode: Option<String>,
) -> TaskDetailDto {
    let e = &task.spec.envelope;
    let (role_plan, collaboration_events) = structured_team_evidence(&artifacts);
    let mut assignment_events = task
        .status
        .as_ref()
        .map(|s| {
            s.assignment_events
                .iter()
                .map(TaskAssignmentEventDto::from)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    canonicalize_assignment_event_roles(&mut assignment_events, &collaboration_events);
    TaskDetailDto {
        name: task.name_any(),
        namespace: task.namespace().unwrap_or_default(),
        objective: clean_objective(&task.spec.objective),
        display_name: clean_display_name(&task.spec.display_name, &task.spec.objective),
        created_at: task
            .metadata
            .creation_timestamp
            .as_ref()
            .map(|timestamp| timestamp.0.to_rfc3339()),
        envelope: EnvelopeDto {
            tier: e.tier,
            authority_ceiling: e.authority_ceiling,
            delegation_depth: e.delegation_depth,
            budget: e.budget.as_ref().map(|b| BudgetDto {
                scope: b.scope,
                tokens: b.tokens,
                usd_micros: b.usd_micros,
            }),
            tool_policy: e.tool_policy_ref.as_ref().map(|r| r.name.clone()),
            egress_allowlist: e.egress_allowlist_ref.as_ref().map(|r| r.name.clone()),
        },
        phase: phase_of(task),
        envelope_digest: task.status.as_ref().and_then(|s| s.envelope_digest.clone()),
        observed_generation: task.status.as_ref().and_then(|s| s.observed_generation),
        lineage: task
            .status
            .as_ref()
            .map(|s| s.lineage.clone())
            .unwrap_or_default(),
        parent: task.spec.parent_ref.as_ref().map(|r| r.name.clone()),
        team: task
            .labels()
            .get("kars.azure.com/team")
            .cloned()
            .or_else(|| task.annotations().get("kars.azure.com/team").cloned()),
        status_message: ready_message(task),
        children,
        launched: task
            .spec
            .execution
            .as_ref()
            .map(|e| e.launch)
            .unwrap_or(false),
        execution_phase: task.status.as_ref().and_then(|s| s.execution_phase.clone()),
        sandbox: task
            .status
            .as_ref()
            .and_then(|s| s.sandbox_ref.as_ref())
            .map(|r| r.name.clone()),
        execution_detail: task
            .status
            .as_ref()
            .and_then(|s| s.execution_detail.clone()),
        assignment: task
            .status
            .as_ref()
            .and_then(|s| s.assignment.as_ref())
            .map(TaskAssignmentStatusDto::from),
        assignment_events,
        assignment_sequence: task.status.as_ref().and_then(|s| s.assignment_sequence),
        egress_mode,
        composition: effective.or_else(|| {
            task.spec.blueprint.as_ref().map(|b| CompositionDto {
                runtime: b.runtime.clone(),
                model: b.model.as_ref().map(|m| m.deployment.clone()),
                instructions: b.instructions.clone(),
                tool_policy: b.tool_policy.clone(),
                mcp_servers: b.mcp_servers.clone(),
                egress: b
                    .egress
                    .iter()
                    .map(|e| match e.port {
                        Some(p) => format!("{}:{}", e.host, p),
                        None => e.host.clone(),
                    })
                    .collect(),
                isolation: b.isolation.clone(),
                memory: b.memory.clone(),
            })
        }),
        sub_agents,
        result,
        artifacts,
        role_plan,
        collaboration_events,
        pull_requests,
        activity,
        telemetry,
        checkpoint,
        agent_identity,
        harness_corrected: task
            .annotations()
            .get("kars.azure.com/harness-corrected")
            .cloned(),
        halted: task.annotations().get("kars.azure.com/halted").cloned(),
        run_requested: task
            .annotations()
            .get("kars.azure.com/run-requested")
            .is_some_and(|v| !v.trim().is_empty()),
        current_run_nonce: task
            .annotations()
            .get("kars.azure.com/run-requested")
            .filter(|value| !value.trim().is_empty())
            .cloned(),
    }
}

/// Build the effective composition from the materialized InferencePolicy +
/// KarsSandbox — the real running config, including controller-defaulted fields.
pub(super) fn composition_from_materialized(
    ip: Option<&kube::core::DynamicObject>,
    sandbox: Option<&kube::core::DynamicObject>,
) -> Option<CompositionDto> {
    let sb = sandbox?;
    let spec = sb.data.get("spec")?;
    let model = ip.and_then(|p| {
        let prim = p.data.get("spec")?.get("modelPreference")?.get("primary")?;
        let dep = prim.get("deployment")?.as_str()?;
        // The deployment string identifies the model; the inference provider is
        // a single cluster-level fact (see Options.provider), not a per-model
        // tag — so we do NOT append a guessed provider here.
        Some(dep.to_string())
    });
    let runtime = spec
        .get("runtime")
        .and_then(|r| r.get("kind"))
        .and_then(|k| k.as_str())
        .map(|s| s.to_string());
    let isolation = spec
        .get("sandbox")
        .and_then(|s| s.get("isolation"))
        .and_then(|i| i.as_str())
        .map(|s| s.to_string());
    let instructions = spec
        .get("agent")
        .and_then(|a| a.get("instructions"))
        .and_then(|i| i.as_str())
        .map(|s| s.to_string());
    let gov = spec.get("governance");
    let tool_policy = gov
        .and_then(|g| g.get("toolPolicyRef"))
        .and_then(|r| r.get("name"))
        .and_then(|n| n.as_str())
        .map(|s| s.to_string());
    let mcp_servers = gov
        .and_then(|g| g.get("mcpServerRefs"))
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| {
                    x.get("name")
                        .and_then(|n| n.as_str())
                        .map(|s| s.to_string())
                })
                .collect()
        })
        .unwrap_or_default();
    let egress = spec
        .get("networkPolicy")
        .and_then(|n| n.get("allowedEndpoints"))
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    let host = e.get("host")?.as_str()?;
                    Some(match e.get("port").and_then(|p| p.as_i64()) {
                        Some(p) => format!("{host}:{p}"),
                        None => host.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let memory = spec
        .get("memoryRef")
        .and_then(|m| m.get("name"))
        .and_then(|n| n.as_str())
        .map(|s| s.to_string());
    Some(CompositionDto {
        runtime,
        model,
        instructions,
        tool_policy,
        mcp_servers,
        egress,
        isolation,
        memory,
    })
}

/// A sub-agent the mission's agent spawned at run time (a labelled KarsSandbox).
#[derive(Debug, Serialize)]
pub struct SubAgentDto {
    pub name: String,
    pub namespace: String,
    pub phase: Option<String>,
    pub runtime: Option<String>,
    pub role: Option<String>,
    pub parent: Option<String>,
    pub logical_agent_id: Option<String>,
    pub model: Option<String>,
}

pub(super) fn to_sub_agent(o: &kube::core::DynamicObject) -> SubAgentDto {
    let spec = o.data.get("spec");
    let status = o.data.get("status");
    SubAgentDto {
        name: o.metadata.name.clone().unwrap_or_default(),
        namespace: o.metadata.namespace.clone().unwrap_or_default(),
        phase: status
            .and_then(|s| s.get("phase"))
            .and_then(|p| p.as_str())
            .map(|s| s.to_string()),
        runtime: spec
            .and_then(|s| s.get("runtime"))
            .and_then(|r| r.get("kind").or(Some(r)))
            .and_then(|k| k.as_str())
            .map(|s| s.to_string()),
        role: o.labels().get("kars.azure.com/role").cloned(),
        parent: o.labels().get("kars.azure.com/parent").cloned(),
        logical_agent_id: o
            .annotations()
            .get("kars.azure.com/logical-agent-id")
            .cloned(),
        model: o.annotations().get("kars.azure.com/model").cloned(),
    }
}
