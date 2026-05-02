// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, it, expect } from "vitest";
import { __test, a2aAgentCommand } from "./a2a.js";

const {
  buildA2aAgentSpecFromFlags,
  validateA2aAgentSpec,
  summarizeA2aAgentRow,
  parseSigningKey,
} = __test;

describe("a2aAgentCommand — registration", () => {
  it("registers all four sub-verbs at top level", () => {
    const cmd = a2aAgentCommand();
    expect(cmd.name()).toBe("a2a-agent");
    const sub = cmd.commands.map((c) => c.name()).sort();
    expect(sub).toEqual(["apply", "delete", "get", "list"]);
  });

  it("apply exposes --endpoint-url, --signing-key, --capability, --require-signed", () => {
    const cmd = a2aAgentCommand();
    const apply = cmd.commands.find((c) => c.name() === "apply")!;
    const flags = apply.options.map((o) => o.long);
    expect(flags).toContain("--endpoint-url");
    expect(flags).toContain("--signing-key");
    expect(flags).toContain("--capability");
    expect(flags).toContain("--require-signed");
    expect(flags).toContain("--from-file");
  });
});

describe("a2aAgentCommand — parseSigningKey", () => {
  it("parses kid:alg:b64u", () => {
    expect(parseSigningKey("k1:EdDSA:abc123")).toEqual({
      kid: "k1",
      alg: "EdDSA",
      publicKeyB64u: "abc123",
    });
  });

  it("parses kid:alg:b64u:notAfter", () => {
    expect(parseSigningKey("k1:EdDSA:abc:1735689600")).toEqual({
      kid: "k1",
      alg: "EdDSA",
      publicKeyB64u: "abc",
      notAfter: 1735689600,
    });
  });

  it("rejects malformed entries", () => {
    expect(() => parseSigningKey("only-two:parts")).toThrow();
    expect(() => parseSigningKey(":EdDSA:abc")).toThrow();
    expect(() => parseSigningKey("k:EdDSA:abc:notanumber")).toThrow();
  });
});

describe("a2aAgentCommand — buildA2aAgentSpecFromFlags", () => {
  it("builds a minimal spec with endpointUrl + one signing key", () => {
    const spec = buildA2aAgentSpecFromFlags({
      endpointUrl: "https://agent.example.com",
      signingKey: ["k1:EdDSA:abc"],
    });
    expect(spec).toEqual({
      endpointUrl: "https://agent.example.com",
      signingKeys: [{ kid: "k1", alg: "EdDSA", publicKeyB64u: "abc" }],
    });
  });

  it("collects multiple capabilities and signingKeys", () => {
    const spec = buildA2aAgentSpecFromFlags({
      endpointUrl: "https://x",
      signingKey: ["k1:EdDSA:a", "k2:EdDSA:b"],
      capability: ["tasks", "streaming"],
    });
    expect((spec.signingKeys as unknown[]).length).toBe(2);
    expect(spec.capabilities).toEqual(["tasks", "streaming"]);
  });

  it("attaches productionMode and trust block", () => {
    const spec = buildA2aAgentSpecFromFlags({
      endpointUrl: "https://x",
      signingKey: ["k1:EdDSA:a"],
      productionMode: true,
      requireSigned: true,
      minSignatures: "2",
      maxSkewSeconds: "30",
    });
    expect(spec.productionMode).toBe(true);
    expect(spec.trust).toEqual({
      requireSignedRequests: true,
      minSignaturesRequired: 2,
      maxClockSkewSeconds: 30,
    });
  });

  it("attaches policyRefs.toolPolicy when --policy-toolpolicy is set", () => {
    const spec = buildA2aAgentSpecFromFlags({
      endpointUrl: "https://x",
      signingKey: ["k1:EdDSA:a"],
      toolPolicy: "default-tp",
    });
    expect(spec.policyRefs).toEqual({ toolPolicy: "default-tp" });
  });

  it("rejects negative --min-signatures", () => {
    expect(() =>
      buildA2aAgentSpecFromFlags({
        endpointUrl: "https://x",
        signingKey: ["k1:EdDSA:a"],
        minSignatures: "-1",
      }),
    ).toThrow(/non-negative/);
  });
});

describe("a2aAgentCommand — validateA2aAgentSpec", () => {
  it("requires endpointUrl", () => {
    const errs = validateA2aAgentSpec({ signingKeys: [{ kid: "k", alg: "EdDSA", publicKeyB64u: "x" }] });
    expect(errs[0]).toMatch(/endpointUrl/);
  });

  it("requires at least one signingKey", () => {
    const errs = validateA2aAgentSpec({ endpointUrl: "https://x" });
    expect(errs.join(" ")).toMatch(/signingKeys/);
  });

  it("rejects non-EdDSA signingKey alg", () => {
    const errs = validateA2aAgentSpec({
      endpointUrl: "https://x",
      signingKeys: [{ kid: "k", alg: "RS256", publicKeyB64u: "x" }],
    });
    expect(errs.join(" ")).toMatch(/EdDSA/);
  });

  it("rejects http:// when productionMode is true", () => {
    const errs = validateA2aAgentSpec({
      endpointUrl: "http://x",
      productionMode: true,
      signingKeys: [{ kid: "k", alg: "EdDSA", publicKeyB64u: "x" }],
    });
    expect(errs.join(" ")).toMatch(/https/);
  });

  it("accepts a complete production spec", () => {
    expect(
      validateA2aAgentSpec({
        endpointUrl: "https://x",
        productionMode: true,
        signingKeys: [{ kid: "k", alg: "EdDSA", publicKeyB64u: "x" }],
      }),
    ).toEqual([]);
  });
});

describe("a2aAgentCommand — summarizeA2aAgentRow", () => {
  const NOW = new Date("2025-01-01T00:00:00Z");

  it("formats name, endpoint, prod, age, status", () => {
    const row = summarizeA2aAgentRow(
      {
        metadata: { name: "ag1", creationTimestamp: "2024-12-31T23:55:00Z" },
        spec: { endpointUrl: "https://ag", productionMode: true },
        status: { phase: "Ready" },
      },
      NOW,
    );
    expect(row).toEqual(["ag1", "https://ag", "yes", "5m", "Ready"]);
  });
});

// Sanity: extending a2a.ts must not regress the original a2a command.
describe("a2aCommand — original surface preserved", () => {
  it("still registers list-exposed and schema", async () => {
    const { a2aCommand } = await import("./a2a.js");
    const cmd = a2aCommand();
    const sub = cmd.commands.map((c) => c.name()).sort();
    expect(sub).toEqual(["list-exposed", "schema"]);
  });
});
