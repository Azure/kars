// Phase 2 S9.3 — vitest cases for the kagent → AzureClaw translator.
//
// Pure-helper coverage. CLI runner I/O is exercised separately via
// the existing migrate command e2e tests.

import { describe, expect, it } from "vitest";
import {
  __test,
  AZURECLAW_GROUP,
  AZURECLAW_VERSION,
  InvalidInputError,
  KAGENT_API_VERSION,
  KAGENT_KIND,
  SANDBOX_LABEL_KEY,
} from "./from_kagent.js";

const {
  sanitizeDnsName,
  hashSuffix,
  generateToolPolicyName,
  cleanMetadata,
  envArrayToMap,
  projectDescription,
  translate,
} = __test;

const baseAgent = {
  apiVersion: KAGENT_API_VERSION,
  kind: KAGENT_KIND,
  metadata: { name: "alice", namespace: "team-a" },
  spec: {
    type: "BYO",
    byo: { deployment: { image: "ghcr.io/example/agent:1.2.3" } },
  },
};

// ---- DNS sanitization ------------------------------------------------------

describe("sanitizeDnsName", () => {
  it("lowercases and replaces non-DNS chars", () => {
    expect(sanitizeDnsName("MyTool/v2")).toBe("mytool-v2");
  });
  it("collapses repeated separators", () => {
    expect(sanitizeDnsName("a---b")).toBe("a-b");
  });
  it("trims leading/trailing dashes", () => {
    expect(sanitizeDnsName("--abc--")).toBe("abc");
  });
  it("falls back to 'x' for empty after sanitization", () => {
    expect(sanitizeDnsName("---")).toBe("x");
    expect(sanitizeDnsName("")).toBe("x");
    expect(sanitizeDnsName("///")).toBe("x");
  });
  it("keeps numbers and dashes verbatim", () => {
    expect(sanitizeDnsName("agent-007-prod")).toBe("agent-007-prod");
  });
});

describe("hashSuffix", () => {
  it("is deterministic", () => {
    expect(hashSuffix(["a", "b", "c"])).toBe(hashSuffix(["a", "b", "c"]));
  });
  it("differs across distinct tuples", () => {
    expect(hashSuffix(["a", "b"])).not.toBe(hashSuffix(["b", "a"]));
  });
  it("returns hex of length 6", () => {
    expect(hashSuffix(["x"])).toMatch(/^[0-9a-f]{6}$/);
  });
});

describe("generateToolPolicyName", () => {
  it("yields ≤ 63-char DNS-safe names", () => {
    const n = generateToolPolicyName(
      "very-long-sandbox-name-that-pushes-the-limit",
      "mcp-server-with-a-rather-verbose-name",
      "tool/with/path",
      0,
    );
    expect(n.length).toBeLessThanOrEqual(63);
    expect(n).toMatch(/^[a-z0-9-]+-[0-9a-f]{6}$/);
  });
  it("disambiguates collisions via hash on the unsanitized tuple", () => {
    const a = generateToolPolicyName("s", "m", "Foo/Bar", 0);
    const b = generateToolPolicyName("s", "m", "foo-bar", 0);
    // Both sanitize the prefix to the same DNS form; the hash suffix
    // on the *unsanitized* input keeps them distinct.
    expect(a).not.toBe(b);
  });
});

// ---- metadata --------------------------------------------------------------

describe("cleanMetadata", () => {
  it("strips server-managed fields", () => {
    expect(
      cleanMetadata({
        name: "n",
        namespace: "ns",
        uid: "abc",
        resourceVersion: "1",
        generation: 5,
        creationTimestamp: "2024-01-01",
        managedFields: [{ x: 1 }],
        ownerReferences: [{ name: "o" }],
        finalizers: ["f"],
      }),
    ).toEqual({ name: "n", namespace: "ns" });
  });
  it("filters kubectl annotations", () => {
    const out = cleanMetadata({
      name: "n",
      annotations: {
        "kubectl.kubernetes.io/last-applied-configuration": "{}",
        "user.example/keep": "yes",
      },
    });
    expect(out.annotations).toEqual({ "user.example/keep": "yes" });
  });
  it("returns empty for non-objects", () => {
    expect(cleanMetadata(null)).toEqual({});
    expect(cleanMetadata("x")).toEqual({});
    expect(cleanMetadata(undefined)).toEqual({});
  });
});

