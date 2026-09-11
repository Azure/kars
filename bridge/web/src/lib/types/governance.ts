


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
