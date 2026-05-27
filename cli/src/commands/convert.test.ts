// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * Unit tests for `kars convert` (S9.2).
 *
 * Tests the pure helpers exposed via `__test`. No filesystem or kubectl IO.
 *
 * Mapping verified against:
 *   - kubernetes-sigs/agent-sandbox @ c8c85f5 (api/v1alpha1/sandbox_types.go)
 *   - controller/src/crd.rs:25-405 (KarsSandbox shape)
 *   - controller/src/reconciler/mod.rs:34-78 (seccomp + runtimeClass logic)
 */
import { describe, it, expect } from "vitest";
import { stringify as yamlStringify } from "yaml";
import { __test } from "./convert.js";

const {
  parseManifest,
  cleanMetadata,
  karssandboxToUpstreamSandbox,
  upstreamSandboxToClawsandbox,
  emitOverlay,
  envArrayToMap,
  canonicaliseSeccomp,
  mapToEnvArray,
  formatYaml,
  dispatch,
  parseTarget,
} = __test;

function clawSandboxFixture(overrides: Record<string, unknown> = {}): string {
  const base = {
    apiVersion: "kars.azure.com/v1alpha1",
    kind: "KarsSandbox",
    metadata: { name: "demo", namespace: "kars-demo" },
    spec: {
      runtime: {
        kind: "OpenClaw",
        openclaw: {
          image: "openclaw:1.2.3",
          extraEnv: { FOO: "bar", ALPHA: "1" },
        },
      },
      sandbox: {
        isolation: "enhanced",
        seccompProfile: "kars-strict",
        readOnlyRootFilesystem: true,
        runAsNonRoot: true,
        allowPrivilegeEscalation: false,
      },
      ...overrides,
    },
  };
  return yamlStringify(base);
}

function upstreamSandboxFixture(overrides: Record<string, unknown> = {}): string {
  const base = {
    apiVersion: "agents.x-k8s.io/v1alpha1",
    kind: "Sandbox",
    metadata: { name: "demo", namespace: "kars-demo" },
    spec: {
      podTemplate: {
        spec: {
          containers: [
            {
              name: "openclaw",
              image: "openclaw:1.2.3",
              env: [
                { name: "ALPHA", value: "1" },
                { name: "FOO", value: "bar" },
              ],
              securityContext: {
                readOnlyRootFilesystem: true,
                runAsNonRoot: true,
                allowPrivilegeEscalation: false,
                seccompProfile: {
                  type: "Localhost",
                  localhostProfile: "profiles/kars-strict.json",
                },
              },
            },
          ],
        },
      },
      replicas: 1,
      ...overrides,
    },
  };
  return yamlStringify(base);
}

describe("parseTarget", () => {
  it("accepts the three known targets", () => {
    expect(parseTarget("karssandbox")).toBe("karssandbox");
    expect(parseTarget("upstream-sandbox")).toBe("upstream-sandbox");
    expect(parseTarget("overlay")).toBe("overlay");
  });
  it("rejects unknown / undefined", () => {
    expect(parseTarget("xyz")).toBeUndefined();
    expect(parseTarget(undefined)).toBeUndefined();
    expect(parseTarget("")).toBeUndefined();
  });
});

describe("parseManifest", () => {
  it("parses a single document with apiVersion+kind", () => {
    const r = parseManifest(clawSandboxFixture());
    expect(r.kind).toBe("KarsSandbox");
    expect(r.apiVersion).toBe("kars.azure.com/v1alpha1");
  });
  it("rejects multi-document YAML", () => {
    const yaml = `${clawSandboxFixture()}---\n${clawSandboxFixture()}`;
    expect(() => parseManifest(yaml)).toThrow(/2 documents/);
  });
  it("rejects empty input", () => {
    expect(() => parseManifest("")).toThrow(/empty/);
    expect(() => parseManifest("# only comments\n")).toThrow(/empty/);
  });
  it("rejects non-mapping root", () => {
    expect(() => parseManifest("- foo\n- bar\n")).toThrow(/single mapping/);
  });
  it("rejects missing apiVersion", () => {
    expect(() => parseManifest("kind: X\n")).toThrow(/apiVersion/);
  });
  it("rejects missing kind", () => {
    expect(() => parseManifest("apiVersion: x/v1\n")).toThrow(/kind/);
  });
});

