import type { BlueprintEgress, BlueprintModel, ExecutionPlan } from "./missions";
import type { EngineeringSignal } from "./teams";


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
