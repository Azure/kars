// kars Bridge BFF — task API DTOs + handlers.
//
// These endpoints are the browser's only way to touch KarsTask resources.
// They map the typed CRD (kars::task) to stable, browser-facing JSON shapes,
// so the UI never depends on raw Kubernetes object envelopes.

use axum::Json;
use axum::extract::{Extension, Path, State};
use serde::{Deserialize, Serialize};

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::kars::task::{KarsTask, KarsTaskSpec, LocalObjectRef, TaskBudget, TaskEnvelope};
use crate::routes::ownership::{
    require_owned_task, require_owned_task_or_output, task_is_owned_by,
};
use crate::state::AppState;
use kube::ResourceExt;
use kube::api::{Api, ListParams, PostParams};

/// Map a `kube::Error` to the right client-facing error. An API rejection with
/// a 4xx status (admission/CEL/validation) is the user's invalid input — a 422
/// carrying the API server's own message — not a gateway failure.
fn map_kube_err(e: kube::Error) -> AppError {
    if let kube::Error::Api(resp) = &e
        && (400..500).contains(&resp.code)
    {
        return AppError::Rejected(resp.message.clone());
    }
    AppError::Upstream(e.to_string())
}

/// Browser-facing budget shape.
#[derive(Debug, Serialize, Deserialize)]
pub struct BudgetDto {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<crate::kars::task::BudgetScope>,
    pub tokens: Option<i64>,
    pub usd_micros: Option<i64>,
}

/// Browser-facing envelope shape.
#[derive(Debug, Serialize, Deserialize)]
pub struct EnvelopeDto {
    pub tier: i32,
    pub authority_ceiling: i32,
    pub delegation_depth: i32,
    pub budget: Option<BudgetDto>,
    pub tool_policy: Option<String>,
    pub egress_allowlist: Option<String>,
}