describe("cleanMetadata", () => {
  it("preserves name + namespace + labels + annotations only", () => {
    const m = cleanMetadata({
      name: "demo",
      namespace: "ns",
      labels: { a: "b" },
      annotations: { x: "y" },
      uid: "abc",
      resourceVersion: "1",
      managedFields: [{ manager: "kubectl" }],
      creationTimestamp: "2026-01-01T00:00:00Z",
    });
    expect(m).toEqual({
      name: "demo",
      namespace: "ns",
      labels: { a: "b" },
      annotations: { x: "y" },
    });
  });
  it("returns empty for non-objects", () => {
    expect(cleanMetadata(null)).toEqual({});
    expect(cleanMetadata(undefined)).toEqual({});
    expect(cleanMetadata("string")).toEqual({});
  });
});

describe("mapToEnvArray", () => {
  it("sorts env keys deterministically", () => {
    const arr = mapToEnvArray({ ZULU: "z", ALPHA: "a", MIKE: "m" });
    expect(arr).toEqual([
      { name: "ALPHA", value: "a" },
      { name: "MIKE", value: "m" },
      { name: "ZULU", value: "z" },
    ]);
  });
  it("skips non-string values", () => {
    const arr = mapToEnvArray({ A: "ok", B: 42 as unknown as string, C: null as unknown as string });
    expect(arr).toEqual([{ name: "A", value: "ok" }]);
  });
});

describe("envArrayToMap", () => {
  it("converts simple env array", () => {
    const r = envArrayToMap([{ name: "A", value: "1" }, { name: "B", value: "2" }]);
    expect(r.extraEnv).toEqual({ A: "1", B: "2" });
    expect(r.envWarnings).toEqual([]);
  });
  it("warns on duplicate literal name (last wins)", () => {
    const r = envArrayToMap([
      { name: "A", value: "first" },
      { name: "A", value: "second" },
    ]);
    expect(r.extraEnv).toEqual({ A: "second" });
    expect(r.envWarnings).toEqual([
      'env "A" set multiple times; last literal wins',
    ]);
  });
  it("drops valueFrom and any prior literal for the same name", () => {
    const r = envArrayToMap([
      { name: "TOKEN", value: "stale" },
      { name: "TOKEN", valueFrom: { secretKeyRef: { name: "s", key: "k" } } },
    ]);
    expect(r.extraEnv.TOKEN).toBeUndefined();
    expect(r.envWarnings).toEqual([
      'env "TOKEN" uses valueFrom; dropped (extraEnv supports literal values only)',
    ]);
  });
  it("warns when a literal later overrides a valueFrom", () => {
    const r = envArrayToMap([
      { name: "TOKEN", valueFrom: { secretKeyRef: { name: "s", key: "k" } } },
      { name: "TOKEN", value: "literal" },
    ]);
    expect(r.extraEnv).toEqual({ TOKEN: "literal" });
    expect(r.envWarnings).toEqual([
      'env "TOKEN" uses valueFrom; dropped (extraEnv supports literal values only)',
      'env "TOKEN" later overrides a prior valueFrom with a literal',
    ]);
  });
  it("returns empty for non-array input", () => {
    const r = envArrayToMap(undefined);
    expect(r.extraEnv).toEqual({});
    expect(r.envWarnings).toEqual([]);
  });
});

describe("canonicaliseSeccomp", () => {
  it("Localhost profiles/<name>.json -> bare name (no warning)", () => {
    const w: string[] = [];
    expect(
      canonicaliseSeccomp({ type: "Localhost", localhostProfile: "profiles/kars-strict.json" }, w, "enhanced"),
    ).toBe("kars-strict");
    expect(w).toEqual([]);
  });
  it("Localhost <name>.json -> bare name + warn", () => {
    const w: string[] = [];
    expect(
      canonicaliseSeccomp({ type: "Localhost", localhostProfile: "kars-strict.json" }, w, "enhanced"),
    ).toBe("kars-strict");
    expect(w[0]).toMatch(/lacks profiles\/ prefix/);
  });
  it("Localhost bare name -> bare name + warn", () => {
    const w: string[] = [];
    expect(
      canonicaliseSeccomp({ type: "Localhost", localhostProfile: "kars-strict" }, w, "enhanced"),
    ).toBe("kars-strict");
    expect(w[0]).toMatch(/not in canonical/);
  });
  it("RuntimeDefault on confidential -> no warning", () => {
    const w: string[] = [];
    expect(canonicaliseSeccomp({ type: "RuntimeDefault" }, w, "confidential")).toBeUndefined();
    expect(w).toEqual([]);
  });
  it("RuntimeDefault on enhanced -> warn", () => {
    const w: string[] = [];
    expect(canonicaliseSeccomp({ type: "RuntimeDefault" }, w, "enhanced")).toBeUndefined();
    expect(w[0]).toMatch(/RuntimeDefault on non-confidential/);
  });
  it("unknown type -> warn + undefined", () => {
    const w: string[] = [];
    expect(canonicaliseSeccomp({ type: "Unconfined" }, w, "enhanced")).toBeUndefined();
    expect(w[0]).toMatch(/unknown seccompProfile.type/);
  });
  it("missing localhostProfile -> warn + undefined", () => {
    const w: string[] = [];
    expect(canonicaliseSeccomp({ type: "Localhost" }, w, "enhanced")).toBeUndefined();
    expect(w[0]).toMatch(/Localhost without localhostProfile/);
  });
});

