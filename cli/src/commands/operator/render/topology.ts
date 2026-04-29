// Topology-view render helper — extracted from operator.ts startDashboard
// closure (S15.e.5) so the closure stays under the §4.2 800-LOC cap.
// Body byte-identical to the original; closure-captured `sandboxes`,
// `securityStates`, and `topologyBox` become an explicit context object.

import type { SandboxInfo, SecurityState } from "../types.js";

interface BlessedBox {
  setContent(content: string): void;
}

export interface TopologyRenderContext {
  sandboxes: SandboxInfo[];
  securityStates: Map<string, SecurityState>;
  topologyBox: BlessedBox;
}

export function renderTopology(ctx: TopologyRenderContext): void {
  const { sandboxes, securityStates, topologyBox } = ctx;

  if (sandboxes.length === 0) {
    topologyBox.setContent("{gray-fg}No agents{/}");
    return;
  }

  const parents = sandboxes.filter((s) => s.role !== "sub-agent");
  const children = sandboxes.filter((s) => s.role === "sub-agent");
  const totalMesh = [...securityStates.values()].reduce((n, s) => n + s.agtMeshSessions, 0);

  const lines: string[] = [];
  lines.push(`{bold}Mesh Topology{/}  ${sandboxes.length} agent${sandboxes.length !== 1 ? "s" : ""}  ·  ${totalMesh} session${totalMesh !== 1 ? "s" : ""}  ·  {gray-fg}[t] back to table{/}`);
  lines.push("");

  function statusIcon(health: string): string {
    return health === "healthy" ? "{green-fg}*{/}" :
           health === "dormant" ? "{blue-fg}~{/}" :
           health === "pending" ? "{yellow-fg}o{/}" :
           health === "degraded" ? "{yellow-fg}!{/}" : "{red-fg}x{/}";
  }

  // Fixed column width for all boxes — keeps alignment clean at scale
  const COL_W = 26;  // inner content width
  const BOX_W = COL_W + 4; // +4 for "│ " and " │"
  const CELL_W = BOX_W + 2; // +2 gap between columns

  // Visual width of a string, counting emoji (surrogate pairs) as 2 cells
  function visualLen(s: string): number {
    const plain = s.replace(/\{[^}]+\}/g, "");
    let w = 0;
    for (const ch of plain) {
      w += ch.codePointAt(0)! > 0xFFFF ? 2 : 1;
    }
    return w;
  }

  // Fit string to exactly w visual columns (pad or truncate)
  function fitVis(s: string, w: number): string {
    const vw = visualLen(s);
    if (vw <= w) return s + " ".repeat(w - vw);
    let used = 0;
    let result = "";
    let i = 0;
    while (i < s.length) {
      if (s[i] === "{") {
        const end = s.indexOf("}", i);
        if (end !== -1) { result += s.slice(i, end + 1); i = end + 1; continue; }
      }
      const cp = s.codePointAt(i)!;
      const cw = cp > 0xFFFF ? 2 : 1;
      if (used + cw > w - 1) break;
      result += String.fromCodePoint(cp);
      i += cp > 0xFFFF ? 2 : 1;
      used += cw;
    }
    return result + "…" + " ".repeat(Math.max(0, w - used - 1));
  }

  function makeBox(name: string, icon: string, line2: string, line3: string): string[] {
    const border = "─".repeat(COL_W + 2);
    return [
      `┌${border}┐`,
      `│ ${icon} ${fitVis(name, COL_W - 2)} │`,
      `│ ${fitVis(line2, COL_W)} │`,
      `│ ${fitVis(line3, COL_W)} │`,
      `└${border}┘`,
    ];
  }

  for (const p of parents) {
    const sec = securityStates.get(p.name);
    const icon = statusIcon(p.health);
    const mode = sec?.egressMode === "enforcing" ? "{green-fg}enforce{/}" :
                 sec?.egressMode === "learning" ? "{yellow-fg}learn{/}" : "";
    const meshInfo = sec ? `↑${sec.agtMeshSent} ↓${sec.agtMeshReceived}` : "";
    const peerCount = sec?.agtTrustScores.filter((t) => t.agent !== p.name && sandboxes.some((s) => s.name === t.agent) && (t.interactions > 0 || t.lastSeen)).length || 0;

    const rtLabel = p.runtime === "docker" ? "D" : "C";
    const pBox = makeBox(p.name, icon, `${rtLabel} ${p.model}  ${mode}`, `${peerCount} peer${peerCount !== 1 ? "s" : ""}  ${meshInfo}  ${p.age}`);
    for (const l of pBox) lines.push(`  ${l}`);

    const subs = children.filter((c) => c.parent === p.name);
    if (subs.length > 0) {
      // Vertical connector from parent center
      const parentCenter = Math.floor(BOX_W / 2) + 2; // +2 for indent
      lines.push(" ".repeat(parentCenter) + "│");

      if (subs.length === 1) {
        // Single child — straight line down
        lines.push(" ".repeat(parentCenter) + "│");
        const childSec = securityStates.get(subs[0].name);
        const ci = statusIcon(subs[0].health);
        const cMesh = childSec ? `↑${childSec.agtMeshSent} ↓${childSec.agtMeshReceived}` : "";
        const cBox = makeBox(subs[0].name, ci, subs[0].model, cMesh);
        // Center single child under parent
        const childIndent = Math.max(2, parentCenter - Math.floor(BOX_W / 2));
        for (const l of cBox) lines.push(" ".repeat(childIndent) + l);
      } else {
        // Multiple children — horizontal bar with drops
        // Each child occupies CELL_W chars; center the group under parent
        const totalGroupW = subs.length * CELL_W - 2; // -2 because last has no trailing gap
        const groupStart = Math.max(4, parentCenter - Math.floor(totalGroupW / 2));

        // Horizontal bar: ├──┬──┬──┤ centered under parent's │
        let bar = " ".repeat(groupStart);
        for (let i = 0; i < subs.length; i++) {
          const mid = Math.floor(BOX_W / 2);
          if (i === 0) {
            bar += "┌" + "─".repeat(mid);
          } else {
            bar += "─".repeat(mid) + "┬";
            if (i < subs.length - 1) {
              bar += "─".repeat(CELL_W - mid - 1);
            }
          }
          if (i === subs.length - 1 && i > 0) {
            bar += "─".repeat(mid) + "┐";
          }
          if (i === 0 && subs.length > 1) {
            bar += "─".repeat(CELL_W - mid - 1);
          }
        }
        lines.push(bar);

        // Drop stubs: │ at center of each column
        let stubs = " ".repeat(groupStart);
        for (let i = 0; i < subs.length; i++) {
          const mid = Math.floor(BOX_W / 2);
          stubs += " ".repeat(mid) + "│" + " ".repeat(CELL_W - mid - 1);
        }
        lines.push(stubs);

        // Render child boxes side-by-side
        const childBoxes: string[][] = [];
        for (const s of subs) {
          const childSec = securityStates.get(s.name);
          const ci = statusIcon(s.health);
          const cMesh = childSec ? `↑${childSec.agtMeshSent} ↓${childSec.agtMeshReceived}` : "";
          childBoxes.push(makeBox(s.name, ci, s.model, cMesh));
        }
        for (let row = 0; row < 5; row++) {
          let line = " ".repeat(groupStart);
          for (let i = 0; i < childBoxes.length; i++) {
            line += childBoxes[i][row];
            if (i < childBoxes.length - 1) line += "  ";
          }
          lines.push(line);
        }
      }

      // Peer-to-peer mesh links
      const peerLinks: string[] = [];
      for (const s of subs) {
        const childSec = securityStates.get(s.name);
        const peers = childSec?.agtTrustScores.filter((t) =>
          t.agent !== s.name && subs.some((sub) => sub.name === t.agent) && t.interactions > 0
        ) || [];
        for (const peer of peers) {
          const key = [s.name, peer.agent].sort().join(":");
          if (!peerLinks.includes(key)) {
            peerLinks.push(key);
            const c = peer.score >= 600 ? "green" : peer.score >= 400 ? "yellow" : "red";
            lines.push(`         {${c}-fg}⟷{/} ${s.name} ↔ ${peer.agent} {gray-fg}(${peer.interactions} msg${peer.interactions !== 1 ? "s" : ""}, trust: ${peer.score}){/}`);
          }
        }
      }
    }

    lines.push("");
  }

  // Orphan sub-agents (parent destroyed but children remain)
  const orphans = children.filter((c) => !parents.some((p) => p.name === c.parent));
  if (orphans.length > 0) {
    lines.push("{gray-fg}─── Orphaned agents ───{/}");
    for (const s of orphans) {
      const icon = statusIcon(s.health);
      lines.push(`  ${icon} ${s.name} {gray-fg}(${s.model}) parent: ${s.parent || "?"}{/}`);
    }
  }

  topologyBox.setContent(lines.join("\n"));
}
