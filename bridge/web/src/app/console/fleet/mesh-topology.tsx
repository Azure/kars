"use client";

// kars Bridge Operator Console — Mesh topology, as a real node-link graph. Each
// agent sandbox is a node (harness-badged, governance-marked) positioned on an
// SVG canvas; a parent→child delegation is drawn as an actual connecting
// line — a REAL end-to-end-encrypted A2A link (a principal spawns a sub-agent
// and talks to it over the AGT mesh). No competitor has encrypted inter-agent
// comms, so none can show this. We draw only edges we can prove from the
// delegation graph; per-session ratchet/handshake state is not exposed as an
// API yet, so it is deliberately NOT fabricated here.
//
// Layout: principals laid out along a horizontal baseline; each principal's
// children fan out in an arc beneath it. Deterministic (not physics-
// simulated) so the same fleet always renders the same graph — appropriate at
// fleet sizes where a force simulation buys nothing but jitter.

import { useMemo, useState } from "react";
import type { Sandbox } from "@/lib/types";

function harnessTone(h: string | null): { stroke: string; fill: string } {
  const k = (h ?? "").toLowerCase();
  if (k.includes("openclaw")) return { stroke: "stroke-signal", fill: "fill-signal/15" };
  if (k.includes("hermes")) return { stroke: "stroke-accent", fill: "fill-accent/15" };
  if (k.includes("anthropic") || k.includes("claude")) return { stroke: "stroke-warning", fill: "fill-warning/15" };
  if (k.includes("langgraph") || k.includes("openai")) return { stroke: "stroke-ok", fill: "fill-ok/15" };
  return { stroke: "stroke-border", fill: "fill-surface-muted" };
}

interface LaidOutNode {
  s: Sandbox;
  x: number;
  y: number;
  depth: number;
  root: string;
}

interface RootLane {
  name: string;
  x: number;
  width: number;
}

const NODE_WIDTH = 196;
const NODE_HEIGHT = 58;
const CHILD_WIDTH = 164;
const CHILD_HEIGHT = 52;
const COLUMN_GAP = 28;
const ROW_GAP = 104;
const LANE_GAP = 44;
const PADDING_X = 28;
const PADDING_Y = 42;

function displayName(sandbox: Sandbox): string {
  if (!sandbox.parent) {
    return (sandbox.team ?? sandbox.name)
      .replace(/-repository-maintenance$/, " maintenance")
      .replace(/-repo-maintenance$/, " maintenance")
      .replaceAll("-", " ")
      .replace(/^\w/, (letter) => letter.toUpperCase());
  }
  for (const role of ["alert-monitor", "fix-generator", "pr-watcher"]) {
    if (sandbox.name.endsWith(role)) return role.replaceAll("-", " ");
  }
  const roleWorker = sandbox.name.match(/-(alert|fix|pr)-([0-9a-f]{8})$/i);
  if (roleWorker) {
    const role = roleWorker[1] === "pr" ? "PR" : roleWorker[1];
    return `${role} worker ${roleWorker[2]}`;
  }
  const parentPrefix = `${sandbox.parent}-`;
  const suffix = sandbox.name.startsWith(parentPrefix)
    ? sandbox.name.slice(parentPrefix.length)
    : sandbox.name;
  if (/^[0-9a-f]{8}$/i.test(suffix)) return `sub-agent ${suffix}`;
  return suffix;
}

function truncate(value: string, length: number): string {
  return value.length > length ? `${value.slice(0, length - 1)}…` : value;
}