describe("karssandboxToUpstreamSandbox (forward)", () => {
  it("maps standard fields under enhanced isolation", () => {
    const parsed = parseManifest(clawSandboxFixture());
    const r = karssandboxToUpstreamSandbox(parsed);
    expect(r.warnings.filter(w => !w.includes("inferenceRef"))).toEqual([]);
    const m = r.manifest as Record<string, unknown>;
    expect(m.apiVersion).toBe("agents.x-k8s.io/v1alpha1");
    expect(m.kind).toBe("Sandbox");
    const spec = m.spec as Record<string, unknown>;
    const podSpec = (spec.podTemplate as Record<string, unknown>).spec as Record<string, unknown>;
    expect(podSpec.runtimeClassName).toBeUndefined();
    expect(spec.replicas).toBe(1);
    const ctn = (podSpec.containers as Array<Record<string, unknown>>)[0];
    expect(ctn.image).toBe("openclaw:1.2.3");
    expect(ctn.env).toEqual([
      { name: "ALPHA", value: "1" },
      { name: "FOO", value: "bar" },
    ]);
    const sc = ctn.securityContext as Record<string, unknown>;
    expect(sc.seccompProfile).toEqual({
      type: "Localhost",
      localhostProfile: "profiles/kars-strict.json",
    });
    expect(sc.readOnlyRootFilesystem).toBe(true);
    expect(sc.runAsNonRoot).toBe(true);
    expect(sc.allowPrivilegeEscalation).toBe(false);
  });

  it("confidential isolation emits kata runtimeClass + RuntimeDefault seccomp", () => {
    const parsed = parseManifest(
      clawSandboxFixture({
        sandbox: { isolation: "confidential", seccompProfile: "kars-strict" },
      }),
    );
    const r = karssandboxToUpstreamSandbox(parsed);
    const podSpec = (
      (r.manifest.spec as Record<string, unknown>).podTemplate as Record<string, unknown>
    ).spec as Record<string, unknown>;
    expect(podSpec.runtimeClassName).toBe("kata-vm-isolation");
    const sc = (podSpec.containers as Array<Record<string, unknown>>)[0]
      .securityContext as Record<string, unknown>;
    expect(sc.seccompProfile).toEqual({ type: "RuntimeDefault" });
  });

  it("seccompProfile RuntimeDefault or empty emits RuntimeDefault", () => {
    for (const seccomp of ["RuntimeDefault", ""]) {
      const parsed = parseManifest(
        clawSandboxFixture({
          sandbox: { isolation: "enhanced", seccompProfile: seccomp },
        }),
      );
      const r = karssandboxToUpstreamSandbox(parsed);
      const sc = (
        ((r.manifest.spec as Record<string, unknown>).podTemplate as Record<string, unknown>)
          .spec as Record<string, unknown>
      );
      const ctnSc = (sc.containers as Array<Record<string, unknown>>)[0]
        .securityContext as Record<string, unknown>;
      expect(ctnSc.seccompProfile).toEqual({ type: "RuntimeDefault" });
    }
  });

  it("warns once per kars-only field", () => {
    const parsed = parseManifest(
      clawSandboxFixture({
        inference: { provider: "azure-openai", model: "gpt-4o" },
        governance: { agtEnabled: true },
        a2a: { enabled: true },
        agent: { name: "fa1" },
        azureServices: [{ kind: "blob" }],
        networkPolicy: { allowEgress: ["*.azure.com"] },
        upstreamCompatibility: { sigsAgentSandbox: "off" },
      }),
    );
    const r = karssandboxToUpstreamSandbox(parsed);
    expect(r.warnings).toHaveLength(7);
    expect(r.warnings.some((w) => w.includes("spec.inference"))).toBe(true);
    expect(r.warnings.some((w) => w.includes("spec.governance"))).toBe(true);
    expect(r.warnings.some((w) => w.includes("spec.a2a"))).toBe(true);
    expect(r.warnings.some((w) => w.includes("spec.agent"))).toBe(true);
    expect(r.warnings.some((w) => w.includes("spec.azureServices"))).toBe(true);
    expect(r.warnings.some((w) => w.includes("spec.networkPolicy"))).toBe(true);
    expect(r.warnings.some((w) => w.includes("spec.upstreamCompatibility"))).toBe(true);
  });

  it("rejects wrong source kind", () => {
    const parsed = parseManifest(upstreamSandboxFixture());
    expect(() => karssandboxToUpstreamSandbox(parsed)).toThrow(/expected source kind=KarsSandbox/);
  });

  it("rejects missing image", () => {
    const yaml = yamlStringify({
      apiVersion: "kars.azure.com/v1alpha1",
      kind: "KarsSandbox",
      metadata: { name: "x" },
      spec: { runtime: { kind: "OpenClaw", openclaw: {} }, sandbox: { isolation: "enhanced" } },
    });
    expect(() => karssandboxToUpstreamSandbox(parseManifest(yaml))).toThrow(/image required/);
  });

  it("warns on dropped status block", () => {
    const yaml = yamlStringify({
      apiVersion: "kars.azure.com/v1alpha1",
      kind: "KarsSandbox",
      metadata: { name: "x" },
      spec: {
        runtime: { kind: "OpenClaw", openclaw: { image: "x:1" } },
        sandbox: { isolation: "enhanced", seccompProfile: "kars-strict" },
      },
      status: { phase: "Ready" },
    });
    const r = karssandboxToUpstreamSandbox(parseManifest(yaml));
    expect(r.warnings.some((w) => w.includes("status block"))).toBe(true);
  });
});

