import { describe, it, expect } from "vitest";
import { mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { __test } from "./attest.js";

describe("attestCommand — canonicalJson", () => {
  it("sorts object keys lexicographically", () => {
    expect(__test.canonicalJson({ b: 1, a: 2 })).toBe('{"a":2,"b":1}');
  });

  it("preserves array order", () => {
    expect(__test.canonicalJson([3, 1, 2])).toBe("[3,1,2]");
  });

  it("recursively canonicalises nested objects", () => {
    const a = { x: { c: 1, b: 2 }, y: [{ k: 1, j: 2 }] };
    const b = { y: [{ j: 2, k: 1 }], x: { b: 2, c: 1 } };
    expect(__test.canonicalJson(a)).toBe(__test.canonicalJson(b));
  });

  it("handles null/undefined as null", () => {
    expect(__test.canonicalJson(null)).toBe("null");
    expect(__test.canonicalJson(undefined)).toBe("null");
  });

  it("emits primitives via JSON.stringify", () => {
    expect(__test.canonicalJson(42)).toBe("42");
    expect(__test.canonicalJson("abc")).toBe('"abc"');
    expect(__test.canonicalJson(true)).toBe("true");
  });
});

describe("attestCommand — specHash", () => {
  it("is deterministic across key-reordered inputs", () => {
    const a = { inference: { model: "x" }, sandbox: { isolation: "enhanced" } };
    const b = { sandbox: { isolation: "enhanced" }, inference: { model: "x" } };
    expect(__test.specHash(a)).toBe(__test.specHash(b));
  });

  it("changes when any field changes", () => {
    const a = { inference: { model: "x" } };
    const b = { inference: { model: "y" } };
    expect(__test.specHash(a)).not.toBe(__test.specHash(b));
  });

  it("emits sha256: prefixed hex", () => {
    const h = __test.specHash({});
    expect(h).toMatch(/^sha256:[0-9a-f]{64}$/);
  });

  it("treats null and missing spec as equivalent (empty object)", () => {
    expect(__test.specHash(undefined)).toBe(__test.specHash({}));
    expect(__test.specHash(null)).toBe(__test.specHash({}));
  });
});

describe("attestCommand — summariseFieldOwners", () => {
  it("returns empty for missing/empty managedFields", () => {
    expect(__test.summariseFieldOwners(undefined)).toEqual([]);
    expect(__test.summariseFieldOwners([])).toEqual([]);
  });

  it("aggregates fields per manager and sorts alphabetically", () => {
    const summary = __test.summariseFieldOwners([
      { manager: "azureclaw-controller", fieldsV1: { "f:status": { "f:phase": {} } } },
      { manager: "kubectl-edit", fieldsV1: { "f:metadata": { "f:labels": { "f:app": {} } } } },
      { manager: "azureclaw-controller", fieldsV1: { "f:spec": { "f:isolation": {} } } },
    ]);
    expect(summary.map((s) => s.manager)).toEqual([
      "azureclaw-controller",
      "kubectl-edit",
    ]);
    const ctl = summary.find((s) => s.manager === "azureclaw-controller")!;
    expect(ctl.fieldsOwned).toBeGreaterThan(0);
  });

  it("treats missing manager as (unknown)", () => {
    const summary = __test.summariseFieldOwners([{ fieldsV1: { "f:status": {} } }]);
    expect(summary.map((s) => s.manager)).toEqual(["(unknown)"]);
  });

  it("counts at least one field for trees with no leaf indicators", () => {
    const summary = __test.summariseFieldOwners([
      { manager: "m", fieldsV1: { "f:nonleaf": { "f:also-nonleaf": { "f:leaf": {} } } } },
    ]);
    expect(summary[0]?.fieldsOwned).toBeGreaterThanOrEqual(1);
  });
});

describe("attestCommand — extractPolicyRefs", () => {
  it("extracts the three top-level refs", () => {
    const refs = __test.extractPolicyRefs({
      toolPolicyRef: { name: "tp1" },
      inferencePolicyRef: { name: "ip1" },
      a2aAgentRef: { name: "agent1" },
    });
    expect(refs).toEqual([
      { kind: "ToolPolicy", name: "tp1" },
      { kind: "InferencePolicy", name: "ip1" },
      { kind: "A2AAgent", name: "agent1" },
    ]);
  });

  it("extracts the legacy governance.toolPolicy.ref shape", () => {
    const refs = __test.extractPolicyRefs({ governance: { toolPolicy: { ref: "legacy" } } });
    expect(refs).toEqual([{ kind: "ToolPolicy", name: "legacy" }]);
  });

  it("ignores refs missing 'name'", () => {
    const refs = __test.extractPolicyRefs({ toolPolicyRef: { other: "x" } });
    expect(refs).toEqual([]);
  });

  it("returns empty for non-object spec", () => {
    expect(__test.extractPolicyRefs(undefined)).toEqual([]);
    expect(__test.extractPolicyRefs(null)).toEqual([]);
    expect(__test.extractPolicyRefs("string")).toEqual([]);
  });
});

describe("attestCommand — formatters", () => {
  const sample = {
    apiVersion: "azureclaw.azure.com/v1alpha1-attest" as const,
    kind: "Attestation" as const,
    generatedAt: "2026-04-27T12:00:00Z",
    sandbox: {
      name: "demo",
      namespace: "azureclaw-system",
      generation: 3,
      observedGeneration: 3,
      phase: "Running",
      specHash: "sha256:" + "a".repeat(64),
      specHashAlgorithm: "sha256-canonical-json" as const,
    },
    fieldOwners: [{ manager: "azureclaw-controller", fieldsOwned: 7 }],
    policyVersions: [
      {
        kind: "ToolPolicy",
        name: "tp",
        namespace: "azureclaw-demo",
        versionHash: "sha256:" + "b".repeat(64),
        bindingConfigMap: "tp-binding",
      },
    ],
    reconcileTraceId: null,
    agtAuditReceiptId: null,
    signature: null,
  };

  it("formatJson round-trips through JSON.parse", () => {
    const out = __test.formatJson(sample);
    expect(JSON.parse(out)).toEqual(sample);
  });

  it("formatHuman names every report field", () => {
    const out = __test.formatHuman(sample);
    expect(out).toContain("demo");
    expect(out).toContain("Running");
    expect(out).toContain("sha256:" + "a".repeat(8));
    expect(out).toContain("azureclaw-controller");
    expect(out).toContain("ToolPolicy");
    expect(out).toContain("(Phase 3)");
  });
});

describe("attestCommand — diffAttestations", () => {
  const baseReport = (overrides: Record<string, unknown> = {}) =>
    ({
      apiVersion: "azureclaw.azure.com/v1alpha1-attest" as const,
      kind: "Attestation" as const,
      generatedAt: "2026-04-01T00:00:00Z",
      sandbox: {
        name: "demo",
        namespace: "azureclaw-system",
        generation: 1,
        observedGeneration: 1,
        phase: "Running",
        specHash: "sha256:" + "a".repeat(64),
        specHashAlgorithm: "sha256-canonical-json" as const,
      },
      fieldOwners: [{ manager: "azureclaw-controller", fieldsOwned: 5 }],
      policyVersions: [
        {
          kind: "ToolPolicy",
          name: "tp",
          namespace: "azureclaw-demo",
          versionHash: "sha256:" + "b".repeat(64),
          bindingConfigMap: "tp-bind",
        },
      ],
      reconcileTraceId: null,
      agtAuditReceiptId: null,
      signature: null,
      ...overrides,
    });

  it("returns drift=false when reports match", () => {
    const a = baseReport();
    const b = baseReport({ generatedAt: "2026-04-02T00:00:00Z" });
    const diff = __test.diffAttestations(a, b);
    expect(diff.drift).toBe(false);
    expect(diff.deltas).toEqual([]);
  });

  it("detects spec hash drift", () => {
    const a = baseReport();
    const b = baseReport({
      sandbox: { ...a.sandbox, specHash: "sha256:" + "c".repeat(64) },
    });
    const diff = __test.diffAttestations(a, b);
    expect(diff.drift).toBe(true);
    expect(diff.deltas).toEqual([
      {
        type: "specHash",
        before: a.sandbox.specHash,
        after: "sha256:" + "c".repeat(64),
      },
    ]);
  });

  it("detects phase change", () => {
    const a = baseReport();
    const b = baseReport({ sandbox: { ...a.sandbox, phase: "Degraded" } });
    const diff = __test.diffAttestations(a, b);
    expect(diff.deltas.find((d) => d.type === "phase")).toEqual({
      type: "phase",
      before: "Running",
      after: "Degraded",
    });
  });

  it("detects policy versionHash drift", () => {
    const a = baseReport();
    const b = baseReport({
      policyVersions: [
        {
          ...a.policyVersions[0],
          versionHash: "sha256:" + "d".repeat(64),
        },
      ],
    });
    const diff = __test.diffAttestations(a, b);
    const policyDelta = diff.deltas.find((d) => d.type === "policyVersionHash");
    expect(policyDelta).toMatchObject({
      type: "policyVersionHash",
      kind: "ToolPolicy",
      name: "tp",
      after: "sha256:" + "d".repeat(64),
    });
  });

  it("detects added/removed policies", () => {
    const a = baseReport();
    const b = baseReport({
      policyVersions: [
        {
          kind: "InferencePolicy",
          name: "ip",
          namespace: "azureclaw-demo",
          versionHash: "sha256:" + "e".repeat(64),
          bindingConfigMap: null,
        },
      ],
    });
    const diff = __test.diffAttestations(a, b);
    const types = diff.deltas.map((d) => d.type).sort();
    expect(types).toEqual(["policyAdded", "policyRemoved"]);
  });

  it("detects new SSA manager (set-comparison; counts ignored)", () => {
    const a = baseReport();
    const b = baseReport({
      fieldOwners: [
        { manager: "azureclaw-controller", fieldsOwned: 99 },
        { manager: "kubectl-edit", fieldsOwned: 1 },
      ],
    });
    const diff = __test.diffAttestations(a, b);
    expect(diff.deltas).toContainEqual({
      type: "fieldOwnerAdded",
      manager: "kubectl-edit",
    });
    expect(diff.deltas.find((d) => d.type === "fieldOwnerRemoved")).toBeUndefined();
  });

  it("ignores field count fluctuation when manager set is identical", () => {
    const a = baseReport();
    const b = baseReport({
      fieldOwners: [{ manager: "azureclaw-controller", fieldsOwned: 99 }],
    });
    const diff = __test.diffAttestations(a, b);
    expect(diff.deltas).toEqual([]);
  });

  it("describeDelta produces a human sentence for every variant", () => {
    const variants: Array<Parameters<typeof __test.describeDelta>[0]> = [
      { type: "specHash", before: "sha256:aaaaaaaaaa", after: "sha256:bbbbbbbbbb" },
      { type: "phase", before: "Running", after: "Degraded" },
      {
        type: "policyVersionHash",
        kind: "ToolPolicy",
        name: "tp",
        before: "sha256:aa",
        after: "sha256:bb",
      },
      { type: "policyAdded", kind: "InferencePolicy", name: "ip" },
      { type: "policyRemoved", kind: "A2AAgent", name: "a" },
      { type: "fieldOwnerAdded", manager: "kubectl-edit" },
      { type: "fieldOwnerRemoved", manager: "azureclaw-controller" },
    ];
    for (const v of variants) {
      const out = __test.describeDelta(v);
      expect(out.length).toBeGreaterThan(0);
      expect(typeof out).toBe("string");
    }
  });
});

describe("attestCommand — loadBaseline", () => {
  it("returns null for missing file", async () => {
    const result = await __test.loadBaseline("/no/such/file/anywhere/xyz.json");
    expect(result).toBeNull();
  });

  it("loads a valid attestation file", async () => {
    const dir = mkdtempSync(join(tmpdir(), "attest-test-"));
    try {
      const path = join(dir, "baseline.json");
      const valid = {
        apiVersion: "azureclaw.azure.com/v1alpha1-attest",
        kind: "Attestation",
        generatedAt: "2026-04-01T00:00:00Z",
        sandbox: {
          name: "demo",
          namespace: "azureclaw-system",
          generation: 1,
          observedGeneration: 1,
          phase: "Running",
          specHash: "sha256:" + "a".repeat(64),
          specHashAlgorithm: "sha256-canonical-json",
        },
        fieldOwners: [],
        policyVersions: [],
        reconcileTraceId: null,
        agtAuditReceiptId: null,
        signature: null,
      };
      writeFileSync(path, JSON.stringify(valid));
      const loaded = await __test.loadBaseline(path);
      expect(loaded?.sandbox.specHash).toBe(valid.sandbox.specHash);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("rejects a file missing the apiVersion sentinel", async () => {
    const dir = mkdtempSync(join(tmpdir(), "attest-test-"));
    try {
      const path = join(dir, "bad.json");
      writeFileSync(path, JSON.stringify({ kind: "Attestation" }));
      await expect(__test.loadBaseline(path)).rejects.toThrow(/not a valid AzureClaw attestation/);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});
