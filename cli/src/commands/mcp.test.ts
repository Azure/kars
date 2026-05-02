// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, it, expect } from "vitest";
import { __test, mcpCommand } from "./mcp.js";

const { buildMcpSpecFromFlags, validateMcpSpec, summarizeMcpRow } = __test;

describe("mcpCommand — registration", () => {
  it("registers all four sub-verbs at top level", () => {
    const cmd = mcpCommand();
    expect(cmd.name()).toBe("mcp");
    const sub = cmd.commands.map((c) => c.name()).sort();
    expect(sub).toEqual(["apply", "delete", "get", "list"]);
  });

  it("apply exposes --url, --production-mode, --oauth-issuer, --allowed-tool", () => {
    const cmd = mcpCommand();
    const apply = cmd.commands.find((c) => c.name() === "apply")!;
    const flags = apply.options.map((o) => o.long);
    expect(flags).toContain("--url");
    expect(flags).toContain("--production-mode");
    expect(flags).toContain("--oauth-issuer");
    expect(flags).toContain("--allowed-tool");
    expect(flags).toContain("--from-file");
  });
});

describe("mcpCommand — buildMcpSpecFromFlags", () => {
  it("emits url + omits productionMode default", () => {
    const spec = buildMcpSpecFromFlags({ url: "http://localhost:3000" });
    expect(spec).toEqual({ url: "http://localhost:3000" });
  });

  it("attaches oauth when issuer/audience/resource set", () => {
    const spec = buildMcpSpecFromFlags({
      url: "https://x",
      productionMode: true,
      oauthIssuer: "https://issuer.example.com",
      oauthAudience: "api://mcp",
    });
    expect(spec.productionMode).toBe(true);
    expect(spec.oauth).toEqual({
      issuer: "https://issuer.example.com",
      audience: "api://mcp",
    });
  });

  it("collects allowed tools and scopes", () => {
    const spec = buildMcpSpecFromFlags({
      url: "https://x",
      allowedTool: ["get-issue", "create-pr"],
      scope: ["mcp.read", "mcp.write"],
    });
    expect(spec.allowedTools).toEqual(["get-issue", "create-pr"]);
    expect(spec.scopes).toEqual(["mcp.read", "mcp.write"]);
  });

  it("converts allowed-sandbox-label into matchLabels object", () => {
    const spec = buildMcpSpecFromFlags({
      url: "https://x",
      allowedSandboxLabel: ["env=prod"],
    });
    expect(spec.allowedSandboxes).toEqual({ matchLabels: { env: "prod" } });
  });

  it("omits allowedSandboxes when no labels supplied", () => {
    const spec = buildMcpSpecFromFlags({ url: "https://x" });
    expect(spec.allowedSandboxes).toBeUndefined();
  });
});

describe("mcpCommand — validateMcpSpec", () => {
  it("requires url", () => {
    const errs = validateMcpSpec({});
    expect(errs[0]).toMatch(/spec.url/);
  });

  it("requires url to start with http(s)://", () => {
    const errs = validateMcpSpec({ url: "ftp://x" });
    expect(errs[0]).toMatch(/http/);
  });

  it("productionMode requires https://", () => {
    const errs = validateMcpSpec({ url: "http://x", productionMode: true, oauth: { issuer: "https://i" } });
    expect(errs.join(" ")).toMatch(/begin with https/);
  });

  it("productionMode requires oauth.issuer", () => {
    const errs = validateMcpSpec({ url: "https://x", productionMode: true });
    expect(errs.join(" ")).toMatch(/oauth.issuer/);
  });

  it("accepts a non-production minimal spec", () => {
    expect(validateMcpSpec({ url: "http://localhost:3000" })).toEqual([]);
  });

  it("accepts a production spec with https + issuer", () => {
    expect(
      validateMcpSpec({
        url: "https://mcp.example.com",
        productionMode: true,
        oauth: { issuer: "https://issuer.example.com" },
      }),
    ).toEqual([]);
  });
});

describe("mcpCommand — summarizeMcpRow", () => {
  const NOW = new Date("2025-01-01T00:00:00Z");

  it("renders name, url, prod, age, phase", () => {
    const row = summarizeMcpRow(
      {
        metadata: { name: "m1", creationTimestamp: "2024-12-25T00:00:00Z" },
        spec: { url: "https://x", productionMode: true },
        status: { phase: "Ready" },
      },
      NOW,
    );
    expect(row).toEqual(["m1", "https://x", "yes", "7d", "Ready"]);
  });

  it("renders 'no' for non-production", () => {
    const row = summarizeMcpRow(
      { metadata: { name: "m2" }, spec: { url: "http://x" } },
      NOW,
    );
    expect(row[2]).toBe("no");
  });
});
