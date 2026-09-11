import type { MissionDelegation } from "./orchestration";
import type { PullRequestRef } from "./workspace";
// kars Bridge web — shared types mirroring the BFF API DTOs.
// The BFF (Rust) owns these shapes; keep field names in sync with
// bff/src/routes/tasks.rs.

export interface Budget {
  scope?: "GovernedInference";
  tokens: number | null;
  usd_micros: number | null;
}

export interface Envelope {
  tier: number;
  authority_ceiling: number;
  delegation_depth: number;
  budget: Budget | null;
  tool_policy: string | null;
  egress_allowlist: string | null;
}

export interface TaskSummary {
  name: string;
  namespace: string;
  objective: string;
  display_name: string | null;
  created_at: string | null;
  tier: number;
  phase: string;
  envelope_digest: string | null;
  team: string | null;
  delivered: boolean;
  failed: boolean;
  launched: boolean;
  execution_phase: string | null;
}

export interface TaskDetail {
  name: string;
  namespace: string;
  objective: string;
  display_name: string | null;
  created_at: string | null;
  envelope: Envelope;
  phase: string;
  envelope_digest: string | null;
  observed_generation: number | null;
  lineage: string[];
  parent: string | null;
  /** The standing team that owns this task (from kars.azure.com/team). */
  team: string | null;
  status_message: string | null;
  children: TaskSummary[];
  launched: boolean;
  execution_phase: string | null;
  sandbox: string | null;
  egress_mode: string | null;
  execution_detail: string | null;
  assignment: TaskAssignmentStatus | null;
  assignment_events: TaskAssignmentEvent[];
  assignment_sequence: number | null;
  composition: Composition | null;
  sub_agents: SubAgent[];
  result: MissionResult | null;
  artifacts: MissionArtifact[];
  role_plan: TeamRolePlan;
  collaboration_events: TeamCollaborationEvent[];
  /** Pull requests the mission opened — first-class deliverables shown on the
   *  Artifacts tab (a PR is a delivery type). Empty when none. */
  pull_requests?: PullRequestRef[];
  activity: ActivityEvent[];
  telemetry: MissionTelemetry | null;
  checkpoint: TaskCheckpoint | null;
  agent_identity: AgentIdentity | null;
  /** A governed capability-routing correction recorded at creation (e.g. a
   *  chat-gateway harness swapped to an autonomous one for a one-shot mission).
   *  Null when no correction was needed. */
  harness_corrected: string | null;
  /** A governed emergency-stop decision (operator/reason/at) when the mission
   *  was halted. Null when never halted. */
  halted: string | null;
  /** Whether a run has ever been requested (the run-requested annotation is set).
   *  Gates the client auto-kickoff so the first run fires exactly once. */
  run_requested: boolean;
  /** Exact latest requested run nonce, available before assignment acknowledgement. */
  current_run_nonce: string | null;
}

export interface TeamRolePlan {
  selected_roles: string[];
  skipped_roles: string[];
}

export interface TeamCollaborationEvent {
  at: string | null;
  event: string;
  agent: string | null;
  member: string | null;
  outcome: string | null;
  message_id: string | null;
  reply_preview: string | null;
  content_preview: string | null;
}

export interface TaskCheckpoint {
  schema: string;
  milestone_id: string;
  status: "pending" | "in_progress" | "completed" | "blocked";
  summary: string;
  acceptance_criteria?: string[];
  artifacts?: string[];
  next_steps?: string[];
  updated_at?: string;
  agent?: string;
}

export interface TaskAssignmentStatus {
  task_id: string;
  state: string;
  worker_did: string | null;
  stage: string | null;
  child_task_id: string | null;
  child_role: string | null;
  last_progress_at: string | null;
  completed_at: string | null;
  error: string | null;
}

export interface TaskAssignmentEvent {
  sequence: number;
  event_id: string;
  task_id: string;
  event_type: string;
  state: string;
  at: string;
  worker_did: string | null;
  stage: string | null;
  child_task_id: string | null;
  child_role: string | null;
  outcome: string | null;
  message: string | null;
}

/** Loop-shape telemetry for a mission run (token totals are on MissionResult). */
export interface MissionTelemetry {
  rounds: number | null;
  tool_calls: number | null;
}

/** One event in the agent's live execution trace. A `round` event records the
 * model call (real token usage); a `tool` event records one tool invocation
 * with a sanitized args/result preview. */
export type ActivityEvent =
  | {
      kind: "round";
      round: number;
      prompt_tokens: number;
      completion_tokens: number;
      total_tokens: number;
      finish_reason: string;
      tool_calls: number;
      ms: number;
      ts: string;
      /** The agent (sandbox) that emitted this event, and its role in the tree.
       *  Present when the stream aggregates the whole agent tree; absent for a
       *  single-agent trace read from the persisted ConfigMap. */
      agent?: string;
      agentInstance?: string;
      agentRole?: "principal" | "subagent";
      seq?: number;
    }
  | {
      kind: "tool";
      round: number;
      name: string;
      args_preview: string;
      result_preview: string;
      ms: number;
      ok: boolean;
      ts: string;
      agent?: string;
      agentInstance?: string;
      agentRole?: "principal" | "subagent";
      seq?: number;
      /** Present on tools authoritatively executed and recorded by the router. */
      source?: "router" | "harness" | "governance";
    };