// ---- env projection --------------------------------------------------------

describe("envArrayToMap", () => {
  it("projects literal entries last-wins with warning on redefine", () => {
    const w: ReturnType<typeof translate>["warnings"] = [];
    const out = envArrayToMap(
      [
        { name: "FOO", value: "1" },
        { name: "BAR", value: "x" },
        { name: "FOO", value: "2" },
      ],
      "spec.env",
      w,
    );
    expect(out).toEqual({ FOO: "2", BAR: "x" });
    expect(w).toHaveLength(1);
    expect(w[0]!.message).toMatch(/redefined/);
  });
  it("warns and drops valueFrom entries", () => {
    const w: ReturnType<typeof translate>["warnings"] = [];
    const out = envArrayToMap(
      [{ name: "S", valueFrom: { secretKeyRef: { name: "x", key: "k" } } }],
      "spec.env",
      w,
    );
    expect(out).toEqual({});
    expect(w[0]!.message).toMatch(/valueFrom/);
  });
  it("drops a prior literal when later valueFrom for same name appears", () => {
    const w: ReturnType<typeof translate>["warnings"] = [];
    const out = envArrayToMap(
      [
        { name: "TOK", value: "abc" },
        { name: "TOK", valueFrom: { secretKeyRef: { name: "s", key: "k" } } },
      ],
      "spec.env",
      w,
    );
    expect(out).toEqual({});
    expect(w.some((x) => x.message.includes("prior literal dropped"))).toBe(true);
  });
});

// ---- description -----------------------------------------------------------

describe("projectDescription", () => {
  it("passes through small descriptions verbatim", () => {
    const w: ReturnType<typeof translate>["warnings"] = [];
    expect(projectDescription("hi", w)).toEqual({ annotation: "hi" });
    expect(w).toHaveLength(0);
  });
  it("truncates and warns when above 4 KiB", () => {
    const big = "a".repeat(5000);
    const w: ReturnType<typeof translate>["warnings"] = [];
    const out = projectDescription(big, w);
    expect(out.truncated).toBe(true);
    expect(out.annotation!.length).toBeLessThanOrEqual(4096);
    expect(w).toHaveLength(1);
  });
});

// ---- translate (full) ------------------------------------------------------

describe("translate: input gating", () => {
  it("rejects wrong apiVersion", () => {
    expect(() =>
      translate({ apiVersion: "v1", kind: "Agent", metadata: { name: "a" } }, {}),
    ).toThrowError(InvalidInputError);
  });
  it("rejects wrong kind", () => {
    expect(() =>
      translate(
        { apiVersion: KAGENT_API_VERSION, kind: "Pod", metadata: { name: "a" } },
        {},
      ),
    ).toThrowError(InvalidInputError);
  });
  it("rejects missing metadata.name", () => {
    expect(() =>
      translate(
        { apiVersion: KAGENT_API_VERSION, kind: KAGENT_KIND, metadata: {} },
        {},
      ),
    ).toThrowError(/metadata\.name/);
  });
  it("rejects unknown spec.type", () => {
    expect(() =>
      translate(
        { ...baseAgent, spec: { type: "Other" } as unknown as typeof baseAgent.spec },
        {},
      ),
    ).toThrowError(/spec\.type/);
  });
  it("rejects BYO without image", () => {
    expect(() =>
      translate({ ...baseAgent, spec: { type: "BYO", byo: {} } }, {}),
    ).toThrowError(/byo\.deployment\.image/);
  });
});

