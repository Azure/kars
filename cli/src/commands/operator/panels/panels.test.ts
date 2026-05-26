// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, it, expect } from "vitest";
import { emptyClusterState, type ClusterState } from "./types.js";
import { clawSandboxPanel } from "./karssandbox.js";
import { clawPairingPanel } from "./karspairing.js";
import { mcpServerPanel } from "./mcpserver.js";
import { toolPolicyPanel } from "./toolpolicy.js";
import { inferencePolicyPanel } from "./inferencepolicy.js";
import { a2aAgentPanel } from "./a2aagent.js";
import { clawMemoryPanel } from "./karsmemory.js";
import { clawEvalPanel } from "./karseval.js";
import { providerStatusPanel } from "./provider_status.js";
import {
  DEFAULT_PANELS,
  PANEL_BY_ID,
  resolvePanels,
  renderDashboard,
} from "./layout.js";
import { isSensitiveKey, redactValue, redactObject } from "./redact.js";
import { FixtureDataSource } from "./datasource.js";
import {
  fullFixture,
  fixtureSandbox,
  fixtureMcp,
  fixtureProviderUnknown,
} from "./fixtures.js";

const ALL_IDS = [
  "karssandbox", "karspairing", "mcpserver", "toolpolicy",
  "inferencepolicy", "a2aagent", "karsmemory", "karseval",
  "provider_status",
];

describe("S14 panels — registry", () => {
  it("exposes nine default panels in canonical order", () => {
    expect(DEFAULT_PANELS.map((p) => p.id)).toEqual(ALL_IDS);
  });

  it("PANEL_BY_ID resolves every default panel", () => {
    for (const id of ALL_IDS) {
      expect(PANEL_BY_ID[id]).toBeDefined();
      expect(PANEL_BY_ID[id].id).toBe(id);
    }
  });

  it("every panel has a non-empty title", () => {
    for (const p of DEFAULT_PANELS) {
      expect(p.title.length).toBeGreaterThan(0);
    }
  });
});

describe("S14 panels — empty cluster (no panic, no false data)", () => {
  const empty = emptyClusterState();
  for (const p of DEFAULT_PANELS) {
    it(`${p.id} renders empty state without throwing`, () => {
      const out = p.render(empty);
      expect(typeof out).toBe("string");
      expect(out.length).toBeGreaterThan(0);
    });
  }

  it("renderDashboard against empty cluster shows agents+providers and the at-a-glance section", () => {
    const out = renderDashboard(empty);
    // Default layout always renders the Health alerts section.
    expect(out).toContain("🚨 Health alerts");
    expect(out).toContain("✓ No alerts");
    // Empty cluster has no CRD instances.
    expect(out).toContain("(no CRD instances)");
    // Old "Triage" wording must not leak through.
    expect(out).not.toContain("Triage");
  });

  it("renderDashboard --panels=all preserves legacy full dump", () => {
    const out = renderDashboard(empty, { panels: "all" });
    for (const p of DEFAULT_PANELS) {
      expect(out).toContain(p.title);
    }
  });

  it("renderDashboard --panels=triage on empty cluster shows header + 'no alerts' only", () => {
    const out = renderDashboard(empty, { panels: "triage" });
    expect(out).toContain("Kars Operator");
    expect(out).toContain("0 agents, ");
    expect(out).toContain("✓ No alerts");
    expect(out).not.toContain("KarsSandbox");
  });

  it("renderDashboard --per-sandbox on empty cluster falls back to flat list", () => {
    const out = renderDashboard(empty, { perSandbox: true });
    // Empty cluster → no per-sandbox grouping headers and no CRD sections.
    expect(out).toContain("(no CRD instances)");
    expect(out).not.toContain("══ Sandbox:");
  });
});

describe("S14 panels — karssandbox", () => {
  it("renders sandbox columns with health color tag", () => {
    const out = clawSandboxPanel.render(fullFixture());
    expect(out).toContain("sb-1");
    expect(out).toContain("sb-2");
    expect(out).toContain("MODEL");
    expect(out).toContain("ISOLATION");
    expect(out).toContain("{green-fg}");
  });

  it("filters by sandbox option", () => {
    const out = clawSandboxPanel.render(fullFixture(), { sandbox: "sb-1" });
    expect(out).toContain("sb-1");
    expect(out).not.toContain("sb-2");
  });
});

