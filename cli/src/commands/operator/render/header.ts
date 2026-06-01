// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Header + health-summary render helpers — extracted from operator.ts
// startDashboard closure (S15.e.5c) so the closure stays under the
// §4.2 800-LOC cap. Bodies byte-identical to the originals;
// closure-captured state is passed via `HeaderRenderContext`.

import type { SandboxInfo, ClusterHealth, MeshHealth } from "../types.js";

interface HeaderBox {
  setContent(content: string): void;
}

interface HeaderScreen {
  width: string | number;
}

export interface HeaderRenderContext {
  sandboxes: SandboxInfo[];
  clusterData: ClusterHealth | null;
  meshHealth: MeshHealth | null;
  isRefreshing: boolean;
  spinFrames: readonly string[];
  spinIdx: number;
  clusterName: string;
  viewMode: "agents" | "topology" | "cluster";
  totalEgressCount: number;
  header: HeaderBox;
  screen: HeaderScreen;
}

export function healthSummary(sandboxes: SandboxInfo[]): string {
  const total = sandboxes.length;
  if (total === 0) return "{gray-fg}no agents{/}";
  const h = sandboxes.filter((s) => s.health === "healthy").length;
  const d = sandboxes.filter((s) => s.health === "degraded").length;
  const x = sandboxes.filter((s) => s.health === "down").length;
  const parts: string[] = [];
  if (h > 0) parts.push(`{green-fg}${h} healthy{/}`);
  if (d > 0) parts.push(`{yellow-fg}${d} degraded{/}`);
  if (x > 0) parts.push(`{red-fg}${x} down{/}`);
  return `${total} agent${total === 1 ? "" : "s"} (${parts.join(", ")})`;
}

export function renderHeader(ctx: HeaderRenderContext): void {
  const {
    sandboxes, clusterData, meshHealth,
    isRefreshing, spinFrames, spinIdx,
    clusterName, viewMode, totalEgressCount,
    header, screen,
  } = ctx;

  const now = new Date().toLocaleTimeString();
  const spin = isRefreshing ? `{cyan-fg}${spinFrames[spinIdx]}{/} ` : "";
  const cName = `{gray-fg}${clusterName}{/}`;

  // Cluster health indicator
  let clusterTag = "";
  if (clusterData) {
    const readyNodes = clusterData.nodes.filter((n) => n.status === "Ready").length;
    const totalNodes = clusterData.nodes.length;
    const nColor = readyNodes === totalNodes ? "green" : readyNodes > 0 ? "yellow" : "red";
    const apiTag = clusterData.apiReachable ? "{green-fg}●{/}" : "{red-fg}●{/}";
    clusterTag = `${apiTag} API  {${nColor}-fg}${readyNodes}/${totalNodes}{/} nodes  │  `;
  }

  // Mesh health indicator
  let meshTag = "";
  if (meshHealth) {
    const relayColor = meshHealth.relayReady ? "green" : "red";
    const regColor = meshHealth.registryReady ? (meshHealth.registryReadyPods < meshHealth.registryPods ? "yellow" : "green") : "red";
    const regCount = meshHealth.registryPods > 0 ? ` (${meshHealth.registryReadyPods}/${meshHealth.registryPods})` : "";
    // Phase 6.c — show the Entra-verification toggle inline with
    // the relay indicator. Three states:
    //   - null  → relay /health unreachable / pre-6.c image → no tag
    //   - false → relay deployed but operator hasn't opted in
    //              → grey "auth:open" (matches today's behaviour)
    //   - true  → relay verifying tokens
    //              → green "auth:entra N/M" with verified-over-total counts
    let authTag = "";
    if (meshHealth.entraVerifyEnabled === true) {
      const verified = meshHealth.verifiedAgents ?? 0;
      const total = meshHealth.connectedAgents ?? 0;
      // Color the count by verification ratio so operators see at a glance
      // that "0/1" is a problem and "3/3" is healthy:
      //   - green   → all connected agents are Entra-verified
      //   - yellow  → some connected but not yet verified (warming up,
      //               or one peer can't reach Entra → fail-open)
      //   - red     → relay is in entra-mode but NO agent has verified
      //               (likely misconfigured Entra app or wrong audience)
      const authColor =
        total === 0 ? "green" :
        verified === total ? "green" :
        verified === 0 ? "red" : "yellow";
      authTag = `  {${authColor}-fg}🔐 auth:entra ${verified}/${total} verified{/}`;
    } else if (meshHealth.entraVerifyEnabled === false) {
      authTag = `  {gray-fg}auth:open (no Entra verification){/}`;
    }
    meshTag = `{${relayColor}-fg}●{/} relay${authTag}  {${regColor}-fg}●{/} registry${regCount}  │  `;
  }

  const viewLabel = viewMode === "cluster" ? "{blue-fg}{bold}[CLUSTER]{/bold}{/}  │  " : "";
  const title = ` ${spin}{bold}kars Operator{/bold}  │  ${cName}  │  ${viewLabel}`;
  const summary = healthSummary(sandboxes);
  const stats = `${clusterTag}${meshTag}${summary}  │  ${totalEgressCount} domain(s)  │  {gray-fg}${now}{/}`;
  const shortStats = `${summary}  │  {gray-fg}${now}{/}`;

  // Measure visible width (strip blessed tags, account for double-wide emoji)
  const visWidth = (s: string) => {
    const plain = s.replace(/\{[^}]*\}/g, "");
    // Each emoji/surrogate pair occupies ~2 columns
    let w = 0;
    for (const ch of plain) {
      w += ch.codePointAt(0)! > 0xffff ? 2 : 1;
    }
    return w;
  };
  const maxW = (screen.width as number) - 2;
  const full = title + stats;
  if (visWidth(full) > maxW) {
    header.setContent(title + shortStats);
  } else {
    header.setContent(full);
  }
}
