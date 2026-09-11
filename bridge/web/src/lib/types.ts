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

// ─── Launch-package options (from /api/options) ──────────────────────────────

export interface ModelOption {
  provider: string;
  deployment: string;
  is_default: boolean;
  /** Short human detail (e.g. "Anthropic · 1.0M ctx · powerful"), when known. */
  detail?: string | null;
}
export interface RuntimeOption {
  kind: string;
  label: string;
  wired: boolean;
  status: "ready" | "needs_image" | "unavailable" | "validated" | "available";
  note: string;
}
export interface ProviderInfo {
  id: string;
  label: string;
  note: string;
}
/** An additional inference provider configured alongside the single
 *  default — e.g. GitHub Copilot as the default plus Azure AI Foundry also
 *  connected. `has_key` only reports whether a dev-mode key is stored, never
 *  the value. `models` are the deployment ids this provider serves, feeding
 *  the shared model catalog tagged with this provider's own tag. */
export interface AdditionalProvider {
  tag: string;
  endpoint: string | null;
  has_key: boolean;
  models: string[];
}
export interface RefOption {
  name: string;
  namespace: string;
  summary: string | null;
  mode?: string | null;
  discovered_tools?: string[];
  tool_schema_digest?: string | null;
  compiled_digest?: string | null;
  backend?: string | null;
  readiness?: string | null;
  version?: string | null;
  recipe?: string | null;
  version_digest?: string | null;
  qualified_routes?: string[];
}
export interface IsolationOption {
  value: string;
  label: string;
  note: string;
}
export interface Options {
  models: ModelOption[];
  default_model: string | null;
  provider: ProviderInfo | null;
  runtimes: RuntimeOption[];
  isolation: IsolationOption[];
  tool_policies: RefOption[];
  mcp_servers: RefOption[];
  mcp_profiles: McpProfileOption[];
  memories: RefOption[];
  skills: RefOption[];
}

/** Operator-curated MCP bundle (a vetted set of McpServers). */
export interface McpProfileOption {
  name: string;
  summary: string | null;
  servers: string[];
}

// ─── Orchestrator: intent → composed launch package (§20) ────────────────────

export interface ComposeProposal {
  tier: number;
  model: BlueprintModel | null;
  model_fallbacks: BlueprintModel[];
  model_basis: string | null;
  runtime: string;
  instructions: string;
  tool_policy: string | null;
  mcp_servers: string[];
  skills: string[];
  egress: BlueprintEgress[];
  isolation: string;
  memory: string | null;
  budget_tokens: number | null;
  execution_plan: ExecutionPlan | null;
  delegation: MissionDelegation;
}

export interface MissionDelegationRole {
  name: string;
  objective: string;
}

export interface MissionDelegation {
  mode: "single-agent" | "principal-specialists";
  roles: MissionDelegationRole[];
  max_parallel: number;
}
export interface ComposeResponse {
  available: boolean;
  reason: string | null;
  proposal: ComposeProposal | null;
  rationale: string | null;
  source: string | null;
}

// ─── Team orchestrator: charter → org chart ──────────────────────────────────

export interface ComposeTeamRole {
  name: string;
  system_prompt: string;
  runtime: string;
  model: string;
  skills: string[];
}

export interface ComposeTeamProposal {
  tier: number;
  cadence_minutes: number;
  instructions: string;
  model: string;
  model_fallbacks: string[];
  model_basis: string | null;
  expected_tokens_per_outcome: number | null;
  efficiency_sample_runs: number;
  mcp_servers: string[];
  memory: string | null;
  egress: BlueprintEgress[];
  egress_mode: "learning" | "strict";
  engineering_enabled: boolean;
  engineering_signals: EngineeringSignal[];
  engineering_poll_interval_seconds: number;
  engineering_auto_run: boolean;
  roles: ComposeTeamRole[];
  execution_plan: ExecutionPlan | null;
  milestones: ComposeTeamMilestone[];
}

export interface TeamChannelStatus {
  channel: string;
  enabled: boolean;
  qualified?: boolean | null;
  detail?: string | null;
}

