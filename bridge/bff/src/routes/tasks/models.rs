use serde::{Deserialize, Serialize};

use super::{PullRequestRef, SubAgentDto};

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
    pub(super) fn into_crd(self) -> crate::kars::task::TaskBlueprint {
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
    pub(super) full_content: Option<String>,
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
