import { describe, it, expect, vi } from "vitest";

/**
 * Tests for the `policy` command's data-transformation logic.
 *
 * The policy command has three sub-commands: allow, deny, get.
 * We test the pure data logic: endpoint list manipulation,
 * patch payload construction, and withAdminAuth from router-admin.
 */

// --- Helpers that mirror the logic in policy.ts ---

interface Endpoint {
  host: string;
  port: number;
}

/** Build a merge-patch payload to update allowedEndpoints (mirrors policy allow). */
function buildAllowPatch(existing: Endpoint[], host: string, port: number) {
  const updated = [...existing, { host, port }];
  return {
    spec: { networkPolicy: { allowedEndpoints: updated } },
  };
}

/** Filter endpoints to remove a host (mirrors policy deny). */
function buildDenyPatch(existing: Endpoint[], hostToRemove: string) {
  const filtered = existing.filter((ep) => ep.host !== hostToRemove);
  const wasRemoved = filtered.length < existing.length;
  return { filtered, wasRemoved };
}

/** Build a full policy-learn apply patch (mirrors policy learn --apply). */
function buildLearnApplyPatch(domains: string[]) {
  const endpoints = domains.map((d) => ({ host: d, port: 443 }));
  return {
    spec: {
      networkPolicy: {
        allowedEndpoints: endpoints,
        learnEgress: false,
      },
    },
  };
}

/** Parse egress rules for display (mirrors policy get display logic). */
function formatEgressRules(
  egress: Array<{ ports?: Array<{ protocol: string; port: number }>; to?: Array<Record<string, any>> }>,
) {
  const rules: string[] = [];
  for (const rule of egress) {
    const ports = (rule.ports || []).map((p) => `${p.protocol}/${p.port}`).join(", ");
    const targets = (rule.to || [])
      .map((t) => {
        if (t.ipBlock) return `ipBlock: ${t.ipBlock.cidr}`;
        if (t.namespaceSelector) return "namespace: kube-system";
        return "?";
      })
      .join(" + ");
    rules.push(`allow ${ports} → ${targets}`);
  }
  return rules;
}

/** Compute sandbox namespace from name (used across policy commands). */
function sandboxNamespace(name: string): string {
  return `azureclaw-${name}`;
}

// Test withAdminAuth directly from router-admin (pure function, no mocks needed)
import { withAdminAuth } from "../router-admin.js";

// --- Tests ---

describe("sandbox namespace resolution", () => {
  it("prefixes name with azureclaw-", () => {
    expect(sandboxNamespace("myagent")).toBe("azureclaw-myagent");
  });

  it("works with hyphenated names", () => {
    expect(sandboxNamespace("my-agent-v2")).toBe("azureclaw-my-agent-v2");
  });
});

describe("policy allow — endpoint addition", () => {
  it("adds a new endpoint to empty list", () => {
    const patch = buildAllowPatch([], "api.example.com", 443);
    expect(patch.spec.networkPolicy.allowedEndpoints).toEqual([
      { host: "api.example.com", port: 443 },
    ]);
  });

  it("appends to existing endpoints", () => {
    const existing = [{ host: "github.com", port: 443 }];
    const patch = buildAllowPatch(existing, "api.openai.com", 443);
    expect(patch.spec.networkPolicy.allowedEndpoints).toHaveLength(2);
    expect(patch.spec.networkPolicy.allowedEndpoints[1]).toEqual({
      host: "api.openai.com",
      port: 443,
    });
  });

  it("preserves existing entries", () => {
    const existing = [
      { host: "github.com", port: 443 },
      { host: "api.github.com", port: 443 },
    ];
    const patch = buildAllowPatch(existing, "new.host.com", 8080);
    expect(patch.spec.networkPolicy.allowedEndpoints[0]).toEqual({
      host: "github.com",
      port: 443,
    });
    expect(patch.spec.networkPolicy.allowedEndpoints).toHaveLength(3);
  });

  it("uses correct patch structure for kubectl merge patch", () => {
    const patch = buildAllowPatch([], "test.com", 443);
    expect(patch).toHaveProperty("spec.networkPolicy.allowedEndpoints");
  });
});