export function MeshTopology({ sandboxes }: { sandboxes: Sandbox[] }) {
  const [hovered, setHovered] = useState<string | null>(null);

  const { nodes, edges, roots, lanes, width, height } = useMemo(() => {
    const live = sandboxes.filter((s) => !s.name.startsWith("bridge-orchestrator"));
    const byName = new Map(live.map((s) => [s.name, s]));
    const childrenOf = new Map<string, Sandbox[]>();
    for (const s of live) {
      if (s.parent && byName.has(s.parent)) {
        const list = childrenOf.get(s.parent);
        if (list) list.push(s);
        else childrenOf.set(s.parent, [s]);
      }
    }
    for (const children of childrenOf.values()) {
      children.sort((a, b) => a.name.localeCompare(b.name));
    }
    const rootList = live
      .filter((s) => !(s.parent && byName.has(s.parent)))
      .sort((a, b) => (a.team ?? a.name).localeCompare(b.team ?? b.name));

    const laid: LaidOutNode[] = [];
    const edgeList: { from: string; to: string }[] = [];
    const laneList: RootLane[] = [];
    const leaves = new Map<string, number>();
    let maxDepth = 0;

    function leafCount(name: string, visiting = new Set<string>()): number {
      if (leaves.has(name)) return leaves.get(name)!;
      if (visiting.has(name)) return 1;
      const next = new Set(visiting);
      next.add(name);
      const children = childrenOf.get(name) ?? [];
      const count = Math.max(
        1,
        children.reduce((total, child) => total + leafCount(child.name, next), 0),
      );
      leaves.set(name, count);
      return count;
    }

    function subtreeWidth(name: string): number {
      return Math.max(NODE_WIDTH + 28, leafCount(name) * (CHILD_WIDTH + COLUMN_GAP));
    }

    function place(name: string, left: number, span: number, depth: number, root: string): number {
      const sandbox = byName.get(name);
      if (!sandbox) return left + span / 2;
      maxDepth = Math.max(maxDepth, depth);
      const children = childrenOf.get(name) ?? [];
      let x = left + span / 2;
      if (children.length > 0) {
        const widths = children.map((child) => subtreeWidth(child.name));
        const total = widths.reduce((sum, value) => sum + value, 0);
        let childLeft = left + (span - total) / 2;
        const childXs = children.map((child, index) => {
          const childX = place(child.name, childLeft, widths[index], depth + 1, root);
          edgeList.push({ from: name, to: child.name });
          childLeft += widths[index];
          return childX;
        });
        x = (childXs[0] + childXs[childXs.length - 1]) / 2;
      }
      laid.push({
        s: sandbox,
        x,
        y: PADDING_Y + depth * ROW_GAP,
        depth,
        root,
      });
      return x;
    }

    let cursor = PADDING_X;
    for (const root of rootList) {
      const laneWidth = subtreeWidth(root.name);
      laneList.push({ name: root.name, x: cursor, width: laneWidth });
      place(root.name, cursor, laneWidth, 0, root.name);
      cursor += laneWidth + LANE_GAP;
    }

    const graphWidth = Math.max(cursor - LANE_GAP + PADDING_X, 720);
    const graphHeight = PADDING_Y * 2 + maxDepth * ROW_GAP + NODE_HEIGHT;
    return {
      nodes: laid,
      edges: edgeList,
      roots: rootList,
      lanes: laneList,
      width: graphWidth,
      height: graphHeight,
    };
  }, [sandboxes]);

  if (roots.length === 0) return null;

  const byName = new Map(nodes.map((n) => [n.s.name, n]));
  const executing = nodes.filter((node) => node.s.executing === true).length;

  return (
    <section className="rounded-2xl border border-border bg-surface p-5">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h2 className="text-sm font-semibold">Team delegation topology</h2>
          <p className="mt-0.5 max-w-3xl text-xs text-foreground-muted">
            Each lane is one principal-led team run. Curved links show proven encrypted delegations,
            including nested sub-agents.
          </p>
        </div>
        <div className="flex flex-wrap gap-1.5 text-[11px]">
          <span className="rounded-full border border-border bg-surface-muted px-2.5 py-1 text-foreground-muted">
            {roots.length} principal{roots.length === 1 ? "" : "s"}
          </span>
          <span className="rounded-full border border-accent/25 bg-accent/10 px-2.5 py-1 text-accent">
            {edges.length} encrypted link{edges.length === 1 ? "" : "s"}
          </span>
          <span className="rounded-full border border-ok/25 bg-ok/10 px-2.5 py-1 text-ok">
            {executing} executing
          </span>
        </div>
      </div>

      <div className="mt-4 overflow-x-auto rounded-xl border border-border bg-surface-muted/20">
        <svg
          viewBox={`0 0 ${width} ${height}`}
          className="block"
          style={{ width: `${Math.max(width, 720)}px`, minWidth: "100%", height: `${height}px` }}
          role="img"
          aria-label="Team delegation topology: principal and sub-agent sandboxes connected by encrypted links"
        >
          <defs>
            <marker
              id="mesh-arrow"
              viewBox="0 0 8 8"
              refX="7"
              refY="4"
              markerWidth="5"
              markerHeight="5"
              orient="auto-start-reverse"
            >
              <path d="M 0 0 L 8 4 L 0 8 z" className="fill-accent/55" />
            </marker>
          </defs>

          {lanes.map((lane, index) => (
            <rect
              key={lane.name}
              x={lane.x + 2}
              y={10}
              width={lane.width - 4}
              height={height - 20}
              rx={14}
              className={index % 2 === 0 ? "fill-surface/70" : "fill-surface-muted/35"}
              stroke="currentColor"
              strokeWidth={1}
              strokeDasharray="3 5"
              opacity={0.65}
            />
          ))}

          {edges.map((e) => {
            const from = byName.get(e.from);
            const to = byName.get(e.to);
            if (!from || !to) return null;
            const active = hovered === e.from || hovered === e.to;
            const startY = from.y + (from.depth === 0 ? NODE_HEIGHT : CHILD_HEIGHT) / 2;
            const endY = to.y - CHILD_HEIGHT / 2;
            const bend = Math.max(24, (endY - startY) * 0.55);
            return (
              <path
                key={`${e.from}->${e.to}`}
                d={`M ${from.x} ${startY} C ${from.x} ${startY + bend}, ${to.x} ${endY - bend}, ${to.x} ${endY}`}
                fill="none"
                className={active ? "stroke-accent" : "stroke-accent/40"}
                strokeWidth={active ? 2.4 : 1.4}
                markerEnd="url(#mesh-arrow)"
              />
            );
          })}

          {nodes.map((n) => {
            const tone = harnessTone(n.s.runtime);
            const running = n.s.phase === "Running" || n.s.phase === "Ready";
            const statusFill =
              running && n.s.executing
                ? "fill-ok"
                : running
                  ? "fill-warning"
                  : "fill-foreground-muted";
            const principal = n.depth === 0;
            const nodeWidth = principal ? NODE_WIDTH : CHILD_WIDTH;
            const nodeHeight = principal ? NODE_HEIGHT : CHILD_HEIGHT;
            const selected = hovered === n.s.name;
            const label = truncate(displayName(n.s), principal ? 29 : 22);
            const parentNode = n.s.parent ? byName.get(n.s.parent) : null;
            const relationship = principal
              ? "team principal"
              : n.depth === 1
                ? "specialist"
                : `spawned by ${parentNode ? displayName(parentNode.s) : "parent"}`;
            return (
              <g
                key={n.s.name}
                onMouseEnter={() => setHovered(n.s.name)}
                onMouseLeave={() => setHovered(null)}
                className="cursor-pointer"
              >
                <rect
                  x={n.x - nodeWidth / 2}
                  y={n.y - nodeHeight / 2}
                  width={nodeWidth}
                  height={nodeHeight}
                  rx={principal ? 13 : 10}
                  className={`${tone.fill} ${selected ? "stroke-accent" : tone.stroke}`}
                  strokeWidth={selected ? 2.5 : principal ? 1.8 : 1.3}
                />
                <circle
                  cx={n.x - nodeWidth / 2 + 13}
                  cy={n.y - nodeHeight / 2 + 13}
                  r={4}
                  className={statusFill}
                  stroke="white"
                  strokeWidth={1}
                />
                {n.s.governed && (
                  <text
                    x={n.x + nodeWidth / 2 - 13}
                    y={n.y - nodeHeight / 2 + 17}
                    textAnchor="middle"
                    className="fill-foreground text-[10px] font-bold"
                  >
                    ✓
                  </text>
                )}
                <text
                  x={n.x}
                  y={n.y - 2}
                  textAnchor="middle"
                  className={`text-[10px] font-semibold ${principal ? "fill-foreground" : "fill-foreground-muted"}`}
                >
                  {label}
                </text>
                <text
                  x={n.x}
                  y={n.y + 14}
                  textAnchor="middle"
                  className="fill-foreground-muted text-[8px]"
                >
                  {truncate(relationship, 24)} ·{" "}
                  {n.s.executing ? "executing" : (n.s.phase ?? "unknown").toLowerCase()}
                </text>
              </g>
            );
          })}
        </svg>
      </div>

      {hovered && byName.get(hovered) && (
        <div className="mt-3 flex flex-wrap items-center gap-2 rounded-lg border border-border bg-surface-muted/40 px-3 py-2 text-[11px]">
          <span className="font-mono font-medium">{hovered}</span>
          {byName.get(hovered)!.s.team && (
            <span className="text-foreground-muted">· team {byName.get(hovered)!.s.team}</span>
          )}
          {byName.get(hovered)!.s.runtime && <span className="text-foreground-muted">· {byName.get(hovered)!.s.runtime}</span>}
          {byName.get(hovered)!.s.parent && (
            <span className="text-foreground-muted">· spawned by {byName.get(hovered)!.s.parent}</span>
          )}
          {byName.get(hovered)!.s.governed && <span className="text-ok">· governed</span>}
          <span className="text-foreground-muted">· {byName.get(hovered)!.s.phase ?? "unknown"}</span>
        </div>
      )}

      <div className="mt-3 flex flex-wrap items-center justify-between gap-2 text-[11px] text-foreground-muted">
        <div className="flex flex-wrap gap-3">
          <span className="inline-flex items-center gap-1.5">
            <span className="h-2 w-2 rounded-full bg-ok" /> executing
          </span>
          <span className="inline-flex items-center gap-1.5">
            <span className="h-2 w-2 rounded-full bg-warning" /> idle / retained
          </span>
          <span className="inline-flex items-center gap-1.5">
            <span className="font-bold text-foreground">✓</span> governed
          </span>
        </div>
        <span>Only controller-recorded parent→child delegations are shown; links are never inferred.</span>
      </div>
    </section>
  );
}