describe("upstreamSandboxToClawsandbox (inverse)", () => {
  it("happy-path round-trip restores KarsSandbox shape", () => {
    const r = upstreamSandboxToClawsandbox(parseManifest(upstreamSandboxFixture()));
    expect(r.warnings.filter(w => !w.includes("inferenceRef"))).toEqual([]);
    const spec = (r.manifest as Record<string, unknown>).spec as Record<string, unknown>;
    const runtime = spec.runtime as Record<string, unknown>;
    expect(runtime.kind).toBe("OpenClaw");
    const openclaw = runtime.openclaw as Record<string, unknown>;
    expect(openclaw.image).toBe("openclaw:1.2.3");
    expect(openclaw.extraEnv).toEqual({ ALPHA: "1", FOO: "bar" });
    const sandbox = spec.sandbox as Record<string, unknown>;
    expect(sandbox.isolation).toBe("enhanced");
    expect(sandbox.seccompProfile).toBe("kars-strict");
    expect(sandbox.readOnlyRootFilesystem).toBe(true);
  });

  it("kata-vm-isolation -> confidential", () => {
    const yaml = yamlStringify({
      apiVersion: "agents.x-k8s.io/v1alpha1",
      kind: "Sandbox",
      metadata: { name: "x" },
      spec: {
        podTemplate: {
          spec: {
            runtimeClassName: "kata-vm-isolation",
            containers: [{ name: "c", image: "x:1", securityContext: { seccompProfile: { type: "RuntimeDefault" } } }],
          },
        },
      },
    });
    const r = upstreamSandboxToClawsandbox(parseManifest(yaml));
    const sandbox = (
      (r.manifest as Record<string, unknown>).spec as Record<string, unknown>
    ).sandbox as Record<string, unknown>;
    expect(sandbox.isolation).toBe("confidential");
    expect(r.warnings.filter(w => !w.includes("inferenceRef"))).toEqual([]);
  });

  it("unknown runtimeClassName -> defaults to enhanced + warn", () => {
    const yaml = yamlStringify({
      apiVersion: "agents.x-k8s.io/v1alpha1",
      kind: "Sandbox",
      metadata: { name: "x" },
      spec: {
        podTemplate: {
          spec: {
            runtimeClassName: "weird-vm",
            containers: [{ name: "c", image: "x:1" }],
          },
        },
      },
    });
    const r = upstreamSandboxToClawsandbox(parseManifest(yaml));
    const sandbox = (
      (r.manifest as Record<string, unknown>).spec as Record<string, unknown>
    ).sandbox as Record<string, unknown>;
    expect(sandbox.isolation).toBe("enhanced");
    expect(r.warnings.some((w) => w.includes('runtimeClassName="weird-vm"'))).toBe(true);
  });

  it("multi-container input -> first wins + warn", () => {
    const yaml = yamlStringify({
      apiVersion: "agents.x-k8s.io/v1alpha1",
      kind: "Sandbox",
      metadata: { name: "x" },
      spec: {
        podTemplate: {
          spec: {
            containers: [
              { name: "primary", image: "p:1" },
              { name: "side", image: "s:1" },
            ],
          },
        },
      },
    });
    const r = upstreamSandboxToClawsandbox(parseManifest(yaml));
    expect(r.warnings.some((w) => w.includes("2 containers"))).toBe(true);
    const openclaw = (
      ((r.manifest as Record<string, unknown>).spec as Record<string, unknown>)
        .runtime as Record<string, unknown>
    ).openclaw as Record<string, unknown>;
    expect(openclaw.image).toBe("p:1");
  });

  it("warns on lossy upstream-only fields", () => {
    const yaml = yamlStringify({
      apiVersion: "agents.x-k8s.io/v1alpha1",
      kind: "Sandbox",
      metadata: { name: "x" },
      spec: {
        podTemplate: {
          metadata: { labels: { app: "x" }, annotations: { "x.io/n": "v" } },
          spec: {
            containers: [{ name: "c", image: "x:1" }],
            volumes: [{ name: "v", emptyDir: {} }],
            initContainers: [{ name: "init", image: "init:1" }],
            serviceAccountName: "custom-sa",
            hostNetwork: true,
            hostPID: true,
            hostIPC: true,
            nodeSelector: { tier: "dedicated" },
            affinity: { nodeAffinity: {} },
            tolerations: [{ key: "x" }],
            imagePullSecrets: [{ name: "regcred" }],
          },
        },
        replicas: 0,
        shutdownTime: "2026-12-31T23:59:59Z",
        shutdownPolicy: "Delete",
        volumeClaimTemplates: [{ metadata: { name: "data" }, spec: { accessModes: ["ReadWriteOnce"] } }],
      },
    });
    const r = upstreamSandboxToClawsandbox(parseManifest(yaml));
    const expected = [
      "shutdownTime",
      "shutdownPolicy",
      "volumeClaimTemplates",
      "replicas=0",
      "spec.volumes",
      "initContainers",
      "serviceAccountName",
      "hostNetwork",
      "hostPID",
      "hostIPC",
      "nodeSelector",
      "affinity",
      "tolerations",
      "imagePullSecrets",
      "metadata.labels",
      "metadata.annotations",
    ];
    for (const fragment of expected) {
      expect(r.warnings.some((w) => w.includes(fragment))).toBe(true);
    }
  });

  it("rejects wrong source kind", () => {
    expect(() => upstreamSandboxToClawsandbox(parseManifest(clawSandboxFixture()))).toThrow(
      /expected source kind=Sandbox/,
    );
  });

  it("rejects missing podTemplate / spec / containers / image", () => {
    const noTpl = yamlStringify({
      apiVersion: "agents.x-k8s.io/v1alpha1",
      kind: "Sandbox",
      metadata: { name: "x" },
      spec: {},
    });
    expect(() => upstreamSandboxToClawsandbox(parseManifest(noTpl))).toThrow(/missing spec.podTemplate/);

    const noContainers = yamlStringify({
      apiVersion: "agents.x-k8s.io/v1alpha1",
      kind: "Sandbox",
      metadata: { name: "x" },
      spec: { podTemplate: { spec: { containers: [] } } },
    });
    expect(() => upstreamSandboxToClawsandbox(parseManifest(noContainers))).toThrow(/no containers/);

    const noImage = yamlStringify({
      apiVersion: "agents.x-k8s.io/v1alpha1",
      kind: "Sandbox",
      metadata: { name: "x" },
      spec: { podTemplate: { spec: { containers: [{ name: "c" }] } } },
    });
    expect(() => upstreamSandboxToClawsandbox(parseManifest(noImage))).toThrow(/missing image/);
  });
});