export interface TeamChannelsState {
  enabled: string[];
  statuses: TeamChannelStatus[];
}

export interface ComposeTeamMilestone {
  id: string;
  title: string;
  description: string;
  owner_role: string | null;
  depends_on: string[];
  acceptance_criteria: string[];
  review_required: boolean;
}

export interface ComposeTeamResponse {
  available: boolean;
  reason: string | null;
  proposal: ComposeTeamProposal | null;
  rationale: string | null;
  source: string | null;
}

// ─── Artifacts index (cross-mission deliverables, §16) ───────────────────────

export interface ArtifactFile {
  name: string;
  size_bytes: number | null;
  has_content: boolean;
  content_address: string | null;
  did: string | null;
}
export interface MissionArtifacts {
  task: string;
  evidence_key: string | null;
  team: string | null;
  archived: boolean;
  display_name: string | null;
  objective: string | null;
  model: string | null;
  finished_at: string | null;
  status: string | null;
  review_status: string;
  review_revision: number;
  files: ArtifactFile[];
  summary: string | null;
  excerpt: string | null;
  pull_requests: PullRequestRef[];
  deliverable_did: string | null;
}
export interface PullRequestRef {
  repo: string;
  number: number;
  url: string;
}
export interface ArtifactsIndex {
  missions: MissionArtifacts[];
}

/** One level of the hierarchical inference token budget (cluster / workspace),
 *  with the live measured daily usage and computed enforcement status. Mirrors
 *  bff/src/routes/budgets.rs::BudgetLevelDto. */
export interface BudgetLevel {
  scope: string;
  label: string;
  daily_tokens: number;
  mode: "passive" | "buffer" | "strict";
  buffer_percent: number;
  used_today: number;
  status: "ok" | "alert" | "over_buffer_headroom" | "blocking";
  percent: number;
  hard_cap: number;
}
export interface InferenceBudgets {
  cluster: BudgetLevel | null;
  cluster_used_today: number;
  workspaces: BudgetLevel[];
  users: BudgetLevel[];
  default_namespace: string;
  unbudgeted_namespaces: string[];
  unbudgeted_users: string[];
  alerts?: BudgetAlert[];
}
export interface BudgetAlert {
  scope: string;
  label: string;
  severity: "alert" | "over_buffer" | "blocking";
  message: string;
}

// ─── Retention policy (mission/team-run auto-cleanup) ───────────────────────

export interface RetentionPolicy {
  default_ttl_seconds: number;
  summary: string;
}

// ─── Pre-flight validation (§20) ─────────────────────────────────────────────

export type CheckStatus = "pass" | "fail" | "warn";
export interface ValidationCheck {
  id: string;
  label: string;
  status: CheckStatus;
  detail: string;
}
export interface ValidationResult {
  ok: boolean;
  checks: ValidationCheck[];
}

/** Autonomy tier labels (1..5), aligned with the kars taxonomy. */
export const TIER_LABELS: Record<number, string> = {
  1: "Manual",
  2: "Shared",
  3: "Conditional",
  4: "Supervised",
  5: "Full",
};

// ─── Teams (standing orgs) ───────────────────────────────────────────────────
// A Team is the durability-axis primitive: a standing org with a charter and a
// cadence loop that mints task-force work autonomously. Distinct from a Mission
// (a finite task force). Mirrors the BFF Teams DTOs.

export interface TeamSummary {
  name: string;
  display_name: string | null;
  charter: string;
  phase: string;
  reporting_to: string | null;
  tier: number;
  member_count: number;
  generated_task_count: number;
  every_minutes: number | null;
  lifecycle_mode: TeamLifecycleMode;
  warm_idle_seconds: number | null;
  runtime_state: TeamRuntimeState | null;
  current_assignment_task: string | null;
  idle_deadline_at: string | null;
  paused: boolean;
  created_at: string | null;
  last_run_at: string | null;
  last_success_at: string | null;
  last_activity_at: string | null;
  next_run_at: string | null;
  health: string | null;
  detail: string | null;
  runs_succeeded: number;
  retained_delivered: number;
  retained_no_action: number;
  retained_failed: number;
}

