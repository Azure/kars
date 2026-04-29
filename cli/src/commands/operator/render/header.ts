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
    meshTag = `{${relayColor}-fg}●{/} relay  {${regColor}-fg}●{/} registry${regCount}  │  `;
  }

  const viewLabel = viewMode === "cluster" ? "{blue-fg}{bold}[CLUSTER]{/bold}{/}  │  " : "";
  const title = ` ${spin}{bold}AzureClaw Operator{/bold}  │  ${cName}  │  ${viewLabel}`;
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
