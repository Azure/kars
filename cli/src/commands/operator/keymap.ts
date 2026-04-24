/**
 * Operator TUI keyboard bindings + status-bar help strings.
 *
 * Extracted from `cli/src/commands/operator.ts` per
 * `docs/implementation-plan.md` §4.2 (monotonic-decrease LOC budget) and
 * §6 item 12 (Phase 0 decomposition slice). Pure data — no `blessed` or
 * `commander` imports, no I/O, easily unit-testable.
 *
 * When adding a new binding: update both the BINDINGS table AND the
 * relevant `statusBarFor*` function, then wire the handler in
 * `operator.ts`. Divergence is a CI warning (planned for Phase 1).
 */

/** Canonical agent-view TUI key bindings (informational). */
export interface KeyBinding {
  readonly key: string;
  readonly action: string;
  readonly scope: "global" | "agents" | "egress" | "cluster" | "topology" | "overlay";
}

export const BINDINGS: readonly KeyBinding[] = [
  { key: "Tab",      action: "cycle focus: agents → egress",          scope: "global" },
  { key: "↑/↓ j/k",  action: "navigate rows in focused table",        scope: "global" },
  { key: "a",        action: "approve selected egress domain",        scope: "egress" },
  { key: "d",        action: "deny selected egress domain / delete",  scope: "egress" },
  { key: "Shift-a",  action: "approve ALL egress domains",            scope: "egress" },
  { key: "e",        action: "enforce egress (lock down)",            scope: "egress" },
  { key: "Shift-l",  action: "toggle learning ↔ enforcement",         scope: "egress" },
  { key: "g",        action: "open/close full AGT detail overlay",    scope: "agents" },
  { key: "t",        action: "toggle topology view",                  scope: "global" },
  { key: "n",        action: "spawn new agent",                       scope: "agents" },
  { key: "m",        action: "switch model for selected agent",       scope: "agents" },
  { key: "l",        action: "tail logs for selected agent",          scope: "agents" },
  { key: "x",        action: "delete selected agent (confirm)",       scope: "agents" },
  { key: "Enter",    action: "connect (shell session)",               scope: "agents" },
  { key: "c",        action: "toggle cluster health view",            scope: "global" },
  { key: "r",        action: "refresh now",                           scope: "global" },
  { key: "q / Esc",  action: "quit (or close overlay)",               scope: "global" },
] as const;

// blessed tag-markup helpers. Not exported — shape is internal to status bar.
const focusAgents = "{cyan-fg}{bold}[Agents]{/bold}{/}  {gray-fg}Egress{/}";
const focusEgress = "{gray-fg}Agents{/}  {yellow-fg}{bold}[Egress]{/bold}{/}";
const viewCluster  = "{blue-fg}{bold}[Cluster]{/bold}{/}";
const viewTopology = "{cyan-fg}{bold}[Topology]{/bold}{/}";
const viewClusterDim  = "{gray-fg}Cluster{/}";
const viewTopologyDim = "{gray-fg}Topology{/}";

const AGENTS_ACTIONS =
  "[Tab] Focus  [↑↓] Nav  [Enter] Connect  [c] Cluster  [t] Topology  " +
  "[a] Approve  [A] All  [d] Del/Deny  [e] Enforce  [L] Learn/Enforce  [g] AGT  [n] Spawn  [r] Refresh  [q] Quit";

/**
 * Status-bar text for the agents view.
 * Returns blessed tag-markup intact; caller passes to `.setContent()`.
 */
export function statusBarForAgents(args: {
  focusedPanel: "agents" | "egress";
  viewMode: "agents" | "cluster" | "topology";
}): string {
  const focusTag = args.focusedPanel === "agents" ? focusAgents : focusEgress;
  const viewTag =
    args.viewMode === "cluster" ? viewCluster :
    args.viewMode === "topology" ? viewTopology :
    viewClusterDim;
  const topoTag = args.viewMode === "topology" ? viewTopology : viewTopologyDim;
  return ` ${focusTag}  ${viewTag}  ${topoTag}  │  ${AGENTS_ACTIONS}`;
}

export function statusBarForTopology(): string {
  return ` ${viewTopology}  │  [t] Back to Agents  [c] Cluster  [r] Refresh  [q] Quit`;
}

export function statusBarForCluster(): string {
  return ` ${viewCluster}  │  [c] Back to Agents  [t] Topology  [r] Refresh  [q] Quit`;
}