export interface TeamRole {
  name: string;
  system_prompt: string | null;
  tier: number | null;
  member_task: string | null;
  skills: string[];
  runtime: string | null;
  model: string | null;
}

export interface LedgerEvent {
  at: string;
  kind: string;
  summary: string;
  task: string | null;
  tokens: number | null;
}

export interface TeamDetail {
  name: string;
  display_name: string | null;
  charter: string;
  phase: string;
  reporting_to: string | null;
  knowledge_commons: string | null;
  tier: number;
  authority_ceiling: number;
  delegation_depth: number;
  paused: boolean;
  every_minutes: number | null;
  lifecycle_mode: TeamLifecycleMode;
  warm_idle_seconds: number | null;
  runtime_state: TeamRuntimeState | null;
  current_assignment_nonce: string | null;
  current_assignment_task: string | null;
  idle_deadline_at: string | null;
  envelope_digest: string | null;
  principal_task: string | null;
  roster: TeamRole[];
  member_count: number;
  generated_task_count: number;
  last_generated_task: string | null;
  last_run_at: string | null;
  next_run_at: string | null;
  detail: string | null;
  health: string | null;
  runs_succeeded: number;
  tokens_spent_total: number;
  commons_entry_count: number;
  last_success_at: string | null;
  created_at: string | null;
  last_activity_at: string | null;
  generated_tasks: string[];
  recent_outcomes: TeamOutcome[];
  recent_outcome_summary: TeamOutcomeSummary;
  tool_policy: string | null;
  tool_policy_default: boolean;
  mcp_servers: string[];
  git_write_repos: string[];
  egress: string[];
  egress_mode: string | null;
  /** Domains the team's agents have actually reached (live, from running runs). */
  learned_egress: string[];
  network_posture: string;
  model: string | null;
  model_fallbacks: string[];
  model_default: boolean;
  memory: string | null;
  runtime: string | null;
  runtime_default: boolean;
  isolation: string | null;
  execution_plan: ExecutionPlan | null;
  tasks: TeamTask[];
  channels: string[];
}

export type TeamLifecycleMode = "ephemeral" | "resourceOptimized" | "persistent";
export type TeamRuntimeState = "Working" | "Warm" | "Hibernating" | "Idle";

export type TeamOutcomeDisposition =
  | "change_proposed"
  | "no_action_needed"
  | "completed"
  | "failed";

export interface TeamOutcome {
  run: string;
  disposition: TeamOutcomeDisposition;
  headline: string;
  detail: string;
  objective: string;
  finished_at: string | null;
  duration_seconds: number | null;
  tokens: number | null;
  model: string | null;
  pull_requests: PullRequestRef[];
  artifact_count: number;
}

export interface TeamOutcomeSummary {
  change_proposed: number;
  no_action_needed: number;
  completed: number;
  failed: number;
}

/** A backlog task assigned to a standing team. */
export interface TeamTask {
  id: string;
  title: string;
  description: string;
  depends_on: string[];
  acceptance_criteria: string[];
  review_required: boolean;
  status: string; // pending | active | done
  run: string | null;
  created_at: string | null;
  done_at: string | null;
  stuck_since?: string | null;
  assignment_nonce?: string | null;
}

export type EngineeringSignal =
  | "dependabot_pr"
  | "dependabot_alert"
  | "code_scanning_alert"
  | "secret_scanning_alert";
export type EngineeringSignalSyncState =
  | "ok"
  | "unavailable"
  | "forbidden"
  | "truncated"
  | "error";
export interface EngineeringSignalResult {
  repo: string;
  signal: EngineeringSignal;
  state: EngineeringSignalSyncState;
  discovered: number;
  detail: string;
}
export type EngineeringSyncState =
  | "disabled"
  | "idle"
  | "syncing"
  | "ok"
  | "partial"
  | "error";
export type EngineeringReviewState =
  | "ready_for_review"
  | "waiting_for_ci"
  | "ci_failed"
  | "blocked"
  | "unknown";