describe("emitOverlay", () => {
  it("emits a fresh KarsSandbox skeleton with overlay binding", () => {
    const r = emitOverlay(parseManifest(upstreamSandboxFixture()), "demo");
    const m = r.manifest as Record<string, unknown>;
    expect(m.apiVersion).toBe("kars.azure.com/v1alpha1");
    expect(m.kind).toBe("KarsSandbox");
    expect((m.metadata as Record<string, unknown>).namespace).toBe("kars-demo");
    const spec = m.spec as Record<string, unknown>;
    const upc = spec.upstreamCompatibility as Record<string, unknown>;
    expect(upc.sigsAgentSandbox).toBe("overlay");
    expect(upc.upstreamSandboxRef).toEqual({ name: "demo" });
    expect(spec.runtime).toBeUndefined();
    expect(spec.sandbox).toBeUndefined();
    expect(spec.resources).toBeUndefined();
    expect(r.warnings.some((w) => w.includes("no governance fields"))).toBe(true);
  });

  it("accepts ns/name and validates against input namespace", () => {
    const r = emitOverlay(parseManifest(upstreamSandboxFixture()), "kars-demo/demo");
    const upc = (
      (r.manifest as Record<string, unknown>).spec as Record<string, unknown>
    ).upstreamCompatibility as Record<string, unknown>;
    expect(upc.upstreamSandboxRef).toEqual({ name: "demo" });
  });

  it("rejects ns mismatch", () => {
    expect(() =>
      emitOverlay(parseManifest(upstreamSandboxFixture()), "other-ns/demo"),
    ).toThrow(/does not match input metadata.namespace/);
  });

  it("rejects KarsSandbox source", () => {
    expect(() => emitOverlay(parseManifest(clawSandboxFixture()), "demo")).toThrow(
      /requires source kind=Sandbox/,
    );
  });

  it("rejects empty name after slash", () => {
    expect(() => emitOverlay(parseManifest(upstreamSandboxFixture()), "ns/")).toThrow(/empty after namespace/);
  });
});

