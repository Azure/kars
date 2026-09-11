

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