export interface EngineeringReviewItem {
  repo: string;
  pr_number: number;
  pr_url: string;
  title: string;
  run: string;
  source_id: string;
  work_id: string;
  task_status: string;
  run_state: string | null;
  selected_roles: string[];
  delivered_roles: string[];
  artifact_count: number | null;
  head_sha: string;
  state: EngineeringReviewState;
  detail: string;
  checks_total: number;
  checks_passed: number;
  observed_at: string;
}

export interface EngineeringSourceStatus {
  state: EngineeringSyncState;
  last_sync_at: string | null;
  last_success_at: string | null;
  last_error: string | null;
  items_discovered: number;
  items_queued: number;
  total_items_queued: number;
  next_poll_at: string | null;
  review_items: EngineeringReviewItem[];
  ready_for_review: number;
  waiting_for_ci: number;
  ci_failed: number;
  signal_results: EngineeringSignalResult[];
}

export interface EngineeringSource {
  configured: boolean;
  enabled: boolean;
  auto_run: boolean;
  repos: string[];
  signals: EngineeringSignal[];
  poll_interval_seconds: number;
  status: EngineeringSourceStatus;
}

export interface GithubConnection {
  connected: boolean;
  account: string | null;
  repos: string[];
}

export interface CommonsEntry {
  id: string;
  title: string;
  author: string;
  source_task: string;
  created_at: string;
  digest: string;
  size_bytes: number;
  content: string | null;
}

export interface CommonsResponse {
  commons: string;
  count: number;
  entries: CommonsEntry[];
}

// ─── Artifact review (§16) ───────────────────────────────────────────────────

export interface ReviewEntry {
  decision: string;
  comment: string | null;
  reviewer: string;
  decided_at: string;
  revision: number;
  /** Whether the reviewer identity is server-attested. A self-reported
   *  (client-supplied) name in V0 is unverified → shown as such. */
  attested?: boolean;
  assignment_nonce?: string | null;
}

export interface ReviewState {
  status: string;
  revision: number;
  history: ReviewEntry[];
  redrive_pending: boolean;
  assignment_nonce: string | null;
}

// ─── Team digests (§20) ──────────────────────────────────────────────────────

export interface Digest {
  team: string;
  at: string;
  reporting_to: string | null;
  health: string;
  summary: string;
  runs_generated: number;
  runs_delivered: number;
  tokens_spent: number;
  knowledge_entries: number;
  channel: string | null;
  gated: boolean;
}


// ─── Governance Receipt ──────────────────────────────────────────────────────

export type ClaimStatus = "PASS" | "PARTIAL" | "FAIL" | "OMITTED";
export interface ReceiptClaim {
  class: string;
  status: string;
  detail: string;
}

export interface ReceiptSignature {
  keyid: string;
  sig: string;
}

export interface Receipt {
  name: string;
  namespace: string;
  task: string;
  envelope_digest: string;
  predicate_type: string;
  scheme: string;
  key_id: string;
  payload_type: string;
  signatures: ReceiptSignature[];
  claims: ReceiptClaim[];
  /** The decoded in-toto Statement — the exact bytes the signature covers. */
  statement: unknown;
  issued_at: string | null;
  /** Inclusion-log sequence (cross-receipt tamper-evidence chain). */
  inclusion_seq: number | null;
  /** Inclusion-log entry hash. */
  inclusion_entry_hash: string | null;
  inclusion_state: "Included" | "Failed" | null;
  inclusion_error: string | null;
  log_segment: string | null;
  checkpoint_tree_size: number | null;
  witnessed: boolean | null;
  /** The log's signed checkpoint (signed tree head), when published. */
  checkpoint: ReceiptCheckpoint | null;
  /** The exact command an auditor runs to verify independently. */
  verify_command: string;
}

/** A KarsEval safety/conformance eval and its latest verdict. */
export interface EvalResult {
  total: number;
  passed: number;
  failed: number;
  errored: number;
  corpus_name: string | null;
  corpus_digest: string | null;
  completed_at: string | null;
}

