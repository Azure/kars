// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * Operator-TUI panel framework — shared type declarations (S14).
 *
 * A `Panel` is a self-contained dashboard fragment that renders a slice
 * of `ClusterState` into a blessed-tag string. The framework is data-
 * source agnostic: the same panel renders identically against a live
 * kubectl-backed snapshot or a fixture used by tests.
 *
 * Per-panel goals (S14, see plan §S14):
 *  - Empty state never throws.
 *  - Conditions reasons render verbatim — no creative rephrasing.
 *  - No panel raw-renders Secret data; secret presence is "<present>"
 *    or "<missing>". Redaction lives in `./redact.ts`.
 */
import type { SandboxInfo } from "../types.js";

/** Generic Kubernetes Condition (status sub-object). */
export interface CrdCondition {
  type: string;
  status: string;       // "True" | "False" | "Unknown"
  reason?: string;
  message?: string;
  lastTransitionTime?: string;
  observedGeneration?: number;
}

export interface CrdItem {
  name: string;
  namespace: string;
  age?: string;
  conditions: CrdCondition[];
}

export interface ClawPairingItem extends CrdItem {
  agentA?: string;
  agentB?: string;
  trust?: string;
  state?: string;
}

export interface McpServerItem extends CrdItem {
  url?: string;
  productionMode?: boolean;
  jwksSecretPresent?: "present" | "missing" | "unknown";
  jwksSecretReason?: string;
  allowedToolCount?: number;
}

export interface ToolPolicyItem extends CrdItem {
  appliesToSandbox?: string;
  commerce?: { mandates?: boolean; floorUsd?: number };
  rateLimitPerMin?: number;
  ruleCount?: number;
}

export interface InferencePolicyItem extends CrdItem {
  appliesToSandbox?: string;
  dailyTokens?: number;
  perRequestTokens?: number;
  guardrailFloor?: string;     // e.g., "high" | "medium" | "low"
  modelPreference?: string[];  // ordered preference list
}

export interface A2AAgentItem extends CrdItem {
  endpointUrl?: string;
  productionMode?: boolean;
  agentCardPublished?: "published" | "pending" | "failed" | "unknown";
  agentCardReason?: string;
  capabilities?: string[];
}

export interface ClawMemoryItem extends CrdItem {
  sandboxRef?: string;
  storeName?: string;
  scope?: string;
  retentionDays?: number;
  rbacScopeSummary?: string;   // e.g., "project-MI: Azure AI User on RG"
  foundryBound?: "bound" | "pending" | "failed" | "unknown";
}

export interface ClawEvalItem extends CrdItem {
  sandboxRef?: string;
  suite?: string;
  schedule?: string;
  lastRunAt?: string;
  lastScore?: string;
  nextScheduledAt?: string;
}

/** One provider-status entry; `unknown` is the only honest answer when we
 *  don't have enough info — see plan §0.2 #10 ("verify, don't guess"). */
export interface ProviderState {
  id: string;
  label: string;
  status: "healthy" | "degraded" | "down" | "unknown";
  reason?: string;     // human-readable explanation, esp. for "unknown"
  detail?: string;     // optional secondary line
}

export interface ProviderStatusSnapshot {
  /** Per-sandbox provider state map (sandbox-name -> providers).
   *  An empty map is legal — empty cluster, or pre-fetch. */
  perSandbox: Map<string, ProviderState[]>;
  /** Cluster-wide providers (e.g., AGC ingress). */
  cluster: ProviderState[];
}

/** Full snapshot consumed by panels. Every field has a safe empty form. */
export interface ClusterState {
  sandboxes: SandboxInfo[];
  pairings: ClawPairingItem[];
  mcpServers: McpServerItem[];
  toolPolicies: ToolPolicyItem[];
  inferencePolicies: InferencePolicyItem[];
  a2aAgents: A2AAgentItem[];
  clawMemories: ClawMemoryItem[];
  clawEvals: ClawEvalItem[];
  providers: ProviderStatusSnapshot;
}

/** Optional render scope — when `sandbox` is set, panel filters to that
 *  sandbox-name only (used by the `--per-sandbox` layout). */
export interface PanelRenderOpts {
  sandbox?: string;
  width?: number;
}

/** Category used by the layout to group panels. Optional for back-compat. */
export type PanelCategory =
  | "agent"           // ClawSandbox — the page's reason to exist
  | "infrastructure"  // InferencePolicy, ToolPolicy — usually present, expected
  | "optional"        // McpServer, A2AAgent, ClawMemory, ClawEval — hidden when 0
  | "internal"        // ClawPairing — controller-managed, foldable
  | "providers";      // Provider/health bar — always shown compact

/** Per-panel summary used by the at-a-glance section. */
export interface PanelSummary {
  total: number;
  healthy: number;
  warning: number;
  error: number;
  unknown: number;
  /** Optional one-line freeform hint (e.g., "primary=gpt-5.4"). */
  detail?: string;
}

/** All panels implement this small surface. */
export interface Panel {
  /** Stable id used by `--panels <a,b,c>` and registry lookups. */
  id: string;
  /** Human-readable title for the panel header. */
  title: string;
  /** Pure render function. Must be safe against an empty `ClusterState`. */
  render(state: ClusterState, opts?: PanelRenderOpts): string;
  /** Optional: panel-specific recommended refresh cadence (ms). */
  refreshIntervalMs?: number;
  /** Optional: one-line "why this CRD matters" tag for the at-a-glance. */
  purpose?: string;
  /** Optional: layout grouping. Defaults to "infrastructure". */
  category?: PanelCategory;
  /** Optional: counts/health summary used by at-a-glance. */
  summarize?(state: ClusterState): PanelSummary;
}

/** An empty `ClusterState` — every consumer must accept this shape. */
export function emptyClusterState(): ClusterState {
  return {
    sandboxes: [],
    pairings: [],
    mcpServers: [],
    toolPolicies: [],
    inferencePolicies: [],
    a2aAgents: [],
    clawMemories: [],
    clawEvals: [],
    providers: { perSandbox: new Map(), cluster: [] },
  };
}
