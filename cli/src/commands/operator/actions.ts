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
// surface is now `azureclaw policy sign --kind egress-allowlist`
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
        "patch", "clawsandbox", sb.name, "-n", "azureclaw-system",
        "--type", "merge", "-p",
        JSON.stringify({ spec: { networkPolicy: { egressMode: "Strict" } } }),
      ], kubeContext), { stdio: "pipe" });
      activityLog.log(`{green-fg}🔒 Enforced{/} ${sb.name}`);
      activityLog.log(`{gray-fg}   ↳ saved to CRD — may trigger pod restart{/}`);
    } catch (e: any) {
      activityLog.log(`{red-fg}✗ Enforce fail:{/} ${e.message?.substring(0, 50)}`);
    }
  }

  async function learnEgress(sb: SandboxInfo): Promise<void> {
    if (!sb.podName) return;
    try {
      await execa("kubectl", kctl([
        "exec", "-n", sb.namespace, sb.podName,
        "-c", "inference-router", "--",
        "curl", "-s", "-X", "POST",
        "http://localhost:8443/egress/learn",
      ], kubeContext), { stdio: "pipe" });
      await execa("kubectl", kctl([
        "patch", "clawsandbox", sb.name, "-n", "azureclaw-system",
        "--type", "merge", "-p",
        JSON.stringify({ spec: { networkPolicy: { egressMode: "Learn" } } }),
      ], kubeContext), { stdio: "pipe" }).catch(() => {});
      activityLog.log(`{yellow-fg}📖 Learning{/} ${sb.name}`);
      activityLog.log(`{gray-fg}   ↳ saved to CRD — may trigger pod restart{/}`);
    } catch (e: any) {
      activityLog.log(`{red-fg}✗ Learn fail:{/} ${e.message?.substring(0, 50)}`);
    }
  }

  return { enforceEgress, learnEgress };
}
