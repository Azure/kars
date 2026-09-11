

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