/// Browser-facing blueprint shape. The request layer is **snake_case** (like
/// every other DTO here and the web's TS types); it maps to the camelCase CRD
/// `TaskBlueprint` on write. Keeping the wire contract consistent here is what
/// prevents silent field-drop on multi-word fields (`tool_policy`,
/// `mcp_servers`).
#[derive(Debug, Deserialize, Default)]
pub struct BlueprintDto {
    #[serde(default)]
    pub runtime: Option<String>,
    #[serde(default)]
    pub model: Option<ModelDto>,
    #[serde(default)]
    pub model_fallbacks: Vec<ModelDto>,
    #[serde(default)]
    pub instructions: Option<String>,
    #[serde(default)]
    pub tool_policy: Option<String>,
    #[serde(default)]
    pub mcp_servers: Vec<String>,
    #[serde(default)]
    pub egress: Vec<EgressDto>,
    #[serde(default)]
    pub egress_mode: Option<String>,
    #[serde(default)]
    pub isolation: Option<String>,
    #[serde(default)]
    pub memory: Option<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub execution_plan: Option<ExecutionPlanDto>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ExecutionPlanDto {
    pub schema: String,
    pub roles: Vec<ExecutionRoleDto>,
    pub max_parallel: i32,
    pub synthesis: ExecutionSynthesisDto,
    #[serde(default)]
    pub deliverables: Vec<ExecutionDeliverableDto>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ExecutionRoleDto {
    pub name: String,
    pub objective: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    pub phases: Vec<ExecutionPhaseDto>,
    #[serde(default)]
    pub budget_tokens: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ExecutionPhaseDto {
    pub name: String,
    pub objective: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub required_tool_calls: Vec<ExecutionRequiredToolCallDto>,
    #[serde(default)]
    pub min_tool_calls: i32,
    pub max_tool_calls: i32,
    #[serde(default)]
    pub fresh_context: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ExecutionRequiredToolCallDto {
    pub name: String,
    #[serde(default)]
    pub arguments: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ExecutionSynthesisDto {
    pub objective: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    pub max_tool_calls: i32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ExecutionDeliverableDto {
    pub name: String,
    #[serde(default)]
    pub media_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModelDto {
    pub provider: String,
    pub deployment: String,
}

#[derive(Debug, Deserialize)]
pub struct EgressDto {
    pub host: String,
    #[serde(default)]
    pub port: Option<i32>,
}

impl BlueprintDto {
    fn into_crd(self) -> crate::kars::task::TaskBlueprint {
        use crate::kars::task::{TaskBlueprint, TaskEgress, TaskModel};
        TaskBlueprint {
            runtime: self.runtime,
            model: self.model.map(|m| TaskModel {
                provider: m.provider,
                deployment: m.deployment,
            }),
            model_fallbacks: self
                .model_fallbacks
                .into_iter()
                .map(|m| TaskModel {
                    provider: m.provider,
                    deployment: m.deployment,
                })
                .collect(),
            instructions: self.instructions,
            tool_policy: self.tool_policy,
            mcp_servers: self.mcp_servers,
            egress: self
                .egress
                .into_iter()
                .map(|e| TaskEgress {
                    host: e.host,
                    port: e.port,
                })
                .collect(),
            egress_mode: self.egress_mode,
            isolation: self.isolation,
            memory: self.memory,
            skills: self.skills,
            git_write: None,
            credential_bindings: None,
            github_binding: None,
            execution_plan: self.execution_plan.map(ExecutionPlanDto::into_crd),
        }
    }
}

impl ExecutionPlanDto {
    pub(crate) fn from_crd(plan: &crate::kars::task::ExecutionPlan) -> Self {
        Self {
            schema: plan.schema.clone(),
            roles: plan
                .roles
                .iter()
                .map(|role| ExecutionRoleDto {
                    name: role.name.clone(),
                    objective: role.objective.clone(),
                    depends_on: role.depends_on.clone(),
                    phases: role
                        .phases
                        .iter()
                        .map(|phase| ExecutionPhaseDto {
                            name: phase.name.clone(),
                            objective: phase.objective.clone(),
                            capabilities: phase.capabilities.clone(),
                            required_tool_calls: phase
                                .required_tool_calls
                                .iter()
                                .map(|call| ExecutionRequiredToolCallDto {
                                    name: call.name.clone(),
                                    arguments: call.arguments.clone(),
                                })
                                .collect(),
                            min_tool_calls: phase.min_tool_calls,
                            max_tool_calls: phase.max_tool_calls,
                            fresh_context: phase.fresh_context,
                        })
                        .collect(),
                    budget_tokens: role.budget_tokens,
                })
                .collect(),
            max_parallel: plan.max_parallel,
            synthesis: ExecutionSynthesisDto {
                objective: plan.synthesis.objective.clone(),
                capabilities: plan.synthesis.capabilities.clone(),
                max_tool_calls: plan.synthesis.max_tool_calls,
            },
            deliverables: plan
                .deliverables
                .iter()
                .map(|deliverable| ExecutionDeliverableDto {
                    name: deliverable.name.clone(),
                    media_type: deliverable.media_type.clone(),
                })
                .collect(),
        }
    }

    pub(crate) fn into_crd(self) -> crate::kars::task::ExecutionPlan {
        use crate::kars::task::{
            ExecutionDeliverable, ExecutionPhase, ExecutionPlan, ExecutionRequiredToolCall,
            ExecutionRole, ExecutionSynthesis,
        };
        ExecutionPlan {
            schema: self.schema,
            roles: self
                .roles
                .into_iter()
                .map(|role| ExecutionRole {
                    name: role.name,
                    objective: role.objective,
                    depends_on: role.depends_on,
                    phases: role
                        .phases
                        .into_iter()
                        .map(|phase| ExecutionPhase {
                            name: phase.name,
                            objective: phase.objective,
                            capabilities: phase.capabilities,
                            required_tool_calls: phase
                                .required_tool_calls
                                .into_iter()
                                .map(|call| ExecutionRequiredToolCall {
                                    name: call.name,
                                    arguments: call.arguments,
                                })
                                .collect(),
                            min_tool_calls: phase.min_tool_calls,
                            max_tool_calls: phase.max_tool_calls,
                            fresh_context: phase.fresh_context,
                        })
                        .collect(),
                    budget_tokens: role.budget_tokens,
                })
                .collect(),
            max_parallel: self.max_parallel,
            synthesis: ExecutionSynthesis {
                objective: self.synthesis.objective,
                capabilities: self.synthesis.capabilities,
                max_tool_calls: self.synthesis.max_tool_calls,
            },
            deliverables: self
                .deliverables
                .into_iter()
                .map(|deliverable| ExecutionDeliverable {
                    name: deliverable.name,
                    media_type: deliverable.media_type,
                })
                .collect(),
        }
    }
}

/// Browser-facing task summary (list view).
#[derive(Debug, Serialize)]
pub struct TaskSummaryDto {
    pub name: String,
    pub namespace: String,
    pub objective: String,
    pub display_name: Option<String>,
    pub created_at: Option<String>,
    pub tier: i32,
    pub phase: String,
    pub envelope_digest: Option<String>,
    /// The standing team that owns this task (from the kars.azure.com/team
    /// label), when it is team machinery rather than a standalone mission. The
    /// Missions surface hides team-owned tasks — they belong to the Team view.
    pub team: Option<String>,
    /// Whether this mission has captured a delivered result (an `ok` run output
    /// exists). The authoritative "done" signal — execution phase returns to
    /// Idle after delivery, so phase alone cannot tell delivered from drafting.
    pub delivered: bool,
    /// Whether this mission's run captured an `error` output — a run that
    /// completed but did NOT succeed. Lets the list badge read "Run failed"
    /// instead of a misleading "Ready to launch" (audit f6).
    pub failed: bool,
    /// Whether the task has been launched (execution gate opened). Without this
    /// the list cannot tell a launched-and-running mission from an un-launched
    /// draft, so a live mission wrongly reads "Ready to launch".
    pub launched: bool,
    /// The controller's execution phase (Running/Idle/Degraded/…), so the list
    /// badge agrees with the detail page — "Running" while the agent works, not
    /// a stale "Ready to launch".
    pub execution_phase: Option<String>,
}

/// Browser-facing task detail (single view).
#[derive(Debug, Serialize)]
pub struct TaskDetailDto {
    pub name: String,
    pub namespace: String,
    pub objective: String,
    pub display_name: Option<String>,
    pub created_at: Option<String>,
    pub envelope: EnvelopeDto,
    pub phase: String,
    pub envelope_digest: Option<String>,
    pub observed_generation: Option<i64>,
    pub lineage: Vec<String>,
    /// Parent task name when this task is a delegated child.
    pub parent: Option<String>,
    /// The standing team that owns this run. Team-owned runs stay inside the
    /// team-native UX rather than leaking into the generic Missions surface.
    pub team: Option<String>,
    /// The `Ready` condition message — surfaces *why* a task is Degraded
    /// (e.g. an amplification rejection), so the UI can explain it.
    pub status_message: Option<String>,
    /// Names of tasks that delegate from this one (its direct children).
    pub children: Vec<TaskSummaryDto>,
    /// Whether the task is launched (execution gate).
    pub launched: bool,
    /// Execution phase: `Idle` | `Launching` | `Running` | `Degraded`.
    pub execution_phase: Option<String>,
    /// Name of the materialized sandbox, when launched.
    pub sandbox: Option<String>,
    /// The live egress enforcement mode the sandbox is running under, read from
    /// the materialized `KarsSandbox`: `"Learn"` (observe + record every domain
    /// the agent reaches, the default) or `"Strict"` (deny anything outside the
    /// allowlist). `None` until a sandbox exists. This is the monitoring→enforced
    /// surface: a customer watches in Learn, then promotes to Strict when
    /// confident the agent's reach is what it should be.
    pub egress_mode: Option<String>,
    /// Human-readable execution detail (e.g. the kind/Foundry caveat).
    pub execution_detail: Option<String>,
    /// Authoritative durable root-assignment snapshot from Kars core.
    pub assignment: Option<TaskAssignmentStatusDto>,
    /// Ordered durable root and child assignment transitions.
    pub assignment_events: Vec<TaskAssignmentEventDto>,
    /// Highest durable assignment event sequence observed by the controller.
    pub assignment_sequence: Option<i64>,
    /// The composed run — what model/harness/tools/services/egress/prompt this
    /// mission actually runs with, projected from the blueprint. Lets a
    /// task-giver review exactly what they launched. `None` when no blueprint
    /// was set (the mission uses controller defaults).
    pub composition: Option<CompositionDto>,
    /// The agents the mission spawned at run time (the running agent/sub-agent
    /// tree, distinct from the governed delegation roles in `children`).
    pub sub_agents: Vec<SubAgentDto>,
    /// The mission's captured run result — a real deliverable produced by a
    /// governed model run, with its real token cost. `None` until the mission
    /// has been run.
    pub result: Option<MissionResultDto>,
    /// The full set of artifact files the mission produced through the agent
    /// loop over the mesh (research report, data files, decision matrix, …),
    /// read from the persisted artifacts ConfigMap. Empty until a mesh run
    /// produces files.
    pub artifacts: Vec<MissionArtifactDto>,
    /// Bounded, server-parsed orchestration plan evidence. This remains complete
    /// even when the source artifact preview is truncated or omitted.
    pub role_plan: TeamRolePlanDto,
    /// Bounded, server-parsed collaboration evidence. Parsing full artifact
    /// contents in the BFF prevents preview limits from changing run truth.
    pub collaboration_events: Vec<TeamCollaborationEventDto>,
    /// Pull requests the mission opened, extracted from its output — surfaced as
    /// first-class deliverables (a PR is a delivery type) on the mission's
    /// Artifacts tab, not just buried in the prose. Empty when none were opened.
    #[serde(default)]
    pub pull_requests: Vec<PullRequestRef>,
    /// The mission's live execution activity — the real per-round and per-tool
    /// trace the agent emitted (token usage, tool names, sanitized arg/result
    /// previews, durations), read from the persisted trace ConfigMap. Empty
    /// until a mesh run produces a trace. This is the source of the Activity
    /// timeline and the clean per-tool audit path.
    pub activity: Vec<serde_json::Value>,
    /// Run telemetry rollup (rounds, tool calls) parsed from the mission output.
    /// Token totals live on `result`; this carries the loop-shape counts.
    pub telemetry: Option<MissionTelemetryDto>,
    /// Latest durable milestone checkpoint emitted by the running harness.
    pub checkpoint: Option<serde_json::Value>,
    /// The running agent's real mesh identity (DID), discovered from the AGT
    /// registry — proof the agent is a live, harness-neutral mesh participant.
    /// `None` when not launched / not yet registered / registry unreachable.
    pub agent_identity: Option<crate::kars::cluster::AgentIdentity>,
    /// A governed capability-routing decision recorded at creation: set when the
    /// requested harness could not run this mission (a chat-gateway harness on a
    /// one-shot mission) and was corrected. Surfaced so the swap is attested, not
    /// silent. `None` when no correction was needed.
    pub harness_corrected: Option<String>,
    /// A governed emergency-stop decision: set when an operator halted this
    /// mission (agent torn down, record retained). Carries the operator/reason/at
    /// string. `None` when the mission was never halted.
    pub halted: Option<String>,
    /// Whether a run has EVER been requested for this mission (the
    /// `kars.azure.com/run-requested` annotation is set). Used by the client
    /// auto-kickoff to fire the first run exactly once — gating on this instead
    /// of "no activity yet" avoids a race where the agent's startup telemetry
    /// (MCP init / tool list) makes the mission look already-active and the
    /// first run is never triggered, leaving it silently idle.
    pub run_requested: bool,
    /// The exact latest requested run nonce. This appears before assignment
    /// acknowledgement and is the authoritative scope for run-bound approvals.
    pub current_run_nonce: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TaskAssignmentStatusDto {
    pub task_id: String,
    pub state: String,
    pub worker_did: Option<String>,
    pub stage: Option<String>,
    pub child_task_id: Option<String>,
    pub child_role: Option<String>,
    pub last_progress_at: Option<String>,
    pub completed_at: Option<String>,
    pub error: Option<String>,
}

impl From<&crate::kars::task::TaskAssignmentStatus> for TaskAssignmentStatusDto {
    fn from(value: &crate::kars::task::TaskAssignmentStatus) -> Self {
        Self {
            task_id: value.task_id.clone(),
            state: value.state.clone(),
            worker_did: value.worker_did.clone(),
            stage: value.stage.clone(),
            child_task_id: value.child_task_id.clone(),
            child_role: value.child_role.clone(),
            last_progress_at: value.last_progress_at.clone(),
            completed_at: value.completed_at.clone(),
            error: value.error.clone(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct TaskAssignmentEventDto {
    pub sequence: i64,
    pub event_id: String,
    pub task_id: String,
    pub event_type: String,
    pub state: String,
    pub at: String,
    pub worker_did: Option<String>,
    pub stage: Option<String>,
    pub child_task_id: Option<String>,
    pub child_role: Option<String>,
    pub outcome: Option<String>,
    pub message: Option<String>,
}

impl From<&crate::kars::task::TaskAssignmentEvent> for TaskAssignmentEventDto {
    fn from(value: &crate::kars::task::TaskAssignmentEvent) -> Self {
        Self {
            sequence: value.sequence,
            event_id: value.event_id.clone(),
            task_id: value.task_id.clone(),
            event_type: value.event_type.clone(),
            state: value.state.clone(),
            at: value.at.clone(),
            worker_did: value.worker_did.clone(),
            stage: value.stage.clone(),
            child_task_id: value.child_task_id.clone(),
            child_role: value.child_role.clone(),
            outcome: value.outcome.clone(),
            message: value.message.clone(),
        }
    }
}

/// Loop-shape telemetry for a mission run (token totals are on the result DTO).
#[derive(Debug, Serialize)]
pub struct MissionTelemetryDto {
    pub rounds: Option<i64>,
    pub tool_calls: Option<i64>,
}

/// A captured mission run result (read from the persisted output ConfigMap).
#[derive(Debug, Serialize)]
pub struct MissionResultDto {
    pub output: String,
    /// Run status the output reflects: `ok` (a real deliverable) or `error`
    /// (e.g. a delivery timeout). The UI must not present an `error` output as
    /// the mission's deliverable.
    pub status: Option<String>,
    pub model: Option<String>,
    pub total_tokens: Option<i64>,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub finished_at: Option<String>,
    /// Assignment nonce that produced this output. Used to hide stale results
    /// while a newer run is materializing.
    pub assignment_nonce: Option<String>,
    /// How the deliverable was produced: `"single_turn"` when the mesh agent
    /// loop was unavailable and this is one model turn (no tools/sub-agents).
    /// Absent (`None`) for a full agent-loop run — the normal case.
    pub source: Option<String>,
    /// Set when the run's `ok` output is actually a capability/limit STOP rather
    /// than a real deliverable — today the daily token budget (enforced by the
    /// sandbox InferencePolicy / router). The UI renders this as an actionable
    /// state ("raise the budget / narrow the objective"), never as the answer.
    pub blocked: Option<RunBlockedDto>,
    /// Whether every artifact declared by the agent was durably persisted.
    /// Older runs may not carry this field.
    pub artifact_persistence: Option<String>,
    pub artifact_count: Option<i64>,
    pub declared_artifact_count: Option<i64>,
}

/// A run that returned transport-`ok` but whose body is a capability/limit stop,
/// not a deliverable. Surfaced so the operator gets an honest, actionable state
/// instead of a non-answer dressed up as the mission's output.
#[derive(Debug, Serialize, Clone)]
pub struct RunBlockedDto {
    /// Machine reason. Today: `"budget"`.
    pub reason: String,
    /// One-line, plain-language explanation.
    pub detail: String,
    /// Tokens spent / the enforced limit, parsed from the router's message when
    /// present (the limit ideally originates from the sandbox InferencePolicy).
    pub spent: Option<i64>,
    pub limit: Option<i64>,
}

/// Classify a transport-`ok` run whose body is really a STOP condition (not a
/// deliverable). Today this recognises the daily token-budget block the router
/// enforces from the sandbox InferencePolicy — its message reads
/// "Daily token budget exceeded (23131/20000 tokens)". Returns `None` for a
/// genuine deliverable (or an already-`error` run, handled separately).
pub(crate) fn classify_blocked(status: Option<&str>, output: &str) -> Option<RunBlockedDto> {
    if status == Some("error") {
        return None;
    }
    let low = output.to_ascii_lowercase();
    let budget_hit = low.contains("token budget")
        && (low.contains("exceeded") || low.contains("429") || low.contains("budget at"));
    if budget_hit {
        let (spent, limit) = parse_budget_pair(output);
        return Some(RunBlockedDto {
            reason: "budget".into(),
            detail: "The run reached its daily token budget and stopped before finishing.".into(),
            spent,
            limit,
        });
    }
    None
}

/// Extract the `spent/limit` pair from a budget message like
/// "... (23131/20000 tokens)". Returns `(None, None)` when absent/unparseable.
fn parse_budget_pair(output: &str) -> (Option<i64>, Option<i64>) {
    // Find a "<digits>/<digits>" run (optionally followed by " tokens").
    let bytes = output.as_bytes();
    for (i, _) in output.match_indices('/') {
        // Walk left over digits.
        let mut l = i;
        while l > 0 && bytes[l - 1].is_ascii_digit() {
            l -= 1;
        }
        // Walk right over digits.
        let mut r = i + 1;
        while r < bytes.len() && bytes[r].is_ascii_digit() {
            r += 1;
        }
        if l < i && r > i + 1 {
            let spent = output[l..i].parse::<i64>().ok();
            let limit = output[i + 1..r].parse::<i64>().ok();
            if spent.is_some() && limit.is_some() {
                return (spent, limit);
            }
        }
    }
    (None, None)
}

/// One artifact file in a mission's deliverable set. `content` is present for
/// text artifacts (markdown, json, csv, …) and `None` for binary ones, which
/// are still listed by name + size so the set is honestly complete.
#[derive(Serialize)]
pub struct MissionArtifactDto {
    pub name: String,
    pub size_bytes: Option<i64>,
    pub content: Option<String>,
    pub content_bytes: Option<i64>,
    pub content_truncated: bool,
    pub source_agent: Option<String>,
    pub source_path: Option<String>,
    pub digest: Option<String>,
    #[serde(skip_serializing)]
    full_content: Option<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct TeamRolePlanDto {
    pub selected_roles: Vec<String>,
    pub skipped_roles: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct TeamCollaborationEventDto {
    pub at: Option<String>,
    pub event: String,
    pub agent: Option<String>,
    pub member: Option<String>,
    pub outcome: Option<String>,
    pub message_id: Option<String>,
    pub reply_preview: Option<String>,
    pub content_preview: Option<String>,
}

impl std::fmt::Debug for MissionArtifactDto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MissionArtifactDto")
            .field("name", &self.name)
            .field("size_bytes", &self.size_bytes)
            .field("content_bytes", &self.content_bytes)
            .field("content_truncated", &self.content_truncated)
            .field("source_agent", &self.source_agent)
            .field("source_path", &self.source_path)
            .field("digest", &self.digest)
            .finish_non_exhaustive()
    }
}

const ARTIFACT_PREVIEW_MAX_BYTES: usize = 8 * 1024;
const ARTIFACT_PREVIEW_TOTAL_BYTES: usize = 64 * 1024;

fn artifact_preview(
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

fn structured_team_evidence(
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

fn canonicalize_assignment_event_roles(
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

fn select_task_checkpoint(
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

fn merge_trace_total_tokens(result: &mut Option<MissionResultDto>, trace_total_tokens: i64) {
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

fn subagent_trace_from_artifacts(artifacts: &[MissionArtifactDto]) -> Vec<serde_json::Value> {
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

fn valid_task_checkpoint(value: serde_json::Value) -> Option<serde_json::Value> {
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

/// The composed run, in plain terms, for the mission-review surface.
#[derive(Debug, Serialize)]
pub struct CompositionDto {
    pub runtime: Option<String>,
    pub model: Option<String>,
    pub instructions: Option<String>,
    pub tool_policy: Option<String>,
    pub mcp_servers: Vec<String>,
    pub egress: Vec<String>,
    pub isolation: Option<String>,
    pub memory: Option<String>,
}

/// Create-task request body from the UI.
#[derive(Debug, Deserialize)]
pub struct CreateTaskRequest {
    pub name: String,
    pub objective: String,
    pub display_name: Option<String>,
    pub envelope: EnvelopeDto,
    /// Optional parent task name — when set, this creates a delegated child
    /// whose envelope the controller verifies against the parent's.
    #[serde(default)]
    pub parent: Option<String>,
    /// The editable run blueprint composed on the launch package
    /// (runtime/model/instructions/tools/MCP/egress/isolation/memory).
    #[serde(default)]
    pub blueprint: Option<BlueprintDto>,
    #[serde(default)]
    pub delegation: Option<crate::routes::compose::ComposeDelegation>,
    /// When true, the task is created already launched — the controller
    /// materializes the sandbox immediately. The package's "launch" action.
    #[serde(default)]
    pub launch: bool,
    /// Repos selected from the authenticated principal's GitHub connection. The
    /// server validates the full set and derives the typed connection reference.
    #[serde(default)]
    pub git_write_repos: Option<Vec<String>>,
    /// The identity creating this mission (the Bridge principal), stamped as
    /// `kars.azure.com/created-by` for per-user budget attribution. The web sets
    /// it from the current session; absent => "unattributed".
    #[serde(default)]
    pub created_by: Option<String>,
    /// Per-mission retention override, in seconds — auto-delete this mission's
    /// record this long after its deliverable lands (mirrors Kubernetes'
    /// `Job.ttlSecondsAfterFinished`). `0` disables retention for this mission
    /// specifically even if a cluster-wide default is set. Absent inherits the
    /// cluster-wide default (which itself defaults to "never").
    #[serde(default)]
    pub retention_ttl_seconds: Option<i64>,
}

fn phase_of(task: &KarsTask) -> String {
    task.status
        .as_ref()
        .and_then(|s| s.phase.clone())
        .unwrap_or_else(|| "Pending".to_string())
}

fn to_summary(task: &KarsTask) -> TaskSummaryDto {
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

fn is_task_owner(task: &KarsTask, principal: &Principal) -> bool {
    task_is_owned_by(task, principal)
}

/// Extract the human deliverable from the agent's run output. The native
/// OpenClaw agent returns a structured `--json` envelope
/// (`{ runId, status, summary, result: { payloads: [ { text } ] } }`); showing
/// that raw — escaped quotes, literal `\n`, JSON braces — is the single most
/// embarrassing thing in the UI. Pull out the actual prose (joining payload
/// texts), tolerating a few shapes; pass plain-text output through unchanged.
fn repair_replacement_question_marks(text: &str) -> String {
    let characters = text.chars().collect::<Vec<_>>();
    let mut repaired = String::with_capacity(text.len());
    for (index, character) in characters.iter().copied().enumerate() {
        if character != '?' {
            repaired.push(character);
            continue;
        }
        let previous = index
            .checked_sub(1)
            .and_then(|at| characters.get(at))
            .copied();
        let next = characters.get(index + 1).copied();
        if previous.is_some_and(char::is_alphanumeric) && next.is_some_and(char::is_alphanumeric) {
            repaired.push('-');
        } else if previous.is_some_and(|value| value.is_ascii_digit())
            && next.is_some_and(char::is_whitespace)
        {
            repaired.push('.');
        } else {
            repaired.push('?');
        }
    }
    repaired
}

fn strip_sandbox_banner(text: &str) -> String {
    let lines = text.lines().collect::<Vec<_>>();
    let first_content = lines.iter().position(|line| !line.trim().is_empty());
    let Some(start) = first_content else {
        return String::new();
    };
    let prefix_end = (start + 16).min(lines.len());
    let prefix = &lines[start..prefix_end];
    let lower_prefix = prefix.join("\n").to_ascii_lowercase();
    if !lower_prefix.contains("kars sandbox")
        || !lower_prefix.contains("sandbox id:")
        || !lower_prefix.contains("security:")
        || !lower_prefix.contains("capabilities:")
    {
        return repair_replacement_question_marks(text.trim());
    }
    let Some(capabilities_offset) = prefix
        .iter()
        .position(|line| line.to_ascii_lowercase().contains("capabilities:"))
    else {
        return repair_replacement_question_marks(text.trim());
    };
    repair_replacement_question_marks(lines[start + capabilities_offset + 1..].join("\n").trim())
}

pub(crate) fn deliverable_text(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        // Native agent envelope: result.payloads[].text
        if let Some(payloads) = v
            .get("result")
            .and_then(|r| r.get("payloads"))
            .and_then(|p| p.as_array())
        {
            let joined = payloads
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n\n");
            if !joined.trim().is_empty() {
                return strip_sandbox_banner(&joined);
            }
        }
        // Other harness shapes.
        for path in [["reply", "text"], ["result", "text"]] {
            if let Some(t) = v
                .get(path[0])
                .and_then(|x| x.get(path[1]))
                .and_then(|t| t.as_str())
                && !t.trim().is_empty()
            {
                return strip_sandbox_banner(t);
            }
        }
        for key in ["text", "output", "summary"] {
            if let Some(t) = v.get(key).and_then(|t| t.as_str())
                && !t.trim().is_empty()
            {
                return strip_sandbox_banner(t);
            }
        }
    }
    // Tolerant fallback: a *truncated* native envelope (the commons caps stored
    // content, which can cut the JSON mid-string so `serde` can't parse it) still
    // begins like `{ "runId": ..., "result": { "payloads": [ { "text": "…` — pull
    // the first `"text"` string value out by hand and JSON-unescape it so old,
    // truncated entries render as prose instead of raw JSON.
    if trimmed.starts_with('{')
        && trimmed.contains("\"text\"")
        && let Some(extracted) = extract_first_json_string(trimmed, "text")
        && !extracted.trim().is_empty()
    {
        return strip_sandbox_banner(&extracted);
    }
    strip_sandbox_banner(raw)
}

/// The sentinel a standing-team run emits when nothing changed since last time.
/// A deliverable that is ONLY this is a no-op, not a real deliverable.
pub(crate) const NO_CHANGE_SENTINEL: &str = "[[NO_MATERIAL_CHANGE]]";

/// A pull request the mission opened — a first-class deliverable type.
#[derive(Debug, Clone, serde::Serialize, PartialEq)]
pub struct PullRequestRef {
    /// `owner/repo`.
    pub repo: String,
    pub number: i64,
    /// The canonical GitHub URL.
    pub url: String,
}

/// Extract the pull requests a mission opened from its deliverable text. The
/// router authors PRs via the keyless git proxy and the agent reports the URL;
/// we surface each as a tracked deliverable. Deduplicated, in first-seen order.
pub(crate) fn extract_pull_requests(text: &str) -> Vec<PullRequestRef> {
    let mut out: Vec<PullRequestRef> = Vec::new();
    // Scan for `github.com/<owner>/<repo>/pull/<number>` occurrences without a
    // regex dep: split on the marker and parse each following segment.
    for seg in text.split("github.com/").skip(1) {
        // owner/repo/pull/NUMBER
        let mut it = seg.splitn(4, '/');
        let (Some(owner), Some(repo), Some(kind)) = (it.next(), it.next(), it.next()) else {
            continue;
        };
        if kind != "pull" && kind != "pulls" {
            continue;
        }
        let Some(rest) = it.next() else { continue };
        let num: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if owner.is_empty() || repo.is_empty() || num.is_empty() {
            continue;
        }
        let Ok(number) = num.parse::<i64>() else {
            continue;
        };
        let repo_full = format!("{owner}/{repo}");
        let url = format!("https://github.com/{repo_full}/pull/{number}");
        let pr = PullRequestRef {
            repo: repo_full,
            number,
            url,
        };
        if !out.contains(&pr) {
            out.push(pr);
        }
    }
    out
}

fn deliverable_pull_requests(
    data: &std::collections::BTreeMap<String, String>,
) -> Vec<PullRequestRef> {
    let status = data.get("status").map(String::as_str);
    let output = data.get("output").map(String::as_str).unwrap_or("");
    if !is_real_deliverable(status, output) {
        return Vec::new();
    }
    extract_pull_requests(&deliverable_text(output))
}

pub(crate) fn is_failure_shaped_output(output: &str) -> bool {
    let text = deliverable_text(output);
    let lower = text
        .trim_start_matches(|character: char| {
            character.is_whitespace()
                || matches!(character, '*' | '_' | '#' | '>' | '`' | '-' | '?' | '🔒')
        })
        .to_ascii_lowercase();
    let head: String = lower.chars().take(800).collect();
    head.starts_with("unexpected tokens remaining in message header")
        || head.starts_with("assignment progress lease expired")
        || head.starts_with("native agent failed")
        || head.starts_with("error processing task")
        || (head.starts_with("kars sandbox - secure ai runtime") && head.contains("how can i help"))
        || head.starts_with("now await pr-watcher")
        || head.starts_with("awaiting handback from")
}

pub(crate) fn is_no_change_output(output: &str) -> bool {
    let text = deliverable_text(output);
    let head = text.trim_start();
    if head.starts_with(NO_CHANGE_SENTINEL) {
        return true;
    }
    let Some(sentinel_at) = head.find(NO_CHANGE_SENTINEL) else {
        return false;
    };
    let prefix = &head[..sentinel_at];
    sentinel_at <= 1_200
        && prefix.to_ascii_lowercase().contains("kars sandbox")
        && prefix.contains("Sandbox ID:")
        && prefix.contains("Security:")
        && prefix.contains("Capabilities:")
}

/// True when a run output is NOT a real, showable deliverable — either the run
/// errored, produced nothing, or reported "no material change". Used to keep
/// hung / zero-output / no-op runs out of the deliverable index and the "latest
/// deliverable" hero (audit f9/f13: a receipt/deliverable requires real work).
pub(crate) fn is_real_deliverable(status: Option<&str>, deliverable: &str) -> bool {
    if status == Some("error") {
        return false;
    }
    let t = deliverable.trim();
    if t.is_empty() {
        return false;
    }
    if is_no_change_output(deliverable) {
        return false;
    }
    if is_failure_shaped_output(deliverable) {
        return false;
    }
    // A capability/limit STOP (e.g. the daily token budget) came back transport-ok
    // but is not the mission's answer — never treat it as a deliverable.
    if classify_blocked(status, deliverable).is_some() {
        return false;
    }
    true
}

/// A clean 2–3 line preview of a deliverable for cards and list rows — never the
/// raw transcript. Strips the no-change sentinel, markdown table/heading noise,
/// and collapses whitespace, then caps the length (audit f3).
pub(crate) fn deliverable_excerpt(raw: &str) -> String {
    let text = deliverable_text(raw);
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }
        // Strip leading markdown wrapping (emphasis / heading / block-quote /
        // inline-code / bullet markers) FIRST, so a wrapped control sentinel
        // like `**[[NO_MATERIAL_CHANGE]]**` is unwrapped before we test for it.
        // Previously the sentinel check ran on the raw line and a bold-wrapped
        // sentinel slipped through into the excerpt.
        let cleaned = l
            .trim_start_matches(['*', '_', '#', '>', '`', '-', ' '])
            .trim();
        if cleaned.is_empty() {
            continue;
        }
        // Drop the no-change sentinel (now unwrapped) and markdown table
        // rows/rules.
        let cleaned = if let Some(reason) = cleaned.strip_prefix(NO_CHANGE_SENTINEL) {
            let reason = reason
                .trim_start_matches(|character: char| {
                    character.is_whitespace() || matches!(character, ':' | '-' | '—')
                })
                .trim();
            if reason.is_empty() {
                continue;
            }
            reason
        } else {
            cleaned
        };
        if cleaned.starts_with('|') {
            continue;
        }
        if cleaned.starts_with("===") {
            continue;
        }
        let lower = cleaned.to_ascii_lowercase();
        if [
            "kars sandbox - secure ai runtime",
            "foundry project:",
            "model:",
            "sandbox id:",
            "security summary",
            "security:",
            "capabilities:",
            "role plan",
            "role roster",
            "roles spawned:",
        ]
        .iter()
        .any(|prefix| lower.starts_with(prefix))
        {
            continue;
        }
        out.push(cleaned.to_string());
        if out.len() >= 3 {
            break;
        }
    }
    let joined = out.join(" ");
    let joined = joined.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.chars().count() > 240 {
        let mut s: String = joined.chars().take(240).collect();
        s.push('…');
        s
    } else {
        joined
    }
}

/// end of input. Returns `None` if the key/opening quote isn't present.
fn extract_first_json_string(s: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let after_key = &s[s.find(&needle)? + needle.len()..];
    let colon = after_key.find(':')?;
    let rest = &after_key[colon + 1..];
    let open = rest.find('"')?;
    let body = &rest[open + 1..];
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => break,
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('/') => out.push('/'),
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                        out.push(ch);
                    }
                }
                Some(other) => out.push(other),
                None => break,
            },
            _ => out.push(c),
        }
    }
    Some(out)
}

/// Human-readable objective for display. A standing-run objective is wrapped
/// with internal scaffolding — `Standing-operation run for team 'X'. Charter:
/// <charter>. Your capabilities: … Operating contract: … --- BEGIN UNTRUSTED
/// REFERENCE DATA …` — none of which a person should see. Extract the charter /
/// intent and drop the capability manifest + injected prior-knowledge preamble.
/// Ordinary mission objectives (no wrapper) pass through unchanged.
pub(crate) fn clean_objective(raw: &str) -> String {
    // Everything from the first scaffolding marker onward is internal.
    const MARKERS: [&str; 5] = [
        "Your capabilities:",
        "Operating contract:",
        "--- BEGIN UNTRUSTED REFERENCE DATA",
        "\n\nMode note",
        "BEGIN UNTRUSTED REFERENCE DATA",
    ];
    let mut end = raw.len();
    for m in MARKERS {
        if let Some(i) = raw.find(m) {
            end = end.min(i);
        }
    }
    let head = raw[..end].trim();
    // Unwrap the standing-run charter prefix when present.
    if let Some(i) = head.find("Charter:") {
        let charter = head[i + "Charter:".len()..].trim();
        let charter = charter.trim_end_matches('.').trim();
        if !charter.is_empty() {
            return charter.to_string();
        }
    }
    // Defense in depth: strip any leaked 2026 loop scaffold so LOOP:/GOAL:/
    // CYCLE/[[…]] control-blobs never reach a title, card, or displayed
    // objective. A scaffold's GOAL line IS the human intent — extract it.
    strip_loop_scaffold(head)
}

/// Extracts the human intent from a leaked loop scaffold. Loop scaffolds are
/// shaped as `LOOP: <pattern>\nGOAL: <intent>\nCYCLE: …\nSUCCESS: …\nSTOP: …\n
/// SUB-AGENT INHERITANCE: …`. If a `GOAL:` line is present we return it (the
/// real intent); otherwise we drop the scaffold control lines and return what
/// remains. Plain objectives (no scaffold) pass through unchanged.
/// True when a string carries a leaked 2026 loop scaffold — used to keep the
/// control-blob out of titles, cards, URLs, and displayed objectives.
pub(crate) fn looks_scaffolded(text: &str) -> bool {
    text.starts_with("LOOP:")
        || text.contains("\nGOAL:")
        || text.starts_with("GOAL:")
        || text.contains("SUB-AGENT INHERITANCE")
        || text.contains("[[")
}

/// Conversational lead-ins that mark a string as a prompt rather than a title
/// ("Can you please …", "I need you to …"). Stripped when deriving a title.
const TITLE_LEAD_INS: [&str; 16] = [
    "can you please ",
    "could you please ",
    "would you please ",
    "can you ",
    "could you ",
    "would you ",
    "please ",
    "i need you to ",
    "i want you to ",
    "i'd like you to ",
    "i would like you to ",
    "i need ",
    "i want ",
    "help me ",
    "let's ",
    "lets ",
];

/// Strip any leading conversational lead-in(s), case-insensitively.
fn strip_title_lead_in(s: &str) -> &str {
    let mut cur = s.trim_start();
    loop {
        let lower = cur.to_ascii_lowercase();
        let mut matched = false;
        for lead in TITLE_LEAD_INS {
            if lower.starts_with(lead) {
                cur = cur[lead.len()..].trim_start();
                matched = true;
                break;
            }
        }
        if !matched {
            return cur;
        }
    }
}

/// Shorten a bare URL token to a compact, human label — a GitHub-style
/// `owner/repo`, else the last path segment, else the host — so a title reads
/// "analyse Azure/kars dependabot PRs", not a 60-char URL.
fn shorten_url_token(tok: &str) -> String {
    let lower = tok.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return tok.to_string();
    }
    let rest = tok
        .trim_end_matches(['.', ',', ')', ']', '?', '!'])
        .split_once("://")
        .map(|x| x.1)
        .unwrap_or(tok);
    let mut parts = rest.split('/');
    let host = parts.next().unwrap_or("");
    let segs: Vec<&str> = parts.filter(|s| !s.is_empty()).collect();
    if host.contains("github.") && segs.len() >= 2 {
        format!("{}/{}", segs[0], segs[1])
    } else if let Some(last) = segs.last() {
        (*last).to_string()
    } else {
        host.to_string()
    }
}

/// True when `display` is a genuine human title, not a truncated prompt: it has
/// no conversational lead-in, carries no URL, isn't just a prefix of the
/// objective, and isn't paragraph-length.
fn is_genuine_title(display: &str, clean_objective: &str) -> bool {
    let lower = display.to_ascii_lowercase();
    if TITLE_LEAD_INS.iter().any(|l| lower.starts_with(l)) {
        return false;
    }
    if lower.contains("http://") || lower.contains("https://") {
        return false;
    }
    let d_trim = lower.trim_end_matches('…').trim();
    let obj_lower = clean_objective.to_ascii_lowercase();
    if d_trim.len() >= 24 && obj_lower.starts_with(d_trim) {
        return false;
    }
    display.chars().count() <= 72
}

/// Derive a compact, title-like phrase from a verbose objective: strip the
/// conversational lead-in, shorten URLs, take the first sentence/clause, drop a
/// trailing " - …" condition tail, cap at a word boundary, and capitalize.
fn concise_title(text: &str) -> String {
    let no_lead = strip_title_lead_in(text.trim());
    let shortened: String = no_lead
        .split_whitespace()
        .map(shorten_url_token)
        .collect::<Vec<_>>()
        .join(" ");
    let first = shortened
        .split(['.', '\n', '?', '!'])
        .find(|s| !s.trim().is_empty())
        .unwrap_or(&shortened)
        .trim();
    // Prompts often append conditions after a dash ("… PRs - categorize the …").
    let first = first.split(" - ").next().unwrap_or(first).trim();
    let capped = if first.chars().count() > 56 {
        // Cut at the last word boundary within the cap.
        let head: String = first.chars().take(56).collect();
        let cut = head.rfind(' ').unwrap_or(head.len());
        format!("{}…", head[..cut].trim_end())
    } else {
        first.to_string()
    };
    let mut chars = capped.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// A clean, human display title for a task. Uses an explicit display name only
/// when it is a GENUINE title (not a conversational prompt truncated into the
/// display slot); otherwise derives a concise title from the cleaned objective.
/// Guarantees LOOP:/GOAL:/[[…]] and raw pasted prompts never reach a card, list
/// row, breadcrumb, or tab — it runs at the read/DTO boundary for every task.
pub(crate) fn clean_display_name(display: &Option<String>, objective: &str) -> Option<String> {
    let clean_obj = clean_objective(objective);
    if let Some(d) = display.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty())
        && !looks_scaffolded(d)
        && is_genuine_title(d, &clean_obj)
    {
        return Some(d.to_string());
    }
    // No genuine title — derive a concise one from the objective (or, when the
    // objective is empty, from the de-scaffolded display string).
    let source = if clean_obj.is_empty() {
        strip_loop_scaffold(display.as_deref().unwrap_or(""))
    } else {
        clean_obj.clone()
    };
    let title = concise_title(&source);
    if title.is_empty() { None } else { Some(title) }
}

fn strip_loop_scaffold(text: &str) -> String {
    if !looks_scaffolded(text) {
        return text.to_string();
    }
    // Prefer the GOAL line — that is the human's restated intent.
    for line in text.lines() {
        let l = line.trim();
        if let Some(rest) = l.strip_prefix("GOAL:") {
            let goal = rest
                .trim()
                .trim_start_matches("[[")
                .trim_end_matches("]]")
                .trim();
            if !goal.is_empty() {
                return goal.to_string();
            }
        }
    }
    // No GOAL line — drop the scaffold control lines and return the remainder.
    const CONTROL_PREFIXES: [&str; 6] = [
        "LOOP:",
        "CYCLE:",
        "SUCCESS:",
        "STOP:",
        "SUB-AGENT INHERITANCE",
        "[",
    ];
    let kept: Vec<&str> = text
        .lines()
        .filter(|l| {
            let t = l.trim();
            !t.is_empty() && !CONTROL_PREFIXES.iter().any(|p| t.starts_with(p))
        })
        .collect();
    kept.join(" ").trim().to_string()
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
fn to_detail(
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

/// Resolve the cluster handle or surface a clear "cluster not wired" error.
pub(crate) fn require_cluster(state: &AppState) -> AppResult<&crate::kars::cluster::Cluster> {
    state.cluster().ok_or(AppError::ClusterUnavailable)
}

/// Sanitize a filename to the ConfigMap key form the controller uses (alnum,
/// '-', '_', '.') so the manifest name can look up its stored content.
fn artifact_key(name: &str) -> String {
    let k: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if k.is_empty() { "artifact".into() } else { k }
}

/// Best-effort content type from a filename extension, so a downloaded artifact
/// opens sensibly in the browser instead of forcing a save dialog for text.
fn artifact_content_type(name: &str) -> &'static str {
    match name
        .rsplit('.')
        .next()
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("md" | "markdown" | "txt" | "log") => "text/markdown; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("csv") => "text/csv; charset=utf-8",
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("yaml" | "yml") => "application/yaml; charset=utf-8",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("svg") => "image/svg+xml",
        Some("pdf") => "application/pdf",
        _ => "application/octet-stream",
    }
}

/// `GET /api/tasks/:ns/:name/artifact/:file` — Bridge-native artifact fetch.
/// Streams one artifact file's bytes (text from `data`, binary from
/// `binaryData`) so operators download deliverables in-product, never via
/// `kubectl`. Inline for previewable types; attachment otherwise.
pub async fn download_artifact(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name, file)): Path<(String, String, String)>,
) -> AppResult<axum::response::Response> {
    use axum::http::header;
    let cluster = require_cluster(&state)?;
    require_owned_task_or_output(cluster, &ns, &name, &principal).await?;
    let key = artifact_key(&file);
    let (bytes, is_binary) = cluster
        .read_mission_artifact_bytes(&name, &key)
        .await
        .ok_or(AppError::NotFound)?;
    let ctype = artifact_content_type(&file);
    // Inline-render text/known media; force a download for opaque binaries.
    let disposition = if is_binary && ctype == "application/octet-stream" {
        format!("attachment; filename=\"{key}\"")
    } else {
        format!("inline; filename=\"{key}\"")
    };
    axum::response::Response::builder()
        .header(header::CONTENT_TYPE, ctype)
        .header(header::CONTENT_DISPOSITION, disposition)
        .header(header::CACHE_CONTROL, "private, max-age=60")
        .body(axum::body::Body::from(bytes))
        .map_err(|e| AppError::Upstream(e.to_string()))
}

/// Merge a mission's artifact manifest (names + sizes, from the output
/// ConfigMap) with the text contents stored in the companion artifacts
/// ConfigMap. Binary artifacts appear in the manifest but carry `content:
/// None`. Returns an empty set honestly when the mission produced no artifacts.
async fn build_artifact_set(
    cluster: &crate::kars::cluster::Cluster,
    name: &str,
    output_data: Option<&std::collections::BTreeMap<String, String>>,
) -> Vec<MissionArtifactDto> {
    let manifest_json = output_data.and_then(|d| d.get("artifacts").cloned());
    let contents = cluster
        .read_mission_artifacts(name)
        .await
        .unwrap_or_default();

    // Prefer the manifest (authoritative order + sizes + binary entries); fall
    // back to whatever text artifacts are stored if no manifest is present.
    if let Some(mj) = manifest_json
        && let Ok(entries) = serde_json::from_str::<Vec<serde_json::Value>>(&mj)
    {
        let mut seen = std::collections::HashSet::new();
        let mut preview_budget = ARTIFACT_PREVIEW_TOTAL_BYTES;
        return entries
            .into_iter()
            .filter_map(|e| {
                let fname = e.get("name")?.as_str()?.to_string();
                // The manifest can list the same file twice (e.g. an artifact
                // recorded by both the run harness and the harvest step). Keep
                // the first — duplicates crash the UI's name-keyed lists.
                if !seen.insert(fname.clone()) {
                    return None;
                }
                let size_bytes = e.get("size_bytes").and_then(|v| v.as_i64());
                let (content, content_bytes, content_truncated, full_content) = artifact_preview(
                    contents.get(&artifact_key(&fname)).cloned(),
                    &mut preview_budget,
                );
                Some(MissionArtifactDto {
                    name: fname,
                    size_bytes,
                    content,
                    content_bytes,
                    content_truncated,
                    source_agent: e
                        .get("source_agent")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    source_path: e
                        .get("source_path")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    digest: e.get("digest").and_then(|v| v.as_str()).map(str::to_string),
                    full_content,
                })
            })
            .collect();
    }

    let mut preview_budget = ARTIFACT_PREVIEW_TOTAL_BYTES;
    contents
        .into_iter()
        .map(|(k, v)| {
            let size_bytes = v.len() as i64;
            let (content, content_bytes, content_truncated, full_content) =
                artifact_preview(Some(v), &mut preview_budget);
            MissionArtifactDto {
                size_bytes: Some(size_bytes),
                name: k,
                content,
                content_bytes,
                content_truncated,
                source_agent: None,
                source_path: None,
                digest: None,
                full_content,
            }
        })
        .collect()
}

/// `GET /api/namespaces/:ns/tasks` — list tasks in a namespace.
pub async fn list_tasks(
    State(state): State<AppState>,
    principal: Option<Extension<Principal>>,
    Path(ns): Path<String>,
) -> AppResult<Json<Vec<TaskSummaryDto>>> {
    let cluster = require_cluster(&state)?;
    let principal = principal
        .map(|Extension(principal)| principal)
        .ok_or_else(|| AppError::Forbidden("signed-in principal required".into()))?;
    let api: Api<KarsTask> = cluster.tasks(&ns);
    let list = api
        .list(&ListParams::default())
        .await
        .map_err(map_kube_err)?;
    // Cross-reference delivered + failed missions in ONE pass over the persisted
    // outputs, so the list can show "Delivered" / "Run failed" instead of
    // misreading an idle delivered run — or a hung errored run — as "drafting".
    let outputs = cluster.list_mission_outputs().await;
    let mut terminal = std::collections::HashMap::<String, &'static str>::new();
    for record in &outputs {
        match record.data.get("status").map(String::as_str) {
            Some("ok")
                if record
                    .data
                    .get("output")
                    .is_some_and(|output| !output.trim().is_empty()) =>
            {
                terminal
                    .entry(record.task_name.clone())
                    .or_insert("delivered");
            }
            Some("error") => {
                terminal.entry(record.task_name.clone()).or_insert("failed");
            }
            _ => {}
        }
    }
    let delivered: std::collections::HashSet<String> = terminal
        .iter()
        .filter(|(_, status)| **status == "delivered")
        .map(|(task, _)| task.clone())
        .collect();
    let failed: std::collections::HashSet<String> = terminal
        .iter()
        .filter(|(_, status)| **status == "failed")
        .map(|(task, _)| task.clone())
        .collect();
    let mut summaries: Vec<TaskSummaryDto> = list
        .items
        .iter()
        .filter(|task| is_task_owner(task, &principal))
        .map(|t| {
            let mut s = to_summary(t);
            s.delivered = delivered.contains(&s.name);
            s.failed = failed.contains(&s.name);
            s
        })
        .collect();

    // Persist history: a mission whose KarsTask CR has been garbage-collected
    // (retired-run GC) still has its delivered/errored output ConfigMap. Without
    // this, completed missions silently vanish from the list mid-session and
    // their direct URLs 404 ("data loss", audit BUG-8). Re-add any output-only
    // mission that isn't already represented by a live CR. Team-run machinery
    // (`<team>-run-<epoch>`) is excluded — those belong to the Team view, which
    // is exactly what the live-CR path already hides.
    let live_names: std::collections::HashSet<String> =
        summaries.iter().map(|s| s.name.clone()).collect();
    for record in &outputs {
        let task = &record.task_name;
        let d = &record.data;
        if d.get("ownerSub").map(String::as_str) != Some(principal.sub.as_str()) {
            continue;
        }
        if live_names.contains(task) || regex_lite_is_team_run(task) {
            continue;
        }
        let is_ok = delivered.contains(task);
        let is_err = failed.contains(task);
        // Only surface a genuinely terminal output (delivered or errored); skip
        // stray/empty outputs so we don't invent phantom missions.
        if !is_ok && !is_err {
            continue;
        }
        summaries.push(TaskSummaryDto {
            name: task.clone(),
            namespace: ns.clone(),
            objective: d.get("objective").cloned().unwrap_or_default(),
            display_name: d
                .get("displayName")
                .cloned()
                .filter(|s| !s.trim().is_empty()),
            created_at: d.get("startedAt").cloned(),
            tier: d.get("tier").and_then(|v| v.parse().ok()).unwrap_or(0),
            phase: if is_err {
                "Failed".into()
            } else {
                "Delivered".into()
            },
            envelope_digest: None,
            team: d.get("team").cloned(),
            delivered: is_ok,
            failed: is_err,
            launched: true,
            execution_phase: Some("Idle".into()),
        });
    }
    Ok(Json(summaries))
}

/// True when `name` looks like a standing-team run task (`<team>-run-<epoch>`),
/// which the Missions surface intentionally hides (they belong to the Team
/// view). A tiny hand-rolled check to avoid a regex dependency.
fn regex_lite_is_team_run(name: &str) -> bool {
    if let Some(idx) = name.rfind("-run-") {
        let suffix = &name[idx + "-run-".len()..];
        return !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit());
    }
    false
}

/// `GET /api/namespaces/:ns/tasks/:name` — fetch one task, with its delegated
/// children resolved (tasks whose `parentRef` points at this task).
pub async fn get_task(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
) -> AppResult<Json<TaskDetailDto>> {
    let cluster = require_cluster(&state)?;
    let api: Api<KarsTask> = cluster.tasks(&ns);
    let task = match api.get_opt(&name).await.map_err(map_kube_err)? {
        Some(t) => t,
        // The KarsTask CR was garbage-collected (retired-run GC) but the
        // mission's terminal output persists. Synthesize a read-only detail from
        // it so a delivered/failed mission's page — and the list link that now
        // shows it — doesn't 404 mid-session. Genuine unknowns still 404.
        None => return synth_detail_from_output(cluster, &ns, &name, &principal).await,
    };
    if !is_task_owner(&task, &principal) {
        return Err(AppError::NotFound);
    }
    // Resolve direct children by scanning the namespace for parentRef == name.
    // Exclude RUN INSTANCES (cadence/taskforce runs named `*-run-<epoch>` or
    // annotated team-role=taskforce): those are run history, not org-chart roles.
    // Without this the org chart floods with every historical run of a standing
    // team as a duplicate node. Same predicate list_agents uses to identify runs.
    let all = api
        .list(&ListParams::default())
        .await
        .map_err(map_kube_err)?;
    let children: Vec<TaskSummaryDto> = all
        .items
        .iter()
        .filter(|t| t.spec.parent_ref.as_ref().is_some_and(|r| r.name == name))
        .filter(|t| {
            let is_run = t.name_any().contains("-run-")
                || t.annotations()
                    .get("kars.azure.com/team-role")
                    .map(String::as_str)
                    == Some("taskforce");
            !is_run
        })
        .map(to_summary)
        .collect();

    // Runtime agents: the sub-agents this mission's agent spawned at run time.
    // The inference router labels each spawned KarsSandbox
    // `kars.azure.com/parent=<sandbox>`; surface them so the org chart reflects
    // the *running* agent/sub-agent tree, not only the governed role tree.
    let sandbox_name = task
        .status
        .as_ref()
        .and_then(|s| s.sandbox_ref.as_ref())
        .map(|r| r.name.clone());
    let sub_agents = match &sandbox_name {
        Some(sb) => cluster
            .sub_agent_sandboxes(&ns, sb)
            .await
            .iter()
            .map(to_sub_agent)
            .collect(),
        None => Vec::new(),
    };

    // Effective composition: once launched, show what the sandbox is ACTUALLY
    // running (read from the materialized InferencePolicy + KarsSandbox,
    // including any controller-defaulted model), not just the submitted
    // blueprint. Pre-launch, fall back to the blueprint (the planned config).
    let effective = match &sandbox_name {
        Some(sb) => {
            let ip = cluster
                .get_kind(&ns, "InferencePolicy", &format!("{name}-inference"))
                .await
                .ok()
                .flatten();
            let sandbox = cluster
                .get_kind(&ns, "KarsSandbox", sb)
                .await
                .ok()
                .flatten();
            composition_from_materialized(ip.as_ref(), sandbox.as_ref())
        }
        None => None,
    };

    // Live egress enforcement mode (Learn/Strict) read from the materialized
    // KarsSandbox — the real monitoring→enforced surface.
    let egress_mode = match &sandbox_name {
        Some(sb) => cluster.sandbox_egress_mode(sb).await,
        None => None,
    };

    // The mission's captured run result (persisted deliverable + real tokens).
    let output_data = cluster.read_mission_output(&name).await;
    let mut result = output_data.as_ref().and_then(|d| {
        let output = deliverable_text(d.get("output")?);
        let blocked = classify_blocked(d.get("status").map(String::as_str), &output);
        Some(MissionResultDto {
            output,
            status: d.get("status").cloned(),
            model: d.get("model").cloned(),
            total_tokens: d.get("totalTokens").and_then(|v| v.parse().ok()),
            prompt_tokens: d.get("promptTokens").and_then(|v| v.parse().ok()),
            completion_tokens: d.get("completionTokens").and_then(|v| v.parse().ok()),
            finished_at: d.get("finishedAt").cloned(),
            assignment_nonce: d.get("assignmentNonce").cloned(),
            source: d.get("source").cloned(),
            blocked,
            artifact_persistence: d.get("artifactPersistence").cloned(),
            artifact_count: d.get("artifactCount").and_then(|v| v.parse().ok()),
            declared_artifact_count: d.get("declaredArtifactCount").and_then(|v| v.parse().ok()),
        })
    });

    // The mission's full artifact set: the manifest (name + size, incl. binary)
    // comes from the output ConfigMap; text contents come from the companion
    // artifacts ConfigMap. Merge them so the set is complete and honest.
    let artifacts = build_artifact_set(cluster, &name, output_data.as_ref()).await;
    let successful_result = result.as_ref().is_some_and(|result| {
        result.status.as_deref() != Some("error") && result.blocked.is_none()
    });
    let checkpoint = select_task_checkpoint(
        cluster.read_mission_progress(&name).await,
        &artifacts,
        successful_result,
    );

    // The mission's live execution activity — the real per-round + per-tool
    // trace the agent emitted, persisted by the controller as the clean audit
    // record. Parsed from the trace ConfigMap; empty when no trace exists.
    let mut activity: Vec<serde_json::Value> = cluster
        .read_mission_trace(&name)
        .await
        .and_then(|raw| serde_json::from_str::<Vec<serde_json::Value>>(&raw).ok())
        .unwrap_or_default();
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

    // LIVE fallback. The persisted trace ConfigMap is written only once, at
    // delivery — so a still-running mission would otherwise show an EMPTY
    // activity trace (blank deploy timeline, agent graph, and map, and a
    // "Waiting for the first model round" that lies while the agent is already
    // on round 3). When no persisted trace exists yet and the mission is
    // launched, pull the SAME live router telemetry the Activity SSE streams —
    // the principal sandbox plus every sub-agent it spawned — so the WHOLE
    // detail page is genuinely live on each poll, not just the SSE tab.
    if activity.is_empty()
        && let Some(principal) = &sandbox_name
    {
        let mut live: Vec<serde_json::Value> = Vec::new();
        for mut ev in cluster.sandbox_live_trace(principal).await {
            if let Some(obj) = ev.as_object_mut() {
                obj.insert("agent".into(), serde_json::json!(name));
                obj.insert("agentInstance".into(), serde_json::json!(principal));
                obj.insert("agentRole".into(), serde_json::json!("principal"));
            }
            live.push(ev);
        }
        let mut descendants = cluster
            .sub_agent_sandbox_names(&ns, principal)
            .await
            .into_iter();
        loop {
            let sub_batch = descendants.by_ref().take(8).collect::<Vec<_>>();
            if sub_batch.is_empty() {
                break;
            }
            let mut polling = tokio::task::JoinSet::new();
            for sub in sub_batch {
                let cluster = cluster.clone();
                polling.spawn(async move {
                    let events = cluster.sandbox_live_trace(&sub).await;
                    (sub, events)
                });
            }
            while let Some(result) = polling.join_next().await {
                let Ok((sub, events)) = result else {
                    continue;
                };
                for mut ev in events {
                    if let Some(obj) = ev.as_object_mut() {
                        obj.insert("agent".into(), serde_json::json!(sub.clone()));
                        obj.insert("agentInstance".into(), serde_json::json!(sub.clone()));
                        obj.insert("agentRole".into(), serde_json::json!("subagent"));
                    }
                    live.push(ev);
                }
            }
        }
        activity = live;
    }

    // Loop-shape telemetry (rounds, tool calls). Token totals live on `result`.
    // Derive rollups from the persisted per-round/per-tool trace when the run's
    // output ConfigMap didn't include them — some harnesses persist the trace
    // but not the totals, which left a DELIVERED mission's map reading
    // "Not run yet" / "No activity". The trace is the honest source either way.
    let trace_round_events = activity
        .iter()
        .filter(|e| e.get("kind").and_then(|k| k.as_str()) == Some("round"))
        .count() as i64;
    let trace_tool_events = activity
        .iter()
        .filter(|e| e.get("kind").and_then(|k| k.as_str()) == Some("tool"))
        .count() as i64;
    let trace_total_tokens: i64 = activity
        .iter()
        .filter(|e| e.get("kind").and_then(|k| k.as_str()) == Some("round"))
        .filter_map(|e| e.get("total_tokens").and_then(serde_json::Value::as_i64))
        .sum();

    // Backfill the token total on the result from the trace when the output CM
    // didn't carry it (so token burn shows on a delivered run with a trace).
    merge_trace_total_tokens(&mut result, trace_total_tokens);

    let telemetry = {
        let mut rounds = output_data
            .as_ref()
            .and_then(|d| d.get("rounds").and_then(|v| v.parse::<i64>().ok()));
        let mut tool_calls = output_data
            .as_ref()
            .and_then(|d| d.get("toolCalls").and_then(|v| v.parse::<i64>().ok()));
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

    // The running agent's real mesh identity, discovered from the AGT registry
    // (harness-neutral). Only meaningful once a sandbox is running.
    let agent_identity = match &sandbox_name {
        Some(sb) => cluster.discover_agent_identity(sb).await,
        None => None,
    };

    // Pull requests the mission opened, extracted from its raw output — a PR is a
    // first-class delivery type, surfaced on the Artifacts tab (not just prose).
    let pull_requests = output_data
        .as_ref()
        .map(deliverable_pull_requests)
        .unwrap_or_default();

    Ok(Json(to_detail(
        &task,
        children,
        sub_agents,
        effective,
        result,
        artifacts,
        pull_requests,
        activity,
        telemetry,
        checkpoint,
        agent_identity,
        egress_mode,
    )))
}

/// `DELETE /api/namespaces/:ns/tasks/:name` — delete a mission and sweep its
/// persisted artifacts (deliverable, files, trace, review), so a deleted mission
/// leaves no orphaned ConfigMaps behind on the Artifacts page or as output-only
/// history. Mirrors the team-delete sweep. Idempotent-ish: 404 for unknowns.
pub async fn delete_task(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    let has_cr = require_owned_task_or_output(cluster, &ns, &name, &principal)
        .await?
        .is_some();
    if has_cr {
        cluster
            .delete_task(&ns, &name)
            .await
            .map_err(|e| AppError::Upstream(e.to_string()))?;
    } else {
        // CR already gone — just sweep the leftover ConfigMaps.
        cluster.sweep_mission_artifacts(&name).await;
    }
    Ok(Json(serde_json::json!({
        "deleted": true,
        "note": "Mission deleted. Its sandbox, deliverable, files, trace, and review record were removed."
    })))
}

/// Build a read-only mission detail purely from persisted ConfigMaps when the
/// KarsTask CR is gone (retired-run GC). Returns `NotFound` only when there is
/// genuinely no persisted output for the name. The envelope/composition are
/// left empty (the CR that carried them is gone) but the deliverable, artifacts,
/// activity trace, and telemetry — the parts a reviewer actually needs after the
/// fact — are surfaced, along with a terminal phase.
async fn synth_detail_from_output(
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

#[derive(serde::Serialize)]
pub struct TroubleshootDto {
    /// Whether a sandbox pod was found for this run at all.
    pub pod_found: bool,
    /// Ready containers vs total (e.g. "2/2") when a pod exists.
    pub pod_summary: Option<String>,
    /// Per-container state (name, ready, restarts, running/waiting/terminated).
    pub containers: Vec<crate::kars::cluster::ContainerState>,
    /// The tail of the agent container's REAL logs — the ground-truth evidence.
    pub agent_log_tail: Vec<String>,
    /// The specific log/status lines that matched a known failure signature —
    /// the smoking gun, highlighted for the reader.
    pub evidence: Vec<String>,
    /// Plain-language cause + remedy, derived from the REAL evidence above.
    pub cause: String,
    pub remedy: String,
    /// True when the harness itself is the problem (a chat-gateway on a one-shot
    /// mission) — the UI steers the re-compose to OpenClaw.
    pub harness_issue: bool,
    /// The recorded run status/reason, for cross-reference.
    pub result_status: Option<String>,
    pub result_reason: Option<String>,
}

/// `GET /api/namespaces/:ns/tasks/:name/troubleshoot` — actually troubleshoot a
/// run by reading the sandbox pod's real container states + agent logs and
/// diagnosing from that ground truth (not by pattern-matching a status string).
pub async fn troubleshoot_task(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
) -> AppResult<Json<TroubleshootDto>> {
    let cluster = require_cluster(&state)?;
    let task = require_owned_task_or_output(cluster, &ns, &name, &principal).await?;
    // Resolve the sandbox: for a mission the sandbox is named after the task.
    // Fall back to the task's recorded sandbox reference when present.
    let sandbox = task
        .and_then(|t| t.status.and_then(|s| s.sandbox_ref).map(|r| r.name))
        .unwrap_or_else(|| name.clone());

    let logs = cluster
        .read_sandbox_logs(&sandbox, "agent", 120)
        .await
        .unwrap_or_default();
    let containers = cluster.sandbox_container_states(&sandbox).await;
    let health = cluster.sandbox_pod_health(&sandbox).await;
    let output = cluster.read_mission_output(&name).await;
    let result_status = output.as_ref().and_then(|d| d.get("status").cloned());
    let result_reason = output.as_ref().and_then(|d| d.get("output").cloned());

    let log_lines: Vec<String> = logs.lines().map(|s| s.to_string()).collect();
    let (cause, remedy, harness_issue, evidence) =
        diagnose_run_failure(&log_lines, &containers, result_reason.as_deref());

    // Keep the last ~30 log lines for the "raw evidence" view.
    let agent_log_tail: Vec<String> = log_lines.iter().rev().take(30).rev().cloned().collect();

    Ok(Json(TroubleshootDto {
        pod_found: !containers.is_empty() || health.is_some(),
        pod_summary: health
            .as_ref()
            .map(|h| format!("{}/{}", h.ready_containers, h.total_containers)),
        containers,
        agent_log_tail,
        evidence,
        cause,
        remedy,
        harness_issue,
        result_status,
        result_reason,
    }))
}

/// Diagnose a run failure from REAL evidence: the agent log lines, the container
/// states, and the recorded reason. Returns (cause, remedy, harness_issue,
/// evidence-lines). Signatures are ordered most-specific first.
fn diagnose_run_failure(
    log_lines: &[String],
    containers: &[crate::kars::cluster::ContainerState],
    reason: Option<&str>,
) -> (String, String, bool, Vec<String>) {
    let find = |needles: &[&str]| -> Vec<String> {
        log_lines
            .iter()
            .filter(|l| {
                let low = l.to_lowercase();
                needles.iter().any(|n| low.contains(&n.to_lowercase()))
            })
            .cloned()
            .collect::<Vec<_>>()
    };

    // 1. Container-level infrastructure failures (authoritative).
    for c in containers {
        if let Some(r) = c.reason.as_deref() {
            let rl = r.to_lowercase();
            if rl.contains("imagepull") || rl.contains("errimage") {
                return (
                    format!("The “{}” container can't pull its image ({r}).", c.name),
                    "This is an infrastructure issue — the image tag is missing or the registry is unreachable. An operator should check the image reference and ACR/registry access.".into(),
                    false,
                    vec![format!("container {} is {} ({r})", c.name, c.state)],
                );
            }
            if rl.contains("crashloop") {
                return (
                    format!("The “{}” container is crash-looping (restarted {} times).", c.name, c.restarts),
                    "The container starts and immediately exits. Check the agent logs below for the panic/exit reason; often a bad config, missing secret, or an incompatible image.".into(),
                    false,
                    find(&["error", "panic", "fatal", "exited", "traceback"]),
                );
            }
            if rl.contains("oomkill") {
                return (
                    format!("The “{}” container was OOM-killed (out of memory).", c.name),
                    "The run exceeded the sandbox memory limit. Reduce the working set or raise the sandbox resources.".into(),
                    false,
                    vec![format!("container {} terminated: OOMKilled", c.name)],
                );
            }
        }
    }

    // 2. Hermes chat-gateway idle — the exact evidence from the entrypoint.
    let hermes = find(&[
        "no channels",
        "idle daemon mode",
        "no messaging platforms enabled",
        "gateway in idle",
    ]);
    if !hermes.is_empty() {
        return (
            "The agent is running on the Hermes chat-gateway harness, which started in IDLE DAEMON MODE because no messaging channels are configured. It is waiting for inbound messages (Telegram/Slack/…) and never executes a one-shot autonomous mission — so the run produced nothing and timed out.".into(),
            "Re-compose this mission on the OpenClaw harness (built for autonomous missions). Hermes only fits work that is DRIVEN by a chat channel.".into(),
            true,
            hermes,
        );
    }

    // 3. Content safety / auth / rate limit from logs.
    let safety = find(&[
        "content safety",
        "jailbreak",
        "blocked by policy",
        "content_filter",
    ]);
    if !safety.is_empty() {
        return (
            "A content-safety policy blocked the run.".into(),
            "Adjust the objective to avoid the flagged content, or ask an operator about the content-safety floor.".into(),
            false,
            safety,
        );
    }
    let auth = find(&[
        "401 unauthorized",
        "403 forbidden",
        "authentication failed",
        "invalid api key",
    ]);
    if !auth.is_empty() {
        return (
            "The agent's model calls were rejected by the provider (authentication/authorization).".into(),
            "An operator should check the router's provider credentials / workload-identity role for this model.".into(),
            false,
            auth,
        );
    }
    let rate = find(&["429", "rate limit", "too many requests", "quota"]);
    if !rate.is_empty() {
        return (
            "The model provider rate-limited or quota-limited the run.".into(),
            "Re-run after a short wait, or an operator can raise the model deployment's quota."
                .into(),
            false,
            rate,
        );
    }
    let schema = find(&[
        "stream_options.include_usage",
        "unknown parameter: 'stream_options",
        "stream_options: extra inputs",
    ]);
    if !schema.is_empty() {
        return (
            "The selected model rejected the translated inference request before it could reason or call tools.".into(),
            "This is a model/router compatibility issue, not an egress or prompt problem. Deploy the corrected inference router, then re-run the same mission; selecting another catalogue model is only a temporary workaround.".into(),
            false,
            schema,
        );
    }

    // 4. Fall back to the recorded reason.
    let rl = reason.unwrap_or("").to_lowercase();
    if rl.contains("did not come online")
        || rl.contains("not yet discoverable")
        || rl.contains("mesh registry")
    {
        return (
            "The agent never registered on the encrypted mesh within the startup window, so the controller timed the run out.".into(),
            "Re-run it — a fresh sandbox often comes up cleanly. If it repeats, check the agent logs below and the sandbox events.".into(),
            false,
            find(&["mesh", "relay", "register", "keepalive"]),
        );
    }
    if rl.contains("no progress heartbeat") || rl.contains("timed out") || rl.contains("timeout") {
        return (
            "The agent started but stopped making progress, so the controller timed the run out."
                .into(),
            "Re-run it; if it stalls again, narrow the objective or raise the token/time budget."
                .into(),
            false,
            find(&["error", "timeout", "stalled"]),
        );
    }

    (
        "The run ended without producing a deliverable. See the agent's own logs below for the specifics.".into(),
        "Re-run it, or re-compose with a different harness/model. If the logs show a repeating error, address that first.".into(),
        false,
        find(&["error", "panic", "fatal", "exception"]),
    )
}

/// Build the effective composition from the materialized InferencePolicy +
/// KarsSandbox — the real running config, including controller-defaulted fields.
fn composition_from_materialized(
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

fn to_sub_agent(o: &kube::core::DynamicObject) -> SubAgentDto {
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

fn validate_mission_fallback_route(
    options: &crate::routes::options::Options,
    blueprint: &BlueprintDto,
    runtime: &str,
    model: &ModelDto,
    required_capabilities: &std::collections::BTreeSet<String>,
    max_parallel: i32,
    total_tokens: Option<i64>,
) -> AppResult<()> {
    if !options
        .models
        .iter()
        .any(|option| option.provider == model.provider && option.deployment == model.deployment)
    {
        return Err(AppError::BadRequest(format!(
            "fallback model route `{}::{}` is not present in the live model catalogue",
            model.provider, model.deployment
        )));
    }
    match crate::routes::options::route_qualification(
        runtime,
        &model.provider,
        &model.deployment,
        required_capabilities,
        max_parallel,
        total_tokens,
    ) {
        Ok(true) => {}
        Ok(false) => {
            return Err(AppError::BadRequest(format!(
                "fallback route `{runtime} · {}::{}` lacks atomic qualification for capabilities: {}",
                model.provider,
                model.deployment,
                required_capabilities
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        Err(error) => {
            return Err(AppError::Upstream(format!(
                "route qualification configuration error: {error}"
            )));
        }
    }
    let route = crate::routes::options::route_label(runtime, &model.provider, &model.deployment);
    for server in &blueprint.mcp_servers {
        let option = options
            .mcp_servers
            .iter()
            .find(|option| option.name == *server)
            .ok_or_else(|| {
                AppError::BadRequest(format!(
                    "MCP server `{server}` is not present in the live options catalogue"
                ))
            })?;
        if !crate::routes::options::mcp_server_qualified_for_route(
            runtime,
            &model.provider,
            &model.deployment,
            option,
        )
        .map_err(|error| {
            AppError::Upstream(format!(
                "resource qualification configuration error: {error}"
            ))
        })? {
            return Err(AppError::BadRequest(format!(
                "MCP server `{server}` lacks current resource qualification for fallback {route}"
            )));
        }
    }
    if let Some(memory) = blueprint
        .memory
        .as_deref()
        .filter(|memory| !memory.is_empty())
    {
        let option = options
            .memories
            .iter()
            .find(|option| option.name == memory)
            .ok_or_else(|| {
                AppError::BadRequest(format!(
                    "memory `{memory}` is not present in the live options catalogue"
                ))
            })?;
        if !crate::routes::options::memory_binding_qualified_for_route(
            runtime,
            &model.provider,
            &model.deployment,
            option,
        )
        .map_err(|error| {
            AppError::Upstream(format!(
                "resource qualification configuration error: {error}"
            ))
        })? {
            return Err(AppError::BadRequest(format!(
                "memory `{memory}` lacks current resource qualification for fallback {route}"
            )));
        }
    }
    for skill in &blueprint.skills {
        let option = options
            .skills
            .iter()
            .find(|option| option.name == *skill)
            .ok_or_else(|| {
                AppError::BadRequest(format!(
                    "skill `{skill}` is not present in the approved live catalogue"
                ))
            })?;
        if !crate::routes::options::skill_version_qualified_for_route(
            runtime,
            &model.provider,
            &model.deployment,
            option,
        )
        .map_err(|error| {
            AppError::Upstream(format!(
                "resource qualification configuration error: {error}"
            ))
        })? {
            return Err(AppError::BadRequest(format!(
                "skill `{skill}` lacks current version qualification for fallback {route}"
            )));
        }
    }
    Ok(())
}

/// `POST /api/namespaces/:ns/tasks` — create a task.
///
/// The BFF never sets status — it submits the spec and lets the controller
/// validate the envelope and stamp the digest. Admission (CEL) rejects an
/// amplifying envelope here, which we surface as a 422-style upstream error.
pub async fn create_task(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(ns): Path<String>,
    Json(mut req): Json<CreateTaskRequest>,
) -> AppResult<Json<TaskDetailDto>> {
    let cluster = require_cluster(&state)?;
    cluster.credential_grant(&ns).await.map_err(map_kube_err)?;
    let api: Api<KarsTask> = cluster.tasks(&ns);
    // The caller cannot choose attribution; it is derived from the verified
    // Bridge session inserted by auth middleware.
    req.created_by = Some(principal.name.clone());
    let created_by = principal.name.clone();
    if let Some(plan) = req
        .blueprint
        .as_ref()
        .and_then(|blueprint| blueprint.execution_plan.as_ref())
    {
        crate::routes::compose::validate_execution_plan(plan).map_err(AppError::BadRequest)?;
        req.envelope.delegation_depth = 1;
    } else if let Some(delegation) = req.delegation.as_ref() {
        crate::routes::compose::validate_delegation(delegation).map_err(AppError::BadRequest)?;
        req.envelope.delegation_depth = i32::from(delegation.mode == "principal-specialists");
    }
    let mut harness_correction: Option<String> = None;
    if let Some(blueprint) = req.blueprint.as_mut()
        && let Some(runtime) = blueprint.runtime.as_deref()
        && crate::routes::compose::is_non_autonomous_harness(runtime)
    {
        harness_correction = Some(format!(
            "harness {runtime} is a bootstrap-only adapter (no autonomous task loop) and cannot run a one-shot mission; corrected to OpenClaw"
        ));
        blueprint.runtime = Some("OpenClaw".to_string());
    }
    let git_write = crate::routes::github::authorize_git_write(
        cluster,
        &ns,
        &principal,
        req.git_write_repos.as_deref(),
    )
    .await?;
    if req.blueprint.as_ref().is_some_and(|blueprint| {
        blueprint.runtime.as_deref().unwrap_or("OpenClaw") != "OpenClaw"
            && !blueprint.skills.is_empty()
    }) {
        return Err(AppError::BadRequest(
            "controller-mounted file skills are currently supported only by OpenClaw".into(),
        ));
    }
    if req
        .blueprint
        .as_ref()
        .and_then(|blueprint| blueprint.tool_policy.as_deref())
        == Some("kars-team-member")
    {
        return Err(AppError::BadRequest(
            "kars-team-member is reserved for declared standing-team specialists".into(),
        ));
    }
    if let Some(blueprint) = req.blueprint.as_mut() {
        if blueprint.model_fallbacks.len() > 8 {
            return Err(AppError::BadRequest(
                "model_fallbacks may contain at most 8 routes".into(),
            ));
        }
        if blueprint.model.is_none() {
            let options = crate::routes::options::build_options(cluster).await?;
            blueprint.model = options
                .models
                .iter()
                .find(|model| model.is_default)
                .or_else(|| options.models.first())
                .map(|model| ModelDto {
                    provider: model.provider.clone(),
                    deployment: model.deployment.clone(),
                });
        }
    }
    if let Some(blueprint) = req.blueprint.as_ref()
        && let Some(model) = blueprint.model.as_ref()
    {
        let options = crate::routes::options::build_options(cluster).await?;
        let served = options.models.iter().any(|option| {
            option.provider == model.provider && option.deployment == model.deployment
        });
        if !served {
            return Err(AppError::BadRequest(format!(
                "model route `{}::{}` is not present in the live model catalogue",
                model.provider, model.deployment
            )));
        }
        let runtime = blueprint.runtime.as_deref().unwrap_or("OpenClaw");
        if !cluster.runnable_runtimes().await.contains(runtime) {
            return Err(AppError::BadRequest(format!(
                "runtime `{runtime}` cannot start on this cluster"
            )));
        }
        let (required_capabilities, max_parallel) =
            crate::routes::validate::qualification_requirements(blueprint, None);
        let total_tokens = req
            .envelope
            .budget
            .as_ref()
            .and_then(|budget| budget.tokens);
        match crate::routes::options::route_qualification(
            runtime,
            &model.provider,
            &model.deployment,
            &required_capabilities,
            max_parallel,
            total_tokens,
        ) {
            Ok(true) => {}
            Ok(false) => {
                return Err(AppError::BadRequest(format!(
                    "runtime/model route `{runtime} · {}::{}` lacks qualification evidence for capabilities: {}",
                    model.provider,
                    model.deployment,
                    required_capabilities
                        .iter()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
            Err(error) => {
                return Err(AppError::Upstream(format!(
                    "route qualification configuration error: {error}"
                )));
            }
        }
        fn find_resource<'a>(
            items: &'a [crate::routes::options::RefOption],
            name: &str,
        ) -> Option<&'a crate::routes::options::RefOption> {
            items.iter().find(|option| option.name == name)
        }
        for server in &blueprint.mcp_servers {
            let Some(option) = find_resource(&options.mcp_servers, server) else {
                return Err(AppError::BadRequest(format!(
                    "MCP server `{server}` is not present in the live options catalogue"
                )));
            };
            match crate::routes::options::mcp_server_qualified_for_route(
                runtime,
                &model.provider,
                &model.deployment,
                option,
            ) {
                Ok(true) => {}
                Ok(false) => {
                    return Err(AppError::BadRequest(format!(
                        "MCP server `{server}` lacks retained resource qualification for {} at current schema {}",
                        crate::routes::options::route_label(
                            runtime,
                            &model.provider,
                            &model.deployment
                        ),
                        option.tool_schema_digest.as_deref().unwrap_or("missing")
                    )));
                }
                Err(error) => {
                    return Err(AppError::Upstream(format!(
                        "resource qualification configuration error: {error}"
                    )));
                }
            }
        }
        if let Some(memory) = blueprint
            .memory
            .as_deref()
            .filter(|memory| !memory.is_empty())
        {
            let Some(option) = find_resource(&options.memories, memory) else {
                return Err(AppError::BadRequest(format!(
                    "memory `{memory}` is not present in the live options catalogue"
                )));
            };
            match crate::routes::options::memory_binding_qualified_for_route(
                runtime,
                &model.provider,
                &model.deployment,
                option,
            ) {
                Ok(true) => {}
                Ok(false) => {
                    return Err(AppError::BadRequest(format!(
                        "memory `{memory}` lacks retained resource qualification for {} at backend {} / compiled digest {}",
                        crate::routes::options::route_label(
                            runtime,
                            &model.provider,
                            &model.deployment
                        ),
                        option.backend.as_deref().unwrap_or("missing"),
                        option.compiled_digest.as_deref().unwrap_or("missing"),
                    )));
                }
                Err(error) => {
                    return Err(AppError::Upstream(format!(
                        "resource qualification configuration error: {error}"
                    )));
                }
            }
        }
        for skill in &blueprint.skills {
            let Some(option) = find_resource(&options.skills, skill) else {
                return Err(AppError::BadRequest(format!(
                    "skill `{skill}` is not present in the approved live catalogue"
                )));
            };
            match crate::routes::options::skill_version_qualified_for_route(
                runtime,
                &model.provider,
                &model.deployment,
                option,
            ) {
                Ok(true) => {}
                Ok(false) => {
                    return Err(AppError::BadRequest(format!(
                        "skill `{skill}` lacks retained resource qualification for {} at version digest {}",
                        crate::routes::options::route_label(
                            runtime,
                            &model.provider,
                            &model.deployment
                        ),
                        option.version_digest.as_deref().unwrap_or("missing")
                    )));
                }
                Err(error) => {
                    return Err(AppError::Upstream(format!(
                        "resource qualification configuration error: {error}"
                    )));
                }
            }
        }
        let mut seen = std::collections::BTreeSet::new();
        for fallback in &blueprint.model_fallbacks {
            let key = format!("{}::{}", fallback.provider, fallback.deployment);
            if key == format!("{}::{}", model.provider, model.deployment) || !seen.insert(key) {
                continue;
            }
            validate_mission_fallback_route(
                &options,
                blueprint,
                runtime,
                fallback,
                &required_capabilities,
                max_parallel,
                total_tokens,
            )?;
        }
    }

    // Aggregate inference-budget gate (cluster + workspace + user). A launched
    // mission consumes inference tokens, so a strict/over-buffer budget at any
    // tier blocks starting new work. Draft (unlaunched) missions don't run yet,
    // so they pass — the gate re-applies when they run.
    if req.launch {
        crate::routes::budgets::enforce_launch_budget(cluster, &ns, &created_by).await?;
    }

    // Default the tool policy to `kars-default` when neither the request envelope
    // nor the blueprint pins one. This is not cosmetic: the AGT mesh transport the
    // run's delivery rides on requires a mounted ToolPolicy. With governance OFF
    // the sandbox mounts no policy, the AGT engine fails closed, and the agent can
    // never send its `task_response` back to the controller — the run streams live
    // but NEVER delivers (no output ConfigMap, endless re-dispatch). Every bridge
    // mission must be governed; `kars-default` is the cluster's baseline policy.
    // An explicit blueprint tool policy still wins (governance_spec prefers it), so
    // we only inject the default when the blueprint carries none.
    let blueprint_has_tool_policy = req
        .blueprint
        .as_ref()
        .and_then(|b| b.tool_policy.as_ref())
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    let tool_policy_ref = req
        .envelope
        .tool_policy
        .clone()
        .filter(|s| !s.is_empty())
        .or_else(|| (!blueprint_has_tool_policy).then(|| "kars-default".to_string()))
        .map(|name| LocalObjectRef { name });

    // ── Hard capability match, defense-in-depth ─────────────────────────────
    // A direct mission is one-shot autonomous; a bootstrap-only adapter has no
    // task-execution loop and delivers nothing. The compose flow already
    // corrects this, but a manually-edited package could still name one — so
    // enforce it again at creation: rewrite the harness to OpenClaw and record
    // the correction as a governance annotation on the task so it survives into
    // the run and the receipt/decision view. (Hermes/BYO are autonomous — kept.)
    let mut blueprint = req.blueprint.map(BlueprintDto::into_crd);
    if let Some((git_write, binding)) = git_write {
        let blueprint = blueprint.get_or_insert_with(Default::default);
        blueprint.git_write = Some(git_write);
        blueprint.github_binding = Some(binding);
    }
    let spec = KarsTaskSpec {
        objective: req.objective,
        display_name: req.display_name,
        execution: req.launch.then_some(crate::kars::task::TaskExecution {
            launch: true,
            runtime: None,
        }),
        blueprint,
        parent_ref: req
            .parent
            .filter(|s| !s.is_empty())
            .map(|name| LocalObjectRef { name }),
        envelope: TaskEnvelope {
            tier: req.envelope.tier,
            authority_ceiling: req.envelope.authority_ceiling,
            delegation_depth: req.envelope.delegation_depth,
            budget: req.envelope.budget.map(|b| TaskBudget {
                scope: b.scope,
                tokens: b.tokens,
                usd_micros: b.usd_micros,
            }),
            tool_policy_ref,
            egress_allowlist_ref: req
                .envelope
                .egress_allowlist
                .filter(|s| !s.is_empty())
                .map(|name| LocalObjectRef { name }),
        },
        retention_ttl_seconds: req.retention_ttl_seconds,
    };
    let mut task = KarsTask::new(&req.name, spec);
    if let Some(plan) = task
        .spec
        .blueprint
        .as_ref()
        .and_then(|blueprint| blueprint.execution_plan.as_ref())
    {
        let total_tokens = task
            .spec
            .envelope
            .budget
            .as_ref()
            .and_then(|budget| budget.tokens)
            .ok_or_else(|| {
                AppError::BadRequest(
                    "execution-plan missions require an explicit total token budget".into(),
                )
            })?;
        let (principal_tokens, child_tokens) =
            crate::routes::compose::delegation_budget_allocation(total_tokens, plan.roles.len())
                .map_err(AppError::BadRequest)?;
        let annotations = task
            .metadata
            .annotations
            .get_or_insert_with(Default::default);
        annotations.insert(
            "kars.azure.com/mission-budget-total".into(),
            total_tokens.to_string(),
        );
        annotations.insert(
            "kars.azure.com/mission-principal-budget".into(),
            principal_tokens.to_string(),
        );
        annotations.insert(
            "kars.azure.com/mission-child-budget".into(),
            child_tokens.to_string(),
        );
        annotations.insert(
            "kars.azure.com/mission-specialist-count".into(),
            plan.roles.len().to_string(),
        );
        annotations.insert(
            "kars.azure.com/mission-decomposition".into(),
            "execution-plan/v1".into(),
        );
    }
    // Record the capability correction on the task so it's durable and surfaces in
    // the governed record (the run reads task annotations; the receipt/decision
    // view can attest the harness was corrected rather than silently swapped).
    if let Some(reason) = &harness_correction {
        task.metadata
            .annotations
            .get_or_insert_with(Default::default)
            .insert(
                "kars.azure.com/harness-corrected".to_string(),
                reason.clone(),
            );
    }
    // Stamp the creator for per-user budget attribution.
    task.metadata
        .annotations
        .get_or_insert_with(Default::default)
        .insert("kars.azure.com/created-by".to_string(), created_by.clone());
    let annotations = task
        .metadata
        .annotations
        .get_or_insert_with(Default::default);
    annotations.insert(
        "kars.azure.com/owner-sub".to_string(),
        principal.sub.clone(),
    );
    annotations.insert(
        "kars.azure.com/owner-name".to_string(),
        principal.name.clone(),
    );
    let launch = task
        .spec
        .execution
        .as_ref()
        .is_some_and(|execution| execution.launch);
    if let Some(execution) = task.spec.execution.as_mut() {
        execution.launch = false;
    }
    let created = api
        .create(&PostParams::default(), &task)
        .await
        .map_err(map_kube_err)?;
    cluster
        .finish_created_credentials(
            &crate::kars::credentials::Target {
                kind: "KarsTask".into(),
                namespace: ns.clone(),
                name: created.name_any(),
                uid: created
                    .uid()
                    .ok_or_else(|| AppError::Upstream("Task CREATE omitted UID".into()))?,
            },
            launch,
        )
        .await
        .map_err(map_kube_err)?;
    let created = api.get(&created.name_any()).await.map_err(map_kube_err)?;
    Ok(Json(to_detail(
        &created,
        Vec::new(),
        Vec::new(),
        None,
        None,
        Vec::new(),
        Vec::new(),
        Vec::new(),
        None,
        None,
        None,
        None,
    )))
}

/// Per-mission promote request body.
#[derive(Debug, Deserialize)]
pub struct PromoteMissionRequest {
    pub tier: i32,
}

/// `POST /api/namespaces/:ns/tasks/:name/promote` — request a per-mission tier
/// promotion (§12). Patches `spec.requestedTier`; the controller opens a human
/// `KarsApproval` and widens the envelope only once approved. The BFF never
/// widens an envelope directly.
pub async fn promote_task(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
    Json(body): Json<PromoteMissionRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    require_owned_task(cluster, &ns, &name, &principal).await?;
    if !(1..=5).contains(&body.tier) {
        return Err(AppError::BadRequest("tier must be in 1..5".into()));
    }
    let api: Api<KarsTask> = cluster.tasks(&ns);
    let patch = serde_json::json!({ "spec": { "requestedTier": body.tier } });
    api.patch(
        &name,
        &kube::api::PatchParams::default(),
        &kube::api::Patch::Merge(patch),
    )
    .await
    .map_err(map_kube_err)?;
    Ok(Json(serde_json::json!({
        "requested": true,
        "tier": body.tier,
        "note": "A human approval has been opened. This mission is promoted only once it is approved."
    })))
}

/// Governed emergency-stop request.
#[derive(Debug, Deserialize)]
pub struct HaltRequest {
    /// Why the operator is halting — recorded on the governed decision so the
    /// stop is attestable ("who halted this, when, and why"), not anonymous.
    pub reason: Option<String>,
}

/// `POST /api/namespaces/:ns/tasks/:name/halt` — governed emergency-stop.
///
/// A one-click halt that STOPS a running mission/agent without destroying its
/// record: it flips `spec.execution.launch` to false (the controller's teardown
/// reconcile then deletes the sandbox + InferencePolicy, so the agent is removed
/// from the mesh and can no longer receive or answer delegated work) and stamps
/// a governed decision annotation (`kars.azure.com/halted` = operator/reason/at)
/// so the halt itself is a durable, attestable record. The deliverable, trace,
/// and receipt remain — unlike DELETE, which removes everything. No major agent
/// platform ships a governed kill; kars can, because it owns the K8s control
/// plane (to stop) and the governance record (to attest).
pub async fn halt_task(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
    Json(body): Json<HaltRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    let api: Api<KarsTask> = cluster.tasks(&ns);
    require_owned_task(cluster, &ns, &name, &principal).await?;
    let reason = body
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("operator emergency-stop");
    let at = chrono::Utc::now().to_rfc3339();
    let decision = format!("halted by operator at {at}: {reason}");
    // Un-launch (controller tears down the running sandbox) AND record the
    // governed decision atomically in one merge patch.
    let patch = serde_json::json!({
        "metadata": { "annotations": { "kars.azure.com/halted": decision } },
        "spec": { "execution": { "launch": false } },
    });
    api.patch(
        &name,
        &kube::api::PatchParams::default(),
        &kube::api::Patch::Merge(patch),
    )
    .await
    .map_err(map_kube_err)?;
    Ok(Json(serde_json::json!({
        "halted": true,
        "at": at,
        "reason": reason,
        "note": "The agent's sandbox is being torn down; the mission record, deliverable, and audit trail are retained. The halt is recorded as a governed decision.",
    })))
}

/// Replicate request — how many identical runs to launch for reliability (pass^k).
#[derive(Debug, Deserialize)]
pub struct ReplicateRequest {
    /// Number of additional identical runs to create (2–5). Each becomes a
    /// distinct KarsTask sharing this task's exact objective + envelope, so the
    /// efficiency frontier can compute pass^k reliability across them.
    pub count: u32,
    /// When true, each clone is launched immediately; when false, they are
    /// created as ready-to-run packages the caller launches. Default true.
    #[serde(default = "default_true")]
    pub launch: bool,
}

fn default_true() -> bool {
    true
}

/// `POST /api/namespaces/:ns/tasks/:name/replicate` — the pass^k runner.
///
/// Clones a mission's EXACT package (objective + envelope + blueprint) into
/// `count` distinct sibling tasks so they run independently and the efficiency
/// engine can measure pass^k reliability (fraction of the repeated package
/// accepted on EVERY attempt). Honest: this creates real, governed runs — the
/// same package, nothing weakened — not a simulated repeat.
pub async fn replicate_task(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
    Json(req): Json<ReplicateRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    let count = req.count.clamp(1, 5);
    let api: Api<KarsTask> = cluster.tasks(&ns);
    let source = require_owned_task(cluster, &ns, &name, &principal).await?;
    if source
        .spec
        .blueprint
        .as_ref()
        .and_then(|blueprint| blueprint.credential_bindings.as_ref())
        .is_some_and(|bindings| {
            bindings
                .sources
                .iter()
                .any(|source| source.scope != "workspace")
        })
    {
        return Err(AppError::BadRequest("Independent replicas cannot inherit another target's credential UID; use an approved workspace source or stage per-replica credentials".into()));
    }

    // A short suffix keyed off the current time keeps clone names unique across
    // repeated replicate calls (so a second batch doesn't collide with a first).
    let batch = chrono::Utc::now().timestamp() % 100000;
    let mut created: Vec<String> = Vec::new();
    for i in 1..=count {
        let clone_name = format!("{name}-rep-{batch}-{i}");
        let mut spec = source.spec.clone();
        // Force the execution gate to the requested launch state; strip parent
        // linkage so each clone is an independent, top-level run.
        spec.execution = Some(crate::kars::task::TaskExecution {
            launch: false,
            runtime: None,
        });
        spec.parent_ref = None;
        let mut task = KarsTask::new(&clone_name, spec);
        // Label the batch so the UI can group a reliability cohort together.
        task.metadata
            .labels
            .get_or_insert_with(Default::default)
            .insert("kars.azure.com/reliability-of".into(), name.clone());
        let annotations = task
            .metadata
            .annotations
            .get_or_insert_with(Default::default);
        annotations.insert("kars.azure.com/owner-sub".into(), principal.sub.clone());
        annotations.insert("kars.azure.com/owner-name".into(), principal.name.clone());
        let captured = api
            .create(&PostParams::default(), &task)
            .await
            .map_err(map_kube_err)?;
        cluster
            .finish_created_credentials(
                &crate::kars::credentials::Target {
                    kind: "KarsTask".into(),
                    namespace: ns.clone(),
                    name: captured.name_any(),
                    uid: captured
                        .uid()
                        .ok_or_else(|| AppError::Upstream("Replica CREATE omitted UID".into()))?,
                },
                req.launch,
            )
            .await
            .map_err(map_kube_err)?;
        created.push(clone_name);
    }

    Ok(Json(serde_json::json!({
        "replicated": name,
        "count": created.len(),
        "runs": created,
        "note": format!("{} identical runs created — pass^{} reliability will appear on the efficiency frontier once they complete and are reviewed.", created.len(), created.len() + 1),
    })))
}

/// Launch/un-launch request body.
#[derive(Debug, Deserialize)]
pub struct LaunchRequest {
    pub launch: bool,
}

/// `POST /api/namespaces/:ns/tasks/:name/launch` — flip the execution gate.
///
/// The §20 launch action: setting `launch: true` asks the controller to
/// materialize a governed sandbox; `false` tears it down. The BFF only patches
/// the spec — the controller does the materialization and reports status.
pub async fn launch_task(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
    Json(req): Json<LaunchRequest>,
) -> AppResult<Json<TaskDetailDto>> {
    let cluster = require_cluster(&state)?;
    require_owned_task(cluster, &ns, &name, &principal).await?;
    let api: Api<KarsTask> = cluster.tasks(&ns);
    let patch = serde_json::json!({
        "apiVersion": "kars.azure.com/v1alpha1",
        "kind": "KarsTask",
        "spec": { "execution": { "launch": req.launch } },
    });
    let patched = api
        .patch(
            &name,
            &kube::api::PatchParams::default(),
            &kube::api::Patch::Merge(&patch),
        )
        .await
        .map_err(map_kube_err)?;
    Ok(Json(to_detail(
        &patched,
        Vec::new(),
        Vec::new(),
        None,
        None,
        Vec::new(),
        Vec::new(),
        Vec::new(),
        None,
        None,
        None,
        None,
    )))
}

#[derive(Debug, Deserialize)]
pub struct IncreaseTaskBudgetRequest {
    pub daily_tokens: i64,
}

/// Request an owned Mission's token-budget increase. Bridge never widens the
/// trust envelope directly; the controller opens a typed human approval and is
/// the sole writer of the new ceiling after approval.
pub async fn increase_task_budget(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
    Json(req): Json<IncreaseTaskBudgetRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    let task = require_owned_task(cluster, &ns, &name, &principal).await?;
    let current = task
        .spec
        .envelope
        .budget
        .as_ref()
        .and_then(|budget| budget.tokens)
        .unwrap_or(0);
    if req.daily_tokens <= current {
        return Err(AppError::BadRequest(format!(
            "new daily token budget must be greater than the current {current}"
        )));
    }

    let tasks: Api<KarsTask> = cluster.tasks(&ns);
    let request_id = chrono::Utc::now().timestamp_micros().to_string();
    let patched = tasks
        .patch(
            &name,
            &kube::api::PatchParams::default(),
            &kube::api::Patch::Merge(&serde_json::json!({
                "metadata": {
                    "annotations": {
                        "kars.azure.com/requested-by": principal.name,
                        "kars.azure.com/requested-by-sub": principal.sub,
                        "kars.azure.com/budget-request-id": request_id
                    }
                },
                "spec": {
                    "requestedBudgetTokens": req.daily_tokens
                }
            })),
        )
        .await
        .map_err(map_kube_err)?;

    Ok(Json(serde_json::json!({
        "requested": true,
        "name": name,
        "budget_tokens": req.daily_tokens,
        "resource_version": patched.metadata.resource_version,
        "note": "A typed human approval is being opened. The controller widens the budget only after approval."
    })))
}

/// Body for a temporary egress request from a mission: the agent (or operator
/// on its behalf) asks to reach an extra website. Materialized as an
/// `EgressApproval` the controller reconciles through human approval — the BFF
/// never widens the sandbox's allowlist directly.
#[derive(Debug, Deserialize)]
pub struct EgressRequest {
    pub host: String,
    pub port: Option<u16>,
    pub reason: String,
    /// Time-to-live, e.g. "2h". Bounded by the cluster ceiling. Default "2h".
    pub ttl: Option<String>,
}

/// Normalize a human-friendly TTL (`"2h"`, `"30m"`, `"24h"`, `"1d"`, `"90s"`) to
/// the ISO-8601 duration the controller's `EgressApproval` reconciler requires
/// (`"PT2H"`, `"PT30M"`, `"P1D"`, `"PT90S"`). An already-ISO value (starts with
/// `P`) passes through uppercased. Unrecognized input falls back to `"PT2H"`
/// rather than emitting an invalid TTL that leaves the grant Pending forever.
fn normalize_ttl(raw: &str) -> String {
    let t = raw.trim();
    if t.is_empty() {
        return "PT2H".into();
    }
    if t.starts_with('P') || t.starts_with('p') {
        return t.to_ascii_uppercase();
    }
    let split = t.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(t.len());
    let (num, unit) = t.split_at(split);
    let n: u64 = num.trim().parse().unwrap_or(0);
    if n == 0 {
        return "PT2H".into();
    }
    match unit.trim().to_ascii_lowercase().as_str() {
        "s" | "sec" | "secs" => format!("PT{n}S"),
        "m" | "min" | "mins" => format!("PT{n}M"),
        "h" | "hr" | "hrs" | "hour" | "hours" => format!("PT{n}H"),
        "d" | "day" | "days" => format!("P{n}D"),
        _ => "PT2H".into(),
    }
}

/// `POST /api/namespaces/:ns/tasks/:name/egress` — file a temporary egress
/// grant request for this mission's sandbox. Returns the created EgressApproval
/// name; it widens nothing until a human approves it.
pub async fn request_egress(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
    Json(req): Json<EgressRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    let host = req.host.trim().to_string();
    if host.is_empty() {
        return Err(AppError::BadRequest("host is required".into()));
    }
    if req.reason.trim().len() < 3 {
        return Err(AppError::BadRequest("reason is required".into()));
    }
    let task = require_owned_task(cluster, &ns, &name, &principal).await?;
    task.status
        .as_ref()
        .and_then(|s| s.sandbox_ref.as_ref())
        .ok_or_else(|| AppError::BadRequest("mission has no running sandbox to widen".into()))?;
    let port = req.port.unwrap_or(443);
    let ttl = normalize_ttl(req.ttl.as_deref().unwrap_or("2h"));
    use sha2::{Digest, Sha256};
    let suffix = hex::encode(Sha256::digest(format!("{host}:{port}").as_bytes()));
    let approval_name = format!("{name}-eg-{}", &suffix[..12]);
    let task_uid = task
        .metadata
        .uid
        .clone()
        .ok_or_else(|| AppError::Upstream("task has no Kubernetes UID".into()))?;
    let body = serde_json::json!({
        "apiVersion": "kars.azure.com/v1alpha1",
        "kind": "KarsApproval",
        "metadata": {
            "name": approval_name,
            "namespace": ns,
            "ownerReferences": [{
                "apiVersion": "kars.azure.com/v1alpha1",
                "kind": "KarsTask",
                "name": name,
                "uid": task_uid,
                "controller": true,
                "blockOwnerDeletion": true
            }],
            "labels": {
                "kars.azure.com/req-task": name,
                "kars.azure.com/req-kind": "egress"
            },
            "annotations": {
                "kars.azure.com/req-kind": "egress",
                "kars.azure.com/req-target": host,
                "kars.azure.com/req-port": port.to_string(),
                "kars.azure.com/req-ttl": ttl,
                "kars.azure.com/requested-by": principal.name,
                "kars.azure.com/requested-by-sub": principal.sub,
                "kars.azure.com/owner-sub": principal.sub,
                "kars.azure.com/owner-name": principal.name
            }
        },
        "spec": {
            "taskRef": {"name": name},
            "requestedBy": {
                "subject": principal.sub,
                "name": principal.name
            },
            "action": {
                "kind": "egress",
                "summary": format!("Allow the mission to reach {host}:{port}"),
                "detail": format!(
                    "{} Approving creates an exact, time-boxed {host}:{port} grant.",
                    req.reason.trim()
                )
            },
            "ttl": "PT24H"
        },
    });
    let created = cluster
        .apply_kind(&ns, "KarsApproval", body, false)
        .await
        .map_err(map_kube_err)?;
    Ok(Json(serde_json::json!({
        "requested": true,
        "name": created.metadata.name,
        "note": "Pending human approval. No egress is granted until a distinct operator approves it in the Bridge inbox."
    })))
}

/// `GET /api/namespaces/:ns/tasks/:name/egress/learned` — the domains the agent
/// has actually reached, observed by the router in Learn mode. This is the
/// evidence a customer reviews before promoting the mission to enforced.
pub async fn get_learned_egress(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    let task = require_owned_task(cluster, &ns, &name, &principal).await?;
    let sandbox = task
        .status
        .as_ref()
        .and_then(|s| s.sandbox_ref.as_ref())
        .map(|r| r.name.clone());
    let Some(sandbox) = sandbox else {
        return Ok(Json(
            serde_json::json!({ "available": false, "reason": "no running sandbox yet", "domains": [] }),
        ));
    };
    let mode = cluster
        .sandbox_egress_mode(&sandbox)
        .await
        .unwrap_or_else(|| "Learn".into());
    let enforced = cluster.sandbox_allowlist(&sandbox).await;
    match cluster.sandbox_learned_domains(&sandbox).await {
        Ok(domains) => Ok(Json(
            serde_json::json!({ "available": true, "mode": mode, "domains": domains, "enforced": enforced }),
        )),
        Err(e) => Ok(Json(
            serde_json::json!({ "available": false, "mode": mode, "reason": e.to_string(), "domains": [], "enforced": enforced }),
        )),
    }
}

/// Body for flipping a mission's egress enforcement mode.
#[derive(Debug, Deserialize)]
pub struct EgressModeRequest {
    /// `"learning"` (clear the allowlist → controller runs Learn) or
    /// `"enforced"` (pin the allowlist → controller runs Strict).
    pub mode: String,
    /// The hosts to enforce when `mode == "enforced"`. Typically the reviewed
    /// subset of the learned domains.
    #[serde(default)]
    pub allow: Vec<String>,
    /// When true, UNION `allow` with the mission's current enforced allowlist
    /// instead of replacing it — so granting one host (e.g. from a blocker) can
    /// never silently wipe previously-approved hosts. The NetworkMode panel,
    /// which sets the full list deliberately, leaves this false (replace).
    #[serde(default)]
    pub merge: bool,
}

/// `POST /api/namespaces/:ns/tasks/:name/egress-mode` — promote a mission from
/// learning (monitoring) to enforced, or back. This drives the REAL lever: the
/// controller derives `egressMode: Strict` + an allowlist when the blueprint
/// names egress hosts, and `Learn` when it is empty. Operator-gated; the
/// controller re-reconciles the sandbox, so this is durable, not a UI toggle.
pub async fn set_egress_mode(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
    Json(req): Json<EgressModeRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    let task = require_owned_task(cluster, &ns, &name, &principal).await?;
    let enforced = match req.mode.as_str() {
        "enforced" | "strict" => true,
        "learning" | "learn" => false,
        _ => {
            return Err(AppError::BadRequest(
                "mode must be 'enforced' or 'learning'".into(),
            ));
        }
    };
    // Parse "host" or "host:port" into the blueprint egress shape.
    let egress: Vec<serde_json::Value> = if enforced {
        let mut parsed: Vec<serde_json::Value> = req
            .allow
            .iter()
            .filter_map(|h| {
                let h = h.trim();
                if h.is_empty() {
                    return None;
                }
                match h
                    .rsplit_once(':')
                    .and_then(|(host, p)| p.parse::<u16>().ok().map(|p| (host, p)))
                {
                    Some((host, port)) => Some(serde_json::json!({ "host": host, "port": port })),
                    None => Some(serde_json::json!({ "host": h })),
                }
            })
            .collect();
        // Additive grant: union with the mission's CURRENT enforced allowlist so
        // approving one host never clobbers the others (a k8s merge-patch of an
        // array replaces it wholesale, so we must merge here, before patching).
        if req.merge {
            let existing: Vec<serde_json::Value> = task
                .spec
                .blueprint
                .map(|b| b.egress)
                .unwrap_or_default()
                .into_iter()
                .map(|e| match e.port {
                    Some(p) => serde_json::json!({ "host": e.host, "port": p }),
                    None => serde_json::json!({ "host": e.host }),
                })
                .collect();
            let key = |v: &serde_json::Value| {
                format!(
                    "{}:{}",
                    v.get("host").and_then(|h| h.as_str()).unwrap_or(""),
                    v.get("port").and_then(|p| p.as_u64()).unwrap_or(0)
                )
            };
            let mut seen: std::collections::HashSet<String> = parsed.iter().map(key).collect();
            for e in existing {
                if seen.insert(key(&e)) {
                    parsed.push(e);
                }
            }
        }
        if parsed.is_empty() {
            return Err(AppError::BadRequest(
                "enforcing requires at least one allowed host — review the learned domains first"
                    .into(),
            ));
        }
        parsed
    } else {
        Vec::new()
    };
    // Patch the mission's blueprint egress; the controller compiles it into the
    // sandbox's networkPolicy (Strict + allowlist, or Learn when empty).
    let patch = serde_json::json!({ "spec": { "blueprint": { "egress": egress } } });
    cluster
        .tasks(&ns)
        .patch(
            &name,
            &kube::api::PatchParams::default(),
            &kube::api::Patch::Merge(patch),
        )
        .await
        .map_err(map_kube_err)?;
    Ok(Json(serde_json::json!({
        "updated": true,
        "mode": if enforced { "enforced" } else { "learning" },
        "note": if enforced {
            "Promoted to enforced — the sandbox will deny anything outside the approved allowlist on its next reconcile."
        } else {
            "Back to learning — the sandbox observes and records every domain it reaches without denying."
        }
    })))
}

/// One running agent + what it is doing now, for the lifecycle view.
#[derive(Debug, Serialize)]
pub struct AgentLifecycleDto {
    pub sandbox: String,
    pub namespace: String,
    pub phase: Option<String>,
    pub parent: Option<String>,
    /// The task this agent is executing (label-derived), if any.
    pub task: Option<String>,
    pub objective: Option<String>,
    pub tier: Option<i32>,
    /// Live activity counts from the persisted trace (rounds + tool calls).
    pub rounds: usize,
    pub tool_calls: usize,
    pub last_action: Option<String>,
    /// True when the agent's sandbox pod is still running (working now) vs a
    /// recently-completed run (its ephemeral sandbox already torn down).
    pub live: bool,
    /// Real token cost of the run (from the mission output), when known.
    pub tokens: Option<i64>,
    /// The run's token budget ceiling (from the envelope), when set — so the UI
    /// can render spend against limit ("spent / budget") rather than a bare
    /// number. `None` for an uncapped run.
    pub budget_tokens: Option<i64>,
    /// Run outcome: `ok` | `error` (from the mission output), when finished.
    pub status: Option<String>,
    /// When the run delivered (from the mission output).
    pub finished_at: Option<String>,
    /// Owning standing team, if this is a team run.
    pub team: Option<String>,
    /// Human label for the run.
    pub display_name: Option<String>,
    /// Live pod health (readiness, restarts, uptime, node) — only for live
    /// agents; `None` for finished runs whose sandbox was torn down.
    pub health: Option<crate::kars::cluster::PodHealth>,
}

/// Whether a `KarsTask` should surface on the "Active agents" fleet views. A
/// surfaceable run is either a team taskforce run, a `*-run-<ts>` scheduled run,
/// OR a launched direct mission (a one-off the user kicked off from `/new`).
/// Un-launched drafts and team structural tasks (a non-taskforce `team-role`)
/// are NOT agents yet, so they stay out. Without the direct-mission arm the
/// flagship "Active agents" page was empty for the single most common action —
/// launch a mission and watch it — because a direct mission carries neither the
/// taskforce role nor a `-run-` suffix.
fn is_surfaceable_run(t: &KarsTask) -> bool {
    let name = t.name_any();
    let role = t
        .annotations()
        .get("kars.azure.com/team-role")
        .map(String::as_str);
    if role == Some("taskforce") || name.contains("-run-") {
        return true;
    }
    // A launched direct mission: no team structural role, and it was launched.
    let launched = t.spec.execution.as_ref().map(|e| e.launch).unwrap_or(false);
    role.is_none() && launched
}

/// `GET /api/agents` — recent and live agent runs with their real work. Sources
/// from run KarsTasks + their persisted mission telemetry (not idle pods), so
/// the page answers "what have my agents been doing, and what's working now" —
/// live runs first, then recently completed. Ephemeral run sandboxes tear down
/// after delivering, so their work would otherwise vanish; here it persists.
pub async fn list_agents(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> AppResult<Json<Vec<AgentLifecycleDto>>> {
    let cluster = require_cluster(&state)?;
    let tasks_api = cluster.tasks("kars-system");
    let all = tasks_api
        .list(&kube::api::ListParams::default())
        .await
        .map_err(map_kube_err)?;

    // Runs only: a team-owned run, a `*-run-<ts>` task, or a launched direct
    // mission. Members/principals (structural team roles) are standing authority,
    // not work to surface here.
    let mut runs: Vec<&KarsTask> = all
        .items
        .iter()
        .filter(|task| is_surfaceable_run(task) && is_task_owner(task, &principal))
        .collect();
    // Freshest first.
    runs.sort_by(|a, b| {
        let ta = a.metadata.creation_timestamp.as_ref().map(|t| t.0);
        let tb = b.metadata.creation_timestamp.as_ref().map(|t| t.0);
        tb.cmp(&ta)
    });

    let mut out = Vec::new();
    for task in runs.into_iter().take(24) {
        let name = task.name_any();
        let team = task
            .metadata
            .labels
            .as_ref()
            .and_then(|l| l.get("kars.azure.com/team").cloned());

        // Mission output: tokens, status, finished, round/tool rollup.
        let output = cluster.read_mission_output(&name).await;
        let (tokens, status, finished_at, mut rounds, mut tool_calls) = match &output {
            Some(d) => (
                d.get("totalTokens").and_then(|v| v.parse::<i64>().ok()),
                d.get("status").cloned(),
                d.get("finishedAt").cloned(),
                d.get("rounds")
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(0),
                d.get("toolCalls")
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(0),
            ),
            None => (None, None, None, 0, 0),
        };

        // Live iff the run's sandbox pod is running AND it hasn't delivered a
        // terminal result yet. A DIRECT mission's sandbox lingers (Running) after
        // it delivers, so "Running pod" alone would mislabel a finished, idle
        // mission as live and inflate the fleet's "working now" count. A delivered
        // (or errored) run is Recent, not Live — its outcome and telemetry show in
        // the recent list. (Team run sandboxes tear down on delivery, so this is a
        // no-op for them.)
        let delivered = status.is_some();
        let live = !delivered && cluster.running_pod_for_sandbox(&name).await.is_some();
        // Honest pod health for a live agent (readiness/restarts/uptime/node).
        let health = if live {
            cluster.sandbox_pod_health(&name).await
        } else {
            None
        };

        // For a live run, the trace's last tool tells "what it's doing now";
        // also a more current round/tool count than the (post-hoc) output.
        let mut last_action = None;
        if live {
            // A live run has no persisted trace CM yet (it's written at delivery),
            // so fall back to the router's live trace — otherwise a working agent
            // reports 0 rounds / 0 tool calls / no current action.
            let trace: Vec<serde_json::Value> = match cluster
                .read_mission_trace(&name)
                .await
                .and_then(|raw| serde_json::from_str::<Vec<serde_json::Value>>(&raw).ok())
            {
                Some(t) if !t.is_empty() => t,
                _ => cluster.sandbox_live_trace(&name).await,
            };
            if !trace.is_empty() {
                let r = trace
                    .iter()
                    .filter(|e| e.get("kind").and_then(|k| k.as_str()) == Some("round"))
                    .count();
                let tc = trace
                    .iter()
                    .filter(|e| e.get("kind").and_then(|k| k.as_str()) == Some("tool"))
                    .count();
                if r > 0 {
                    rounds = r;
                }
                if tc > 0 {
                    tool_calls = tc;
                }
                last_action = trace
                    .last()
                    .and_then(|e| e.get("name").and_then(|n| n.as_str()).map(String::from));
            }
        }

        let phase = if live {
            Some("Running".into())
        } else if status.as_deref() == Some("ok") {
            Some("Delivered".into())
        } else if status.is_some() {
            Some("Errored".into())
        } else {
            Some("Idle".into())
        };

        out.push(AgentLifecycleDto {
            sandbox: name.clone(),
            namespace: task.namespace().unwrap_or_default(),
            phase,
            parent: task.spec.parent_ref.as_ref().map(|p| p.name.clone()),
            task: Some(name.clone()),
            objective: Some(clean_objective(&task.spec.objective)),
            tier: Some(task.spec.envelope.tier),
            rounds,
            tool_calls,
            last_action,
            live,
            tokens,
            budget_tokens: task.spec.envelope.budget.as_ref().and_then(|b| b.tokens),
            status,
            finished_at,
            team,
            display_name: clean_display_name(&task.spec.display_name, &task.spec.objective),
            health,
        });
    }

    // Live runs first, then most-recent finished.
    out.sort_by(|a, b| b.live.cmp(&a.live).then(b.finished_at.cmp(&a.finished_at)));
    Ok(Json(out))
}

// ─── Fleet live telemetry (at-scale "what's happening now") ──────────────────

#[derive(serde::Serialize)]
pub struct FleetActivityItem {
    /// The run/agent this event came from.
    pub agent: String,
    pub display_name: Option<String>,
    pub team: Option<String>,
    /// "tool" | "round".
    pub kind: String,
    /// For a tool event, the tool name; for a round, the finish reason.
    pub label: String,
    /// Optional short argument/host preview for a tool event.
    pub detail: Option<String>,
    /// Whether a tool event failed (ok=false) — surfaced in red.
    pub failed: bool,
    /// Round index the event belongs to.
    pub round: i64,
    /// Monotonic sequence within the run's trace (for stable ordering).
    pub seq: i64,
    /// Milliseconds the step took, when known.
    pub ms: Option<i64>,
}

#[derive(serde::Serialize)]
pub struct FleetTelemetryDto {
    /// Agents whose sandbox pod is running right now.
    pub working: usize,
    /// Distinct standing teams with a live run.
    pub teams_active: usize,
    /// Sub-agents (runs with a parent) currently live.
    pub sub_agents: usize,
    /// Live token burn summed across working agents (from their in-flight trace).
    pub tokens_in_flight: i64,
    /// Tool calls summed across working agents this run.
    pub tool_calls: i64,
    /// Model rounds summed across working agents this run.
    pub rounds: i64,
    /// The most recent activity across ALL live agents, newest first — a single
    /// chronological fleet feed of what every working agent is doing right now.
    pub feed: Vec<FleetActivityItem>,
}

/// `GET /api/agents/fleet` — aggregate LIVE telemetry across every working
/// agent, plus a single merged activity feed of what they're all doing right
/// now. This is the "at scale" view: instead of drilling into one mission, see
/// the whole fleet's live tool-by-tool work in one stream. Sourced from each
/// live run's real execution trace — never fabricated; an idle fleet returns
/// zeros and an empty feed.
pub async fn fleet_telemetry(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> AppResult<Json<FleetTelemetryDto>> {
    let cluster = require_cluster(&state)?;
    let tasks_api = cluster.tasks("kars-system");
    let all = tasks_api
        .list(&kube::api::ListParams::default())
        .await
        .map_err(map_kube_err)?;

    let runs: Vec<&KarsTask> = all
        .items
        .iter()
        .filter(|task| is_surfaceable_run(task) && is_task_owner(task, &principal))
        .collect();

    let mut working = 0usize;
    let mut teams: std::collections::BTreeSet<String> = Default::default();
    let mut sub_agents = 0usize;
    let mut tokens_in_flight = 0i64;
    let mut tool_calls = 0i64;
    let mut rounds = 0i64;
    let mut feed: Vec<FleetActivityItem> = Vec::new();

    for task in runs {
        let name = task.name_any();
        // Only agents that are actually running right now contribute trace.
        if cluster.running_pod_for_sandbox(&name).await.is_none() {
            continue;
        }
        // A delivered direct mission keeps a lingering Running pod but is idle —
        // its historical tokens are NOT "in flight". Skip it here so the live
        // counters reflect only work happening now (it still shows, with its
        // outcome, in the Recent runs list from /api/agents).
        if cluster
            .read_mission_output(&name)
            .await
            .and_then(|d| d.get("status").cloned())
            .is_some()
        {
            continue;
        }
        working += 1;
        for sub in cluster.sub_agent_sandbox_names("kars-system", &name).await {
            if cluster.running_pod_for_sandbox(&sub).await.is_some() {
                working += 1;
                sub_agents += 1;
            }
        }
        let team = task
            .metadata
            .labels
            .as_ref()
            .and_then(|l| l.get("kars.azure.com/team").cloned());
        if let Some(t) = &team {
            teams.insert(t.clone());
        }
        let display_name = clean_display_name(&task.spec.display_name, &task.spec.objective);

        // Prefer the persisted trace (delivered runs); for a LIVE run the trace
        // CM doesn't exist yet, so fall back to the router's live trace — else
        // every actively-working agent shows zero rounds/tokens/tools (the exact
        // opposite of "what's happening now"). Mirrors get_task's live fallback.
        let trace: Vec<serde_json::Value> = match cluster
            .read_mission_trace(&name)
            .await
            .and_then(|raw| serde_json::from_str::<Vec<serde_json::Value>>(&raw).ok())
        {
            Some(t) if !t.is_empty() => t,
            _ => cluster.sandbox_live_trace(&name).await,
        };
        if trace.is_empty() {
            continue;
        }

        for e in &trace {
            let kind = e.get("kind").and_then(|k| k.as_str()).unwrap_or("");
            let round = e.get("round").and_then(|v| v.as_i64()).unwrap_or(0);
            let seq = e.get("seq").and_then(|v| v.as_i64()).unwrap_or(0);
            let ms = e.get("ms").and_then(|v| v.as_i64());
            match kind {
                "round" => {
                    rounds += 1;
                    tokens_in_flight += e.get("total_tokens").and_then(|v| v.as_i64()).unwrap_or(0);
                    feed.push(FleetActivityItem {
                        agent: name.clone(),
                        display_name: display_name.clone(),
                        team: team.clone(),
                        kind: "round".into(),
                        label: e
                            .get("finish_reason")
                            .and_then(|v| v.as_str())
                            .filter(|s| !s.is_empty())
                            .unwrap_or("model round")
                            .to_string(),
                        detail: None,
                        failed: false,
                        round,
                        seq,
                        ms,
                    });
                }
                "tool" => {
                    tool_calls += 1;
                    let failed = e.get("ok").and_then(|v| v.as_bool()) == Some(false);
                    feed.push(FleetActivityItem {
                        agent: name.clone(),
                        display_name: display_name.clone(),
                        team: team.clone(),
                        kind: "tool".into(),
                        label: e
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("tool")
                            .to_string(),
                        detail: e
                            .get("args_preview")
                            .and_then(|v| v.as_str())
                            .filter(|s| !s.is_empty())
                            .map(|s| s.chars().take(80).collect()),
                        failed,
                        round,
                        seq,
                        ms,
                    });
                }
                _ => {}
            }
        }
    }

    // Newest activity first, capped so a busy fleet stays responsive.
    feed.sort_by(|a, b| b.round.cmp(&a.round).then(b.seq.cmp(&a.seq)));
    feed.truncate(40);

    Ok(Json(FleetTelemetryDto {
        working,
        teams_active: teams.len(),
        sub_agents,
        tokens_in_flight,
        tool_calls,
        rounds,
        feed,
    }))
}

#[cfg(test)]
mod tests {
    use super::BlueprintDto;
    use super::ExecutionPhaseDto;
    use super::ExecutionPlanDto;
    use super::ExecutionRoleDto;
    use super::ExecutionSynthesisDto;
    use super::MissionResultDto;
    use super::ModelDto;
    use super::TaskAssignmentEventDto;
    use super::TeamCollaborationEventDto;
    use super::canonicalize_assignment_event_roles;
    use super::clean_objective;
    use super::deliverable_pull_requests;
    use super::diagnose_run_failure;
    use super::merge_trace_total_tokens;
    use super::normalize_ttl;
    use super::select_task_checkpoint;
    use super::to_sub_agent;
    use super::{
        ARTIFACT_PREVIEW_MAX_BYTES, ARTIFACT_PREVIEW_TOTAL_BYTES, MissionArtifactDto,
        artifact_preview, structured_team_evidence, subagent_trace_from_artifacts,
        valid_task_checkpoint,
    };

    #[test]
    fn execution_plan_dto_into_crd_preserves_web_search_capability() {
        let blueprint = BlueprintDto {
            execution_plan: Some(ExecutionPlanDto {
                schema: "kars.execution-plan/v1".into(),
                roles: vec![ExecutionRoleDto {
                    name: "source-scout".into(),
                    objective: "Discover exact URLs and fetch the evidence.".into(),
                    depends_on: Vec::new(),
                    phases: vec![ExecutionPhaseDto {
                        name: "discover".into(),
                        objective: "Search and fetch the exact URLs.".into(),
                        capabilities: vec!["web-search".into(), "network".into()],
                        required_tool_calls: Vec::new(),
                        min_tool_calls: 1,
                        max_tool_calls: 4,
                        fresh_context: true,
                    }],
                    budget_tokens: None,
                }],
                max_parallel: 1,
                synthesis: ExecutionSynthesisDto {
                    objective: "Return the verified answer.".into(),
                    capabilities: Vec::new(),
                    max_tool_calls: 0,
                },
                deliverables: Vec::new(),
            }),
            ..Default::default()
        };

        let crd = blueprint.into_crd();
        assert_eq!(
            crd.execution_plan.expect("execution plan").roles[0].phases[0].capabilities,
            vec!["web-search".to_string(), "network".to_string()]
        );
    }

    #[test]
    fn blueprint_dto_preserves_ordered_model_fallbacks() {
        let blueprint = BlueprintDto {
            model: Some(ModelDto {
                provider: "local-inference".into(),
                deployment: "gpt-oss-120b".into(),
            }),
            model_fallbacks: vec![
                ModelDto {
                    provider: "github-copilot".into(),
                    deployment: "gpt-5.6-sol".into(),
                },
                ModelDto {
                    provider: "foundry".into(),
                    deployment: "gpt-5.4-pro".into(),
                },
            ],
            ..Default::default()
        };

        let crd = blueprint.into_crd();
        assert_eq!(crd.model_fallbacks.len(), 2);
        assert_eq!(crd.model_fallbacks[0].provider, "github-copilot");
        assert_eq!(crd.model_fallbacks[1].deployment, "gpt-5.4-pro");
    }

    #[test]
    fn schema_rejection_is_diagnosed_as_router_model_compatibility() {
        let logs = vec![
            "rawError=400 Unknown parameter: 'stream_options.include_usage'".to_string(),
            "LLM request failed: provider rejected the request schema or tool payload.".to_string(),
        ];
        let (cause, remedy, harness_issue, evidence) =
            diagnose_run_failure(&logs, &[], Some("provider rejected the request schema"));
        assert!(cause.contains("translated inference request"));
        assert!(remedy.contains("corrected inference router"));
        assert!(!harness_issue);
        assert_eq!(evidence.len(), 1);
    }

    #[test]
    fn completed_subagent_trace_is_rehydrated_from_artifact() {
        let artifacts = vec![MissionArtifactDto {
            name: "artifacts/.run-x/subagent-telemetry.jsonl".into(),
            size_bytes: None,
            content: Some(
                r#"{"at":"2026-07-23T12:00:00Z","event":"subagent_trace","member":"ci-verifier","trace":{"kind":"tool","name":"github_checks","ok":true}}"#
                    .into(),
            ),
            content_bytes: None,
            content_truncated: false,
            source_agent: None,
            source_path: None,
            digest: None,
            full_content: None,
        }];

        let events = subagent_trace_from_artifacts(&artifacts);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["agent"], "ci-verifier");
        assert_eq!(events[0]["agentRole"], "subagent");
        assert_eq!(events[0]["ts"], "2026-07-23T12:00:00Z");
        assert_eq!(events[0]["name"], "github_checks");
    }

    #[test]
    fn artifact_preview_is_bounded_but_full_content_remains_internal() {
        let mut budget = ARTIFACT_PREVIEW_TOTAL_BYTES;
        let full = "x".repeat(ARTIFACT_PREVIEW_MAX_BYTES + 100);
        let (preview, bytes, truncated, internal) =
            artifact_preview(Some(full.clone()), &mut budget);

        assert_eq!(
            preview.as_ref().map(String::len),
            Some(ARTIFACT_PREVIEW_MAX_BYTES)
        );
        assert_eq!(bytes, Some(full.len() as i64));
        assert!(truncated);
        assert_eq!(internal.as_deref(), Some(full.as_str()));
    }

    #[test]
    fn artifact_previews_share_a_bounded_response_budget() {
        let mut budget = ARTIFACT_PREVIEW_MAX_BYTES + 100;
        let first = "a".repeat(ARTIFACT_PREVIEW_MAX_BYTES + 1);
        let second = "b".repeat(ARTIFACT_PREVIEW_MAX_BYTES);

        let (first_preview, _, first_truncated, _) = artifact_preview(Some(first), &mut budget);
        let (second_preview, _, second_truncated, _) = artifact_preview(Some(second), &mut budget);

        assert_eq!(
            first_preview.as_ref().map(String::len),
            Some(ARTIFACT_PREVIEW_MAX_BYTES)
        );
        assert_eq!(second_preview.as_ref().map(String::len), Some(100));
        assert!(first_truncated);
        assert!(second_truncated);
        assert_eq!(budget, 0);
    }

    #[test]
    fn empty_artifact_is_not_reported_as_truncated() {
        let mut budget = ARTIFACT_PREVIEW_TOTAL_BYTES;
        let (preview, bytes, truncated, internal) =
            artifact_preview(Some(String::new()), &mut budget);

        assert_eq!(preview.as_deref(), Some(""));
        assert_eq!(bytes, Some(0));
        assert!(!truncated);
        assert_eq!(internal.as_deref(), Some(""));
    }

    #[test]
    fn artifact_preview_respects_utf8_boundaries_and_exhausted_budget() {
        let mut budget = 5;
        let (preview, bytes, truncated, _) =
            artifact_preview(Some("abcd\u{1f642}".to_string()), &mut budget);

        assert_eq!(preview.as_deref(), Some("abcd"));
        assert_eq!(bytes, Some(8));
        assert!(truncated);
        assert_eq!(budget, 1);

        budget = 0;
        let (preview, bytes, truncated, _) =
            artifact_preview(Some("still here".to_string()), &mut budget);
        assert!(preview.is_none());
        assert_eq!(bytes, Some(10));
        assert!(truncated);
    }

    #[test]
    fn full_artifact_content_is_private_but_available_for_trace_recovery() {
        let telemetry = r#"{"at":"2026-07-23T12:00:00Z","event":"subagent_trace","member":"ci-verifier","trace":{"kind":"tool","name":"github_checks","ok":true}}"#;
        let artifact = MissionArtifactDto {
            name: "artifacts/.run-x/subagent-telemetry.jsonl".into(),
            size_bytes: Some(telemetry.len() as i64),
            content: Some("{\"at\":\"2026".into()),
            content_bytes: Some(telemetry.len() as i64),
            content_truncated: true,
            source_agent: None,
            source_path: None,
            digest: None,
            full_content: Some(telemetry.into()),
        };

        let serialized = serde_json::to_value(&artifact).expect("serialize artifact preview");
        assert_eq!(serialized["content"], "{\"at\":\"2026");
        assert!(serialized.get("full_content").is_none());

        let events = subagent_trace_from_artifacts(&[artifact]);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["name"], "github_checks");
    }

    #[test]
    fn structured_team_evidence_uses_full_content_not_preview_order() {
        let role_plan = MissionArtifactDto {
            name: "role-plan.json".into(),
            size_bytes: None,
            content: None,
            content_bytes: Some(91),
            content_truncated: true,
            source_agent: None,
            source_path: None,
            digest: None,
            full_content: Some(
                r#"{"selected_roles":[{"role":"builder"}],"skipped_roles":["observer"]}"#.into(),
            ),
        };
        let collaboration = MissionArtifactDto {
            name: "collaboration.jsonl".into(),
            size_bytes: None,
            content: Some("{\"at\":\"truncated".into()),
            content_bytes: Some(200),
            content_truncated: true,
            source_agent: None,
            source_path: None,
            digest: None,
            full_content: Some(
                r#"{"at":"2026-07-23T12:00:00Z","event":"child_handback","from_agent":"builder","outcome":"success","reply_preview":"done"}"#
                    .into(),
            ),
        };

        let (plan, events) = structured_team_evidence(&[role_plan, collaboration]);

        assert_eq!(plan.selected_roles, ["builder"]);
        assert_eq!(plan.skipped_roles, ["observer"]);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].member.as_deref(), Some("builder"));
        assert_eq!(events[0].event, "child_handback");
    }

    #[test]
    fn assignment_ledger_uses_canonical_role_from_assignment_message() {
        let mut events = vec![TaskAssignmentEventDto {
            sequence: 1,
            event_id: "event-1".into(),
            task_id: "run-1".into(),
            event_type: "child_progress".into(),
            state: "Completed".into(),
            at: "2026-08-04T21:36:50Z".into(),
            worker_did: None,
            stage: Some("child_handback".into()),
            child_task_id: Some("message-1".into()),
            child_role: Some("principal-remediatio-b9220dcf".into()),
            outcome: Some("success".into()),
            message: None,
        }];
        let collaboration = vec![TeamCollaborationEventDto {
            at: Some("2026-08-04T21:35:32Z".into()),
            event: "assignment_sent".into(),
            agent: Some("principal".into()),
            member: Some("remediation-engineer".into()),
            outcome: None,
            message_id: Some("message-1".into()),
            reply_preview: None,
            content_preview: None,
        }];

        canonicalize_assignment_event_roles(&mut events, &collaboration);

        assert_eq!(
            events[0].child_role.as_deref(),
            Some("remediation-engineer")
        );
    }

    #[test]
    fn successful_result_hides_stale_bootstrap_checkpoint() {
        let progress = serde_json::json!({
            "schema": "kars.checkpoint/v1",
            "milestone_id": "dependency-pr",
            "status": "in_progress",
            "summary": "Controller initialized the durable milestone checkpoint."
        });

        assert!(select_task_checkpoint(Some(progress.clone()), &[], true).is_none());
        assert!(select_task_checkpoint(Some(progress), &[], false).is_some());
    }

    #[test]
    fn completed_artifact_checkpoint_wins_over_bootstrap_progress() {
        let completed = r#"{
            "schema": "kars.checkpoint/v1",
            "milestone_id": "dependency-pr",
            "status": "completed",
            "summary": "All required handbacks were retained."
        }"#;
        let artifact = MissionArtifactDto {
            name: "task-checkpoint.json".into(),
            size_bytes: Some(completed.len() as i64),
            content: None,
            content_bytes: Some(completed.len() as i64),
            content_truncated: true,
            source_agent: None,
            source_path: None,
            digest: None,
            full_content: Some(completed.into()),
        };
        let progress = serde_json::json!({
            "schema": "kars.checkpoint/v1",
            "milestone_id": "dependency-pr",
            "status": "in_progress",
            "summary": "Controller initialized the durable milestone checkpoint."
        });

        let checkpoint =
            select_task_checkpoint(Some(progress), &[artifact], true).expect("checkpoint");

        assert_eq!(checkpoint["status"], "completed");
    }

    #[test]
    fn aggregate_trace_tokens_replace_principal_only_total() {
        let mut result = Some(MissionResultDto {
            output: "done".into(),
            status: Some("ok".into()),
            model: None,
            total_tokens: Some(8_505),
            prompt_tokens: None,
            completion_tokens: None,
            finished_at: None,
            assignment_nonce: None,
            source: None,
            blocked: None,
            artifact_persistence: None,
            artifact_count: None,
            declared_artifact_count: None,
        });

        merge_trace_total_tokens(&mut result, 22_016);

        assert_eq!(result.and_then(|value| value.total_tokens), Some(22_016));
    }

    #[test]
    fn subagent_projects_human_identity_and_runtime_metadata() {
        let object: kube::core::DynamicObject = serde_json::from_value(serde_json::json!({
            "apiVersion": "kars.azure.com/v1alpha1",
            "kind": "KarsSandbox",
            "metadata": {
                "name": "researcher-run-7f4c",
                "namespace": "kars-team",
                "labels": {
                    "kars.azure.com/role": "Research specialist",
                    "kars.azure.com/parent": "principal-run"
                },
                "annotations": {
                    "kars.azure.com/logical-agent-id": "researcher",
                    "kars.azure.com/model": "gpt-5.4"
                }
            },
            "spec": {
                "runtime": {
                    "kind": "openclaw"
                }
            },
            "status": {
                "phase": "Running"
            }
        }))
        .expect("deserialize KarsSandbox");

        let dto = to_sub_agent(&object);

        assert_eq!(dto.name, "researcher-run-7f4c");
        assert_eq!(dto.namespace, "kars-team");
        assert_eq!(dto.phase.as_deref(), Some("Running"));
        assert_eq!(dto.runtime.as_deref(), Some("openclaw"));
        assert_eq!(dto.role.as_deref(), Some("Research specialist"));
        assert_eq!(dto.parent.as_deref(), Some("principal-run"));
        assert_eq!(dto.logical_agent_id.as_deref(), Some("researcher"));
        assert_eq!(dto.model.as_deref(), Some("gpt-5.4"));
    }

    #[test]
    fn malformed_checkpoint_is_not_exposed_to_the_ui() {
        assert!(
            valid_task_checkpoint(serde_json::json!({
                "schema": "kars.checkpoint/v1",
                "status": "completed"
            }))
            .is_none()
        );
        assert!(
            valid_task_checkpoint(serde_json::json!({
                "schema": "kars.checkpoint/v1",
                "milestone_id": "build",
                "status": "completed",
                "summary": "Artifact produced",
                "artifacts": "not-an-array"
            }))
            .is_none()
        );
        assert!(
            valid_task_checkpoint(serde_json::json!({
                "schema": "kars.checkpoint/v1",
                "milestone_id": "build",
                "status": "completed",
                "summary": "Artifact produced"
            }))
            .is_some()
        );
    }

    #[test]
    fn clean_objective_strips_loop_scaffold() {
        // A leaked loop scaffold must never reach a title — extract the GOAL.
        let scaffolded = "LOOP: ReAct — Reason + Act\nGOAL: find the Azure/kars star count and write a paragraph\nCYCLE: reason, act, observe\nSUCCESS: a paragraph with the count\nSTOP: when delivered\nSUB-AGENT INHERITANCE: give each sub-agent the same loop";
        assert_eq!(
            clean_objective(scaffolded),
            "find the Azure/kars star count and write a paragraph"
        );
    }

    #[test]
    fn clean_objective_passes_plain_through() {
        assert_eq!(
            clean_objective("Summarize the Q3 report"),
            "Summarize the Q3 report"
        );
    }

    #[test]
    fn clean_objective_strips_bracket_goal() {
        let s = "LOOP: eval-iterate\nGOAL: [[raise CLI test coverage]]\nSTOP: green";
        assert_eq!(clean_objective(s), "raise CLI test coverage");
    }

    #[test]
    fn deliverable_text_strips_fixed_sandbox_banner() {
        let raw = "# ? kars Sandbox - Secure AI Runtime on Azure\n\
            - **Foundry Project:** project\n\
            - **Model:** gpt\n\
            - **Sandbox ID:** run-1\n\
            - **Security:** isolated\n\
            - **Capabilities:** tools and reasoning\n\n\
            [[NO_MATERIAL_CHANGE]] nothing changed.";
        assert_eq!(
            super::deliverable_text(raw),
            "[[NO_MATERIAL_CHANGE]] nothing changed."
        );
    }

    #[test]
    fn deliverable_text_repairs_legacy_question_mark_replacements() {
        assert_eq!(
            super::deliverable_text("1? Role?plan: non?root; GHSA?w8wr?v893?vjvp"),
            "1. Role-plan: non-root; GHSA-w8wr-v893-vjvp"
        );
    }

    #[test]
    fn real_deliverable_gates_error_and_no_change() {
        use super::is_real_deliverable;
        assert!(!is_real_deliverable(Some("error"), "anything"));
        assert!(!is_real_deliverable(Some("ok"), "   "));
        assert!(!is_real_deliverable(
            Some("ok"),
            "[[NO_MATERIAL_CHANGE]] nothing changed"
        ));
        assert!(!is_real_deliverable(
            Some("ok"),
            "kars Sandbox - Secure AI Runtime on Azure\nSandbox ID: run-1\nSecurity: isolated\nCapabilities: tools\n[[NO_MATERIAL_CHANGE]] nothing changed"
        ));
        assert!(is_real_deliverable(Some("ok"), "Here is the report."));
        assert!(is_real_deliverable(None, "Some output"));
        // A budget-blocked ok-run is NOT a deliverable.
        assert!(!is_real_deliverable(
            Some("ok"),
            "API call failed after 3 retries: HTTP 429: Daily token budget exceeded (23131/20000 tokens)."
        ));
        assert!(!is_real_deliverable(
            Some("ok"),
            "unexpected tokens remaining in message header: Some(...)"
        ));
        assert!(!is_real_deliverable(
            Some("ok"),
            "assignment progress lease expired after 90s without renewal"
        ));
        assert!(is_real_deliverable(
            Some("ok"),
            "Completed remediation successfully. A prior child reported assignment progress lease expired, but its replacement delivered."
        ));
    }

    #[test]
    fn failed_output_cannot_create_pull_request_deliverables() {
        let data = std::collections::BTreeMap::from([
            ("status".to_string(), "error".to_string()),
            (
                "output".to_string(),
                "Claimed https://github.com/example/repo/pull/134".to_string(),
            ),
        ]);
        assert!(deliverable_pull_requests(&data).is_empty());
    }

    #[test]
    fn classify_blocked_detects_budget_and_parses_pair() {
        use super::classify_blocked;
        let b = classify_blocked(
            Some("ok"),
            "API call failed after 3 retries: HTTP 429: Daily token budget exceeded (23131/20000 tokens).",
        )
        .expect("budget block detected");
        assert_eq!(b.reason, "budget");
        assert_eq!(b.spent, Some(23131));
        assert_eq!(b.limit, Some(20000));
        // A real deliverable is not blocked.
        assert!(classify_blocked(Some("ok"), "Here is the finished report.").is_none());
        // An error run is handled elsewhere, not as blocked.
        assert!(classify_blocked(Some("error"), "Daily token budget exceeded").is_none());
    }

    #[test]
    fn assignment_dto_uses_the_web_snake_case_contract() {
        let event = TaskAssignmentEventDto {
            sequence: 3,
            event_id: "root:3".into(),
            task_id: "root".into(),
            event_type: "child_progress".into(),
            state: "Completed".into(),
            at: "2026-07-20T12:00:00Z".into(),
            worker_did: Some("did:agt:worker".into()),
            stage: Some("child_handback".into()),
            child_task_id: Some("child-1".into()),
            child_role: Some("reviewer".into()),
            outcome: Some("success".into()),
            message: None,
        };
        let value = serde_json::to_value(event).expect("serialize assignment event");
        assert_eq!(value["child_task_id"], "child-1");
        assert_eq!(value["child_role"], "reviewer");
        assert_eq!(value["event_type"], "child_progress");
        assert!(value.get("childTaskId").is_none());
    }

    #[test]
    fn deliverable_excerpt_strips_noise() {
        use super::deliverable_excerpt;
        let raw =
            "[[NO_MATERIAL_CHANGE]]\n# Heading\n| a | b |\n---\nThe repo star count is 1,234.";
        let ex = deliverable_excerpt(raw);
        assert!(ex.contains("star count"));
        assert!(!ex.contains("NO_MATERIAL_CHANGE"));
        assert!(!ex.contains('|'));
    }

    #[test]
    fn team_run_names_are_detected() {
        use super::regex_lite_is_team_run;
        assert!(regex_lite_is_team_run("kars-repo-health-run-1783099875"));
        assert!(regex_lite_is_team_run("ci-monitor-team-run-42"));
        // Standalone missions and non-numeric suffixes are NOT team runs.
        assert!(!regex_lite_is_team_run("audit-the-readme"));
        assert!(!regex_lite_is_team_run("some-run-abc"));
        assert!(!regex_lite_is_team_run("foo-run-"));
        assert!(!regex_lite_is_team_run("plainname"));
    }

    #[test]
    fn deliverable_excerpt_drops_markdown_wrapped_sentinel() {
        use super::deliverable_excerpt;
        // Bold-/emphasis-wrapped sentinel must still be recognized and dropped
        // (regression: it used to leak into the excerpt because the sentinel
        // check ran before markdown-wrapping was stripped).
        let raw = "**[[NO_MATERIAL_CHANGE]]** +3 stars\nThe repo now has 1,234 stars.";
        let ex = deliverable_excerpt(raw);
        assert!(
            !ex.contains("NO_MATERIAL_CHANGE"),
            "excerpt leaked sentinel: {ex}"
        );
        assert!(ex.contains("1,234 stars"));
    }

    #[test]
    fn clean_display_name_prefers_intent_over_scaffold() {
        use super::clean_display_name;
        // Scaffold display name -> derive (capitalized) from objective.
        assert_eq!(
            clean_display_name(
                &Some("LOOP: ReAct".to_string()),
                "GOAL: count the stars\nSTOP: done"
            ),
            Some("Count the stars".to_string())
        );
        // Real display name -> kept.
        assert_eq!(
            clean_display_name(&Some("Weekly repo digest".to_string()), "whatever"),
            Some("Weekly repo digest".to_string())
        );
        // A conversational prompt pasted into the display slot is NOT a title —
        // derive a concise one: strip the lead-in, shorten the URL, drop the
        // trailing "- …" condition tail, capitalize.
        assert_eq!(
            clean_display_name(
                &Some(
                    "Can you please check https://github.com/Azure/kars and analyse all dependabot PR"
                        .to_string()
                ),
                "Can you please check https://github.com/Azure/kars and analyse all dependabot PRs - categorize the ones which are safe to merge",
            ),
            Some("Check Azure/kars and analyse all dependabot PRs".to_string())
        );
    }

    #[test]
    fn concise_title_strips_lead_in_and_shortens_url() {
        use super::concise_title;
        assert_eq!(
            concise_title("I need you to summarise https://example.com/reports/q3 today"),
            "Summarise q3 today".to_string()
        );
        // Long objective is capped at a word boundary with an ellipsis.
        let long = "review every open pull request across the entire organisation and produce a ranked risk report";
        let t = concise_title(long);
        assert!(t.chars().count() <= 57, "title too long: {t}");
        assert!(t.ends_with('…'), "expected ellipsis: {t}");
        assert!(t.starts_with("Review "), "expected capitalized start: {t}");
    }

    #[test]
    fn ttl_human_to_iso8601() {
        assert_eq!(normalize_ttl("2h"), "PT2H");
        assert_eq!(normalize_ttl("30m"), "PT30M");
        assert_eq!(normalize_ttl("24h"), "PT24H");
        assert_eq!(normalize_ttl("1d"), "P1D");
        assert_eq!(normalize_ttl("90s"), "PT90S");
        assert_eq!(normalize_ttl(" 8 h "), "PT8H");
    }

    #[test]
    fn ttl_passthrough_and_fallback() {
        assert_eq!(normalize_ttl("PT2H"), "PT2H"); // already ISO
        assert_eq!(normalize_ttl("pt45m"), "PT45M"); // uppercased
        assert_eq!(normalize_ttl(""), "PT2H"); // empty → default
        assert_eq!(normalize_ttl("garbage"), "PT2H"); // unrecognized → default
        assert_eq!(normalize_ttl("0h"), "PT2H"); // zero → default
    }
}
