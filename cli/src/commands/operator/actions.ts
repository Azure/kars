// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Operator dashboard action helpers — extracted from startDashboard
// (S15.e.4) so the closure stays under the §4.2 800-LOC cap.
//
// Both helpers shell out via the inference-router pod (port 8443)
// using the existing `kctl` wrapper for `--context` injection.
//
// Slice 5c.1: `approveDomain` / `denyDomain` were removed alongside
// the `/egress/approve` and `/egress/deny` router endpoints. Domain
// approval is no longer an in-memory side door — the allowlist is
// signed and published by the controller, so the operator-facing
// surface is now `kars policy sign --kind egress-allowlist`
// (future Slice 1c.2 generalization). `enforceEgress` also no
// longer hits the deleted `/egress/enforce` route; the CRD patch
// is the authoritative path.

import { execa } from "execa";
import { kctl } from "./helpers.js";
import type { SandboxInfo } from "./types.js";

export interface ActionContext {
  getSandboxes: () => SandboxInfo[];
  activityLog: { log(msg: string): void };
  kubeContext?: string;
}

export interface OperatorActions {
  enforceEgress(sb: SandboxInfo): Promise<void>;
  learnEgress(sb: SandboxInfo): Promise<void>;
}

export function createActions(ctx: ActionContext): OperatorActions {
  const { activityLog, kubeContext } = ctx;

  async function enforceEgress(sb: SandboxInfo): Promise<void> {
    if (!sb.podName) return;
    try {
      await execa("kubectl", kctl([
        "patch", "karssandbox", sb.name, "-n", "kars-system",
        "--type", "merge", "-p",
        JSON.stringify({ spec: { networkPolicy: { egressMode: "Strict" } } }),
      ], kubeContext), { stdio: "pipe" });
      // Best-effort live toggle so Strict takes effect without waiting for the
      // controller to roll the pod. Never let a probe failure (missing admin
      // token, older router) surface as an "enforce failed" — the CRD patch
      // above is the authoritative source of truth.
      await execa("kubectl", kctl([
        "exec", "-n", sb.namespace, sb.podName,
        "-c", "inference-router", "--",
        "/usr/local/bin/kars-inference-router", "probe", "POST", "/egress/learn",
        JSON.stringify({ enabled: false }),
      ], kubeContext), { stdio: "pipe" }).catch(() => {});
      activityLog.log(`{green-fg}🔒 Enforced{/} ${sb.name}`);
      activityLog.log(`{gray-fg}   ↳ saved to CRD — may trigger pod restart{/}`);
    } catch (e: any) {
      activityLog.log(`{red-fg}✗ Enforce fail:{/} ${e.message?.substring(0, 50)}`);
    }
  }

  async function learnEgress(sb: SandboxInfo): Promise<void> {
    if (!sb.podName) return;
    try {
      // Authoritative: the CRD `egressMode` drives the router's EGRESS_MODE on
      // the next reconcile. Patch it FIRST (mirrors enforceEgress, which only
      // patches the CRD) so the mode change is durable even if the live toggle
      // below can't reach the router.
      await execa("kubectl", kctl([
        "patch", "karssandbox", sb.name, "-n", "kars-system",
        "--type", "merge", "-p",
        JSON.stringify({ spec: { networkPolicy: { egressMode: "Learn" } } }),
      ], kubeContext), { stdio: "pipe" });
      // Best-effort live toggle so learn mode takes effect immediately without
      // waiting for a pod restart. MUST send {enabled:true} (an empty body
      // defaults the router to enabled:false, i.e. it would DISABLE learn).
      // Wrapped in its own catch so a probe failure can never block or fail the
      // authoritative CRD patch above — the root cause of the prior
      // "cannot move Strict → Learn" error.
      await execa("kubectl", kctl([
        "exec", "-n", sb.namespace, sb.podName,
        "-c", "inference-router", "--",
        "/usr/local/bin/kars-inference-router", "probe", "POST", "/egress/learn",
        JSON.stringify({ enabled: true }),
      ], kubeContext), { stdio: "pipe" }).catch(() => {});
      activityLog.log(`{yellow-fg}📖 Learning{/} ${sb.name}`);
      activityLog.log(`{gray-fg}   ↳ saved to CRD — may trigger pod restart{/}`);
    } catch (e: any) {
      activityLog.log(`{red-fg}✗ Learn fail:{/} ${e.message?.substring(0, 50)}`);
    }
  }

  return { enforceEgress, learnEgress };
}