describe("S14 panels — karspairing", () => {
  it("renders agentA ↔ agentB and Conditions reasons verbatim", () => {
    const out = clawPairingPanel.render(fullFixture());
    expect(out).toContain("alice");
    expect(out).toContain("bob");
    expect(out).toContain("HandshakeComplete");
    expect(out).toContain("Paired=True");
  });

  it("empty pairings render the empty marker", () => {
    expect(clawPairingPanel.render(emptyClusterState())).toContain("(none)");
  });
});

describe("S14 panels — mcpserver", () => {
  it("renders url + jwks presence + tool count", () => {
    const out = mcpServerPanel.render(fullFixture());
    expect(out).toContain("mcp-fs");
    expect(out).toContain("http://mcp-fs");
    expect(out).toContain("jwks-secret");
    expect(out).toContain("<present>");
    expect(out).toContain("tools=4");
  });

  it("missing jwks renders <missing> not raw value", () => {
    const state: ClusterState = emptyClusterState();
    state.mcpServers = [{ ...fixtureMcp(), jwksSecretPresent: "missing" }];
    const out = mcpServerPanel.render(state);
    expect(out).toContain("<missing>");
  });

  it("unknown jwks surfaces a verbatim reason", () => {
    const state: ClusterState = emptyClusterState();
    state.mcpServers = [{
      ...fixtureMcp(),
      jwksSecretPresent: "unknown",
      jwksSecretReason: "Secret API forbidden",
    }];
    const out = mcpServerPanel.render(state);
    expect(out).toContain("<unknown>");
    expect(out).toContain("Secret API forbidden");
  });
});

describe("S14 panels — toolpolicy", () => {
  it("renders appliesTo + commerce + rate-limit", () => {
    const out = toolPolicyPanel.render(fullFixture());
    expect(out).toContain("appliesTo");
    expect(out).toContain("sb-1");
    expect(out).toContain("rate-limit=30/min");
    expect(out).toContain("commerce");
    expect(out).toContain("$5.00");
  });
});

describe("S14 panels — inferencepolicy", () => {
  it("renders budgets + guardrail floor + model preference order", () => {
    const out = inferencePolicyPanel.render(fullFixture());
    expect(out).toContain("daily=100000");
    expect(out).toContain("guardrail-floor");
    expect(out).toContain("high");
    expect(out).toContain("1.gpt-4.1");
    expect(out).toContain("2.gpt-4o");
  });
});

describe("S14 panels — a2aagent", () => {
  it("renders endpoint + AgentCard publication status + capabilities", () => {
    const out = a2aAgentPanel.render(fullFixture());
    expect(out).toContain("agent-card-1");
    expect(out).toContain("https://example.test/a2a");
    expect(out).toContain("AgentCard");
    expect(out).toContain("published");
    expect(out).toContain("capabilities");
    expect(out).toContain("streaming");
  });
});

describe("S14 panels — karsmemory", () => {
  it("renders Foundry binding + RBAC scope summary", () => {
    const out = clawMemoryPanel.render(fullFixture());
    expect(out).toContain("foundry-binding");
    expect(out).toContain("bound");
    expect(out).toContain("project-MI: Azure AI User on RG");
    expect(out).toContain("retention=30d");
  });
});

describe("S14 panels — karseval", () => {
  it("renders lastRunAt + lastScore + nextScheduledAt", () => {
    const out = clawEvalPanel.render(fullFixture());
    expect(out).toContain("eval-nightly");
    expect(out).toContain("rag-quality");
    expect(out).toContain("last-run: 2026-04-29T02:00:00Z");
    expect(out).toContain("score: 0.91");
    expect(out).toContain("next: 2026-04-30T02:00:00Z");
  });
});

