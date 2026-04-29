// Cluster-view render helpers — extracted from operator.ts startDashboard
// closure (S15.e.5) so the closure stays under the §4.2 800-LOC cap.
// Bodies byte-identical to originals; closure-captured `clusterData` and
// the two blessed widgets (clusterNodeBox, clusterInfoBox) become an
// explicit context object.

import type { ClusterHealth, NodeInfo } from "../types.js";

interface BlessedBox {
  setLabel(label: string): void;
  setContent(content: string): void;
}

export interface ClusterRenderContext {
  clusterData: ClusterHealth | null;
  clusterNodeBox: BlessedBox;
  clusterInfoBox: BlessedBox;
}

/** Render a small bar chart: ██░░ 34% */
export function makeBar(pct: number): string {
  const width = 6;
  const filled = Math.round((pct / 100) * width);
  const color = pct > 80 ? "red" : pct > 50 ? "yellow" : "green";
  const bar = "█".repeat(filled) + "░".repeat(width - filled);
  return `{${color}-fg}${bar}{/} ${pct}%`;
}

export function renderCluster(ctx: ClusterRenderContext): void {
  const { clusterData, clusterNodeBox, clusterInfoBox } = ctx;

  if (!clusterData) {
    clusterNodeBox.setContent(" {gray-fg}Loading cluster data...{/}");
    clusterInfoBox.setContent(" {gray-fg}Loading...{/}");
    return;
  }

  const c = clusterData;

  // ── Node table ──
  const readyCount = c.nodes.filter((n) => n.status === "Ready").length;
  const totalNodes = c.nodes.length;
  const nodeColor = readyCount === totalNodes ? "green" : readyCount > 0 ? "yellow" : "red";
  clusterNodeBox.setLabel(` 🖥  Nodes  {${nodeColor}-fg}${readyCount}/${totalNodes} Ready{/} `);

  // Build per-pool summary
  const pools = new Map<string, NodeInfo[]>();
  for (const n of c.nodes) {
    const p = n.pool || "default";
    if (!pools.has(p)) pools.set(p, []);
    pools.get(p)!.push(n);
  }

  const nodeLines: string[] = [];
  for (const [pool, nodes] of pools) {
    const poolReady = nodes.filter((n) => n.status === "Ready").length;
    const poolColor = poolReady === nodes.length ? "green" : "yellow";
    nodeLines.push(`{bold}{underline}Pool: ${pool}{/}  {${poolColor}-fg}${poolReady}/${nodes.length} Ready{/}`);
    nodeLines.push("");

    // Column headers
    nodeLines.push(` {cyan-fg}${"Node".padEnd(38)} ${"Status".padEnd(10)} ${"CPU".padEnd(12)} ${"Memory".padEnd(12)} Version{/}`);

    for (const n of nodes) {
      const dot = n.status === "Ready" ? "{green-fg}●{/}" : "{red-fg}●{/}";
      const shortName = n.name.length > 36 ? n.name.substring(0, 36) + ".." : n.name;

      // CPU bar
      const cpuNum = parseInt(n.cpuPct, 10);
      const cpuBar = !isNaN(cpuNum) ? makeBar(cpuNum) : n.cpuPct;

      // Mem bar
      const memNum = parseInt(n.memPct, 10);
      const memBar = !isNaN(memNum) ? makeBar(memNum) : n.memPct;

      nodeLines.push(` ${dot} ${shortName.padEnd(37)} ${n.status.padEnd(10)} ${cpuBar.padEnd(12)} ${memBar.padEnd(12)} ${n.version}`);
    }
    nodeLines.push("");
  }

  clusterNodeBox.setContent(nodeLines.join("\n"));

  // ── Cluster info ──
  const apiColor = !c.apiReachable ? "red" : c.apiLatencyMs < 1000 ? "green" : "yellow";
  const apiLabel = !c.apiReachable ? "unreachable" : `${c.apiLatencyMs}ms`;

  const infoLines: string[] = [
    `{bold}{underline}API Server{/}`,
    ` Health     {${apiColor}-fg}● ${apiLabel}{/}`,
    "",
    `{bold}{underline}Resources{/}`,
  ];

  // Aggregate CPU/mem from top data
  let totalCpuMilli = 0;
  let totalMemMi = 0;
  for (const n of c.nodes) {
    const cpuStr = n.cpuCores;
    if (cpuStr.endsWith("m")) totalCpuMilli += parseInt(cpuStr, 10) || 0;
    const memStr = n.memBytes;
    if (memStr.endsWith("Mi")) totalMemMi += parseInt(memStr, 10) || 0;
  }
  infoLines.push(
    ` CPU Used   ${totalCpuMilli}m (${c.nodes.length > 0 ? (c.nodes.reduce((s, n) => s + (parseInt(n.cpuPct, 10) || 0), 0) / c.nodes.length).toFixed(0) : 0}% avg)`,
    ` Mem Used   ${(totalMemMi / 1024).toFixed(1)}Gi (${c.nodes.length > 0 ? (c.nodes.reduce((s, n) => s + (parseInt(n.memPct, 10) || 0), 0) / c.nodes.length).toFixed(0) : 0}% avg)`,
  );

  // Quotas
  if (c.quotas.length > 0) {
    infoLines.push("", `{bold}{underline}Quotas{/}`);
    for (const q of c.quotas) {
      infoLines.push(` ${q.namespace}`);
      if (q.cpuHard !== "-") infoLines.push(`   CPU  ${q.cpuUsed}/${q.cpuHard}`);
      if (q.memHard !== "-") infoLines.push(`   Mem  ${q.memUsed}/${q.memHard}`);
    }
  }

  // PVCs
  if (c.pvcs.length > 0) {
    const bound = c.pvcs.filter((p) => p.phase === "Bound").length;
    const pending = c.pvcs.filter((p) => p.phase === "Pending").length;
    const pvColor = pending > 0 ? "yellow" : "green";
    infoLines.push("", `{bold}{underline}Storage{/}`);
    infoLines.push(` PVCs  {${pvColor}-fg}${bound} bound{/}${pending > 0 ? ` {yellow-fg}${pending} pending{/}` : ""} / ${c.pvcs.length} total`);
  }

  // Warnings
  if (c.warnings.length > 0) {
    infoLines.push("", `{bold}{underline}⚠  Recent Warnings{/}`);
    for (const w of c.warnings.slice(-6)) {
      const color = ["BackOff", "OOMKilled", "ImagePullBackOff", "Evicted", "NodeNotReady"].includes(w.reason) ? "red" : "yellow";
      infoLines.push(` {${color}-fg}${w.reason}{/} ${w.object}`);
      infoLines.push(`   {gray-fg}${w.message} (${w.time} ago){/}`);
    }
  } else {
    infoLines.push("", `{green-fg}✓ No warnings{/}`);
  }

  clusterInfoBox.setContent(infoLines.join("\n"));
}