export interface Eval {
  name: string;
  namespace: string;
  display_name: string | null;
  target_sandbox: string | null;
  corpus: string | null;
  phase: string | null;
  schedule: string | null;
  last_run_at: string | null;
  last_result: EvalResult | null;
  created: string | null;
}

/** A single eval case: what it probes + its latest verdict. */
export interface EvalCase {
  id: string;
  tags: string[];
  probe: string | null;
  expected: string | null;
  actual: string | null;
  actual_reason: string | null;
  pass: boolean | null;
  /** True when the case couldn't be evaluated (target unreachable) — inconclusive, not a policy fail. */
  errored: boolean;
}

/** The detailed eval report — corpus cases merged with per-case verdicts. */
export interface EvalReport {
  name: string;
  corpus: string | null;
  total: number;
  passed: number;
  failed: number;
  /** Cases the runner couldn't evaluate (target unreachable) — inconclusive, shown separately. */
  errored: number;
  completed_at: string | null;
  per_case_available: boolean;
  cases: EvalCase[];
}

export interface ReceiptCheckpoint {
  tree_size: number;
  root_hash: string;
  key_id: string;
  published_at: string | null;
}

/** A signed receipt claim mapped to an external regulatory obligation. */
export interface ComplianceControl {
  control_id: string;
  framework: string;
  reference: string;
  receipt_class: string;
  status: string;
  evidence: string;
  /** True for the `regulatory` claim class — a named V0 limitation (external
   *  transparency anchor lands in V1), so it reads PARTIAL on every receipt
   *  this product issues today, not a gap specific to this task. */
  advisory?: boolean;
}

/** A compliance evidence pack derived from a mission's signed receipt. */
export interface CompliancePack {
  task: string;
  namespace: string;
  generated_at: string;
  predicate_type: string;
  envelope_digest: string;
  signature_scheme: string;
  key_id: string;
  inclusion_seq: number | null;
  issued_at: string | null;
  verify_command: string;
  controls: ComplianceControl[];
  satisfied: number;
  partial: number;
  /** Count of `advisory` controls — excluded from `partial`. */
  advisory?: number;
}

// ─── Steering / HITL approvals ───────────────────────────────────────────────

export interface Approval {
  name: string;
  namespace: string;
  task: string;
  team: string | null;
  milestone: string | null;
  action_kind: string;
  summary: string;
  detail: string | null;
  requested_tier: number | null;
  phase: string;
  decider: string | null;
  requested_at: string | null;
  decided_at: string | null;
  expires_at: string | null;
  bound_envelope_digest: string | null;
  run_nonce: string | null;
  resource_version: string;
  generation: number;
  /** Whether a human can still act on this (only a Pending approval). */
  actionable: boolean;
}

// ─── System / wiring ─────────────────────────────────────────────────────────

export type WiringStatus = "live" | "partial" | "not_wired";

export interface PipelineStage {
  id: string;
  name: string;
  description: string;
  status: WiringStatus;
  detail: string;
}

export interface CrdStatus {
  name: string;
  installed: boolean;
}

export interface SystemCounts {
  tasks: number;
  ready_tasks: number;
  degraded_tasks: number;
  digested_tasks: number;
  sandboxes: number | null;
}

export interface SystemStatus {
  namespace: string;
  controller_reachable: boolean;
  crds: CrdStatus[];
  counts: SystemCounts;
  pipeline: PipelineStage[];
}

/** One concrete, act-on-it problem the live diagnostics scan found. */
export interface DiagnosticIssue {
  severity: "critical" | "warning";
  kind: string;
  subject: string;
  reason: string;
  detail: string | null;
  remedy: string;
}

export interface Diagnostics {
  issues: DiagnosticIssue[];
  scanned_pods: number;
  scanned_sandboxes: number;
  healthy: boolean;
}

/** The orchestrator's proposed loop for an intent, shown in the Loop Designer. */
export interface LoopProposal {
  pattern: string;
  goal: string;
  criteria: string;
  rationale: string;
  source: "orchestrator" | "heuristic";
}