describe("S14 panels — provider_status", () => {
  it("renders per-sandbox grouping with verbatim 'unknown' reasons", () => {
    const out = providerStatusPanel.render(fullFixture());
    expect(out).toContain("Per-sandbox: sb-1");
    expect(out).toContain("foundry");
    expect(out).toContain("Cluster-wide");
    expect(out).toContain("agc");
    expect(out).toContain("no Gateway objects");
  });

  it("filters per-sandbox view to the named sandbox only", () => {
    const out = providerStatusPanel.render(fullFixture(), { sandbox: "sb-1" });
    expect(out).toContain("Per-sandbox (sb-1)");
    expect(out).not.toContain("Per-sandbox: sb-2");
  });

  it("empty providers render (none) without throwing", () => {
    const out = providerStatusPanel.render(emptyClusterState());
    expect(out).toContain("(none)");
  });

  it("each unknown branch surfaces a reason (no fake healthy data)", () => {
    const state = emptyClusterState();
    state.sandboxes = [fixtureSandbox("sb-x")];
    state.providers.perSandbox.set("sb-x", [
      fixtureProviderUnknown("foundry", "not probed"),
      fixtureProviderUnknown("agt", "router unreachable"),
      fixtureProviderUnknown("acr", "events api forbidden"),
      fixtureProviderUnknown("identity", "no SA"),
    ]);
    state.providers.cluster = [fixtureProviderUnknown("agc", "no Gateway objects")];
    const out = providerStatusPanel.render(state);
    for (const reason of [
      "not probed", "router unreachable", "events api forbidden", "no SA", "no Gateway objects",
    ]) {
      expect(out).toContain(reason);
    }
  });
});

describe("S14 panels — layout flag wiring", () => {
  it("--panels filters and orders correctly", () => {
    const sel = resolvePanels("mcpserver,karssandbox");
    expect(sel.map((p) => p.id)).toEqual(["mcpserver", "karssandbox"]);
  });

  it("--panels=all and undefined and empty fall through to defaults", () => {
    expect(resolvePanels(undefined).map((p) => p.id)).toEqual(ALL_IDS);
    expect(resolvePanels("all").map((p) => p.id)).toEqual(ALL_IDS);
    expect(resolvePanels("").map((p) => p.id)).toEqual(ALL_IDS);
  });

  it("--panels with unknown ids drops them silently", () => {
    const sel = resolvePanels("karssandbox,nope,mcpserver");
    expect(sel.map((p) => p.id)).toEqual(["karssandbox", "mcpserver"]);
  });

  it("renderDashboard --per-sandbox groups by sandbox-name", () => {
    const out = renderDashboard(fullFixture(), { perSandbox: true });
    expect(out).toContain("══ Sandbox: sb-1 ══");
    expect(out).toContain("══ Sandbox: sb-2 ══");
    // sb-1 group must precede sb-2 group
    expect(out.indexOf("sb-1 ══")).toBeLessThan(out.indexOf("sb-2 ══"));
  });

  it("renderDashboard with empty selection returns a placeholder", () => {
    const out = renderDashboard(fullFixture(), { panels: "nonexistent" });
    expect(out).toContain("(no panels selected)");
  });
});

describe("S14 panels — secret redaction", () => {
  it("isSensitiveKey matches KEY/TOKEN/SECRET/PASSWORD/JWKS", () => {
    for (const k of [
      "API_KEY", "BOT_TOKEN", "FOO_SECRET", "DB_PASSWORD",
      "JWKS_PRIV", "PRIVATE_PEM", "AUTH_CREDENTIAL",
    ]) {
      expect(isSensitiveKey(k)).toBe(true);
    }
    for (const k of ["url", "phase", "name", "namespace"]) {
      expect(isSensitiveKey(k)).toBe(false);
    }
  });

  it("redactValue returns <present>/<missing> for sensitive keys", () => {
    expect(redactValue("API_KEY", "abc123")).toBe("<present>");
    expect(redactValue("API_KEY", undefined)).toBe("<missing>");
    expect(redactValue("API_KEY", "")).toBe("<missing>");
    expect(redactValue("url", "http://x")).toBe("http://x");
  });

  it("redactObject collapses sensitive entries", () => {
    const out = redactObject({ API_KEY: "leak", url: "http://x" });
    expect(out).toContain("API_KEY=<present>");
    expect(out).toContain("url=http://x");
  });
});