describe("translate: ClawSandbox basics", () => {
  it("emits a ClawSandbox with sandbox-label and provenance annotations", () => {
    const r = translate(baseAgent, {});
    const cs = r.resources[0]!;
    expect(cs.kind).toBe("ClawSandbox");
    expect(cs.apiVersion).toBe(`${AZURECLAW_GROUP}/${AZURECLAW_VERSION}`);
    expect(cs.metadata.name).toBe("alice");
    expect(cs.metadata.namespace).toBe("team-a");
    expect(cs.metadata.labels![SANDBOX_LABEL_KEY]).toBe("alice");
    expect(cs.metadata.annotations![`${AZURECLAW_GROUP}/migrated-from`]).toBe(
      `${KAGENT_API_VERSION} ${KAGENT_KIND}`,
    );
    expect(cs.metadata.annotations![`${AZURECLAW_GROUP}/kagent-agent`]).toBe(
      "team-a/alice",
    );
    expect(((cs.spec.runtime as { openclaw: { image: string } }).openclaw).image).toBe(
      "ghcr.io/example/agent:1.2.3",
    );
    expect((cs.spec.sandbox as { isolation: string }).isolation).toBe("enhanced");
  });

  it("honours --isolation override", () => {
    const r = translate(baseAgent, { isolation: "confidential" });
    expect(
      (r.resources[0]!.spec.sandbox as { isolation: string }).isolation,
    ).toBe("confidential");
  });

  it("rejects pre-existing conflicting sandbox label", () => {
    expect(() =>
      translate(
        {
          ...baseAgent,
          metadata: {
            ...baseAgent.metadata,
            labels: { [SANDBOX_LABEL_KEY]: "different" },
          },
        },
        {},
      ),
    ).toThrowError(/already set/);
  });

  it("preserves a pre-existing matching sandbox label", () => {
    const r = translate(
      {
        ...baseAgent,
        metadata: {
          ...baseAgent.metadata,
          labels: { [SANDBOX_LABEL_KEY]: "alice", team: "a" },
        },
      },
      {},
    );
    expect(r.resources[0]!.metadata.labels).toMatchObject({
      team: "a",
      [SANDBOX_LABEL_KEY]: "alice",
    });
  });

  it("warns on namespace override mismatch", () => {
    const r = translate(baseAgent, { namespace: "other" });
    expect(r.summary.namespace).toBe("other");
    expect(
      r.warnings.some((w) =>
        w.message.includes("overriding input namespace 'team-a'"),
      ),
    ).toBe(true);
  });

  it("does not warn when --namespace matches input", () => {
    const r = translate(baseAgent, { namespace: "team-a" });
    expect(r.warnings.filter((w) => w.path === "metadata.namespace")).toHaveLength(
      0,
    );
  });
});

describe("translate: Declarative agent runnability", () => {
  const decl = {
    apiVersion: KAGENT_API_VERSION,
    kind: KAGENT_KIND,
    metadata: { name: "decl", namespace: "ns" },
    spec: {
      type: "Declarative",
      declarative: { runtime: "python", modelConfig: "gpt4-config" },
    },
  };

  it("emits a non-runnable ClawSandbox when no image is provided", () => {
    const r = translate(decl, {});
    expect(r.summary.runnable).toBe(false);
    expect(r.resources[0]!.spec.runtime).toBeUndefined();
    expect(
      r.warnings.some((w) => w.message.includes("--image to override")),
    ).toBe(true);
  });

  it("uses --image override for Declarative", () => {
    const r = translate(decl, { image: "myorg/runtime:v1" });
    expect(r.summary.runnable).toBe(true);
    expect(((r.resources[0]!.spec.runtime as { openclaw: { image: string } }).openclaw).image).toBe(
      "myorg/runtime:v1",
    );
  });

  it("emits InferencePolicy with provenance annotation when modelConfig is set", () => {
    const r = translate(decl, { image: "myorg/runtime:v1" });
    const ip = r.resources.find((x) => x.kind === "InferencePolicy");
    expect(ip).toBeDefined();
    expect(ip!.metadata.annotations![`${AZURECLAW_GROUP}/kagent-model-config`]).toBe(
      "gpt4-config",
    );
    expect((ip!.spec.appliesTo as { sandboxName: string }).sandboxName).toBe(
      "decl",
    );
    expect(
      r.warnings.some((w) => w.path === "spec.declarative.modelConfig"),
    ).toBe(true);
  });

  it("does NOT emit InferencePolicy when modelConfig is absent", () => {
    const noModel = {
      ...decl,
      spec: { type: "Declarative", declarative: { runtime: "python" } },
    };
    const r = translate(noModel, { image: "x" });
    expect(r.resources.find((x) => x.kind === "InferencePolicy")).toBeUndefined();
    expect(r.summary.inferencePolicyCount).toBe(0);
  });
});

