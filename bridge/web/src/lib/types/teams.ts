import type { ExecutionPlan } from "./missions";
import type { PullRequestRef } from "./workspace";


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
