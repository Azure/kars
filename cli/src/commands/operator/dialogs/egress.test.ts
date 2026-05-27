// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, it, expect } from "vitest";
import { __test } from "./egress.js";
import type { SandboxInfo, EgressDomain, SecurityState } from "../types.js";
import type { SignedDetail } from "./egress.js";

const { buildFleetRows, formatFleetRow } = __test;

function sb(name: string): SandboxInfo {
  return {
    name, namespace: `kars-${name}`, status: "Running", health: "healthy",
    model: "gpt-4.1", isolation: "enhanced", channels: "", age: "1d",
    podName: `${name}-0`, restarts: 0, role: "controller", parent: "", runtime: "aks",
  };
}

function dom(domain: string, sandbox: string, state: "learned" | "approved"): EgressDomain {
  return { domain, sandbox, namespace: `kars-${sandbox}`, state };
}

function sec(name: string, mode: "learning" | "enforcing" | "unknown", allowlist: number, blocklist = 0): SecurityState {
  return {
    sandbox: name, isolation: "enhanced", runtime: "runc", seccomp: "kars-strict",
    networkPolicy: true, adminAuth: true, readyz: true, readyzDetail: "ok",
    egressMode: mode, learnedDomains: 0, allowlistDomains: allowlist,
    blocklistDomains: blocklist, blocklistLearnMode: false,
    agtEnabled: false, agtAuditEntries: 0, agtAuditIntegrity: true, agtKnownAgents: 0, agtTrustThreshold: 0,
    agtRecentAudit: [], agtTrustScores: [], agtRelayConnected: false, agtRegistryAgents: 0, agtAmid: "",
    agtMeshSessions: 0, agtMeshSent: 0, agtMeshReceived: 0, agtTrustUpdates: 0, agtTotalInteractions: 0,
    agtGovernanceMode: "", agtPolicyEvaluations: 0, agtPolicyDenials: 0, agtPolicyRateLimits: 0,
    agtEvalLatencyUs: 0, agtBehaviorAlerts: 0, agtBehaviorDetail: [], agtContentFlags: 0, agtPolicyRules: 0,
    agtReputation: null, agtRelayUrl: "", agtRegistryUrl: "",
    totalRequests: 0, errorRequests: 0, inputTokens: 0, outputTokens: 0, avgLatencyMs: 0,
  };
}

describe("egress drawer — buildFleetRows", () => {
  const noSigned = new Map<string, SignedDetail>();

  it("returns empty array when no sandboxes", () => {
    expect(buildFleetRows([], new Map(), new Map(), noSigned)).toEqual([]);
  });

  it("counts learned + approved domains per sandbox", () => {
    const sandboxes = [sb("a"), sb("b")];
    const egress = new Map<string, EgressDomain[]>([
      ["a", [dom("api.openai.com", "a", "approved"), dom("evil.example.com", "a", "learned"), dom("foo.com", "a", "learned")]],
      ["b", [dom("api.cohere.com", "b", "approved")]],
    ]);
    const rows = buildFleetRows(sandboxes, egress, new Map(), noSigned);
    expect(rows[0]).toMatchObject({ name: "a", learned: 2, allowlist: 1 });
    expect(rows[1]).toMatchObject({ name: "b", learned: 0, allowlist: 1 });
  });

  it("reports signed-state honestly from the signed map (not from heuristic)", () => {
    const sandboxes = [sb("a"), sb("b"), sb("c"), sb("d")];
    const states = new Map<string, SecurityState>([
      // Old heuristic ('enforcing && allowlist>0') would have marked a, b
      // signed; only the live spec is the source of truth now.
      ["a", sec("a", "enforcing", 4)],
      ["b", sec("b", "enforcing", 4)],
      ["c", sec("c", "learning", 8)],
      ["d", sec("d", "enforcing", 0)],
    ]);
    const signed = new Map<string, SignedDetail>([
      ["a", { state: "signed-active", ref: { display: "policy/egress-allowlist/a@sha256:abcdef012345" } }],
      ["b", { state: "signed-pending", ref: { display: "policy/egress-allowlist/b@sha256:cafef00dbeef" } }],
      // c, d → no entry → "unknown"
    ]);
    const rows = buildFleetRows(sandboxes, new Map(), states, signed);
    expect(rows.map((r) => r.signed)).toEqual([
      "signed-active", "signed-pending", "unknown", "unknown",
    ]);
  });

  it("derives mode from securityStates, defaults to 'unknown'", () => {
    const sandboxes = [sb("a"), sb("b")];
    const states = new Map<string, SecurityState>([
      ["a", sec("a", "enforcing", 0)],
    ]);
    const rows = buildFleetRows(sandboxes, new Map(), states, noSigned);
    expect(rows[0].mode).toBe("enforcing");
    expect(rows[1].mode).toBe("unknown");
  });
});

describe("egress drawer — formatFleetRow", () => {
  it("renders agent name + counts + 'signed ✓' for signed-active", () => {
    const out = formatFleetRow({ name: "agent-a", mode: "enforcing", learned: 2, allowlist: 5, signed: "signed-active" });
    expect(out).toContain("agent-a");
    expect(out).toContain("L:2");
    expect(out).toContain("A:5");
    expect(out).toContain("enforcing");
    expect(out).toContain("signed ✓");
  });

  it("renders 'unsigned' when truly unsigned (the user-facing fix)", () => {
    const out = formatFleetRow({ name: "agent-b", mode: "enforcing", learned: 0, allowlist: 3, signed: "unsigned" });
    expect(out).toContain("unsigned");
    expect(out).not.toContain("signed ✓");
  });

  it("renders 'signing…' for signed-pending", () => {
    const out = formatFleetRow({ name: "agent-c", mode: "enforcing", learned: 0, allowlist: 3, signed: "signed-pending" });
    expect(out).toContain("signing");
  });
});