describe("dispatch", () => {
  it("karssandbox target -> inverse", () => {
    const r = dispatch(parseManifest(upstreamSandboxFixture()), { target: "karssandbox" });
    expect((r.manifest as Record<string, unknown>).kind).toBe("KarsSandbox");
  });
  it("upstream-sandbox target -> forward", () => {
    const r = dispatch(parseManifest(clawSandboxFixture()), { target: "upstream-sandbox" });
    expect((r.manifest as Record<string, unknown>).kind).toBe("Sandbox");
  });
  it("overlay target requires --sandbox-ref", () => {
    expect(() =>
      dispatch(parseManifest(upstreamSandboxFixture()), { target: "overlay" }),
    ).toThrow(/--sandbox-ref/);
  });
});

describe("formatYaml", () => {
  it("emits stable YAML", () => {
    const out = formatYaml({ apiVersion: "v1", kind: "X", metadata: { name: "demo" } });
    expect(out).toContain("apiVersion: v1");
    expect(out).toContain("kind: X");
    expect(out).toContain("name: demo");
  });
});

describe("round-trip stability", () => {
  it("KarsSandbox -> upstream -> KarsSandbox preserves core fields", () => {
    const original = clawSandboxFixture();
    const forward = karssandboxToUpstreamSandbox(parseManifest(original));
    const inverse = upstreamSandboxToClawsandbox(parseManifest(yamlStringify(forward.manifest)));
    const spec = (inverse.manifest as Record<string, unknown>).spec as Record<string, unknown>;
    const runtime = spec.runtime as Record<string, unknown>;
    expect(runtime.kind).toBe("OpenClaw");
    const openclaw = runtime.openclaw as Record<string, unknown>;
    expect(openclaw.image).toBe("openclaw:1.2.3");
    expect(openclaw.extraEnv).toEqual({ ALPHA: "1", FOO: "bar" });
    const sandbox = spec.sandbox as Record<string, unknown>;
    expect(sandbox.isolation).toBe("enhanced");
    expect(sandbox.seccompProfile).toBe("kars-strict");
  });
});