describe("S14 panels — FixtureDataSource", () => {
  it("returns the stored snapshot verbatim", async () => {
    const snap = fullFixture();
    const ds = new FixtureDataSource(snap);
    const out = await ds.fetch();
    expect(out).toBe(snap);
    expect(out.sandboxes).toHaveLength(2);
  });
});

describe("S20 panels — triage + at-a-glance layout", () => {
  it("collectTriage returns no items for an empty cluster", async () => {
    const { collectTriage } = await import("./layout.js");
    expect(collectTriage(emptyClusterState())).toEqual([]);
  });

  it("collectTriage surfaces False conditions verbatim", async () => {
    const { collectTriage } = await import("./layout.js");
    const state = emptyClusterState();
    state.inferencePolicies = [{
      name: "ip-1",
      namespace: "kars-sb-1",
      conditions: [
        { type: "Ready", status: "False", reason: "ModelMissing", message: "gpt-foo not found" },
      ],
    }];
    const triage = collectTriage(state);
    expect(triage).toHaveLength(1);
    expect(triage[0].panel).toBe("InferencePolicy");
    expect(triage[0].name).toBe("ip-1");
    expect(triage[0].reason).toBe("ModelMissing");
    expect(triage[0].message).toBe("gpt-foo not found");
  });

  it("collectTriage flags down sandboxes", async () => {
    const { collectTriage } = await import("./layout.js");
    const state = emptyClusterState();
    state.sandboxes = [{
      name: "sb-down", namespace: "kars-sb-down", status: "CrashLoopBackOff",
      health: "down", model: "gpt-4.1", isolation: "enhanced", channels: "",
      age: "1m", podName: "p", restarts: 5, role: "controller", parent: "", runtime: "aks",
    }];
    const triage = collectTriage(state);
    expect(triage).toHaveLength(1);
    expect(triage[0].panel).toBe("KarsSandbox");
    expect(triage[0].status).toBe("False");
  });

  it("default dashboard hides empty optional CRDs", () => {
    const out = renderDashboard(emptyClusterState());
    // Optional CRDs shouldn't surface as section headers when empty.
    expect(out).not.toContain("═══ McpServer");
    expect(out).not.toContain("═══ KarsMemory");
    expect(out).not.toContain("═══ KarsEval");
    expect(out).not.toContain("═══ A2AAgent");
    expect(out).not.toContain("═══ KarsPairing");
    // KarsSandbox section is only rendered when there are sandboxes.
    expect(out).not.toContain("═══ KarsSandbox");
  });

  it("dashboard with issues prepends a 🚨 Health alerts section", () => {
    const state = emptyClusterState();
    state.inferencePolicies = [{
      name: "ip-bad", namespace: "kars",
      conditions: [{ type: "Ready", status: "False", reason: "Bad", message: "x" }],
    }];
    const out = renderDashboard(state);
    expect(out).toContain("🚨 Health alerts");
    expect(out).toContain("InferencePolicy");
    expect(out).toContain("ip-bad");
    expect(out).toContain("Bad");
    expect(out).not.toContain("Triage");
  });

  it("default dashboard renders one section per non-empty CRD type with [N] indices", () => {
    const out = renderDashboard(fullFixture());
    // Each non-empty CRD type gets its own section header.
    expect(out).toContain("═══ KarsSandbox (");
    expect(out).toContain("═══ InferencePolicy (");
    expect(out).toContain("═══ ToolPolicy (");
    // Drill-in indices.
    expect(out).toContain("[1]");
    // Compact table column header.
    expect(out).toContain("PHASE");
    expect(out).toContain("STATUS");
  });

  it("each panel exposes purpose + category metadata", () => {
    for (const p of DEFAULT_PANELS) {
      expect(p.category).toBeDefined();
      expect(typeof p.purpose).toBe("string");
      expect(p.purpose!.length).toBeGreaterThan(0);
    }
  });

  it("each panel implements summarize()", () => {
    const empty = emptyClusterState();
    for (const p of DEFAULT_PANELS) {
      expect(p.summarize).toBeDefined();
      const s = p.summarize!(empty);
      expect(typeof s.total).toBe("number");
      expect(s.total).toBe(0);
    }
  });

  it("--panels=all preserves the legacy full dump", () => {
    const out = renderDashboard(emptyClusterState(), { panels: "all" });
    for (const p of DEFAULT_PANELS) {
      expect(out).toContain(`┄ ${p.title} ┄`);
    }
  });
});

