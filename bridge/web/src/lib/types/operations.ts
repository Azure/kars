

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