describe("translate: Tools", () => {
  const withTools = (tools: unknown[]) => ({
    apiVersion: KAGENT_API_VERSION,
    kind: KAGENT_KIND,
    metadata: { name: "t", namespace: "ns" },
    spec: { type: "Declarative", declarative: { tools } },
  });

  it("fans out one ToolPolicy per (mcpServer, toolName)", () => {
    const r = translate(
      withTools([
        {
          type: "McpServer",
          mcpServer: { name: "fs", toolNames: ["read", "write"] },
        },
      ]),
      { image: "x" },
    );
    const tps = r.resources.filter((x) => x.kind === "ToolPolicy");
    expect(tps).toHaveLength(2);
    expect(tps.map((t) => (t.spec.appliesTo as { tool: string }).tool).sort()).toEqual(
      ["read", "write"],
    );
    // governance auto-enabled when at least one ToolPolicy is emitted.
    expect((r.resources[0]!.spec.governance as { enabled: boolean }).enabled).toBe(
      true,
    );
  });

  it("sets approval.mode='always' only for tools listed in requireApproval", () => {
    const r = translate(
      withTools([
        {
          type: "McpServer",
          mcpServer: {
            name: "git",
            toolNames: ["status", "push"],
            requireApproval: ["push"],
          },
        },
      ]),
      { image: "x" },
    );
    const tps = r.resources.filter((x) => x.kind === "ToolPolicy");
    const push = tps.find((t) => (t.spec.appliesTo as { tool: string }).tool === "push")!;
    const status = tps.find(
      (t) => (t.spec.appliesTo as { tool: string }).tool === "status",
    )!;
    expect((push.spec.approval as { mode: string }).mode).toBe("always");
    expect(status.spec.approval).toBeUndefined();
  });

  it("emits a wildcard ToolPolicy when toolNames is empty and warns", () => {
    const r = translate(
      withTools([{ type: "McpServer", mcpServer: { name: "fs" } }]),
      { image: "x" },
    );
    const tps = r.resources.filter((x) => x.kind === "ToolPolicy");
    expect(tps).toHaveLength(1);
    expect((tps[0]!.spec.appliesTo as { tool: string }).tool).toBe("*");
    expect(r.warnings.some((w) => w.message.includes("wildcard"))).toBe(true);
  });

  it("rejects McpServer tool with missing name", () => {
    expect(() =>
      translate(withTools([{ type: "McpServer", mcpServer: {} }]), { image: "x" }),
    ).toThrowError(/mcpServer\.name/);
  });

  it("rejects type=Agent with missing agent.name", () => {
    expect(() =>
      translate(withTools([{ type: "Agent", agent: {} }]), { image: "x" }),
    ).toThrowError(/agent.*name/);
  });

  it("warns and drops type=Agent tools (does not emit ToolPolicy)", () => {
    const r = translate(
      withTools([{ type: "Agent", agent: { name: "other-agent" } }]),
      { image: "x" },
    );
    expect(r.resources.filter((x) => x.kind === "ToolPolicy")).toHaveLength(0);
    expect(
      r.warnings.some((w) => w.message.includes("agent-as-tool not supported")),
    ).toBe(true);
  });

  it("emits ToolPolicies with deterministic name suffix and stable order", () => {
    const r = translate(
      withTools([
        { type: "McpServer", mcpServer: { name: "b", toolNames: ["x"] } },
        { type: "McpServer", mcpServer: { name: "a", toolNames: ["y"] } },
      ]),
      { image: "x" },
    );
    const tps = r.resources.filter((x) => x.kind === "ToolPolicy");
    const names = tps.map((t) => t.metadata.name);
    expect(names).toEqual([...names].sort());
  });

  it("dedupes repeated toolNames", () => {
    const r = translate(
      withTools([
        {
          type: "McpServer",
          mcpServer: { name: "fs", toolNames: ["read", "read"] },
        },
      ]),
      { image: "x" },
    );
    expect(r.resources.filter((x) => x.kind === "ToolPolicy")).toHaveLength(1);
  });

  it("warns on Tool.headersFrom and mcpServer.allowedHeaders", () => {
    const r = translate(
      withTools([
        {
          type: "McpServer",
          mcpServer: {
            name: "x",
            toolNames: ["t"],
            allowedHeaders: ["x-tenant"],
          },
          headersFrom: [{ secretRef: { name: "s", key: "k" } }],
        },
      ]),
      { image: "x" },
    );
    expect(r.warnings.some((w) => w.path.endsWith(".headersFrom"))).toBe(true);
    expect(r.warnings.some((w) => w.path.endsWith(".allowedHeaders"))).toBe(true);
  });
});