describe("policy deny — endpoint removal", () => {
  it("removes matching host from endpoints", () => {
    const existing = [
      { host: "github.com", port: 443 },
      { host: "api.github.com", port: 443 },
    ];
    const { filtered, wasRemoved } = buildDenyPatch(existing, "github.com");
    expect(wasRemoved).toBe(true);
    expect(filtered).toEqual([{ host: "api.github.com", port: 443 }]);
  });

  it("reports no removal when host not in list", () => {
    const existing = [{ host: "github.com", port: 443 }];
    const { filtered, wasRemoved } = buildDenyPatch(existing, "not-present.com");
    expect(wasRemoved).toBe(false);
    expect(filtered).toHaveLength(1);
  });

  it("returns empty array when removing last endpoint", () => {
    const existing = [{ host: "only.host.com", port: 443 }];
    const { filtered, wasRemoved } = buildDenyPatch(existing, "only.host.com");
    expect(wasRemoved).toBe(true);
    expect(filtered).toEqual([]);
  });

  it("removes all entries for matching host regardless of port", () => {
    const existing = [
      { host: "api.example.com", port: 443 },
      { host: "api.example.com", port: 8080 },
      { host: "other.com", port: 443 },
    ];
    const { filtered } = buildDenyPatch(existing, "api.example.com");
    expect(filtered).toEqual([{ host: "other.com", port: 443 }]);
  });
});

describe("policy learn — apply patch", () => {
  it("converts domains to endpoints with port 443", () => {
    const patch = buildLearnApplyPatch(["api.github.com", "pypi.org"]);
    expect(patch.spec.networkPolicy.allowedEndpoints).toEqual([
      { host: "api.github.com", port: 443 },
      { host: "pypi.org", port: 443 },
    ]);
  });

  it("disables learnEgress when applying", () => {
    const patch = buildLearnApplyPatch(["example.com"]);
    expect(patch.spec.networkPolicy.learnEgress).toBe(false);
  });

  it("produces empty endpoints for empty domain list", () => {
    const patch = buildLearnApplyPatch([]);
    expect(patch.spec.networkPolicy.allowedEndpoints).toEqual([]);
    expect(patch.spec.networkPolicy.learnEgress).toBe(false);
  });
});

describe("egress rule formatting", () => {
  it("formats ipBlock rules", () => {
    const rules = formatEgressRules([
      {
        ports: [{ protocol: "TCP", port: 443 }],
        to: [{ ipBlock: { cidr: "10.0.0.0/8" } }],
      },
    ]);
    expect(rules).toEqual(["allow TCP/443 → ipBlock: 10.0.0.0/8"]);
  });

  it("formats namespace selector rules", () => {
    const rules = formatEgressRules([
      {
        ports: [{ protocol: "TCP", port: 53 }],
        to: [{ namespaceSelector: {} }],
      },
    ]);
    expect(rules).toEqual(["allow TCP/53 → namespace: kube-system"]);
  });

  it("handles multiple ports in one rule", () => {
    const rules = formatEgressRules([
      {
        ports: [
          { protocol: "TCP", port: 443 },
          { protocol: "TCP", port: 80 },
        ],
        to: [{ ipBlock: { cidr: "0.0.0.0/0" } }],
      },
    ]);
    expect(rules[0]).toBe("allow TCP/443, TCP/80 → ipBlock: 0.0.0.0/0");
  });

  it("returns ? for unknown target types", () => {
    const rules = formatEgressRules([
      { ports: [{ protocol: "TCP", port: 443 }], to: [{ podSelector: {} }] },
    ]);
    expect(rules[0]).toContain("?");
  });

  it("handles empty egress list", () => {
    expect(formatEgressRules([])).toEqual([]);
  });
});

describe("withAdminAuth (router-admin)", () => {
  it("adds Authorization header when token is provided", () => {
    const args = withAdminAuth(["curl", "-sf", "http://localhost:8443/healthz"], "my-token");
    expect(args).toContain("-H");
    expect(args).toContain("Authorization: Bearer my-token");
    // URL should still be last
    expect(args[args.length - 1]).toBe("http://localhost:8443/healthz");
  });

  it("returns args unchanged when token is empty", () => {
    const original = ["curl", "-sf", "http://localhost:8443/healthz"];
    const args = withAdminAuth(original, "");
    expect(args).toEqual(original);
  });
});