/** One artifact file in a mission's deliverable set. `content` is present for
 * text artifacts (markdown/json/csv/…) and null for binary ones. */
export interface MissionArtifact {
  name: string;
  size_bytes: number | null;
  content: string | null;
  content_bytes: number | null;
  content_truncated: boolean;
  source_agent: string | null;
  source_path: string | null;
  digest: string | null;
}

/** A running agent's real mesh identity, discovered from the AGT registry. */
export interface AgentIdentity {
  did: string;
  capabilities: string[];
  last_seen: string | null;
  reputation_score: number | null;
}

/** A captured mission run result — a real deliverable + real token cost. */
export interface MissionResult {
  output: string;
  status: string | null;
  model: string | null;
  total_tokens: number | null;
  prompt_tokens: number | null;
  completion_tokens: number | null;
  finished_at: string | null;
  assignment_nonce: string | null;
  /** How the deliverable was produced. "single_turn" = one model turn (no
   *  tools/sub-agents) because the mesh agent loop was unavailable; absent for
   *  a full agent-loop run. */
  source: string | null;
  /** Set when this run's ok-output is actually a capability/limit STOP (today the
   *  daily token budget), not a deliverable — rendered as an actionable state. */
  blocked: RunBlocked | null;
  artifact_persistence: "complete" | "partial" | null;
  artifact_count: number | null;
  declared_artifact_count: number | null;
}

export interface RunBlocked {
  /** Machine reason. Today: "budget". */
  reason: string;
  detail: string;
  spent: number | null;
  limit: number | null;
}

/** A sub-agent the mission's agent spawned at run time (a labelled sandbox). */
export interface SubAgent {
  name: string;
  namespace: string;
  phase: string | null;
  runtime: string | null;
  role: string | null;
  parent: string | null;
  logical_agent_id: string | null;
  model: string | null;
}

/** The composed run — what a mission actually runs with (from the blueprint). */
export interface Composition {
  runtime: string | null;
  model: string | null;
  instructions: string | null;
  tool_policy: string | null;
  mcp_servers: string[];
  egress: string[];
  isolation: string | null;
  memory: string | null;
}

export interface CreateTaskRequest {
  name: string;
  objective: string;
  display_name: string | null;
  envelope: Envelope;
  parent?: string | null;
  blueprint?: Blueprint | null;
  delegation?: MissionDelegation | null;
  launch?: boolean;
  /** Repos (owner/name) from this principal's GitHub connection. The BFF
   *  validates the complete set and derives the connection reference. */
  git_write_repos?: string[] | null;
  /** The creating principal, for per-user budget attribution. */
  created_by?: string | null;
}

/** A model route — provider tag + deployment, lands on InferencePolicy. */
export interface BlueprintModel {
  provider: string;
  deployment: string;
}

/** A network destination the mission may reach. */
export interface BlueprintEgress {
  host: string;
  port?: number | null;
}

/**
 * The editable run composition reviewed on the launch package. Every field maps
 * to a real field on the materialized InferencePolicy / KarsSandbox; the
 * controller compiles it. Mirrors bff/src/kars/task.rs::TaskBlueprint.
 */
export interface Blueprint {
  runtime?: string | null;
  model?: BlueprintModel | null;
  model_fallbacks?: BlueprintModel[];
  instructions?: string | null;
  tool_policy?: string | null;
  mcp_servers?: string[];
  egress?: BlueprintEgress[];
  egress_mode?: "strict" | "learning";
  isolation?: string | null;
  memory?: string | null;
  skills?: string[];
  execution_plan?: ExecutionPlan | null;
}

export interface ExecutionPlan {
  schema: "kars.execution-plan/v1";
  roles: ExecutionRole[];
  max_parallel: number;
  synthesis: ExecutionSynthesis;
  deliverables: ExecutionDeliverable[];
}

export interface ExecutionRole {
  name: string;
  objective: string;
  depends_on: string[];
  phases: ExecutionPhase[];
  budget_tokens?: number | null;
}

export interface ExecutionPhase {
  name: string;
  objective: string;
  capabilities: ExecutionCapability[];
  required_tool_calls?: ExecutionRequiredToolCall[];
  min_tool_calls?: number;
  max_tool_calls: number;
  fresh_context: boolean;
}

export interface ExecutionRequiredToolCall {
  name: "github_actions_job_logs";
  arguments: Record<string, string>;
}

export type ExecutionCapability =
  | "filesystem-read"
  | "filesystem-write"
  | "shell"
  | "network"
  | "mcp"
  | "memory";

export interface ExecutionSynthesis {
  objective: string;
  capabilities: ExecutionCapability[];
  max_tool_calls: number;
}

export interface ExecutionDeliverable {
  name: string;
  media_type?: string | null;
}