/** kars-SRE agent + Headlamp plugin integration status. */
export interface Integrations {
  sre_present: boolean;
  sre_phase: string | null;
  sre_ready: string | null;
  sre_activate_cmd: string;
  headlamp_deployed: boolean;
  headlamp_url: string | null;
  headlamp_paths: { label: string; path: string }[];
  headlamp_install_hint: string;
}

/** Orchestrator (compose engine) health + the active inference path. */
export interface Orchestrator {
  mode: "direct" | "sandbox" | "none";
  direct_configured: boolean;
  sandbox_present: boolean;
  sandbox_phase: string | null;
  sandbox_ready: string | null;
  sandbox_restarts: number | null;
  sandbox_waiting_reason: string | null;
  router_candidates: number;
  recommend_direct: boolean;
  note: string;
}

export const WIRING_LABELS: Record<WiringStatus, string> = {
  live: "Live",
  partial: "Partial",
  not_wired: "Not wired",
};

// ─── Operator Console projections (real CRD reads) ──────────────────────────

export interface Sandbox {
  name: string;
  namespace: string;
  runtime_namespace: string | null;
  phase: string | null;
  runtime: string | null;
  isolation: string | null;
  tool_policy: string | null;
  inference_policy: string | null;
  governed: boolean;
  team: string | null;
  parent: string | null;
  message: string | null;
  created: string | null;
  working: boolean | null;
  /** Currently executing a task (Running AND not yet delivered) — distinct
   *  from `working` (has ever produced activity). See operator.rs SandboxDto. */
  executing: boolean | null;
  cpu_millicores: number | null;
  memory_bytes: number | null;
  conditions: Array<{
    type_: string;
    status: string;
    reason: string | null;
    message: string | null;
  }>;
}

export interface NodeCapacity {
  name: string;
  cpu_usage_millicores: number | null;
  cpu_allocatable_millicores: number | null;
  memory_usage_bytes: number | null;
  memory_allocatable_bytes: number | null;
  cpu_percent: number | null;
  memory_percent: number | null;
}

export interface ClusterCapacity {
  metrics_available: boolean;
  metrics_error: string | null;
  team_max_concurrent_runs: number;
  global_active_runs_limit: number;
  active_team_runs: number;
  pod_metrics_available: boolean;
  pod_metrics_error: string | null;
  nodes: NodeCapacity[];
}

export interface McpServer {
  name: string;
  namespace: string;
  url: string | null;
  phase: string | null;
  mode: "Managed" | "External" | null;
  endpoint: string | null;
  workload_ref: string | null;
  discovered_tools: string[];
  tool_schema_digest: string | null;
  production: boolean | null;
  allowed_tools: string[];
  created: string | null;
  spec: Record<string, unknown>;
}

export interface ToolPolicy {
  name: string;
  namespace: string;
  phase: string | null;
  version_hash: string | null;
  applies_to: string | null;
  has_governance_profile: boolean;
  allowed: string[];
  created: string | null;
  spec: Record<string, unknown>;
}

export interface InferencePolicy {
  name: string;
  namespace: string;
  phase: string | null;
  version_hash: string | null;
  sandbox: string | null;
  daily_token_budget: number | null;
  content_safety: boolean;
  created: string | null;
  spec: Record<string, unknown>;
}

export interface EgressApproval {
  name: string;
  namespace: string;
  sandbox: string | null;
  phase: string | null;
  reason: string | null;
  hosts: string[];
  expires_at: string | null;
  created: string | null;
}

// ─── GitHub App (platform identity) ─────────────────────────────────────────

export interface GithubApp {
  configured: boolean;
  slug: string | null;
  install_url: string | null;
}

export interface DiscoveredModel {
  id: string;
  label: string | null;
  /** True for the one starred/pre-selected pick — currently only populated
   *  for GitHub Copilot's curated catalog (mirrors `kars dev`'s picker). */
  recommended?: boolean;
}

// ─── kars-SRE self-remediation proposals ────────────────────────────────────

export interface SreAction {
  name: string;
  namespace: string;
  action_type: string;
  target_namespace: string | null;
  target_name: string | null;
  params: Record<string, unknown>;
  rationale: string | null;
  diagnosis: string | null;
  approval_state: string;
  approval_note: string | null;
  phase: string;
  applied_at: string | null;
  ttl_minutes: number | null;
  created_at: string | null;
  actionable: boolean;
}