describe("CRD panel — per-type sections + drill-in", () => {
  it("renderCrdSections returns one row per CRD instance with stable indices", async () => {
    const { renderCrdSections } = await import("./layout.js");
    const { rows, body } = renderCrdSections(fullFixture());
    expect(rows.length).toBeGreaterThan(0);
    // Indices are 1-based and contiguous.
    expect(rows.map((r) => r.index)).toEqual(rows.map((_, i) => i + 1));
    // Body contains a section per non-empty kind.
    expect(body).toContain("═══ KarsSandbox");
  });

  it("renderCrdSections on empty cluster returns the (no CRD instances) placeholder", async () => {
    const { renderCrdSections } = await import("./layout.js");
    const { body, rows } = renderCrdSections(emptyClusterState());
    expect(rows).toEqual([]);
    expect(body).toContain("(no CRD instances)");
  });

  it("renderCrdItemDetail returns the verbose render of a single CRD", async () => {
    const { renderCrdItemDetail } = await import("./layout.js");
    const state = fullFixture();
    const sb = state.sandboxes[0];
    const out = renderCrdItemDetail(state, "KarsSandbox", sb.name, sb.namespace);
    expect(out).toContain("KarsSandbox detail");
    expect(out).toContain(sb.name);
  });

  it("renderCrdItemDetail returns a friendly 'not found' for unknown items", async () => {
    const { renderCrdItemDetail } = await import("./layout.js");
    const out = renderCrdItemDetail(emptyClusterState(), "KarsSandbox", "ghost", "kars-ghost");
    expect(out).toContain("not found");
  });

  it("bucketFromConditions: Ready=True + Progressing=False + Degraded=False → healthy (regression: do not treat False on negative-polarity conditions as error)", async () => {
    const { bucketFromConditions } = await import("./util.js");
    const conds = [
      { type: "Ready", status: "True", lastTransitionTime: "", reason: "Reconciled", message: "" },
      { type: "Progressing", status: "False", lastTransitionTime: "", reason: "Reconciled", message: "" },
      { type: "Degraded", status: "False", lastTransitionTime: "", reason: "Reconciled", message: "" },
    ];
    expect(bucketFromConditions(conds)).toBe("healthy");
  });

  it("bucketFromConditions: Degraded=True dominates Ready=True → error", async () => {
    const { bucketFromConditions } = await import("./util.js");
    const conds = [
      { type: "Ready", status: "True", lastTransitionTime: "", reason: "", message: "" },
      { type: "Degraded", status: "True", lastTransitionTime: "", reason: "", message: "" },
    ];
    expect(bucketFromConditions(conds)).toBe("error");
  });

  it("bucketFromConditions: Ready=False → error, Ready=Unknown → unknown", async () => {
    const { bucketFromConditions } = await import("./util.js");
    expect(bucketFromConditions([
      { type: "Ready", status: "False", lastTransitionTime: "", reason: "", message: "" },
    ])).toBe("error");
    expect(bucketFromConditions([
      { type: "Ready", status: "Unknown", lastTransitionTime: "", reason: "", message: "" },
    ])).toBe("unknown");
  });

  it("KarsPairing rows: empty conditions + state=Active → phase=healthy (regression: pairing reconciler emits no conditions)", async () => {
    const { renderCrdSections } = await import("./layout.js");
    const state = emptyClusterState();
    state.pairings = [{
      name: "p1", namespace: "kars-system", age: undefined, conditions: [],
      agentA: "a", agentB: "b", state: "Active", trust: "verified",
    }];
    const { rows } = renderCrdSections(state);
    const pairing = rows.find((r) => r.kind === "KarsPairing");
    expect(pairing?.phase).toBe("healthy");
  });
});