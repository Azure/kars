// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Operator dashboard action helpers — extracted from startDashboard
// (S15.e.4) so the closure stays under the §4.2 800-LOC cap. Bodies
// are byte-identical to the originals; closure-captured `sandboxes`,
// `activityLog`, and `kubeContext` become an explicit context object.
//
// All four helpers shell out via the inference-router pod (port 8443)
// using the existing `kctl` wrapper for `--context` injection.

import { execa } from "execa";
import { kctl } from "./helpers.js";
import type { EgressDomain, SandboxInfo } from "./types.js";

export interface ActionContext {
  getSandboxes: () => SandboxInfo[];
  activityLog: { log(msg: string): void };
  kubeContext?: string;
}

export interface OperatorActions {
  approveDomain(domain: EgressDomain): Promise<void>;
  denyDomain(domain: EgressDomain): Promise<void>;
  enforceEgress(sb: SandboxInfo): Promise<void>;
  learnEgress(sb: SandboxInfo): Promise<void>;
}

export function createActions(ctx: ActionContext): OperatorActions {
  const { activityLog, kubeContext } = ctx;

  async function approveDomain(domain: EgressDomain): Promise<void> {
    const sb = ctx.getSandboxes().find((s) => s.name === domain.sandbox);
    if (!sb?.podName) {
      activityLog.log(`{red-fg}✗ No pod for{/} ${domain.sandbox}`);
      return;
    }
    try {
      await execa("kubectl", kctl([
        "exec", "-n", domain.namespace, sb.podName,
        "-c", "inference-router", "--",
        "curl", "-s", "-X", "POST",
        "-H", "Content-Type: application/json",
        "-d", JSON.stringify({ domain: domain.domain }),
        "http://localhost:8443/egress/approve",
      ], kubeContext), { stdio: "pipe" });
      activityLog.log(`{green-fg}✓ Approved{/} ${domain.domain}`);
    } catch (e: any) {
      activityLog.log(`{red-fg}✗ Approve fail:{/} ${e.message?.substring(0, 50)}`);
    }
  }

  async function denyDomain(domain: EgressDomain): Promise<void> {
    const sb = ctx.getSandboxes().find((s) => s.name === domain.sandbox);
    if (!sb?.podName) {
      activityLog.log(`{red-fg}✗ No pod for{/} ${domain.sandbox}`);
      return;
    }
    try {
      await execa("kubectl", kctl([
        "exec", "-n", domain.namespace, sb.podName,
        "-c", "inference-router", "--",
        "curl", "-s", "-X", "POST",
        "-H", "Content-Type: application/json",
        "-d", JSON.stringify({ domain: domain.domain }),
        "http://localhost:8443/egress/deny",
      ], kubeContext), { stdio: "pipe" });
      activityLog.log(`{yellow-fg}✗ Denied{/} ${domain.domain}`);
    } catch (e: any) {
      activityLog.log(`{red-fg}✗ Deny fail:{/} ${e.message?.substring(0, 50)}`);
    }
  }

  async function enforceEgress(sb: SandboxInfo): Promise<void> {
    if (!sb.podName) return;
    try {
      await execa("kubectl", kctl([
        "exec", "-n", sb.namespace, sb.podName,
        "-c", "inference-router", "--",
        "curl", "-s", "-X", "POST",
        "http://localhost:8443/egress/enforce",
      ], kubeContext), { stdio: "pipe" });
      // Persist to CRD so the controller preserves the mode across restarts
      await execa("kubectl", kctl([
        "patch", "clawsandbox", sb.name, "-n", "azureclaw-system",
        "--type", "merge", "-p",
        JSON.stringify({ spec: { networkPolicy: { learnEgress: false } } }),
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
      await execa("kubectl", kctl([
        "exec", "-n", sb.namespace, sb.podName,
        "-c", "inference-router", "--",
        "curl", "-s", "-X", "POST",
        "http://localhost:8443/egress/learn",
      ], kubeContext), { stdio: "pipe" });
      // Persist to CRD so the controller preserves the mode across restarts
      await execa("kubectl", kctl([
        "patch", "clawsandbox", sb.name, "-n", "azureclaw-system",
        "--type", "merge", "-p",
        JSON.stringify({ spec: { networkPolicy: { learnEgress: true } } }),
      ], kubeContext), { stdio: "pipe" }).catch(() => {});
      activityLog.log(`{yellow-fg}📖 Learning{/} ${sb.name}`);
      activityLog.log(`{gray-fg}   ↳ saved to CRD — may trigger pod restart{/}`);
    } catch (e: any) {
      activityLog.log(`{red-fg}✗ Learn fail:{/} ${e.message?.substring(0, 50)}`);
    }
  }

  return { approveDomain, denyDomain, enforceEgress, learnEgress };
}