// ─── Insights / scorecard (real + honest) ───────────────────────────────────

export interface CountPair {
  label: string;
  count: number;
}

export interface Insights {
  missions_by_phase: CountPair[];
  missions_by_tier: CountPair[];
  decisions: CountPair[];
  launched: number;
  receipts_issued: number;
  inclusion_log_size: number;
  amplification_rejections: number;
  runtime_metrics_available: boolean;
  runtime_metrics_note: string | null;
}

export interface Scorecard {
  task: string;
  namespace: string;
  tier: number | null;
  launched: boolean;
  execution_phase: string | null;
  token_budget: number | null;
  decisions_recorded: number;
  approvals_granted: number;
  approvals_denied: number;
  receipt_issued: boolean;
  run_total_tokens: number | null;
  run_prompt_tokens: number | null;
  run_completion_tokens: number | null;
  run_model: string | null;
  runtime_metrics_available: boolean;
  runtime_metrics_note: string | null;
}

export interface ReceiptSummary {
  name: string;
  namespace: string;
  task: string | null;
  envelope_digest: string | null;
  key_id: string | null;
  inclusion_seq: number | null;
  created: string | null;
  verdict: "verified" | "failed" | "partial" | "none";
}

export interface Audit {
  receipts: ReceiptSummary[];
  inclusion_log_size: number;
  checkpoint: {
    tree_size: number;
    root_hash: string;
    key_id: string;
    published_at: string | null;
  } | null;
  /** Real cryptographic integrity verdict computed server-side: the whole hash
   *  chain recomputed + the signed checkpoint verified against the anchor. */
  integrity: {
    chain_consistent: boolean;
    tree_size: number;
    checkpoint_verified: boolean;
    witness_present: boolean;
    anchor_pinned: boolean;
  };
}

/** One independent verification check performed server-side by the BFF. */
export interface VerifyCheck {
  name: string;
  passed: boolean;
  detail: string;
  /** Displayed but not cryptographically re-verified here (e.g. the V0 witness
   *  whose public key isn't published) — rendered as "shown, not verified",
   *  never a green ✓. */
  advisory?: boolean;
  /** The recorded value the proof expected (e.g. a logged hash), when shown. */
  expected?: string | null;
  /** The value the BFF independently recomputed — visibly matches `expected`. */
  computed?: string | null;
}

export interface InclusionEvidence {
  seq: number;
  receipt: string;
  payload_sha256: string;
  prev_hash: string;
  entry_hash: string;
  recomputed_entry_hash: string;
  chain_head: string;
  chain_consistent: boolean;
  tree_size: number;
}

export interface CheckpointEvidence {
  tree_size: number;
  root_hash: string;
  signed_note: string;
  signature_b64: string;
  signature_valid: boolean;
  witness_key_id?: string | null;
  witness_signature_b64?: string | null;
}

export interface Evidence {
  signed_statement?: unknown;
  signature_b64?: string | null;
  scheme?: string | null;
  anchor_key_id?: string | null;
  anchor_public_key_b64?: string | null;
  inclusion?: InclusionEvidence | null;
  checkpoint?: CheckpointEvidence | null;
}

/** The result of in-browser (BFF-side) cryptographic receipt verification. */
export interface VerifyResult {
  verified: boolean;
  checks: VerifyCheck[];
  evidence: Evidence;
}

// ─── Cross-harness efficiency frontier (§3B, Pillar B) ───────────────────────

export interface RouteEfficiency {
  route: string;
  harness: string;
  runs: number;
  delivered: number;
  success_rate: number;
  accepted: number;
  acceptance_rate: number;
  avg_tokens: number;
  tokens_per_outcome: number;
  avg_rounds: number;
  avg_tool_calls: number;
  // 2026 enrichments.
  avg_prompt_tokens: number;
  avg_completion_tokens: number;
  tool_fail_rate: number;
  avg_wall_ms: number;
  p95_wall_ms: number;
  avg_ttfa_ms: number;
  reliability_rate: number | null;
  reliability_k: number | null;
  reliability_samples: number;
  usd_per_outcome: number | null;
  cache_hit_rate: number;
  top_fault: string;
}