describe("translate: lossy fields", () => {
  it("warns on spec.skills (refs)", () => {
    const r = translate(
      { ...baseAgent, spec: { ...baseAgent.spec, skills: { refs: ["a"] } } },
      {},
    );
    expect(r.warnings.some((w) => w.path === "spec.skills")).toBe(true);
  });
  it("warns on spec.allowedNamespaces", () => {
    const r = translate(
      { ...baseAgent, spec: { ...baseAgent.spec, allowedNamespaces: { from: "All" } } },
      {},
    );
    expect(r.warnings.some((w) => w.path === "spec.allowedNamespaces")).toBe(true);
  });
  it("warns on each declarative-only field", () => {
    const decl = {
      apiVersion: KAGENT_API_VERSION,
      kind: KAGENT_KIND,
      metadata: { name: "d", namespace: "ns" },
      spec: {
        type: "Declarative",
        declarative: {
          systemMessage: "hi",
          systemMessageFrom: { configMapKeyRef: { name: "x", key: "k" } },
          promptTemplate: { dataSources: [] },
          runtime: "python",
          stream: true,
          executeCodeBlocks: true,
          memory: { modelConfig: "m" },
          context: { compaction: {} },
          a2aConfig: { skills: [] },
        },
      },
    };
    const r = translate(decl, { image: "x" });
    const paths = new Set(r.warnings.map((w) => w.path));
    for (const k of [
      "spec.declarative.systemMessage",
      "spec.declarative.systemMessageFrom",
      "spec.declarative.promptTemplate",
      "spec.declarative.runtime",
      "spec.declarative.stream",
      "spec.declarative.executeCodeBlocks",
      "spec.declarative.memory",
      "spec.declarative.context",
      "spec.declarative.a2aConfig",
    ]) {
      expect(paths.has(k)).toBe(true);
    }
  });

  it("warns on each unsupported deployment field (BYO)", () => {
    const r = translate(
      {
        ...baseAgent,
        spec: {
          type: "BYO",
          byo: {
            deployment: {
              image: "img",
              replicas: 3,
              tolerations: [{ key: "x" }],
              affinity: { nodeAffinity: {} },
              nodeSelector: { zone: "x" },
              imagePullPolicy: "Always",
              imagePullSecrets: [{ name: "s" }],
              volumes: [{ name: "v", emptyDir: {} }],
              volumeMounts: [{ name: "v", mountPath: "/x" }],
              securityContext: {},
              podSecurityContext: {},
              serviceAccountName: "sa",
            },
          },
        },
      },
      {},
    );
    const paths = new Set(r.warnings.map((w) => w.path));
    for (const k of [
      "spec.byo.deployment.replicas",
      "spec.byo.deployment.tolerations",
      "spec.byo.deployment.affinity",
      "spec.byo.deployment.nodeSelector",
      "spec.byo.deployment.imagePullPolicy",
      "spec.byo.deployment.imagePullSecrets",
      "spec.byo.deployment.volumes",
      "spec.byo.deployment.volumeMounts",
      "spec.byo.deployment.securityContext",
      "spec.byo.deployment.podSecurityContext",
      "spec.byo.deployment.serviceAccountName",
    ]) {
      expect(paths.has(k)).toBe(true);
    }
  });
});

