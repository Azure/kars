// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * Auto-resume support for `kars up`.
 *
 * Strategy: every successful phase in up.ts calls `markPhaseDone(phase, ...)`
 * which merges into the existing `~/.kars/context.json`. On a subsequent
 * run, `loadResumeState()` returns the last completed phase (subject to
 * topology-match + TTL checks), and `isPhaseSkippable()` is consulted before
 * each expensive phase. On full success, `markPhaseDone("complete", ...)` is
 * called from sandbox_bringup so the next `up` starts fresh.
 *
 * Why only some phases skip:
 *   - rg, infra, kubectl, helm, mesh, sandbox already self-probe and finish
 *     in seconds; running them again is harmless.
 *   - network and images are the expensive ones (network: ~6 min, images:
 *     ~minutes per image with --build, or ~30s/image with --source-acr).
 *     These are the high-value skips on resume.
 */

import { loadContext, saveContext, type DeploymentContext, type UpPhase } from "../../config.js";

/** Linear order of phases; index() comparison drives skip-logic. */
const PHASE_ORDER: UpPhase[] = [
  "rg",
  "infra",
  "network",
  "kubectl",
  "images",
  "helm",
  "mesh",
  "sandbox",
  "complete",
];

/** Phases we actually skip on resume (others are cheap or self-idempotent). */
const SKIPPABLE: ReadonlySet<UpPhase> = new Set<UpPhase>(["network", "images"]);

/** Hard TTL — older partial state is ignored (subscription/quota may have changed). */
const RESUME_TTL_MS = 7 * 24 * 60 * 60 * 1000; // 7 days

/**
 * Topology fields that, if changed between runs, should invalidate a
 * partial resume (the saved context is for a different deployment).
 */
export interface ResumeTopology {
  subscription?: string;
  region?: string;
  resourceGroup?: string;
  aksCluster?: string;
  sandboxName?: string;
  sourceAcr?: string;
}

export interface ResumeState {
  /** Last completed phase from the previous run (never "complete"). */
  resumeFromPhase: UpPhase;
  /** The cached context from disk, including all values from prior phases. */
  ctx: DeploymentContext;
  /** Age of the saved context, milliseconds. */
  ageMs: number;
  /** If non-null, a human-readable note why we re-ran a phase even though it might be skippable. */
  warning?: string;
}

/**
 * Returns true if the saved context's topology matches the current run.
 * If a saved field is unset, it is treated as "compatible" (older context).
 */
function topologyMatches(saved: DeploymentContext, current: ResumeTopology): boolean {
  const cmp = (a?: string, b?: string): boolean => !a || !b || a === b;
  return (
    cmp(saved.subscription, current.subscription) &&
    cmp(saved.region, current.region) &&
    cmp(saved.resourceGroup, current.resourceGroup) &&
    cmp(saved.aksCluster, current.aksCluster) &&
    cmp(saved.sandboxName, current.sandboxName) &&
    cmp(saved.sourceAcr, current.sourceAcr)
  );
}

/**
 * Decide whether to resume from a partial previous run. Returns null if any of:
 *   - no context.json on disk
 *   - context.phase is missing or "complete"
 *   - topology mismatches the current invocation
 *   - context is older than RESUME_TTL_MS
 *   - caller passed `fromScratch: true`
 */
export function loadResumeState(
  options: { fromScratch?: boolean },
  topology: ResumeTopology,
): ResumeState | null {
  if (options.fromScratch) return null;
  const ctx = loadContext();
  if (!ctx || !ctx.phase || ctx.phase === "complete") return null;
  if (!topologyMatches(ctx, topology)) return null;
  const ageMs = ctx.savedAt ? Date.now() - new Date(ctx.savedAt).getTime() : Number.POSITIVE_INFINITY;
  if (!Number.isFinite(ageMs) || ageMs < 0 || ageMs > RESUME_TTL_MS) return null;
  return { resumeFromPhase: ctx.phase, ctx, ageMs };
}

/**
 * Returns true if `phase` was already completed in the previous run AND is
 * one of the phases we choose to skip on resume.
 */
export function isPhaseSkippable(phase: UpPhase, resumeFromPhase: UpPhase | null | undefined): boolean {
  if (!resumeFromPhase) return false;
  if (!SKIPPABLE.has(phase)) return false;
  const cur = PHASE_ORDER.indexOf(phase);
  const last = PHASE_ORDER.indexOf(resumeFromPhase);
  return cur >= 0 && last >= 0 && cur <= last;
}

/**
 * Persist that `phase` just completed. Merges any newly-resolved values
 * (e.g. acrLoginServer after Bicep) into the on-disk context. Topology
 * fields are stamped on every call so the resume guard has fresh values.
 */
export function markPhaseDone(
  phase: UpPhase,
  partial: Partial<DeploymentContext> = {},
  topology: ResumeTopology = {},
): void {
  const existing = loadContext() ?? {};
  const merged: DeploymentContext = {
    ...existing,
    ...partial,
    phase,
    subscription: topology.subscription ?? partial.subscription ?? existing.subscription,
    region: topology.region ?? partial.region ?? existing.region,
    resourceGroup: topology.resourceGroup ?? partial.resourceGroup ?? existing.resourceGroup,
    aksCluster: topology.aksCluster ?? partial.aksCluster ?? existing.aksCluster,
    sandboxName: topology.sandboxName ?? existing.sandboxName,
    sourceAcr: topology.sourceAcr ?? existing.sourceAcr,
  };
  if (phase !== "complete" && !merged.phaseStartedAt) {
    merged.phaseStartedAt = new Date().toISOString();
  }
  saveContext(merged);
}

/** Format a millisecond age compactly: "<1m", "12m", "3h", "2d". */
export function formatAge(ms: number): string {
  if (!Number.isFinite(ms) || ms < 0) return "?";
  const min = Math.floor(ms / 60_000);
  if (min < 1) return "<1m";
  if (min < 60) return `${min}m`;
  const hr = Math.floor(min / 60);
  if (hr < 24) return `${hr}h`;
  return `${Math.floor(hr / 24)}d`;
}

export const _internal = { PHASE_ORDER, SKIPPABLE, RESUME_TTL_MS };