export interface Efficiency {
  routes: RouteEfficiency[];
  recommended: string | null;
  recommended_harness: string | null;
  recommended_basis: string | null;
  recommended_low_confidence: boolean;
  total_runs: number;
  priced: boolean;
}

export interface SkillSummary {
  name: string;
  namespace: string;
  version: string | null;
  summary: string | null;
  bounding_policy: string | null;
  phase: string | null;
  version_digest: string | null;
  attestation_verified: boolean | null;
  // Operator trust gate.
  review: string;
  locked_digest: string | null;
  approved_by: string | null;
  approved_at: string | null;
  usable: boolean;
  spec: Record<string, unknown>;
}

export interface ProfileRole {
  name: string;
  system_prompt: string | null;
  skills: string[];
}

export interface ProfileSummary {
  name: string;
  namespace: string;
  domain: string | null;
  phase: string | null;
  template_digest: string | null;
  display_name: string | null;
  charter_template: string | null;
  tier: number | null;
  tool_policy: string | null;
  knowledge_commons: string | null;
  roles: ProfileRole[];
  spec: Record<string, unknown>;
}

export interface AgentLifecycle {
  sandbox: string;
  namespace: string;
  phase: string | null;
  parent: string | null;
  task: string | null;
  objective: string | null;
  tier: number | null;
  rounds: number;
  tool_calls: number;
  last_action: string | null;
  live: boolean;
  tokens: number | null;
  /** The run's token budget ceiling (envelope), when set — for spend-vs-limit. */
  budget_tokens: number | null;
  status: string | null;
  finished_at: string | null;
  team: string | null;
  display_name: string | null;
  health: PodHealth | null;
}

/** Honest pod-level health of a live agent (no CPU/mem — status-derived). */
export interface PodHealth {
  ready_containers: number;
  total_containers: number;
  restarts: number;
  uptime_seconds: number | null;
  node: string | null;
  waiting_reason: string | null;
}

/** Datapath-completeness witness — the optional eBPF (Inspektor Gadget) witness
 * cross-checks kernel-observed egress against each sandbox's declared allowlist.
 * `enabled: false` => the witness isn't installed (show enable instructions). */
export interface DatapathWitnessSandbox {
  namespace: string;
  sandbox: string;
  declared_hosts: string[];
  observed_dns: string[];
  observed_connects: number;
  beyond_declared: string[];
  unused_declared: string[];
  verdict: "COMPLIANT" | "BEYOND-DECLARED" | "LEARN" | string;
}
export interface DatapathWitness {
  enabled: boolean;
  generated_at: string | null;
  window_seconds: number | null;
  sandboxes: DatapathWitnessSandbox[];
  install_hint: string;
}

/** A single cross-agent activity event in the fleet live feed. */
export interface FleetActivityItem {
  agent: string;
  display_name: string | null;
  team: string | null;
  kind: "tool" | "round" | string;
  label: string;
  detail: string | null;
  failed: boolean;
  round: number;
  seq: number;
  ms: number | null;
}
/** Fleet-wide live telemetry — aggregate metrics + merged activity feed. */
export interface FleetTelemetry {
  working: number;
  teams_active: number;
  sub_agents: number;
  tokens_in_flight: number;
  tool_calls: number;
  rounds: number;
  feed: FleetActivityItem[];
}

/** Live troubleshooting evidence for a failed run (from GET …/troubleshoot). */
export interface TroubleshootContainer {
  name: string;
  ready: boolean;
  restarts: number;
  state: string;
  reason: string | null;
}
export interface Troubleshoot {
  pod_found: boolean;
  pod_summary: string | null;
  containers: TroubleshootContainer[];
  agent_log_tail: string[];
  evidence: string[];
  cause: string;
  remedy: string;
  harness_issue: boolean;
  result_status: string | null;
  result_reason: string | null;
}