describe("translate: networking", () => {
  it("projects allowedDomains into networkPolicy.allowedEndpoints", () => {
    const r = translate(
      {
        ...baseAgent,
        spec: {
          ...baseAgent.spec,
          sandbox: { network: { allowedDomains: ["api.example.com", "db.local"] } },
        },
      },
      {},
    );
    const np = r.resources[0]!.spec.networkPolicy as {
      defaultDeny: boolean;
      allowedEndpoints: { host: string }[];
    };
    expect(np.defaultDeny).toBe(true);
    expect(np.allowedEndpoints.map((e) => e.host)).toEqual([
      "api.example.com",
      "db.local",
    ]);
  });

  it("warns on wildcard domains but still passes them through", () => {
    const r = translate(
      {
        ...baseAgent,
        spec: {
          ...baseAgent.spec,
          sandbox: { network: { allowedDomains: ["*.example.com"] } },
        },
      },
      {},
    );
    const np = r.resources[0]!.spec.networkPolicy as {
      allowedEndpoints: { host: string }[];
    };
    expect(np.allowedEndpoints[0]!.host).toBe("*.example.com");
    expect(r.warnings.some((w) => w.message.includes("wildcard"))).toBe(true);
  });
});

describe("translate: bundle ordering and JSON shape", () => {
  it("orders bundle: ClawSandbox, InferencePolicy, ToolPolicies", () => {
    const r = translate(
      {
        apiVersion: KAGENT_API_VERSION,
        kind: KAGENT_KIND,
        metadata: { name: "z", namespace: "ns" },
        spec: {
          type: "Declarative",
          declarative: {
            modelConfig: "m",
            tools: [
              { type: "McpServer", mcpServer: { name: "fs", toolNames: ["r"] } },
            ],
          },
        },
      },
      { image: "x" },
    );
    expect(r.resources.map((x) => x.kind)).toEqual([
      "ClawSandbox",
      "InferencePolicy",
      "ToolPolicy",
    ]);
  });

  it("produces a clean BYO happy-path with zero warnings", () => {
    const r = translate(baseAgent, {});
    expect(r.warnings).toHaveLength(0);
    expect(r.summary.runnable).toBe(true);
  });

  it("preserves env entries on BYO deployment", () => {
    const r = translate(
      {
        ...baseAgent,
        spec: {
          type: "BYO",
          byo: {
            deployment: {
              image: "img",
              env: [
                { name: "FOO", value: "1" },
                { name: "BAR", value: "two" },
              ],
            },
          },
        },
      },
      {},
    );
    expect(((r.resources[0]!.spec.runtime as { openclaw: { extraEnv: Record<string, string> } }).openclaw).extraEnv).toEqual({
      FOO: "1",
      BAR: "two",
    });
    expect(r.warnings).toHaveLength(0);
  });
});

describe("translate: description handling", () => {
  it("preserves short descriptions verbatim as annotation", () => {
    const r = translate(
      { ...baseAgent, spec: { ...baseAgent.spec, description: "a friendly agent" } },
      {},
    );
    expect(
      r.resources[0]!.metadata.annotations![`${AZURECLAW_GROUP}/kagent-description`],
    ).toBe("a friendly agent");
    expect(r.warnings.filter((w) => w.path === "spec.description")).toHaveLength(0);
  });
  it("truncates and warns on huge descriptions", () => {
    const r = translate(
      {
        ...baseAgent,
        spec: { ...baseAgent.spec, description: "x".repeat(5000) },
      },
      {},
    );
    expect(
      r.resources[0]!.metadata.annotations![
        `${AZURECLAW_GROUP}/kagent-description-truncated`
      ],
    ).toBe("true");
    expect(r.warnings.some((w) => w.path === "spec.description")).toBe(true);
  });
});
