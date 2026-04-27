import { describe, it, expect } from "vitest";
import {
  BINDINGS,
  statusBarForAgents,
  statusBarForTopology,
  statusBarForCluster,
} from "./keymap.js";

describe("operator keymap — BINDINGS invariants", () => {
  it("has all expected global/agents/egress keys", () => {
    const keys = BINDINGS.map((b) => b.key);
    for (const required of ["Tab", "↑/↓ j/k", "a", "d", "q / Esc", "Enter"]) {
      expect(keys).toContain(required);
    }
  });

  it("every binding has a non-empty action and a known scope", () => {
    const valid = new Set([
      "global",
      "agents",
      "egress",
      "cluster",
      "topology",
      "overlay",
    ]);
    for (const b of BINDINGS) {
      expect(b.key.length).toBeGreaterThan(0);
      expect(b.action.length).toBeGreaterThan(0);
      expect(valid.has(b.scope)).toBe(true);
    }
  });
});

describe("operator keymap — status-bar content", () => {
  it("agents view with agents focus highlights [Agents]", () => {
    const s = statusBarForAgents({ focusedPanel: "agents", viewMode: "agents" });
    expect(s).toContain("{bold}[Agents]");
    expect(s).toContain("[Tab] Focus");
    expect(s).toContain("[q] Quit");
  });

  it("agents view with egress focus highlights [Egress]", () => {
    const s = statusBarForAgents({ focusedPanel: "egress", viewMode: "agents" });
    expect(s).toContain("{bold}[Egress]");
  });

  it("topology view mentions [Topology] once and [t] Back", () => {
    const s = statusBarForTopology();
    expect(s).toContain("{bold}[Topology]");
    expect(s).toContain("[t] Back to Agents");
  });

  it("cluster view mentions [Cluster] and [c] Back", () => {
    const s = statusBarForCluster();
    expect(s).toContain("{bold}[Cluster]");
    expect(s).toContain("[c] Back to Agents");
  });
});
