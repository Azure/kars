

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
